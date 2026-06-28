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

use rusqlite::OpenFlags;
use semfs_core::backend::SqliteVecStore;
use semfs_core::cache::Db;
use semfs_core::embed::{Embedder, EmbeddingModel, LocalEmbedder};
use semfs_core::extract::extract_text;

/// Recursively collect files under `root`, skipping VCS/hidden dirs.
fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
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

/// CODE-lane embedder. `SEMFS_CODE_EMBED_MODEL=gemma-q4` → BYO gemma-q4 ONNX with
/// a DISTINCT identity (`gemma-q4-onnx-code`) so the seed gets a real code lane
/// (`vchunks_code`) uniform with the text lane. Unset ⇒ `None` (no code lane,
/// backward-compatible with the old text-only `seed_dir`). Mirrors the daemon's
/// `resolve::build_code_embedder`.
fn build_code_embedder() -> anyhow::Result<Option<Arc<dyn Embedder>>> {
    if std::env::var("SEMFS_CODE_EMBED_MODEL").as_deref() == Ok("gemma-q4") {
        let dir = std::env::var("SEMFS_CODE_EMBED_ONNX_DIR")
            .or_else(|_| std::env::var("SEMFS_EMBED_ONNX_DIR"))
            .unwrap_or_else(|_| format!("{}/gemma_q4", std::env::var("HOME").unwrap_or_default()));
        println!("code embedder: BYO gemma-q4 ONNX @ {dir} (lane=gemma-q4-onnx-code)");
        Ok(Some(Arc::new(LocalEmbedder::from_onnx_dir(
            Path::new(&dir),
            768,
            "model_q4",
            "gemma-q4-onnx-code",
        )?)))
    } else {
        Ok(None)
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
        Ok(Arc::new(LocalEmbedder::from_registry(
            EmbeddingModel::EmbeddingGemma300M,
            None,
        )?))
    }
}

/// Append one failure/skip record to the ledger (no-op if no ledger is open).
fn log_fail(
    ledger: &mut Option<std::fs::File>,
    filepath: &str,
    stage: &str,
    reason: &str,
    bytes: usize,
) {
    if let Some(w) = ledger {
        use std::io::Write;
        let _ = writeln!(
            w,
            "{}",
            serde_json::json!({
                "filepath": filepath, "stage": stage, "reason": reason, "bytes": bytes
            })
        );
    }
}

