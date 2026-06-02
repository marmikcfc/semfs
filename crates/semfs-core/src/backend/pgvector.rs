//! Postgres / pgvector backend (Phase 6) — behind the `pg-local` feature.
//!
//! Reuses the backend-agnostic layers (chunk, embed, rerank, graph, rank) over a
//! Postgres store: `vector` for KNN, `tsvector` for keyword. The multi-writer
//! tier — where SQLite's single-writer ceiling hurts. Vectors are bound as text
//! cast to `::vector` (no `pgvector` crate) to keep one sqlx version.
//!
//! We hold a single `PgConnection` behind a `Mutex` rather than a `PgPool`: the
//! embedded pglite-oxide server (WASM, single-threaded) serves one connection at
//! a time, and a real Postgres is equally happy with a serialized connection for
//! this workload. Wiring `PgVectorStore` into the CLI resolver / `CacheFs` write
//! path (which needs an async `LocalIndexer`) is a documented follow-on.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{Connection, PgConnection, Row};
use tokio::sync::Mutex;

use super::{chunk, graph, rank, SearchHit, SemanticIndex};
use crate::embed::Embedder;
use crate::llm::LlmClient;
use crate::rerank::Reranker;

/// Over-fetch per ranked list before collapsing chunks → files.
const SEARCH_POOL: i64 = 80;

/// Format an f32 vector as a pgvector text literal: `[1,2,3]`.
pub(crate) fn vec_literal(v: &[f32]) -> String {
    let mut s = String::from("[");
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

/// Take a transaction-scoped advisory lock keyed by `(container, filepath)`,
/// released on commit/rollback. Serializes same-file writers across
/// connections/processes within a container.
async fn lock_path(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    container: &str,
    filepath: &str,
) -> anyhow::Result<()> {
    // Lock key includes the container so different mounts sharing one Postgres
    // don't contend (or, worse, serialize) on the same path. `\x1f` (unit
    // separator) can't appear in a validated tag or a real path.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
        .bind(format!("{container}\u{1f}{filepath}"))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Local/cloud-embedder-fed semantic index over Postgres + pgvector.
///
/// One Postgres database can hold many mounts: every row is namespaced by
/// `container` (the mount tag), and every write/search/lock predicate filters on
/// it — so two mounts sharing a `SEMFS_PG_URL` can't read, overwrite, or delete
/// each other's documents even when file paths collide.
pub struct PgVectorStore {
    conn: Mutex<PgConnection>,
    container: String,
    embedder: Arc<dyn Embedder>,
    reranker: Option<Arc<dyn Reranker>>,
    graph_llm: Option<Arc<LlmClient>>,
    /// Pending L7-extraction queue, present iff `graph_llm` is. `index()` enqueues
    /// here after writing chunks; `run_graph_worker` drains it so the per-file
    /// blocking LLM call stays OFF the synchronous index/flush path.
    graph_queue: Option<Arc<crate::cache::GraphQueue>>,
    /// Keeps embedded-backend resources alive for as long as the store lives, in
    /// DROP ORDER (front first): the pglite server is pushed before any temp-dir
    /// cleanup guard, so the server shuts down (releasing its files) BEFORE the
    /// directory is removed. Opaque so the struct stays feature-agnostic; empty
    /// for external Postgres.
    _keepalive: Vec<Box<dyn std::any::Any + Send + Sync>>,
}

impl std::fmt::Debug for PgVectorStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgVectorStore")
            .field("container", &self.container)
            .field("dimensions", &self.embedder.dimensions())
            .field("has_reranker", &self.reranker.is_some())
            .field("has_graph_extractor", &self.graph_llm.is_some())
            .finish()
    }
}

