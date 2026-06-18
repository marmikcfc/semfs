//! Internal KG priors for local retrieval.
//!
//! This layer is intentionally hidden from the agent: it only adds a bounded
//! file-level prior before rerank, and never emits KG artifacts in search
//! results. v1 only reorders files already present in the candidate pool.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, params_from_iter, Connection};

const MAX_MATCHED_ENTITIES: usize = 32;
const MAX_PRIOR: f64 = 0.15;
const DIRECT_ENTITY_CAP: f64 = 0.08;
const COMMUNITY_CAP: f64 = 0.05;
const NEIGHBOR_CAP: f64 = 0.04;
const GIANT_COMMUNITY_PENALTY_CAP: f64 = 0.03;
const GIANT_COMMUNITY_START: usize = 24;
const GIANT_COMMUNITY_SPAN: usize = 72;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct KgPriorResult {
    pub priors: HashMap<String, f64>,
    pub matched_entities: Vec<String>,
    pub matched_communities: Vec<i64>,
}

#[derive(Debug, Clone)]
struct MatchedEntity {
    path: String,
    name: String,
}

pub fn enabled() -> bool {
    matches!(
        std::env::var("SEMFS_HIDDEN_KG").ok().as_deref(),
        Some("1" | "on" | "true" | "yes")
    )
}

pub fn query_kg_priors(
    conn: &Connection,
    query: &str,
    candidate_files: impl IntoIterator<Item = String>,
) -> anyhow::Result<KgPriorResult> {
    let candidate_files = dedup_strings(candidate_files);
    if candidate_files.is_empty() {
        return Ok(KgPriorResult::default());
    }
    if query.trim().is_empty() || !has_table(conn, "graph_entity")? || !has_table(conn, "edges")? {
        return Ok(KgPriorResult::default());
    }

    let tokens = query_tokens(query);
    if tokens.is_empty() {
        return Ok(KgPriorResult::default());
    }

    let matched = match_entities(conn, &tokens)?;
    if matched.is_empty() {
        return Ok(KgPriorResult::default());
    }
    let matched_entity_paths: HashSet<String> = matched.iter().map(|m| m.path.clone()).collect();
    let matched_entity_names: Vec<String> = matched.iter().map(|m| m.name.clone()).collect();

    let direct_counts = direct_entity_counts(conn, &matched_entity_paths)?;
    if direct_counts.is_empty() {
        return Ok(KgPriorResult::default());
    }

    let candidate_set: HashSet<String> = candidate_files.iter().cloned().collect();
    let candidate_edges = file_entities(conn, &candidate_files)?;
    let direct_related_entities =
        related_entities(&direct_counts, &candidate_edges, &matched_entity_paths);

    let mut matched_communities = Vec::new();
    let mut community_weight_map = HashMap::new();
    let mut community_size_map = HashMap::new();
    let mut candidate_communities = HashMap::new();
    if has_table(conn, "graph_community")? {
        community_weight_map =
            community_weights(conn, direct_counts.iter().map(|(k, v)| (k.as_str(), *v)))?;
        matched_communities = sorted_i64_keys(&community_weight_map);
        if !matched_communities.is_empty() {
            community_size_map = community_sizes(conn, &matched_communities)?;
            candidate_communities = file_communities(conn, &candidate_files)?;
        }
    }

    let max_direct = direct_counts
        .iter()
        .filter(|(fp, _)| candidate_set.contains(*fp))
        .map(|(_, count)| *count)
        .max()
        .unwrap_or(1) as f64;
    let neighbor_counts = neighbor_counts(&candidate_edges, &direct_related_entities);
    let max_neighbor = neighbor_counts.values().copied().max().unwrap_or(0) as f64;
    let max_community = community_weight_map.values().copied().max().unwrap_or(0) as f64;

    let mut priors = HashMap::new();
    for fp in candidate_files {
        let direct_score = direct_counts
            .get(&fp)
            .map(|count| DIRECT_ENTITY_CAP * (*count as f64 / max_direct).min(1.0))
            .unwrap_or(0.0);
        let mut neighbor_score = 0.0;
        if let Some(overlap) = neighbor_counts.get(&fp) {
            if max_neighbor > 0.0 {
                neighbor_score = NEIGHBOR_CAP * (*overlap as f64 / max_neighbor).min(1.0);
            }
        }
        let mut community_score = 0.0;
        if let Some(communities) = candidate_communities.get(&fp) {
            let mut best: f64 = 0.0;
            for cid in communities {
                let Some(weight) = community_weight_map.get(cid) else {
                    continue;
                };
                let norm = if max_community > 0.0 {
                    (*weight as f64 / max_community).min(1.0)
                } else {
                    0.0
                };
                let size = *community_size_map.get(cid).unwrap_or(&0);
                let community_score_for_cid = COMMUNITY_CAP * norm;
                let penalty = giant_community_penalty(size);
                best = best.max((community_score_for_cid - penalty).max(0.0));
            }
            community_score = best;
        }
        if direct_score == 0.0 {
            neighbor_score *= 0.5;
            community_score *= 0.5;
        }
        let score = direct_score + neighbor_score + community_score;
        let bounded = score.clamp(0.0, MAX_PRIOR);
        if bounded > 0.0 {
            priors.insert(fp, bounded);
        }
    }

    Ok(KgPriorResult {
        priors,
        matched_entities: matched_entity_names,
        matched_communities,
    })
}

