//! Background L7 (entity-graph) work queue.
//!
//! L7 entity extraction makes a blocking LLM call per file. Done inline on the
//! index/flush path it serializes through the single FUSE dispatch thread (~1
//! file/s, CPU idle) — see `tickets/parallelize-l7/`. Instead, `index()`
//! enqueues the file here after writing its vectors, and `run_graph_worker`
//! drains the queue with bounded concurrency, so N extractions run at once.
//!
//! In-memory and safe to lose on restart: edges are re-derived on the next
//! write, and a missing graph only weakens the ±5% co-mention boost, never
//! recall.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{watch, Notify, Semaphore};
use tokio::task::JoinSet;

/// Max concurrent in-flight entity extractions. L7 is network-IO-bound (a
/// blocking LLM call), so this can comfortably exceed the CPU count; the work
/// runs on the blocking pool. Bounded to avoid hammering the provider.
const L7_CONCURRENCY: usize = 8;

#[derive(Debug, Default)]
struct Inner {
    queue: VecDeque<(u64, String)>,
    /// Inos currently pending or in-flight — dedups repeat enqueues so a file
    /// re-read several times in one warm is extracted once per settle.
    active: HashSet<u64>,
    inflight: usize,
}

/// FIFO queue of `(ino, filepath)` files awaiting entity extraction, deduped by
/// ino. Cloneable handle (`Arc`) shared by the indexer (enqueue) and the worker
/// (claim/complete).
#[derive(Debug)]
pub struct GraphQueue {
    inner: Mutex<Inner>,
    notify: Notify,
}

impl GraphQueue {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            notify: Notify::new(),
        })
    }

    /// Enqueue a file for extraction. No-op if it's already pending/in-flight.
    pub fn enqueue(&self, ino: u64, filepath: String) {
        let notify = {
            let mut inner = self.inner.lock();
            if !inner.active.insert(ino) {
                false
            } else {
                inner.queue.push_back((ino, filepath));
                true
            }
        };
        if notify {
            self.notify.notify_one();
        }
    }

    pub(crate) fn claim_next(&self) -> Option<(u64, String)> {
        let mut inner = self.inner.lock();
        let item = inner.queue.pop_front()?;
        inner.inflight += 1;
        Some(item)
    }

    pub(crate) fn complete(&self, ino: u64) {
        let mut inner = self.inner.lock();
        inner.active.remove(&ino);
        inner.inflight = inner.inflight.saturating_sub(1);
    }

    pub fn notify(&self) -> &Notify {
        &self.notify
    }

    /// `true` when nothing is queued or in-flight — a warm waits on this (via
    /// the externally-visible `edges` row count) before declaring "done".
    pub fn is_idle(&self) -> bool {
        let inner = self.inner.lock();
        inner.queue.is_empty() && inner.inflight == 0
    }

    /// Queued + in-flight depth, for status/metrics.
    pub fn depth(&self) -> usize {
        let inner = self.inner.lock();
        inner.queue.len() + inner.inflight
    }
}

/// Drain the indexer's L7 queue with bounded concurrency until shutdown.
/// Exits immediately if the indexer has no graph extractor. Each claimed file
/// is extracted via [`LocalIndexer::index_graph`] (which runs the blocking LLM
/// call on the blocking pool), so up to `L7_CONCURRENCY` extractions overlap.
pub async fn run_graph_worker(
    indexer: Arc<dyn super::LocalIndexer>,
    mut shutdown: watch::Receiver<bool>,
) {
    let Some(queue) = indexer.graph_queue() else {
        return;
    };
    let sem = Arc::new(Semaphore::new(L7_CONCURRENCY));
    let mut set = JoinSet::new();

    'outer: loop {
        tokio::select! {
            _ = shutdown.changed() => { if *shutdown.borrow() { break 'outer; } }
            _ = queue.notify().notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }

        // Reap finished tasks so the JoinSet can't grow unbounded.
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
            let Some((ino, filepath)) = queue.claim_next() else {
                drop(permit);
                break;
            };
            let idx = indexer.clone();
            let q = queue.clone();
            set.spawn(async move {
                let _permit = permit;
                if let Err(e) = idx.index_graph(ino, &filepath).await {
                    tracing::warn!(ino, filepath = %filepath, error = %e, "L7 extraction failed");
                }
                q.complete(ino);
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
    fn enqueue_dedups_by_ino() {
        let q = GraphQueue::new();
        q.enqueue(1, "/a.md".into());
        q.enqueue(1, "/a.md".into()); // dup pending → ignored
        q.enqueue(2, "/b.md".into());
        assert_eq!(q.depth(), 2);
        assert!(!q.is_idle());
    }

    #[test]
    fn claim_complete_lifecycle() {
        let q = GraphQueue::new();
        q.enqueue(1, "/a.md".into());
        let (ino, fp) = q.claim_next().unwrap();
        assert_eq!((ino, fp.as_str()), (1, "/a.md"));
        assert!(!q.is_idle()); // in-flight
        assert!(q.claim_next().is_none());
        q.complete(1);
        assert!(q.is_idle());
        // can re-enqueue after completion (re-index)
        q.enqueue(1, "/a.md".into());
        assert_eq!(q.depth(), 1);
    }
}
