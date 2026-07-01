//! Merge partial seed DBs (built by sharded `seed_dir` runs) into a master seed.
//! Used to parallelize a large `seed_dir` build across N Modal workers: each
//! worker indexes its `SEMFS_SHARD=k/N` slice into a partial DB, then this tool
//! folds every partial's chunks + vectors + fts rows into one master DB.
//!
//! Correctness: the three searchable tables join on `chunks.id` (vec0 `vchunks`
//! and fts5 `ffts` use it as `rowid`). Partial DBs each start their `id` at 1, so
//! we re-insert chunks WITHOUT an explicit id — AUTOINCREMENT assigns a fresh id
//! in the master — and carry the embedding/fts rows to that new id. `ino` is the
//! GLOBAL file index (sharded seed_dir writes i+1), so it is already unique across
//! shards and is preserved as-is. The embedding column round-trips as a raw
//! little-endian f32 blob (the exact format `seed_dir` writes), so no re-embedding.
//!
//! Run: merge_seeds <master.db> <partial1.db> [partial2.db ...]

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use semfs_core::cache::Db;

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
        [name],
        |_| Ok(true),
    )
    .optional()
    .ok()
    .flatten()
    .unwrap_or(false)
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let master = args
        .next()
        .expect("usage: merge_seeds <master.db> <partial1.db> [partial2.db ...]");
    let partials: Vec<String> = args.collect();
    assert!(!partials.is_empty(), "no partial DBs given");
    assert!(Path::new(&master).exists(), "master not found: {master}");

    // Trigger sqlite-vec auto-registration (process-global) so every Connection
    // opened afterwards can read/write the vec0 `vchunks` virtual table.
    let _reg = Db::open_in_memory()?;
    drop(_reg);

    let mconn = Connection::open(&master)?;
    mconn.execute_batch("PRAGMA busy_timeout=120000; PRAGMA synchronous=NORMAL;")?;
    let has_code_m = table_exists(&mconn, "vchunks_code");

    let before: i64 = mconn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
    let files_before: i64 =
        mconn.query_row("SELECT COUNT(DISTINCT filepath) FROM chunks", [], |r| r.get(0))?;
    println!("master before: {before} chunks, {files_before} files (code_lane={has_code_m})");

    let mut grand_added = 0usize;
    for partial in &partials {
        assert!(Path::new(partial).exists(), "partial not found: {partial}");
        let pconn = Connection::open(partial)?;
        let has_code_p = table_exists(&pconn, "vchunks_code");

        let rows: Vec<(i64, i64, String, i64, String, Option<i64>, i64)> = {
            let mut stmt = pconn.prepare(
                "SELECT id, ino, filepath, ord, text, last_accessed_at, access_count \
                 FROM chunks ORDER BY id",
            )?;
            let mapped = stmt.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?;
            mapped.collect::<Result<_, _>>()?
        };

        let tx = mconn.unchecked_transaction()?;
        let mut added = 0usize;
        for (cid, ino, fp, ordv, text, laa, ac) in rows {
            tx.execute(
                "INSERT INTO chunks(ino, filepath, ord, text, last_accessed_at, access_count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![ino, fp, ordv, text, laa, ac],
            )?;
            let nid = tx.last_insert_rowid();

            // Text-lane embedding (the common case). If the chunk lived in the
            // code lane instead, carry that vector over when both DBs have it.
            let temb: Option<Vec<u8>> = pconn
                .query_row("SELECT embedding FROM vchunks WHERE rowid = ?1", [cid], |r| {
                    r.get(0)
                })
                .optional()?;
            if let Some(blob) = temb {
                tx.execute(
                    "INSERT INTO vchunks(rowid, embedding) VALUES (?1, ?2)",
                    params![nid, blob],
                )?;
            } else if has_code_p && has_code_m {
                let cemb: Option<Vec<u8>> = pconn
                    .query_row(
                        "SELECT embedding FROM vchunks_code WHERE rowid = ?1",
                        [cid],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(blob) = cemb {
                    tx.execute(
                        "INSERT INTO vchunks_code(rowid, embedding) VALUES (?1, ?2)",
                        params![nid, blob],
                    )?;
                }
            }

            tx.execute(
                "INSERT INTO ffts(rowid, text) VALUES (?1, ?2)",
                params![nid, text],
            )?;
            added += 1;
        }
        tx.commit()?;
        grand_added += added;
        println!("merged {partial}: +{added} chunks");
    }

    let after: i64 = mconn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
    let files_after: i64 =
        mconn.query_row("SELECT COUNT(DISTINCT filepath) FROM chunks", [], |r| r.get(0))?;
    let vrows: i64 = mconn.query_row("SELECT COUNT(*) FROM vchunks", [], |r| r.get(0))?;
    println!(
        "master after: {after} chunks (+{grand_added}), {files_after} files, {vrows} vectors"
    );
    assert_eq!(
        after - before,
        grand_added as i64,
        "chunk count delta != rows added"
    );
    Ok(())
}
