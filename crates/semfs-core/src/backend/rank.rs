//! Shared post-retrieval ranking pipeline — backend-agnostic.
//!
//! `SqliteVecStore` and `PgVectorStore` produce raw ranked lists from their own
//! storage (vec0+fts5 vs pgvector+tsvector), then run the SAME ranking here:
//! RRF fusion → L5 rerank → L7 co-mention boost → L6 salience. Keeping this in
//! one place means the two backends can never drift in ranking semantics.

use std::collections::{HashMap, HashSet};

use super::SearchHit;
use crate::rerank::Reranker;

/// RRF constant: a list's rank-`r` contribution is `1/(RRF_K + r)`.
pub const RRF_K: f64 = 60.0;
/// Number of retrieval lanes (text vector / code vector / keyword FTS). Sizes the
/// per-lane best-rank array in `FileAcc`.
const N_LANES: usize = 4;

/// The retrieval lane a chunk came from. RRF fuses one vote PER LANE (a file's
/// best chunk in that lane), so fusion must know which lane each bump belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Text-embedding KNN (e5 over `vchunks` / pgvector `chunks`).
    Text = 0,
    /// Code-embedding KNN (Jina over `vchunks_code`). sqlite_vec only.
    Code = 1,
    /// Keyword BM25 / Postgres FTS.
    Fts = 2,
    /// Filename/path-token match. Agents query terms that match the *path*
    /// (e.g. "best-selling product data" → `best_selling_product_core_data_list.txt`);
    /// content-only ranking misses this. A path-token lane surfaces the file the
    /// user is clearly naming, so grep returns it #1 and the agent stops there
    /// instead of crawling. (tickets/ls-kg-semantic-readdir; case-289 token lever.)
    Path = 3,
}
/// Salience recency half-life (days).
const SALIENCE_HALF_LIFE_DAYS: f64 = 14.0;

/// Knob A — how many of a file's best-matching chunks to concatenate as the
/// reranker's input. Default 1 (the single best-rank chunk): on this corpus,
/// adding more chunks DILUTED the signal for numeric/sparse docs and *lowered*
/// the answer file's rerank rank (measured #4→#6 at N=3). Also bounded above by
/// the reranker's 1024-token window (`rerank::local`) ≈ 4 chunks, so N>4 just
/// truncates. Override `SEMFS_RERANK_CHUNKS` to sweep. (ticket local-ranking-precision.)
pub const RERANK_CHUNKS_PER_FILE: usize = 1;

