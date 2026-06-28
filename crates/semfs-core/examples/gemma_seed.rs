//! Build a Gemma-indexed seed from a COPY of an e5 seed, by swapping only the
//! TEXT vector lane (ticket: embedder-upgrade-gemma-qwen3). Reuses the copy's
//! POSIX file tree (fs_*), chunks, ffts (BM25 is embedder-agnostic) and code
//! lane (jina); re-embeds every chunk's text with EmbeddingGemma-300M, rebuilds
//! `vchunks` at 768d, and re-stamps the text embedder identity so semfs's guard
//! accepts it. The source e5 seed is NEVER opened — operate on the copy only.
//!
//! Usage:  cp ~/.semfs/chanpin-e5-nosum.db ~/.semfs/chanpin-gemma.db
//!         cargo run --release -p semfs-core --example gemma_seed -- ~/.semfs/chanpin-gemma.db

use rusqlite::{params, Connection};
use semfs_core::cache::Db;
use semfs_core::embed::{Embedder, EmbeddingModel, LocalEmbedder};

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn main() -> anyhow::Result<()> {
    let dst = std::env::args()
        .nth(1)
        .expect("usage: gemma_seed <copy.db>");

    // Install the sqlite-vec auto-extension hook process-wide (so our raw
    // Connection can create/insert vec0 tables).
    let _hook = Db::open_in_memory()?;

    let emb = LocalEmbedder::from_registry(EmbeddingModel::EmbeddingGemma300M, None)?;
    let dims = emb.dimensions();
    let ident = emb.identity();
    println!("gemma: dims={dims} identity={ident}");

    let mut conn = Connection::open(&dst)?;

    let rows: Vec<(i64, String)> = {
        let mut s = conn.prepare("SELECT id, text FROM chunks ORDER BY id")?;
        let v: Vec<(i64, String)> = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|x| x.ok())
            .collect();
        v
    };
    println!("re-embedding {} chunks with gemma...", rows.len());

    let texts: Vec<String> = rows.iter().map(|(_, t)| t.clone()).collect();
    let mut vecs: Vec<Vec<f32>> = Vec::with_capacity(rows.len());
    let mut done = 0usize;
    for w in texts.chunks(64) {
        vecs.extend(emb.embed(w)?);
        done += w.len();
        if done % 640 == 0 || done == texts.len() {
            println!("  embedded {done}/{}", texts.len());
        }
    }

    // Swap the text vector lane: drop e5's vchunks (384d), recreate at gemma 768d,
    // reinsert by the SAME rowid (== chunks.id) so vec0/fts/chunks stay joined.
    conn.execute_batch("DROP TABLE IF EXISTS vchunks;")?;
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE vchunks USING vec0(embedding float[{dims}]);"
    ))?;
    {
        let tx = conn.transaction()?;
        {
            let mut ins = tx.prepare("INSERT INTO vchunks(rowid, embedding) VALUES (?1, ?2)")?;
            for ((id, _), v) in rows.iter().zip(vecs.iter()) {
                ins.execute(params![id, vec_to_blob(v)])?;
            }
        }
        tx.commit()?;
    }

    // Re-stamp the TEXT embedder identity + dims so SqliteVecStore::new accepts it.
    conn.execute(
        "INSERT OR REPLACE INTO fs_config(key,value) VALUES('text_embed_model', ?1)",
        params![ident],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO fs_config(key,value) VALUES('text_embed_dims', ?1)",
        params![dims.to_string()],
    )?;

    let cn: i64 = conn.query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))?;
    let vn: i64 = conn.query_row("SELECT count(*) FROM vchunks", [], |r| r.get(0))?;
    let ans: i64 = conn.query_row(
        "SELECT count(*) FROM chunks WHERE filepath LIKE '%best_selling_product_core_data_list%'",
        [],
        |r| r.get(0),
    )?;
    println!("done: chunks={cn} text_vchunks={vn} (answer-file chunks present: {ans})");
    Ok(())
}