fn has_table(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    let exists = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |r| r.get::<_, bool>(0),
    )?;
    Ok(exists)
}

fn dedup_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

fn query_tokens(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for tok in query
        .split(|c: char| !c.is_alphanumeric())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
    {
        let keep = if tok.is_ascii() {
            tok.chars().count() >= 3
        } else {
            tok.chars().count() >= 2
        };
        if keep && seen.insert(tok.clone()) {
            out.push(tok);
        }
    }
    out
}

fn match_entities(conn: &Connection, tokens: &[String]) -> anyhow::Result<Vec<MatchedEntity>> {
    let mut matched = Vec::new();
    let mut seen = HashSet::new();
    let mut stmt = conn.prepare(
        "SELECT path, name FROM graph_entity \
         WHERE lower(name) LIKE '%' || ?1 || '%' \
         ORDER BY length(name) ASC, name ASC \
         LIMIT ?2",
    )?;
    for token in tokens {
        let rows = stmt.query_map(params![token, MAX_MATCHED_ENTITIES as i64], |r| {
            Ok(MatchedEntity {
                path: r.get(0)?,
                name: r.get(1)?,
            })
        })?;
        for row in rows.flatten() {
            if seen.insert(row.path.clone()) {
                matched.push(row);
                if matched.len() >= MAX_MATCHED_ENTITIES {
                    return Ok(matched);
                }
            }
        }
    }
    Ok(matched)
}

fn direct_entity_counts(
    conn: &Connection,
    entity_paths: &HashSet<String>,
) -> anyhow::Result<HashMap<String, usize>> {
    let entity_paths: Vec<String> = entity_paths.iter().cloned().collect();
    if entity_paths.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; entity_paths.len()].join(",");
    let sql = format!(
        "SELECT from_path, COUNT(DISTINCT to_path) \
         FROM edges WHERE to_path IN ({placeholders}) GROUP BY from_path"
    );
    let mut out = HashMap::new();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(entity_paths.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows.flatten() {
        out.insert(row.0, row.1.max(0) as usize);
    }
    Ok(out)
}

fn file_entities(
    conn: &Connection,
    files: &[String],
) -> anyhow::Result<HashMap<String, HashSet<String>>> {
    if files.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; files.len()].join(",");
    let sql = format!("SELECT from_path, to_path FROM edges WHERE from_path IN ({placeholders})");
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(files.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows.flatten() {
        out.entry(row.0).or_default().insert(row.1);
    }
    Ok(out)
}

fn related_entities(
    direct_counts: &HashMap<String, usize>,
    file_entities: &HashMap<String, HashSet<String>>,
    matched_entity_paths: &HashSet<String>,
) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for fp in direct_counts.keys() {
        if let Some(entities) = file_entities.get(fp) {
            for entity in entities {
                if matched_entity_paths.contains(entity) {
                    continue;
                }
                *out.entry(entity.clone()).or_insert(0usize) += 1;
            }
        }
    }
    out
}

