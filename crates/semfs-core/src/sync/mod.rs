//! Background sync engine.
//!
//! Four loops:
//!
//! - **Loop A — delta pull.** Every ~30s, walk `/v3/documents/list` sorted by
//!   `updatedAt desc` and reconcile anything newer than our watermark into
//!   the local cache.
//! - **Loop C — deletion scan.** Every ~5min, diff the full remote ID set
//!   against the local `fs_remote` table and unlink anything that
//!   disappeared.
//! - **Loop D — push worker.** Claims queued push jobs from `push_queue`
//!   and sends them; coalesces rapid writes to at most 2 server requests
//!   per filepath (one inflight + one pending).
//! - **Loop E — inflight poller.** Polls `GET /v3/documents/:id` for docs
//!   whose server-side processing hasn't flipped to `done` yet; updates
//!   `mirrored_updated_at` and emits INFO/WARN/STOP tiers when stuck.

pub mod pull;
pub mod push;
pub mod scan;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::cache::CacheFs;

#[derive(Debug, Clone, Copy)]
pub enum InitialPullProgress {
    DeletionScan(scan::DeletionScanProgress),
    Pull(pull::PullProgress),
}

/// Knobs for the sync engine. All optional — defaults are production-sane.
#[derive(Debug, Clone, Copy)]
pub struct SyncOptions {
    pub delta_interval: Duration,
    pub deletion_scan_interval: Duration,
    pub pull_enabled: bool,
    pub push_enabled: bool,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            delta_interval: Duration::from_secs(30),
            deletion_scan_interval: Duration::from_secs(300),
            pull_enabled: true,
            push_enabled: true,
        }
    }
}

/// Orchestrates background sync for a mount. Spawn with [`SyncEngine::start`]
/// and signal shutdown via the `watch::Sender<bool>` (true = stop).
#[derive(Debug)]
pub struct SyncEngine;

impl SyncEngine {
    /// Run the synchronous startup sequence: a deletion scan first (to catch
    /// anything deleted while we were offline), then hydrate. A COLD cache does a
    /// full pull; a WARM cache (prior watermark) does a cheap delta — re-mounting
    /// an already-hydrated container must not re-reconcile every doc (redundant,
    /// and on a heavy shared cache it thrashes). Blocks until both complete.
    pub async fn initial_pull(fs: &Arc<CacheFs>) -> anyhow::Result<(usize, usize)> {
        let removed = scan::deletion_scan(fs).await.unwrap_or(0);
        let reconciled = if pull::cache_is_warm(fs) {
            pull::delta_pull(fs).await?
        } else {
            pull::full_pull(fs).await?
        };
        Ok((removed, reconciled))
    }

    pub async fn initial_pull_with_progress<F>(
        fs: &Arc<CacheFs>,
        mut on_progress: F,
    ) -> anyhow::Result<(usize, usize)>
    where
        F: FnMut(InitialPullProgress) + Send,
    {
        let removed = if fs.db().remote_count() == 0 {
            0
        } else {
            scan::deletion_scan_with_progress(fs, |progress| {
                on_progress(InitialPullProgress::DeletionScan(progress));
            })
            .await
            .unwrap_or(0)
        };
        // Warm cache (prior watermark): the container is already hydrated, so a
        // full re-reconcile of every doc on each mount is pure redundant work
        // (and thrashes a heavy shared cache). Do a cheap delta — it pages only
        // until the watermark, catching new/updated docs; deletions are handled
        // by the scan above + the periodic loop. Cold cache → full hydrating pull.
        let reconciled = if pull::cache_is_warm(fs) {
            pull::delta_pull(fs).await?
        } else {
            pull::full_pull_with_progress(fs, |progress| {
                on_progress(InitialPullProgress::Pull(progress));
            })
            .await?
        };
        Ok((removed, reconciled))
    }

    /// Spawn background loops for this mount.
    ///
    /// Push-side loops — D (push worker) and E (inflight status poller) — are
    /// spawned only when `opts.push_enabled` is true. Setting it to false is how
    /// `semfs mount --no-push` gives a read-only mount: local writes never reach
    /// the server, so a shared/seeded container can be read without contaminating
    /// it.
    ///
    /// Pull-side loops — A (delta pull) and C (deletion scan) — are spawned
    /// only when `opts.pull_enabled` is true. Setting it to false is how
    /// `semfs mount --no-sync` stops polling for remote changes while
    /// keeping local writes flowing to Supermemory.
    ///
    /// Returns a JoinSet whose tasks exit when `shutdown.send(true)` is
    /// called.
    pub fn start(
        fs: Arc<CacheFs>,
        opts: SyncOptions,
        shutdown: watch::Receiver<bool>,
    ) -> JoinSet<()> {
        let mut set = JoinSet::new();

        if opts.pull_enabled {
            let fs_a = fs.clone();
            let mut sd_a = shutdown.clone();
            set.spawn(async move {
                run_delta_loop(fs_a, opts.delta_interval, &mut sd_a).await;
            });

            let fs_c = fs.clone();
            let mut sd_c = shutdown.clone();
            set.spawn(async move {
                run_deletion_loop(fs_c, opts.deletion_scan_interval, &mut sd_c).await;
            });

            // Loop F — hydration worker. Gated on pull_enabled so
            // `--no-sync` mounts make no remote reads.
            let fs_f = fs.clone();
            let sd_f = shutdown.clone();
            set.spawn(async move {
                crate::cache::hydration::run_hydration_worker(fs_f, sd_f).await;
            });
        }

        if opts.push_enabled {
            let fs_d = fs.clone();
            let sd_d = shutdown.clone();
            set.spawn(async move {
                push::run_push_worker(fs_d, sd_d).await;
            });

            let fs_e = fs.clone();
            let sd_e = shutdown.clone();
            set.spawn(async move {
                push::run_inflight_poller(fs_e, sd_e).await;
            });
        }

        set
    }

