//! The [`FileSystem`] and [`File`] traits — the core filesystem abstraction.
//!
//! Every backend in semfs (`MemFs` in-memory, `CacheFs` SQLite-
//! backed in M5, future experiments) implements these traits. Every frontend
//! (FUSE and NFS mount adapters in M3) calls into them.

use std::sync::Arc;

use async_trait::async_trait;

use super::error::VfsResult;
use super::types::{DirEntry, FileAttr, FilesystemStats, SetAttr};

/// The filesystem trait, defined in terms of inode numbers (POSIX/FUSE semantics).
///
/// All methods are asynchronous to accommodate backends that need to perform
/// network or disk I/O (e.g. the SQLite-backed implementation that fronts the
/// Supermemory API). In-memory backends like `MemFs` fulfil the async contract
/// without actually awaiting anything.
///
/// Methods that return `Option<T>` use `Ok(None)` to indicate "not found" as a
/// normal outcome. Actual failures (I/O errors, invalid inputs) return `Err`.
#[async_trait]
pub trait FileSystem: Send + Sync {
    // ─── Lookup and metadata ────────────────────────────────────────────────

    /// Resolve a name inside a parent directory to its attributes.
    ///
    /// Returns `Ok(None)` if the entry does not exist in the parent directory.
    async fn lookup(&self, parent_ino: u64, name: &str) -> VfsResult<Option<FileAttr>>;

    /// Get attributes for an inode by ID.
    ///
    /// Returns `Ok(None)` if the inode does not exist.
    async fn getattr(&self, ino: u64) -> VfsResult<Option<FileAttr>>;

    /// Update attributes on an inode. Only fields set to `Some` in [`SetAttr`]
    /// are changed; others are preserved.
    async fn setattr(&self, ino: u64, attr: SetAttr) -> VfsResult<FileAttr>;

    // ─── Directories ────────────────────────────────────────────────────────

    /// List names in a directory.
    ///
    /// Returns `Ok(None)` if `ino` does not exist or is not a directory.
    /// Names are returned in sorted order for determinism.
    async fn readdir(&self, ino: u64) -> VfsResult<Option<Vec<String>>>;

    /// List directory entries with full attributes in one call.
    ///
    /// Equivalent to calling [`readdir`](Self::readdir) followed by
    /// [`getattr`](Self::getattr) for each name, but avoids the N+1 round-trip
    /// pattern. Returns `Ok(None)` if `ino` does not exist or is not a directory.
    async fn readdir_plus(&self, ino: u64) -> VfsResult<Option<Vec<DirEntry>>>;

    /// Create a new directory inside `parent_ino`.
    async fn mkdir(
        &self,
        parent_ino: u64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> VfsResult<FileAttr>;

    /// Remove an empty directory.
    async fn rmdir(&self, parent_ino: u64, name: &str) -> VfsResult<()>;

    // ─── File handles ───────────────────────────────────────────────────────

    /// Open an existing file and return a handle for I/O operations.
    ///
    /// `flags` carries POSIX open flags (e.g. `O_RDONLY`, `O_RDWR`). Backends
    /// may use them for permission checks or caching decisions.
    async fn open(&self, ino: u64, flags: i32) -> VfsResult<BoxedFile>;

    /// Create a new regular file and return both its attributes and an open handle.
    async fn create_file(
        &self,
        parent_ino: u64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> VfsResult<(FileAttr, BoxedFile)>;

    /// Remove a file (non-directory) from a directory.
    ///
    /// If the target is the last name referencing its inode, the inode and its
    /// contents are freed. Otherwise, link count is decremented.
    async fn unlink(&self, parent_ino: u64, name: &str) -> VfsResult<()>;

    // ─── Symbolic and hard links ────────────────────────────────────────────

    /// Read the target path of a symbolic link.
    ///
    /// Returns `Ok(None)` if `ino` does not exist; returns an error if `ino`
    /// exists but is not a symbolic link.
    async fn readlink(&self, ino: u64) -> VfsResult<Option<String>>;

    /// Create a symbolic link named `name` in `parent_ino` pointing to `target`.
    async fn symlink(
        &self,
        parent_ino: u64,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> VfsResult<FileAttr>;

    /// Create a hard link — a new directory entry referencing an existing inode.
    ///
    /// Returns the updated attributes of the linked inode (with incremented
    /// `nlink`).
    async fn link(&self, ino: u64, new_parent_ino: u64, new_name: &str) -> VfsResult<FileAttr>;

    // ─── Rename ─────────────────────────────────────────────────────────────

    /// Move or rename an entry from one parent+name to another.
    ///
    /// If the destination already exists, the rename replaces it according to
    /// POSIX semantics: file-over-file replaces, directory-over-directory
    /// requires the destination to be empty, and mixing types is an error.
    async fn rename(
        &self,
        old_parent_ino: u64,
        old_name: &str,
        new_parent_ino: u64,
        new_name: &str,
    ) -> VfsResult<()>;

    // ─── Filesystem-wide ────────────────────────────────────────────────────

    /// Report filesystem-level statistics (backs `statfs(2)` / `df`).
    async fn statfs(&self) -> VfsResult<FilesystemStats>;

    /// Kernel cache hint: the kernel is releasing `nlookup` references to `ino`.
    ///
    /// Backends that cache per-inode resources (file descriptors, pinned data)
    /// should decrement a reference count here. The default is a no-op,
    /// suitable for in-memory and database-backed backends.
    async fn forget(&self, _ino: u64, _nlookup: u64) {}
}

/// A handle to an open file, returned by [`FileSystem::open`] and
/// [`FileSystem::create_file`].
///
/// Operations on a handle don't require a path or parent lookup — the
/// identity of the open file was resolved when the handle was created.
///
/// Implementations must derive [`Debug`](std::fmt::Debug) so that
/// [`Result::unwrap_err`] and similar formatting on `Result<BoxedFile, _>`
/// works without compiler errors in tests.
#[async_trait]
pub trait File: Send + Sync + std::fmt::Debug {
    /// Read up to `size` bytes starting at `offset` (POSIX `pread` semantics).
    ///
    /// Returns fewer bytes than requested if end-of-file is reached. An empty
    /// vector means `offset` is at or past the end of the file.
    async fn read(&self, offset: u64, size: usize) -> VfsResult<Vec<u8>>;

    /// Write `data` starting at `offset` (POSIX `pwrite` semantics).
    ///
    /// Returns the number of bytes actually written. Extends the file if
    /// `offset + data.len()` exceeds the current size.
    async fn write(&self, offset: u64, data: &[u8]) -> VfsResult<u32>;

    /// Truncate or zero-extend the file to exactly `size` bytes.
    async fn truncate(&self, size: u64) -> VfsResult<()>;

    /// Flush any buffered writes to the backend's primary storage.
    ///
    /// No-op for `MemFs`; meaningful for cached backends.
    async fn flush(&self) -> VfsResult<()>;

    /// Synchronise file data to durable storage (backs `fsync(2)`).
    async fn fsync(&self) -> VfsResult<()>;

    /// Read current attributes of the open file.
    async fn getattr(&self) -> VfsResult<FileAttr>;
}

/// A shareable, reference-counted file handle.
///
/// `Arc` rather than `Box` so the handle can be cloned across async tasks
/// (the FUSE and NFS mount adapters need this — they dispatch reads and
/// writes to worker tasks without moving ownership).
pub type BoxedFile = Arc<dyn File + Send + Sync>;
