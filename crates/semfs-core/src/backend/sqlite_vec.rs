//! Local hybrid index (Phase 4): `SqliteVecStore`.
//!
//! Implements [`SemanticIndex`] over the existing SQLite cache extended with
//! sqlite-vec (`vchunks`) + fts5 (`ffts`). `index` chunks → embeds → writes all
//! three tables in one transaction; `search` fuses vec0 KNN and BM25 with
//! Reciprocal Rank Fusion. Ports `bash/src/backends/sqlite-vec.ts`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::{SearchHit, SemanticIndex};
use crate::cache::Db;
use crate::embed::Embedder;
use crate::rerank::Reranker;

/// RRF constant. score contribution per ranked list = 1 / (RRF_K + rank).
const RRF_K: f64 = 60.0;
/// Over-fetch per ranked list before collapsing chunks → files.
const SEARCH_POOL: usize = 80;

/// Local, offline semantic index over the SQLite cache.
#[derive(Debug)]
pub struct SqliteVecStore {
    db: Arc<Db>,
    embedder: Arc<dyn Embedder>,
    /// Optional L5 reranker, applied to candidates after RRF in `search`.
    reranker: Option<Arc<dyn Reranker>>,
    /// Optional L7 graph extractor (LLM). When present, `index` extracts typed
    /// entities and writes file→entity edges. `None` = no graph.
    graph_llm: Option<Arc<crate::llm::LlmClient>>,
}

impl SqliteVecStore {
    /// Build a store and ensure the vec0 tables exist at the embedder's width.
    pub fn new(db: Arc<Db>, embedder: Arc<dyn Embedder>) -> anyhow::Result<Self> {
        db.ensure_vector_tables(embedder.dimensions(), None)?;
        Ok(Self {
            db,
            embedder,
            reranker: None,
            graph_llm: None,
        })
    }

    /// Open over an EXISTING index without touching the schema — for readers
    /// like `grep`. Unlike [`SqliteVecStore::new`], this does NOT call
    /// `ensure_vector_tables`, so a reader can never drop/rebuild the writer's
    /// vec0 tables on a dimension mismatch (a dim mismatch instead surfaces as a
    /// query-time error, never data loss).
    pub fn open_existing(db: Arc<Db>, embedder: Arc<dyn Embedder>) -> Self {
        Self {
            db,
            embedder,
            reranker: None,
            graph_llm: None,
        }
    }

    /// Attach an L5 reranker. Search reranks the post-RRF candidates by their
    /// chunk text and re-sorts. Works with any [`Reranker`] (local or cloud).
    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    /// Attach the L7 graph extractor (LLM). `index` will extract typed entities
    /// from each file and write file→entity edges. Only the writer (daemon) needs
    /// this; search reads whatever edges exist.
    pub fn with_graph_extractor(mut self, llm: Arc<crate::llm::LlmClient>) -> Self {
        self.graph_llm = Some(llm);
        self
    }

