//! Local SQLite cache.
//!
//! Persists inodes, dentries, file chunks, and sync state between daemon
//! restarts.
//!
//! The cache is a *passive store*: it never calls the API or spawns
//! background tasks. The sync engine (in [`crate::sync`]) is the only
//! thing that mutates sync-state fields (added in M7–M8).

pub(crate) mod db;
mod file;
mod fs;
pub mod graph_queue;
pub mod hydration;

pub use db::{is_macos_noise_path, Db, DEFAULT_CHUNK_SIZE, DENTRY_CACHE_MAX, ROOT_INO};
pub use fs::{ReconcileOutcome, CacheFs};
pub use graph_queue::{run_graph_worker, GraphQueue};
pub use hydration::{HydrationKey, HydrationScheduler};

pub(crate) use fs::parse_iso_to_ms;

/// Maintains the local semantic search index as files change.
///
/// Defined here (not in [`crate::backend`]) so the cache write path can drive
/// indexing without a module cycle — `backend::SqliteVecStore` already depends
/// on the cache, so it implements this trait rather than the cache importing it.
///
/// Async because backends differ: `SqliteVecStore` is sync (rusqlite) and wraps
/// its sync methods, while `PgVectorStore` is async (sqlx). All write-path call
/// sites (`flush`, `unlink`, `rename`) are already `async fn`s, so awaiting here
/// is natural — and it lets an async backend participate without a `block_on`
/// hack (which would panic from inside the existing runtime).
#[async_trait::async_trait]
pub trait LocalIndexer: Send + Sync + std::fmt::Debug {
    /// (Re)index a file's content; replaces any prior chunks for `filepath`.
    async fn index(&self, ino: u64, filepath: &str, content: &str) -> anyhow::Result<()>;
    /// Drop a file's chunks from the index (on delete or rename-away).
    async fn remove(&self, filepath: &str) -> anyhow::Result<()>;
    /// Relabel a file's index rows from `old` → `new` (on rename). Any rows the
    /// destination already had are dropped first (overwrite). No re-embedding —
    /// the content is unchanged, only its path.
    async fn rename(&self, old: &str, new: &str) -> anyhow::Result<()>;

    /// The pending L7-extraction queue, if this indexer has an entity-graph LLM
    /// attached. `None` (the default) → no graph work, so `run_graph_worker`
    /// exits immediately. `index()` enqueues here after writing a file's
    /// vectors; the worker drains it with bounded concurrency.
    fn graph_queue(&self) -> Option<std::sync::Arc<graph_queue::GraphQueue>> {
        None
    }

    /// Extract entities for one file (reading its content from the local store)
    /// and write its graph `edges`. Called by `run_graph_worker` OFF the write
    /// path, so the per-file blocking LLM call no longer serializes indexing.
    /// Default no-op for indexers without a graph extractor.
    async fn index_graph(&self, _ino: u64, _filepath: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
