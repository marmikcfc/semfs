//! Standalone EmbeddingGemma-300M recall probe (ticket: embedder-upgrade-gemma-qwen3).
//! Pulls the answer chunk + the distractors that currently outrank it from a seed DB,
//! embeds codex's verbatim queries with fastembed's EmbeddingGemma300M, and reports
//! where the answer ranks — with and without Gemma's retrieval prompts.
//!
//! Run on EC2:  cargo run --release -p semfs-core --example gemma_probe -- ~/.semfs/chanpin-e5-nosum.db

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use rusqlite::{Connection, OpenFlags};

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb + 1e-8)
}

fn main() -> anyhow::Result<()> {
    let db = std::env::args().nth(1).expect("usage: gemma_probe <db>");
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    // (label, filepath LIKE pattern) — ANSWER + the EN-query top-10 distractors + the 403 file.
    let patterns: &[(&str, &str)] = &[
        ("ANSWER.txt", "%best_selling_product_core_data_list%"),
        ("DASHBOARD.xlsx", "%6-product-sales-analysis-dashboard%"),
        ("d:taobao_summary", "%_activity_summary_taobaoonsite%"),
        ("d:sales_perf.json", "%02_sales_performance%"),
        ("d:taobao_followup", "%_activity_taobaoactivity_followup%"),
        ("d:taobao_daily", "%_activity_daily_special_tao%"),
        ("d:user_journey.pdf", "%user_journey_map_core_usage%"),
        ("d:metrics_dash.py", "%scripts/metrics_dashboard.py%"),
        ("d:top10_403html", "%top10_product_status_table%"),
        ("d:roster.json", "%03_team_roster%"),
    ];

    let mut docs: Vec<(String, String)> = Vec::new();
    for (label, pat) in patterns {
        let txt: Option<String> = conn
            .query_row(
                "SELECT text FROM chunks WHERE filepath LIKE ?1 ORDER BY length(text) DESC LIMIT 1",
                [pat],
                |r| r.get(0),
            )
            .ok();
        match txt {
            Some(t) => docs.push((label.to_string(), t)),
            None => println!("(no chunk for {label})"),
        }
    }
    println!("pulled {} docs from {db}\n", docs.len());

    let mut model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::EmbeddingGemma300M).with_show_download_progress(true),
    )?;

    let queries: &[(&str, &str)] = &[
        (
            "CLOUD",
            "best-selling product data file top10 product title transaction amount conversion rate",
        ),
        (
            "LOCAL",
            "best-selling product data file title transaction amount conversion rate",
        ),
    ];

    for use_prompt in [false, true] {
        println!("================ Gemma retrieval prompts: {use_prompt} ================");
        let doc_texts: Vec<String> = docs
            .iter()
            .map(|(_, t)| {
                if use_prompt {
                    format!("title: none | text: {t}")
                } else {
                    t.clone()
                }
            })
            .collect();
        let doc_emb = model.embed(&doc_texts, None)?;

        for (qlabel, q) in queries {
            let qtext = if use_prompt {
                format!("task: search result | query: {q}")
            } else {
                (*q).to_string()
            };
            let qe = model.embed(&[qtext], None)?.remove(0);
            let mut scored: Vec<(f32, &str)> = docs
                .iter()
                .enumerate()
                .map(|(i, (l, _))| (cosine(&qe, &doc_emb[i]), l.as_str()))
                .collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            let arank = scored
                .iter()
                .position(|(_, l)| *l == "ANSWER.txt")
                .map(|p| p + 1);
            let drank = scored
                .iter()
                .position(|(_, l)| *l == "DASHBOARD.xlsx")
                .map(|p| p + 1);
            println!(
                "\n[{qlabel}] answer.txt rank={arank:?}  dashboard.xlsx rank={drank:?}  (of {})",
                docs.len()
            );
            for (i, (s, l)) in scored.iter().enumerate() {
                let mark = if *l == "ANSWER.txt" || *l == "DASHBOARD.xlsx" {
                    " <=="
                } else {
                    ""
                };
                println!("   {:>2}. {:<20} {:.4}{}", i + 1, l, s, mark);
            }
        }
    }
    Ok(())
}
