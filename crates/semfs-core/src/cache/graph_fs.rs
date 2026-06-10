//! Graph-as-filesystem read model — the bounded projection the FUSE traversal
//! ops expose so `ls`/`find`/`os.walk` over the `/by-topic/` overlay become a
//! guided knowledge-graph walk instead of a blind tree scan.
//!
//! Pure logic over the persisted projection tables (`graph_community`,
//! `graph_god_node`, `graph_entity`) — no FUSE types here, so it is fully
//! unit-testable. The FS layer (cache/fs.rs) maps these entries onto synthetic
//! inodes. MVP: root → community dirs, community dir → bounded member files.
//! Typed cross-edge symlinks (graph_relation) are a later enrichment.
//!
//! See tickets/ls-kg-semantic-readdir/graph-as-filesystem-traversal.md.

use rusqlite::Connection;
use std::collections::HashSet;

/// Graph-as-filesystem feature switch. `SEMFS_GRAPH_FS=1|on|true|yes` exposes the
/// `/by-topic/` overlay; default OFF so it is A/B-able against the real tree.
pub fn graph_fs_enabled() -> bool {
    matches!(
        std::env::var("SEMFS_GRAPH_FS")
            .ok()
            .map(|s| s.trim().to_ascii_lowercase()),
        Some(ref v) if v == "1" || v == "on" || v == "true" || v == "yes"
    )
}

/// Tunable bounds that keep `os.walk`/`ls -R` finite regardless of graph density.
/// Defaults mirror digest's MAX_TOPICS/FILES_PER_TOPIC thinking; env overrides
/// per workload. Worst-case full-walk size = top_topics × files_per_node.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    /// God-node dirs listed at the overlay root (BFS start points).
    pub top_topics: usize,
    /// Member files (real entries) listed per god-node dir before silent cap.
    pub files_per_node: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Bounds { top_topics: 30, files_per_node: 25 }
    }
}

impl Bounds {
    /// Read bounds from the environment, falling back to [`Default`].
    pub fn from_env() -> Self {
        let d = Bounds::default();
        Bounds {
            top_topics: env_usize("SEMFS_GRAPH_TOP_TOPICS", d.top_topics),
            files_per_node: env_usize("SEMFS_GRAPH_FILES_PER_NODE", d.files_per_node),
        }
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// Synthetic inode base for the graph overlay. Real inodes are SQLite rowids
/// (`< 2^40` even for huge corpora), so `>= 2^48` never collides.
pub const GRAPH_INO_BASE: u64 = 1 << 48;
/// Inode of the `/by-topic` overlay root directory.
pub const BY_TOPIC_INO: u64 = GRAPH_INO_BASE;
/// Name of the overlay root directory under the real mount root.
pub const BY_TOPIC_NAME: &str = "by-topic";

/// True if `ino` is in the synthetic graph-overlay range.
pub fn is_graph_ino(ino: u64) -> bool {
    ino >= GRAPH_INO_BASE
}

/// Synthetic inode for a community god-node directory.
pub fn community_to_ino(community_id: i64) -> u64 {
    GRAPH_INO_BASE + 1 + community_id as u64
}

/// Inverse of [`community_to_ino`]. `None` for the overlay root itself.
pub fn ino_to_community(ino: u64) -> Option<i64> {
    if ino > BY_TOPIC_INO {
        Some((ino - GRAPH_INO_BASE - 1) as i64)
    } else {
        None
    }
}

/// Resolve a member-file entry name within a community dir back to its real
/// corpus path. Entry names may be basename-disambiguated, so we match on the
/// computed entry name (not the raw basename).
pub fn resolve_member(
    conn: &Connection,
    community_id: i64,
    name: &str,
    b: &Bounds,
) -> rusqlite::Result<Option<String>> {
    Ok(graph_community_entries(conn, community_id, b)?
        .into_iter()
        .find(|e| e.name == name)
        .and_then(|e| e.real_path))
}

/// Kind of a synthetic graph entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A community god-node directory (synthetic).
    Dir,
    /// A member file — resolves to a REAL corpus inode for reads.
    File,
}

/// One entry in a graph-overlay directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEntry {
    /// Collision-free path component.
    pub name: String,
    pub kind: EntryKind,
    /// For files: the real corpus path the FS resolves to (so reads hit real
    /// bytes + annotations). None for dirs.
    pub real_path: Option<String>,
    /// For dirs: the community this dir represents. None for files.
    pub community_id: Option<i64>,
}

