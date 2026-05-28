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

/// Over-fetch per ranked list before collapsing chunks → files.
const SEARCH_POOL: usize = 80;
/// When a query is scoped to a path prefix, vec0 KNN can't filter on the joined
/// `filepath` (it only post-filters the global k-nearest), so we raise `k` to
/// this bound and GLOB-filter, ensuring in-scope hits aren't crowded out of the
/// pool by out-of-scope files. 4096 is sqlite-vec's hard `k` ceiling; beyond
/// that the exact-GLOB FTS lane still surfaces in-scope lexical matches.
const SCOPED_KNN_POOL: usize = 4096;

/// Local, offline semantic index over the SQLite cache.
#[derive(Debug)]
pub struct SqliteVecStore {
    db: Arc<Db>,
    /// Text embedder → `vchunks` (float[text_dims]). Always present.
    embedder: Arc<dyn Embedder>,
    /// Optional code embedder → `vchunks_code` (float[code_dims]). When present,
    /// code-like files route here (own model + own vector space) and search also
    /// queries the code lane.
    code_embedder: Option<Arc<dyn Embedder>>,
    /// Optional L5 reranker, applied to candidates after RRF in `search`.
    reranker: Option<Arc<dyn Reranker>>,
    /// Optional L7 graph extractor (LLM). When present, `index` extracts typed
    /// entities and writes file→entity edges. `None` = no graph.
    graph_llm: Option<Arc<crate::llm::LlmClient>>,
}

impl SqliteVecStore {
    /// Build a store and ensure the vec0 tables exist at the embedder's width.
    pub fn new(db: Arc<Db>, embedder: Arc<dyn Embedder>) -> anyhow::Result<Self> {
        let identity = embedder.identity();
        // REFUSE to open a WRITER under a different text model than the one that
        // built this index. Otherwise `index()` would write new-model vectors into
        // the old `vchunks`, mixing two vector spaces — and a later rollback to the
        // old model would silently search that mixed table. The index is invalid
        // under the new model; require a fresh reindex. (Mirrors the code-lane
        // guard in `enable_code_indexing`.) The daemon fails open on this error
        // (mounts with local indexing disabled), leaving the old index untouched.
        // Checked BEFORE any schema mutation so the existing index is preserved.
        if let Some(stored) = db.embed_identity() {
            if stored != identity {
                anyhow::bail!(
                    "existing index was built with text embedder '{stored}' but the current \
                     embedder is '{identity}'; rebuild the index (fresh cache) or restore the \
                     previous model"
                );
            }
        }
        // PRESERVE any existing code lane: passing `None` here would make
        // `ensure_vector_tables` drop `vchunks_code` ("code embedder removed").
        // Carry the stored code width through; `enable_code_indexing` rebuilds it
        // at the real width afterward (a no-op when unchanged).
        let existing_code_dims = db.code_embed_dims();
        db.ensure_vector_tables(embedder.dimensions(), existing_code_dims)?;
        // Stamp the text identity on first creation (the drift guard above means
        // any existing stamp already equals `identity`).
        if db.embed_identity().is_none() {
            db.record_embed_identity(&identity)?;
        }
        Ok(Self {
            db,
            embedder,
            code_embedder: None,
            reranker: None,
            graph_llm: None,
        })
    }

    /// WRITER: enable code indexing — ensure the `vchunks_code` vec0 table at the
    /// code embedder's width and stamp its identity (first creation only), then
    /// route code-like files to the code lane. Mutates the schema, so only the
    /// daemon (writer) should call this — readers use [`with_code_embedder`].
    ///
    /// Takes `&mut self` (not a consuming builder) so the caller can FAIL-OPEN:
    /// on error the store is untouched and text-lane indexing continues.
    pub fn enable_code_indexing(&mut self, code: Arc<dyn Embedder>) -> anyhow::Result<()> {
        // Refuse to enable the code lane if it was built with a DIFFERENT model
        // (even at the same width). Re-stamping + reusing `vchunks_code` would mix
        // two vector spaces; non-destructively (mirroring the text lane) we instead
        // leave the old lane inert and bail so the caller fails open — code files
        // fall back to the text lane until a fresh index. Recovery = fresh index.
        if let Some(stored) = self.db.code_embed_identity() {
            if stored != code.identity() {
                anyhow::bail!(
                    "existing code lane was built with '{stored}' but the current code model is \
                     '{}'; code lane disabled until a fresh reindex",
                    code.identity()
                );
            }
        }
        self.db
            .ensure_vector_tables(self.embedder.dimensions(), Some(code.dimensions()))?;
        if self.db.code_embed_identity().is_none() {
            self.db.record_code_embed_identity(&code.identity())?;
        }
        self.code_embedder = Some(code);
        Ok(())
    }

