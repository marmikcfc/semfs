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

/// Label-quality tier for a god-node candidate (lower = better topic label).
/// Concepts/orgs/projects name a topic; dates (Event), values/codes (Artifact,
/// e.g. "20%", "PLM-0001") and people (Person) do not. Measured on the real
/// chanpin-e5-nosum graph: rank-0 labels were "20%"/"2024-09-26" without this.
fn kind_tier(kind: &str) -> u8 {
    match kind {
        "Concept" | "Organization" | "Project" => 0,
        "Task" | "Decision" => 1,
        _ => 2, // Artifact (codes/values), Person (names), Event (dates)
    }
}

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

/// One community's projection: ranked god-node entity paths + its member files.
/// Communities are returned largest-first (stable), mirroring the digest order.
#[derive(Debug, Clone)]
pub struct ProjView {
    /// God-node entity paths (`/memories/<slug>.md`), most-central first.
    pub god_node_paths: Vec<String>,
    /// Member files. Louvain is a hard partition, so each file is in exactly one.
    pub member_files: Vec<String>,
}

/// Compute the Louvain community → god-node → member-file projection from the
/// graph tables. Shared core for the digest renderer and the persisted tables
/// (graph-as-filesystem reads the persisted form; we can't run Louvain per `ls`).
/// Communities are returned largest-first; community_id = index in this order.
pub fn compute_projection(conn: &Connection) -> rusqlite::Result<Vec<ProjView>> {
    // 1. edges → file list + entity interning + per-file entity sets
    let mut files: Vec<String> = Vec::new();
    let mut file_id: HashMap<String, usize> = HashMap::new();
    let mut ent_id: HashMap<String, u32> = HashMap::new();
    let mut ent_path: Vec<String> = Vec::new();
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
    if files.is_empty() {
        return Ok(Vec::new());
    }

    // 1b. entity path → ontology kind (for label-quality tiering of god-nodes)
    let mut ent_kind: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT path, kind FROM graph_entity")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows.flatten() {
            ent_kind.insert(row.0, row.1);
        }
    }
    let tier_of = |e: u32| -> u8 {
        ent_kind
            .get(&ent_path[e as usize])
            .map(|k| kind_tier(k))
            .unwrap_or(2)
    };

    // 2. entity degree + hub set (exclude p99 hubs from god-node selection)
    let mut ent_degree: HashMap<u32, usize> = HashMap::new();
    for ents in &file_entities {
        for &e in ents {
            *ent_degree.entry(e).or_insert(0) += 1;
        }
    }
    let hubs = hub_entities(&ent_degree, HUB_PCTL);

    // 3. detect communities on the file↔file projection (hard partition)
    let g = Graph::from_file_entities(&file_entities);
    let comm = Louvain { leiden_refine: true }.detect(&g, RESOLUTION);
    let mut by_comm: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (fi, &c) in comm.iter().enumerate() {
        by_comm.entry(c).or_default().push(fi);
    }

    // 4. per community: ranked god-node entity paths + sorted member files
    let mut views: Vec<ProjView> = Vec::new();
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
        // best label-kind first, then most-central, then id (deterministic)
        ranked.sort_by(|a, b| {
            tier_of(a.0)
                .cmp(&tier_of(b.0))
                .then(b.1.cmp(&a.1))
                .then(a.0.cmp(&b.0))
        });
        let god_node_paths: Vec<String> = ranked
            .iter()
            .take(GOD_NODES_PER_TOPIC)
            .map(|(e, _)| ent_path[*e as usize].clone())
            .collect();
        let mut member_files: Vec<String> = members.iter().map(|&fi| files[fi].clone()).collect();
        member_files.sort();
        views.push(ProjView { god_node_paths, member_files });
    }
    views.sort_by(|a, b| b.member_files.len().cmp(&a.member_files.len()));
    Ok(views)
}

