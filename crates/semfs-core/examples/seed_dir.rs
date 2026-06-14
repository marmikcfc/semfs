//! Build a semfs seed DB from a raw corpus directory — the dir→seed indexer
//! that the mount daemon does online, run offline (no cloud). For each file it
//! extracts text (`extract::extract_text`) and indexes it through the REAL
//! `SqliteVecStore::index()` engine (chunk + embed + vec0 write), producing the
//! same `chunks`/`vchunks` a mounted+synced container would. Used to build the
//! gemma-q4 `kaifa` seed before the dual-lane KG build (`build_kg`).
//!
//! Embedder selection mirrors the `semfs` binary's `build_embedder`:
//!   SEMFS_EMBED_MODEL=gemma-q4  + SEMFS_EMBED_ONNX_DIR=<dir>  → BYO Q4 gemma ONNX
//!   (anything else)                                          → registry gemma fp32
//!
//! Run: SEMFS_EMBED_MODEL=gemma-q4 SEMFS_EMBED_ONNX_DIR=/data/models/gemma_q4 \
//!        cargo run --release -p semfs-core --example seed_dir -- <out.db> <corpus_dir>

use std::path::{Path, PathBuf};
use std::sync::Arc;

use semfs_core::backend::SqliteVecStore;
use semfs_core::cache::Db;
use semfs_core::embed::{Embedder, EmbeddingModel, LocalEmbedder};
use semfs_core::extract::extract_text;

/// Recursively collect files under `root`, skipping VCS/hidden dirs.
fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue; // .git, .DS_Store, dotfiles
        }
        if p.is_dir() {
            if name == "node_modules" || name == "target" || name == "__pycache__" {
                continue;
            }
            walk(root, &p, out);
        } else if p.is_file() {
            out.push(p);
        }
    }
}

fn build_embedder() -> anyhow::Result<Arc<dyn Embedder>> {
    if std::env::var("SEMFS_EMBED_MODEL").as_deref() == Ok("gemma-q4") {
        let dir = std::env::var("SEMFS_EMBED_ONNX_DIR")
            .unwrap_or_else(|_| format!("{}/gemma_q4", std::env::var("HOME").unwrap_or_default()));
        println!("embedder: BYO gemma-q4 ONNX @ {dir}");
        Ok(Arc::new(LocalEmbedder::from_onnx_dir(
            Path::new(&dir),
            768,
            "model_q4",
            "gemma-q4-onnx",
        )?))
    } else {
        println!("embedder: registry EmbeddingGemma-300M (fp32)");
        Ok(Arc::new(LocalEmbedder::from_registry(EmbeddingModel::EmbeddingGemma300M, None)?))
    }
}

fn main() -> anyhow::Result<()> {
    let db_path = std::env::args().nth(1).expect("usage: seed_dir <out.db> <corpus_dir>");
    let corpus = std::env::args().nth(2).expect("usage: seed_dir <out.db> <corpus_dir>");
    let corpus = std::fs::canonicalize(&corpus)?;

    let embedder = build_embedder()?;
    let db = Arc::new(Db::open(Path::new(&db_path))?);
    let store = SqliteVecStore::new(db, embedder)?;

    let mut files = Vec::new();
    walk(&corpus, &corpus, &mut files);
    files.sort();
    println!("indexing {} files from {} → {db_path}", files.len(), corpus.display());

    let rt = tokio::runtime::Runtime::new()?;
    let (mut ok, mut empty, mut err) = (0usize, 0usize, 0usize);
    for (i, path) in files.iter().enumerate() {
        let rel = path.strip_prefix(&corpus).unwrap_or(path);
        let vpath = format!("/{}", rel.to_string_lossy());
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                err += 1;
                continue;
            }
        };
        match rt.block_on(extract_text(&vpath, &bytes)) {
            Some(text) if !text.trim().is_empty() => {
                if store.index(i as u64 + 1, &vpath, &text).is_ok() {
                    ok += 1;
                } else {
                    err += 1;
                }
            }
            _ => empty += 1, // unsupported / no text layer (e.g. image, scanned pdf)
        }
        if (i + 1) % 100 == 0 {
            println!("  {}/{} ({ok} indexed)", i + 1, files.len());
        }
    }
    println!("seed done: {ok} indexed, {empty} no-text, {err} errors (of {} files)", files.len());
    Ok(())
}
