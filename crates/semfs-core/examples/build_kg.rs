//! Rebuild the knowledge graph FROM SCRATCH over an existing seed DB — graphify
//! parity. **Dual lane**: source-code files go through the deterministic, local,
//! free tree-sitter AST lane (`graph_ast`); every other file (docs/pdf/images)
//! goes through the production LLM `extract_graph`. Both write the same tables:
//!   - `graph_entity`  : node (path, name, kind, file_type, source_file)
//!   - `edges`         : file→entity co-mention (feeds communities/god-nodes)
//!   - `graph_relation`: entity→entity typed edge (the graphify relationship graph)
//!
//! The AST lane needs the file's FULL source, which the overlap-aware `chunks`
//! table cannot reconstruct — so it reads from `<corpus_dir>` on disk. Pass the
//! corpus dir as the 2nd arg to enable it; without it, EVERYTHING uses the LLM
//! lane (back-compat for the all-docs chanpin path).
//!
//! It WIPES the old graph tables first (idempotent rebuild) and leaves the
//! vector/chunk index untouched. The LLM lane is concurrent (8 network-bound
//! workers); the AST lane is in-process and CPU-bound (no API latency).
//!
//! Run: OPENROUTER_API_KEY=... cargo run --release -p semfs-core \
//!        --example build_kg -- /path/to/seed.db [/path/to/corpus_dir]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use std::path::Path;

use rusqlite::{params, Connection};
use semfs_core::backend::graph::{entity_path, extract_graph, GraphExtraction};
use semfs_core::backend::graph_ast;
use semfs_core::llm::LlmClient;

/// graphify `file_type` from the path extension.
fn file_type_of(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "py" | "ts" | "tsx" | "js" | "jsx" | "go" | "rs" | "java" | "c" | "cpp" | "h" | "rb"
        | "php" | "swift" | "kt" | "scala" | "sh" => "code",
        "pdf" => "paper",
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg" => "image",
        _ => "document",
    }
}