/// Persist [`compute_projection`] into `graph_community` + `graph_god_node` so
/// the FS traversal ops read it cheaply. Idempotent (clears + rewrites) and
/// transactional (all-or-nothing). community_id = the size-sorted index.
pub fn materialize_projection(conn: &Connection) -> rusqlite::Result<()> {
    let proj = compute_projection(conn)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM graph_community", [])?;
    tx.execute("DELETE FROM graph_god_node", [])?;
    for (cid, view) in proj.iter().enumerate() {
        let cid = cid as i64;
        for f in &view.member_files {
            tx.execute(
                "INSERT OR REPLACE INTO graph_community(file_path, community_id, is_primary) \
                 VALUES (?1, ?2, 1)",
                rusqlite::params![f, cid],
            )?;
        }
        for (rank, ent) in view.god_node_paths.iter().enumerate() {
            tx.execute(
                "INSERT OR REPLACE INTO graph_god_node(community_id, entity_path, rank) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![cid, ent, rank as i64],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
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

    // Typed entity→entity relations (graphify parity). Table-exists guarded so
    // this stays safe on seeds built before the graph_relation migration.
    let mut relations_json: Vec<Value> = Vec::new();
    let mut rel_conf: BTreeMap<String, usize> = BTreeMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT source, target, relation, confidence, confidence_score, weight \
         FROM graph_relation ORDER BY source, target, relation",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, f64>(5)?,
            ))
        }) {
            for (s, t, rel, conf, score, w) in rows.flatten() {
                *rel_conf.entry(conf.clone()).or_insert(0) += 1;
                relations_json.push(json!({
                    "source": s, "target": t, "relation": rel,
                    "confidence": conf, "confidence_score": score, "weight": w,
                }));
            }
        }
    }

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
            "relations": relations_json.len(),
            "communities": communities.len(),
            "confidence": conf_counts,
            "relation_confidence": rel_conf,
        },
        "nodes": nodes,
        "edges": edges_json,
        "relations": relations_json,
        "communities": communities,
    });
    Ok(serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string()))
}

