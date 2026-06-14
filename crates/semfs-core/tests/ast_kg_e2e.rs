//! End-to-end AST code-lane test (ticket `ast-kg-code-lane`, goal-mandated).
//!
//! Mirrors a real `semfs mount` over a folder of code, minus the NFS transport
//! (which does not affect graph content): it indexes a fixture code tree through
//! the **real** `SqliteVecStore::index()` engine — the exact code path the mount
//! daemon calls on every file write — into a seed DB, then runs the **real**
//! `build_kg` driver (dual lane) against it and asserts the knowledge graph.
//!
//! Hermetic: a stub embedder avoids any model download, and the corpus is a
//! temp dir. No network, no cloud, no OpenRouter key (the fixture is all code,
//! so the LLM doc lane is never invoked).

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use rusqlite::Connection;
use semfs_core::backend::SqliteVecStore;
use semfs_core::cache::Db;
use semfs_core::embed::Embedder;

/// Deterministic 16-d stub embedder — exercises the real index() write path
/// (chunking + vec0 insert) without downloading an ONNX model.
#[derive(Debug)]
struct StubEmbedder;
impl Embedder for StubEmbedder {
    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; 16];
                for (i, b) in t.bytes().enumerate() {
                    v[i % 16] += b as f32;
                }
                let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                v.iter().map(|x| x / n).collect()
            })
            .collect())
    }
    fn dimensions(&self) -> usize {
        16
    }
}

/// Locate the prebuilt `build_kg` example binary (release preferred).
fn build_kg_bin() -> PathBuf {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    for profile in ["release", "debug"] {
        let p = target.join(profile).join("examples/build_kg");
        if p.exists() {
            return p;
        }
    }
    panic!("build_kg example not built — run: cargo build --release -p semfs-core --example build_kg");
}

#[test]
fn folder_to_knowledge_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let corpus = tmp.path().join("workspace");
    let db_path = tmp.path().join("seed.db");

    // ── A fixture code workspace: cross-file Python + Go + TypeScript. ──
    // Exercises every edge type: contains/method/imports/inherits/calls/uses,
    // and cross-FILE resolution (Worker→Animal lives in another file).
    let files: &[(&str, &str)] = &[
        (
            "pkg/base.py",
            "class Animal:\n    def speak(self):\n        return noise()\n\ndef noise():\n    return \"...\"\n",
        ),
        (
            "pkg/dog.py",
            "from pkg.base import Animal\n\nclass Dog(Animal):\n    def speak(self):\n        return bark()\n\ndef bark():\n    return \"woof\"\n",
        ),
        (
            "srv/server.go",
            "package srv\n\nimport \"fmt\"\n\ntype Server struct{}\n\nfunc (s *Server) Start() {\n    boot()\n}\n\nfunc boot() {\n    fmt.Println(\"up\")\n}\n",
        ),
        (
            "web/svc.ts",
            "interface Greeter { greet(): string; }\n\nclass Base { hello() { return \"hi\"; } }\n\nclass Service extends Base implements Greeter {\n  greet() { return this.hello(); }\n}\n",
        ),
    ];

    let db = Arc::new(Db::open(&db_path).unwrap());
    let store = SqliteVecStore::new(db, Arc::new(StubEmbedder)).unwrap();

    for (i, (rel, content)) in files.iter().enumerate() {
        let disk = corpus.join(rel);
        std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
        std::fs::write(&disk, content).unwrap();
        // Index through the REAL engine, with the mount-style "/rel" path.
        store.index(i as u64 + 1, &format!("/{rel}"), content).unwrap();
    }
    drop(store); // release the WAL/connection before the subprocess opens the DB.

    // ── Run the REAL dual-lane KG driver over the indexed seed + corpus. ──
    let out = Command::new(build_kg_bin())
        .arg(&db_path)
        .arg(&corpus)
        .output()
        .expect("run build_kg");
    assert!(
        out.status.success(),
        "build_kg failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // ── Assert the knowledge graph. ──
    let conn = Connection::open(&db_path).unwrap();

    let entity = |name: &str, kind: &str| -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM graph_entity WHERE name=?1 AND kind=?2 AND file_type='code'",
            [name, kind],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    };
    let edge = |relation: &str, src_like: &str, tgt_like: &str, conf: &str| -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM graph_relation \
             WHERE relation=?1 AND source LIKE ?2 AND target LIKE ?3 AND confidence=?4",
            rusqlite::params![relation, src_like, tgt_like, conf],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    };

    // Entities with correct code kinds.
    assert!(entity("Animal", "class"), "Animal class missing");
    assert!(entity("Dog", "class"), "Dog class missing");
    assert!(entity("speak", "method"), "speak method missing");
    assert!(entity("noise", "function"), "noise function missing");
    assert!(entity("Server", "class"), "Go Server struct missing");
    assert!(entity("Start", "method"), "Go Start method missing");
    assert!(entity("Greeter", "interface"), "TS Greeter interface missing");

    // EXTRACTED edges.
    assert!(edge("contains", "%dog.py", "%Dog", "EXTRACTED"), "file→Dog contains missing");
    assert!(edge("method", "%Dog", "%Dog.speak", "EXTRACTED"), "class→method missing");
    assert!(edge("imports", "%dog.py", "pkg.base", "EXTRACTED"), "py import missing");
    // CROSS-FILE inherits: Dog (module pkg.dog) → Animal (module pkg.base).
    // Differing module prefixes prove the symbol resolved across files.
    assert!(edge("inherits", "pkg.dog.Dog", "pkg.base.Animal", "EXTRACTED"), "cross-file inherits missing");
    // TS multiple inheritance: extends Base + implements Greeter.
    assert!(edge("inherits", "%Service", "%Base", "EXTRACTED"), "TS extends missing");
    assert!(edge("inherits", "%Service", "%Greeter", "EXTRACTED"), "TS implements missing");

    // INFERRED edges (weight 0.8).
    assert!(edge("calls", "%Dog.speak", "%bark", "INFERRED"), "Dog.speak→bark call missing");
    assert!(edge("calls", "%Start", "%boot", "INFERRED"), "Go Start→boot call missing");
    let w: f64 = conn
        .query_row("SELECT MIN(weight) FROM graph_relation WHERE confidence='INFERRED'", [], |r| r.get(0))
        .unwrap();
    assert!((w - 0.8).abs() < 1e-9, "INFERRED weight should be 0.8, got {w}");

    // source_location populated for every code relation.
    let blank: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM graph_relation WHERE source_location IS NULL OR source_location=''",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(blank, 0, "every code relation must carry source_location");

    // Sanity: a real graph was produced.
    let (ne, nr): (i64, i64) = (
        conn.query_row("SELECT COUNT(*) FROM graph_entity", [], |r| r.get(0)).unwrap(),
        conn.query_row("SELECT COUNT(*) FROM graph_relation", [], |r| r.get(0)).unwrap(),
    );
    assert!(ne >= 7 && nr >= 8, "graph too small: {ne} entities, {nr} relations");
    eprintln!("KG built from mounted folder: {ne} entities, {nr} relations");
}
