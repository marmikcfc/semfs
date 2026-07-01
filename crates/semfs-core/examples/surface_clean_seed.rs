//! Remove agent-visible KG surface artifacts from a seed DB while preserving the
//! hidden graph tables. Runs through rusqlite with sqlite-vec auto-registered,
//! so vec0 tables (`vchunks`, `vchunks_code`) stay row-count consistent.
//!
//! Run: cargo run --release -p semfs-core --example surface_clean_seed -- /path/to/seed.db

use rusqlite::{params, Connection};
use semfs_core::cache::Db;

fn has_table(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

fn count(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
}

fn main() -> anyhow::Result<()> {
    let db_path = std::env::args()
        .nth(1)
        .expect("usage: surface_clean_seed <db>");

    // Install the sqlite-vec auto-extension hook before opening the seed.
    let _hook = Db::open_in_memory()?;
    let mut conn = Connection::open(&db_path)?;
    let tx = conn.transaction()?;

    let mut deleted = serde_json::Map::new();

    let kg_ino: Option<i64> = tx
        .query_row(
            "SELECT ino FROM fs_dentry WHERE parent_ino = 1 AND name = 'kg'",
            [],
            |r| r.get(0),
        )
        .ok();
    if let Some(kg_ino) = kg_ino {
        let child_inodes: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT ino FROM fs_dentry WHERE parent_ino = ?1")?;
            let rows = stmt.query_map([kg_ino], |r| r.get::<_, i64>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for ino in &child_inodes {
            tx.execute("DELETE FROM fs_data WHERE ino = ?1", [ino])?;
            tx.execute("DELETE FROM fs_inode WHERE ino = ?1", [ino])?;
        }
        tx.execute("DELETE FROM fs_dentry WHERE parent_ino = ?1", [kg_ino])?;
        tx.execute("DELETE FROM fs_inode WHERE ino = ?1", [kg_ino])?;
        tx.execute(
            "DELETE FROM fs_dentry WHERE parent_ino = 1 AND name = 'kg'",
            [],
        )?;
        deleted.insert("kg_children".into(), child_inodes.len().into());
        deleted.insert("kg_dir".into(), 1.into());
    } else {
        deleted.insert("kg_children".into(), 0.into());
        deleted.insert("kg_dir".into(), 0.into());
    }

    for (name, key) in [("AGENTS.md", "agents_md"), ("CLAUDE.md", "claude_md")] {
        let ino: Option<i64> = tx
            .query_row(
                "SELECT d.ino FROM fs_dentry d JOIN fs_inode i ON d.ino = i.ino \
                 WHERE d.parent_ino = 1 AND d.name = ?1 AND i.derived = 1",
                [name],
                |r| r.get(0),
            )
            .ok();
        if let Some(ino) = ino {
            tx.execute("DELETE FROM fs_data WHERE ino = ?1", [ino])?;
            tx.execute("DELETE FROM fs_inode WHERE ino = ?1", [ino])?;
            tx.execute(
                "DELETE FROM fs_dentry WHERE parent_ino = 1 AND name = ?1",
                [name],
            )?;
            deleted.insert(key.into(), 1.into());
        } else {
            deleted.insert(key.into(), 0.into());
        }
    }

    let paths = ["/AGENTS.md", "/CLAUDE.md"];
    let mut direct_chunk_ids: Vec<i64> = Vec::new();
    for path in paths {
        let mut stmt = tx.prepare("SELECT id FROM chunks WHERE filepath = ?1")?;
        let rows = stmt.query_map([path], |r| r.get::<_, i64>(0))?;
        direct_chunk_ids.extend(rows.collect::<Result<Vec<_>, _>>()?);
    }
    let mut kg_chunk_ids: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT id FROM chunks WHERE filepath LIKE '/kg/%'")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    direct_chunk_ids.append(&mut kg_chunk_ids);
    direct_chunk_ids.sort_unstable();
    direct_chunk_ids.dedup();

    let has_code = has_table(&tx, "vchunks_code")?;
    let mut vchunks_deleted = 0;
    let mut vchunks_code_deleted = 0;
    let mut ffts_deleted = 0;
    for id in &direct_chunk_ids {
        vchunks_deleted += tx.execute("DELETE FROM vchunks WHERE rowid = ?1", params![id])?;
        if has_code {
            vchunks_code_deleted +=
                tx.execute("DELETE FROM vchunks_code WHERE rowid = ?1", params![id])?;
        }
        ffts_deleted += tx.execute("DELETE FROM ffts WHERE rowid = ?1", params![id])?;
    }
    let chunks_deleted = if direct_chunk_ids.is_empty() {
        0
    } else {
        tx.execute(
            &format!(
                "DELETE FROM chunks WHERE id IN ({})",
                std::iter::repeat_n("?", direct_chunk_ids.len())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            rusqlite::params_from_iter(direct_chunk_ids.iter()),
        )?
    };
    deleted.insert("chunks".into(), chunks_deleted.into());
    deleted.insert("ffts".into(), ffts_deleted.into());
    deleted.insert("vchunks".into(), vchunks_deleted.into());
    deleted.insert("vchunks_code".into(), vchunks_code_deleted.into());

    let leftovers: i64 = tx.query_row(
        "SELECT count(*) FROM fs_dentry WHERE parent_ino = 1 AND name IN ('AGENTS.md', 'CLAUDE.md', 'kg')",
        [],
        |r| r.get(0),
    )?;
    if leftovers != 0 {
        anyhow::bail!("surface artifacts still visible after cleanup: {leftovers}");
    }

    let chunk_n = count(&tx, "chunks")?;
    let text_n = count(&tx, "vchunks")?;
    let code_n = if has_code {
        count(&tx, "vchunks_code")?
    } else {
        0
    };
    if chunk_n != text_n + code_n {
        anyhow::bail!(
            "vector count mismatch after surface cleanup: chunks={chunk_n} vs vectors={}",
            text_n + code_n
        );
    }

    tx.commit()?;

    let out = serde_json::json!({
        "surface_clean": true,
        "deleted": deleted,
        "table_counts": {
            "edges": count(&conn, "edges")?,
            "graph_community": count(&conn, "graph_community")?,
            "graph_god_node": count(&conn, "graph_god_node")?,
            "chunks": chunk_n,
            "vchunks": text_n,
            "vchunks_code": code_n,
            "vector_total": text_n + code_n,
        }
    });
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}
