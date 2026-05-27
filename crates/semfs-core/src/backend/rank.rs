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
/// Salience recency half-life (days).
const SALIENCE_HALF_LIFE_DAYS: f64 = 14.0;

/// Accumulate one ranked list into the fused per-file map, keeping the first
/// (strongest) chunk text as the file's representative.
pub fn rrf_bump(acc: &mut HashMap<String, (String, f64)>, fp: String, chunk: String, rank: usize) {
    let s = 1.0 / (RRF_K + rank as f64);
    acc.entry(fp)
        .and_modify(|e| e.1 += s)
        .or_insert((chunk, s));
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

/// Collapse the fused map → prefix-filtered, score-sorted hits.
pub fn to_hits(by_file: HashMap<String, (String, f64)>, prefix: Option<&str>) -> Vec<SearchHit> {
    let mut hits: Vec<SearchHit> = by_file
        .into_iter()
        .filter(|(fp, _)| prefix.map_or(true, |p| fp.starts_with(p)))
        .map(|(fp, (chunk, score))| SearchHit {
            filepath: Some(fp),
            memory: None,
            chunk: Some(chunk),
            similarity: score,
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
                    h.similarity *= 1.05;
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
            h.similarity *= salience(now_ms, last, count);
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
}
