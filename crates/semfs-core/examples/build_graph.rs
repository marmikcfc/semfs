//! K3 — comprehensive L7 entity extraction over an existing seed, so the
//! knowledge graph has content. Reuses the production `extract_entities`
//! (gpt-4.1-nano via OpenRouter); writes `edges` + `graph_entity` (names
//! preserved for KG labels). No re-embedding — operates on the chunk text
//! already in the DB. Concurrent (8 workers) since each call is network-bound.
//!
//! Run: OPENROUTER_API_KEY=... cargo run --release -p semfs-core \
//!        --example build_graph -- ~/.semfs/chanpin-gemma.db

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use semfs_core::backend::graph::{entity_path, extract_entities};
use semfs_core::llm::LlmClient;

fn main() -> anyhow::Result<()> {
    let db = std::env::args().nth(1).expect("usage: build_graph <db>");
    let key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .expect("OPENROUTER_API_KEY required");

    let conn = Connection::open(&db)?;
    // Ensure the KG tables/columns exist (this seed predates them).
    let _ = conn.execute(
        "ALTER TABLE edges ADD COLUMN confidence TEXT NOT NULL DEFAULT 'INFERRED'",
        [],
    );
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS graph_entity(path TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);",
    )?;

    // One concatenated text blob per file (cap to keep extraction fast).
    let files: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT filepath, text FROM chunks ORDER BY filepath, id",
        )?;
        let mut acc: std::collections::BTreeMap<String, String> = Default::default();
        for r in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (fp, t) = r?;
            let e = acc.entry(fp).or_default();
            if e.chars().count() < 6000 {
                e.push('\n');
                e.push_str(&t);
            }
        }
        acc.into_iter()
            .map(|(fp, t)| (fp, t.chars().take(6000).collect()))
            .collect()
    };
    println!("extracting entities for {} files (8 workers)…", files.len());

    let files = Arc::new(files);
    let cursor = Arc::new(AtomicUsize::new(0));
    let out: Arc<Mutex<Vec<(String, Vec<semfs_core::backend::graph::ExtractedEntity>)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let files = files.clone();
        let cursor = cursor.clone();
        let out = out.clone();
        let done = done.clone();
        let key = key.clone();
        handles.push(std::thread::spawn(move || {
            let client = LlmClient::openrouter(key);
            loop {
                let i = cursor.fetch_add(1, Ordering::SeqCst);
                if i >= files.len() {
                    break;
                }
                let (fp, text) = &files[i];
                if let Ok(ents) = extract_entities(&client, text) {
                    if !ents.is_empty() {
                        out.lock().unwrap().push((fp.clone(), ents));
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
    let mut n_edges = 0usize;
    for (fp, ents) in results.iter() {
        tx.execute("DELETE FROM edges WHERE from_path=?1", params![fp])?;
        for e in ents {
            let node = entity_path(&e.name);
            tx.execute(
                "INSERT OR IGNORE INTO edges(from_path,to_path,edge_kind,created_at,confidence) \
                 VALUES (?1,?2,?3,?4,'INFERRED')",
                params![fp, node, e.kind, now],
            )?;
            tx.execute(
                "INSERT INTO graph_entity(path,name,kind) VALUES (?1,?2,?3) \
                 ON CONFLICT(path) DO UPDATE SET name=excluded.name, kind=excluded.kind",
                params![node, e.name, e.kind],
            )?;
            n_edges += 1;
        }
    }
    tx.commit()?;
    println!(
        "done: {} files with entities, {} edges, {} distinct entities",
        results.len(),
        n_edges,
        conn.query_row("SELECT COUNT(*) FROM graph_entity", [], |r| r.get::<_, i64>(0))?
    );
    Ok(())
}