/// Display label for a community's god-node dir: its rank-0 god-node entity
/// name, sanitized to a single safe path component. Falls back to `topic-<id>`
/// when the community has no usable god-node. NOT collision-resolved — the
/// caller dedups across a listing (see [`graph_root_entries`]).
pub fn community_dir_name(conn: &Connection, community_id: i64) -> String {
    let label: Option<String> = conn
        .query_row(
            "SELECT e.name FROM graph_god_node g JOIN graph_entity e ON e.path = g.entity_path \
             WHERE g.community_id = ?1 ORDER BY g.rank LIMIT 1",
            [community_id],
            |r| r.get::<_, String>(0),
        )
        .ok();
    let sanitized = label.map(|n| sanitize_component(&n)).unwrap_or_default();
    if sanitized.is_empty() {
        format!("topic-{community_id}")
    } else {
        sanitized
    }
}

/// Overlay root: one dir per community, largest-first (community_id order, since
/// id 0 = largest at materialize time), capped at `top_topics`. Names are
/// deduplicated within the listing.
pub fn graph_root_entries(conn: &Connection, b: &Bounds) -> rusqlite::Result<Vec<GraphEntry>> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT community_id FROM graph_community ORDER BY community_id LIMIT ?1")?;
    let cids: Vec<i64> = stmt
        .query_map([b.top_topics as i64], |r| r.get::<_, i64>(0))?
        .filter_map(|r| r.ok())
        .collect();

    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(cids.len());
    for cid in cids {
        let base = community_dir_name(conn, cid);
        // disambiguate colliding labels by community id (POSIX: unique dentries)
        let name = if seen.contains(&base) {
            format!("{base}-{cid}")
        } else {
            base
        };
        seen.insert(name.clone());
        out.push(GraphEntry {
            name,
            kind: EntryKind::Dir,
            real_path: None,
            community_id: Some(cid),
        });
    }
    Ok(out)
}