impl PgVectorStore {
    /// Connect and ensure the schema (extension + chunks/edges tables) at the
    /// embedder's vector width. `container` namespaces every row so multiple
    /// mounts can share one database safely. The `vector` extension must be
    /// available in the server (e.g. pglite-oxide started with `extensions::VECTOR`).
    ///
    /// NOTE: the schema is created with the `container` column from the start;
    /// this backend is new, so there's no in-place migration of a pre-container
    /// `chunks`/`edges` table (a stale schema fails fast on the first query —
    /// rebuild the index).
    pub async fn connect(
        database_url: &str,
        container: &str,
        embedder: Arc<dyn Embedder>,
    ) -> anyhow::Result<Self> {
        let mut conn = PgConnection::connect(database_url).await?;
        let dims = embedder.dimensions();
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&mut conn)
            .await?;
        // `dims` is the embedder's vector width (a usize), never user input —
        // safe to interpolate. sqlx 0.9's `query()` requires `&'static str`, so
        // the dynamic CREATE TABLE needs an explicit `AssertSqlSafe`.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE TABLE IF NOT EXISTS chunks (\
                id BIGSERIAL PRIMARY KEY, container TEXT NOT NULL, ino BIGINT NOT NULL, \
                filepath TEXT NOT NULL, ord INT NOT NULL, text TEXT NOT NULL, \
                last_accessed_at BIGINT, access_count BIGINT NOT NULL DEFAULT 0, \
                embedding vector({dims}))"
        )))
        .execute(&mut conn)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_chunks_container_filepath \
             ON chunks(container, filepath)",
        )
        .execute(&mut conn)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_chunks_fts ON chunks \
             USING gin(to_tsvector('simple', text))",
        )
        .execute(&mut conn)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS edges (container TEXT NOT NULL, from_path TEXT NOT NULL, \
             to_path TEXT NOT NULL, edge_kind TEXT NOT NULL, created_at BIGINT NOT NULL, \
             PRIMARY KEY (container, from_path, to_path, edge_kind))",
        )
        .execute(&mut conn)
        .await?;

        // Fail fast on dimension drift. `CREATE TABLE IF NOT EXISTS` silently
        // keeps a pre-existing `chunks` at its old width, so reusing a database
        // with a different embedding model would otherwise defer the failure to
        // the first `::vector` insert/search. pgvector stores the declared
        // dimension directly in `atttypmod` (sized column > 0; unsized = -1).
        let existing: Option<i32> = sqlx::query_scalar(
            "SELECT a.atttypmod FROM pg_attribute a JOIN pg_class c ON a.attrelid = c.oid \
             WHERE c.relname = 'chunks' AND a.attname = 'embedding' \
             AND a.attnum > 0 AND NOT a.attisdropped",
        )
        .fetch_optional(&mut conn)
        .await?;
        if let Some(td) = existing {
            if td > 0 && td as usize != dims {
                anyhow::bail!(
                    "existing chunks.embedding is vector({td}) but the embedder produces \
                     {dims}-dimensional vectors; rebuild the index or use a matching model"
                );
            }
        }

        // Embedder-identity guard (parity with the SQLite backend): a same-WIDTH
        // model swap would otherwise silently search old vectors against the new
        // query space. Stamp the identity on first creation; fail closed if a
        // reopen presents a different model — recovery is a fresh index.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS index_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        )
        .execute(&mut conn)
        .await?;
        let identity = embedder.identity();
        let stored: Option<String> =
            sqlx::query_scalar("SELECT value FROM index_meta WHERE key = 'text_embed_model'")
                .fetch_optional(&mut conn)
                .await?;
        match stored {
            None => {
                sqlx::query(
                    "INSERT INTO index_meta (key, value) VALUES ('text_embed_model', $1)",
                )
                .bind(&identity)
                .execute(&mut conn)
                .await?;
            }
            Some(s) if s != identity => anyhow::bail!(
                "existing index was built with embedder '{s}' but the current embedder is \
                 '{identity}'; rebuild the index or use a matching model"
            ),
            Some(_) => {}
        }

        Ok(Self {
            conn: Mutex::new(conn),
            container: container.to_string(),
            embedder,
            reranker: None,
            graph_llm: None,
            graph_queue: None,
            _keepalive: Vec::new(),
        })
    }

    /// Start an EMBEDDED pglite server (persisting under `data_dir`) with the
    /// `vector` extension, and connect to it — no external Postgres needed. The
    /// server is owned by the returned store (kept alive until the store drops),
    /// so a single process (the daemon) owns the one connection; `grep` reaches
    /// it via IPC, never opening its own. This is how pglite ships in-box.
    #[cfg(feature = "pg-local")]
    pub async fn embedded(
        data_dir: std::path::PathBuf,
        container: &str,
        embedder: Arc<dyn Embedder>,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| anyhow::anyhow!("create pglite data dir {}: {e}", data_dir.display()))?;
        let server = pglite_oxide::PgliteServer::builder()
            .path(data_dir)
            .tcp("127.0.0.1:0".parse().unwrap())
            .extension(pglite_oxide::extensions::VECTOR)
            .start()
            .map_err(|e| anyhow::anyhow!("start embedded pglite: {e}"))?;
        let url = server.database_url();
        let mut store = Self::connect(&url, container, embedder).await?;
        // Server FIRST so it drops (and releases its data-dir files) before any
        // cleanup guard the caller pushes afterward.
        store._keepalive.push(Box::new(server));
        Ok(store)
    }

    /// Attach an extra resource to keep alive for the store's lifetime (e.g. a
    /// temp-dir cleanup guard for an ephemeral embedded mount). Pushed AFTER the
    /// server, so on drop the server shuts down before this guard runs.
    #[cfg(feature = "pg-local")]
    pub fn push_keepalive(&mut self, guard: Box<dyn std::any::Any + Send + Sync>) {
        self._keepalive.push(guard);
    }

    /// Whether this container's index is usable for local search: it must be
    /// NON-EMPTY (a fresh/unpopulated Postgres index would return `Ok([])`, a
    /// false "no results"; the reader should fall back to cloud instead). The
    /// embedder-identity + dimension guards already ran at `connect`.
    pub async fn is_searchable(&self) -> bool {
        let mut conn = self.conn.lock().await;
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM chunks WHERE container = $1)",
        )
        .bind(&self.container)
        .fetch_one(&mut *conn)
        .await
        .unwrap_or(false)
    }

    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    pub fn with_graph_extractor(mut self, llm: Arc<LlmClient>) -> Self {
        self.graph_llm = Some(llm);
        self.graph_queue = Some(crate::cache::GraphQueue::new());
        self
    }

    /// Chunk → embed → write chunks/edges atomically; re-index replaces by path.
    pub async fn index(&self, ino: u64, filepath: &str, content: &str) -> anyhow::Result<()> {
        let chunks = chunk::recursive_chunks(content, &chunk::ChunkOptions::default());
        let vectors = self.embedder.embed(&chunks)?;
        let now = now_ms();
        let mut conn = self.conn.lock().await;
        let mut tx = conn.begin().await?;
        // Serialize same-file writers ACROSS connections/processes (the in-process
        // Mutex only covers this store). A transaction-scoped advisory lock keyed
        // by the path makes the delete+insert "replace by path" atomic against a
        // concurrent reindex/remove/rename — otherwise two writers on different
        // MVCC snapshots could both commit and leave duplicate/mixed chunks.
        lock_path(&mut tx, &self.container, filepath).await?;
        // Drop only this file's CHUNKS (not its edges — L7 is deferred to the
        // graph worker, so edges persist until it re-derives them; mirrors the
        // SQLite backend).
        sqlx::query("DELETE FROM chunks WHERE container = $1 AND filepath = $2")
            .bind(&self.container)
            .bind(filepath)
            .execute(&mut *tx)
            .await?;
        for (ord, (text, vec)) in chunks.iter().zip(vectors.iter()).enumerate() {
            sqlx::query(
                "INSERT INTO chunks(container, ino, filepath, ord, text, last_accessed_at, embedding) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7::vector)",
            )
            .bind(&self.container)
            .bind(ino as i64)
            .bind(filepath)
            .bind(ord as i32)
            .bind(text)
            .bind(now)
            .bind(vec_literal(vec))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        drop(conn);

        // L7 is DEFERRED: enqueue for background entity extraction so the blocking
        // per-file LLM call never sits on the synchronous index/flush path.
        if let Some(q) = &self.graph_queue {
            q.enqueue(ino, filepath.to_string());
        }
        Ok(())
    }

    /// L7 (deferred): extract entities for one already-indexed file and write its
    /// `edges`. PgVectorStore has no cache-DB handle, so it reconstructs the
    /// file's content from its stored chunk `text` (verbatim windows; overlap
    /// duplication is harmless for entity extraction). The blocking LLM call runs
    /// on the blocking pool; sqlx writes are async. Fail-open. No-op without an
    /// extractor.
    pub async fn index_graph(&self, _ino: u64, filepath: &str) -> anyhow::Result<()> {
        let Some(llm) = self.graph_llm.clone() else {
            return Ok(());
        };
        let content: String = {
            let mut conn = self.conn.lock().await;
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT text FROM chunks WHERE container = $1 AND filepath = $2 ORDER BY ord",
            )
            .bind(&self.container)
            .bind(filepath)
            .fetch_all(&mut *conn)
            .await?;
            rows.into_iter().map(|(t,)| t).collect::<Vec<_>>().join("\n")
        };
        if content.is_empty() {
            return Ok(());
        }
        // Blocking ureq LLM call → off the async runtime so many can overlap.
        let entities = match tokio::task::spawn_blocking(move || graph::extract_entities(&llm, &content))
            .await?
        {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(filepath, "entity extraction failed ({e}); no edges");
                return Ok(());
            }
        };
        let now = now_ms();
        let mut conn = self.conn.lock().await;
        let mut tx = conn.begin().await?;
        lock_path(&mut tx, &self.container, filepath).await?;
        // Replace this file's edges (idempotent re-derive).
        sqlx::query("DELETE FROM edges WHERE container = $1 AND from_path = $2")
            .bind(&self.container)
            .bind(filepath)
            .execute(&mut *tx)
            .await?;
        for ent in &entities {
            sqlx::query(
                "INSERT INTO edges(container, from_path, to_path, edge_kind, created_at) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
            )
            .bind(&self.container)
            .bind(filepath)
            .bind(graph::entity_path(&ent.name))
            .bind(&ent.kind)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn remove(&self, filepath: &str) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().await;
        let mut tx = conn.begin().await?;
        lock_path(&mut tx, &self.container, filepath).await?;
        sqlx::query("DELETE FROM chunks WHERE container = $1 AND filepath = $2")
            .bind(&self.container)
            .bind(filepath)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM edges WHERE container = $1 AND from_path = $2")
            .bind(&self.container)
            .bind(filepath)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Relabel `old` → `new` (drops the destination's prior rows first). No
    /// re-embed — content is unchanged.
    pub async fn rename(&self, old: &str, new: &str) -> anyhow::Result<()> {
        if old == new {
            return Ok(());
        }
        let mut conn = self.conn.lock().await;
        let mut tx = conn.begin().await?;
        // Lock both endpoints in a stable order so two opposite renames (A→B and
        // B→A) can't deadlock, and a concurrent reindex of either path is
        // serialized. Acquired in SEPARATE sequential statements: Postgres does
        // not define evaluation order within one statement's target list, so
        // combining both calls in a single SELECT would NOT guarantee lo-then-hi.
        let (lo, hi) = if old <= new { (old, new) } else { (new, old) };
        lock_path(&mut tx, &self.container, lo).await?;
        lock_path(&mut tx, &self.container, hi).await?;
        sqlx::query("DELETE FROM chunks WHERE container = $1 AND filepath = $2")
            .bind(&self.container)
            .bind(new)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM edges WHERE container = $1 AND from_path = $2")
            .bind(&self.container)
            .bind(new)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE chunks SET filepath = $3 WHERE container = $1 AND filepath = $2")
            .bind(&self.container)
            .bind(old)
            .bind(new)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE edges SET from_path = $3 WHERE container = $1 AND from_path = $2")
            .bind(&self.container)
            .bind(old)
            .bind(new)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[async_trait]
impl SemanticIndex for PgVectorStore {
    async fn search(
        &self,
        query: &str,
        filepath: Option<&str>,
    ) -> anyhow::Result<Vec<SearchHit>> {
        // Embed the query on a blocking thread. The Embedder trait is synchronous
        // (local fastembed = CPU-bound; cloud = blocking `ureq`), and running it
        // inline would NOT yield — so the daemon's `tokio::time::timeout` around
        // this search could never fire while embedding stalled, leaving the single
        // connection pinned past the client deadline. `spawn_blocking` lets the
        // future await (yield), so the timeout stays effective and cancellable.
        let embedder = self.embedder.clone();
        let q = query.to_string();
        let qvec = tokio::task::spawn_blocking(move || embedder.embed(&[q]))
            .await
            .map_err(|e| anyhow::anyhow!("query embed task failed: {e}"))??
            .pop()
            .unwrap_or_default();
        let qlit = vec_literal(&qvec);
        // Scope predicate pushed into each retrieval lane so a `/prefix/` query
        // can't be crowded out of the global top-K by out-of-scope files (a
        // false-negative bug if filtered only after `LIMIT`). NULL = unscoped.
        // Uses `starts_with` for a LITERAL prefix match — `LIKE` would treat `%`
        // and `_` in real paths as wildcards and over-match.
        let scope = filepath.map(|p| p.to_string());

        let mut by_file: HashMap<String, (String, f64)> = HashMap::new();
        // filepath -> the representative chunk's row id, captured at retrieval.
        // Used in phase 2 to detect a concurrent same-path reindex (new ids), so
        // we never return a snippet/score from pre-rewrite content.
        let mut rep_chunk: HashMap<String, i64> = HashMap::new();

        // Phase 1 — retrieval. Hold the single connection only for the queries.
        {
            let mut conn = self.conn.lock().await;

            // Vector KNN (cosine distance operator), scoped to this container.
            let rows = sqlx::query(
                "SELECT id, filepath, text FROM chunks \
                 WHERE container = $4 AND ($2::text IS NULL OR starts_with(filepath, $2)) \
                 ORDER BY embedding <=> $1::vector LIMIT $3",
            )
            .bind(&qlit)
            .bind(&scope)
            .bind(SEARCH_POOL)
            .bind(&self.container)
            .fetch_all(&mut *conn)
            .await?;
            for (i, row) in rows.iter().enumerate() {
                let (id, fp): (i64, String) = (row.get(0), row.get(1));
                rep_chunk.entry(fp.clone()).or_insert(id);
                rank::rrf_bump(&mut by_file, fp, row.get(2), i);
            }

            // Keyword (Postgres FTS). Fail-soft — vector hits stand.
            if let Ok(rows) = sqlx::query(
                "SELECT id, filepath, text FROM chunks \
                 WHERE container = $4 \
                 AND to_tsvector('simple', text) @@ plainto_tsquery('simple', $1) \
                 AND ($3::text IS NULL OR starts_with(filepath, $3)) \
                 ORDER BY ts_rank(to_tsvector('simple', text), plainto_tsquery('simple', $1)) DESC \
                 LIMIT $2",
            )
            .bind(query)
            .bind(SEARCH_POOL)
            .bind(&scope)
            .bind(&self.container)
            .fetch_all(&mut *conn)
            .await
            {
                for (i, row) in rows.iter().enumerate() {
                    let (id, fp): (i64, String) = (row.get(0), row.get(1));
                    rep_chunk.entry(fp.clone()).or_insert(id);
                    rank::rrf_bump(&mut by_file, fp, row.get(2), i);
                }
            }
        }

        let mut hits = rank::to_hits(by_file, filepath);

        // L5 rerank runs OUTSIDE the connection lock — the reranker trait is
        // synchronous and may block on a local model or HTTP; holding the only
        // connection across it would stall every other search/index/write. Run it
        // on a blocking thread for the same reason as the query embed above: keep
        // the search future yielding so the IPC timeout stays effective.
        if let Some(reranker) = &self.reranker {
            let reranker = reranker.clone();
            let q = query.to_string();
            let mut h = hits;
            hits = tokio::task::spawn_blocking(move || {
                rank::apply_reranker(&mut h, reranker.as_ref(), &q).map(|()| h)
            })
            .await
            .map_err(|e| anyhow::anyhow!("rerank task failed: {e}"))??;
        }

        // Phase 2 — re-acquire the connection for salience/entity stats + access
        // bump. rank.rs takes sync closures, so we pre-fetch into maps first.
        let paths: Vec<String> = hits.iter().filter_map(|h| h.filepath.clone()).collect();
        let mut stats: HashMap<String, (Option<i64>, i64)> = HashMap::new();
        let mut ents: HashMap<String, HashSet<String>> = HashMap::new();
        let now = now_ms();
        if !paths.is_empty() {
            let mut conn = self.conn.lock().await;
            if let Ok(srows) = sqlx::query(
                "SELECT filepath, MAX(last_accessed_at), COALESCE(SUM(access_count), 0)::bigint \
                 FROM chunks WHERE container = $2 AND filepath = ANY($1) GROUP BY filepath",
            )
            .bind(&paths)
            .bind(&self.container)
            .fetch_all(&mut *conn)
            .await
            {
                for row in &srows {
                    stats.insert(row.get(0), (row.get(1), row.get(2)));
                }
            }
            // Revalidate against CHUNK IDENTITY: the lock was released for
            // reranking, so a concurrent rename/remove/reindex may have
            // invalidated a hit. Keep a hit only if a chunk row still exists with
            // the exact (id, filepath) we retrieved — drops ghosts from rename
            // (filepath changed), remove (gone), AND same-path reindex (new ids).
            // Skip on query error (don't nuke results on a transient failure).
            let rep_ids: Vec<i64> = hits
                .iter()
                .filter_map(|h| h.filepath.as_ref().and_then(|fp| rep_chunk.get(fp).copied()))
                .collect();
            if !rep_ids.is_empty() {
                if let Ok(rows) = sqlx::query(
                    "SELECT id, filepath FROM chunks WHERE container = $2 AND id = ANY($1)",
                )
                .bind(&rep_ids)
                .bind(&self.container)
                .fetch_all(&mut *conn)
                .await
                {
                    let live: HashMap<i64, String> =
                        rows.iter().map(|r| (r.get(0), r.get(1))).collect();
                    hits.retain(|h| {
                        h.filepath.as_ref().is_some_and(|fp| {
                            rep_chunk
                                .get(fp)
                                .and_then(|id| live.get(id))
                                .is_some_and(|cur| cur == fp)
                        })
                    });
                }
            }
            if let Ok(erows) = sqlx::query(
                "SELECT from_path, to_path FROM edges WHERE container = $2 AND from_path = ANY($1)",
            )
            .bind(&paths)
            .bind(&self.container)
            .fetch_all(&mut *conn)
            .await
            {
                for row in &erows {
                    ents.entry(row.get(0)).or_default().insert(row.get(1));
                }
            }
            let _ = sqlx::query(
                "UPDATE chunks SET access_count = access_count + 1, last_accessed_at = $2 \
                 WHERE container = $3 AND filepath = ANY($1)",
            )
            .bind(&paths)
            .bind(now)
            .bind(&self.container)
            .execute(&mut *conn)
            .await;
        }

        rank::apply_comention_boost(&mut hits, |fp| ents.get(fp).cloned().unwrap_or_default());
        rank::apply_salience(&mut hits, now, |fp| stats.get(fp).copied().unwrap_or((None, 0)));
        rank::sort_desc(&mut hits);
        Ok(hits)
    }
}

/// Bridge to the cache write path (daemon): drive Postgres indexing as files
/// change. Naturally async — delegates to the inherent async methods. Wiring a
/// `PgVectorStore` here is what makes the pgvector backend reachable from
/// `semfs mount`/`grep` (vs. SQLite, the default).
#[async_trait]
impl crate::cache::LocalIndexer for PgVectorStore {
    async fn index(&self, ino: u64, filepath: &str, content: &str) -> anyhow::Result<()> {
        PgVectorStore::index(self, ino, filepath, content).await
    }
    async fn remove(&self, filepath: &str) -> anyhow::Result<()> {
        PgVectorStore::remove(self, filepath).await
    }
    async fn rename(&self, old: &str, new: &str) -> anyhow::Result<()> {
        PgVectorStore::rename(self, old, new).await
    }
    fn graph_queue(&self) -> Option<Arc<crate::cache::GraphQueue>> {
        self.graph_queue.clone()
    }
    async fn index_graph(&self, ino: u64, filepath: &str) -> anyhow::Result<()> {
        PgVectorStore::index_graph(self, ino, filepath).await
    }
}

// Tests use the embedded pglite server (the `pg-local` feature); the production
// `pg` feature alone has no in-process Postgres to test against.
#[cfg(all(test, feature = "pg-local"))]
mod tests {
    use super::*;
    use crate::embed::HashEmbedder;

    /// Serializes pglite server *startup*: parallel `temporary()` servers race
    /// while first populating pglite-oxide's SHARED on-disk template/extension
    /// cache (archive extraction isn't concurrency-safe). Only `start()` is
    /// serialized — the independent servers then run their test bodies in parallel.
    static PG_START_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Start a temporary embedded Postgres with pgvector enabled.
    fn pg() -> pglite_oxide::PgliteServer {
        let _g = PG_START_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        pglite_oxide::PgliteServer::builder()
            .temporary()
            .tcp("127.0.0.1:0".parse().unwrap())
            .extension(pglite_oxide::extensions::VECTOR)
            .start()
            .expect("start pglite with pgvector")
    }

    /// With a graph extractor attached, `index()` ENQUEUES the file for deferred
    /// L7 extraction (rather than calling the LLM inline). Constructing the client
    /// does no network I/O — only `index_graph` (not exercised here) would.
    #[tokio::test]
    async fn pg_index_enqueues_graph_work_when_extractor_present() {
        use crate::cache::LocalIndexer;
        let server = pg();
        let llm = Arc::new(crate::llm::LlmClient::openrouter("test-key".into()));
        let store =
            PgVectorStore::connect(&server.database_url(), "t", Arc::new(HashEmbedder::new(384)))
                .await
                .unwrap()
                .with_graph_extractor(llm);
        let q = store.graph_queue().expect("graph queue present with extractor");
        assert!(q.is_idle());
        store.index(7, "/notes/a.md", "hello world").await.unwrap();
        assert_eq!(q.depth(), 1, "index() must enqueue the file for L7 extraction");
        drop(store);
        let _ = server.shutdown();
    }

    /// Spike + parity: index two docs, search finds the right one; rename relabels;
    /// remove deletes. Proves the full pgvector pipeline end to end.
    #[tokio::test]
    async fn pg_index_search_and_rename() {
        let server = pg();
        let store = PgVectorStore::connect(&server.database_url(), "t", Arc::new(HashEmbedder::new(384)))
            .await
            .expect("connect + schema");

        store
            .index(2, "/auth.md", "user login and credential verification flow")
            .await
            .unwrap();
        store
            .index(3, "/cook.md", "banana bread recipe with walnuts and sugar")
            .await
            .unwrap();

        let hits = store.search("credential login", None).await.unwrap();
        assert_eq!(hits[0].filepath.as_deref(), Some("/auth.md"));

        store.rename("/auth.md", "/auth2.md").await.unwrap();
        let after = store.search("credential login", None).await.unwrap();
        assert_eq!(after[0].filepath.as_deref(), Some("/auth2.md"));
        assert!(after.iter().all(|h| h.filepath.as_deref() != Some("/auth.md")));

        store.remove("/auth2.md").await.unwrap();
        let gone = store.search("credential login", None).await.unwrap();
        assert!(gone.iter().all(|h| h.filepath.as_deref() != Some("/auth2.md")));

        // Close the client connection before shutting the server down, else the
        // server blocks waiting for the still-open connection to drain.
        drop(store);
        let _ = server.shutdown();
    }

    /// Multi-tenancy: two containers sharing ONE Postgres database, with the SAME
    /// file path, must not see, overwrite, or delete each other's documents.
    ///
    /// Connections are opened SEQUENTIALLY (one at a time): the embedded pglite
    /// test server serves a single connection at a time, so two concurrent
    /// `PgVectorStore`s would deadlock. (A real multi-connection Postgres is what
    /// makes simultaneous mounts possible in production; the namespacing logic is
    /// what this test validates, and it's connection-count-independent.)
    #[tokio::test]
    async fn pg_containers_are_isolated() {
        let server = pg();
        let url = server.database_url();

        // alice indexes /README.md, then releases her connection.
        {
            let a = PgVectorStore::connect(&url, "alice", Arc::new(HashEmbedder::new(384)))
                .await
                .unwrap();
            a.index(2, "/README.md", "alice alpha credential login secret").await.unwrap();
        }
        // bob indexes the SAME path with different content.
        {
            let b = PgVectorStore::connect(&url, "bob", Arc::new(HashEmbedder::new(384)))
                .await
                .unwrap();
            b.index(2, "/README.md", "bob banana bread recipe walnuts").await.unwrap();
            // bob sees only his own row for that path (alice's is invisible).
            let hb = b.search("banana recipe", None).await.unwrap();
            assert_eq!(hb.len(), 1);
            assert!(hb[0].chunk.as_deref().unwrap().contains("bob"));
        }
        // alice's row survived bob's write to the same path, and her remove does
        // not touch bob's.
        {
            let a = PgVectorStore::connect(&url, "alice", Arc::new(HashEmbedder::new(384)))
                .await
                .unwrap();
            let ha = a.search("credential login", None).await.unwrap();
            assert_eq!(ha.len(), 1, "alice still sees only her own /README.md");
            assert!(ha[0].chunk.as_deref().unwrap().contains("alice"));
            a.remove("/README.md").await.unwrap();
            assert!(!a.is_searchable().await, "alice's index now empty");
        }
        {
            let b = PgVectorStore::connect(&url, "bob", Arc::new(HashEmbedder::new(384)))
                .await
                .unwrap();
            assert!(b.is_searchable().await, "bob untouched by alice's remove");
            assert_eq!(b.search("banana recipe", None).await.unwrap().len(), 1);
        }
        let _ = server.shutdown();
    }

    /// Scoped search returns in-scope matches even when many out-of-scope files
    /// match the same terms — the scope predicate is pushed into both lanes.
    #[tokio::test]
    async fn pg_scoped_search_survives_crowding() {
        let server = pg();
        let store = PgVectorStore::connect(&server.database_url(), "t", Arc::new(HashEmbedder::new(384)))
            .await
            .expect("connect");
        for i in 0..100 {
            store
                .index(1000 + i, &format!("/noise/{i}.md"), "alpha shared keyword here")
                .await
                .unwrap();
        }
        store
            .index(2, "/scope/target.md", "alpha shared keyword here")
            .await
            .unwrap();

        let hits = store
            .search("alpha shared keyword", Some("/scope/"))
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.filepath.as_deref() == Some("/scope/target.md")),
            "scoped search dropped the in-scope file under crowding"
        );
        assert!(
            hits.iter().all(|h| h
                .filepath
                .as_deref()
                .map_or(true, |p| p.starts_with("/scope/"))),
            "scoped search leaked out-of-scope files"
        );

        drop(store);
        let _ = server.shutdown();
    }

    /// Scope prefixes containing LIKE wildcards (`%`, `_`) must match literally,
    /// not as wildcards that over-match unrelated paths.
    #[tokio::test]
    async fn pg_scoped_search_treats_like_wildcards_literally() {
        let server = pg();
        let store = PgVectorStore::connect(&server.database_url(), "t", Arc::new(HashEmbedder::new(384)))
            .await
            .expect("connect");
        store
            .index(2, "/r/100%/f.md", "alpha shared keyword")
            .await
            .unwrap();
        // `/r/100x/y.md` matches the LIKE pattern `/r/100%/%` but not the literal prefix.
        store
            .index(3, "/r/100x/y.md", "alpha shared keyword")
            .await
            .unwrap();

        let hits = store
            .search("alpha shared keyword", Some("/r/100%/"))
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.filepath.as_deref() == Some("/r/100%/f.md")),
            "literal in-scope file missing"
        );
        assert!(
            hits.iter().all(|h| h.filepath.as_deref() != Some("/r/100x/y.md")),
            "LIKE wildcard prefix over-matched a sibling"
        );

        drop(store);
        let _ = server.shutdown();
    }

    /// Reusing a database with a mismatched embedding dimension fails fast at
    /// connect rather than deferring to the first insert/search.
    #[tokio::test]
    async fn pg_connect_rejects_dimension_drift() {
        let server = pg();
        let first = PgVectorStore::connect(&server.database_url(), "t", Arc::new(HashEmbedder::new(384)))
            .await
            .expect("first connect creates vector(384)");
        // Close the connection so the single-connection server is free.
        drop(first);

        let mismatched =
            PgVectorStore::connect(&server.database_url(), "t", Arc::new(HashEmbedder::new(256))).await;
        assert!(
            mismatched.is_err(),
            "connect must reject a 256-d embedder against an existing vector(384) table"
        );

        let _ = server.shutdown();
    }

    /// Reusing a database with a SAME-width but DIFFERENT model fails fast at
    /// connect (identity guard), rather than silently searching stale vectors.
    #[tokio::test]
    async fn pg_connect_rejects_model_swap() {
        #[derive(Debug)]
        struct TaggedEmbedder {
            dims: usize,
            id: String,
        }
        impl crate::embed::Embedder for TaggedEmbedder {
            fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
                Ok(texts.iter().map(|_| vec![0.0; self.dims]).collect())
            }
            fn dimensions(&self) -> usize {
                self.dims
            }
            fn identity(&self) -> String {
                self.id.clone()
            }
        }

        let server = pg();
        let first = PgVectorStore::connect(&server.database_url(), "t", Arc::new(HashEmbedder::new(384)))
            .await
            .expect("first connect stamps identity hash:384");
        drop(first);

        // Same width (384), different identity → must be refused.
        let swapped = PgVectorStore::connect(
            &server.database_url(),
            "t",
            Arc::new(TaggedEmbedder { dims: 384, id: "other-model:384".into() }),
        )
        .await;
        assert!(
            swapped.is_err(),
            "connect must reject a same-width but different-model embedder"
        );

        let _ = server.shutdown();
    }
}