    /// Final deletion scan before the mount releases. Best-effort: logs on
    /// failure and returns.
    pub async fn unmount_scan(fs: &Arc<CacheFs>) {
        match scan::deletion_scan(fs).await {
            Ok(n) if n > 0 => tracing::info!(removed = n, "final deletion scan"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "final deletion scan failed"),
        }
    }
}

async fn run_delta_loop(
    fs: Arc<CacheFs>,
    base_interval: Duration,
    shutdown: &mut watch::Receiver<bool>,
) {
    let mut empty_streak = 0u32;
    loop {
        let interval = adaptive_interval(base_interval, empty_streak);
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() { return; }
            }
        }

        match pull::delta_pull(&fs).await {
            Ok(n) => {
                if n == 0 {
                    empty_streak = empty_streak.saturating_add(1);
                } else {
                    empty_streak = 0;
                    tracing::debug!(reconciled = n, "delta pull");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "delta pull failed");
            }
        }
    }
}

async fn run_deletion_loop(
    fs: Arc<CacheFs>,
    base_interval: Duration,
    shutdown: &mut watch::Receiver<bool>,
) {
    loop {
        let interval = jittered(base_interval, 30);
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() { return; }
            }
        }

        match scan::deletion_scan(&fs).await {
            Ok(n) if n > 0 => tracing::info!(removed = n, "deletion scan"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "deletion scan failed"),
        }
    }
}

/// Adaptive cadence: shorter after activity, stretch when idle, add ±jitter.
fn adaptive_interval(base: Duration, empty_streak: u32) -> Duration {
    let secs = base.as_secs_f64();
    let adjusted = if empty_streak == 0 {
        (secs / 3.0).max(10.0)
    } else if empty_streak >= 3 {
        (secs * 2.0).min(60.0)
    } else {
        secs
    };
    jittered(Duration::from_secs_f64(adjusted), 5)
}

/// Add uniform ±`max_jitter_secs` jitter to an interval (never below 1s).
fn jittered(base: Duration, max_jitter_secs: i64) -> Duration {
    // Cheap pseudo-random from system time nanos; avoids pulling in a
    // dedicated RNG crate for this one use.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as i64)
        .unwrap_or(0);
    let jitter = (nanos % (2 * max_jitter_secs + 1)) - max_jitter_secs;
    let secs = (base.as_secs() as i64 + jitter).max(1);
    Duration::from_secs(secs as u64)
}

#[cfg(test)]
mod start_gating_tests {
    use super::*;
    use crate::cache::Db;

    fn fs() -> Arc<CacheFs> {
        Arc::new(CacheFs::new(Arc::new(Db::open_in_memory().unwrap())))
    }

    fn opts(pull_enabled: bool, push_enabled: bool) -> SyncOptions {
        SyncOptions {
            delta_interval: Duration::from_secs(30),
            deletion_scan_interval: Duration::from_secs(300),
            pull_enabled,
            push_enabled,
        }
    }

    // start() spawns A,C,F (pull side) and D,E (push side). Counting the
    // spawned tasks proves --no-push (push_enabled=false) does NOT start the
    // push worker / inflight poller, and --no-sync gates the pull side.
    #[tokio::test]
    async fn start_gates_loops_on_pull_and_push_flags() {
        let (_tx, rx) = watch::channel(false);
        assert_eq!(SyncEngine::start(fs(), opts(true, true), rx.clone()).len(), 5, "pull+push");
        assert_eq!(SyncEngine::start(fs(), opts(true, false), rx.clone()).len(), 3, "--no-push: pull loops only");
        assert_eq!(SyncEngine::start(fs(), opts(false, true), rx.clone()).len(), 2, "--no-sync: push loops only");
        assert_eq!(SyncEngine::start(fs(), opts(false, false), rx.clone()).len(), 0, "both off");
    }
}
