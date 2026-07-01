//! Internal KG priors for local retrieval.
//!
//! This layer is intentionally hidden from the agent: it only adds a bounded
//! file-level prior before rerank, and never emits KG artifacts in search
//! results. v1 only reorders files already present in the candidate pool.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, params_from_iter, Connection};

const MAX_MATCHED_ENTITIES: usize = 32;
const MAX_PRIOR: f64 = 0.15;
const MAX_KG_CANDIDATES: usize = 80;
const MAX_DIRECT_FILES: usize = 40;
const MAX_COMMUNITY_FILES: usize = 80;
const MAX_COMMUNITY_FILES_PER_COMMUNITY: usize = 8;
const MAX_NEIGHBOR_FILES: usize = 40;
const DIRECT_ENTITY_CAP: f64 = 0.08;
const COMMUNITY_CAP: f64 = 0.05;
const NEIGHBOR_CAP: f64 = 0.04;
const GIANT_COMMUNITY_PENALTY_CAP: f64 = 0.03;
const GIANT_COMMUNITY_START: usize = 24;
const GIANT_COMMUNITY_SPAN: usize = 72;

// PPR (Personalized PageRank) variant of the graph prior — A/B-able via SEMFS_KG_PPR.
// Replaces the fixed 1-hop neighbor walk with damped multi-hop diffusion over the
// bipartite file<->entity `edges` graph, seeded at the query's matched entities.
const PPR_CAP: f64 = 0.12; // == DIRECT_ENTITY_CAP + NEIGHBOR_CAP: bound the graph term like 1-hop
const PPR_RESTART_DEFAULT: f64 = 0.5; // teleport-to-seed prob/step (higher = fewer effective hops)
const PPR_ITERS_DEFAULT: usize = 30;
const PPR_MAX_EDGES: usize = 400_000; // safety: above this, fall back to 1-hop (don't load the world)

