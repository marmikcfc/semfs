//! L5 compute — build the `KNOWLEDGE_GRAPH.md` digest from the persisted graph.
//!
//! Reads `edges` (file↔entity) + `graph_entity` (slug→name), projects to a
//! file↔file graph, runs Louvain→Leiden community detection ([`backend::community`]),
//! picks per-community god-nodes (top-degree entities, p99 hubs excluded), and
//! renders the markdown ([`cache::digest`]). The caller materializes the string
//! as the root virtual file (a derived, local-only fs node).

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::Connection;

use crate::backend::community::{hub_entities, CommunityDetector, Graph, Louvain};
use crate::cache::digest::{render, CommunityView};

const GOD_NODES_PER_TOPIC: usize = 4;
const FILES_PER_TOPIC: usize = 4;
const HUB_PCTL: f64 = 0.99;
const RESOLUTION: f64 = 1.0;

/// KG feature switch. `SEMFS_KG=off|0|false|no` disables the knowledge graph
/// (no `KNOWLEDGE_GRAPH.md` materialized, no KG mention in the agent contract).
/// Default ON. Lets the KG be A/B'd against the no-KG baseline.
pub fn kg_enabled() -> bool {
    !matches!(
        std::env::var("SEMFS_KG")
            .ok()
            .map(|s| s.trim().to_ascii_lowercase()),
        Some(ref v) if v == "off" || v == "0" || v == "false" || v == "no"
    )
}

/// Build the full `KNOWLEDGE_GRAPH.md` body from the graph tables.
pub fn build_digest(conn: &Connection) -> rusqlite::Result<String> {
    // 1. edges → file list + entity interning + per-file entity sets
    let mut files: Vec<String> = Vec::new();
    let mut file_id: HashMap<String, usize> = HashMap::new();
    let mut ent_id: HashMap<String, u32> = HashMap::new();
    let mut ent_path: Vec<String> = Vec::new(); // ent_id -> to_path
    let mut file_entities: Vec<HashSet<u32>> = Vec::new();

    {
        let mut stmt = conn.prepare("SELECT from_path, to_path FROM edges")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (fp, tp) = row?;
            let fi = *file_id.entry(fp.clone()).or_insert_with(|| {
                files.push(fp.clone());
                file_entities.push(HashSet::new());
                files.len() - 1
            });
            let ei = *ent_id.entry(tp.clone()).or_insert_with(|| {
                ent_path.push(tp.clone());
                (ent_path.len() - 1) as u32
            });
            file_entities[fi].insert(ei);
        }
    }

    // dir-map + total files: from the chunk index (all indexed files, not just
    // those with entities), so the map reflects the whole workspace.
    let (dir_map, total_files) = dir_map(conn)?;

    if files.is_empty() {
        // No entity graph yet — ship the structural map so the file still
        // orients the agent (Phase-0 fallback).
        return Ok(render(&[], total_files, &dir_map, FILES_PER_TOPIC));
    }

    // 2. entity degree (#files mentioning it) + hub set
    let mut ent_degree: HashMap<u32, usize> = HashMap::new();
    for ents in &file_entities {
        for &e in ents {
            *ent_degree.entry(e).or_insert(0) += 1;
        }
    }
    let hubs = hub_entities(&ent_degree, HUB_PCTL);

    // 3. entity slug-path -> display name
    let mut names: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT path, name FROM graph_entity")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows.flatten() {
            names.insert(row.0, row.1);
        }
    }

    // 4. detect communities on the file↔file projection
    let g = Graph::from_file_entities(&file_entities);
    let comm = Louvain { leiden_refine: true }.detect(&g, RESOLUTION);

    // 5. per community: members + god-nodes (top-degree entities, hubs excluded)
    let mut by_comm: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (fi, &c) in comm.iter().enumerate() {
        by_comm.entry(c).or_default().push(fi);
    }
    let mut views: Vec<CommunityView> = Vec::new();
    for (_c, members) in by_comm {
        // count entity degree WITHIN this community
        let mut local_deg: HashMap<u32, usize> = HashMap::new();
        for &fi in &members {
            for &e in &file_entities[fi] {
                if !hubs.contains(&e) {
                    *local_deg.entry(e).or_insert(0) += 1;
                }
            }
        }
        let mut ranked: Vec<(u32, usize)> = local_deg.into_iter().collect();
        // most central first; tie-break by entity id for determinism
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let god_entities: Vec<String> = ranked
            .iter()
            .take(GOD_NODES_PER_TOPIC)
            .map(|(e, _)| {
                let path = &ent_path[*e as usize];
                names
                    .get(path)
                    .cloned()
                    .unwrap_or_else(|| slug_of(path))
            })
            .collect();
        let mut member_files: Vec<String> = members.iter().map(|&fi| files[fi].clone()).collect();
        member_files.sort();
        views.push(CommunityView {
            god_entities,
            size: member_files.len(),
            member_files,
        });
    }
    // largest topics first
    views.sort_by(|a, b| b.size.cmp(&a.size));

    Ok(render(&views, total_files, &dir_map, FILES_PER_TOPIC))
}

