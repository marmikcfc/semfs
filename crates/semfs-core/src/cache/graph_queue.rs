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

    /// True when nothing is queued or in-flight — the graph has settled. Lets the
    /// worker debounce the KG recompute to a quiet period (L6 dynamic refresh).
    pub fn is_settled(&self) -> bool {
        let inner = self.inner.lock();
        inner.queue.is_empty() && inner.inflight == 0
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
    kg_refresh: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
) {
    let Some(queue) = indexer.graph_queue() else {
        return;
    };
    let sem = Arc::new(Semaphore::new(L7_CONCURRENCY));
    let mut set = JoinSet::new();
    // L6: edges changed since the last KG recompute. Set when we spawn an
    // extraction; cleared after a debounced recompute once the queue settles.
    let mut dirty = false;

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
            dirty = true;
        }

        // L6 dynamic refresh: once the queue has fully settled after a batch of
        // edge changes, recompute the workspace KG / KNOWLEDGE_GRAPH.md ONCE.
        // The 500ms select tick is the debounce — bursts of writes coalesce into
        // a single recompute rather than one per file.
        if dirty && set.is_empty() && queue.is_settled() {
            if let Some(refresh) = &kg_refresh {
                refresh();
            }
            dirty = false;
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

    // --- SEM-56: settle → materialize_projection scheduling -----------------
    //
    // `run_graph_worker`'s `kg_refresh` callback (L6) is the hook the daemon
    // uses to call `materialize_projection` once the live KG settles (see
    // `daemon_runtime.rs`'s `refresh_knowledge_graph`, which wraps it). These
    // tests drive that trigger directly: a minimal `LocalIndexer` stands in
    // for the store so the debounce/once-per-settle contract and the actual
    // Leiden-community materialization are both verified without a real
    // gliner/AST extractor or network.

    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct FakeGraphIndexer {
        queue: Arc<GraphQueue>,
        extract_calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::cache::LocalIndexer for FakeGraphIndexer {
        async fn index(&self, _ino: u64, _filepath: &str, _content: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn remove(&self, _filepath: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn rename(&self, _old: &str, _new: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn graph_queue(&self) -> Option<Arc<GraphQueue>> {
            Some(self.queue.clone())
        }
        async fn index_graph(&self, _ino: u64, _filepath: &str) -> anyhow::Result<()> {
            self.extract_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// A burst of enqueued files must coalesce into ONE `kg_refresh` call once
    /// the queue drains and settles — never one call per file. Louvain is
    /// O(graph); per-write materialization is exactly what `materialize_kg`'s
    /// doc comment forbids.
    #[tokio::test]
    async fn settle_runs_kg_refresh_once_per_batch_not_per_file() {
        let queue = GraphQueue::new();
        let extract_calls = Arc::new(AtomicUsize::new(0));
        let indexer: Arc<dyn crate::cache::LocalIndexer> = Arc::new(FakeGraphIndexer {
            queue: queue.clone(),
            extract_calls: extract_calls.clone(),
        });
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let refresh_calls_cb = refresh_calls.clone();
        let kg_refresh: Option<Arc<dyn Fn() + Send + Sync + 'static>> =
            Some(Arc::new(move || {
                refresh_calls_cb.fetch_add(1, Ordering::SeqCst);
            }));

        // Enqueue a burst of 3 files before the worker even starts draining.
        queue.enqueue(1, "/a.go".into());
        queue.enqueue(2, "/b.go".into());
        queue.enqueue(3, "/c.go".into());

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let worker = tokio::spawn(run_graph_worker(indexer, shutdown_rx, kg_refresh));

        // Past the 500ms debounce tick, long enough for the burst to fully
        // drain and the queue to settle at least once.
        tokio::time::sleep(Duration::from_millis(1300)).await;
        shutdown_tx.send(true).unwrap();
        worker.await.unwrap();

        assert_eq!(
            extract_calls.load(Ordering::SeqCst),
            3,
            "all 3 files must still be extracted"
        );
        assert_eq!(
            refresh_calls.load(Ordering::SeqCst),
            1,
            "kg_refresh must fire ONCE per settle, not once per file"
        );
    }

    /// End-to-end (minus a real extractor): wire `kg_refresh` to the actual
    /// `materialize_projection` over an in-memory graph with two disjoint
    /// clusters (mirroring the two-Go-package live E2E). Proves the settle
    /// trigger really does populate `graph_community` — not just that the
    /// callback fires.
    #[tokio::test]
    async fn settle_triggered_refresh_materializes_communities_from_edges() {
        use rusqlite::Connection;
        let conn = Arc::new(parking_lot::Mutex::new(Connection::open_in_memory().unwrap()));
        {
            let c = conn.lock();
            c.execute_batch(
                "CREATE TABLE edges(from_path TEXT, to_path TEXT, edge_kind TEXT, created_at INT, confidence TEXT);
                 CREATE TABLE graph_entity(path TEXT PRIMARY KEY, name TEXT, kind TEXT);
                 CREATE TABLE chunks(id INTEGER PRIMARY KEY, filepath TEXT, text TEXT);
                 CREATE TABLE graph_community(file_path TEXT, community_id INTEGER, is_primary INTEGER DEFAULT 1, PRIMARY KEY(file_path,community_id));
                 CREATE TABLE graph_god_node(community_id INTEGER, entity_path TEXT, rank INTEGER, PRIMARY KEY(community_id,entity_path));",
            )
            .unwrap();
            // Two disjoint clusters, no shared entity between them.
            for (file, ent) in [
                ("/pkgA/widget.go", "Widget"),
                ("/pkgA/widget_store.go", "Widget"),
                ("/pkgB/invoice.go", "Invoice"),
                ("/pkgB/invoice_ledger.go", "Invoice"),
            ] {
                let ent_path = format!("/entities/{ent}");
                c.execute(
                    "INSERT INTO edges(from_path,to_path,edge_kind,created_at,confidence) \
                     VALUES (?1,?2,'Concept',0,'INFERRED')",
                    rusqlite::params![file, ent_path],
                )
                .unwrap();
                c.execute(
                    "INSERT OR REPLACE INTO graph_entity(path,name,kind) VALUES (?1,?2,'Concept')",
                    rusqlite::params![ent_path, ent],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO chunks(filepath,text) VALUES (?1,'x')",
                    rusqlite::params![file],
                )
                .unwrap();
            }
        }

        let queue = GraphQueue::new();
        let indexer: Arc<dyn crate::cache::LocalIndexer> = Arc::new(FakeGraphIndexer {
            queue: queue.clone(),
            extract_calls: Arc::new(AtomicUsize::new(0)),
        });
        let conn_for_refresh = conn.clone();
        let kg_refresh: Option<Arc<dyn Fn() + Send + Sync + 'static>> =
            Some(Arc::new(move || {
                let c = conn_for_refresh.lock();
                super::super::graph_file::materialize_projection(&c)
                    .expect("materialize_projection must not fail on a well-formed graph");
            }));

        // Baseline (pre-settle): the live path's community tables start empty —
        // this is the bug SEM-56 fixes (batch-only materialization).
        {
            let c = conn.lock();
            let n: i64 = c
                .query_row("SELECT COUNT(*) FROM graph_community", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "graph_community must be empty before the graph settles");
        }

        queue.enqueue(1, "/pkgA/widget.go".into());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let worker = tokio::spawn(run_graph_worker(indexer, shutdown_rx, kg_refresh));
        tokio::time::sleep(Duration::from_millis(1000)).await;
        shutdown_tx.send(true).unwrap();
        worker.await.unwrap();

        let c = conn.lock();
        let communities: i64 = c
            .query_row(
                "SELECT COUNT(DISTINCT community_id) FROM graph_community",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let members: i64 = c
            .query_row("SELECT COUNT(*) FROM graph_community", [], |r| r.get(0))
            .unwrap();
        assert!(
            communities >= 2,
            "two disjoint clusters must yield >=2 communities, got {communities}"
        );
        assert_eq!(members, 4, "all 4 files must be projected into a community");
    }
}