    /// Index a file: chunk → embed → write `chunks`/`ffts`/`vchunks` atomically.
    /// Re-indexing the same `filepath` replaces its prior chunks (and their
    /// rowid-linked vec0/fts rows). Removing a file = `index` with empty content.
    pub fn index(&self, ino: u64, filepath: &str, content: &str) -> anyhow::Result<()> {
        let chunks = super::chunk::recursive_chunks(content, &super::chunk::ChunkOptions::default());
        let vectors = self.embedder.embed(&chunks)?;

        // L7: extract entities via the LLM BEFORE locking the db (network call).
        // Fail-open — a write never fails because extraction did.
        let entities = match &self.graph_llm {
            Some(llm) => super::graph::extract_entities(llm, content).unwrap_or_else(|e| {
                tracing::warn!("entity extraction failed ({e}); no graph edges for {filepath}");
                Vec::new()
            }),
            None => Vec::new(),
        };

        let mut conn = self.db.conn.lock();
        let tx = conn.transaction()?;

        // Drop this file's prior chunks + their rowid-linked vec0/fts rows.
        drop_file_chunks(&tx, filepath)?;

        // Insert fresh chunks; the chunk's rowid is reused for vec0 + fts so the
        // three tables join back on the same id. `last_accessed_at` = now so a
        // freshly-written file starts fully salient (L6).
        let now = now_ms();
        for (ord, (text, vec)) in chunks.iter().zip(vectors.iter()).enumerate() {
            tx.execute(
                "INSERT INTO chunks(ino, filepath, ord, text, last_accessed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![ino as i64, filepath, ord as i64, text, now],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO vchunks(rowid, embedding) VALUES (?1, ?2)",
                rusqlite::params![id, vec_to_blob(vec)],
            )?;
            tx.execute(
                "INSERT INTO ffts(rowid, text) VALUES (?1, ?2)",
                rusqlite::params![id, text],
            )?;
        }

        // L7: file → entity edges (the entities the LLM found). Old edges were
        // dropped above; re-derive from this write.
        for ent in &entities {
            tx.execute(
                "INSERT OR IGNORE INTO edges(from_path, to_path, edge_kind, created_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    filepath,
                    super::graph::entity_path(&ent.name),
                    ent.kind,
                    now
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Drop a file's chunks (and their rowid-linked vec0/fts rows) from the
    /// index — on delete, or before re-indexing under a new path on rename.
    pub fn remove(&self, filepath: &str) -> anyhow::Result<()> {
        let mut conn = self.db.conn.lock();
        let tx = conn.transaction()?;
        drop_file_chunks(&tx, filepath)?;
        tx.commit()?;
        Ok(())
    }

    /// Relabel a file's index rows `old` → `new` on rename. Cheap: vec0/fts rows
    /// are keyed by rowid (content-derived, path-independent), so only the
    /// `chunks.filepath` label and outbound `edges.from_path` change — no
    /// re-embedding. Any rows the destination already had are dropped first.
    pub fn rename(&self, old: &str, new: &str) -> anyhow::Result<()> {
        if old == new {
            return Ok(());
        }
        let mut conn = self.db.conn.lock();
        let tx = conn.transaction()?;
        // Overwrite: clear the destination's existing index rows.
        drop_file_chunks(&tx, new)?;
        tx.execute(
            "UPDATE chunks SET filepath = ?2 WHERE filepath = ?1",
            rusqlite::params![old, new],
        )?;
        tx.execute(
            "UPDATE edges SET from_path = ?2 WHERE from_path = ?1",
            rusqlite::params![old, new],
        )?;
        tx.commit()?;
        Ok(())
    }
}

/// Delete a file's chunks and their rowid-linked vec0/fts rows within a txn.
fn drop_file_chunks(tx: &rusqlite::Transaction, filepath: &str) -> rusqlite::Result<()> {
    let ids: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT id FROM chunks WHERE filepath = ?1")?;
        let rows = stmt.query_map([filepath], |r| r.get::<_, i64>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    for id in ids {
        tx.execute("DELETE FROM vchunks WHERE rowid = ?1", [id])?;
        tx.execute("DELETE FROM ffts WHERE rowid = ?1", [id])?;
    }
    tx.execute("DELETE FROM chunks WHERE filepath = ?1", [filepath])?;
    // L7: this file's outbound edges go too (re-derived on write, gone on delete).
    tx.execute("DELETE FROM edges WHERE from_path = ?1", [filepath])?;
    Ok(())
}

/// Bridge to the cache write path: lets `CacheFs`/`SqliteFile` maintain the
/// index on writes/deletes without a module cycle.
impl crate::cache::LocalIndexer for SqliteVecStore {
    fn index(&self, ino: u64, filepath: &str, content: &str) -> anyhow::Result<()> {
        SqliteVecStore::index(self, ino, filepath, content)
    }
    fn remove(&self, filepath: &str) -> anyhow::Result<()> {
        SqliteVecStore::remove(self, filepath)
    }
    fn rename(&self, old: &str, new: &str) -> anyhow::Result<()> {
        SqliteVecStore::rename(self, old, new)
    }
}

#[async_trait]
impl SemanticIndex for SqliteVecStore {
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
        let qblob = vec_to_blob(&qvec);

        // filepath -> (representative chunk, summed RRF score)
        let mut by_file: HashMap<String, (String, f64)> = HashMap::new();

        let conn = self.db.conn.lock();

        // Vector KNN (vec0).
        {
            let mut stmt = conn.prepare(
                "SELECT c.filepath, c.text FROM vchunks v \
                 JOIN chunks c ON c.id = v.rowid \
                 WHERE v.embedding MATCH ?1 AND k = ?2 ORDER BY distance",
            )?;
            let rows = stmt.query_map(rusqlite::params![qblob, SEARCH_POOL as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for (rank, row) in rows.enumerate() {
                let (fp, text) = row?;
                rrf_bump(&mut by_file, fp, text, rank);
            }
        }

        // Keyword BM25 (fts5). Malformed queries fail soft — vector hits stand.
        if let Some(fq) = to_fts_query(query) {
            if let Ok(mut stmt) = conn.prepare(
                "SELECT c.filepath, c.text FROM ffts \
                 JOIN chunks c ON c.id = ffts.rowid \
                 WHERE ffts MATCH ?1 ORDER BY rank LIMIT ?2",
            ) {
                if let Ok(rows) = stmt.query_map(rusqlite::params![fq, SEARCH_POOL as i64], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                }) {
                    for (rank, row) in rows.enumerate() {
                        if let Ok((fp, text)) = row {
                            rrf_bump(&mut by_file, fp, text, rank);
                        }
                    }
                }
            }
        }
        drop(conn);

        let mut hits: Vec<SearchHit> = by_file
            .into_iter()
            .filter(|(fp, _)| filepath.map_or(true, |pref| fp.starts_with(pref)))
            .map(|(fp, (chunk, score))| SearchHit {
                filepath: Some(fp),
                memory: None,
                chunk: Some(chunk),
                similarity: score,
            })
            .collect();
        hits.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // L5: rerank the candidates (already collapsed to files, so a modest set)
        // by their representative chunk text, then re-sort by the reranker score.
        // For very large candidate sets a top-N cap should precede this call.
        if let Some(reranker) = &self.reranker {
            if !hits.is_empty() {
                let docs: Vec<String> = hits
                    .iter()
                    .map(|h| {
                        h.chunk
                            .clone()
                            .or_else(|| h.filepath.clone())
                            .unwrap_or_default()
                    })
                    .collect();
                let scores = reranker.rerank(query, &docs)?;
                for (h, s) in hits.iter_mut().zip(scores) {
                    h.similarity = s as f64;
                }
                hits.sort_by(|a, b| {
                    b.similarity
                        .partial_cmp(&a.similarity)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
        }

        // L6 salience: multiply each hit by its recency/access nudge, computed
        // from the STORED stats (before bumping), so a recently-written or
        // more-used file wins ties. Then bump the returned files' stats.
        {
            let now = now_ms();
            let conn = self.db.conn.lock();

            // L7 graph boost (co-mention): a hit file that SHARES an extracted
            // entity with another hit file gets a small nudge — files about the
            // same things are mutually relevant. Fail-soft.
            use std::collections::{HashMap, HashSet};
            let hitpaths: Vec<String> = hits.iter().filter_map(|h| h.filepath.clone()).collect();
            let mut entities_of: HashMap<String, HashSet<String>> = HashMap::new();
            for fp in &hitpaths {
                let set = conn
                    .prepare("SELECT to_path FROM edges WHERE from_path = ?1")
                    .and_then(|mut stmt| {
                        stmt.query_map([fp], |r| r.get::<_, String>(0)).map(|rows| {
                            rows.filter_map(|r| r.ok()).collect::<HashSet<String>>()
                        })
                    })
                    .unwrap_or_default();
                entities_of.insert(fp.clone(), set);
            }
            for h in hits.iter_mut() {
                if let Some(fp) = &h.filepath {
                    if let Some(mine) = entities_of.get(fp) {
                        let shares = !mine.is_empty()
                            && hitpaths.iter().any(|other| {
                                other != fp
                                    && entities_of
                                        .get(other)
                                        .is_some_and(|o| !o.is_disjoint(mine))
                            });
                        if shares {
                            h.similarity *= 1.05;
                        }
                    }
                }
            }

            for h in hits.iter_mut() {
                if let Some(fp) = &h.filepath {
                    let (last, count): (Option<i64>, i64) = conn
                        .query_row(
                            "SELECT MAX(last_accessed_at), COALESCE(SUM(access_count), 0) \
                             FROM chunks WHERE filepath = ?1",
                            [fp],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .unwrap_or((None, 0));
                    h.similarity *= salience(now, last, count);
                }
            }
            for h in hits.iter() {
                if let Some(fp) = &h.filepath {
                    let _ = conn.execute(
                        "UPDATE chunks SET access_count = access_count + 1, last_accessed_at = ?2 \
                         WHERE filepath = ?1",
                        rusqlite::params![fp, now],
                    );
                }
            }
        }
        hits.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(hits)
    }
}

/// Salience half-life: a file's recency weight halves every this-many days.
const SALIENCE_HALF_LIFE_DAYS: f64 = 14.0;

/// Epoch milliseconds now.
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// L6 salience multiplier — recency (exponential decay) + log-damped access.
/// Bounded to a narrow band around 1.0 so it **nudges** ranking, never dominates
/// the semantic/rerank score: fresher or more-used files win ties and close calls.
fn salience(now_ms: i64, last_accessed_ms: Option<i64>, access_count: i64) -> f64 {
    let recency = match last_accessed_ms {
        Some(t) => {
            let age_days = ((now_ms - t).max(0) as f64) / 86_400_000.0;
            0.5f64.powf(age_days / SALIENCE_HALF_LIFE_DAYS) // 1.0 fresh → 0.5 at half-life
        }
        None => 0.5, // unknown → neutral
    };
    let access = (1.0 + access_count.max(0) as f64).ln(); // 0, 0.69, 1.10, …
    1.0 + 0.2 * (recency - 0.5) + 0.05 * access
}

/// Reciprocal Rank Fusion: add `1/(RRF_K + rank)` for a file, keeping the first
/// (strongest) chunk text seen as its representative.
fn rrf_bump(acc: &mut HashMap<String, (String, f64)>, fp: String, chunk: String, rank: usize) {
    let s = 1.0 / (RRF_K + rank as f64);
    acc.entry(fp)
        .and_modify(|e| e.1 += s)
        .or_insert((chunk, s));
}

/// Build a safe fts5 MATCH expression: quoted, OR-joined alphanumeric tokens.
fn to_fts_query(q: &str) -> Option<String> {
    let toks: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| format!("\"{}\"", s.to_lowercase()))
        .collect();
    if toks.is_empty() {
        None
    } else {
        Some(toks.join(" OR "))
    }
}

/// f32 vector → little-endian byte blob (sqlite-vec's native format).
fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::HashEmbedder;

    fn store() -> SqliteVecStore {
        let db = Arc::new(Db::open_in_memory().unwrap());
        SqliteVecStore::new(db, Arc::new(HashEmbedder::new(384))).unwrap()
    }

    #[tokio::test]
    async fn index_then_search_finds_file_by_overlap() {
        let s = store();
        s.index(2, "/notes/auth.md", "user login and credential verification flow")
            .unwrap();
        s.index(3, "/notes/cooking.md", "banana bread recipe with walnuts and sugar")
            .unwrap();

        let hits = s.search("credential login", None).await.unwrap();
        assert!(!hits.is_empty(), "expected at least one hit");
        assert_eq!(
            hits[0].filepath.as_deref(),
            Some("/notes/auth.md"),
            "the auth note must outrank the cooking note for a login query"
        );
    }

    #[tokio::test]
    async fn reindex_replaces_old_chunks_not_accumulates() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let s = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        s.index(2, "/n.md", "alpha beta gamma").unwrap();
        s.index(2, "/n.md", "delta epsilon zeta").unwrap();
        let conn = db.conn.lock();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM chunks WHERE filepath = '/n.md'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "re-index must replace, not accumulate, chunks");
    }

    #[test]
    fn salience_rewards_recency_and_access_but_stays_bounded() {
        let now = 1_000_000_000_000i64;
        let day = 86_400_000i64;
        // recency: a file touched today beats one touched 60 days ago.
        assert!(salience(now, Some(now), 0) > salience(now, Some(now - 60 * day), 0));
        // access: more-used beats unused at equal recency.
        assert!(salience(now, Some(now), 25) > salience(now, Some(now), 0));
        // bounded: always a nudge, never a takeover.
        for (last, acc) in [(Some(now), 0i64), (Some(now - 365 * day), 0), (Some(now), 1000), (None, 0)] {
            let s = salience(now, last, acc);
            assert!(s > 0.85 && s < 1.5, "salience {s} escaped the nudge band");
        }
    }

    /// L7 edge lifecycle (LLM extraction itself is gated/tested in `graph.rs`):
    /// re-indexing drops a file's prior edges and delete removes them. Edges are
    /// inserted manually here since unit tests have no LLM.
    #[tokio::test]
    async fn reindex_and_delete_clear_a_files_edges() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap());
        store.index(2, "/notes/proj.md", "anything").unwrap();
        let add_edge = || {
            db.conn
                .lock()
                .execute(
                    "INSERT INTO edges(from_path,to_path,edge_kind,created_at) \
                     VALUES ('/notes/proj.md','/memories/stripe.md','Organization',1)",
                    [],
                )
                .unwrap();
        };
        let count = || -> i64 {
            db.conn
                .lock()
                .query_row(
                    "SELECT count(*) FROM edges WHERE from_path = '/notes/proj.md'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        add_edge();
        assert_eq!(count(), 1);
        store.index(2, "/notes/proj.md", "changed").unwrap();
        assert_eq!(count(), 0, "re-index must drop prior edges");
        add_edge();
        store.remove("/notes/proj.md").unwrap();
        assert_eq!(count(), 0, "delete must drop edges");
    }

    /// Rename relabels the index (no re-embed) and drops the overwritten
    /// destination's stale rows. Fixes the "stale after rename" correctness bug.
    #[tokio::test]
    async fn rename_relabels_index_and_drops_overwritten_destination() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap());
        store.index(2, "/old.md", "alpha beta gamma").unwrap();
        store.index(3, "/dest.md", "delta epsilon zeta").unwrap();

        store.rename("/old.md", "/dest.md").unwrap();

        {
            let conn = db.conn.lock();
            let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
            assert_eq!(
                count("SELECT count(*) FROM chunks WHERE filepath='/old.md'"),
                0,
                "/old.md relabeled away"
            );
            let dest_text: String = conn
                .query_row("SELECT text FROM chunks WHERE filepath='/dest.md'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(dest_text, "alpha beta gamma", "/dest.md holds the moved content");
            assert_eq!(
                count("SELECT count(*) FROM chunks WHERE text LIKE '%delta%'"),
                0,
                "overwritten destination's original content must be dropped"
            );
        }

        // The rowid join survived the relabel — search still resolves /dest.md.
        let hits = store.search("alpha beta gamma", None).await.unwrap();
        assert_eq!(hits[0].filepath.as_deref(), Some("/dest.md"));
    }

    /// L7 co-mention boost: three identical-content files; two share an entity
    /// (manual edges), the third shares none → it ranks last.
    #[tokio::test]
    async fn comention_boost_demotes_the_unconnected_file() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap());
        for (ino, fp) in [(2u64, "/a.md"), (3, "/b.md"), (4, "/c.md")] {
            store.index(ino, fp, "alpha beta gamma").unwrap();
        }
        {
            let conn = db.conn.lock();
            for fp in ["/a.md", "/b.md"] {
                conn.execute(
                    "INSERT INTO edges(from_path,to_path,edge_kind,created_at) \
                     VALUES (?1,'/memories/x.md','Concept',1)",
                    [fp],
                )
                .unwrap();
            }
        }
        let hits = store.search("alpha beta gamma", None).await.unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits.last().unwrap().filepath.as_deref(),
            Some("/c.md"),
            "the file sharing no entity should rank last"
        );
    }