#[derive(Debug, Default, Clone, PartialEq)]
pub struct KgPriorResult {
    pub priors: HashMap<String, f64>,
    pub matched_entities: Vec<String>,
    pub matched_communities: Vec<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KgCandidateReason {
    DirectEntity,
    Community,
    NeighborEntity,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KgCandidate {
    pub filepath: String,
    pub reason: KgCandidateReason,
    pub score: f64,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct KgCandidateResult {
    pub candidates: Vec<KgCandidate>,
    pub matched_entities: Vec<String>,
    pub matched_communities: Vec<i64>,
}

#[derive(Debug, Clone)]
struct MatchedEntity {
    path: String,
    name: String,
}

pub fn enabled() -> bool {
    truthy_env("SEMFS_HIDDEN_KG")
}

pub fn retrieval_enabled() -> bool {
    truthy_env("SEMFS_HIDDEN_KG_RETRIEVAL")
}

/// PPR variant of the hidden-KG graph prior (replaces the 1-hop neighbor walk).
pub fn ppr_enabled() -> bool {
    truthy_env("SEMFS_KG_PPR")
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

    // A/B: PPR multi-hop diffusion vs the fixed 1-hop neighbor walk. Empty PPR result
    // (no edges / graph too large / no reachable seed) → fall back to the 1-hop path.
    let ppr = ppr_enabled();
    let ppr_scores = if ppr {
        ppr_file_scores(conn, &matched_entity_paths, &candidate_set)?
    } else {
        HashMap::new()
    };

    let mut priors = HashMap::new();
    for fp in candidate_files {
        // Graph term: PPR mass (multi-hop) when enabled, else direct + 1-hop neighbor.
        let (graph_score, connected) = if ppr {
            let g = ppr_scores.get(&fp).copied().unwrap_or(0.0);
            (PPR_CAP * g.clamp(0.0, 1.0), g > 0.0)
        } else {
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
            if direct_score == 0.0 {
                neighbor_score *= 0.5;
            }
            (direct_score + neighbor_score, direct_score > 0.0)
        };
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
        // Not graph-connected to the query → halve the community prior (matches the
        // 1-hop rule where direct_score==0 halved neighbor + community).
        if !connected {
            community_score *= 0.5;
        }
        let score = graph_score + community_score;
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

pub fn query_kg_candidates(
    conn: &Connection,
    query: &str,
    scope: Option<&str>,
    limit: usize,
) -> anyhow::Result<KgCandidateResult> {
    if query.trim().is_empty() || !has_table(conn, "graph_entity")? || !has_table(conn, "edges")? {
        return Ok(KgCandidateResult::default());
    }

    let tokens = query_tokens(query);
    if tokens.is_empty() {
        return Ok(KgCandidateResult::default());
    }

    let matched = match_entities(conn, &tokens)?;
    if matched.is_empty() {
        return Ok(KgCandidateResult::default());
    }
    let matched_entity_paths: HashSet<String> = matched.iter().map(|m| m.path.clone()).collect();
    let matched_entity_names: Vec<String> = matched.iter().map(|m| m.name.clone()).collect();

    let direct_counts = direct_entity_counts_scoped(conn, &matched_entity_paths, scope)?;
    if direct_counts.is_empty() {
        return Ok(KgCandidateResult::default());
    }

    let mut community_weight_map = HashMap::new();
    let mut community_size_map = HashMap::new();
    let mut matched_communities = Vec::new();
    if has_table(conn, "graph_community")? {
        community_weight_map =
            community_weights(conn, direct_counts.iter().map(|(k, v)| (k.as_str(), *v)))?;
        matched_communities = sorted_weighted_communities(&community_weight_map);
        if !matched_communities.is_empty() {
            community_size_map = community_sizes(conn, &matched_communities)?;
        }
    }

    let direct_files: Vec<String> = direct_counts.keys().cloned().collect();
    let direct_entities = file_entities(conn, &direct_files)?;
    let related_entity_counts =
        related_entities(&direct_counts, &direct_entities, &matched_entity_paths);
    let neighbor_counts = neighbor_file_counts_scoped(conn, &related_entity_counts, scope)?;

    let max_direct = direct_counts.values().copied().max().unwrap_or(1) as f64;
    let max_neighbor = neighbor_counts.values().copied().max().unwrap_or(0) as f64;
    let max_community = community_weight_map.values().copied().max().unwrap_or(0) as f64;

    let mut candidates: HashMap<String, KgCandidate> = HashMap::new();
    let mut direct_sorted: Vec<(String, usize)> = direct_counts.into_iter().collect();
    direct_sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (fp, count) in direct_sorted.into_iter().take(MAX_DIRECT_FILES) {
        let score = DIRECT_ENTITY_CAP * (count as f64 / max_direct).min(1.0);
        upsert_candidate(&mut candidates, fp, KgCandidateReason::DirectEntity, score);
    }

    let mut neighbor_sorted: Vec<(String, usize)> = neighbor_counts.into_iter().collect();
    neighbor_sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (fp, overlap) in neighbor_sorted.into_iter().take(MAX_NEIGHBOR_FILES) {
        if max_neighbor <= 0.0 {
            continue;
        }
        let score = NEIGHBOR_CAP * (overlap as f64 / max_neighbor).min(1.0);
        upsert_candidate(
            &mut candidates,
            fp,
            KgCandidateReason::NeighborEntity,
            score,
        );
    }

    if !matched_communities.is_empty() {
        let community_candidates = files_in_communities_scoped(
            conn,
            &matched_communities,
            scope,
            MAX_COMMUNITY_FILES_PER_COMMUNITY,
        )?;
        let mut community_total = 0usize;
        for (cid, files) in community_candidates {
            if community_total >= MAX_COMMUNITY_FILES {
                break;
            }
            let Some(weight) = community_weight_map.get(&cid) else {
                continue;
            };
            let norm = if max_community > 0.0 {
                (*weight as f64 / max_community).min(1.0)
            } else {
                0.0
            };
            let penalty = giant_community_penalty(*community_size_map.get(&cid).unwrap_or(&0));
            let score = (COMMUNITY_CAP * norm - penalty).max(0.0);
            if score == 0.0 {
                continue;
            }
            for fp in files {
                if community_total >= MAX_COMMUNITY_FILES {
                    break;
                }
                community_total += 1;
                upsert_candidate(&mut candidates, fp, KgCandidateReason::Community, score);
            }
        }
    }

    let mut out: Vec<KgCandidate> = candidates.into_values().collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| reason_rank(a.reason).cmp(&reason_rank(b.reason)))
            .then_with(|| a.filepath.cmp(&b.filepath))
    });
    out.truncate(limit.min(MAX_KG_CANDIDATES));

    Ok(KgCandidateResult {
        candidates: out,
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
    direct_entity_counts_scoped(conn, entity_paths, None)
}

fn direct_entity_counts_scoped(
    conn: &Connection,
    entity_paths: &HashSet<String>,
    scope: Option<&str>,
) -> anyhow::Result<HashMap<String, usize>> {
    let entity_paths: Vec<String> = entity_paths.iter().cloned().collect();
    if entity_paths.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; entity_paths.len()].join(",");
    let sql = format!(
        "SELECT from_path, COUNT(DISTINCT to_path) \
         FROM edges WHERE to_path IN ({placeholders}) \
         AND (?{} IS NULL OR instr(from_path, ?{}) = 1) GROUP BY from_path",
        entity_paths.len() + 1,
        entity_paths.len() + 2
    );
    let mut out = HashMap::new();
    let mut stmt = conn.prepare(&sql)?;
    let mut binds: Vec<rusqlite::types::Value> = entity_paths
        .iter()
        .cloned()
        .map(rusqlite::types::Value::from)
        .collect();
    binds.push(scope.map_or(rusqlite::types::Value::Null, |s| {
        rusqlite::types::Value::from(s.to_string())
    }));
    binds.push(scope.map_or(rusqlite::types::Value::Null, |s| {
        rusqlite::types::Value::from(s.to_string())
    }));
    let rows = stmt.query_map(params_from_iter(binds.iter()), |r| {
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

fn neighbor_file_counts_scoped(
    conn: &Connection,
    related_entities: &HashMap<String, usize>,
    scope: Option<&str>,
) -> anyhow::Result<HashMap<String, usize>> {
    let entities: Vec<String> = related_entities.keys().cloned().collect();
    if entities.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; entities.len()].join(",");
    let sql = format!(
        "SELECT from_path, COUNT(DISTINCT to_path) \
         FROM edges WHERE to_path IN ({placeholders}) \
         AND (?{} IS NULL OR instr(from_path, ?{}) = 1) GROUP BY from_path",
        entities.len() + 1,
        entities.len() + 2
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut binds: Vec<rusqlite::types::Value> = entities
        .iter()
        .cloned()
        .map(rusqlite::types::Value::from)
        .collect();
    binds.push(scope.map_or(rusqlite::types::Value::Null, |s| {
        rusqlite::types::Value::from(s.to_string())
    }));
    binds.push(scope.map_or(rusqlite::types::Value::Null, |s| {
        rusqlite::types::Value::from(s.to_string())
    }));
    let rows = stmt.query_map(params_from_iter(binds.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut out = HashMap::new();
    for row in rows.flatten() {
        out.insert(row.0, row.1.max(0) as usize);
    }
    Ok(out)
}

fn files_in_communities_scoped(
    conn: &Connection,
    communities: &[i64],
    scope: Option<&str>,
    per_community_cap: usize,
) -> anyhow::Result<Vec<(i64, Vec<String>)>> {
    if communities.is_empty() || per_community_cap == 0 {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; communities.len()].join(",");
    let sql = format!(
        "SELECT community_id, file_path FROM graph_community \
         WHERE community_id IN ({placeholders}) \
         AND (?{} IS NULL OR instr(file_path, ?{}) = 1) \
         ORDER BY community_id ASC, is_primary DESC, file_path ASC",
        communities.len() + 1,
        communities.len() + 2
    );
    let mut binds: Vec<rusqlite::types::Value> = communities
        .iter()
        .copied()
        .map(rusqlite::types::Value::from)
        .collect();
    binds.push(scope.map_or(rusqlite::types::Value::Null, |s| {
        rusqlite::types::Value::from(s.to_string())
    }));
    binds.push(scope.map_or(rusqlite::types::Value::Null, |s| {
        rusqlite::types::Value::from(s.to_string())
    }));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(binds.iter()), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    let mut seen: HashMap<i64, HashSet<String>> = HashMap::new();
    for row in rows.flatten() {
        let (cid, fp) = row;
        let group = out.entry(cid).or_default();
        if group.len() >= per_community_cap {
            continue;
        }
        let accepted = seen.entry(cid).or_default().insert(fp.clone());
        if accepted {
            group.push(fp);
        }
    }
    let mut ordered = Vec::new();
    for cid in communities {
        if let Some(files) = out.remove(cid) {
            ordered.push((*cid, files));
        }
    }
    Ok(ordered)
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

fn sorted_weighted_communities(map: &HashMap<i64, usize>) -> Vec<i64> {
    let mut out: Vec<(i64, usize)> = map.iter().map(|(k, v)| (*k, *v)).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.into_iter().map(|(cid, _)| cid).collect()
}

fn truthy_env(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1" | "on" | "true" | "yes")
    )
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|x| x.is_finite())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// Personalized PageRank over the bipartite file<->entity `edges` graph, seeded at
/// `seed_entities` (the query's matched entities). Returns max-normalized stationary
/// mass for each candidate file. An empty map means "fall back to the 1-hop prior":
/// no edges, the graph is too large, or no seed entity exists in the graph.
fn ppr_file_scores(
    conn: &Connection,
    seed_entities: &HashSet<String>,
    candidate_files: &HashSet<String>,
) -> anyhow::Result<HashMap<String, f64>> {
    let n_edges: i64 = conn
        .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
        .unwrap_or(0);
    if n_edges <= 0 || n_edges as usize > PPR_MAX_EDGES {
        return Ok(HashMap::new());
    }
    // Undirected bipartite adjacency: file <-> entity (edges.from_path=file, to_path=entity).
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT from_path, to_path FROM edges")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows.flatten() {
            let (f, e) = row;
            adj.entry(f.clone()).or_default().push(e.clone());
            adj.entry(e).or_default().push(f);
        }
    }
    let seeds: Vec<String> = seed_entities
        .iter()
        .filter(|e| adj.contains_key(*e))
        .cloned()
        .collect();
    if seeds.is_empty() {
        return Ok(HashMap::new());
    }
    let restart = env_f64("SEMFS_PPR_RESTART", PPR_RESTART_DEFAULT).clamp(0.05, 0.95);
    let iters = env_usize("SEMFS_PPR_ITERS", PPR_ITERS_DEFAULT).min(200);
    let seed_mass = 1.0 / seeds.len() as f64;
    let mut seed_vec: HashMap<String, f64> = HashMap::with_capacity(seeds.len());
    for s in &seeds {
        seed_vec.insert(s.clone(), seed_mass);
    }
    // Power iteration: r = restart*seed + (1-restart) * (row-normalized adjacency) * r.
    let mut r = seed_vec.clone();
    for _ in 0..iters {
        let mut next: HashMap<String, f64> = HashMap::with_capacity(r.len() * 2);
        for (k, v) in &seed_vec {
            *next.entry(k.clone()).or_insert(0.0) += restart * v;
        }
        for (node, mass) in &r {
            let Some(nbrs) = adj.get(node) else { continue };
            if nbrs.is_empty() {
                continue;
            }
            let share = (1.0 - restart) * mass / nbrs.len() as f64;
            for nb in nbrs {
                *next.entry(nb.clone()).or_insert(0.0) += share;
            }
        }
        r = next;
    }
    // Restrict to candidate files and max-normalize to [0, 1].
    let mut out: HashMap<String, f64> = HashMap::new();
    let mut maxm = 0.0_f64;
    for f in candidate_files {
        if let Some(m) = r.get(f) {
            if *m > 0.0 {
                out.insert(f.clone(), *m);
                if *m > maxm {
                    maxm = *m;
                }
            }
        }
    }
    if maxm > 0.0 {
        for v in out.values_mut() {
            *v /= maxm;
        }
    }
    Ok(out)
}

fn reason_rank(reason: KgCandidateReason) -> usize {
    match reason {
        KgCandidateReason::DirectEntity => 0,
        KgCandidateReason::NeighborEntity => 1,
        KgCandidateReason::Community => 2,
    }
}

fn upsert_candidate(
    candidates: &mut HashMap<String, KgCandidate>,
    filepath: String,
    reason: KgCandidateReason,
    score: f64,
) {
    let bounded = score.clamp(0.0, MAX_PRIOR);
    if bounded <= 0.0 {
        return;
    }
    match candidates.get_mut(&filepath) {
        Some(existing) => {
            let replace = bounded > existing.score
                || ((bounded - existing.score).abs() < 1e-12
                    && reason_rank(reason) < reason_rank(existing.reason));
            if replace {
                existing.score = bounded;
                existing.reason = reason;
            }
        }
        None => {
            candidates.insert(
                filepath.clone(),
                KgCandidate {
                    filepath,
                    reason,
                    score: bounded,
                },
            );
        }
    }
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
    fn ppr_diffuses_to_multi_hop_only_files() {
        // Graph (file<->entity): revenue—{a,b}; a—{conversion_rate,q1}; q1—d; d—widget;
        // widget—c. Seeded at `revenue`, c.md is reachable ONLY multi-hop (rev→a→q1→d→
        // widget→c) — the 1-hop prior can't reach it, PPR can.
        let db = fixture_db();
        let conn = db.conn.lock();
        let seeds: HashSet<String> = ["/memories/revenue.md".to_string()].into_iter().collect();
        let candidates: HashSet<String> = ["/sales/a.md", "/sales/b.md", "/sales/c.md", "/sales/d.md"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let scores = ppr_file_scores(&conn, &seeds, &candidates).unwrap();
        let a = scores.get("/sales/a.md").copied().unwrap_or(0.0);
        let c = scores.get("/sales/c.md").copied().unwrap_or(0.0);
        let max = scores.values().copied().fold(0.0_f64, f64::max);
        assert!(a > 0.0, "direct file a.md must get PPR mass");
        assert!(c > 0.0, "multi-hop-only file c.md must get positive PPR mass (1-hop cannot)");
        assert!(a > c, "a direct file must outrank the far multi-hop file");
        assert!((max - 1.0).abs() < 1e-9, "scores must be max-normalized to 1.0");
    }

    #[test]
    fn ppr_empty_without_seed_in_graph() {
        let db = fixture_db();
        let conn = db.conn.lock();
        let seeds: HashSet<String> = ["/does/not/exist.md".to_string()].into_iter().collect();
        let candidates: HashSet<String> =
            ["/sales/a.md".to_string()].into_iter().collect();
        let scores = ppr_file_scores(&conn, &seeds, &candidates).unwrap();
        assert!(scores.is_empty(), "no seed in graph → empty → caller falls back to 1-hop");
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
    fn retrieval_enabled_parses_common_truthy_values() {
        std::env::set_var("SEMFS_HIDDEN_KG_RETRIEVAL", "on");
        assert!(retrieval_enabled());
        std::env::set_var("SEMFS_HIDDEN_KG_RETRIEVAL", "1");
        assert!(retrieval_enabled());
        std::env::set_var("SEMFS_HIDDEN_KG_RETRIEVAL", "true");
        assert!(retrieval_enabled());
        std::env::remove_var("SEMFS_HIDDEN_KG_RETRIEVAL");
        assert!(!retrieval_enabled());
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

    #[test]
    fn query_kg_candidates_returns_direct_community_and_neighbor_files() {
        let db = fixture_db();
        let conn = db.conn.lock();
        let result = query_kg_candidates(&conn, "revenue conversion rate", None, 16).unwrap();
        assert_eq!(result.matched_entities.len(), 2);
        assert_eq!(result.matched_communities, vec![1]);
        assert_eq!(result.candidates[0].filepath, "/sales/a.md");
        assert_eq!(result.candidates[0].reason, KgCandidateReason::DirectEntity);
        assert!(result
            .candidates
            .iter()
            .any(|c| c.filepath == "/sales/d.md" && c.reason == KgCandidateReason::NeighborEntity));
        assert!(result
            .candidates
            .iter()
            .any(|c| c.filepath == "/sales/b.md" && c.reason == KgCandidateReason::DirectEntity));
    }

    #[test]
    fn query_kg_candidates_honors_scope_prefix() {
        let db = fixture_db();
        let conn = db.conn.lock();
        let result = query_kg_candidates(&conn, "revenue", Some("/sales/c"), 16).unwrap();
        assert!(
            result.candidates.is_empty(),
            "scope should filter out non-matching graph candidates"
        );
    }

    #[test]
    fn query_kg_candidates_degrades_cleanly_without_graph_tables() {
        let conn = Connection::open_in_memory().unwrap();
        let result = query_kg_candidates(&conn, "revenue", None, 16).unwrap();
        assert!(result.candidates.is_empty());
        assert!(result.matched_entities.is_empty());
        assert!(result.matched_communities.is_empty());
    }
}
