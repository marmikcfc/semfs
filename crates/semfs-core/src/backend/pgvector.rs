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

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Local/cloud-embedder-fed semantic index over Postgres + pgvector.
pub struct PgVectorStore {
    conn: Mutex<PgConnection>,
    embedder: Arc<dyn Embedder>,
    reranker: Option<Arc<dyn Reranker>>,
    graph_llm: Option<Arc<LlmClient>>,
}

impl std::fmt::Debug for PgVectorStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgVectorStore")
            .field("dimensions", &self.embedder.dimensions())
            .field("has_reranker", &self.reranker.is_some())
            .field("has_graph_extractor", &self.graph_llm.is_some())
            .finish()
    }
}

impl PgVectorStore {
    /// Connect and ensure the schema (extension + chunks/edges tables) at the
    /// embedder's vector width. The `vector` extension must be available in the
    /// server (e.g. pglite-oxide started with `extensions::VECTOR`).
    pub async fn connect(database_url: &str, embedder: Arc<dyn Embedder>) -> anyhow::Result<Self> {
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
                id BIGSERIAL PRIMARY KEY, ino BIGINT NOT NULL, filepath TEXT NOT NULL, \
                ord INT NOT NULL, text TEXT NOT NULL, last_accessed_at BIGINT, \
                access_count BIGINT NOT NULL DEFAULT 0, embedding vector({dims}))"
        )))
        .execute(&mut conn)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_chunks_filepath ON chunks(filepath)")
            .execute(&mut conn)
            .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_chunks_fts ON chunks \
             USING gin(to_tsvector('simple', text))",
        )
        .execute(&mut conn)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS edges (from_path TEXT NOT NULL, to_path TEXT NOT NULL, \
             edge_kind TEXT NOT NULL, created_at BIGINT NOT NULL, \
             PRIMARY KEY (from_path, to_path, edge_kind))",
        )
        .execute(&mut conn)
        .await?;
        Ok(Self {
            conn: Mutex::new(conn),
            embedder,
            reranker: None,
            graph_llm: None,
        })
    }

    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    pub fn with_graph_extractor(mut self, llm: Arc<LlmClient>) -> Self {
        self.graph_llm = Some(llm);
        self
    }

    /// Chunk → embed → write chunks/edges atomically; re-index replaces by path.
    pub async fn index(&self, ino: u64, filepath: &str, content: &str) -> anyhow::Result<()> {
        let chunks = chunk::recursive_chunks(content, &chunk::ChunkOptions::default());
        let vectors = self.embedder.embed(&chunks)?;
        let entities = match &self.graph_llm {
            Some(llm) => graph::extract_entities(llm, content).unwrap_or_else(|e| {
                tracing::warn!("entity extraction failed ({e}); no graph edges for {filepath}");
                Vec::new()
            }),
            None => Vec::new(),
        };
        let now = now_ms();
        let mut conn = self.conn.lock().await;
        let mut tx = conn.begin().await?;
        sqlx::query("DELETE FROM chunks WHERE filepath = $1")
            .bind(filepath)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM edges WHERE from_path = $1")
            .bind(filepath)
            .execute(&mut *tx)
            .await?;
        for (ord, (text, vec)) in chunks.iter().zip(vectors.iter()).enumerate() {
            sqlx::query(
                "INSERT INTO chunks(ino, filepath, ord, text, last_accessed_at, embedding) \
                 VALUES ($1, $2, $3, $4, $5, $6::vector)",
            )
            .bind(ino as i64)
            .bind(filepath)
            .bind(ord as i32)
            .bind(text)
            .bind(now)
            .bind(vec_literal(vec))
            .execute(&mut *tx)
            .await?;
        }
        for ent in &entities {
            sqlx::query(
                "INSERT INTO edges(from_path, to_path, edge_kind, created_at) \
                 VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            )
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
        sqlx::query("DELETE FROM chunks WHERE filepath = $1")
            .bind(filepath)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM edges WHERE from_path = $1")
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
        sqlx::query("DELETE FROM chunks WHERE filepath = $1")
            .bind(new)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM edges WHERE from_path = $1")
            .bind(new)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE chunks SET filepath = $2 WHERE filepath = $1")
            .bind(old)
            .bind(new)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE edges SET from_path = $2 WHERE from_path = $1")
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
        let qvec = self
            .embedder
            .embed(&[query.to_string()])?
            .pop()
            .unwrap_or_default();
        let qlit = vec_literal(&qvec);

        let mut conn = self.conn.lock().await;
        let mut by_file: HashMap<String, (String, f64)> = HashMap::new();

        // Vector KNN (cosine distance operator).
        let rows = sqlx::query(
            "SELECT filepath, text FROM chunks ORDER BY embedding <=> $1::vector LIMIT $2",
        )
        .bind(&qlit)
        .bind(SEARCH_POOL)
        .fetch_all(&mut *conn)
        .await?;
        for (i, row) in rows.iter().enumerate() {
            rank::rrf_bump(&mut by_file, row.get(0), row.get(1), i);
        }

        // Keyword (Postgres FTS). Fail-soft — vector hits stand.
        if let Ok(rows) = sqlx::query(
            "SELECT filepath, text FROM chunks \
             WHERE to_tsvector('simple', text) @@ plainto_tsquery('simple', $1) \
             ORDER BY ts_rank(to_tsvector('simple', text), plainto_tsquery('simple', $1)) DESC \
             LIMIT $2",
        )
        .bind(query)
        .bind(SEARCH_POOL)
        .fetch_all(&mut *conn)
        .await
        {
            for (i, row) in rows.iter().enumerate() {
                rank::rrf_bump(&mut by_file, row.get(0), row.get(1), i);
            }
        }

        let mut hits = rank::to_hits(by_file, filepath);

        if let Some(reranker) = &self.reranker {
            rank::apply_reranker(&mut hits, reranker.as_ref(), query)?;
        }

        // Pre-fetch salience stats + entities for hit files, then apply via the
        // shared sync rank helpers (rank.rs takes sync closures; we feed maps).
        let paths: Vec<String> = hits.iter().filter_map(|h| h.filepath.clone()).collect();
        let mut stats: HashMap<String, (Option<i64>, i64)> = HashMap::new();
        let mut ents: HashMap<String, HashSet<String>> = HashMap::new();
        if !paths.is_empty() {
            if let Ok(srows) = sqlx::query(
                "SELECT filepath, MAX(last_accessed_at), COALESCE(SUM(access_count), 0)::bigint \
                 FROM chunks WHERE filepath = ANY($1) GROUP BY filepath",
            )
            .bind(&paths)
            .fetch_all(&mut *conn)
            .await
            {
                for row in &srows {
                    stats.insert(row.get(0), (row.get(1), row.get(2)));
                }
            }
            if let Ok(erows) =
                sqlx::query("SELECT from_path, to_path FROM edges WHERE from_path = ANY($1)")
                    .bind(&paths)
                    .fetch_all(&mut *conn)
                    .await
            {
                for row in &erows {
                    ents.entry(row.get(0)).or_default().insert(row.get(1));
                }
            }
        }

        let now = now_ms();
        rank::apply_comention_boost(&mut hits, |fp| ents.get(fp).cloned().unwrap_or_default());
        rank::apply_salience(&mut hits, now, |fp| stats.get(fp).copied().unwrap_or((None, 0)));

        if !paths.is_empty() {
            let _ = sqlx::query(
                "UPDATE chunks SET access_count = access_count + 1, last_accessed_at = $2 \
                 WHERE filepath = ANY($1)",
            )
            .bind(&paths)
            .bind(now)
            .execute(&mut *conn)
            .await;
        }
        rank::sort_desc(&mut hits);
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::HashEmbedder;

    /// Start a temporary embedded Postgres with pgvector enabled.
    fn pg() -> pglite_oxide::PgliteServer {
        pglite_oxide::PgliteServer::builder()
            .temporary()
            .tcp("127.0.0.1:0".parse().unwrap())
            .extension(pglite_oxide::extensions::VECTOR)
            .start()
            .expect("start pglite with pgvector")
    }

    /// Spike + parity: index two docs, search finds the right one; rename relabels;
    /// remove deletes. Proves the full pgvector pipeline end to end.
    #[tokio::test]
    async fn pg_index_search_and_rename() {
        let server = pg();
        let store = PgVectorStore::connect(&server.database_url(), Arc::new(HashEmbedder::new(384)))
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
}
