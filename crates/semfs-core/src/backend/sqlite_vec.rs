//! Local hybrid index (Phase 4): `SqliteVecStore`.
//!
//! Implements [`SemanticIndex`] over the existing SQLite cache extended with
//! sqlite-vec (`vchunks`) + fts5 (`ffts`). `index` chunks → embeds → writes all
//! three tables in one transaction; `search` fuses vec0 KNN and BM25 with
//! Reciprocal Rank Fusion. Ports `bash/src/backends/sqlite-vec.ts`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::{SearchHit, SemanticIndex};
use crate::cache::Db;
use crate::embed::Embedder;
use crate::rerank::Reranker;

/// Over-fetch per ranked list before collapsing chunks → files.
const SEARCH_POOL: usize = 80;

/// Cap on how many RRF candidates feed the L5 cross-encoder rerank. A rerank
/// pass is O(candidates) and was a ~350% CPU hog under load; the answer is
/// virtually always in the top RRF candidates, so rerank only the head and let
/// the long tail keep its RRF order. Bounds per-query rerank cost. (ticket
/// search-deadline-fails-closed-to-empty, fix #2.)
const RERANK_CANDIDATES: usize = 50;

/// Cooperative deadline for a single SQLite search. The whole search runs inside
/// `spawn_blocking`, which Tokio CANNOT cancel — so the daemon's outer
/// `tokio::time::timeout` only stops WAITING on the result, it can't abort the
/// blocking work. At its cancellation points the search may only ever SHED WORK,
/// never zero a result that matched: past the deadline it still returns the
/// retrieved RRF hits but SKIPS the expensive cross-encoder rerank (the stage
/// worth guarding the shared `Mutex<Connection>` against). Kept STRICTLY UNDER
/// `daemon::ipc::SEARCH_TIMEOUT` (50s) — by a margin — so the in-search
/// cooperative degrade (return RRF hits, skip rerank) reliably wins the race
/// against the daemon's outer hard timeout, which would otherwise cut the search
/// off with nothing. (RCA 2026-06-04-semfs-grep-hangs-post-search-under-load #3.)
const SEARCH_DEADLINE: Duration = Duration::from_secs(20);
/// When a query is scoped to a path prefix, vec0 KNN can't filter on the joined
/// `filepath` (it only post-filters the global k-nearest), so we raise `k` to
/// this bound and GLOB-filter, ensuring in-scope hits aren't crowded out of the
/// pool by out-of-scope files. 4096 is sqlite-vec's hard `k` ceiling; beyond
/// that the exact-GLOB FTS lane still surfaces in-scope lexical matches.
const SCOPED_KNN_POOL: usize = 4096;

/// Knob B — how many ranked files we RETURN (Supermemory's `/v4/search` returns
/// ~10 by default). The rerank POOL upstream stays at `RERANK_CANDIDATES`; this
/// caps only the handed-back set so the agent isn't flooded with thin hits, and
/// so we reconstruct whole-doc text for just the top-N. Override `SEMFS_RESULT_LIMIT`.
const RESULT_LIMIT: usize = 10;

/// Knob B — per-document byte ceiling on the whole-document text we attach per
/// returned hit. We RANK on the matched chunk but RETURN the whole document
/// (reconstructed from `chunks` — the raw file on the mount is binary for
/// Office/PDF, so its text exists only here). Bounds the IPC payload: 10 docs ×
/// this is ~Supermemory's footprint. The full file is always still on the mount.
const DOC_RETURN_CAP: usize = 64 * 1024;

/// `SEMFS_RESULT_LIMIT` override → how many hits to return (falls back to
/// `RESULT_LIMIT`). A non-positive / unparsable value is ignored.
fn result_limit() -> usize {
    std::env::var("SEMFS_RESULT_LIMIT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(RESULT_LIMIT)
}

/// `SEMFS_DOC_RETURN_CAP` override → per-document byte ceiling on whole-doc text
/// attached per hit (falls back to `DOC_RETURN_CAP`). Lowering it cuts the grep
/// payload an agent re-replays in context — a token lever with no re-seed.
fn doc_return_cap() -> usize {
    std::env::var("SEMFS_DOC_RETURN_CAP")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DOC_RETURN_CAP)
}

/// `SEMFS_RETURN_MODE=snippet` (or `chunk`) → return ONLY the matched chunk(s)
/// per hit instead of the whole document. Cloud-style compact returns: on a
/// corpus of LARGE docs the whole-doc payload floods the agent's multi-turn
/// context (the dominant token sink), so returning just the reranker's matched
/// chunk (already computed, on the hit) cuts payload by ~doc/chunk. The full
/// file is always still on the mount if the agent needs more context.
fn snippet_return_mode() -> bool {
    std::env::var("SEMFS_RETURN_MODE")
        .map(|v| matches!(v.as_str(), "snippet" | "chunk"))
        .unwrap_or(false)
}

/// True if `path` is the agent's own output directory (`model_output/`) — where
/// the agent stages its deliverable. Such files are never SOURCES, so search
/// excludes them (a prior run's fabricated output must not be retrieved as data).
fn is_agent_output_path(path: &str) -> bool {
    let p = path.trim_start_matches('/');
    p == "model_output" || p.starts_with("model_output/")
}

/// A post-rerank ranking stage (`SEMFS_SALIENCE` / `SEMFS_COMENTION`) is ENABLED
/// unless its env var is explicitly set to an off value. Lets us A/B the L6/L7
/// boosts off for deterministic, pure-rerank ordering.
fn rank_stage_enabled(var: &str) -> bool {
    !std::env::var(var)
        .map(|v| matches!(v.as_str(), "0" | "off" | "false" | "no"))
        .unwrap_or(false)
}

/// Largest index `<= max` that is a UTF-8 char boundary, so `&s[..n]` stays
/// valid. Critical for the Chinese corpus (3-byte chars) — a mid-char cut panics.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Stitch a file's ordered chunks back into its (capped) document text. Chunks
/// are verbatim, contiguous windows with a fixed word overlap, so consecutive
/// chunks share a suffix/prefix span; we drop the largest such overlap to avoid
/// duplicating it. Result is capped to `DOC_RETURN_CAP` on a char boundary.
fn stitch_chunks(parts: &[String]) -> String {
    let mut out = String::new();
    for part in parts {
        if out.is_empty() {
            out.push_str(part);
            continue;
        }
        // Largest k (on char boundaries) where the tail of `out` equals the head
        // of `part` — the verbatim chunk overlap. Bounded so cost stays linear-ish.
        let max = out.len().min(part.len()).min(8192);
        let mut overlap = 0;
        let mut k = max;
        while k > 0 {
            if part.is_char_boundary(k)
                && out.is_char_boundary(out.len() - k)
                && out[out.len() - k..] == part[..k]
            {
                overlap = k;
                break;
            }
            k -= 1;
        }
        out.push_str(&part[overlap..]);
        if out.len() >= doc_return_cap() {
            break;
        }
    }
    out.truncate(floor_char_boundary(&out, doc_return_cap()));
    out
}

