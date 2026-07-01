-- semfs local cache schema.

-- Inode metadata. Every file, directory, and symlink gets a row here.
-- ino is AUTOINCREMENT so inode numbers are never reused.
-- dirty_since: epoch-ms when the user last wrote this inode locally; pull reconciler
-- skips an inode whose dirty_since is newer than the remote updatedAt (local wins).
CREATE TABLE IF NOT EXISTS fs_inode (
    ino          INTEGER PRIMARY KEY AUTOINCREMENT,
    mode         INTEGER NOT NULL,
    nlink        INTEGER NOT NULL DEFAULT 0,
    uid          INTEGER NOT NULL DEFAULT 0,
    gid          INTEGER NOT NULL DEFAULT 0,
    size         INTEGER NOT NULL DEFAULT 0,
    atime        INTEGER NOT NULL,
    mtime        INTEGER NOT NULL,
    ctime        INTEGER NOT NULL,
    rdev         INTEGER NOT NULL DEFAULT 0,
    atime_nsec   INTEGER NOT NULL DEFAULT 0,
    mtime_nsec   INTEGER NOT NULL DEFAULT 0,
    ctime_nsec   INTEGER NOT NULL DEFAULT 0,
    dirty_since  INTEGER,
    derived      INTEGER NOT NULL DEFAULT 0
);

-- Directory entries: maps (parent_ino, name) → child ino.
CREATE TABLE IF NOT EXISTS fs_dentry (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    parent_ino INTEGER NOT NULL,
    ino        INTEGER NOT NULL,
    UNIQUE(parent_ino, name)
);
CREATE INDEX IF NOT EXISTS idx_dentry_parent ON fs_dentry(parent_ino, name);

-- Chunked file data. Files are split into fixed-size chunks (default 4096).
CREATE TABLE IF NOT EXISTS fs_data (
    ino         INTEGER NOT NULL,
    chunk_index INTEGER NOT NULL,
    data        BLOB    NOT NULL,
    PRIMARY KEY (ino, chunk_index)
);

-- Symlink targets.
CREATE TABLE IF NOT EXISTS fs_symlink (
    ino    INTEGER PRIMARY KEY,
    target TEXT NOT NULL
);

-- Key-value configuration (chunk_size, schema_version, etc.).
CREATE TABLE IF NOT EXISTS fs_config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Remote document ID tracking. Maps local inode → Supermemory API document ID.
-- Populated on first successful flush (POST) and on pull reconciliation. Used
-- for subsequent updates (PATCH) and for delta-pull version comparison via
-- mirrored_updated_at.
CREATE TABLE IF NOT EXISTS fs_remote (
    ino                  INTEGER PRIMARY KEY,
    remote_id            TEXT    NOT NULL,
    mirrored_updated_at  INTEGER,
    last_status          TEXT,
    last_status_at       INTEGER
);

-- Persistent push queue. One row per filepath enforces latest-wins coalescing:
-- if a write arrives while another write for the same filepath is queued (but
-- not yet inflight), the new write replaces it. If the earlier write IS
-- inflight, the new write sits in the pending_* columns and promotes once
-- the inflight op finishes.
CREATE TABLE IF NOT EXISTS push_queue (
    filepath             TEXT PRIMARY KEY,
    op                   TEXT NOT NULL,
    content_ino          INTEGER,
    rename_to            TEXT,
    remote_id            TEXT,              -- known at enqueue for update/delete/rename; NULL for pure create
    inflight_started_at  INTEGER,           -- non-NULL marks this row as currently being sent
    pending_op           TEXT,
    pending_content_ino  INTEGER,
    pending_rename_to    TEXT,
    last_error           TEXT,
    attempt              INTEGER NOT NULL DEFAULT 0,
    updated_at           INTEGER NOT NULL,
    poisoned             INTEGER NOT NULL DEFAULT 0,
    last_status          INTEGER
);
CREATE INDEX IF NOT EXISTS idx_push_queue_updated ON push_queue(updated_at);

-- General KV for sync timestamps and ID-set snapshots.
--   last_seen_updated_at   — watermark for delta pull (loop A)
--   last_scan_total_items  — for skip-if-unchanged deletion scan (loop C)
CREATE TABLE IF NOT EXISTS sync_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- ── Local semantic index (Phase 2) ──────────────────────────────────────────
-- Dimension-INDEPENDENT tables only. The vec0 vector tables (vchunks /
-- vchunks_code) bind a float[N] width, so they are created at runtime from the
-- configured embedder dims — see Db::ensure_vector_tables. Mirrors the TS
-- SqliteVecStore split (chunks/fts5 static; vec0 runtime).