/// `SEMFS_RERANK_CHUNKS` override → chunks per file fed to the reranker.
fn rerank_chunks_per_file() -> usize {
    std::env::var("SEMFS_RERANK_CHUNKS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(RERANK_CHUNKS_PER_FILE)
}

/// Per-file fusion accumulator: the best (lowest) rank this file achieved in each
/// lane, plus every matched chunk (for the reranker input). The RRF score is
/// derived in `score()` as the sum over lanes of `1/(RRF_K + best_rank)` — i.e.
/// each file votes ONCE per lane with its best chunk, NOT once per chunk. Per-chunk
/// summation made the score chunk-count-weighted, letting high-chunk code/JSON
/// files bury low-chunk answer files on content queries
/// (rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md).
#[derive(Debug, Default)]
pub struct FileAcc {
    /// Best (lowest) rank seen per lane; `None` = the file never matched that lane.
    pub best_rank: [Option<usize>; N_LANES],
    pub chunks: Vec<(usize, String)>,
}

impl FileAcc {
    /// Fused RRF score: one contribution per lane the file matched, using that
    /// lane's best rank. Max/best-rank aggregation — count-invariant.
    pub fn score(&self) -> f64 {
        self.best_rank
            .iter()
            .filter_map(|r| *r)
            .map(|r| 1.0 / (RRF_K + r as f64))
            .sum()
    }
}

/// Record one retrieved chunk into the fused per-file map: keep the file's best
/// (lowest) rank for this `lane`, and collect the chunk (with its rank) as a
/// rerank-input candidate. Fusion is per-lane (best rank), so repeat chunks from
/// the same file+lane add rerank candidates but do NOT inflate the score.
pub fn rrf_bump(
    acc: &mut HashMap<String, FileAcc>,
    fp: String,
    chunk: String,
    rank: usize,
    lane: Lane,
) {
    let e = acc.entry(fp).or_default();
    let slot = &mut e.best_rank[lane as usize];
    *slot = Some(slot.map_or(rank, |best| best.min(rank)));
    e.chunks.push((rank, chunk));
}

/// A file's top-`n` DISTINCT chunks by best (lowest) rank, concatenated in rank
/// order — the reranker input. Dedups chunk text (the same chunk can surface in
/// several lanes), keeping its best rank. Empty in → empty string.
fn top_chunks_text(mut chunks: Vec<(usize, String)>, n: usize) -> String {
    chunks.sort_by_key(|(r, _)| *r);
    let mut seen: HashSet<String> = HashSet::new();
    let mut picked: Vec<String> = Vec::new();
    for (_, text) in chunks {
        if seen.insert(text.clone()) {
            picked.push(text);
            if picked.len() >= n {
                break;
            }
        }
    }
    picked.join("\n")
}

/// L6 salience multiplier — recency (exp decay) + log-damped access, bounded to
/// a narrow band so it nudges ranking without dominating relevance.
pub fn salience(now_ms: i64, last_accessed_ms: Option<i64>, access_count: i64) -> f64 {
    let recency = match last_accessed_ms {
        Some(t) => {
            let age_days = ((now_ms - t).max(0) as f64) / 86_400_000.0;
            0.5f64.powf(age_days / SALIENCE_HALF_LIFE_DAYS)
        }
        None => 0.5,
    };
    let access = (1.0 + access_count.max(0) as f64).ln();
    1.0 + 0.2 * (recency - 0.5) + 0.05 * access
}

/// Collapse the fused map → prefix-filtered, score-sorted hits. `chunk` carries
/// the file's top-N matched chunks (Knob A) — the reranker input; the final
/// whole-document text is attached later (Knob B, in the backend).
pub fn to_hits(by_file: HashMap<String, FileAcc>, prefix: Option<&str>) -> Vec<SearchHit> {
    let n = rerank_chunks_per_file();
    let mut hits: Vec<SearchHit> = by_file
        .into_iter()
        .filter(|(fp, _)| prefix.map_or(true, |p| fp.starts_with(p)))
        .map(|(fp, acc)| SearchHit {
            filepath: Some(fp),
            memory: None,
            similarity: acc.score(),
            chunk: Some(top_chunks_text(acc.chunks, n)),
        })
        .collect();
    sort_desc(&mut hits);
    hits
}

/// Sort hits by descending score.
pub fn sort_desc(hits: &mut [SearchHit]) {
    hits.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// L5: replace scores with cross-encoder rerank scores, then re-sort.
pub fn apply_reranker(
    hits: &mut [SearchHit],
    reranker: &dyn Reranker,
    query: &str,
) -> anyhow::Result<()> {
    if hits.is_empty() {
        return Ok(());
    }
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
    sort_desc(hits);
    Ok(())
}

/// L7: ×1.05 for any hit that shares an extracted entity with another hit.
/// `entities(filepath)` returns the file's entity set (storage-specific lookup).
pub fn apply_comention_boost(hits: &mut [SearchHit], entities: impl Fn(&str) -> HashSet<String>) {
    let paths: Vec<String> = hits.iter().filter_map(|h| h.filepath.clone()).collect();
    let ents: HashMap<String, HashSet<String>> =
        paths.iter().map(|p| (p.clone(), entities(p))).collect();
    for h in hits.iter_mut() {
        if let Some(fp) = &h.filepath {
            if let Some(mine) = ents.get(fp) {
                let shares = !mine.is_empty()
                    && paths
                        .iter()
                        .any(|o| o != fp && ents.get(o).is_some_and(|e| !e.is_disjoint(mine)));
                if shares {
                    // Sign-correct nudge: rerank scores are cross-encoder LOGITS that
                    // can be NEGATIVE. A plain `*= 1.05` on a negative score makes it
                    // MORE negative → demotes the hit (inverts the intended boost). So
                    // divide when negative: a boost always raises rank, both signs.
                    let f = 1.05f64;
                    h.similarity *= if h.similarity >= 0.0 { f } else { 1.0 / f };
                }
            }
        }
    }
}

/// L6: multiply each hit by its salience. `stats(filepath)` returns
/// `(last_accessed_ms, access_count)` (storage-specific lookup).
pub fn apply_salience(hits: &mut [SearchHit], now_ms: i64, stats: impl Fn(&str) -> (Option<i64>, i64)) {
    for h in hits.iter_mut() {
        if let Some(fp) = &h.filepath {
            let (last, count) = stats(fp);
            // Sign-correct: salience is a multiplicative nudge in [0.85,1.5], but
            // rerank scores can be NEGATIVE — `*= f` would then invert (high salience
            // demotes). Divide when negative so higher salience always raises rank.
            let f = salience(now_ms, last, count);
            h.similarity *= if h.similarity >= 0.0 { f } else { 1.0 / f };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salience_rewards_recency_and_access_but_stays_bounded() {
        let now = 1_000_000_000_000i64;
        let day = 86_400_000i64;
        assert!(salience(now, Some(now), 0) > salience(now, Some(now - 60 * day), 0));
        assert!(salience(now, Some(now), 25) > salience(now, Some(now), 0));
        for (last, acc) in [(Some(now), 0i64), (Some(now - 365 * day), 0), (Some(now), 1000), (None, 0)] {
            let s = salience(now, last, acc);
            assert!(s > 0.85 && s < 1.5, "salience {s} escaped the nudge band");
        }
    }

    /// Knob A: the reranker input is a file's top-N DISTINCT chunks in best-rank
    /// order. Lower rank wins; duplicate chunk text (same chunk across lanes) is
    /// collapsed; at most N are kept.
    #[test]
    fn top_chunks_text_picks_top_n_distinct_by_rank() {
        let chunks = vec![
            (5, "c_fifth".to_string()),
            (0, "a_best".to_string()),
            (5, "a_best".to_string()), // dup of the best chunk via another lane
            (2, "b_second".to_string()),
            (9, "d_worst".to_string()),
        ];
        // top-3 by rank, deduped, joined best-first: a_best(0), b_second(2), c_fifth(5)
        assert_eq!(top_chunks_text(chunks, 3), "a_best\nb_second\nc_fifth");
        // n larger than available → all distinct, no panic
        assert_eq!(top_chunks_text(vec![(1, "only".to_string())], 3), "only");
        // empty → empty string
        assert_eq!(top_chunks_text(vec![], 3), "");
    }

    /// rrf_bump sums the score ACROSS lanes (one vote per lane, best rank) AND
    /// collects every chunk for rerank input.
    #[test]
    fn rrf_bump_sums_across_lanes_and_collects_chunks() {
        let mut acc: HashMap<String, FileAcc> = HashMap::new();
        rrf_bump(&mut acc, "/f".into(), "chunk_a".into(), 0, Lane::Text);
        rrf_bump(&mut acc, "/f".into(), "chunk_b".into(), 1, Lane::Fts);
        let e = &acc["/f"];
        assert_eq!(e.chunks.len(), 2);
        let expected = 1.0 / (RRF_K + 0.0) + 1.0 / (RRF_K + 1.0);
        assert!((e.score() - expected).abs() < 1e-12);
    }

    /// Root-cause fix: a file's score is COUNT-INVARIANT within a lane. Many
    /// matching chunks in one lane contribute ONE vote at the file's best (lowest)
    /// rank — not a per-chunk sum. A 1-chunk file at rank 0 must outscore a
    /// 50-chunk file whose best rank is 5, in the same lane.
    #[test]
    fn rrf_bump_is_count_invariant_within_a_lane() {
        let mut acc: HashMap<String, FileAcc> = HashMap::new();
        // High-chunk file: 50 chunks in the code lane, best rank 5.
        for rank in 5..55 {
            rrf_bump(&mut acc, "/many.py".into(), format!("c{rank}"), rank, Lane::Code);
        }
        // Low-chunk file: a single chunk in the text lane at the top rank.
        rrf_bump(&mut acc, "/answer.xlsx".into(), "best".into(), 0, Lane::Text);

        // Each file scores once for its lane at its best rank — chunk count ignored.
        assert!((acc["/many.py"].score() - 1.0 / (RRF_K + 5.0)).abs() < 1e-12);
        assert!((acc["/answer.xlsx"].score() - 1.0 / (RRF_K + 0.0)).abs() < 1e-12);
        // The 1-chunk answer file now outranks the 50-chunk code file.
        assert!(acc["/answer.xlsx"].score() > acc["/many.py"].score());
        // All 50 chunks are still collected for rerank input.
        assert_eq!(acc["/many.py"].chunks.len(), 50);
    }

    /// Within a lane, only the BEST (lowest) rank counts, regardless of arrival
    /// order; distinct lanes each contribute their own best rank.
    #[test]
    fn rrf_bump_keeps_best_rank_per_lane() {
        let mut acc: HashMap<String, FileAcc> = HashMap::new();
        // Same lane, worse rank arrives first then better — best (2) must win.
        rrf_bump(&mut acc, "/f".into(), "a".into(), 7, Lane::Text);
        rrf_bump(&mut acc, "/f".into(), "b".into(), 2, Lane::Text);
        // A second lane adds an independent vote.
        rrf_bump(&mut acc, "/f".into(), "c".into(), 3, Lane::Fts);
        let e = &acc["/f"];
        assert_eq!(e.best_rank[Lane::Text as usize], Some(2));
        assert_eq!(e.best_rank[Lane::Fts as usize], Some(3));
        assert_eq!(e.best_rank[Lane::Code as usize], None);
        let expected = 1.0 / (RRF_K + 2.0) + 1.0 / (RRF_K + 3.0);
        assert!((e.score() - expected).abs() < 1e-12);
    }
}
