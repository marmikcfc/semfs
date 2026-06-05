//! Full-corpus EmbeddingGemma-300M recall test (ticket: embedder-upgrade-gemma-qwen3).
//! Embeds EVERY chunk of a seed DB with Gemma, then KNN-ranks codex's verbatim queries
//! over the whole corpus (file-level max-pool) — the faithful "does Gemma retrieve the
//! answer" gate, no prompts (matches semfs's current registry path).
//!
//! Run: cargo run --release -p semfs-core --example gemma_corpus -- ~/.semfs/chanpin-e5-nosum.db

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb + 1e-8)
}

fn main() -> anyhow::Result<()> {
    let db = std::env::args().nth(1).expect("usage: gemma_corpus <db>");
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    let mut stmt = conn.prepare("SELECT filepath, text FROM chunks")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|x| x.ok())
        .collect();
    println!("loaded {} chunks from {db}", rows.len());

    // arg 2 = model: "gemma" (default) | "e5"  — lets us measure the answer's true
    // full-corpus vector rank per embedder (decides whether a bigger KNN k helps).
    let which = std::env::args().nth(2).unwrap_or_else(|| "gemma".to_string());
    let em = match which.as_str() {
        "e5" => EmbeddingModel::MultilingualE5Small,
        _ => EmbeddingModel::EmbeddingGemma300M,
    };
    println!("embedder: {em:?}");
    let mut model =
        TextEmbedding::try_new(InitOptions::new(em).with_show_download_progress(false))?;

    // Embed all chunk texts (truncate very long chunks; gemma clamps anyway).
    let texts: Vec<String> = rows
        .iter()
        .map(|(_, t)| t.chars().take(4000).collect::<String>())
        .collect();
    let mut emb: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
    let bs = 64usize;
    let mut done = 0;
    for window in texts.chunks(bs) {
        let v = model.embed(window, None)?;
        emb.extend(v);
        done += window.len();
        if done % 640 == 0 || done == texts.len() {
            println!("  embedded {done}/{}", texts.len());
        }
    }

    let answer = "best_selling_product_core_data_list";
    let dash = "6-product-sales-analysis-dashboard";
    let queries: &[(&str, &str)] = &[
        ("CLOUD", "best-selling product data file top10 product title transaction amount conversion rate"),
        ("LOCAL", "best-selling product data file title transaction amount conversion rate"),
        ("LOCAL2", "best-selling product data file"),
        ("LOCAL4", "best selling product"),
    ];

    for (qlabel, q) in queries {
        let qe = model.embed(&[(*q).to_string()], None)?.remove(0);
        // file-level max-pool over chunk cosines
        let mut best: HashMap<&str, f32> = HashMap::new();
        for (i, (fp, _)) in rows.iter().enumerate() {
            let s = cosine(&qe, &emb[i]);
            let e = best.entry(fp.as_str()).or_insert(f32::MIN);
            if s > *e {
                *e = s;
            }
        }
        let mut ranked: Vec<(&str, f32)> = best.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        let arank = ranked.iter().position(|(fp, _)| fp.contains(answer)).map(|p| p + 1);
        let drank = ranked.iter().position(|(fp, _)| fp.contains(dash)).map(|p| p + 1);
        println!(
            "\n[{qlabel}] answer.txt rank = {arank:?} / {} files   dashboard.xlsx rank = {drank:?}",
            ranked.len()
        );
        for (i, (fp, s)) in ranked.iter().take(8).enumerate() {
            let short = fp.rsplit('/').next().unwrap_or(fp);
            let mark = if fp.contains(answer) || fp.contains(dash) { " <==" } else { "" };
            println!("   {:>2}. {:.4}  {}{}", i + 1, s, short, mark);
        }
    }
    Ok(())
}