/// Build `graph.json` — the queryable knowledge graph (graphify parity): the
/// full node+edge list plus per-community summaries and a confidence breakdown.
/// Deterministic (sorted) so re-runs diff cleanly. Nodes are files + entities;
/// edges are the stored file→entity links with their relation kind + confidence.
pub fn build_graph_json(conn: &Connection) -> rusqlite::Result<String> {
    use serde_json::{json, Value};

    // edges (from_path, to_path, edge_kind, confidence)
    let mut edges: Vec<(String, String, String, String)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT from_path, to_path, edge_kind, COALESCE(confidence,'INFERRED') FROM edges",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows.flatten() {
            edges.push(row);
        }
    }

    // entity path -> (name, kind)
    let mut ent: BTreeMap<String, (String, String)> = BTreeMap::new();
    {
        let mut stmt = conn.prepare("SELECT path, name, kind FROM graph_entity")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        for row in rows.flatten() {
            ent.insert(row.0, (row.1, row.2));
        }
    }

    // nodes: distinct files (from edges) + entities, sorted for determinism
    let mut files: BTreeMap<String, ()> = BTreeMap::new();
    let mut conf_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (fp, _tp, _k, c) in &edges {
        files.insert(fp.clone(), ());
        *conf_counts.entry(c.clone()).or_insert(0) += 1;
    }
    let mut nodes: Vec<Value> = Vec::new();
    for fp in files.keys() {
        nodes.push(json!({
            "id": fp,
            "label": fp.rsplit('/').next().unwrap_or(fp),
            "type": "file",
        }));
    }
    for (path, (name, kind)) in &ent {
        nodes.push(json!({"id": path, "label": name, "type": "entity", "kind": kind}));
    }

    let edges_json: Vec<Value> = {
        let mut e: Vec<(String, String, String, String)> = edges.clone();
        e.sort();
        e.into_iter()
            .map(|(s, t, rel, c)| json!({"source": s, "target": t, "relation": rel, "confidence": c}))
            .collect()
    };

    // communities (reuse the same projection as the digest)
    let mut communities: Vec<Value> = Vec::new();
    if !edges.is_empty() {
        let mut file_id: HashMap<String, usize> = HashMap::new();
        let mut flist: Vec<String> = Vec::new();
        let mut ent_id: HashMap<String, u32> = HashMap::new();
        let mut ent_path: Vec<String> = Vec::new();
        let mut file_entities: Vec<HashSet<u32>> = Vec::new();
        for (fp, tp, _k, _c) in &edges {
            let fi = *file_id.entry(fp.clone()).or_insert_with(|| {
                flist.push(fp.clone());
                file_entities.push(HashSet::new());
                flist.len() - 1
            });
            let ei = *ent_id.entry(tp.clone()).or_insert_with(|| {
                ent_path.push(tp.clone());
                (ent_path.len() - 1) as u32
            });
            file_entities[fi].insert(ei);
        }
        let mut ent_degree: HashMap<u32, usize> = HashMap::new();
        for ents in &file_entities {
            for &e in ents {
                *ent_degree.entry(e).or_insert(0) += 1;
            }
        }
        let hubs = hub_entities(&ent_degree, HUB_PCTL);
        let g = Graph::from_file_entities(&file_entities);
        let comm = Louvain { leiden_refine: true }.detect(&g, RESOLUTION);
        let mut by_comm: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (fi, &c) in comm.iter().enumerate() {
            by_comm.entry(c).or_default().push(fi);
        }
        let mut tmp: Vec<(usize, Vec<String>, Vec<String>)> = Vec::new();
        for (_c, members) in by_comm {
            let mut local_deg: HashMap<u32, usize> = HashMap::new();
            for &fi in &members {
                for &e in &file_entities[fi] {
                    if !hubs.contains(&e) {
                        *local_deg.entry(e).or_insert(0) += 1;
                    }
                }
            }
            let mut ranked: Vec<(u32, usize)> = local_deg.into_iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let gods: Vec<String> = ranked
                .iter()
                .take(GOD_NODES_PER_TOPIC)
                .map(|(e, _)| {
                    let p = &ent_path[*e as usize];
                    ent.get(p).map(|(n, _)| n.clone()).unwrap_or_else(|| slug_of(p))
                })
                .collect();
            let mut mf: Vec<String> = members.iter().map(|&fi| flist[fi].clone()).collect();
            mf.sort();
            tmp.push((mf.len(), gods, mf));
        }
        tmp.sort_by(|a, b| b.0.cmp(&a.0));
        for (i, (size, gods, mf)) in tmp.into_iter().enumerate() {
            communities.push(json!({"id": i, "size": size, "god_nodes": gods, "files": mf}));
        }
    }

    let doc = json!({
        "generated_by": "semfs",
        "stats": {
            "files": files.len(),
            "entities": ent.len(),
            "edges": edges.len(),
            "communities": communities.len(),
            "confidence": conf_counts,
        },
        "nodes": nodes,
        "edges": edges_json,
        "communities": communities,
    });
    Ok(serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string()))
}

