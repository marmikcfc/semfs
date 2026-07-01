//! Materialize the POSIX file tree (`fs_inode`/`fs_dentry`/`fs_data`) into an
//! existing seed from its corpus, with FULL content and WITHOUT re-embedding — the
//! offline builder that makes a `seed_dir`-built seed mountable. `seed_dir`/the
//! `index()` engine only write `chunks`/`vchunks` (search), never the tree, so a
//! seed built without this is search-only (empty `ls`, no `cat`). Run AFTER
//! `seed_dir` (+ `build_kg`); it touches only the `fs_*` tables.
//!
//! Walk rules mirror `seed_dir` exactly (skip hidden / node_modules / target /
//! __pycache__) so the materialized tree == the indexed file set.
//!
//! Run: cargo run --release -p semfs-core --example materialize_fs -- <seed.db> <corpus_dir>

use std::path::{Path, PathBuf};

use semfs_core::cache::Db;

/// Recursively collect files under `root`, skipping VCS/hidden dirs — identical to
/// `seed_dir::walk` so the file set matches what was indexed.
fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
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

fn main() -> anyhow::Result<()> {
    let db_path = std::env::args()
        .nth(1)
        .expect("usage: materialize_fs <seed.db> <corpus_dir>");
    let corpus = std::env::args()
        .nth(2)
        .expect("usage: materialize_fs <seed.db> <corpus_dir>");
    let corpus = std::fs::canonicalize(&corpus)?;

    let db = Db::open(Path::new(&db_path))?;
    let mut files = Vec::new();
    walk(&corpus, &corpus, &mut files);
    files.sort();
    // SEMFS_FS_RESUME=1: skip files already in the materialized tree → CONVERGE across Modal
    // worker preemptions (which otherwise restart this from scratch). Mirrors embed/KG resume.
    let resume = std::env::var("SEMFS_FS_RESUME").ok().as_deref() == Some("1");
    let done: std::collections::HashSet<String> = if resume {
        db.materialized_file_paths()
    } else {
        std::collections::HashSet::new()
    };
    eprintln!(
        "materializing fs_* for {} files from {} → {db_path} (resume={resume}, already_done={})",
        files.len(),
        corpus.display(),
        done.len()
    );

    let (mut ok, mut err, mut skipped) = (0usize, 0usize, 0usize);
    for (i, path) in files.iter().enumerate() {
        let rel = path.strip_prefix(&corpus).unwrap_or(path);
        let vpath = format!("/{}", rel.to_string_lossy());
        if resume && done.contains(&vpath) {
            skipped += 1;
        } else {
            match std::fs::read(path) {
                Ok(bytes) => match db.materialize_file(&vpath, &bytes) {
                    Ok(_) => ok += 1,
                    Err(e) => {
                        err += 1;
                        eprintln!("fs materialize failed {vpath}: {e}");
                    }
                },
                Err(e) => {
                    err += 1;
                    eprintln!("read failed {vpath}: {e}");
                }
            }
        }
        if (i + 1) % 200 == 0 {
            // stderr is unbuffered → streams live into Modal logs (the tight leash).
            eprintln!(
                "  progress {}/{} (materialized={ok} skipped={skipped} err={err})",
                i + 1,
                files.len()
            );
        }
    }
    eprintln!(
        "fs_* done: {ok} materialized, {skipped} skipped (resume), {err} errors (of {} files)",
        files.len()
    );
    Ok(())
}
