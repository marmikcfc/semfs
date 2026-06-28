//! Sanity-gate a bring-your-own-ONNX embedder (e.g. gemma-q4) BEFORE seeding a
//! large corpus: embed a known triplet and assert the related pair is far more
//! similar than the unrelated one. Wrong pooling/quantization/output_key produce
//! garbage embeddings that pass silently — this catches them in seconds.
//!
//! Run: SEMFS_EMBED_ONNX_DIR=$HOME/gemma_q4 [SEMFS_EMBED_ONNX_BASE=model_q4] \
//!        cargo run --release -p semfs-core --example embed_probe
//! Exit 0 = sane, 3 = suspect (not discriminating).

use semfs_core::embed::{Embedder, LocalEmbedder};
use std::path::Path;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn main() -> anyhow::Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = std::env::var("SEMFS_EMBED_ONNX_DIR").unwrap_or_else(|_| format!("{home}/gemma_q4"));
    let base = std::env::var("SEMFS_EMBED_ONNX_BASE").unwrap_or_else(|_| "model_q4".to_string());
    eprintln!("loading BYO-ONNX embedder from {dir} (base={base})...");
    let e = LocalEmbedder::from_onnx_dir(Path::new(&dir), 768, &base, "gemma-q4-onnx")?;
    eprintln!("identity={} dims={}", e.identity(), e.dimensions());

    let texts = vec![
        "how do I reset my account password".to_string(),
        "to reset your password click forgot password and follow the email link".to_string(),
        "bananas are a good source of potassium and dietary fiber".to_string(),
    ];
    let v = e.embed(&texts)?;
    if v.iter().any(|x| x.len() != 768) {
        anyhow::bail!(
            "unexpected vector dims: {:?}",
            v.iter().map(|x| x.len()).collect::<Vec<_>>()
        );
    }
    let related = cosine(&v[0], &v[1]);
    let unrelated = cosine(&v[0], &v[2]);
    println!("cosine(password~password)={related:.4}  cosine(password~bananas)={unrelated:.4}");
    if related > unrelated && related > 0.3 {
        println!("SANE: q4 embeddings discriminate (related ≫ unrelated). OK to seed.");
        Ok(())
    } else {
        println!(
            "SUSPECT: embeddings do NOT discriminate — try SEMFS_EMBED_ONNX_BASE=model_q4f16, \
                  or a different QuantizationMode/output_key. DO NOT seed with this."
        );
        std::process::exit(3);
    }
}
