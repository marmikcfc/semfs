//! Background read-side hydration queue.
//!
//! Invariant: VFS syscall paths must not await network I/O. Misses enqueue
//! a refresh here and return immediately; the queue is in-memory and safe
//! to lose on restart (delta pull and `semfs sync` are the durable paths).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::{watch, Notify, Semaphore};
use tokio::task::JoinSet;

const HYDRATION_CONCURRENCY: usize = 4;
const NEGATIVE_TTL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HydrationKey {
    Exact(String),
    /// Must end with `/`.
    Prefix(String),
}

impl HydrationKey {
    pub fn path(&self) -> &str {
        match self {
            HydrationKey::Exact(p) | HydrationKey::Prefix(p) => p,
        }
    }
}

#[derive(Debug)]
struct Inner {
    queue: VecDeque<HydrationKey>,
    pending: HashSet<HydrationKey>,
    inflight: HashSet<HydrationKey>,
    recent: HashMap<HydrationKey, Instant>,
}

#[derive(Debug)]
pub struct HydrationScheduler {
    inner: Mutex<Inner>,
    notify: Notify,
    negative_ttl: Duration,
}

impl HydrationScheduler {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                pending: HashSet::new(),
                inflight: HashSet::new(),
                recent: HashMap::new(),
            }),
            notify: Notify::new(),
            negative_ttl: NEGATIVE_TTL,
        })
    }

    pub fn enqueue(&self, key: HydrationKey) {
        let now = Instant::now();
        let should_notify = {
            let mut inner = self.inner.lock();
            inner
                .recent
                .retain(|_, ts| now.duration_since(*ts) < self.negative_ttl);
            if inner.recent.contains_key(&key) {
                return;
            }
            if inner.pending.contains(&key) || inner.inflight.contains(&key) {
                return;
            }
            inner.pending.insert(key.clone());
            inner.queue.push_back(key);
            true
        };
        if should_notify {
            self.notify.notify_one();
        }
    }

    pub(crate) fn claim_next(&self) -> Option<HydrationKey> {
        let mut inner = self.inner.lock();
        let key = inner.queue.pop_front()?;
        inner.pending.remove(&key);
        inner.inflight.insert(key.clone());
        Some(key)
    }

    pub(crate) fn complete(&self, key: HydrationKey) {
        let mut inner = self.inner.lock();
        inner.inflight.remove(&key);
        inner.recent.insert(key, Instant::now());
    }

    pub fn notify(&self) -> &Notify {
        &self.notify
    }

    pub fn pending_len(&self) -> usize {
        self.inner.lock().queue.len()
    }

    pub fn inflight_len(&self) -> usize {
        self.inner.lock().inflight.len()
    }
}

pub async fn run_hydration_worker(
    fs: Arc<crate::cache::CacheFs>,
    mut shutdown: watch::Receiver<bool>,
) {
    if fs.api().is_none() {
        return;
    }
    let sched = fs.hydration().clone();
    let sem = Arc::new(Semaphore::new(HYDRATION_CONCURRENCY));
    let mut set = JoinSet::new();

    'outer: loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() { break 'outer; }
            }
            _ = sched.notify().notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }

        // Reap finished spawns; otherwise the JoinSet grows unbounded
        // under steady load.
        while let Ok(Some(_)) =
            tokio::time::timeout(Duration::from_millis(0), set.join_next()).await
        {}

        loop {
            if *shutdown.borrow() {
                break 'outer;
            }
            let permit = match sem.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => break,
            };
            let Some(key) = sched.claim_next() else {
                drop(permit);
                break;
            };
            let fs_clone = fs.clone();
            set.spawn(async move {
                let _permit = permit;
                let path = key.path().to_string();
                match fs_clone.hydrate_path(&path).await {
                    Ok(()) => {
                        tracing::debug!(key = ?key, "hydration: pull ok");
                    }
                    Err(e) => {
                        tracing::warn!(key = ?key, error = %e, "hydration: pull failed");
                    }
                }
                fs_clone.hydration().complete(key);
            });
        }
    }

    // Graceful drain on shutdown.
    while set.join_next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_dedupes_pending() {
        let s = HydrationScheduler::new();
        s.enqueue(HydrationKey::Prefix("/docs/".into()));
        s.enqueue(HydrationKey::Prefix("/docs/".into()));
        s.enqueue(HydrationKey::Prefix("/docs/".into()));
        assert_eq!(s.pending_len(), 1);
    }

    #[test]
    fn claim_next_drains_in_fifo_order() {
        let s = HydrationScheduler::new();
        s.enqueue(HydrationKey::Exact("/a.md".into()));
        s.enqueue(HydrationKey::Exact("/b.md".into()));
        s.enqueue(HydrationKey::Exact("/c.md".into()));
        assert_eq!(s.claim_next(), Some(HydrationKey::Exact("/a.md".into())));
        assert_eq!(s.claim_next(), Some(HydrationKey::Exact("/b.md".into())));
        assert_eq!(s.claim_next(), Some(HydrationKey::Exact("/c.md".into())));
        assert_eq!(s.claim_next(), None);
    }

    #[test]
    fn enqueue_skips_inflight() {
        let s = HydrationScheduler::new();
        s.enqueue(HydrationKey::Exact("/x".into()));
        let _ = s.claim_next();
        assert_eq!(s.inflight_len(), 1);
        s.enqueue(HydrationKey::Exact("/x".into())); // already inflight
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn negative_ttl_suppresses_recent_completion() {
        let s = HydrationScheduler::new();
        s.enqueue(HydrationKey::Exact("/x".into()));
        let key = s.claim_next().unwrap();
        s.complete(key);
        // Within TTL: skipped.
        s.enqueue(HydrationKey::Exact("/x".into()));
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn negative_ttl_expires_eventually() {
        // Use a scheduler with a very short TTL so the test stays fast.
        let s = Arc::new(HydrationScheduler {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                pending: HashSet::new(),
                inflight: HashSet::new(),
                recent: HashMap::new(),
            }),
            notify: Notify::new(),
            negative_ttl: Duration::from_millis(20),
        });
        s.enqueue(HydrationKey::Exact("/x".into()));
        let key = s.claim_next().unwrap();
        s.complete(key);
        std::thread::sleep(Duration::from_millis(40));
        s.enqueue(HydrationKey::Exact("/x".into()));
        assert_eq!(s.pending_len(), 1);
    }
}
