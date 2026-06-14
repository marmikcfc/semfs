//! Run Louvain community detection over a seed's file↔entity graph and persist
//! the `graph_community` / `graph_god_node` projection — the form the
//! graph-as-filesystem `/kg/` overlay + `KNOWLEDGE_GRAPH.md` digest read from.
//!
//! `build_kg` writes the raw graph (entities/relations/edges) but NOT the
//! community projection; the mount can't run Louvain per `ls`, so it must be
//! materialized once. Cheap (no LLM, no network) — Louvain over the edge table.
//!
//! Run: cargo run --release -p semfs-core --example materialize_kg -- /path/to/seed.db

use rusqlite::Connection;
use semfs_core::cache::graph_file::materialize_projection;
use semfs_core::cache::Db;

fn main() -> anyhow::Result<()> {
    let db = std::env::args().nth(1).expect("usage: materialize_kg <db>");
    // Install the sqlite-vec auto-extension hook process-wide so opening a seed
    // that has vec0 tables doesn't trip on the unknown vtab module.
    let _hook = Db::open_in_memory()?;
    let conn = Connection::open(&db)?;
    materialize_projection(&conn)?;
    let communities: i64 =
        conn.query_row("SELECT COUNT(DISTINCT community_id) FROM graph_community", [], |r| r.get(0))?;
    let members: i64 = conn.query_row("SELECT COUNT(*) FROM graph_community", [], |r| r.get(0))?;
    let gods: i64 = conn.query_row("SELECT COUNT(*) FROM graph_god_node", [], |r| r.get(0))?;
    println!("materialized: {communities} communities, {members} member-file rows, {gods} god-node rows");
    Ok(())
}