-- One row per chunk of an indexed file. `ino` ties the chunk back to its
-- inode (content lives in fs_data); `filepath` is denormalized so search can
-- return paths without a dentry walk; `ord` is the chunk's position. Chunks are
-- re-derived on write (DELETE WHERE filepath=? then re-insert) and removed on delete.
CREATE TABLE IF NOT EXISTS chunks (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    ino              INTEGER NOT NULL,
    filepath         TEXT    NOT NULL,
    ord              INTEGER NOT NULL,
    text             TEXT    NOT NULL,
    -- L6 salience stats: stamped on write, bumped on search hit.
    last_accessed_at INTEGER,
    access_count     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_chunks_ino ON chunks(ino);
CREATE INDEX IF NOT EXISTS idx_chunks_filepath ON chunks(filepath);

-- L7 entity/link graph: typed edges between files, re-derived on write and
-- removed on delete (mutable-FS substrate — no temporal/versioning).
-- `confidence` is categorical (EXTRACTED / INFERRED / AMBIGUOUS); today the LLM
-- path writes INFERRED — see tickets/ls-kg-semantic-readdir/graphify_kg_architecture.md.
CREATE TABLE IF NOT EXISTS edges (
    from_path  TEXT NOT NULL,
    to_path    TEXT NOT NULL,
    edge_kind  TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    confidence TEXT NOT NULL DEFAULT 'INFERRED',
    PRIMARY KEY (from_path, to_path, edge_kind)
);
CREATE INDEX IF NOT EXISTS idx_edges_to ON edges(to_path);

-- Graphify KG: recover an entity's original display name + type from its node
-- path (`/memories/<slug>.md`). `slugify` is lossy (CJK → e-<hash>), so the raw
-- name is stored here at extraction time for god-node labels in KNOWLEDGE_GRAPH.md.
CREATE TABLE IF NOT EXISTS graph_entity (
    path TEXT PRIMARY KEY,   -- = edges.to_path (the /memories/<slug>.md node)
    name TEXT NOT NULL,      -- original entity name (CJK preserved)
    kind TEXT NOT NULL,      -- ontology type (Person/Organization/Concept/…)
    file_type TEXT,          -- graphify: code|document|paper|image (nullable)
    source_file TEXT,        -- file the entity was first extracted from (nullable)
    rationale TEXT           -- graphify: design-intent text on the node (nullable)
);

-- Graphify-parity entity↔entity relationship graph (typed, directed). Distinct
-- from `edges` (which is the file→entity co-mention graph). source/target are
-- `/memories/<slug>.md` node paths. Re-derived on rebuild; weight = strength.
CREATE TABLE IF NOT EXISTS graph_relation (
    source           TEXT NOT NULL,   -- /memories/<slug>.md
    target           TEXT NOT NULL,   -- /memories/<slug>.md
    relation         TEXT NOT NULL,   -- calls|cites|conceptually_related_to|… (RELATION_TYPES)
    confidence       TEXT NOT NULL DEFAULT 'INFERRED',  -- EXTRACTED|INFERRED|AMBIGUOUS
    confidence_score REAL NOT NULL DEFAULT 0.5,
    source_file      TEXT,            -- file the relation was extracted from
    source_location  TEXT,
    weight           REAL NOT NULL DEFAULT 1.0,
    created_at       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (source, target, relation)
);
CREATE INDEX IF NOT EXISTS idx_graph_relation_src ON graph_relation(source);
CREATE INDEX IF NOT EXISTS idx_graph_relation_tgt ON graph_relation(target);

-- Materialized Louvain projection (graph-as-filesystem). The community→god-node→
-- member-file skeleton is otherwise recomputed ephemerally inside build_digest();
-- the FS traversal ops (readdir/lookup) can't run Louvain per `ls`, so the
-- projection is persisted here once at KG-refresh time and read cheaply.
-- See tickets/ls-kg-semantic-readdir/graph-as-filesystem-traversal.md §4.4.
-- Louvain is a hard partition, so each file maps to exactly one community
-- (is_primary=1); the column reserves future multi-membership without churn.
CREATE TABLE IF NOT EXISTS graph_community (
    file_path    TEXT    NOT NULL,
    community_id INTEGER NOT NULL,
    is_primary   INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (file_path, community_id)
);
CREATE INDEX IF NOT EXISTS idx_graph_community_cid ON graph_community(community_id);

-- Ranked god-nodes (central entities) per community — labels the god-node dir
-- and seeds cross-edge navigation. rank 0 = most central (the dir's display name).
CREATE TABLE IF NOT EXISTS graph_god_node (
    community_id INTEGER NOT NULL,
    entity_path  TEXT    NOT NULL,   -- = graph_entity.path (/memories/<slug>.md)
    rank         INTEGER NOT NULL,
    PRIMARY KEY (community_id, entity_path)
);
CREATE INDEX IF NOT EXISTS idx_graph_god_node_cid ON graph_god_node(community_id);

-- BM25 keyword index over chunk text. rowid is kept equal to chunks.id so the
-- vec0 KNN and fts5 BM25 result sets join back to the same chunk.
CREATE VIRTUAL TABLE IF NOT EXISTS ffts USING fts5(text);

-- L1 parse accounting: binary files whose content could not be extracted to
-- searchable text (unsupported format, parse failure, OCR key absent). Keyed by
-- inode so a later successful flush or an unlink clears the row. Surfaced as
-- `unindexed_files` in `semfs status` — no binary is ever silently dropped.
CREATE TABLE IF NOT EXISTS fs_unindexed (
    ino       INTEGER PRIMARY KEY,
    filepath  TEXT    NOT NULL,
    format    TEXT    NOT NULL,
    ts        INTEGER NOT NULL
);