fn community_weights<'a>(
    conn: &Connection,
    direct_counts: impl IntoIterator<Item = (&'a str, usize)>,
) -> anyhow::Result<HashMap<i64, usize>> {
    let direct_counts: Vec<(&str, usize)> = direct_counts.into_iter().collect();
    if direct_counts.is_empty() {
        return Ok(HashMap::new());
    }
    let files: Vec<&str> = direct_counts.iter().map(|(fp, _)| *fp).collect();
    let weights_by_file: HashMap<&str, usize> = direct_counts.into_iter().collect();
    let placeholders = vec!["?"; files.len()].join(",");
    let sql = format!(
        "SELECT file_path, community_id FROM graph_community WHERE file_path IN ({placeholders})"
    );
    let mut out = HashMap::new();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(files.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows.flatten() {
        if let Some(weight) = weights_by_file.get(row.0.as_str()) {
            *out.entry(row.1).or_insert(0usize) += *weight;
        }
    }
    Ok(out)
}

fn community_sizes(conn: &Connection, communities: &[i64]) -> anyhow::Result<HashMap<i64, usize>> {
    if communities.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; communities.len()].join(",");
    let sql = format!(
        "SELECT community_id, COUNT(DISTINCT file_path) \
         FROM graph_community WHERE community_id IN ({placeholders}) GROUP BY community_id"
    );
    let mut out = HashMap::new();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(communities.iter()), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows.flatten() {
        out.insert(row.0, row.1.max(0) as usize);
    }
    Ok(out)
}

fn file_communities(
    conn: &Connection,
    files: &[String],
) -> anyhow::Result<HashMap<String, Vec<i64>>> {
    if files.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; files.len()].join(",");
    let sql = format!(
        "SELECT file_path, community_id FROM graph_community WHERE file_path IN ({placeholders})"
    );
    let mut out: HashMap<String, Vec<i64>> = HashMap::new();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(files.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows.flatten() {
        out.entry(row.0).or_default().push(row.1);
    }
    Ok(out)
}

fn neighbor_counts(
    candidate_edges: &HashMap<String, HashSet<String>>,
    related_entities: &HashMap<String, usize>,
) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for (fp, entities) in candidate_edges {
        let overlap = entities
            .iter()
            .filter(|entity| related_entities.contains_key(*entity))
            .count();
        if overlap > 0 {
            out.insert(fp.clone(), overlap);
        }
    }
    out
}

fn giant_community_penalty(size: usize) -> f64 {
    if size <= GIANT_COMMUNITY_START {
        return 0.0;
    }
    let excess = size.saturating_sub(GIANT_COMMUNITY_START) as f64;
    let span = GIANT_COMMUNITY_SPAN as f64;
    (excess / span).clamp(0.0, 1.0) * GIANT_COMMUNITY_PENALTY_CAP
}

