//! Configuration and XDG paths.
//!
//! Resolves cache database location, log file paths, and IPC socket paths
//! per operating system. Uses the `directories` crate so we don't branch
//! on OS manually.

pub mod credentials;

use std::path::PathBuf;

/// Return the platform-appropriate cache directory for semfs.
///
/// - Linux: `$XDG_CACHE_HOME/semfs` (usually `~/.cache/semfs`)
/// - macOS: `~/Library/Caches/semfs`
pub fn cache_dir() -> PathBuf {
    directories::ProjectDirs::from("ai", "supermemory", "semfs")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| {
            // Fallback if home directory can't be determined.
            PathBuf::from("/tmp/semfs")
        })
}

/// True if `s` is safe to use as a SINGLE filesystem path component: non-empty,
/// not `.`/`..`, and free of path separators, NUL, or absolute-path markers.
///
/// `org_id` arrives from the `/v3/session` response and `container_tag` from the
/// CLI; both get joined into cache paths that `--clean`/ephemeral cleanup feed to
/// `remove_dir_all`. A value like `..` or `a/b` would escape the cache subtree and
/// delete an unintended location — so callers MUST validate before building a
/// destructive path from untrusted input.
pub fn is_safe_path_component(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains('\0')
}

/// Fixed, key-independent local home for semfs state: `~/.semfs`.
///
/// The local SQLite cache lives directly under here (`~/.semfs/<tag>.db`), so a
/// local mount NEVER needs an `org_id` to locate its cache — hence no
/// `/v3/session` round-trip, no valid Supermemory key, and offline-capable.
/// (Replaces the old `<cache>/<org_id>/<tag>.db` layout; see
/// `tickets/decouple-sqlite-cache-scoping-from-supermemory/`.)
pub fn semfs_home() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().join(".semfs"))
        .unwrap_or_else(|| PathBuf::from("/tmp/semfs"))
}

/// Local SQLite cache path: `~/.semfs/<container_tag>.db`. Org-independent — the
/// `container_tag` alone namespaces the file (it's validated as a safe path
/// component at CLI parse time).
pub fn cache_db_path(container_tag: &str) -> PathBuf {
    semfs_home().join(format!("{container_tag}.db"))
}

pub fn legacy_cache_db_path(container_tag: &str) -> PathBuf {
    cache_dir().join(format!("{container_tag}.db"))
}

/// Path to the daemon log file.
pub fn daemon_log_path() -> PathBuf {
    cache_dir().join("daemon.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_db_path_is_under_semfs_home_and_tagged() {
        let path = cache_db_path("my-tag");
        assert_eq!(path.parent().unwrap(), semfs_home());
        assert_eq!(path.file_name().unwrap(), "my-tag.db");
    }

    #[test]
    fn cache_db_path_is_org_independent() {
        // Fixed root: the same tag always resolves to the same path, with no org
        // dimension. (Regression guard for the decouple-from-Supermemory fix.)
        assert_eq!(cache_db_path("work"), semfs_home().join("work.db"));
    }

    #[test]
    fn cache_db_path_different_tags_differ() {
        assert_ne!(cache_db_path("a"), cache_db_path("b"));
    }

    #[test]
    fn safe_path_component_accepts_real_ids() {
        assert!(is_safe_path_component("org_abc123"));
        assert!(is_safe_path_component("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_safe_path_component("_ephemeral"));
    }

    #[test]
    fn safe_path_component_rejects_escapes() {
        assert!(!is_safe_path_component(""));
        assert!(!is_safe_path_component("."));
        assert!(!is_safe_path_component(".."));
        assert!(!is_safe_path_component("a/b"));
        assert!(!is_safe_path_component("../etc"));
        assert!(!is_safe_path_component("a\\b"));
        assert!(!is_safe_path_component("a\0b"));
    }
}
