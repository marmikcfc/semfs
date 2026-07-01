//! `push_seed` — drain a seed's `push_queue` into Supermemory, **MOUNTLESS**.
//!
//! Reuses semfs's OWN push worker (`CacheFs::with_api` + `run_push_worker`), so the
//! documents land in exactly the shape the `cloud` storage backend reads. That means the
//! SAME seed serves BOTH backends: `sqlite` (local search) and `cloud`/supermemory (after
//! this push). No FUSE, no daemon — so it runs anywhere, including Modal's gVisor where the
//! seeds live (the `semfs sync` CLI can't: it only nudges an already-running daemon).
//!
//! The seed's `push_queue` is populated by `index()` at build time, so a freshly indexed
//! seed already carries everything to push.
//!
//! Run:
//!   SUPERMEMORY_API_KEY=... [SEMFS_API_URL=https://api.supermemory.ai] \
//!     push_seed <seed.db> <container_tag>
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use semfs_core::api::ApiClient;
use semfs_core::cache::{CacheFs, Db};
use semfs_core::sync::push::run_push_worker;
use tokio::sync::watch;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    let seed = std::env::args()
        .nth(1)
        .expect("usage: push_seed <seed.db> <container_tag>");
    let container = std::env::args()
        .nth(2)
        .expect("usage: push_seed <seed.db> <container_tag>");
    let api_url =
        std::env::var("SEMFS_API_URL").unwrap_or_else(|_| "https://api.supermemory.ai".to_string());
    let api_key = std::env::var("SUPERMEMORY_API_KEY").context("SUPERMEMORY_API_KEY env required")?;

    eprintln!("push_seed: seed={seed} container={container} api={api_url}");

    // Mirror the daemon: validate the key, then carry the session user_id into the client.
    let session = ApiClient::validate_key(&api_url, &api_key)
        .await
        .context("validating Supermemory API key")?;
    let mut api = ApiClient::new(&api_url, &api_key, &container);
    if let Some(uid) = session.user_id.clone() {
        api = api.with_user_id(uid);
    }
    let api = Arc::new(api);

    // Open the SEED directly.
    let db = Arc::new(Db::open(Path::new(&seed)).context("opening seed db")?);

    // `--backfill`: enqueue the whole corpus for push. The indexer (`seed_dir`) and
    // `materialize_fs` write the cache directly and never enqueue — only the live FUSE write
    // path does — so a freshly built seed has an empty push_queue. This fills it so the whole
    // corpus pushes to Supermemory in one shot.
    if std::env::args().any(|a| a == "--backfill") {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let prefix = std::env::var("SEMFS_PUSH_PREFIX").ok();
        let n = db.enqueue_all_real_files_for_push(now_ms, prefix.as_deref());
        eprintln!(
            "push_seed: --backfill enqueued {n} real files into push_queue{}",
            prefix.map(|p| format!(" (prefix {p})")).unwrap_or_default()
        );
    }

    let fs = Arc::new(CacheFs::with_api(db, api));

    let start = fs.push_queue_len();
    eprintln!("push_seed: {start} docs queued in push_queue");
    if start == 0 {
        eprintln!("push_seed: push_queue EMPTY — nothing to push (was the seed built with index()?)");
        return Ok(());
    }

    let (tx, rx) = watch::channel(false);
    let worker = tokio::spawn(run_push_worker(fs.clone(), rx));

    // Poll until the queue drains. Stall guard: 5 min with zero progress → stop (poisoned/wedged).
    let mut last = start;
    let mut stalls = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let pending = fs.push_queue_len();
        eprintln!("push_seed: pending {pending}/{start}");
        if pending == 0 {
            break;
        }
        if pending == last {
            stalls += 1;
            if stalls >= 100 {
                eprintln!("push_seed: STALLED at {pending} pending (~5 min no progress) — stopping");
                break;
            }
        } else {
            stalls = 0;
            last = pending;
        }
    }

    let _ = tx.send(true);
    let _ = worker.await;
    let remaining = fs.push_queue_len();
    eprintln!(
        "push_seed: DONE — pushed {} of {start} docs to Supermemory container '{container}' ({remaining} left)",
        start - remaining
    );
    if remaining > 0 {
        std::process::exit(2);
    }
    Ok(())
}