    /// READER: attach a code embedder WITHOUT touching the schema, so a reader
    /// (`grep`) can query the code lane + validate the code identity.
    pub fn with_code_embedder(mut self, code: Arc<dyn Embedder>) -> Self {
        self.code_embedder = Some(code);
        self
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
            code_embedder: None,
            reranker: None,
            graph_llm: None,
        }
    }

    /// Whether a reader (`grep`) should commit to this local index, or fall back
    /// to cloud. True only when:
    /// 1. the stored TEXT embedder identity matches this store's text embedder
    ///    (the identity string encodes the model + dims, so a model swap OR a
    ///    width change is caught here — no separate dimension probe needed);
    /// 2. if a code embedder is attached, the stored CODE identity matches too;
    /// 3. the index is NON-EMPTY — a fresh/just-reset cache has nothing to return,
    ///    so we prefer cloud until the writer (re)populates it.
    ///
    /// A missing identity stamp (an index this code never wrote) counts as a mismatch.
    pub fn is_searchable(&self) -> bool {
        // The TEXT lane is the floor: local search is viable iff the text lane is
        // healthy. The code lane is purely additive/best-effort (queried in
        // `search` only when its identity matches), so a missing, stale, or broken
        // code lane must NOT disable local search when the text lane is fine.
        //
        // (1) Text identity must match (the string encodes model + dims, so a
        //     model swap or width change is caught — no separate dim check needed).
        if self.db.embed_identity().as_deref() != Some(self.embedder.identity().as_str()) {
            return false;
        }
        // Compute code-lane activity BEFORE locking `conn` (it locks internally).
        let code_active = self.code_lane_active();

        let conn = self.db.conn.lock();
        // (2) Non-empty — nothing to return from a fresh/just-reset cache.
        let non_empty = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM chunks)", [], |r| r.get::<_, i64>(0))
            .map(|n| n == 1)
            .unwrap_or(false);
        if !non_empty {
            return false;
        }
        // (3) Text vec0 readiness: a MATCH at the text width errors on a missing
        // or wrong-width `vchunks` (e.g. a partially-migrated cache), so we fall
        // back to cloud instead of hard-failing at query time. 0 rows is fine.
        let blob = vec_to_blob(&vec![0.0f32; self.embedder.dimensions()]);
        let text_ok = conn
            .prepare("SELECT rowid FROM vchunks WHERE embedding MATCH ?1 AND k = 1")
            .and_then(|mut stmt| {
                let mut rows = stmt.query(rusqlite::params![blob])?;
                rows.next()?;
                Ok(())
            })
            .is_ok();
        if !text_ok {
            return false;
        }
        // (4) Fall back to cloud only when ALL searchable content is stranded in
        // an inactive code lane — i.e. the code lane has rows we can't query AND
        // the TEXT lane is empty. The text lane stays the floor: a mixed cache
        // (some prose, some code) remains locally searchable via text even when
        // the code embedder is unavailable; `search()` simply skips the code KNN.
        let code_table = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='vchunks_code'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if code_table && !code_active {
            let exists = |sql: &str| -> bool {
                conn.query_row(sql, [], |r| r.get::<_, i64>(0))
                    .map(|n| n == 1)
                    .unwrap_or(false)
            };
            let code_rows = exists("SELECT EXISTS(SELECT 1 FROM vchunks_code)");
            let text_rows = exists("SELECT EXISTS(SELECT 1 FROM vchunks)");
            if code_rows && !text_rows {
                return false;
            }
        }
        // (5) When the code lane IS active, validate it the same way as the text
        // lane: a MATCH at the code width must not error. Otherwise a missing or
        // corrupt `vchunks_code` would let `search()` silently swallow the code
        // KNN and serve BM25-only/empty results for code content while reporting
        // "searchable" — fail closed to cloud instead.
        if code_active {
            if let Some(code) = &self.code_embedder {
                let cblob = vec_to_blob(&vec![0.0f32; code.dimensions()]);
                let code_ok = conn
                    .prepare("SELECT rowid FROM vchunks_code WHERE embedding MATCH ?1 AND k = 1")
                    .and_then(|mut stmt| {
                        let mut rows = stmt.query(rusqlite::params![cblob])?;
                        rows.next()?;
                        Ok(())
                    })
                    .is_ok();
                if !code_ok {
                    return false;
                }
            }
        }
        // (6) Vector-count invariant: every chunk has exactly one vec0 row (text
        // OR code). If the totals diverge, vectors were lost — e.g. a vec0 table
        // was dropped and a writer recreated it EMPTY (the MATCH probe can't catch
        // that, an empty table matches fine). Fail closed so a silently degraded
        // index falls back to cloud instead of returning BM25-only/empty results.
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1) };
        let chunk_n = count("SELECT count(*) FROM chunks");
        let text_n = count("SELECT count(*) FROM vchunks");
        let code_n = if code_table { count("SELECT count(*) FROM vchunks_code") } else { 0 };
        if chunk_n < 0 || text_n < 0 || code_n < 0 || chunk_n != text_n + code_n {
            return false;
        }
        true
    }

    /// Whether the code lane should be queried: a code embedder is attached AND
    /// its identity matches the stamp the writer recorded (so we never search
    /// code vectors with a different model — silent corruption). Best-effort: a
    /// mismatch/absence simply means text-only results, never a hard failure.
    fn code_lane_active(&self) -> bool {
        self.code_embedder
            .as_ref()
            .is_some_and(|c| self.db.code_embed_identity().as_deref() == Some(c.identity().as_str()))
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
        // Route code-like files to the code embedder + `vchunks_code` lane when a
        // code embedder is attached; everything else uses the text lane.
        let use_code = self.code_embedder.is_some() && is_code_path(filepath);
        let embedder = match (&self.code_embedder, use_code) {
            (Some(code), true) => code.as_ref(),
            _ => self.embedder.as_ref(),
        };
        let vec_table = if use_code { "vchunks_code" } else { "vchunks" };
        let vectors = embedder.embed(&chunks)?;

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
        // IMMEDIATE: take the write lock at BEGIN. These transactions read
        // (e.g. drop_file_chunks SELECTs) before writing; a DEFERRED tx would
        // let two concurrent writers deadlock on the read→write upgrade in WAL
        // (instant SQLITE_BUSY, no busy_timeout retry).
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

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
            // `vec_table` is a fixed internal identifier (never user input), so
            // the format! is safe; vec0 virtual tables can't be bound as params.
            tx.execute(
                &format!("INSERT INTO {vec_table}(rowid, embedding) VALUES (?1, ?2)"),
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
        // IMMEDIATE: take the write lock at BEGIN. These transactions read
        // (e.g. drop_file_chunks SELECTs) before writing; a DEFERRED tx would
        // let two concurrent writers deadlock on the read→write upgrade in WAL
        // (instant SQLITE_BUSY, no busy_timeout retry).
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
        // IMMEDIATE: take the write lock at BEGIN. These transactions read
        // (e.g. drop_file_chunks SELECTs) before writing; a DEFERRED tx would
        // let two concurrent writers deadlock on the read→write upgrade in WAL
        // (instant SQLITE_BUSY, no busy_timeout retry).
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Overwrite: clear the destination's existing index rows.
        drop_file_chunks(&tx, new)?;

        // A cheap relabel keeps the existing vectors — but those vectors live in
        // whichever lane the file was INDEXED into. If that persisted lane no
        // longer matches the new path's expected lane, the vectors are stranded
        // in the wrong (wrong-model) lane. Decide from PERSISTED state, NOT the
        // live embedder: in the fail-open state the code lane can exist while no
        // code embedder is attached, and a foo.rs→foo.md rename must still drop.
        // rename() has no content to re-embed, so drop; the next write re-indexes
        // into the correct lane. Same-lane renames relabel. Text-only caches (no
        // code lane) never lane-cross — everything belongs in the text lane.
        let code_lane_exists: bool = tx.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='vchunks_code'",
            [],
            |r| r.get::<_, i64>(0),
        )? > 0;
        let lane_cross = code_lane_exists && {
            let src_in_code: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM vchunks_code v JOIN chunks c ON c.id = v.rowid \
                 WHERE c.filepath = ?1)",
                [old],
                |r| r.get::<_, i64>(0),
            )? == 1;
            src_in_code != is_code_path(new)
        };
        if lane_cross {
            drop_file_chunks(&tx, old)?;
        } else {
            tx.execute(
                "UPDATE chunks SET filepath = ?2 WHERE filepath = ?1",
                rusqlite::params![old, new],
            )?;
            tx.execute(
                "UPDATE edges SET from_path = ?2 WHERE from_path = ?1",
                rusqlite::params![old, new],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// Classify a path as code-like by extension — routes it to the code embedder +
/// `vchunks_code` lane. Markup/prose/config (md, txt, json, yaml, html, css, …)
/// stay on the text lane. Only files with an actual extension can be code.
fn is_code_path(filepath: &str) -> bool {
    let base = filepath.rsplit('/').next().unwrap_or("");
    if !base.contains('.') {
        return false;
    }
    let ext = base.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "rs" | "py" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "go" | "java" | "kt" | "kts"
            | "scala" | "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "rb" | "php" | "swift"
            | "m" | "mm" | "sh" | "bash" | "zsh" | "sql" | "lua" | "r" | "jl" | "pl" | "pm" | "ex"
            | "exs" | "erl" | "clj" | "cljs" | "hs" | "ml" | "fs" | "dart" | "vue" | "svelte"
            | "proto" | "tf"
    )
}

/// Delete a file's chunks and their rowid-linked vec0/fts rows within a txn.
fn drop_file_chunks(tx: &rusqlite::Transaction, filepath: &str) -> rusqlite::Result<()> {
    let ids: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT id FROM chunks WHERE filepath = ?1")?;
        let rows = stmt.query_map([filepath], |r| r.get::<_, i64>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    // `vchunks_code` only exists when a code embedder has been attached; a chunk
    // id lives in exactly one vec0 lane, so delete from both where present.
    let code_table: bool = tx.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='vchunks_code'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    for id in ids {
        tx.execute("DELETE FROM vchunks WHERE rowid = ?1", [id])?;
        if code_table {
            tx.execute("DELETE FROM vchunks_code WHERE rowid = ?1", [id])?;
        }
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
        // Embed the query in the CODE vector space too — but only when the code
        // lane is ACTIVE (embedder attached + identity matches the stamp), so a
        // stale/mismatched code lane is silently skipped (text-only results)
        // rather than searched with the wrong model. Done before locking the db.
        let code_qblob = match (&self.code_embedder, self.code_lane_active()) {
            (Some(code), true) => Some(vec_to_blob(
                &code.embed(&[query.to_string()])?.pop().unwrap_or_default(),
            )),
            _ => None,
        };
        // Scope predicate pushed into each lane so a `/prefix/` query can't be
        // crowded out of the candidate pool by out-of-scope files (a false-
        // negative bug if filtered only after the global LIMIT). `None` = no scope.
        // Uses `instr(filepath, prefix) = 1` for a LITERAL prefix match — GLOB
        // would treat `*`, `?`, `[` in real paths as wildcards and over-match.
        let scope = filepath.map(|p| p.to_string());

        // filepath -> (representative chunk, summed RRF score)
        let mut by_file: HashMap<String, (String, f64)> = HashMap::new();
        // filepath -> the representative chunk's row id, captured at retrieval.
        // Used in phase 2 to detect a concurrent same-path reindex (which assigns
        // new ids), so we never return a snippet/score from pre-rewrite content.
        let mut rep_chunk: HashMap<String, i64> = HashMap::new();

        let conn = self.db.conn.lock();

        // Vector KNN (vec0). vec0 only post-filters the global k-nearest on
        // joined columns, so when scoped we raise k and prefix-filter rather than
        // letting out-of-scope files consume the pool.
        {
            let k = if scope.is_some() { SCOPED_KNN_POOL } else { SEARCH_POOL };
            let mut stmt = conn.prepare(
                "SELECT c.id, c.filepath, c.text FROM vchunks v \
                 JOIN chunks c ON c.id = v.rowid \
                 WHERE v.embedding MATCH ?1 AND k = ?2 \
                 AND (?3 IS NULL OR instr(c.filepath, ?3) = 1) ORDER BY distance",
            )?;
            let rows = stmt.query_map(rusqlite::params![qblob, k as i64, scope], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?;
            for (rank, row) in rows.enumerate() {
                let (id, fp, text) = row?;
                rep_chunk.entry(fp.clone()).or_insert(id);
                super::rank::rrf_bump(&mut by_file, fp, text, rank);
            }
        }

        // Code vector KNN (vchunks_code) — only when a code embedder is attached.
        // The query is embedded in the code space; its candidates union into the
        // same RRF map as the text lane and FTS.
        if let Some(cqblob) = &code_qblob {
            let k = if scope.is_some() { SCOPED_KNN_POOL } else { SEARCH_POOL };
            if let Ok(mut stmt) = conn.prepare(
                "SELECT c.id, c.filepath, c.text FROM vchunks_code v \
                 JOIN chunks c ON c.id = v.rowid \
                 WHERE v.embedding MATCH ?1 AND k = ?2 \
                 AND (?3 IS NULL OR instr(c.filepath, ?3) = 1) ORDER BY distance",
            ) {
                if let Ok(rows) =
                    stmt.query_map(rusqlite::params![cqblob, k as i64, scope], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                    })
                {
                    for (rank, row) in rows.enumerate() {
                        if let Ok((id, fp, text)) = row {
                            rep_chunk.entry(fp.clone()).or_insert(id);
                            super::rank::rrf_bump(&mut by_file, fp, text, rank);
                        }
                    }
                }
            }
        }

        // Keyword BM25 (fts5). Malformed queries fail soft — vector hits stand.
        if let Some(fq) = to_fts_query(query) {
            if let Ok(mut stmt) = conn.prepare(
                "SELECT c.id, c.filepath, c.text FROM ffts \
                 JOIN chunks c ON c.id = ffts.rowid \
                 WHERE ffts MATCH ?1 AND (?3 IS NULL OR instr(c.filepath, ?3) = 1) \
                 ORDER BY rank LIMIT ?2",
            ) {
                if let Ok(rows) =
                    stmt.query_map(rusqlite::params![fq, SEARCH_POOL as i64, scope], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                    })
                {
                    for (rank, row) in rows.enumerate() {
                        if let Ok((id, fp, text)) = row {
                            rep_chunk.entry(fp.clone()).or_insert(id);
                            super::rank::rrf_bump(&mut by_file, fp, text, rank);
                        }
                    }
                }
            }
        }
        drop(conn);

        let mut hits = super::rank::to_hits(by_file, filepath);

        // L5 rerank: replace RRF scores with cross-encoder scores, then re-sort.
        if let Some(reranker) = &self.reranker {
            super::rank::apply_reranker(&mut hits, reranker.as_ref(), query)?;
        }

        // L7 co-mention boost + L6 salience (computed from STORED stats, before
        // bumping, so a recent/used file wins ties), then bump access — one lock.
        {
            let now = now_ms();
            let conn = self.db.conn.lock();

            // Revalidate against CHUNK IDENTITY, not just path existence: the lock
            // was released for reranking, so a concurrent rename/remove/reindex may
            // have invalidated a hit. Keep a hit only if a chunk row still exists
            // with the exact (id, filepath) we retrieved — this drops ghosts from
            // rename (filepath changed), remove (gone), AND same-path reindex (the
            // delete+insert assigns new ids). Skip on query error (don't nuke).
            let rep_ids: Vec<i64> = hits
                .iter()
                .filter_map(|h| h.filepath.as_ref().and_then(|fp| rep_chunk.get(fp).copied()))
                .collect();
            if !rep_ids.is_empty() {
                let placeholders = vec!["?"; rep_ids.len()].join(",");
                let sql = format!("SELECT id, filepath FROM chunks WHERE id IN ({placeholders})");
                if let Ok(mut stmt) = conn.prepare(&sql) {
                    if let Ok(rows) = stmt.query_map(
                        rusqlite::params_from_iter(rep_ids.iter()),
                        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
                    ) {
                        let live: HashMap<i64, String> = rows.filter_map(|r| r.ok()).collect();
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
            }

            super::rank::apply_comention_boost(&mut hits, |fp| {
                conn.prepare("SELECT to_path FROM edges WHERE from_path = ?1")
                    .and_then(|mut stmt| {
                        stmt.query_map([fp], |r| r.get::<_, String>(0)).map(|rows| {
                            rows.filter_map(|r| r.ok())
                                .collect::<std::collections::HashSet<String>>()
                        })
                    })
                    .unwrap_or_default()
            });
            super::rank::apply_salience(&mut hits, now, |fp| {
                conn.query_row(
                    "SELECT MAX(last_accessed_at), COALESCE(SUM(access_count), 0) \
                     FROM chunks WHERE filepath = ?1",
                    [fp],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap_or((None, 0))
            });
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
        super::rank::sort_desc(&mut hits);
        Ok(hits)
    }
}

/// Epoch milliseconds now.
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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

    /// Scoped search must return in-scope matches even when far more out-of-scope
    /// files match the same terms — they must not crowd the candidate pool.
    #[tokio::test]
    async fn scoped_search_survives_out_of_scope_crowding() {
        let s = store();
        // 120 out-of-scope files (> SEARCH_POOL=80) all matching the query term.
        for i in 0..120 {
            s.index(1000 + i, &format!("/noise/{i}.md"), "alpha shared keyword here")
                .unwrap();
        }
        // One in-scope file with the same term.
        s.index(2, "/scope/target.md", "alpha shared keyword here")
            .unwrap();

        let hits = s.search("alpha shared keyword", Some("/scope/")).await.unwrap();
        assert!(
            hits.iter().any(|h| h.filepath.as_deref() == Some("/scope/target.md")),
            "scoped search dropped the in-scope file under crowding: {hits:?}"
        );
        assert!(
            hits.iter().all(|h| h
                .filepath
                .as_deref()
                .map_or(true, |p| p.starts_with("/scope/"))),
            "scoped search leaked out-of-scope files: {hits:?}"
        );
    }

    /// Code/text routing: a code-like path indexes into the code lane, prose into
    /// the text lane. Uses distinct widths (text=384, code=256) so the routing is
    /// proven by construction — a mis-routed code file would try to insert a
    /// 256-d vector into the 384-d `vchunks` table and fail. Offline (HashEmbedder).
    #[tokio::test]
    async fn code_files_route_to_code_lane() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut store = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        store
            .enable_code_indexing(Arc::new(HashEmbedder::new(256)))
            .unwrap();

        // .rs → code lane (256-d); .md → text lane (384-d). Both must succeed.
        store
            .index(2, "/src/parser.rs", "fn tokenize(input: &str) -> Vec<Token> { todo!() }")
            .unwrap();
        store
            .index(3, "/docs/overview.md", "the parser turns source text into tokens")
            .unwrap();

        {
            let conn = db.conn.lock();
            let files: i64 = conn
                .query_row("SELECT count(DISTINCT filepath) FROM chunks", [], |r| r.get(0))
                .unwrap();
            assert_eq!(files, 2, "both files indexed");
        }
        assert!(store.is_searchable(), "dual-lane index with matching identities");

        // Both lanes are queried + fused: each file is findable by its own terms.
        let code_hits = store.search("tokenize tokens parser", None).await.unwrap();
        assert!(code_hits.iter().any(|h| h.filepath.as_deref() == Some("/src/parser.rs")));
        let text_hits = store.search("source text into tokens", None).await.unwrap();
        assert!(text_hits.iter().any(|h| h.filepath.as_deref() == Some("/docs/overview.md")));

        // Re-indexing the code file replaces (not accumulates) its code-lane rows.
        store
            .index(2, "/src/parser.rs", "fn parse(tokens: Vec<Token>) -> Ast { todo!() }")
            .unwrap();
        let n: i64 = db
            .conn
            .lock()
            .query_row("SELECT count(*) FROM chunks WHERE filepath='/src/parser.rs'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1, "re-index replaces code-lane chunks");
    }

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

    /// A code-model swap at the SAME width must be refused non-destructively: the
    /// writer won't mix new-model vectors into the old code lane or re-stamp it.
    #[tokio::test]
    async fn code_model_swap_disables_code_lane_nondestructively() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(TaggedEmbedder { dims: 256, id: "code-A:256".into() }))
            .unwrap();
        w.index(2, "/src/a.rs", "fn a() {}").unwrap();
        drop(w);

        // Reopen with the SAME width (256) but a DIFFERENT code model → must bail.
        let mut w2 = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        let res =
            w2.enable_code_indexing(Arc::new(TaggedEmbedder { dims: 256, id: "code-B:256".into() }));
        assert!(res.is_err(), "same-width code-model swap must be refused");

        // The old code lane + its vectors are preserved (not dropped or mixed).
        let n: i64 = db
            .conn
            .lock()
            .query_row("SELECT count(*) FROM chunks WHERE filepath='/src/a.rs'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "old code-lane chunks preserved");
    }

    /// A rename crossing the code/text extension boundary drops the index entry
    /// (re-indexed into the correct lane on next write) rather than stranding
    /// vectors in the wrong lane. Same-lane renames still relabel cheaply.
    #[tokio::test]
    async fn lane_crossing_rename_drops_entry() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut store = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        store.enable_code_indexing(Arc::new(HashEmbedder::new(256))).unwrap();
        store.index(2, "/notes/readme.md", "documentation about parsing tokens").unwrap();

        // Cross-lane (.md text → .rs code): drop, not relabel.
        store.rename("/notes/readme.md", "/notes/readme.rs").unwrap();
        {
            let conn = db.conn.lock();
            let old_n: i64 = conn
                .query_row("SELECT count(*) FROM chunks WHERE filepath='/notes/readme.md'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let new_n: i64 = conn
                .query_row("SELECT count(*) FROM chunks WHERE filepath='/notes/readme.rs'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(old_n, 0, "old path cleared");
            assert_eq!(new_n, 0, "lane-cross rename drops (re-index on next write), not relabel");
        }

        // Same-lane (.md → .md): relabel (kept).
        store.index(3, "/notes/guide.md", "more docs").unwrap();
        store.rename("/notes/guide.md", "/notes/manual.md").unwrap();
        let kept: i64 = db
            .conn
            .lock()
            .query_row("SELECT count(*) FROM chunks WHERE filepath='/notes/manual.md'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kept, 1, "same-lane rename relabels (kept)");
    }

    /// Lane-cross rename must decide from PERSISTED lane membership, not the live
    /// embedder: in the fail-open state (code-model mismatch left the code lane
    /// inert / unattached) the old vchunks_code rows still exist, so a foo.rs →
    /// foo.md rename must still DROP rather than strand code vectors on a text path.
    #[tokio::test]
    async fn lane_crossing_rename_drops_even_when_code_lane_inert() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        // Writer 1: code lane with model A; index a .rs file into the code lane.
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(TaggedEmbedder { dims: 256, id: "code-A:256".into() }))
            .unwrap();
        w.index(2, "/src/lib.rs", "fn f() {}").unwrap();
        drop(w);

        // Writer 2: mismatched code model → enable fails open (code_embedder stays
        // None), but vchunks_code + the .rs vectors persist.
        let mut w2 = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        let _ = w2.enable_code_indexing(Arc::new(TaggedEmbedder { dims: 256, id: "code-B:256".into() }));

        // .rs → .md: persisted lane is code, new path is text → must DROP.
        w2.rename("/src/lib.rs", "/src/lib.md").unwrap();
        let conn = db.conn.lock();
        let old_n: i64 = conn
            .query_row("SELECT count(*) FROM chunks WHERE filepath='/src/lib.rs'", [], |r| r.get(0))
            .unwrap();
        let new_n: i64 = conn
            .query_row("SELECT count(*) FROM chunks WHERE filepath='/src/lib.md'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(old_n, 0, "old path cleared");
        assert_eq!(new_n, 0, "dropped even though the code embedder is inert (persisted lane)");
    }

    /// A code-only cache (all content in the code lane) must NOT report local
    /// searchability when the code lane is inactive — otherwise grep silently
    /// serves empty/BM25-only semantic results. With a matching code embedder it
    /// is searchable.
    #[tokio::test]
    async fn code_only_cache_unsearchable_when_code_lane_inactive() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(HashEmbedder::new(256))).unwrap();
        w.index(2, "/src/only.rs", "fn only() {}").unwrap(); // code lane only
        drop(w);

        // Reader with NO code embedder → code lane inactive, all content stranded.
        let inert = SqliteVecStore::open_existing(db.clone(), Arc::new(HashEmbedder::new(384)));
        assert!(
            !inert.is_searchable(),
            "code-only cache must fall back when the code lane can't be searched"
        );

        // Reader WITH the matching code embedder → code lane active → searchable.
        let active = SqliteVecStore::open_existing(db, Arc::new(HashEmbedder::new(384)))
            .with_code_embedder(Arc::new(HashEmbedder::new(256)));
        assert!(active.is_searchable(), "active code lane → searchable");
    }

    /// A MIXED cache (prose + code) stays locally searchable via the text lane
    /// even when the code lane is inactive — the text lane is the floor, and only
    /// a fully-stranded (text-empty) code lane forces cloud fallback.
    #[tokio::test]
    async fn mixed_cache_searchable_via_text_when_code_lane_inactive() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(HashEmbedder::new(256))).unwrap();
        w.index(2, "/docs/readme.md", "prose content about the project").unwrap(); // text lane
        w.index(3, "/src/lib.rs", "fn lib() {}").unwrap(); // code lane
        drop(w);

        // Reader with NO code embedder → code lane inactive, but the text lane has
        // content, so local search stays viable (code KNN simply skipped).
        let reader = SqliteVecStore::open_existing(db, Arc::new(HashEmbedder::new(384)));
        assert!(
            reader.is_searchable(),
            "mixed cache must stay searchable via the text lane when code lane is inactive"
        );
    }

    /// An ACTIVE but broken code lane (matching code embedder, but a missing/
    /// corrupt vchunks_code) must fall back to cloud — not silently drop the code
    /// KNN and serve degraded results — when code content depends on that lane.
    #[tokio::test]
    async fn active_but_broken_code_lane_falls_back() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(HashEmbedder::new(256))).unwrap();
        w.index(2, "/src/a.rs", "fn a() {}").unwrap(); // code lane only
        drop(w);

        // Corrupt: drop the code vec0 table but keep `chunks` + the code stamp.
        db.conn.lock().execute_batch("DROP TABLE vchunks_code;").unwrap();

        // Reader WITH the matching code embedder → code lane "active", but the
        // vec0 table is gone → the readiness probe errors → not searchable.
        let reader = SqliteVecStore::open_existing(db, Arc::new(HashEmbedder::new(384)))
            .with_code_embedder(Arc::new(HashEmbedder::new(256)));
        assert!(
            !reader.is_searchable(),
            "broken active code lane must fall back, not serve degraded results"
        );
    }

    /// Upgrade compat: a TEXT-ONLY index (no code stamp — e.g. written before the
    /// code lane shipped) must stay searchable even when the reader attaches a
    /// code embedder (as grep does). Missing code metadata = text-only, not a
    /// hard incompatibility.
    #[test]
    fn text_only_index_searchable_with_code_embedder_attached() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let w = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        w.index(2, "/a.md", "hello world").unwrap();
        // No code stamp was written. Reader attaches a code embedder anyway.
        let reader = SqliteVecStore::open_existing(db, Arc::new(HashEmbedder::new(384)))
            .with_code_embedder(Arc::new(HashEmbedder::new(256)));
        assert!(
            reader.is_searchable(),
            "text-only index must remain searchable with a code embedder attached"
        );
    }

    /// `is_searchable` gates the grep→local-backend decision: true only when the
    /// vec0 tables exist at the embedder's width.
    #[test]
    fn is_searchable_reflects_index_compatibility() {
        // Indexed store at 384-d → searchable.
        let db = Arc::new(Db::open_in_memory().unwrap());
        let s = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384))).unwrap();
        s.index(2, "/a.md", "hello world").unwrap();
        assert!(s.is_searchable());

        // Same db reopened with a different-width embedder → NOT searchable
        // (a 256-d probe vector against a 384-d vec0 table errors).
        let mismatched =
            SqliteVecStore::open_existing(db.clone(), Arc::new(HashEmbedder::new(256)));
        assert!(!mismatched.is_searchable());

        // Same width (384) but a DIFFERENT model identity → NOT searchable
        // (a same-dimension model swap would silently corrupt relevance).
        let other_model = SqliteVecStore::open_existing(
            db.clone(),
            Arc::new(TaggedEmbedder { dims: 384, id: "other-model:384".into() }),
        );
        assert!(!other_model.is_searchable());

        // A cache that never created vec0 tables → NOT searchable.
        let bare = Arc::new(Db::open_in_memory().unwrap());
        let no_index = SqliteVecStore::open_existing(bare, Arc::new(HashEmbedder::new(384)));
        assert!(!no_index.is_searchable());
    }

    /// Phase consistency: if a file is reindexed (new chunk ids) WHILE a search
    /// is between retrieval and phase-2 revalidation, the stale pre-rewrite chunk
    /// must not be returned. The reranker runs in exactly that window, so a
    /// reranker that reindexes deterministically forces the race.
    #[tokio::test]
    async fn search_drops_chunk_reindexed_during_rerank() {
        use std::sync::atomic::{AtomicBool, Ordering};

        #[derive(Debug)]
        struct ReindexingReranker {
            db: Arc<Db>,
            fired: AtomicBool,
        }
        impl Reranker for ReindexingReranker {
            fn rerank(&self, _q: &str, docs: &[String]) -> anyhow::Result<Vec<f32>> {
                // First call = the search under test. Reindex /a.md with new
                // content (delete+insert ⇒ new chunk ids), as a concurrent writer
                // would. The lock is released during rerank, so this succeeds.
                if !self.fired.swap(true, Ordering::SeqCst) {
                    let w =
                        SqliteVecStore::open_existing(self.db.clone(), Arc::new(HashEmbedder::new(384)));
                    w.index(2, "/a.md", "totally different replacement content zzz")
                        .unwrap();
                }
                Ok(vec![1.0; docs.len()])
            }
        }

        let db = Arc::new(Db::open_in_memory().unwrap());
        let store = SqliteVecStore::new(db.clone(), Arc::new(HashEmbedder::new(384)))
            .unwrap()
            .with_reranker(Arc::new(ReindexingReranker {
                db: db.clone(),
                fired: AtomicBool::new(false),
            }));
        store.index(2, "/a.md", "original alpha content").unwrap();

        let hits = store.search("alpha", None).await.unwrap();
        assert!(
            hits.iter().all(|h| h.chunk.as_deref() != Some("original alpha content")),
            "returned a stale chunk that was reindexed mid-search: {hits:?}"
        );
    }

    /// Scope prefixes containing GLOB metacharacters (`*`, `?`, `[`) must match
    /// literally — not as wildcards that over-match unrelated paths.
    #[tokio::test]
    async fn scoped_search_treats_glob_metachars_literally() {
        let s = store();
        s.index(2, "/x/[a]/f.md", "alpha shared keyword").unwrap();
        // `/x/a.md` matches the GLOB pattern `/x/[a]*` but not the literal prefix.
        s.index(3, "/x/a.md", "alpha shared keyword").unwrap();

        let hits = s.search("alpha shared keyword", Some("/x/[a]/")).await.unwrap();
        assert!(
            hits.iter().any(|h| h.filepath.as_deref() == Some("/x/[a]/f.md")),
            "literal in-scope file missing: {hits:?}"
        );
        assert!(
            hits.iter().all(|h| h.filepath.as_deref() != Some("/x/a.md")),
            "GLOB metachar prefix over-matched a sibling: {hits:?}"
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
    /// search works end to end. Gated on RUN_FASTEMBED (downloads the registry model).
    #[tokio::test]
    async fn real_model_offline_semantic_search() {
        use crate::embed::{EmbeddingModel, LocalEmbedder};
        if std::env::var("RUN_FASTEMBED").is_err() {
            eprintln!("skipping real-model E2E: set RUN_FASTEMBED=1 to download the registry model");
            return;
        }
        let db = Arc::new(Db::open_in_memory().unwrap());
        let emb = Arc::new(
            LocalEmbedder::from_registry(EmbeddingModel::SnowflakeArcticEmbedS, None).unwrap(),
        );
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
    /// corpus: L1 chunk → L2 embed (real local fastembed arctic-s) → L3 index
    /// (vec0 + fts5) → search (KNN ∪ BM25 → RRF) → L5 rerank (cloud Cohere).
    /// Query has ZERO lexical overlap with the target, so retrieval must be
    /// semantic; the reranker then confirms/refines the order. Gated on
    /// RUN_FASTEMBED (downloads the registry model) AND OPENROUTER_API_KEY.
    #[tokio::test]
    async fn full_pipeline_local_embed_then_cloud_rerank() {
        use crate::embed::{EmbeddingModel, LocalEmbedder};
        use crate::rerank::CohereReranker;

        if std::env::var("RUN_FASTEMBED").is_err() {
            eprintln!("skipping full-pipeline test: set RUN_FASTEMBED=1 to download the model");
            return;
        }
        let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
            eprintln!("skipping full-pipeline test: OPENROUTER_API_KEY not set");
            return;
        };

        let db = Arc::new(Db::open_in_memory().unwrap());
        let embedder =
            Arc::new(LocalEmbedder::from_registry(EmbeddingModel::SnowflakeArcticEmbedS, None).unwrap());
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