/// Local, offline semantic index over the SQLite cache.
///
/// `Clone` is cheap (every field is an `Arc`) and shares the SAME underlying
/// `Db`/connection — used to move a handle into `spawn_blocking` for the search
/// path without borrowing `self`.
#[derive(Debug, Clone)]
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
    /// Optional L7 graph extractor (LLM). When present, the background graph
    /// worker extracts typed entities and writes file→entity edges. `None` = no
    /// graph.
    graph_llm: Option<Arc<crate::llm::LlmClient>>,
    /// Pending L7-extraction queue, present iff `graph_llm` is. `index()` enqueues
    /// a file here after writing its vectors; `run_graph_worker` drains it. Keeps
    /// the blocking per-file LLM call OFF the synchronous index/flush path.
    graph_queue: Option<Arc<crate::cache::GraphQueue>>,
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
        let stored = db.embed_identity();
        match &stored {
            Some(s) if *s != identity => anyhow::bail!(
                "existing index was built with text embedder '{s}' but the current embedder is \
                 '{identity}'; rebuild the index (fresh cache) or restore the previous model"
            ),
            Some(_) => {
                // Matching stamp: the vec tables must already be CONSISTENT with
                // `chunks`. `ensure_vector_tables` below would silently recreate a
                // missing vec table EMPTY (CREATE IF NOT EXISTS), stranding existing
                // chunks vectorless on a partial restore/corruption. Validate the
                // count invariant BEFORE that mutation and refuse if vectors are
                // missing — require a rebuild rather than a silent empty repair.
                let conn = db.conn.lock();
                let count = |sql: &str| -> i64 {
                    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
                };
                let has = |name: &str| -> bool {
                    conn.query_row(
                        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                        [name],
                        |r| r.get::<_, i64>(0),
                    )
                    .map(|n| n > 0)
                    .unwrap_or(false)
                };
                let chunk_n = count("SELECT count(*) FROM chunks");
                let text_n = if has("vchunks") { count("SELECT count(*) FROM vchunks") } else { 0 };
                let code_n = if has("vchunks_code") {
                    count("SELECT count(*) FROM vchunks_code")
                } else {
                    0
                };
                if chunk_n < 0 || text_n < 0 || code_n < 0 || chunk_n != text_n + code_n {
                    anyhow::bail!(
                        "stamped cache has {chunk_n} chunks but {text_n}+{code_n} vectors \
                         (vec table missing/undercounted — corruption); rebuild the index"
                    );
                }
                drop(conn);
                // The identity stamp (matched above) encodes the true width, so the
                // separate `text_embed_dims` metadata MUST be present AND agree. A
                // corrupt OR MISSING dims row (partial restore) would let
                // `ensure_vector_tables` leave a wrong-width / drop the populated
                // `vchunks` — refuse instead of trusting (or recreating from) it.
                match db.text_embed_dims() {
                    Some(d) if d == embedder.dimensions() => {}
                    other => anyhow::bail!(
                        "stamped cache has text_embed_dims={other:?} but the embedder width is {} \
                         (corrupt/missing dims metadata); rebuild the index",
                        embedder.dimensions()
                    ),
                }
            }
            None => {
                // No stamp is only safe on a PROVABLY brand-new text lane. Existing
                // `chunks` rows mean vectors were indexed under an unknown model
                // (a legacy or partially-recovered cache); adopting it would let
                // `index()` mix spaces or `ensure_vector_tables` drop/recreate on a
                // width change. Refuse — require a fresh reindex.
                let has_rows = db
                    .conn
                    .lock()
                    .query_row("SELECT EXISTS(SELECT 1 FROM chunks)", [], |r| r.get::<_, i64>(0))
                    .map(|n| n == 1)
                    .unwrap_or(false);
                if has_rows {
                    anyhow::bail!(
                        "existing index has chunks but no recorded text embedder identity \
                         (legacy/corrupt cache); rebuild the index (fresh cache)"
                    );
                }
            }
        }
        // Guard the CODE lane's metadata before the schema mutation below. If the
        // code lane is stamped but its dims row was lost (partial restore), the
        // `existing_code_dims = None` we'd pass to `ensure_vector_tables` would
        // make it DROP the populated `vchunks_code` ("code embedder removed").
        // Refuse on that inconsistency instead of silently destroying the lane.
        if db.code_embed_identity().is_some() && db.code_embed_dims().is_none() {
            anyhow::bail!(
                "code lane is stamped but code_embed_dims is missing (corrupt metadata); \
                 rebuild the index"
            );
        }
        // PRESERVE any existing code lane: passing `None` here would make
        // `ensure_vector_tables` drop `vchunks_code` ("code embedder removed").
        // Carry the stored code width through; `enable_code_indexing` rebuilds it
        // at the real width afterward (a no-op when unchanged).
        let existing_code_dims = db.code_embed_dims();
        db.ensure_vector_tables(embedder.dimensions(), existing_code_dims)?;
        // Stamp the text identity on first creation (brand-new lane, verified above).
        if stored.is_none() {
            db.record_embed_identity(&identity)?;
        }
        Ok(Self {
            db,
            embedder,
            code_embedder: None,
            reranker: None,
            graph_llm: None,
            graph_queue: None,
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
        let stored = self.db.code_embed_identity();
        match &stored {
            Some(s) if *s != code.identity() => anyhow::bail!(
                "existing code lane was built with '{s}' but the current code model is '{}'; \
                 code lane disabled until a fresh reindex",
                code.identity()
            ),
            Some(_) => {
                // Matching code stamp: the stored code width MUST be present AND
                // agree with the embedder (the stamp encodes it). A corrupt OR
                // MISSING `code_embed_dims` would make ensure_vector_tables drop /
                // leave a wrong-width `vchunks_code` — refuse instead.
                match self.db.code_embed_dims() {
                    Some(d) if d == code.dimensions() => {}
                    other => anyhow::bail!(
                        "code lane has code_embed_dims={other:?} but the code embedder width is \
                         {} (corrupt/missing dims metadata); rebuild the index",
                        code.dimensions()
                    ),
                }
            }
            None => {
                // No code stamp is only safe on a brand-new code lane. Existing
                // `vchunks_code` rows without a stamp = legacy/corrupt → refuse
                // (don't adopt + mix). The daemon fails open (text lane only).
                let has_rows = {
                    let conn = self.db.conn.lock();
                    let table = conn
                        .query_row(
                            "SELECT count(*) FROM sqlite_master WHERE type='table' AND \
                             name='vchunks_code'",
                            [],
                            |r| r.get::<_, i64>(0),
                        )
                        .map(|n| n > 0)
                        .unwrap_or(false);
                    table
                        && conn
                            .query_row("SELECT EXISTS(SELECT 1 FROM vchunks_code)", [], |r| {
                                r.get::<_, i64>(0)
                            })
                            .map(|n| n == 1)
                            .unwrap_or(false)
                };
                if has_rows {
                    anyhow::bail!(
                        "existing code lane has vectors but no recorded code embedder identity \
                         (legacy/corrupt); rebuild the index"
                    );
                }
            }
        }
        self.db
            .ensure_vector_tables(self.embedder.dimensions(), Some(code.dimensions()))?;
        if stored.is_none() {
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
            graph_queue: None,
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
        // (4) Any POPULATED code lane that we cannot search forces cloud fallback —
        // even on a mixed cache. If `vchunks_code` has rows but the code lane is
        // not active (no/mismatched/unstamped code embedder), serving text-only
        // would silently drop code recall (false negatives for code-heavy queries).
        // Fail closed so the cloud — which has everything — answers instead.
        let code_table = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='vchunks_code'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        let code_rows = code_table
            && conn
                .query_row("SELECT EXISTS(SELECT 1 FROM vchunks_code)", [], |r| r.get::<_, i64>(0))
                .map(|n| n == 1)
                .unwrap_or(false);
        if code_rows && !code_active {
            return false;
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
        self.graph_queue = Some(crate::cache::GraphQueue::new());
        self
    }

    /// Index a file: chunk → embed → write `chunks`/`ffts`/`vchunks` atomically.
    /// Re-indexing the same `filepath` replaces its prior chunks (and their
    /// rowid-linked vec0/fts rows). Removing a file = `index` with empty content.
    pub fn index(&self, ino: u64, filepath: &str, content: &str) -> anyhow::Result<()> {
        let path_is_code = is_code_path(filepath);
        // If the cache ADVERTISES a code lane but this writer has no active code
        // embedder (fail-open state: code-model init failed, or a model-mismatch
        // bail), DO NOT index a code-like file into the text lane — that would
        // strand its vectors in the wrong space permanently. Drop any prior entry
        // (so we don't keep stale/wrong-lane vectors) and skip; the file is
        // re-indexed correctly once a valid code embedder is available.
        if path_is_code && self.code_embedder.is_none() && self.db.code_embed_identity().is_some() {
            let mut conn = self.db.conn.lock();
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            drop_file_chunks(&tx, filepath)?;
            tx.commit()?;
            tracing::warn!(
                "code lane advertised but no active code embedder; skipping code file {filepath} \
                 (re-index when the code model is available)"
            );
            return Ok(());
        }

        // Bound content BEFORE chunking so one large file (text, code, or
        // extracted) can't produce unbounded chunks → embed grind that stalls
        // the whole import (see chunk::MAX_INDEX_BYTES). Source-independent: this
        // guards the UTF-8 text path too, not just extraction.
        let full_len = content.len();
        let content = super::chunk::cap_index_content(content);
        if content.len() < full_len {
            tracing::warn!(
                filepath,
                full_len,
                capped = content.len(),
                "content exceeds index cap; indexing head only (partial)"
            );
        }
        let chunks = super::chunk::recursive_chunks(content, &super::chunk::ChunkOptions::default());
        // Route code-like files to the code embedder + `vchunks_code` lane when a
        // code embedder is attached; everything else uses the text lane.
        let use_code = self.code_embedder.is_some() && path_is_code;
        let embedder = match (&self.code_embedder, use_code) {
            (Some(code), true) => code.as_ref(),
            _ => self.embedder.as_ref(),
        };
        let vec_table = if use_code { "vchunks_code" } else { "vchunks" };
        let vectors = embedder.embed(&chunks)?;

        let mut conn = self.db.conn.lock();
        // IMMEDIATE: take the write lock at BEGIN. These transactions read
        // (e.g. drop_file_vectors SELECTs) before writing; a DEFERRED tx would
        // let two concurrent writers deadlock on the read→write upgrade in WAL
        // (instant SQLITE_BUSY, no busy_timeout retry).
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // Drop this file's prior chunks + their rowid-linked vec0/fts rows. NOT
        // its edges — L7 is now deferred (see graph_queue): edges persist until
        // the background worker re-derives them, so a re-index doesn't blank the
        // graph in the gap before extraction completes.
        drop_file_vectors(&tx, filepath)?;

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

        tx.commit()?;
        drop(conn);

        // L7 is DEFERRED: enqueue this file for background entity extraction so
        // the blocking per-file LLM call never sits on the synchronous write
        // path (which the FUSE single dispatch thread serializes). The worker
        // (run_graph_worker) drains the queue with bounded concurrency.
        if let Some(q) = &self.graph_queue {
            q.enqueue(ino, filepath.to_string());
        }
        Ok(())
    }

    /// L7 (deferred): extract entities for one already-indexed file and write its
    /// `edges`. Reads the file's content from the local cache, runs the blocking
    /// LLM extraction on the blocking pool (so many can overlap), then replaces
    /// the file's edge rows. Fail-open: a missing graph only weakens the ±5%
    /// co-mention boost, never recall. No-op without a graph extractor.
    pub async fn index_graph(&self, ino: u64, filepath: &str) -> anyhow::Result<()> {
        let Some(llm) = self.graph_llm.clone() else {
            return Ok(());
        };
        let db = self.db.clone();
        let fp = filepath.to_string();
        // The whole unit (content read → blocking LLM → edge write) is sync, so
        // run it on the blocking pool; the worker's semaphore bounds concurrency.
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let raw = db.read_all_content(ino);
            let Ok(content) = String::from_utf8(raw) else {
                return Ok(()); // binary/non-UTF8 — nothing to extract
            };
            let entities = match super::graph::extract_entities(&llm, &content) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(filepath = %fp, "entity extraction failed ({e}); no edges");
                    return Ok(());
                }
            };
            let now = now_ms();
            let mut conn = db.conn.lock();
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // Replace this file's edges (idempotent re-derive).
            drop_file_edges(&tx, &fp)?;
            for ent in &entities {
                let node = super::graph::entity_path(&ent.name);
                tx.execute(
                    "INSERT OR IGNORE INTO edges(from_path, to_path, edge_kind, created_at, confidence) \
                     VALUES (?1, ?2, ?3, ?4, 'INFERRED')",
                    rusqlite::params![fp, node, ent.kind, now],
                )?;
                // Preserve the original (CJK-safe) entity name for KG god-node
                // labels; the slug in `node` is lossy. Last writer wins on name.
                tx.execute(
                    "INSERT INTO graph_entity(path, name, kind) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(path) DO UPDATE SET name=excluded.name, kind=excluded.kind",
                    rusqlite::params![node, ent.name, ent.kind],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?
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
/// Does NOT touch `edges` — L7 is maintained separately (see `drop_file_edges`),
/// so a vector re-index doesn't transiently blank the graph.
fn drop_file_vectors(tx: &rusqlite::Transaction, filepath: &str) -> rusqlite::Result<()> {
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
    Ok(())
}

/// Delete a file's outbound L7 graph edges within a txn (re-derived on the next
/// deferred extraction, gone on delete).
fn drop_file_edges(tx: &rusqlite::Transaction, filepath: &str) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM edges WHERE from_path = ?1", [filepath])?;
    Ok(())
}

/// Delete a file's vectors AND its edges — full removal (delete / rename clear).
fn drop_file_chunks(tx: &rusqlite::Transaction, filepath: &str) -> rusqlite::Result<()> {
    drop_file_vectors(tx, filepath)?;
    drop_file_edges(tx, filepath)?;
    Ok(())
}

/// Bridge to the cache write path: lets `CacheFs`/`SqliteFile` maintain the
/// index on writes/deletes without a module cycle. The trait is async (so an
/// async backend can implement it); SqliteVecStore's work is sync (rusqlite +
/// fastembed), so each method just calls the sync inherent method — same
/// behaviour as before the trait went async.
#[async_trait::async_trait]
impl crate::cache::LocalIndexer for SqliteVecStore {
    async fn index(&self, ino: u64, filepath: &str, content: &str) -> anyhow::Result<()> {
        SqliteVecStore::index(self, ino, filepath, content)
    }
    async fn remove(&self, filepath: &str) -> anyhow::Result<()> {
        SqliteVecStore::remove(self, filepath)
    }
    async fn rename(&self, old: &str, new: &str) -> anyhow::Result<()> {
        SqliteVecStore::rename(self, old, new)
    }
    fn graph_queue(&self) -> Option<Arc<crate::cache::GraphQueue>> {
        self.graph_queue.clone()
    }
    async fn index_graph(&self, ino: u64, filepath: &str) -> anyhow::Result<()> {
        SqliteVecStore::index_graph(self, ino, filepath).await
    }
}

#[async_trait]
impl SemanticIndex for SqliteVecStore {
    async fn search(
        &self,
        query: &str,
        filepath: Option<&str>,
    ) -> anyhow::Result<Vec<SearchHit>> {
        // The whole search is synchronous: rusqlite (vec0/fts5) plus a blocking
        // embed and rerank (cloud = blocking HTTP). Run it on a blocking thread so
        // the daemon's `tokio::time::timeout` around the IPC search can actually
        // preempt a stalled cloud embed/rerank before the client deadline — and so
        // the !Send rusqlite guards never cross an await. Cloning the store is
        // cheap (Arcs) and shares the same connection.
        let store = self.clone();
        let query = query.to_string();
        let filepath = filepath.map(|s| s.to_string());
        // Compute the cooperative deadline here (passed into the blocking body so
        // it is also injectable in tests).
        let deadline = Instant::now() + SEARCH_DEADLINE;
        tokio::task::spawn_blocking(move || {
            store.search_blocking(&query, filepath.as_deref(), deadline)
        })
        .await
        .map_err(|e| anyhow::anyhow!("sqlite search task failed: {e}"))?
    }
}

impl SqliteVecStore {
    /// The fully-synchronous search body. Called from the `SemanticIndex::search`
    /// async wrapper inside `spawn_blocking` (above).
    fn search_blocking(
        &self,
        query: &str,
        filepath: Option<&str>,
        deadline: Instant,
    ) -> anyhow::Result<Vec<SearchHit>> {
        // Cooperative cancellation: the daemon's outer timeout can't abort this
        // blocking task, so at the deadline we shed the EXPENSIVE work (rerank) but
        // still return the candidates we already have — the deadline may only ever
        // reduce work, never zero a result that matched (see the bail vs. degrade
        // note below). `deadline` is passed in so it's injectable in tests.

        let qvec = self
            .embedder
            .embed(&[query.to_string()])?
            .pop()
            .unwrap_or_default();
        let qvec_len = qvec.len();
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
        let mut by_file: HashMap<String, super::rank::FileAcc> = HashMap::new();
        // Per-stage candidate counters for L1→L7 search-pipeline observability:
        // when a search returns empty we want to see EXACTLY which stage zeroed it
        // (embed / vec / code / fts / rerank / phase-2 revalidation). See RCA
        // 2026-06-04-semfs-codex-clean-seed-timeout-poor-local-search-recall.
        let (mut vec_n, mut code_n, mut fts_n) = (0usize, 0usize, 0usize);
        // filepath -> the representative chunk's row id, captured at retrieval.
        // Used in phase 2 to detect a concurrent same-path reindex (which assigns
        // new ids), so we never return a snippet/score from pre-rewrite content.
        let mut rep_chunk: HashMap<String, i64> = HashMap::new();

        // Deadline degradation (NOT a bail): if the query-embed already blew the
        // deadline under CPU starvation, we still hold a usable query vector, and
        // retrieval (vec/code/fts KNN) is cheap bounded SQLite — far short of the
        // multi-second rerank the deadline really guards against. Proceeding and
        // returning best-effort RRF hits is strictly better than failing CLOSED to
        // empty: an agent that sees "0 results" for a query that DOES match
        // abandons semantic search entirely. The expensive rerank below is the
        // stage actually skipped past the deadline — mirroring its own degrade
        // path. (RCA 2026-06-04-…-search-deadline-fails-closed-to-empty.)
        if Instant::now() >= deadline {
            tracing::warn!(
                "sqlite search exceeded its {}s deadline during query-embed; \
                 returning best-effort RRF hits (rerank will be skipped) rather \
                 than failing closed to empty",
                SEARCH_DEADLINE.as_secs()
            );
        }
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
                super::rank::rrf_bump(&mut by_file, fp, text, rank, super::rank::Lane::Text);
                vec_n += 1;
            }
        }

        // Code vector KNN (vchunks_code) — only when the code lane is ACTIVE
        // (code_qblob is Some). Unlike the fail-soft FTS lane, errors here
        // PROPAGATE: the reader committed to a searchable code lane via
        // is_searchable(), so a runtime vec0 failure (e.g. vchunks_code dropped/
        // corrupted concurrently after the probe) must surface so the caller
        // (grep) falls back to cloud rather than silently serve text-only results.
        if let Some(cqblob) = &code_qblob {
            let k = if scope.is_some() { SCOPED_KNN_POOL } else { SEARCH_POOL };
            let mut stmt = conn.prepare(
                "SELECT c.id, c.filepath, c.text FROM vchunks_code v \
                 JOIN chunks c ON c.id = v.rowid \
                 WHERE v.embedding MATCH ?1 AND k = ?2 \
                 AND (?3 IS NULL OR instr(c.filepath, ?3) = 1) ORDER BY distance",
            )?;
            let rows = stmt.query_map(rusqlite::params![cqblob, k as i64, scope], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?;
            for (rank, row) in rows.enumerate() {
                let (id, fp, text) = row?;
                rep_chunk.entry(fp.clone()).or_insert(id);
                super::rank::rrf_bump(&mut by_file, fp, text, rank, super::rank::Lane::Code);
                code_n += 1;
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
                            super::rank::rrf_bump(&mut by_file, fp, text, rank, super::rank::Lane::Fts);
                            fts_n += 1;
                        }
                    }
                }
            }
        }

        // Path-token lane: rank files whose PATH matches the query's content
        // tokens. Agents query terms that name the file ("best-selling product
        // data" → best_selling_product_core_data_list.txt); content-only ranking
        // can bury it, so grep misses and the agent crawls. This lane votes the
        // clearly-named file into the pool so grep returns it #1. Disable with
        // SEMFS_PATH_LANE=off. (case-289 token lever; tickets/ls-kg-semantic-readdir.)
        // `path_pinned` holds the strongest filename match(es); they are forced into
        // the returned top-N below so the cross-encoder reranker can't demote a
        // correctly-named file on the basis of its content (e.g. an error-page
        // "source" the task needs the agent to find and report).
        let mut path_pinned: Vec<String> = Vec::new();
        // Error-page sources get pinned ABOVE regular filename matches: a broken
        // source is the highest-value signal for an error-detection task, and a
        // tight RESULT_LIMIT must not let a valid-looking look-alike crowd it out.
        let mut error_pinned: Vec<String> = Vec::new();
        if !matches!(std::env::var("SEMFS_PATH_LANE").ok().as_deref(), Some("off")) {
            let toks: Vec<String> = query
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| t.chars().count() >= 3)
                .map(|t| t.to_string())
                .collect();
            if !toks.is_empty() {
                // first chunk per file = representative text + id for rerank input
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT c.id, c.filepath, c.text FROM chunks c \
                     JOIN (SELECT filepath, MIN(id) mid FROM chunks GROUP BY filepath) g \
                       ON c.id = g.mid \
                     WHERE (?1 IS NULL OR instr(c.filepath, ?1) = 1)",
                ) {
                    if let Ok(rows) = stmt.query_map(rusqlite::params![scope], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                    }) {
                        let mut scored: Vec<(usize, i64, String, String)> = Vec::new();
                        for row in rows.flatten() {
                            let (id, fp, text) = row;
                            // normalize separators so path tokens become words
                            let norm: String = fp
                                .to_lowercase()
                                .chars()
                                .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                                .collect();
                            let hits = toks
                                .iter()
                                .filter(|t| norm.split(' ').any(|w| w == t.as_str()))
                                .count();
                            // require ≥2 matching path tokens so generic words
                            // ("data", "report") alone don't pull in noise
                            if hits >= 2 {
                                scored.push((hits, id, fp, text));
                            }
                        }
                        // most path-token matches first → best path-lane rank
                        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.2.cmp(&b.2)));
                        // Pin the files with the most matching path tokens (the
                        // near-exact filename matches), capped so a broad query
                        // can't pin the whole pool. These survive reranking below.
                        let max_hits = scored.first().map(|(h, ..)| *h).unwrap_or(0);
                        if max_hits >= 2 {
                            path_pinned = scored
                                .iter()
                                .filter(|(h, ..)| *h == max_hits)
                                .take(2)
                                .map(|(_, _, fp, _)| fp.clone())
                                .collect();
                        }
                        for (rank, (_hits, id, fp, text)) in
                            scored.into_iter().take(SEARCH_POOL).enumerate()
                        {
                            rep_chunk.entry(fp.clone()).or_insert(id);
                            super::rank::rrf_bump(
                                &mut by_file,
                                fp,
                                text,
                                rank,
                                super::rank::Lane::Path,
                            );
                        }
                    }
                }
            }
        }

        // H1 integrity lane: GUARANTEE a corrupt/error-page SOURCE that matches the
        // query surfaces, so the agent reports it instead of copying a valid-looking
        // look-alike. Error pages are few but the path-lane's SEARCH_POOL cap can
        // crowd them out for broad queries (e.g. "best-selling product data" matches
        // dozens of files on "product"/"data"). We find error pages directly and
        // add any sharing >=1 query token with their path to the pool at top rank;
        // the annotation+pin pass below then labels them SOURCE INACCESSIBLE and
        // keeps them past the reranker. Disable with SEMFS_INTEGRITY_LANE=off.
        if !matches!(
            std::env::var("SEMFS_INTEGRITY_LANE").ok().as_deref(),
            Some("off")
        ) {
            let toks: Vec<String> = query
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| t.chars().count() >= 3)
                .map(|t| t.to_string())
                .collect();
            if !toks.is_empty() {
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT c.id, c.filepath, c.text FROM chunks c \
                     JOIN (SELECT filepath, MIN(id) mid FROM chunks GROUP BY filepath) g \
                       ON c.id = g.mid \
                     WHERE length(c.text) < 2048 \
                       AND (instr(lower(c.text),'403 forbidden')>0 \
                            OR instr(lower(c.text),'404 not found')>0 \
                            OR instr(lower(c.text),'openresty')>0 \
                            OR instr(lower(c.text),'502 bad gateway')>0) \
                       AND (?1 IS NULL OR instr(c.filepath, ?1) = 1)",
                ) {
                    if let Ok(rows) = stmt.query_map(rusqlite::params![scope], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                    }) {
                        for (id, fp, text) in rows.flatten() {
                            if !text.trim_start().to_ascii_lowercase().starts_with("<html") {
                                continue;
                            }
                            let norm: String = fp
                                .to_lowercase()
                                .chars()
                                .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                                .collect();
                            let overlap = toks
                                .iter()
                                .filter(|t| norm.split(' ').any(|w| w == t.as_str()))
                                .count();
                            if overlap >= 1 {
                                rep_chunk.entry(fp.clone()).or_insert(id);
                                super::rank::rrf_bump(
                                    &mut by_file,
                                    fp,
                                    text,
                                    0,
                                    super::rank::Lane::Path,
                                );
                            }
                        }
                    }
                }
            }
        }
        drop(conn);

        // The KG digest is an orientation artifact, not corpus content — never
        // return it as a search hit (it would otherwise self-match queries about
        // the workspace and waste a result slot).
        by_file.remove("/KNOWLEDGE_GRAPH.md");

        // Saved HTTP-error pages (e.g. a `.xlsx` that is really a 321-byte
        // "403 Forbidden" openresty page — case-289 ships these as the actual
        // "source" data files). Earlier we DROPPED these to avoid the agent
        // opening them and hitting the binary-parse format-trap. That was wrong:
        // dropping hid the one fact a task may need to *report* — that the source
        // is inaccessible. Instead, SURFACE the file with a clear annotation so
        // the agent reports the error rather than fabricating or substituting
        // data, and knows not to parse it. Detected by the representative chunk
        // being a short HTML error page.
        for (fp, acc) in by_file.iter_mut() {
            let rep = acc
                .chunks
                .iter()
                .min_by_key(|(r, _)| *r)
                .map(|(_, t)| t.as_str())
                .unwrap_or("");
            let low = rep.trim_start().to_ascii_lowercase();
            let is_error_page = low.starts_with("<html")
                && rep.len() < 2048
                && (low.contains("403 forbidden")
                    || low.contains("404 not found")
                    || low.contains("openresty")
                    || low.contains("502 bad gateway"));
            if is_error_page {
                // Mark it as an inaccessible source. NOTE: error pages are NOT
                // forced above real data (that would bury the answer on normal
                // tasks); they only get a RESERVED slot in the returned set below
                // (`error_pinned`), so a corrupt source stays visible without
                // outranking a legitimate data file.
                if !error_pinned.contains(fp) {
                    error_pinned.push(fp.clone());
                }
                let status = if low.contains("403 forbidden") {
                    "403 Forbidden"
                } else if low.contains("404 not found") {
                    "404 Not Found"
                } else if low.contains("502 bad gateway") {
                    "502 Bad Gateway"
                } else {
                    "HTTP error"
                };
                let fname = fp.rsplit('/').next().unwrap_or(fp.as_str());
                let ext = fname.rsplit('.').next().filter(|e| *e != fname).unwrap_or("");
                let label = if ext.is_empty() {
                    String::new()
                } else {
                    format!(" (labeled .{ext} but is HTML, not that format)")
                };
                // Generic, FACTUAL metadata about the file: it is an HTTP error page,
                // the format does not match its extension, and the data is unreadable.
                // semfs states the facts; the agent decides how to report them. No
                // copy-verbatim, no rubric-tuned wording — these terms are just true.
                let note = format!(
                    "[semfs: SOURCE INACCESSIBLE — {fname} is an HTTP {status} error page in \
                     HTML format{label}; the underlying data is inaccessible (cannot be read). \
                     Do not parse it as its extension implies, and do not substitute another \
                     file's data for it.]"
                );
                acc.chunks = vec![(0, note)];
            }
        }

        let mut hits = super::rank::to_hits(by_file, filepath);
        let hits_after_rrf = hits.len();
        let mut reranked = false;

        // SEMFS_DEBUG_RANKING: dump the FULL RRF-sorted candidate pool (pre-truncate).
        // RRF is document-aggregated (by_file), so this IS the "whole document"
        // ranking — lets us see a target file's RRF rank and whether it falls
        // outside the rerank window (RERANK_CANDIDATES). See
        // tickets/local-ranking-precision-vs-supermemory.
        if std::env::var("SEMFS_DEBUG_RANKING").is_ok() {
            for (i, h) in hits.iter().enumerate() {
                tracing::info!(
                    stage = "RRF",
                    rank = i,
                    score = h.similarity,
                    fp = h.filepath.as_deref().unwrap_or(""),
                    "RANKDUMP"
                );
            }
        }

        // L5 rerank: replace RRF scores with cross-encoder scores, then re-sort.
        // Cancellation point: the reranker is synchronous (cloud = blocking HTTP).
        // If past the deadline, SKIP the expensive rerank — but DO NOT return here.
        // We must still fall through to phase 2, which revalidates every hit
        // against the current (chunk id, filepath) and drops ghosts from a
        // concurrent rename/remove/reindex; returning pre-revalidation hits would
        // surface stale/deleted content as authoritative. Phase 2 is fast local
        // SQL (no network), so re-taking the connection briefly is not the
        // multi-second monopolization the deadline guards against.
        if let Some(reranker) = &self.reranker {
            if Instant::now() >= deadline {
                tracing::warn!(
                    "sqlite search hit its {}s deadline before rerank; skipping rerank, \
                     returning RRF-ranked hits (still revalidated in phase 2)",
                    SEARCH_DEADLINE.as_secs()
                );
            } else {
                // Bound rerank CPU: only the top RRF candidates are reranked; the
                // tail keeps its RRF rank below them. Caps the cross-encoder cost
                // so one search can't peg the box.
                hits.truncate(RERANK_CANDIDATES);
                super::rank::apply_reranker(&mut hits, reranker.as_ref(), query)?;
                reranked = true;
            }
        }

        // SEMFS_DEBUG_RANKING: dump the post-rerank order (chunk-level cross-encoder
        // scores) to compare against the RRF/document order above — the whole-doc
        // vs chunk experiment.
        if reranked && std::env::var("SEMFS_DEBUG_RANKING").is_ok() {
            for (i, h) in hits.iter().enumerate().take(RERANK_CANDIDATES) {
                tracing::info!(
                    stage = "RERANK",
                    rank = i,
                    score = h.similarity,
                    fp = h.filepath.as_deref().unwrap_or(""),
                    "RANKDUMP"
                );
            }
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

            // L7 co-mention + L6 salience are post-rerank multiplicative nudges. Both
            // are now sign-correct (see rank.rs), but they remain STATEFUL (salience
            // reads access_count, bumped every search → run-to-run drift). Kill-switches
            // `SEMFS_COMENTION=off` / `SEMFS_SALIENCE=off` disable them for deterministic,
            // pure-rerank ordering (A/B the ranking-trust hypothesis).
            if rank_stage_enabled("SEMFS_COMENTION") {
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
            }
            if rank_stage_enabled("SEMFS_SALIENCE") {
                super::rank::apply_salience(&mut hits, now, |fp| {
                    conn.query_row(
                        "SELECT MAX(last_accessed_at), COALESCE(SUM(access_count), 0) \
                         FROM chunks WHERE filepath = ?1",
                        [fp],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap_or((None, 0))
                });
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
        // L1→L7 pipeline observability: one line shows where a search zeroed out.
        // qvec_len=0 → query embed failed; vec_n+code_n+fts_n=0 → retrieval found
        // nothing (recall gap); rrf_files>0 but final_hits=0 → phase-2 revalidation
        // dropped them. See RCA 2026-06-04-...poor-local-search-recall.
        let final_hits = hits.len();
        tracing::info!(
            query,
            qvec_len,
            vec_n,
            code_n,
            fts_n,
            rrf_files = hits_after_rrf,
            reranked,
            final_hits,
            scope = scope.as_deref().unwrap_or(""),
            "search pipeline counts (L1 retrieve → RRF → L5 rerank → phase-2)"
        );
        if final_hits == 0 {
            tracing::warn!(
                query,
                qvec_len,
                vec_n,
                code_n,
                fts_n,
                rrf_files = hits_after_rrf,
                "search returned ZERO hits — see per-stage counts to locate the drop"
            );
        }
        super::rank::sort_desc(&mut hits);

        // SEMFS_DEBUG_RANKING: dump the FINAL order (post L6 salience + L7 co-mention,
        // after the final sort) to compare against the RERANK order above — exposes
        // any post-rerank reordering (e.g. multiplicative salience/co-mention acting
        // on NEGATIVE cross-encoder scores).
        if std::env::var("SEMFS_DEBUG_RANKING").is_ok() {
            for (i, h) in hits.iter().enumerate().take(RERANK_CANDIDATES) {
                tracing::info!(
                    stage = "FINAL",
                    rank = i,
                    score = h.similarity,
                    fp = h.filepath.as_deref().unwrap_or(""),
                    "RANKDUMP"
                );
            }
        }

        // Pin strong filename matches to the front so a near-exact filename query
        // returns that file even when the cross-encoder reranked it down on content
        // (stable sort: pinned keep their order, everything else keeps its order).
        if !path_pinned.is_empty() {
            // Strong filename (data) matches move to the front so a near-exact
            // filename query returns that file even if reranked down on content.
            // Stable: pinned keep their order, the rest keep theirs. Error pages
            // are deliberately NOT included here — they must not outrank real data.
            hits.sort_by_key(|h| {
                !h.filepath
                    .as_deref()
                    .is_some_and(|p| path_pinned.iter().any(|e| e == p))
            });
        }
        // RESERVE one returned slot for a corrupt/error source when one matched the
        // query, WITHOUT displacing the top data result: if none is already inside
        // the returned window, move the best error-page hit into the LAST slot. This
        // keeps a broken source visible (so the agent can report it) while a
        // legitimate data file still ranks #1 — the safe fix to "errors ranked first".
        {
            let lim = result_limit();
            if lim >= 2 && !error_pinned.is_empty() {
                let in_window = hits
                    .iter()
                    .take(lim)
                    .any(|h| h.filepath.as_deref().is_some_and(|p| error_pinned.iter().any(|e| e == p)));
                if !in_window {
                    if let Some(pos) = hits
                        .iter()
                        .position(|h| h.filepath.as_deref().is_some_and(|p| error_pinned.iter().any(|e| e == p)))
                    {
                        if pos >= lim {
                            let h = hits.remove(pos);
                            hits.insert(lim - 1, h);
                        }
                    }
                }
            }
        }

        // Never return the agent's own output dir as a SOURCE: a prior run's
        // deliverable under `model_output/` may be fabricated, and retrieving it
        // lures the agent into copying it (case-289: stale fabricated list →
        // 207K tokens, dishonest 5/15). Drop before the top-N cap.
        hits.retain(|h| !h.filepath.as_deref().is_some_and(is_agent_output_path));

        // Knob B: cap to the returned top-N (Supermemory parity) BEFORE attaching
        // documents, so we reconstruct text for ~N files, not the whole pool.
        hits.truncate(result_limit());

        // Knob B: attach the WHOLE document per returned hit. We ranked on the
        // matched chunk; the agent now receives the full document (like Supermemory)
        // so it doesn't keep re-searching for context. For Office/PDF the file on
        // the mount is raw binary — the text exists ONLY in `chunks` — so we stitch
        // it from `chunks ORDER BY ord` and put it in `memory`, the SAME field the
        // cloud path fills, so grep renders local + cloud identically.
        if !hits.is_empty() {
            if snippet_return_mode() {
                // H1 trust-marker path: leave `memory` None so `grep` renders each hit
                // via the chunk presenter (line ranges + `# ^ COMPLETE FILE — …do not
                // open it`), matching the cloud backend. That "the excerpt IS the file,
                // don't re-open it" signal is what stops codex pandas/xlrd/zipfile-
                // parsing the file — the format-trap token sink (case-289 gfs2/gfs3).
                // Populating `memory` here would short-circuit grep.rs before the
                // presenter and emit a bare `path:dump` with no marker (the old local
                // behavior). The `[semfs: SOURCE INACCESSIBLE]` 403 chunk is preserved
                // (grep handles it ahead of the presenter), so local keeps honesty too.
                for h in hits.iter_mut() {
                    h.memory = None;
                }
            } else {
                let conn = self.db.conn.lock();
                let prepared =
                    conn.prepare("SELECT text FROM chunks WHERE filepath = ?1 ORDER BY ord");
                if let Ok(mut stmt) = prepared {
                    for h in hits.iter_mut() {
                        let Some(fp) = h.filepath.clone() else { continue };
                        if let Ok(rows) = stmt.query_map([&fp], |r| r.get::<_, String>(0)) {
                            let parts: Vec<String> = rows.filter_map(|r| r.ok()).collect();
                            if !parts.is_empty() {
                                h.memory = Some(stitch_chunks(&parts));
                            }
                        }
                    }
                }
            }
        }

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
    use crate::embed::StubEmbedder;

    fn store() -> SqliteVecStore {
        let db = Arc::new(Db::open_in_memory().unwrap());
        SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384))).unwrap()
    }

    /// Knob B whole-doc reconstruction. Consecutive chunks overlap verbatim, so
    /// stitching must DROP the shared span (no duplication) yet reproduce the
    /// document, and must NEVER split a multibyte char at the byte cap.
    #[test]
    fn stitch_chunks_dedups_overlap_and_reconstructs() {
        // "the quick brown fox jumps over" split into two overlapping windows
        // sharing "brown fox jumps".
        let parts = vec![
            "the quick brown fox jumps".to_string(),
            "brown fox jumps over the lazy dog".to_string(),
        ];
        assert_eq!(
            stitch_chunks(&parts),
            "the quick brown fox jumps over the lazy dog"
        );
        // No overlap → plain concatenation (no content invented/dropped).
        let disjoint = vec!["alpha".to_string(), "beta".to_string()];
        assert_eq!(stitch_chunks(&disjoint), "alphabeta");
        // Single chunk → itself.
        assert_eq!(stitch_chunks(&["solo".to_string()]), "solo");
    }

    /// The cap must land on a UTF-8 boundary — the corpus is Chinese (3-byte
    /// chars) — or `&s[..cut]` panics. Drive a chunk past DOC_RETURN_CAP.
    #[test]
    fn stitch_chunks_caps_on_char_boundary() {
        let big = "中".repeat(DOC_RETURN_CAP); // 3 bytes each, far over the cap
        let out = stitch_chunks(&[big]);
        assert!(out.len() <= DOC_RETURN_CAP);
        assert!(out.is_char_boundary(out.len())); // valid slice, no mid-char cut
        assert!(out.chars().all(|c| c == '中'));
    }

    /// Regression (RCA 2026-06-03-extract-uncapped-utf8-text-path): a large UTF-8
    /// text/code file (e.g. a minified node_modules bundle) takes the `Ok(text)`
    /// branch in flush() and is handed whole to `index()`. `index()` must cap
    /// content before chunking so chunk count / embed work is bounded per file —
    /// regardless of source — or one file stalls the whole import.
    #[test]
    fn index_caps_oversized_content_before_chunking() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        let fp = "/x/node_modules/docx/dist/index.umd.cjs";
        let huge = "alpha beta gamma delta ".repeat(160_000); // ~3.7 MiB UTF-8
        assert!(huge.len() > 3 * 1024 * 1024);

        store.index(1, fp, &huge).unwrap();

        let stored: i64 = db
            .conn
            .lock()
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(text)), 0) FROM chunks WHERE filepath = ?1",
                [fp],
                |r| r.get(0),
            )
            .unwrap();
        // Capped to ~1 MiB head (overlap inflates a little); far under the 3.7 MiB
        // input. Uncapped, this would store ≳ the full size and grind the embedder.
        assert!(
            (stored as usize) <= 2 * 1024 * 1024,
            "content not capped before indexing: stored {stored} bytes"
        );
        assert!(stored > 0, "nothing indexed");
    }

    /// Regression (ticket search-deadline-fails-closed-to-empty): when the
    /// cooperative deadline is already blown (CPU starvation simulated by a
    /// past deadline), the search must DEGRADE to best-effort RRF hits, never
    /// fail closed to empty — an agent that sees "0 results" for a query that
    /// matches abandons semantic search.
    #[test]
    fn search_past_deadline_degrades_to_hits_not_empty() {
        use std::time::{Duration, Instant};
        let s = store();
        s.index(1, "/a.md", "alpha credential login verification flow")
            .unwrap();
        s.index(2, "/b.md", "unrelated gardening content").unwrap();

        // Deadline already in the past → the pre-connection point trips.
        let past = Instant::now() - Duration::from_secs(1);
        let hits = s
            .search_blocking("credential login", None, past)
            .expect("past-deadline search must not error");
        assert!(
            !hits.is_empty(),
            "past-deadline search must return best-effort hits, not fail closed to empty"
        );
        assert!(hits.iter().any(|h| h.filepath.as_deref() == Some("/a.md")));
    }

    /// Fix #2: the L5 rerank only sees the top `RERANK_CANDIDATES` RRF hits, so a
    /// single search can't drive an unbounded cross-encoder pass.
    #[tokio::test]
    async fn rerank_candidate_count_is_capped() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Debug)]
        struct CountingReranker {
            seen: AtomicUsize,
        }
        impl Reranker for CountingReranker {
            fn rerank(&self, _q: &str, docs: &[String]) -> anyhow::Result<Vec<f32>> {
                self.seen.store(docs.len(), Ordering::SeqCst);
                Ok(vec![1.0; docs.len()])
            }
        }

        let db = Arc::new(Db::open_in_memory().unwrap());
        let counter = Arc::new(CountingReranker {
            seen: AtomicUsize::new(0),
        });
        let store = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384)))
            .unwrap()
            .with_reranker(counter.clone() as Arc<dyn Reranker>);
        // 60 files (> RERANK_CANDIDATES) all matching the query term.
        for i in 0..60u64 {
            store
                .index(i + 1, &format!("/f{i}.md"), "shared keyword alpha beta gamma")
                .unwrap();
        }

        let hits = store.search("shared keyword", None).await.unwrap();
        let seen = counter.seen.load(Ordering::SeqCst);
        assert!(
            seen <= RERANK_CANDIDATES,
            "reranker saw {seen} docs; cap is {RERANK_CANDIDATES}"
        );
        assert!(hits.len() <= RERANK_CANDIDATES);
        assert!(!hits.is_empty());
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
    /// 256-d vector into the 384-d `vchunks` table and fail. Offline (StubEmbedder).
    #[tokio::test]
    async fn code_files_route_to_code_lane() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut store = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        store
            .enable_code_indexing(Arc::new(StubEmbedder::new(256)))
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
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(TaggedEmbedder { dims: 256, id: "code-A:256".into() }))
            .unwrap();
        w.index(2, "/src/a.rs", "fn a() {}").unwrap();
        drop(w);

        // Reopen with the SAME width (256) but a DIFFERENT code model → must bail.
        let mut w2 = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
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

    /// A legacy/corrupt cache — chunks present but the text identity stamp is gone
    /// — must be refused by the writer, not silently adopted under the current
    /// model (which could mix spaces or drop/recreate the vec table).
    #[tokio::test]
    async fn writer_refuses_index_with_chunks_but_no_text_stamp() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384)))
            .unwrap()
            .index(2, "/a.md", "hello")
            .unwrap();
        // Wipe the text identity stamp, keeping chunks/vectors (legacy/corrupt).
        db.conn
            .lock()
            .execute("DELETE FROM fs_config WHERE key='text_embed_model'", [])
            .unwrap();
        let res = SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384)));
        assert!(res.is_err(), "must refuse a populated index with no text stamp");
    }

    /// Same for the code lane: vchunks_code rows present but the code stamp gone
    /// → enable_code_indexing must refuse rather than adopt.
    #[tokio::test]
    async fn writer_refuses_code_lane_with_rows_but_no_code_stamp() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(StubEmbedder::new(256))).unwrap();
        w.index(2, "/a.rs", "fn a() {}").unwrap(); // populates vchunks_code
        drop(w);
        db.conn
            .lock()
            .execute("DELETE FROM fs_config WHERE key='code_embed_model'", [])
            .unwrap();
        // new() succeeds (text stamp intact); enable_code_indexing must refuse.
        let mut w2 = SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384))).unwrap();
        let res = w2.enable_code_indexing(Arc::new(StubEmbedder::new(256)));
        assert!(res.is_err(), "must refuse a populated code lane with no code stamp");
    }

    /// Fail-open writer (code lane advertised, but no active code embedder) must
    /// NOT index code-like files into the text lane — it skips them (dropping any
    /// stale entry) so vectors aren't stranded in the wrong space. Text files
    /// still index normally.
    #[tokio::test]
    async fn code_file_skipped_when_lane_advertised_but_no_code_embedder() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        // Writer 1 establishes the code lane (stamp), no files yet.
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(StubEmbedder::new(256))).unwrap();
        drop(w);

        // Writer 2: fail-open — code lane advertised but NO code embedder attached.
        let w2 = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        w2.index(2, "/src/x.rs", "fn x() {}").unwrap(); // code path → must be skipped
        w2.index(3, "/docs/y.md", "hello world").unwrap(); // text path → indexed

        let conn = db.conn.lock();
        let code_n: i64 = conn
            .query_row("SELECT count(*) FROM chunks WHERE filepath='/src/x.rs'", [], |r| r.get(0))
            .unwrap();
        let text_n: i64 = conn
            .query_row("SELECT count(*) FROM chunks WHERE filepath='/docs/y.md'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(code_n, 0, "code file must be skipped, not stranded in the text lane");
        assert_eq!(text_n, 1, "text file still indexes");
    }

    /// A stamped cache whose vec table is missing/undercounted (partial restore /
    /// corruption) must be refused by the writer — NOT silently recreated empty
    /// (which would strand existing chunks vectorless).
    #[tokio::test]
    async fn writer_refuses_stamped_cache_with_missing_vectors() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384)))
            .unwrap()
            .index(2, "/a.md", "hello")
            .unwrap(); // chunks + vchunks populated, stamp present
        // Corrupt: drop vchunks (vectors gone) but keep chunks + the stamp.
        db.conn.lock().execute_batch("DROP TABLE vchunks;").unwrap();
        let res = SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384)));
        assert!(res.is_err(), "stamped cache with chunks but missing vchunks must be refused");
    }

    /// Corrupt `text_embed_dims` metadata (identity stamp + counts still
    /// consistent) must make the writer REFUSE — not let ensure_vector_tables drop
    /// the populated `vchunks` based on the bad metadata.
    #[tokio::test]
    async fn writer_refuses_corrupt_text_embed_dims() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384)))
            .unwrap()
            .index(2, "/a.md", "hello")
            .unwrap();
        db.conn
            .lock()
            .execute("UPDATE fs_config SET value='256' WHERE key='text_embed_dims'", [])
            .unwrap();
        let res = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384)));
        assert!(res.is_err(), "corrupt text_embed_dims must be refused");
        // The vectors must NOT have been dropped (writer refused before mutating).
        let n: i64 = db
            .conn
            .lock()
            .query_row("SELECT count(*) FROM vchunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "vchunks rows preserved (no destructive recreate)");
    }

    /// Corrupt `code_embed_dims` must make `enable_code_indexing` REFUSE rather
    /// than let ensure_vector_tables drop the populated `vchunks_code`.
    #[tokio::test]
    async fn writer_refuses_corrupt_code_embed_dims() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(StubEmbedder::new(256))).unwrap();
        w.index(2, "/a.rs", "fn a() {}").unwrap();
        drop(w);
        db.conn
            .lock()
            .execute("UPDATE fs_config SET value='128' WHERE key='code_embed_dims'", [])
            .unwrap();
        let mut w2 = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        let res = w2.enable_code_indexing(Arc::new(StubEmbedder::new(256)));
        assert!(res.is_err(), "corrupt code_embed_dims must be refused");
        let n: i64 = db
            .conn
            .lock()
            .query_row("SELECT count(*) FROM vchunks_code", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "vchunks_code rows preserved (no destructive recreate)");
    }

    /// Missing (not just mismatched) text_embed_dims on a stamped cache must be
    /// refused — ensure_vector_tables would otherwise treat None as compatible.
    #[tokio::test]
    async fn writer_refuses_missing_text_embed_dims() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384)))
            .unwrap()
            .index(2, "/a.md", "hello")
            .unwrap();
        db.conn
            .lock()
            .execute("DELETE FROM fs_config WHERE key='text_embed_dims'", [])
            .unwrap();
        assert!(
            SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384))).is_err(),
            "stamped cache with missing text_embed_dims must be refused"
        );
    }

    /// Missing code_embed_dims on a stamped code lane must be refused by new()
    /// BEFORE ensure_vector_tables (which would drop the populated vchunks_code).
    #[tokio::test]
    async fn writer_refuses_missing_code_embed_dims() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(StubEmbedder::new(256))).unwrap();
        w.index(2, "/a.rs", "fn a() {}").unwrap();
        drop(w);
        db.conn
            .lock()
            .execute("DELETE FROM fs_config WHERE key='code_embed_dims'", [])
            .unwrap();
        assert!(
            SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).is_err(),
            "stamped code lane with missing code_embed_dims must be refused"
        );
        // vchunks_code preserved — refused before any destructive recreate.
        let n: i64 = db
            .conn
            .lock()
            .query_row("SELECT count(*) FROM vchunks_code", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "vchunks_code rows preserved");
    }

    /// A code-lane vec0 failure AFTER is_searchable committed (e.g. vchunks_code
    /// dropped concurrently) must make search() return Err — so grep falls back to
    /// cloud — rather than silently degrade to text-only results.
    #[tokio::test]
    async fn search_errors_when_active_code_lane_fails_at_query_time() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut store = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        store.enable_code_indexing(Arc::new(StubEmbedder::new(256))).unwrap();
        store.index(2, "/src/a.rs", "fn a() {}").unwrap();
        store.index(3, "/docs/b.md", "prose").unwrap();
        assert!(store.is_searchable(), "healthy dual cache is searchable");
        // Simulate a concurrent corruption AFTER the readiness check.
        db.conn.lock().execute_batch("DROP TABLE vchunks_code;").unwrap();
        let res = store.search("anything", None).await;
        assert!(res.is_err(), "active code-lane query failure must propagate, not be swallowed");
    }

    /// A rename crossing the code/text extension boundary drops the index entry
    /// (re-indexed into the correct lane on next write) rather than stranding
    /// vectors in the wrong lane. Same-lane renames still relabel cheaply.
    #[tokio::test]
    async fn lane_crossing_rename_drops_entry() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut store = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        store.enable_code_indexing(Arc::new(StubEmbedder::new(256))).unwrap();
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
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(TaggedEmbedder { dims: 256, id: "code-A:256".into() }))
            .unwrap();
        w.index(2, "/src/lib.rs", "fn f() {}").unwrap();
        drop(w);

        // Writer 2: mismatched code model → enable fails open (code_embedder stays
        // None), but vchunks_code + the .rs vectors persist.
        let mut w2 = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
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
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(StubEmbedder::new(256))).unwrap();
        w.index(2, "/src/only.rs", "fn only() {}").unwrap(); // code lane only
        drop(w);

        // Reader with NO code embedder → code lane inactive, all content stranded.
        let inert = SqliteVecStore::open_existing(db.clone(), Arc::new(StubEmbedder::new(384)));
        assert!(
            !inert.is_searchable(),
            "code-only cache must fall back when the code lane can't be searched"
        );

        // Reader WITH the matching code embedder → code lane active → searchable.
        let active = SqliteVecStore::open_existing(db, Arc::new(StubEmbedder::new(384)))
            .with_code_embedder(Arc::new(StubEmbedder::new(256)));
        assert!(active.is_searchable(), "active code lane → searchable");
    }

    /// A MIXED cache (prose + code) with a POPULATED but inactive code lane must
    /// fail closed to cloud — serving text-only would silently drop code recall.
    /// (When the code lane is active, it's searchable; when EMPTY, the text lane
    /// alone suffices — both covered elsewhere.)
    #[tokio::test]
    async fn mixed_cache_with_populated_inactive_code_lane_falls_back() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(StubEmbedder::new(256))).unwrap();
        w.index(2, "/docs/readme.md", "prose content about the project").unwrap(); // text lane
        w.index(3, "/src/lib.rs", "fn lib() {}").unwrap(); // code lane (has rows)
        drop(w);

        // Reader with NO code embedder → code lane inactive but populated → must
        // fall back to cloud rather than silently serve text-only results.
        let inert = SqliteVecStore::open_existing(db.clone(), Arc::new(StubEmbedder::new(384)));
        assert!(
            !inert.is_searchable(),
            "populated inactive code lane must force cloud fallback even on a mixed cache"
        );

        // With the matching code embedder → code lane active → searchable.
        let active = SqliteVecStore::open_existing(db, Arc::new(StubEmbedder::new(384)))
            .with_code_embedder(Arc::new(StubEmbedder::new(256)));
        assert!(active.is_searchable(), "active code lane → searchable");
    }

    /// An ACTIVE but broken code lane (matching code embedder, but a missing/
    /// corrupt vchunks_code) must fall back to cloud — not silently drop the code
    /// KNN and serve degraded results — when code content depends on that lane.
    #[tokio::test]
    async fn active_but_broken_code_lane_falls_back() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut w = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        w.enable_code_indexing(Arc::new(StubEmbedder::new(256))).unwrap();
        w.index(2, "/src/a.rs", "fn a() {}").unwrap(); // code lane only
        drop(w);

        // Corrupt: drop the code vec0 table but keep `chunks` + the code stamp.
        db.conn.lock().execute_batch("DROP TABLE vchunks_code;").unwrap();

        // Reader WITH the matching code embedder → code lane "active", but the
        // vec0 table is gone → the readiness probe errors → not searchable.
        let reader = SqliteVecStore::open_existing(db, Arc::new(StubEmbedder::new(384)))
            .with_code_embedder(Arc::new(StubEmbedder::new(256)));
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
        let w = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        w.index(2, "/a.md", "hello world").unwrap();
        // No code stamp was written. Reader attaches a code embedder anyway.
        let reader = SqliteVecStore::open_existing(db, Arc::new(StubEmbedder::new(384)))
            .with_code_embedder(Arc::new(StubEmbedder::new(256)));
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
        let s = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
        s.index(2, "/a.md", "hello world").unwrap();
        assert!(s.is_searchable());

        // Same db reopened with a different-width embedder → NOT searchable
        // (a 256-d probe vector against a 384-d vec0 table errors).
        let mismatched =
            SqliteVecStore::open_existing(db.clone(), Arc::new(StubEmbedder::new(256)));
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
        let no_index = SqliteVecStore::open_existing(bare, Arc::new(StubEmbedder::new(384)));
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
                        SqliteVecStore::open_existing(self.db.clone(), Arc::new(StubEmbedder::new(384)));
                    w.index(2, "/a.md", "totally different replacement content zzz")
                        .unwrap();
                }
                Ok(vec![1.0; docs.len()])
            }
        }

        let db = Arc::new(Db::open_in_memory().unwrap());
        let store = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384)))
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
        let s = SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap();
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

    /// L7 edge lifecycle. Edges are now maintained by the DEFERRED graph worker
    /// (`index_graph`), NOT by `index()`. So a vector re-index PRESERVES a file's
    /// edges (the worker re-derives them shortly after), and only delete/rename
    /// clears them. Edges inserted manually since unit tests have no LLM (the
    /// extraction itself is tested in `graph.rs`).
    #[tokio::test]
    async fn reindex_preserves_edges_delete_clears_them() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
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
        // Vector re-index must NOT touch edges (deferred L7 owns them now).
        store.index(2, "/notes/proj.md", "changed").unwrap();
        assert_eq!(count(), 1, "vector re-index must PRESERVE edges");
        // Delete clears them.
        store.remove("/notes/proj.md").unwrap();
        assert_eq!(count(), 0, "delete must drop edges");
    }

    /// With a graph extractor attached, `index()` ENQUEUES the file for deferred
    /// L7 extraction (rather than calling the LLM inline). Constructing the
    /// client does no network I/O — only `index_graph` (not exercised here) would.
    #[tokio::test]
    async fn index_enqueues_graph_work_when_extractor_present() {
        use crate::cache::LocalIndexer;
        let db = Arc::new(Db::open_in_memory().unwrap());
        let llm = Arc::new(crate::llm::LlmClient::openrouter("test-key".into()));
        let store = SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384)))
            .unwrap()
            .with_graph_extractor(llm);
        let q = store.graph_queue().expect("graph queue present with extractor");
        assert!(q.is_idle());
        store.index(7, "/notes/a.md", "hello world").unwrap();
        assert_eq!(q.depth(), 1, "index() must enqueue the file for L7 extraction");
        // A store WITHOUT an extractor has no queue and enqueues nothing.
        let plain = SqliteVecStore::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Arc::new(StubEmbedder::new(384)),
        )
        .unwrap();
        assert!(plain.graph_queue().is_none());
    }

    /// Rename relabels the index (no re-embed) and drops the overwritten
    /// destination's stale rows. Fixes the "stale after rename" correctness bug.
    #[tokio::test]
    async fn rename_relabels_index_and_drops_overwritten_destination() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
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
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
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
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
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
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
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

    /// L1 parse on flush: a binary `.docx` (invalid UTF-8) is extracted
    /// in-process and its text becomes searchable through the real store — the
    /// core of `tickets/local-document-extractors`.
    #[tokio::test]
    async fn flush_extracts_and_indexes_binary_docx() {
        use crate::cache::{CacheFs, LocalIndexer, ROOT_INO};
        use crate::vfs::FileSystem;

        const DOCX: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.docx");

        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
        let fs = CacheFs::new(db).with_indexer(store.clone() as Arc<dyn LocalIndexer>);

        let (_attr, handle) = fs.create_file(ROOT_INO, "report.docx", 0o644, 0, 0).await.unwrap();
        handle.write(0, DOCX).await.unwrap();
        handle.flush().await.unwrap();

        // The CJK title now lives in the index — extraction happened on flush.
        let hits = store.search("数据安全风险整改进度月度汇总报告", None).await.unwrap();
        assert!(!hits.is_empty(), "docx text was not extracted+indexed on flush");
        assert_eq!(hits[0].filepath.as_deref(), Some("/report.docx"));
        // A successfully extracted file is not in the unindexed bucket.
        assert_eq!(fs.unindexed_count(), 0);
    }

    /// Flushing a binary xlsx indexes extracted text into `chunks` so it is
    /// queryable via `Db::get_extracted_text` (the grep-inline path). No sibling
    /// file is materialised — that would duplicate storage already in `chunks`.
    #[tokio::test]
    async fn flush_xlsx_extracted_text_queryable_via_db() {
        use crate::cache::{CacheFs, LocalIndexer, ROOT_INO};
        use crate::vfs::FileSystem;

        const XLSX: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.xlsx");

        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
        let fs = CacheFs::new(db.clone()).with_indexer(store.clone() as Arc<dyn LocalIndexer>);

        let (_attr, handle) = fs
            .create_file(ROOT_INO, "sales.xlsx", 0o644, 0, 0)
            .await
            .unwrap();
        handle.write(0, XLSX).await.unwrap();
        handle.flush().await.unwrap();

        // No `.extracted.md` sibling in the FUSE mount — storage stays lean.
        let names = fs.readdir(ROOT_INO).await.unwrap().unwrap_or_default();
        assert!(
            !names.iter().any(|n| n.ends_with(".extracted.md")),
            "unexpected extracted sibling in FUSE; entries: {names:?}"
        );

        // Extracted text must be queryable from the chunks table.
        let text = db
            .get_extracted_text("/sales.xlsx")
            .expect("extracted text must be present in chunks");
        assert!(
            text.contains("Changan Automobile"),
            "extracted text missing known cell; got: {:?}",
            &text[..text.len().min(200)]
        );
    }

    /// `Db::upsert_extracted_sibling` materialises a read-only `.extracted.md`
    /// sibling in the FUSE mount — the `SEMFS_EXTRACT_SIBLING=on` delivery path,
    /// where the agent `cat`s a few lines instead of receiving the whole file
    /// inline via grep. Idempotent: a repeat call reuses the derived inode.
    #[tokio::test]
    async fn upsert_extracted_sibling_materialises_readonly_sibling() {
        use crate::cache::{CacheFs, LocalIndexer, ROOT_INO};
        use crate::vfs::FileSystem;

        const XLSX: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.xlsx");

        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
        let fs = CacheFs::new(db.clone()).with_indexer(store.clone() as Arc<dyn LocalIndexer>);

        let (_attr, handle) = fs
            .create_file(ROOT_INO, "sales.xlsx", 0o644, 0, 0)
            .await
            .unwrap();
        handle.write(0, XLSX).await.unwrap();
        handle.flush().await.unwrap();

        let text = db
            .get_extracted_text("/sales.xlsx")
            .expect("extracted text must be present in chunks");

        // Materialise the sibling (what flush does under SEMFS_EXTRACT_SIBLING=on),
        // then again to prove idempotency.
        let ino1 = db.upsert_extracted_sibling("/sales.xlsx", &text).unwrap();
        let ino2 = db.upsert_extracted_sibling("/sales.xlsx", &text).unwrap();
        assert_eq!(ino1, ino2, "upsert must reuse the derived inode in place");

        // Visible in the FUSE mount as a sibling of the source file.
        let names = fs.readdir(ROOT_INO).await.unwrap().unwrap_or_default();
        assert!(
            names.iter().any(|n| n == "sales.xlsx.extracted.md"),
            "extracted sibling missing from FUSE; entries: {names:?}"
        );
    }

    /// A binary file with no recoverable text is recorded as unindexed (visible
    /// in `semfs status`) — never silently dropped, never crashes the flush.
    #[tokio::test]
    async fn flush_records_unextractable_binary_as_unindexed() {
        use crate::cache::{CacheFs, LocalIndexer, ROOT_INO};
        use crate::vfs::FileSystem;

        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
        let fs = CacheFs::new(db).with_indexer(store.clone() as Arc<dyn LocalIndexer>);

        // 0xFF is never a valid UTF-8 lead byte → fails the text path, sniffs
        // Unknown → no extractor → unindexed.
        let (_attr, handle) = fs.create_file(ROOT_INO, "blob.bin", 0o644, 0, 0).await.unwrap();
        handle.write(0, &[0xFF, 0xFE, 0x00, 0x01, 0x02]).await.unwrap();
        handle.flush().await.unwrap();

        assert_eq!(fs.unindexed_count(), 1, "unextractable binary must be counted");

        // A later flush that yields text clears the marker.
        handle.write(0, b"now i am plain searchable text").await.unwrap();
        handle.flush().await.unwrap();
        assert_eq!(fs.unindexed_count(), 0, "successful re-flush must clear the marker");
    }

    /// Regression (Codex HIGH): a previously-indexed text file overwritten by an
    /// unextractable binary must be DEINDEXED (no stale search hits) AND counted
    /// as unindexed — not left searchable with stale content.
    #[tokio::test]
    async fn overwriting_indexed_text_with_binary_deindexes_and_counts() {
        use crate::cache::{CacheFs, LocalIndexer, ROOT_INO};
        use crate::vfs::FileSystem;

        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
        let fs = CacheFs::new(db).with_indexer(store.clone() as Arc<dyn LocalIndexer>);

        let (_a, h) = fs.create_file(ROOT_INO, "note.md", 0o644, 0, 0).await.unwrap();
        h.write(0, b"alpha sentinel beta gamma delta").await.unwrap();
        h.flush().await.unwrap();
        assert!(!store.search("sentinel", None).await.unwrap().is_empty());

        // Overwrite fully with binary (200 > 31 bytes → all-binary, invalid UTF-8).
        h.write(0, &[0xFFu8; 200]).await.unwrap();
        h.flush().await.unwrap();

        assert!(
            store.search("sentinel", None).await.unwrap().is_empty(),
            "stale text must be deindexed when overwritten by an unextractable binary"
        );
        assert_eq!(fs.unindexed_count(), 1);
    }

    /// Regression (Codex HIGH): when extraction SUCCEEDS but indexing FAILS, the
    /// file must be recorded as unindexed (accounted), never silently dropped.
    #[tokio::test]
    async fn extracted_but_index_error_is_recorded_unindexed() {
        use crate::cache::{CacheFs, LocalIndexer, ROOT_INO};
        use crate::vfs::FileSystem;

        #[derive(Debug)]
        struct FailingIndexer;
        #[async_trait::async_trait]
        impl LocalIndexer for FailingIndexer {
            async fn index(&self, _: u64, _: &str, _: &str) -> anyhow::Result<()> {
                anyhow::bail!("simulated index failure")
            }
            async fn remove(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            async fn rename(&self, _: &str, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
        }

        const DOCX: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.docx");
        let db = Arc::new(Db::open_in_memory().unwrap());
        let fs = CacheFs::new(db).with_indexer(Arc::new(FailingIndexer) as Arc<dyn LocalIndexer>);

        let (_a, h) = fs.create_file(ROOT_INO, "r.docx", 0o644, 0, 0).await.unwrap();
        h.write(0, DOCX).await.unwrap();
        h.flush().await.unwrap(); // extraction OK, index() errors

        assert_eq!(
            fs.unindexed_count(),
            1,
            "extracted-but-unindexable file must be accounted, not dropped"
        );
    }

    /// Regression (Codex MEDIUM): rename keeps `fs_unindexed` consistent —
    /// overwriting one unindexed file with another drops the destination's marker
    /// (no overcount) and the surviving source marker is relabeled atomically.
    #[tokio::test]
    async fn rename_overwrite_keeps_unindexed_count_consistent() {
        use crate::cache::{CacheFs, LocalIndexer, ROOT_INO};
        use crate::vfs::FileSystem;

        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
        let fs = CacheFs::new(db).with_indexer(store as Arc<dyn LocalIndexer>);

        for name in ["a.bin", "b.bin"] {
            let (_a, h) = fs.create_file(ROOT_INO, name, 0o644, 0, 0).await.unwrap();
            h.write(0, &[0xFFu8; 10]).await.unwrap();
            h.flush().await.unwrap();
        }
        assert_eq!(fs.unindexed_count(), 2);

        // Overwrite a.bin with b.bin: destination marker cleared in-tx, source
        // marker relabeled to the destination path.
        fs.rename(ROOT_INO, "b.bin", ROOT_INO, "a.bin").await.unwrap();
        assert_eq!(
            fs.unindexed_count(),
            1,
            "overwrite must drop the destination's marker and keep exactly the source's"
        );

        // Unlinking the surviving path clears it → no leak.
        fs.unlink(ROOT_INO, "a.bin").await.unwrap();
        assert_eq!(fs.unindexed_count(), 0);
    }

    // Real-model offline semantic search (arctic-s embed → index → search on a
    // zero-overlap query) is validated live by `crates/e2e/phase_local_l1_l5.sh`
    // through a real mount — kept out of `cargo test` (no download/network here).

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
    /// a deterministic StubEmbedder index — so no local model loads here. Proves
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
        let store = SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384)))
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

    /// H1b: a prior run's deliverable under `model_output/` (possibly fabricated)
    /// must never be returned as a search SOURCE — case-289's worst run read a
    /// stale fabricated `model_output/` list and copied it (207K tokens, 5/15,
    /// dishonest). Search must exclude the agent's own output dir.
    #[tokio::test]
    async fn search_excludes_agent_output_dir() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store = SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384))).unwrap();
        store
            .index(2, "/notes/auth.md", "reset your password via the emailed link")
            .unwrap();
        store
            .index(3, "/model_output/answer.md", "reset your password via the emailed link")
            .unwrap();
        let hits = store.search("password reset", None).await.unwrap();
        assert!(!hits.is_empty(), "the real source should still be found");
        assert!(
            hits.iter().all(|h| h.filepath.as_deref() != Some("/model_output/answer.md")),
            "model_output/ (agent's own output) must be excluded from search"
        );
    }

    /// H1: in snippet mode the store must NOT populate `memory`, so `grep` renders
    /// each hit through the chunk presenter (line ranges + `# ^ COMPLETE FILE …do not
    /// open it`) instead of a bare `path:dump`. Populating `memory` short-circuits
    /// grep.rs ahead of the presenter — the local behavior that left codex distrusting
    /// the excerpt and format-trapping (.xls parse loops). Chunk stays for the presenter.
    #[tokio::test]
    async fn snippet_mode_leaves_memory_none_so_grep_emits_complete_marker() {
        std::env::set_var("SEMFS_RETURN_MODE", "snippet");
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store = SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384))).unwrap();
        store
            .index(2, "/notes/auth.md", "reset your password via the emailed link")
            .unwrap();
        let hits = store.search("password reset", None).await.unwrap();
        std::env::remove_var("SEMFS_RETURN_MODE");
        assert!(!hits.is_empty(), "expected a hit for the indexed file");
        assert!(
            hits[0].memory.is_none(),
            "snippet mode must leave memory None so grep uses the chunk presenter"
        );
        assert!(hits[0].chunk.is_some(), "chunk must be preserved for the presenter");
    }

    // The whole local pipeline to the reranker stage (real fastembed embed →
    // index → RRF → rerank) is validated live by `crates/e2e/phase_local_l1_l5.sh`
    // with the local int8 reranker. The local-embed + CLOUD-rerank composition is
    // covered by its independently-tested halves (real local embed via the e2e
    // script; cloud rerank via `search_with_cloud_reranker_applies_rerank_scores`),
    // so no download-gated combination test lives in `cargo test`.

    // ── Realistic end-to-end tests (Workstream C) ───────────────────────────

    /// C2: a multi-chunk document — a needle in the MIDDLE is retrievable, and
    /// the returned chunk actually contains it (proves chunk-granular retrieval).
    #[tokio::test]
    async fn multi_chunk_doc_retrieves_middle_chunk() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store =
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
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
        // StubEmbedder bucket collisions, so we assert retrieval, not the snippet.)
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
            let store = SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384))).unwrap();
            store.index(2, "/p.md", "persistent alpha beta content").unwrap();
        } // store + db dropped — simulates a daemon restart

        let db2 = Arc::new(Db::open(&path).unwrap());
        let store2 = SqliteVecStore::open_existing(db2, Arc::new(StubEmbedder::new(384)));
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
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
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
            Arc::new(SqliteVecStore::new(db.clone(), Arc::new(StubEmbedder::new(384))).unwrap());
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
            SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384))).unwrap(); // create vec0 tables
        }
        let mut handles = vec![];
        for w in 0..2u64 {
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                let db = Arc::new(Db::open(&p).unwrap());
                let store = SqliteVecStore::open_existing(db, Arc::new(StubEmbedder::new(384)));
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
            Arc::new(SqliteVecStore::new(db, Arc::new(StubEmbedder::new(384))).unwrap());
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