fn main() -> anyhow::Result<()> {
    let db_path = std::env::args()
        .nth(1)
        .expect("usage: seed_dir <out.db> <corpus_dir>");
    let corpus = std::env::args()
        .nth(2)
        .expect("usage: seed_dir <out.db> <corpus_dir>");
    let corpus = std::fs::canonicalize(&corpus)?;

    // SEMFS_SHARD="k/N": process only files whose GLOBAL sorted index i satisfies
    // i % N == k. The ino is the global index (i+1) on every shard, so inos stay
    // unique across shards and the partial DBs merge without ino collisions. All
    // shards walk+sort the SAME corpus, so the partition is deterministic.
    let (shard_k, shard_n) = std::env::var("SEMFS_SHARD")
        .ok()
        .and_then(|s| {
            let mut it = s.split('/');
            let k: usize = it.next()?.parse().ok()?;
            let n: usize = it.next()?.parse().ok()?;
            Some((k, n))
        })
        .unwrap_or((0, 1));
    assert!(
        shard_n >= 1 && shard_k < shard_n,
        "SEMFS_SHARD must be k/N with 0 <= k < N (got {shard_k}/{shard_n})"
    );
    if shard_n > 1 {
        println!("shard {shard_k}/{shard_n}: indexing files where global_index % {shard_n} == {shard_k}");
    }

    // Resume: skip files already indexed. Two sources, both read-only (WAL-safe):
    //   SEMFS_RESUME_DB=<master.db> — skip files already in a SEPARATE master DB
    //     (used by shard workers: they write a partial but skip the master's done
    //      files, so prior single-worker progress is preserved).
    //   SEMFS_SEED_RESUME=1 — skip files already in THIS out_db (preemption restart).
    // When both apply, a file is skipped if present in EITHER.
    let ro = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let master_conn = std::env::var("SEMFS_RESUME_DB")
        .ok()
        .filter(|p| Path::new(p).exists())
        .and_then(|p| {
            println!("resume: skipping files already in master {p}");
            rusqlite::Connection::open_with_flags(&p, ro).ok()
        });
    let self_conn = if std::env::var("SEMFS_SEED_RESUME").as_deref() == Ok("1")
        && Path::new(&db_path).exists()
    {
        println!("resume: skipping files already in {db_path}");
        rusqlite::Connection::open_with_flags(&db_path, ro).ok()
    } else {
        None
    };
    let already_indexed = |vpath: &str| -> bool {
        let hit = |c: &rusqlite::Connection| {
            c.query_row(
                "SELECT 1 FROM chunks WHERE filepath = ?1 LIMIT 1",
                rusqlite::params![vpath],
                |_| Ok(true),
            )
            .unwrap_or(false)
        };
        master_conn.as_ref().is_some_and(hit) || self_conn.as_ref().is_some_and(hit)
    };

    let embedder = build_embedder()?;
    let db = Arc::new(Db::open(Path::new(&db_path))?);
    let mut store = SqliteVecStore::new(db, embedder)?;
    // Attach the CODE lane so code files index into `vchunks_code` (gemma) instead
    // of the text lane — the piece the old text-only seed_dir build was missing.
    if let Some(code) = build_code_embedder()? {
        store.enable_code_indexing(code)?;
        println!("code lane enabled (vchunks_code)");
    }

    let mut files = Vec::new();
    walk(&corpus, &corpus, &mut files);
    files.sort();
    println!(
        "indexing {} files from {} → {db_path}",
        files.len(),
        corpus.display()
    );

    // Failure ledger (SEM-38 R7): every file that fails or doesn't get embedded gets
    // a jsonl line {filepath, stage, reason, bytes}. Per-shard path (no interleaving)
    // from SEMFS_EMBED_LEDGER; unset ⇒ no ledger. Reconcile across shards after merge.
    let mut ledger = std::env::var("SEMFS_EMBED_LEDGER").ok().and_then(|p| {
        let p = if shard_n > 1 {
            format!("{p}.shard{shard_k}of{shard_n}")
        } else {
            p
        };
        std::fs::File::create(&p).ok()
    });

    let rt = tokio::runtime::Runtime::new()?;
    let (mut ok, mut empty, mut err, mut skipped) = (0usize, 0usize, 0usize, 0usize);
    for (i, path) in files.iter().enumerate() {
        // Shard filter: this worker owns only files at i % N == k. ino stays = i+1
        // (the global index), so it is unique across shards.
        if i % shard_n != shard_k {
            continue;
        }
        let rel = path.strip_prefix(&corpus).unwrap_or(path);
        let vpath = format!("/{}", rel.to_string_lossy());

        // Resume: skip files already indexed in the master and/or this DB.
        if already_indexed(&vpath) {
            skipped += 1;
            ok += 1;
            if (i + 1) % 1000 == 0 {
                println!("  {}/{} ({ok} indexed, {skipped} skipped)", i + 1, files.len());
            }
            continue;
        }

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                err += 1;
                log_fail(&mut ledger, &vpath, "read", &e.to_string(), 0);
                continue;
            }
        };
        // extract_text handles only BINARY doc formats (docx/pptx/xlsx/pdf/img)
        // and returns None for plain-text/code (sniffed as Unknown). The mount
        // daemon indexes those raw, so we do too: fall back to raw UTF-8 — this
        // is the path that covers .py/.java/.ts/.go/.md/.json/.yaml/… code+docs.
        let text = match rt.block_on(extract_text(&vpath, &bytes)) {
            Some(t) if !t.trim().is_empty() => Some(t),
            _ => String::from_utf8(bytes.clone())
                .ok()
                .filter(|s| !s.trim().is_empty()),
        };
        match text {
            Some(text) => {
                if store.index(i as u64 + 1, &vpath, &text).is_ok() {
                    ok += 1;
                } else {
                    err += 1;
                    log_fail(&mut ledger, &vpath, "embed", "index_failed", bytes.len());
                }
            }
            None => {
                empty += 1; // no extractable text (binary w/o extractor, empty file)
                log_fail(&mut ledger, &vpath, "extract", "no_text", bytes.len());
            }
        }
        if (i + 1) % 100 == 0 {
            println!("  {}/{} ({ok} indexed, {skipped} skipped)", i + 1, files.len());
        }
    }
    println!(
        "seed done: {ok} indexed ({skipped} skipped/resumed), {empty} no-text, {err} errors (of {} files)",
        files.len()
    );
    Ok(())
}
