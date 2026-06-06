//! Phase 2 (fast): sparse-only retrieval rank for case 289. No dense re-embed
//! (that's the slow part on CPU and is already measured in Phase 1 = answer #0).
//! Reports where the answer FILE ranks using ONLY the sparse lexical lane —
//! directly comparable to BM25's standalone contribution.
//!
//! Run: cargo run --release -p semfs-core --example sparse_only -- <db> <bgem3|splade>

use fastembed::{SparseInitOptions, SparseModel, SparseTextEmbedding};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;

fn main() -> anyhow::Result<()> {
    let db = std::env::args().nth(1).expect("usage: sparse_only <db> [bgem3|splade]");
    let which = std::env::args().nth(2).unwrap_or_else(|| "splade".into());
    let model = match which.as_str() {
        "bgem3" => SparseModel::BGEM3,
        _ => SparseModel::SPLADEPPV1,
    };
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare("SELECT filepath, text FROM chunks ORDER BY filepath, id")?;
    let mut acc: HashMap<String, String> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for r in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (fp, text) = r?;
        if !acc.contains_key(&fp) {
            order.push(fp.clone());
        }
        let e = acc.entry(fp).or_default();
        if e.chars().count() < 800 {
            e.push('\n');
            e.push_str(&text);
        }
    }
    let files: Vec<String> = order.clone();
    let texts: Vec<String> = order.iter().map(|f| acc[f].chars().take(800).collect()).collect();
    println!("{} files; sparse model={which}", files.len());

    let mut sparse = SparseTextEmbedding::try_new(
        SparseInitOptions::new(model).with_show_download_progress(false),
    )?;
    let mut docs: Vec<HashMap<u32, f32>> = Vec::with_capacity(texts.len());
    for w in texts.chunks(32) {
        for e in sparse.embed(w.to_vec(), None)? {
            docs.push(e.indices.iter().map(|&i| i as u32).zip(e.values).collect());
        }
    }
    println!("sparse-embedded {} files", docs.len());

    let answer = "best_selling_product_core_data_list";
    let query = "top10 best selling products 畅销商品 成交金额 转化率 商品标题";
    for run in 1..=3 {
        let q = sparse.embed(vec![query.to_string()], None)?.remove(0);
        let qmap: HashMap<u32, f32> =
            q.indices.iter().map(|&i| i as u32).zip(q.values).collect();
        let mut scored: Vec<(&str, f32)> = files
            .iter()
            .enumerate()
            .map(|(i, fp)| {
                let s: f32 = docs[i].iter().map(|(k, v)| qmap.get(k).copied().unwrap_or(0.0) * v).sum();
                (fp.as_str(), s)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let rank = scored.iter().position(|(fp, _)| fp.contains(answer));
        let top: Vec<&str> = scored.iter().take(3).map(|(fp, _)| fp.rsplit('/').next().unwrap_or(fp)).collect();
        println!("run{run}: sparse-only answer rank #{rank:?} / {} files; top3={top:?}", files.len());
    }
    Ok(())
}
