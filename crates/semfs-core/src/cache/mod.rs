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
pub mod hydration;
pub mod profile;

pub use db::{is_macos_noise_path, Db, DEFAULT_CHUNK_SIZE, DENTRY_CACHE_MAX, ROOT_INO};
pub use fs::{ReconcileOutcome, CacheFs};
pub use hydration::{HydrationKey, HydrationScheduler};

pub(crate) use fs::parse_iso_to_ms;

/// Maintains the local semantic search index as files change.
///
/// Defined here (not in [`crate::backend`]) so the cache write path can drive
/// indexing without a module cycle — `backend::SqliteVecStore` already depends
/// on the cache, so it implements this trait rather than the cache importing it.
pub trait LocalIndexer: Send + Sync + std::fmt::Debug {
    /// (Re)index a file's content; replaces any prior chunks for `filepath`.
    fn index(&self, ino: u64, filepath: &str, content: &str) -> anyhow::Result<()>;
    /// Drop a file's chunks from the index (on delete or rename-away).
    fn remove(&self, filepath: &str) -> anyhow::Result<()>;
    /// Relabel a file's index rows from `old` → `new` (on rename). Any rows the
    /// destination already had are dropped first (overwrite). No re-embedding —
    /// the content is unchanged, only its path.
    fn rename(&self, old: &str, new: &str) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests;