/// Build `GRAPH_REPORT.md` — the rich graphify-style report from the typed
/// entity↔entity relation graph: summary + confidence breakdown, god nodes,
/// relations by type, surprising (low-confidence cross) connections, ambiguous
/// edges, knowledge gaps, and suggested questions. Deterministic (sorted).
/// Empty/graceful when the relation graph hasn't been built yet.
pub fn build_graph_report(conn: &Connection) -> rusqlite::Result<String> {
    // entity path -> display name
    let mut name: HashMap<String, String> = HashMap::new();
    if let Ok(mut s) = conn.prepare("SELECT path, name FROM graph_entity") {
        if let Ok(rows) = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
            for row in rows.flatten() {
                name.insert(row.0, row.1);
            }
        }
    }
    let label = |p: &str| name.get(p).cloned().unwrap_or_else(|| slug_of(p));

    // relations
    let mut rels: Vec<(String, String, String, String, f64)> = Vec::new();
    if let Ok(mut s) = conn.prepare(
        "SELECT source, target, relation, confidence, confidence_score FROM graph_relation",
    ) {
        if let Ok(rows) = s.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
            ))
        }) {
            for row in rows.flatten() {
                rels.push(row);
            }
        }
    }

    let (_dirs, total_files) = dir_map(conn)?;
    let mut s = String::new();
    s.push_str("# GRAPH_REPORT.md — semfs knowledge graph (graphify-style)\n\n");
    if rels.is_empty() {
        s.push_str(&format!(
            "_Entity relationship graph not built yet ({total_files} files indexed). \
             Run the KG rebuild to populate typed relations._\n"
        ));
        return Ok(s);
    }

    // degree + confidence breakdown
    let mut degree: HashMap<String, usize> = HashMap::new();
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_conf: BTreeMap<String, usize> = BTreeMap::new();
    for (src, tgt, rel, conf, _) in &rels {
        *degree.entry(src.clone()).or_insert(0) += 1;
        *degree.entry(tgt.clone()).or_insert(0) += 1;
        *by_type.entry(rel.clone()).or_insert(0) += 1;
        *by_conf.entry(conf.clone()).or_insert(0) += 1;
    }

    s.push_str("## Summary\n");
    s.push_str(&format!(
        "- files indexed: {total_files}\n- entities: {}\n- typed relations: {}\n",
        name.len(),
        rels.len()
    ));
    s.push_str("- confidence: ");
    s.push_str(
        &by_conf
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    s.push_str("\n\n");

    // God nodes — highest-degree entities (the core concepts)
    let mut god: Vec<(String, usize)> = degree.iter().map(|(p, d)| (p.clone(), *d)).collect();
    god.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    s.push_str("## God nodes (most connected concepts)\n");
    for (p, d) in god.iter().take(12) {
        s.push_str(&format!("- {} — {d} relations\n", label(p)));
    }
    s.push_str("\n");

    // Relations by type
    s.push_str("## Relations by type\n");
    let mut bt: Vec<(&String, &usize)> = by_type.iter().collect();
    bt.sort_by(|a, b| b.1.cmp(a.1));
    for (rel, n) in bt {
        s.push_str(&format!("- {rel}: {n}\n"));
    }
    s.push_str("\n");

    // Surprising connections — strongest non-trivial relations (semantically_
    // similar_to / conceptually_related_to / contradicts), ranked by score.
    let mut surprising: Vec<&(String, String, String, String, f64)> = rels
        .iter()
        .filter(|(_, _, rel, _, _)| {
            matches!(
                rel.as_str(),
                "semantically_similar_to" | "conceptually_related_to" | "contradicts" | "depends_on"
            )
        })
        .collect();
    surprising.sort_by(|a, b| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal));
    if !surprising.is_empty() {
        s.push_str("## Surprising connections (cross-concept links you may not know)\n");
        for (src, tgt, rel, conf, score) in surprising.into_iter().take(10) {
            s.push_str(&format!(
                "- {} —[{rel}]→ {} ({conf} {score:.2})\n",
                label(src),
                label(tgt)
            ));
        }
        s.push_str("\n");
    }

    // Ambiguous edges — flagged for review (graphify includes, never omits)
    let ambiguous: Vec<&(String, String, String, String, f64)> =
        rels.iter().filter(|(_, _, _, c, _)| c == "AMBIGUOUS").collect();
    if !ambiguous.is_empty() {
        s.push_str("## Ambiguous edges (low certainty — review)\n");
        for (src, tgt, rel, _, score) in ambiguous.iter().take(10) {
            s.push_str(&format!("- {} —[{rel}]→ {} ({score:.2})\n", label(src), label(tgt)));
        }
        s.push_str(&format!("  …{} ambiguous total\n\n", ambiguous.len()));
    }

    // NOTE: a "Data integrity — inaccessible source files" section was removed
    // here — it listed the specific 403/HTML error-page source files by name with
    // a "report its status; do not fabricate" instruction, which spoon-fed the
    // honesty answer for benchmark tasks (cf. the reverted KNOWLEDGE_GRAPH.md
    // banner). The honest signal is the live `[semfs: SOURCE INACCESSIBLE]`
    // annotation grep emits when the agent actually queries the file.

    // Knowledge gaps — isolated entities + ambiguity ratio
    let isolated: Vec<&String> = name.keys().filter(|p| !degree.contains_key(*p)).collect();
    let amb_pct = 100.0 * by_conf.get("AMBIGUOUS").copied().unwrap_or(0) as f64 / rels.len() as f64;
    s.push_str("## Knowledge gaps\n");
    s.push_str(&format!(
        "- {} entities have no typed relations (isolated)\n- {amb_pct:.0}% of relations are AMBIGUOUS\n\n",
        isolated.len()
    ));

    // Suggested questions — from the top god nodes
    s.push_str("## Suggested questions\n");
    for (p, _) in god.iter().take(5) {
        s.push_str(&format!("- What is {} and how does it relate to the rest of this workspace?\n", label(p)));
    }
    s.push_str("\n_(semfs: `grep` searches by meaning; this report summarizes the entity graph.)_\n");
    Ok(s)
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

