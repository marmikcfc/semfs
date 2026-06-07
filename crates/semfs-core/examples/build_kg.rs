//! Rebuild the knowledge graph FROM SCRATCH over an existing seed DB — graphify
//! parity. For each file it runs the production `extract_graph` (entities +
//! TYPED entity→entity relations with confidence) and writes three things:
//!   - `graph_entity`  : node (path, name, kind, file_type, source_file)
//!   - `edges`         : file→entity co-mention (feeds communities/god-nodes)
//!   - `graph_relation`: entity→entity typed edge (the graphify relationship graph)
//!
//! It WIPES the old graph tables first (idempotent rebuild) and leaves the
//! vector/chunk index untouched. Concurrent (8 workers; each call is network-bound).
//!
//! Run: OPENROUTER_API_KEY=... cargo run --release -p semfs-core \
//!        --example build_kg -- /path/to/seed.db

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use semfs_core::backend::graph::{entity_path, extract_graph, GraphExtraction};
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
    let db = std::env::args().nth(1).expect("usage: build_kg <db>");
    let key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .expect("OPENROUTER_API_KEY required");

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
        let client = LlmClient::openrouter(key.clone());
        for (fp, text) in files.iter().take(5) {
            match extract_graph(&client, text) {
                Ok(g) => println!("OK {fp}: {} entities, {} relations", g.entities.len(), g.relations.len()),
                Err(e) => println!("ERR {fp}: {e}"),
            }
        }
        return Ok(());
    }
    println!("rebuilding KG for {} files (8 workers)…", files.len());

    let files = Arc::new(files);
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
    tx.commit()?;
    let distinct_ent: i64 = conn.query_row("SELECT COUNT(*) FROM graph_entity", [], |r| r.get(0))?;
    let distinct_rel: i64 = conn.query_row("SELECT COUNT(*) FROM graph_relation", [], |r| r.get(0))?;
    println!(
        "done: {} files w/ graph; {n_ent} entity-mentions, {n_edge} file→entity edges, \
         {n_rel} entity→entity relations | distinct entities={distinct_ent}, distinct relations={distinct_rel}",
        results.len()
    );
    Ok(())
}