fn sorted_i64_keys(map: &HashMap<i64, usize>) -> Vec<i64> {
    let mut out: Vec<i64> = map.keys().copied().collect();
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Db;

    fn fixture_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn.lock();
        conn.execute(
            "INSERT INTO graph_entity (path, name, kind) VALUES (?1, ?2, 'Concept')",
            params!["/memories/revenue.md", "Revenue"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_entity (path, name, kind) VALUES (?1, ?2, 'Concept')",
            params!["/memories/conversion_rate.md", "Conversion Rate"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_entity (path, name, kind) VALUES (?1, ?2, 'Project')",
            params!["/memories/q1.md", "Q1 Plan"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_entity (path, name, kind) VALUES (?1, ?2, 'Artifact')",
            params!["/memories/widget.md", "Widget"],
        )
        .unwrap();

        for (from_path, to_path) in [
            ("/sales/a.md", "/memories/revenue.md"),
            ("/sales/a.md", "/memories/conversion_rate.md"),
            ("/sales/a.md", "/memories/q1.md"),
            ("/sales/b.md", "/memories/revenue.md"),
            ("/sales/c.md", "/memories/widget.md"),
            ("/sales/d.md", "/memories/q1.md"),
            ("/sales/d.md", "/memories/widget.md"),
        ] {
            conn.execute(
                "INSERT INTO edges (from_path, to_path, edge_kind, created_at, confidence) \
                 VALUES (?1, ?2, 'mentions', 0, 'INFERRED')",
                params![from_path, to_path],
            )
            .unwrap();
        }

        for (file_path, community_id) in [
            ("/sales/a.md", 1i64),
            ("/sales/b.md", 1),
            ("/sales/d.md", 1),
            ("/sales/c.md", 2),
        ] {
            conn.execute(
                "INSERT INTO graph_community (file_path, community_id, is_primary) VALUES (?1, ?2, 1)",
                params![file_path, community_id],
            )
            .unwrap();
        }

        for i in 0..40 {
            conn.execute(
                "INSERT INTO graph_community (file_path, community_id, is_primary) VALUES (?1, 1, 1)",
                params![format!("/noise/{i}.md")],
            )
            .unwrap();
        }
        drop(conn);
        db
    }

    #[test]
    fn enabled_parses_common_truthy_values() {
        std::env::set_var("SEMFS_HIDDEN_KG", "on");
        assert!(enabled());
        std::env::set_var("SEMFS_HIDDEN_KG", "1");
        assert!(enabled());
        std::env::set_var("SEMFS_HIDDEN_KG", "true");
        assert!(enabled());
        std::env::remove_var("SEMFS_HIDDEN_KG");
        assert!(!enabled());
    }

    #[test]
    fn query_kg_priors_scores_direct_and_neighbor_candidates() {
        let db = fixture_db();
        let conn = db.conn.lock();
        let result = query_kg_priors(
            &conn,
            "revenue conversion rate",
            vec![
                "/sales/a.md".to_string(),
                "/sales/b.md".to_string(),
                "/sales/d.md".to_string(),
            ],
        )
        .unwrap();

        assert_eq!(result.matched_entities.len(), 2);
        assert!(result.matched_entities.iter().any(|e| e == "Revenue"));
        assert!(result
            .matched_entities
            .iter()
            .any(|e| e == "Conversion Rate"));
        assert_eq!(result.matched_communities, vec![1]);
        let a = result.priors["/sales/a.md"];
        let b = result.priors["/sales/b.md"];
        let d = result.priors["/sales/d.md"];
        assert!(
            a > b,
            "direct overlap on two entities should beat one entity"
        );
        assert!(
            b > d,
            "direct overlap should beat a neighbor-only community file"
        );
        assert!(
            d > 0.0,
            "neighbor/community candidate should still get a bounded prior"
        );
        assert!(a <= MAX_PRIOR);
    }

    #[test]
    fn query_kg_priors_penalizes_giant_communities() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn.lock();
        for (path, name) in [
            ("/memories/revenue.md", "Revenue"),
            ("/memories/conversion_rate.md", "Conversion Rate"),
        ] {
            conn.execute(
                "INSERT INTO graph_entity (path, name, kind) VALUES (?1, ?2, 'Concept')",
                params![path, name],
            )
            .unwrap();
        }
        for (file, community) in [("/large.md", 1i64), ("/small.md", 2i64)] {
            conn.execute(
                "INSERT INTO edges (from_path, to_path, edge_kind, created_at, confidence) VALUES (?1, '/memories/revenue.md', 'mentions', 0, 'INFERRED')",
                params![file],
            ).unwrap();
            conn.execute(
                "INSERT INTO graph_community (file_path, community_id, is_primary) VALUES (?1, ?2, 1)",
                params![file, community],
            ).unwrap();
        }
        for i in 0..48 {
            conn.execute(
                "INSERT INTO graph_community (file_path, community_id, is_primary) VALUES (?1, 1, 1)",
                params![format!("/large-noise/{i}.md")],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO graph_community (file_path, community_id, is_primary) VALUES ('/small-peer.md', 2, 1)",
            [],
        )
        .unwrap();

        let result = query_kg_priors(
            &conn,
            "revenue",
            vec!["/large.md".to_string(), "/small.md".to_string()],
        )
        .unwrap();
        assert!(
            result.priors["/small.md"] > result.priors["/large.md"],
            "giant community penalty should reduce the large-cluster candidate"
        );
    }

    #[test]
    fn query_kg_priors_degrades_cleanly_without_graph_tables() {
        let conn = Connection::open_in_memory().unwrap();
        let result = query_kg_priors(
            &conn,
            "revenue",
            vec!["/sales/a.md".to_string(), "/sales/b.md".to_string()],
        )
        .unwrap();
        assert!(result.priors.is_empty());
        assert!(result.matched_entities.is_empty());
        assert!(result.matched_communities.is_empty());
    }
}