    /// L6 applied in search: two files with IDENTICAL content (equal RRF), the
    /// more-accessed one must rank first — proving salience breaks ties.
    #[tokio::test]
    async fn salience_breaks_ties_toward_more_accessed_file() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap());
        store.index(2, "/a.md", "alpha beta gamma delta").unwrap();
        store.index(3, "/b.md", "alpha beta gamma delta").unwrap();
        {
            let conn = db.conn.lock();
            conn.execute(
                "UPDATE chunks SET access_count = 50 WHERE filepath = '/b.md'",
                [],
            )
            .unwrap();
        }
        let hits = store.search("alpha beta gamma delta", None).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].filepath.as_deref(),
            Some("/b.md"),
            "the more-accessed file should win the tie via salience"
        );
    }

    /// E2E through the REAL filesystem write path (the same VFS methods the NFS
    /// mount drives): create_file → write → flush indexes the file, and unlink
    /// removes it. Proves the write-path wiring, not just the standalone index().
    #[tokio::test]
    async fn write_path_maintains_index_and_unlink_removes() {
        use crate::cache::{CacheFs, LocalIndexer, ROOT_INO};
        use crate::vfs::FileSystem;

        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap());
        let fs = CacheFs::new(db).with_indexer(store.clone() as Arc<dyn LocalIndexer>);

        // Create + write + flush exactly as the mount layer would.
        let (_attr, handle) = fs
            .create_file(ROOT_INO, "auth.md", 0o644, 0, 0)
            .await
            .unwrap();
        handle
            .write(0, b"user login and credential verification flow")
            .await
            .unwrap();
        handle.flush().await.unwrap();

        // Flush indexed it → search finds it.
        let hits = store.search("credential login", None).await.unwrap();
        assert!(!hits.is_empty(), "write path did not populate the index");
        assert_eq!(hits[0].filepath.as_deref(), Some("/auth.md"));

        // Unlink drops it from the index.
        fs.unlink(ROOT_INO, "auth.md").await.unwrap();
        let after = store.search("credential login", None).await.unwrap();
        assert!(
            after.is_empty(),
            "unlink must remove the file's chunks from the index, got {after:?}"
        );
    }

    /// FULL local pipeline with the REAL model: embed → index → search on a query
    /// with ZERO lexical overlap with the stored text. HashEmbedder cannot bridge
    /// this; only a real semantic model can — so passing proves offline semantic
    /// search works end to end. Skips if the model files aren't present.
    #[tokio::test]
    async fn real_model_offline_semantic_search() {
        use crate::embed::LocalEmbedder;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bash/node_modules/@huggingface/transformers/.cache/Xenova/all-MiniLM-L6-v2");
        if !dir.join("onnx/model.onnx").exists() {
            eprintln!("skipping real-model E2E: model not present at {dir:?}");
            return;
        }
        let db = Arc::new(Db::open_in_memory().unwrap());
        let emb = Arc::new(LocalEmbedder::from_dir(&dir, 384).unwrap());
        let s = SqliteVecStore::new(db, emb).unwrap();

        s.index(
            2,
            "/notes/auth.md",
            "the access token is refreshed by the middleware before each request",
        )
        .unwrap();
        s.index(
            3,
            "/notes/cooking.md",
            "fold the egg whites gently into the batter and bake until golden",
        )
        .unwrap();

        // No word here appears in the auth note — pure semantic match.
        let hits = s
            .search("how does login credential renewal work", None)
            .await
            .unwrap();
        assert!(!hits.is_empty(), "expected a semantic hit");
        assert_eq!(
            hits[0].filepath.as_deref(),
            Some("/notes/auth.md"),
            "real model must find the auth note for a zero-overlap login query"
        );
    }

    /// FULL pipeline with a CLOUD embedder: vectors come from OpenRouter
    /// (text-embedding-3-small, 1536d), but indexing + search stay local
    /// (vec0 `float[1536]` + fts5 + RRF). Proves the local pipeline is embedder-
    /// agnostic and the schema is dimension-agnostic. Gated on OPENROUTER_API_KEY.
    #[tokio::test]
    async fn cloud_model_local_index_semantic_search() {
        use crate::embed::OpenAiEmbedder;
        let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
            eprintln!("skipping cloud E2E: OPENROUTER_API_KEY not set");
            return;
        };
        let db = Arc::new(Db::open_in_memory().unwrap());
        let emb = Arc::new(OpenAiEmbedder::openrouter(key)); // 1536d
        let s = SqliteVecStore::new(db, emb).unwrap();

        s.index(
            2,
            "/notes/auth.md",
            "the access token is refreshed by the middleware before each request",
        )
        .unwrap();
        s.index(
            3,
            "/notes/cooking.md",
            "fold the egg whites gently into the batter and bake until golden",
        )
        .unwrap();

        let hits = s
            .search("how does login credential renewal work", None)
            .await
            .unwrap();
        assert!(!hits.is_empty(), "expected a semantic hit");
        assert_eq!(
            hits[0].filepath.as_deref(),
            Some("/notes/auth.md"),
            "cloud embeddings in the local index must find the auth note for a zero-overlap query"
        );
    }

    /// The reranker seam, exercised with a CLOUD reranker (OpenRouter/Cohere) over
    /// a deterministic HashEmbedder index — so no local model loads here. Proves
    /// rerank actually runs: the final score is a reranker score (≫ the ~1/60 RRF
    /// scores), and the on-topic file ranks first. Gated on OPENROUTER_API_KEY.
    #[tokio::test]
    async fn search_with_cloud_reranker_applies_rerank_scores() {
        use crate::rerank::CohereReranker;
        let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
            eprintln!("skipping cloud-reranker E2E: OPENROUTER_API_KEY not set");
            return;
        };
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store = SqliteVecStore::new(db, Arc::new(HashEmbedder::new(384)))
            .unwrap()
            .with_reranker(Arc::new(CohereReranker::openrouter(key)));

        store
            .index(
                2,
                "/notes/auth.md",
                "to reset your password click forgot password and follow the email link",
            )
            .unwrap();
        store
            .index(
                3,
                "/notes/cooking.md",
                "bananas are a good source of potassium and dietary fiber",
            )
            .unwrap();

        let hits = store
            .search("how do I reset my account password", None)
            .await
            .unwrap();
        assert_eq!(hits[0].filepath.as_deref(), Some("/notes/auth.md"));
        assert!(
            hits[0].similarity > 0.1,
            "final score should be the reranker's (≫ RRF's ~0.017), got {}",
            hits[0].similarity
        );
    }

    /// THE WHOLE PIPELINE to the reranker stage, over a realistic multi-doc
    /// corpus: L1 chunk → L2 embed (real local fastembed all-MiniLM) → L3 index
    /// (vec0 + fts5) → search (KNN ∪ BM25 → RRF) → L5 rerank (cloud Cohere).
    /// Query has ZERO lexical overlap with the target, so retrieval must be
    /// semantic; the reranker then confirms/refines the order. Gated on the
    /// local model dir AND OPENROUTER_API_KEY.
    #[tokio::test]
    async fn full_pipeline_local_embed_then_cloud_rerank() {
        use crate::embed::LocalEmbedder;
        use crate::rerank::CohereReranker;

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bash/node_modules/@huggingface/transformers/.cache/Xenova/all-MiniLM-L6-v2");
        if !dir.join("onnx/model.onnx").exists() {
            eprintln!("skipping full-pipeline test: local model not present");
            return;
        }
        let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
            eprintln!("skipping full-pipeline test: OPENROUTER_API_KEY not set");
            return;
        };

        let db = Arc::new(Db::open_in_memory().unwrap());
        let embedder = Arc::new(LocalEmbedder::from_dir(&dir, 384).unwrap());
        let store = SqliteVecStore::new(db, embedder)
            .unwrap()
            .with_reranker(Arc::new(CohereReranker::openrouter(key)));

        // A small workspace-like corpus across distinct topics.
        let corpus = [
            ("/notes/auth.md", "the access token is refreshed by the middleware before each request"),
            ("/notes/cooking.md", "fold the egg whites gently into the batter and bake until golden"),
            ("/notes/git.md", "rebase your branch onto main and force-push to update the pull request"),
            ("/notes/travel.md", "the bullet train from kyoto to osaka takes about fifteen minutes"),
            ("/notes/db.md", "create an index on the user column to speed up the slow report query"),
        ];
        for (i, (path, content)) in corpus.iter().enumerate() {
            store.index((i + 2) as u64, path, content).unwrap();
        }

        // Pure semantic query — no word here appears in auth.md.
        let hits = store
            .search("how does login credential renewal work", None)
            .await
            .unwrap();

        assert!(!hits.is_empty(), "pipeline returned no hits");
        assert_eq!(
            hits[0].filepath.as_deref(),
            Some("/notes/auth.md"),
            "full pipeline must rank the auth note first; got {:?}",
            hits.iter().map(|h| (&h.filepath, h.similarity)).collect::<Vec<_>>()
        );
        assert!(
            hits[0].similarity > 0.1,
            "top score should be the reranker's, got {}",
            hits[0].similarity
        );
    }

    // ── Realistic end-to-end tests (Workstream C) ───────────────────────────

    /// C2: a multi-chunk document — a needle in the MIDDLE is retrievable, and
    /// the returned chunk actually contains it (proves chunk-granular retrieval).
    #[tokio::test]
    async fn multi_chunk_doc_retrieves_middle_chunk() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap());
        let filler = (0..300).map(|n| format!("filler{n}")).collect::<Vec<_>>().join(" ");
        let content = format!("{filler} unicornmarker zebraquux {filler}");
        store.index(2, "/big.md", &content).unwrap();

        let n: i64 = db
            .conn
            .lock()
            .query_row("SELECT count(*) FROM chunks WHERE filepath='/big.md'", [], |r| r.get(0))
            .unwrap();
        assert!(n >= 2, "long doc must split into multiple chunks, got {n}");

        // A needle in the MIDDLE of a long, multi-chunk doc is still retrievable.
        // (Which chunk becomes the representative depends on rrf tie-breaking +
        // HashEmbedder bucket collisions, so we assert retrieval, not the snippet.)
        let hits = store.search("unicornmarker", None).await.unwrap();
        assert_eq!(hits[0].filepath.as_deref(), Some("/big.md"));
    }

    /// C4: the index persists to disk and survives reopen with NO re-embedding.
    #[tokio::test]
    async fn index_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        {
            let db = Arc::new(Db::open(&path).unwrap());
            let store = SqliteVecStore::new(db, Arc::new(HashEmbedder::new(384))).unwrap();
            store.index(2, "/p.md", "persistent alpha beta content").unwrap();
        } // store + db dropped — simulates a daemon restart

        let db2 = Arc::new(Db::open(&path).unwrap());
        let store2 = SqliteVecStore::open_existing(db2, Arc::new(HashEmbedder::new(384)));
        let hits = store2.search("persistent alpha", None).await.unwrap();
        assert_eq!(hits[0].filepath.as_deref(), Some("/p.md"));
    }

    /// C6: full FS lifecycle through the real VFS path — write, rename, delete —
    /// each tracked in the index.
    #[tokio::test]
    async fn full_lifecycle_tracked_in_index() {
        use crate::cache::{CacheFs, LocalIndexer, ROOT_INO};
        use crate::vfs::FileSystem;
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap());
        let fs = CacheFs::new(db).with_indexer(store.clone() as Arc<dyn LocalIndexer>);

        let (_, h) = fs.create_file(ROOT_INO, "doc.md", 0o644, 0, 0).await.unwrap();
        h.write(0, b"credential renewal flow").await.unwrap();
        h.flush().await.unwrap();
        assert_eq!(
            store.search("credential renewal", None).await.unwrap()[0]
                .filepath
                .as_deref(),
            Some("/doc.md")
        );

        fs.rename(ROOT_INO, "doc.md", ROOT_INO, "renamed.md").await.unwrap();
        let after = store.search("credential renewal", None).await.unwrap();
        assert_eq!(after[0].filepath.as_deref(), Some("/renamed.md"));
        assert!(after.iter().all(|x| x.filepath.as_deref() != Some("/doc.md")));

        fs.unlink(ROOT_INO, "renamed.md").await.unwrap();
        assert!(store.search("credential renewal", None).await.unwrap().is_empty());
    }

    /// C7: a binary (non-UTF-8) file is skipped by the indexer and never crashes.
    #[tokio::test]
    async fn binary_file_is_not_indexed() {
        use crate::cache::{CacheFs, LocalIndexer, ROOT_INO};
        use crate::vfs::FileSystem;
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap());
        let fs = CacheFs::new(db.clone()).with_indexer(store.clone() as Arc<dyn LocalIndexer>);

        let (_, h) = fs.create_file(ROOT_INO, "blob.bin", 0o644, 0, 0).await.unwrap();
        h.write(0, &[0xff, 0xfe, 0x00, 0x01, 0x80, 0x90]).await.unwrap();
        h.flush().await.unwrap(); // must not panic

        let n: i64 = db
            .conn
            .lock()
            .query_row("SELECT count(*) FROM chunks WHERE filepath='/blob.bin'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "binary file must not be indexed");
    }

    /// C5: concurrent writers on one on-disk db (WAL) — no lost writes, no corruption.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writers_one_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        {
            let db = Arc::new(Db::open(&path).unwrap());
            SqliteVecStore::new(db, Arc::new(HashEmbedder::new(384))).unwrap(); // create vec0 tables
        }
        let mut handles = vec![];
        for w in 0..2u64 {
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                let db = Arc::new(Db::open(&p).unwrap());
                let store = SqliteVecStore::open_existing(db, Arc::new(HashEmbedder::new(384)));
                for i in 0..10u64 {
                    store
                        .index(w * 100 + i + 2, &format!("/w{w}-{i}.md"), &format!("alpha {w} {i}"))
                        .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let db = Arc::new(Db::open(&path).unwrap());
        let n: i64 = db
            .conn
            .lock()
            .query_row("SELECT count(DISTINCT filepath) FROM chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 20, "all concurrent writes must land");
    }

    /// C3: hundreds of files — a unique needle is still found (brute-force KNN +
    /// BM25 hold at scale).
    #[tokio::test]
    async fn scale_hundreds_of_files() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db, Arc::new(HashEmbedder::new(384))).unwrap());
        for i in 0..300u64 {
            store
                .index(i + 2, &format!("/f{i}.md"), &format!("document {i} about topic{}", i % 7))
                .unwrap();
        }
        store
            .index(9999, "/needle.md", "the singular zebraquux marker lives here alone")
            .unwrap();
        let hits = store.search("zebraquux marker", None).await.unwrap();
        assert_eq!(
            hits[0].filepath.as_deref(),
            Some("/needle.md"),
            "needle must surface among 301 files"
        );
    }
}
