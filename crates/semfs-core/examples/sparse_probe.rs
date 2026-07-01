//! Phase 2: "sparse index instead of BM25". Builds a sparse lexical lane
//! (SPLADE++ / BGE-M3) over the seeded chunks, fuses it with the dense gemma
//! vector lane via RRF, and reports where the case-289 answer ranks under
//! dense-only / sparse-only / RRF(dense+sparse) — the apples-to-apples vs the
//! BM25 RRF measured in Phase 1.
//!
//! Run: cargo run --release -p semfs-core --example sparse_probe -- \
//!        ~/.semfs/chanpin-gemma.db <bgem3|splade>

use fastembed::{
    EmbeddingModel, InitOptions, SparseInitOptions, SparseModel, SparseTextEmbedding, TextEmbedding,
};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb + 1e-8)
}

/// Sparse dot product over (index -> weight) maps.
fn sparse_dot(q: &HashMap<u32, f32>, d_idx: &[u32], d_val: &[f32]) -> f32 {
    d_idx
        .iter()
        .zip(d_val)
        .map(|(i, v)| q.get(i).copied().unwrap_or(0.0) * v)
        .sum()
}

/// File-level max-pool ranking from per-chunk scores; returns files sorted desc.
fn rank_files<'a>(rows: &'a [(String, String)], scores: &[f32]) -> Vec<(&'a str, f32)> {
    let mut best: HashMap<&str, f32> = HashMap::new();
    for (i, (fp, _)) in rows.iter().enumerate() {
        let e = best.entry(fp.as_str()).or_insert(f32::MIN);
        if scores[i] > *e {
            *e = scores[i];
        }
    }
    let mut v: Vec<(&str, f32)> = best.into_iter().collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v
}

fn rank_of(ranked: &[(&str, f32)], needle: &str) -> Option<usize> {
    ranked.iter().position(|(fp, _)| fp.contains(needle))
}

fn main() -> anyhow::Result<()> {
    let db = std::env::args()
        .nth(1)
        .expect("usage: sparse_probe <db> [bgem3|splade]");
    let which = std::env::args().nth(2).unwrap_or_else(|| "bgem3".into());
    let sparse_model = match which.as_str() {
        "splade" => SparseModel::SPLADEPPV1,
        _ => SparseModel::BGEM3,
    };
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // FILE-LEVEL: concatenate each file's chunks (the answer ranking is file-level
    // max-pool anyway). ~615 files vs ~5400 chunks → ~9x fewer embeds.
    let mut stmt = conn.prepare("SELECT filepath, text FROM chunks ORDER BY filepath, id")?;
    let mut acc: HashMap<String, String> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for r in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (fp, text) = r?;
        if !acc.contains_key(&fp) {
            order.push(fp.clone());
        }
        let e = acc.entry(fp).or_default();
        if e.chars().count() < 4000 {
            e.push('\n');
            e.push_str(&text);
        }
    }
    let rows: Vec<(String, String)> = order
        .iter()
        .map(|fp| (fp.clone(), acc[fp].chars().take(800).collect::<String>()))
        .collect();
    println!(
        "loaded {} files (from chunks) from {db}; sparse model = {which}",
        rows.len()
    );
    let texts: Vec<String> = rows.iter().map(|(_, t)| t.clone()).collect();

    // Dense lane (gemma, matches the seeded index embedder).
    let mut dense = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::EmbeddingGemma300M).with_show_download_progress(false),
    )?;
    let mut demb: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
    for w in texts.chunks(64) {
        demb.extend(dense.embed(w.to_vec(), None)?);
    }

    // Sparse lane.
    let mut sparse = SparseTextEmbedding::try_new(
        SparseInitOptions::new(sparse_model).with_show_download_progress(false),
    )?;
    let semb: Vec<(Vec<u32>, Vec<f32>)> = {
        let mut out = Vec::with_capacity(texts.len());
        for w in texts.chunks(64) {
            for e in sparse.embed(w.to_vec(), None)? {
                out.push((e.indices.iter().map(|&i| i as u32).collect(), e.values));
            }
        }
        out
    };
    println!("embedded dense + sparse for {} chunks", rows.len());

    let answer = "best_selling_product_core_data_list";
    let query = "top10 best selling products 畅销商品 成交金额 转化率 商品标题";

    for run in 1..=3 {
        let qd = dense.embed(vec![query.to_string()], None)?.remove(0);
        let qs = sparse.embed(vec![query.to_string()], None)?.remove(0);
        let qmap: HashMap<u32, f32> = qs
            .indices
            .iter()
            .map(|&i| i as u32)
            .zip(qs.values.iter().copied())
            .collect();

        let dscores: Vec<f32> = (0..rows.len()).map(|i| cosine(&qd, &demb[i])).collect();
        let sscores: Vec<f32> = (0..rows.len())
            .map(|i| sparse_dot(&qmap, &semb[i].0, &semb[i].1))
            .collect();

        let dranked = rank_files(&rows, &dscores);
        let sranked = rank_files(&rows, &sscores);

        // RRF over the two file-level rankings (k=60, the pipeline default).
        let k = 60.0f32;
        let mut rrf: HashMap<&str, f32> = HashMap::new();
        for (r, (fp, _)) in dranked.iter().enumerate() {
            *rrf.entry(fp).or_insert(0.0) += 1.0 / (k + r as f32);
        }
        for (r, (fp, _)) in sranked.iter().enumerate() {
            *rrf.entry(fp).or_insert(0.0) += 1.0 / (k + r as f32);
        }
        let mut fused: Vec<(&str, f32)> = rrf.into_iter().collect();
        fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        println!(
            "run{run}: answer rank  dense-only #{:?}  sparse-only #{:?}  RRF(dense+sparse) #{:?}  / {} files",
            rank_of(&dranked, answer),
            rank_of(&sranked, answer),
            rank_of(&fused, answer),
            dranked.len()
        );
    }
    Ok(())
}