/// A community god-node dir: its member files as REAL entries, sorted, capped at
/// `files_per_node`. Excess files stay reachable via `semfs grep` (silent cap —
/// no fake "+N more" dentry, keeping the listing POSIX-clean).
pub fn graph_community_entries(
    conn: &Connection,
    community_id: i64,
    b: &Bounds,
) -> rusqlite::Result<Vec<GraphEntry>> {
    let mut stmt = conn.prepare(
        "SELECT file_path FROM graph_community WHERE community_id = ?1 ORDER BY file_path LIMIT ?2",
    )?;
    let paths: Vec<String> = stmt
        .query_map(params_cid_limit(community_id, b.files_per_node), |r| {
            r.get::<_, String>(0)
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Files from different dirs can share a basename; a POSIX directory cannot
    // hold two entries with the same name, so disambiguate collisions.
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(paths.len());
    for fp in paths {
        let base = fp.rsplit('/').next().unwrap_or(&fp).to_string();
        let name = uniquify(&base, &mut seen);
        out.push(GraphEntry {
            name,
            kind: EntryKind::File,
            real_path: Some(fp),
            community_id: None,
        });
    }
    Ok(out)
}

/// `(community_id, limit)` params with the limit typed for SQLite.
fn params_cid_limit(cid: i64, limit: usize) -> [i64; 2] {
    [cid, limit as i64]
}

/// Return `base` if unused, else `base` with a numeric suffix before any
/// extension (`report.txt` → `report-1.txt`), recording the chosen name.
fn uniquify(base: &str, seen: &mut HashSet<String>) -> String {
    if seen.insert(base.to_string()) {
        return base.to_string();
    }
    let (stem, ext) = match base.rsplit_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (base, String::new()),
    };
    let mut i = 1;
    loop {
        let cand = format!("{stem}-{i}{ext}");
        if seen.insert(cand.clone()) {
            return cand;
        }
        i += 1;
    }
}

/// Sanitize a god-node label into one safe path component (no `/`, no NUL, no
/// surrounding whitespace). Empty → caller substitutes a fallback.
fn sanitize_component(label: &str) -> String {
    label
        .replace(['/', '\0', '\n', '\r'], "_")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn seed(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE graph_community(file_path TEXT, community_id INTEGER, is_primary INTEGER DEFAULT 1, PRIMARY KEY(file_path,community_id));
             CREATE TABLE graph_god_node(community_id INTEGER, entity_path TEXT, rank INTEGER, PRIMARY KEY(community_id,entity_path));
             CREATE TABLE graph_entity(path TEXT PRIMARY KEY, name TEXT, kind TEXT);",
        )
        .unwrap();
    }

    fn add_community(conn: &Connection, cid: i64, label_path: &str, label_name: &str, files: &[&str]) {
        for f in files {
            conn.execute(
                "INSERT INTO graph_community(file_path,community_id,is_primary) VALUES (?1,?2,1)",
                params![f, cid],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO graph_god_node(community_id,entity_path,rank) VALUES (?1,?2,0)",
            params![cid, label_path],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO graph_entity(path,name,kind) VALUES (?1,?2,'Concept')",
            params![label_path, label_name],
        )
        .unwrap();
    }

    #[test]
    fn root_lists_communities_largest_first_as_dirs() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        add_community(&conn, 0, "/memories/sales.md", "成交金额", &["/a.txt", "/b.txt", "/c.txt"]);
        add_community(&conn, 1, "/memories/tao.md", "taobao", &["/x.md", "/y.md"]);

        let entries = graph_root_entries(&conn, &Bounds::default()).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.kind == EntryKind::Dir));
        assert_eq!(entries[0].name, "成交金额", "community 0 (largest) first");
        assert_eq!(entries[0].community_id, Some(0));
        assert_eq!(entries[1].name, "taobao");
    }

    #[test]
    fn root_caps_at_top_topics() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        add_community(&conn, 0, "/memories/a.md", "topic-a", &["/a.txt"]);
        add_community(&conn, 1, "/memories/b.md", "topic-b", &["/b.txt"]);
        add_community(&conn, 2, "/memories/c.md", "topic-c", &["/c.txt"]);
        let b = Bounds { top_topics: 2, files_per_node: 25 };
        let entries = graph_root_entries(&conn, &b).unwrap();
        assert_eq!(entries.len(), 2, "capped at top_topics");
        assert_eq!(entries[0].community_id, Some(0));
        assert_eq!(entries[1].community_id, Some(1));
    }

    #[test]
    fn root_dedups_colliding_labels() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        add_community(&conn, 0, "/memories/x0.md", "报告", &["/a.txt"]);
        add_community(&conn, 1, "/memories/x1.md", "报告", &["/b.txt"]);
        let entries = graph_root_entries(&conn, &Bounds::default()).unwrap();
        let names: HashSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names.len(), 2, "colliding labels disambiguated");
        assert!(entries.iter().any(|e| e.name == "报告"));
        assert!(entries.iter().any(|e| e.name == "报告-1"));
    }

    #[test]
    fn community_lists_member_files_as_real_entries_sorted() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        add_community(&conn, 0, "/memories/s.md", "sales", &["/c.txt", "/a.txt", "/b.txt"]);
        let entries = graph_community_entries(&conn, 0, &Bounds::default()).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().all(|e| e.kind == EntryKind::File));
        // sorted; real_path points at the corpus file
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].real_path.as_deref(), Some("/a.txt"));
        assert_eq!(entries[2].real_path.as_deref(), Some("/c.txt"));
    }

    #[test]
    fn community_caps_files_per_node() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        let files: Vec<String> = (0..50).map(|i| format!("/f{i:02}.txt")).collect();
        let refs: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
        add_community(&conn, 0, "/memories/s.md", "sales", &refs);
        let b = Bounds { top_topics: 30, files_per_node: 25 };
        let entries = graph_community_entries(&conn, 0, &b).unwrap();
        assert_eq!(entries.len(), 25, "member files capped");
    }

    #[test]
    fn full_walk_is_bounded_and_each_file_appears_once() {
        // Hard-partition guarantee: a recursive walk over root → communities →
        // files yields each file exactly once and never exceeds the caps.
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        add_community(&conn, 0, "/memories/s.md", "sales", &["/a.txt", "/b.txt"]);
        add_community(&conn, 1, "/memories/t.md", "tao", &["/x.md", "/y.md"]);
        let b = Bounds::default();
        let mut seen: Vec<String> = Vec::new();
        for dir in graph_root_entries(&conn, &b).unwrap() {
            let cid = dir.community_id.unwrap();
            for f in graph_community_entries(&conn, cid, &b).unwrap() {
                seen.push(f.real_path.unwrap());
            }
        }
        seen.sort();
        let uniq: HashSet<&String> = seen.iter().collect();
        assert_eq!(seen.len(), uniq.len(), "each file appears exactly once");
        assert_eq!(seen.len(), 4);
        assert!(seen.len() <= b.top_topics * b.files_per_node, "bounded by caps");
    }

    #[test]
    fn community_disambiguates_files_with_same_basename() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        add_community(&conn, 0, "/memories/s.md", "sales", &["/x/report.txt", "/y/report.txt"]);
        let entries = graph_community_entries(&conn, 0, &Bounds::default()).unwrap();
        let names: HashSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names.len(), 2, "same basename must not produce duplicate dentries");
        assert!(names.contains("report.txt"));
        assert!(names.contains("report-1.txt"));
        // real_path still points at the true distinct files
        let reals: HashSet<&str> =
            entries.iter().filter_map(|e| e.real_path.as_deref()).collect();
        assert!(reals.contains("/x/report.txt") && reals.contains("/y/report.txt"));
    }

    #[test]
    fn inode_codec_separates_synthetic_from_real_and_roundtrips() {
        assert!(!is_graph_ino(1));
        assert!(!is_graph_ino(1_000_000));
        assert!(is_graph_ino(BY_TOPIC_INO));
        assert_eq!(ino_to_community(BY_TOPIC_INO), None, "overlay root is not a community");
        for cid in [0i64, 1, 5, 158] {
            let ino = community_to_ino(cid);
            assert!(is_graph_ino(ino));
            assert_eq!(ino_to_community(ino), Some(cid));
        }
    }

    #[test]
    fn resolve_member_maps_disambiguated_name_to_real_path() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        add_community(&conn, 0, "/memories/s.md", "sales", &["/x/report.txt", "/y/report.txt"]);
        let b = Bounds::default();
        assert_eq!(
            resolve_member(&conn, 0, "report.txt", &b).unwrap().as_deref(),
            Some("/x/report.txt")
        );
        assert_eq!(
            resolve_member(&conn, 0, "report-1.txt", &b).unwrap().as_deref(),
            Some("/y/report.txt")
        );
        assert_eq!(resolve_member(&conn, 0, "nope.txt", &b).unwrap(), None);
    }

    #[test]
    fn dir_name_falls_back_when_no_god_node() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        // community 7 has member files but no god-node row
        conn.execute(
            "INSERT INTO graph_community(file_path,community_id,is_primary) VALUES ('/a.txt',7,1)",
            [],
        )
        .unwrap();
        assert_eq!(community_dir_name(&conn, 7), "topic-7");
    }
}
