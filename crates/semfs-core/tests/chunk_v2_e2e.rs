//! End-to-end test for the v2 chunker (opt-in `SEMFS_CHUNK_V2=1`): AST-aware
//! code chunking + context headers, through the REAL `SqliteVecStore::index()`
//! write path. Asserts the load-bearing invariant: the `chunks` lane stays
//! VERBATIM (so grep maps a hit → file line ranges) while the `ffts` (BM25) lane
//! carries the enriched header (path + scope). Own test binary/process → setting
//! the env var here is isolated from other tests.
//!
//! Hermetic: stub embedder (no model download), temp DB, no network.

use std::sync::Arc;

use rusqlite::Connection;
use semfs_core::backend::SqliteVecStore;
use semfs_core::cache::Db;
use semfs_core::embed::Embedder;

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

#[test]
fn v2_stores_verbatim_body_and_header_only_in_fts() {
    std::env::set_var("SEMFS_CHUNK_V2", "1");

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("seed.db");
    let db = Arc::new(Db::open(&db_path).unwrap());
    let store = SqliteVecStore::new(db, Arc::new(StubEmbedder)).unwrap();

    // Two small functions with a comfortable budget → they may merge, but the
    // point is: verbatim in `chunks`, header in `ffts`.
    let src = "def alpha():\n    return 1\n\ndef beta():\n    return 2\n";
    store.index(1, "/svc/pay.py", src).unwrap();

    let conn = Connection::open(&db_path).unwrap();

    // 1) chunks.text is VERBATIM — a substring of the source, never header-prefixed.
    let texts: Vec<String> = conn
        .prepare("SELECT text FROM chunks WHERE filepath = '/svc/pay.py' ORDER BY ord")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(!texts.is_empty(), "no chunks written");
    for t in &texts {
        assert!(src.contains(t.as_str()), "chunks.text NOT verbatim: {t:?}");
        assert!(!t.starts_with('#'), "context header leaked into the grep lane: {t:?}");
    }

    // 2) ffts (BM25) carries the header: path + a def name → searchable identifiers.
    let fts: Vec<String> = conn
        .prepare("SELECT text FROM ffts")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        fts.iter().any(|f| f.contains("# /svc/pay.py")),
        "context header (path) missing from ffts: {fts:?}"
    );
    assert!(
        fts.iter().any(|f| f.contains("alpha")),
        "def name missing from ffts header: {fts:?}"
    );
}