fn main() -> anyhow::Result<()> {
    let db = std::env::args().nth(1).expect("usage: build_kg <db> [corpus_dir]");
    // Optional: only the LLM doc lane needs it. A pure-code corpus runs without it.
    let key = std::env::var("OPENROUTER_API_KEY").ok().filter(|k| !k.trim().is_empty());

    let conn = Connection::open(&db)?;
    // Schema migration (seed predates the graphify-parity columns/table).
    let _ = conn.execute("ALTER TABLE edges ADD COLUMN confidence TEXT NOT NULL DEFAULT 'INFERRED'", []);
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS graph_entity(path TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);",
    )?;
    for col in ["file_type TEXT", "source_file TEXT", "rationale TEXT"] {
        let _ = conn.execute(&format!("ALTER TABLE graph_entity ADD COLUMN {col}"), []);
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS graph_relation(
            source TEXT NOT NULL, target TEXT NOT NULL, relation TEXT NOT NULL,
            confidence TEXT NOT NULL DEFAULT 'INFERRED', confidence_score REAL NOT NULL DEFAULT 0.5,
            source_file TEXT, source_location TEXT, weight REAL NOT NULL DEFAULT 1.0,
            created_at INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (source, target, relation));
         CREATE INDEX IF NOT EXISTS idx_graph_relation_src ON graph_relation(source);
         CREATE INDEX IF NOT EXISTS idx_graph_relation_tgt ON graph_relation(target);",
    )?;

    // Rebuild FROM SCRATCH: drop the old graph (keep chunks/vectors).
    conn.execute_batch("DELETE FROM edges; DELETE FROM graph_entity; DELETE FROM graph_relation;")?;

    // One concatenated text blob per file (cap to keep extraction fast).
    let files: Vec<(String, String)> = {
        let mut stmt = conn.prepare("SELECT filepath, text FROM chunks ORDER BY filepath, id")?;
        let mut acc: BTreeMap<String, String> = Default::default();
        for r in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (fp, t) = r?;
            let e = acc.entry(fp).or_default();
            if e.chars().count() < 6000 {
                e.push('\n');
                e.push_str(&t);
            }
        }
        acc.into_iter().map(|(fp, t)| (fp, t.chars().take(6000).collect())).collect()
    };
    // Smoke mode: `build_kg <db> smoke` — process the first 5 files sequentially
    // with errors + per-file counts visible, to verify extraction before a full run.
    if std::env::args().nth(2).as_deref() == Some("smoke") {
        let key = key.clone().expect("OPENROUTER_API_KEY required for smoke");
        let client = LlmClient::openrouter(key);
        for (fp, text) in files.iter().take(5) {
            match extract_graph(&client, text) {
                Ok(g) => println!("OK {fp}: {} entities, {} relations", g.entities.len(), g.relations.len()),
                Err(e) => println!("ERR {fp}: {e}"),
            }
        }
        return Ok(());
    }

    // ── Dual-lane partition ──────────────────────────────────────────────
    // With a corpus dir, source-code files go through the deterministic AST
    // lane (full source read from disk); everything else stays on the LLM lane.
    let corpus_dir = std::env::args()
        .nth(2)
        .filter(|s| s != "smoke" && Path::new(s).is_dir());
    let mut ast_inputs: Vec<(String, String)> = Vec::new(); // (filepath, FULL source)
    let mut llm_files: Vec<(String, String)> = Vec::new(); // (filepath, capped chunk blob)
    let mut unresolved = 0usize;
    for (fp, capped) in files {
        if let Some(dir) = &corpus_dir {
            if graph_ast::Lang::from_path(&fp).is_some() {
                let disk = Path::new(dir).join(fp.trim_start_matches('/'));
                match std::fs::read_to_string(&disk) {
                    Ok(src) => {
                        ast_inputs.push((fp, src));
                        continue;
                    }
                    Err(_) => unresolved += 1, // code file not on disk → fall to LLM lane
                }
            }
        }
        llm_files.push((fp, capped));
    }
    if corpus_dir.is_some() {
        println!(
            "AST code lane: {} files (corpus {}); LLM doc lane: {} files{}",
            ast_inputs.len(),
            corpus_dir.as_deref().unwrap_or(""),
            llm_files.len(),
            if unresolved > 0 { format!("; {unresolved} code files not found on disk → LLM lane") } else { String::new() },
        );
    }

    // AST lane: parse every code file, then one global cross-file resolve pass.
    let ast_files: Vec<graph_ast::FileAst> = ast_inputs
        .iter()
        .filter_map(|(fp, src)| graph_ast::parse_file(fp, src))
        .collect();
    let code_relations = graph_ast::resolve(&ast_files);

    if !llm_files.is_empty() && key.is_none() {
        anyhow::bail!("OPENROUTER_API_KEY required: {} non-code files need the LLM lane", llm_files.len());
    }
    println!("rebuilding KG: {} code files (AST) + {} doc files (LLM, 8 workers)…", ast_files.len(), llm_files.len());

    let key = key.unwrap_or_default();
    let files = Arc::new(llm_files);
    let cursor = Arc::new(AtomicUsize::new(0));
    let out: Arc<Mutex<Vec<(String, GraphExtraction)>>> = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let (files, cursor, out, done, key) =
            (files.clone(), cursor.clone(), out.clone(), done.clone(), key.clone());
        handles.push(std::thread::spawn(move || {
            let client = LlmClient::openrouter(key);
            loop {
                let i = cursor.fetch_add(1, Ordering::SeqCst);
                if i >= files.len() {
                    break;
                }
                let (fp, text) = &files[i];
                if let Ok(g) = extract_graph(&client, text) {
                    if !g.entities.is_empty() || !g.relations.is_empty() {
                        out.lock().unwrap().push((fp.clone(), g));
                    }
                }
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                if d % 50 == 0 {
                    println!("  {d}/{} files", files.len());
                }
            }
        }));
    }
    for h in handles {
        h.join().ok();
    }

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    let results = out.lock().unwrap();
    let tx = conn.unchecked_transaction()?;
    let (mut n_ent, mut n_edge, mut n_rel) = (0usize, 0usize, 0usize);
    for (fp, g) in results.iter() {
        let ft = file_type_of(fp);
        for e in &g.entities {
            let node = entity_path(&e.name);
            tx.execute(
                "INSERT OR IGNORE INTO edges(from_path,to_path,edge_kind,created_at,confidence) \
                 VALUES (?1,?2,?3,?4,'EXTRACTED')",
                params![fp, node, e.kind, now],
            )?;
            tx.execute(
                "INSERT INTO graph_entity(path,name,kind,file_type,source_file) VALUES (?1,?2,?3,?4,?5) \
                 ON CONFLICT(path) DO UPDATE SET name=excluded.name, kind=excluded.kind, \
                   file_type=COALESCE(graph_entity.file_type, excluded.file_type), \
                   source_file=COALESCE(graph_entity.source_file, excluded.source_file)",
                params![node, e.name, e.kind, ft, fp],
            )?;
            n_ent += 1;
            n_edge += 1;
        }
        for r in &g.relations {
            let (s, t) = (entity_path(&r.source), entity_path(&r.target));
            tx.execute(
                "INSERT OR IGNORE INTO graph_relation\
                 (source,target,relation,confidence,confidence_score,source_file,weight,created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    s, t, r.relation, r.confidence, r.confidence_score, fp,
                    if r.confidence == "EXTRACTED" { 1.0 } else { 0.6 }, now
                ],
            )?;
            n_rel += 1;
        }
    }

    // ── AST code-lane writes ─────────────────────────────────────────────
    // Code entity node key = its module-qualified name (distinct namespace from
    // the doc lane's `/memories/<slug>.md`). Relations carry source_location +
    // real EXTRACTED/INFERRED confidence + weight (closes the comparison-doc gap).
    let (mut n_code_ent, mut n_code_rel) = (0usize, 0usize);
    for f in &ast_files {
        for e in &f.entities {
            tx.execute(
                "INSERT OR IGNORE INTO edges(from_path,to_path,edge_kind,created_at,confidence) \
                 VALUES (?1,?2,?3,?4,'EXTRACTED')",
                params![f.path, e.qualified, e.kind.as_str(), now],
            )?;
            tx.execute(
                "INSERT INTO graph_entity(path,name,kind,file_type,source_file) VALUES (?1,?2,?3,'code',?4) \
                 ON CONFLICT(path) DO UPDATE SET name=excluded.name, kind=excluded.kind, \
                   file_type='code', source_file=COALESCE(graph_entity.source_file, excluded.source_file)",
                params![e.qualified, e.name, e.kind.as_str(), f.path],
            )?;
            n_code_ent += 1;
            n_edge += 1;
        }
    }
    for r in &code_relations {
        tx.execute(
            "INSERT OR IGNORE INTO graph_relation\
             (source,target,relation,confidence,confidence_score,source_file,source_location,weight,created_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                r.source, r.target, r.relation, r.confidence, r.confidence_score,
                r.source_file, r.source_location, r.weight, now
            ],
        )?;
        n_code_rel += 1;
    }
    n_ent += n_code_ent;
    n_rel += n_code_rel;

    tx.commit()?;
    let distinct_ent: i64 = conn.query_row("SELECT COUNT(*) FROM graph_entity", [], |r| r.get(0))?;
    let distinct_rel: i64 = conn.query_row("SELECT COUNT(*) FROM graph_relation", [], |r| r.get(0))?;
    println!(
        "done: {} doc files (LLM) + {} code files (AST); {n_ent} entities \
         ({n_code_ent} code), {n_edge} file→entity edges, {n_rel} relations \
         ({n_code_rel} code) | distinct entities={distinct_ent}, distinct relations={distinct_rel}",
        results.len(),
        ast_files.len(),
    );
    Ok(())
}