/// Top-level directory map (dir -> #indexed files) + total indexed files.
fn dir_map(conn: &Connection) -> rusqlite::Result<(Vec<(String, usize)>, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    let mut stmt = conn.prepare("SELECT DISTINCT filepath FROM chunks")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    for fp in rows.flatten() {
        total += 1;
        let top = top_dir(&fp);
        *counts.entry(top).or_insert(0) += 1;
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(20);
    Ok((v, total))
}

fn top_dir(path: &str) -> String {
    let p = path.trim_start_matches('/');
    match p.find('/') {
        Some(i) => format!("/{}", &p[..i]),
        None => "/".to_string(),
    }
}

fn slug_of(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE edges(from_path TEXT, to_path TEXT, edge_kind TEXT, created_at INT, confidence TEXT);
             CREATE TABLE graph_entity(path TEXT PRIMARY KEY, name TEXT, kind TEXT);
             CREATE TABLE chunks(id INTEGER PRIMARY KEY, filepath TEXT, text TEXT);",
        )
        .unwrap();
    }

    fn add_edge(conn: &Connection, file: &str, ent_name: &str, slug: &str) {
        let path = format!("/memories/{slug}.md");
        conn.execute(
            "INSERT INTO edges(from_path,to_path,edge_kind,created_at,confidence) VALUES (?1,?2,'Concept',0,'INFERRED')",
            rusqlite::params![file, path],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO graph_entity(path,name,kind) VALUES (?1,?2,'Concept')",
            rusqlite::params![path, ent_name],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chunks(filepath,text) VALUES (?1,'x')",
            rusqlite::params![file],
        )
        .unwrap();
    }

    #[test]
    fn builds_digest_with_named_god_nodes_and_communities() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        // cluster 1: two files share entity "成交金额"
        add_edge(&conn, "/desktop/sales/a.txt", "成交金额", "e-aaaa");
        add_edge(&conn, "/desktop/sales/b.txt", "成交金额", "e-aaaa");
        // cluster 2: two files share entity "taobao"
        add_edge(&conn, "/taobao/x.md", "taobao", "taobao");
        add_edge(&conn, "/taobao/y.md", "taobao", "taobao");
        let md = build_digest(&conn).unwrap();
        assert!(md.contains("成交金额"), "CJK god-node name preserved: {md}");
        assert!(md.contains("taobao"));
        assert!(md.contains("## Directory map"));
        assert!(md.contains("KNOWLEDGE_GRAPH.md"));
    }

    #[test]
    fn builds_graph_json_with_nodes_edges_communities() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        add_edge(&conn, "/desktop/sales/a.txt", "成交金额", "e-aaaa");
        add_edge(&conn, "/desktop/sales/b.txt", "成交金额", "e-aaaa");
        add_edge(&conn, "/taobao/x.md", "taobao", "taobao");
        let js = build_graph_json(&conn).unwrap();
        let v: serde_json::Value = serde_json::from_str(&js).unwrap();
        assert_eq!(v["generated_by"], "semfs");
        assert_eq!(v["stats"]["files"], 3);
        assert_eq!(v["stats"]["entities"], 2);
        assert_eq!(v["stats"]["edges"], 3);
        // confidence breakdown present (all INFERRED from the test helper)
        assert_eq!(v["stats"]["confidence"]["INFERRED"], 3);
        // entity node carries name + kind
        let nodes = v["nodes"].as_array().unwrap();
        assert!(nodes.iter().any(|n| n["type"] == "entity" && n["label"] == "成交金额"));
        assert!(nodes.iter().any(|n| n["type"] == "file"));
        assert!(!v["communities"].as_array().unwrap().is_empty());
    }

    #[test]
    fn empty_graph_still_renders_structural_map() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        conn.execute("INSERT INTO chunks(filepath,text) VALUES ('/a/f.txt','x')", []).unwrap();
        let md = build_digest(&conn).unwrap();
        assert!(md.contains("## Directory map"));
        assert!(md.contains("/a  (1 file(s))"));
    }
}