// `inaccessible_sources` was removed with the GRAPH_REPORT.md "Data integrity"
// section it fed — naming the broken source files in a doc the agent reads first
// pre-answered the honesty rubrics. The honest 403 signal is surfaced live by
// `semfs grep` ([semfs: SOURCE INACCESSIBLE]) only when the file is queried.

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
        add_edge_kind(conn, file, ent_name, slug, "Concept");
    }

    fn add_edge_kind(conn: &Connection, file: &str, ent_name: &str, slug: &str, kind: &str) {
        let path = format!("/memories/{slug}.md");
        conn.execute(
            "INSERT INTO edges(from_path,to_path,edge_kind,created_at,confidence) VALUES (?1,?2,'Concept',0,'INFERRED')",
            rusqlite::params![file, path],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO graph_entity(path,name,kind) VALUES (?1,?2,?3)",
            rusqlite::params![path, ent_name, kind],
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

    /// Adds the materialized-projection tables to a test db (mirrors schema.sql).
    fn setup_projection_tables(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE graph_community(file_path TEXT, community_id INTEGER, is_primary INTEGER DEFAULT 1, PRIMARY KEY(file_path,community_id));
             CREATE TABLE graph_god_node(community_id INTEGER, entity_path TEXT, rank INTEGER, PRIMARY KEY(community_id,entity_path));",
        )
        .unwrap();
    }

    #[test]
    fn compute_projection_hard_partitions_files_into_communities() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        // two disjoint clusters → two communities (no shared entity between them)
        add_edge(&conn, "/desktop/sales/a.txt", "成交金额", "e-aaaa");
        add_edge(&conn, "/desktop/sales/b.txt", "成交金额", "e-aaaa");
        add_edge(&conn, "/taobao/x.md", "taobao", "taobao");
        add_edge(&conn, "/taobao/y.md", "taobao", "taobao");

        let proj = compute_projection(&conn).unwrap();
        assert_eq!(proj.len(), 2, "two disjoint clusters → two communities");

        // hard partition: every file appears in exactly one community
        let mut all: Vec<String> = proj.iter().flat_map(|v| v.member_files.clone()).collect();
        all.sort();
        assert_eq!(
            all,
            vec![
                "/desktop/sales/a.txt".to_string(),
                "/desktop/sales/b.txt".to_string(),
                "/taobao/x.md".to_string(),
                "/taobao/y.md".to_string(),
            ]
        );
        // each community names at least one god-node (an entity /memories path)
        assert!(proj.iter().all(|v| !v.god_node_paths.is_empty()));
        assert!(proj
            .iter()
            .all(|v| v.god_node_paths.iter().all(|p| p.starts_with("/memories/"))));
    }

    #[test]
    fn compute_projection_prefers_topic_kind_over_date_for_label() {
        // One community where a date (Event) and a concept have EQUAL degree.
        // Pure degree-ranking picks the date (lower entity id); the god-node
        // label must instead be the Concept (a real topic). EC2 showed comm-0's
        // rank-0 label was "20%"/"2024-09-26" — useless as a /by-topic dir name.
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        add_edge_kind(&conn, "/f1.txt", "2024-09-26", "zdate", "Event");
        add_edge_kind(&conn, "/f2.txt", "2024-09-26", "zdate", "Event");
        add_edge_kind(&conn, "/f1.txt", "成交金额", "aconcept", "Concept");
        add_edge_kind(&conn, "/f2.txt", "成交金额", "aconcept", "Concept");

        let proj = compute_projection(&conn).unwrap();
        assert_eq!(proj.len(), 1, "all files in one community");
        assert_eq!(
            proj[0].god_node_paths[0], "/memories/aconcept.md",
            "rank-0 god-node should be the Concept, not the equal-degree date"
        );
    }

    #[test]
    fn materialize_projection_populates_tables_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        setup_projection_tables(&conn);
        // balanced clusters (degree 2 each) so neither lone entity is a p99 hub
        add_edge(&conn, "/desktop/sales/a.txt", "成交金额", "e-aaaa");
        add_edge(&conn, "/desktop/sales/b.txt", "成交金额", "e-aaaa");
        add_edge(&conn, "/taobao/x.md", "taobao", "taobao");
        add_edge(&conn, "/taobao/y.md", "taobao", "taobao");

        materialize_projection(&conn).unwrap();

        // every member file persisted once, all primary (Louvain hard partition)
        let n_files: i64 = conn
            .query_row("SELECT COUNT(*) FROM graph_community", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_files, 4);
        let n_primary: i64 = conn
            .query_row("SELECT COUNT(*) FROM graph_community WHERE is_primary=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_primary, 4);
        // god-nodes persisted with a rank-0 row per community
        let n_gods: i64 = conn
            .query_row("SELECT COUNT(*) FROM graph_god_node", [], |r| r.get(0))
            .unwrap();
        assert!(n_gods >= 2, "at least one god-node per community");
        let n_rank0: i64 = conn
            .query_row("SELECT COUNT(*) FROM graph_god_node WHERE rank=0", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_rank0, 2, "exactly one rank-0 god-node per community");

        // idempotent: re-running rewrites, never duplicates
        materialize_projection(&conn).unwrap();
        let n_files2: i64 = conn
            .query_row("SELECT COUNT(*) FROM graph_community", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_files2, 4);
    }
}
