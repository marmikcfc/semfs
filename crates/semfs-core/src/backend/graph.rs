//! L7 entity graph — LLM-driven extraction.
//!
//! gpt-4.1-nano extracts named entities (typed by a default workspace ontology)
//! from a file's content. Each entity becomes a `/memories/<slug>.md` node, and
//! the file gets a typed edge to it. Files that share entities are "related" —
//! that's the signal `SqliteVecStore::search` boosts. Re-derived on write,
//! removed on delete (mutable-FS — no temporal modeling).

use serde::Deserialize;

use crate::llm::LlmClient;

/// Default workspace ontology (the PM/biz/finance/dev common core). The LLM is
/// steered to classify entities into these types.
pub const ONTOLOGY: &[&str] = &[
    "Person",
    "Organization",
    "Project",
    "Decision",
    "Task",
    "Event",
    "Artifact",
    "Concept",
];

/// One extracted entity.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ExtractedEntity {
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: String,
}

#[derive(Deserialize)]
struct ExtractionResult {
    #[serde(default)]
    entities: Vec<ExtractedEntity>,
}

/// Extract typed entities from `content` via the LLM. Caller maps each to a
/// `/memories/<slug>.md` edge. **Fail-open**: callers treat `Err` as "no
/// entities" and never block a write on it.
pub fn extract_entities(client: &LlmClient, content: &str) -> anyhow::Result<Vec<ExtractedEntity>> {
    // Structured-output schema: the ontology is an enforced `type` enum, so the
    // model literally cannot emit an out-of-ontology type or malformed shape.
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "entities": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "type": { "type": "string", "enum": ONTOLOGY }
                    },
                    "required": ["name", "type"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["entities"],
        "additionalProperties": false
    });
    let system = "Extract the specific named entities from the user's text and classify each with one \
        of the allowed types. Include only real, specific entities (people, orgs, projects, decisions, \
        tools, etc.) — skip generic words. If there are none, return an empty list.";
    let raw = client.complete_structured(system, content, schema)?;
    // strict json_schema yields clean JSON; the fence strip is a harmless guard.
    let json = strip_code_fence(&raw);
    let parsed: ExtractionResult = serde_json::from_str(&json)
        .map_err(|e| anyhow::anyhow!("entity JSON parse failed: {e}; raw: {raw}"))?;
    Ok(parsed
        .entities
        .into_iter()
        .filter(|e| !e.name.trim().is_empty())
        .collect())
}

/// Relation ontology for entity→entity edges — graphify parity (`skill.md`).
/// Code: calls/implements/references/imports. Docs/papers:
/// cites/conceptually_related_to/semantically_similar_to/depends_on/contradicts/
/// mentions. Cross-domain: shares_data_with/part_of/relates_to.
pub const RELATION_TYPES: &[&str] = &[
    "calls",
    "implements",
    "references",
    "imports",
    "cites",
    "conceptually_related_to",
    "semantically_similar_to",
    "depends_on",
    "contradicts",
    "mentions",
    "shares_data_with",
    "part_of",
    "relates_to",
];

/// Confidence levels — graphify parity. EXTRACTED = explicit (score 1.0);
/// INFERRED = reasonable structural inference (0.4–0.9); AMBIGUOUS = uncertain,
/// flagged but INCLUDED, never omitted (0.1–0.3).
pub const CONFIDENCE_LEVELS: &[&str] = &["EXTRACTED", "INFERRED", "AMBIGUOUS"];

/// One typed entity→entity relationship. `source`/`target` are entity *names*
/// (the driver maps them to `/memories/<slug>.md` node paths).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ExtractedRelation {
    pub source: String,
    pub target: String,
    pub relation: String,
    #[serde(default)]
    pub confidence: String,
    #[serde(default)]
    pub confidence_score: f64,
}

/// A file's extracted knowledge graph: typed entities + typed entity↔entity
/// relations (graphify parity, vs the old entities-only co-mention model).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphExtraction {
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
}

#[derive(Deserialize)]
struct GraphExtractionRaw {
    #[serde(default)]
    entities: Vec<ExtractedEntity>,
    #[serde(default)]
    relations: Vec<ExtractedRelation>,
}

/// Keep only well-formed relations whose endpoints are BOTH extracted entities
/// (graphify: source/target are graph nodes) and that aren't self-loops. Pure,
/// so it is unit-testable without the LLM.
pub fn clean_relations(
    entities: &[ExtractedEntity],
    relations: Vec<ExtractedRelation>,
) -> Vec<ExtractedRelation> {
    let names: std::collections::HashSet<&str> =
        entities.iter().map(|e| e.name.trim()).filter(|n| !n.is_empty()).collect();
    relations
        .into_iter()
        .filter(|r| {
            let (s, t) = (r.source.trim(), r.target.trim());
            !s.is_empty() && !t.is_empty() && s != t && names.contains(s) && names.contains(t)
        })
        .collect()
}

/// Extract a typed knowledge graph (entities + entity→entity relations) from
/// `content` via the LLM — graphify-parity extraction. **Fail-open** like
/// [`extract_entities`]. The relation `source`/`target` reference entity names;
/// callers slugify them to `/memories/<slug>.md` node paths.
pub fn extract_graph(client: &LlmClient, content: &str) -> anyhow::Result<GraphExtraction> {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "entities": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "type": { "type": "string", "enum": ONTOLOGY }
                    },
                    "required": ["name", "type"],
                    "additionalProperties": false
                }
            },
            "relations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "source": { "type": "string" },
                        "target": { "type": "string" },
                        "relation": { "type": "string", "enum": RELATION_TYPES },
                        "confidence": { "type": "string", "enum": CONFIDENCE_LEVELS },
                        "confidence_score": { "type": "number" }
                    },
                    "required": ["source", "target", "relation", "confidence"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["entities", "relations"],
        "additionalProperties": false
    });
    let system = "Extract a knowledge graph from the user's text.\n\
        ENTITIES: specific named things (people, organizations, projects, decisions, tools, \
        artifacts, key domain concepts) — skip generic words; classify each with an allowed type.\n\
        RELATIONS: typed directed edges BETWEEN the entities you listed. `source` and `target` \
        MUST be names from your entities list. Use an allowed relation type. Set confidence: \
        EXTRACTED (confidence_score 1.0) when the relation is explicitly stated in the text; \
        INFERRED (0.4-0.9) for a reasonable inference; AMBIGUOUS (0.1-0.3) when uncertain — \
        INCLUDE it, do NOT omit. NEVER invent edges. Use semantically_similar_to only when the \
        similarity is genuinely non-obvious. Return empty lists if there is nothing. JSON only.";
    let raw = client.complete_structured(system, content, schema)?;
    let json = strip_code_fence(&raw);
    let parsed: GraphExtractionRaw = serde_json::from_str(&json)
        .map_err(|e| anyhow::anyhow!("graph JSON parse failed: {e}; raw: {raw}"))?;
    let entities: Vec<ExtractedEntity> = parsed
        .entities
        .into_iter()
        .filter(|e| !e.name.trim().is_empty())
        .collect();
    let relations = clean_relations(&entities, parsed.relations);
    Ok(GraphExtraction { entities, relations })
}

/// The memory-page path an entity maps to (its graph node).
pub fn entity_path(name: &str) -> String {
    format!("/memories/{}.md", slugify(name))
}

/// Tolerate models that wrap JSON in ```json … ``` fences.
fn strip_code_fence(s: &str) -> String {
    let t = s.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t);
    t.trim().to_string()
}

/// Convert an entity name to a URL-safe slug (`Auth Service` → `auth-service`).
///
/// Punctuation-only or non-ASCII names (`東京`, `🚀 Launch`) would otherwise
/// reduce to an empty slug and conflate distinct entities onto a single graph
/// node — which silently links unrelated files via the co-mention boost. When
/// the ASCII reduction is empty we fall back to a stable hash of the original
/// label so distinct names always map to distinct `to_path`s.
pub fn slugify(label: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in label.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() {
        format!("e-{:016x}", stable_hash(label))
    } else {
        slug
    }
}

/// FNV-1a — deterministic across runs and Rust versions (unlike `DefaultHasher`),
/// so the same entity name always hashes to the same slug.
fn stable_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(n: &str) -> ExtractedEntity {
        ExtractedEntity { name: n.into(), kind: "Concept".into() }
    }
    fn rel(s: &str, t: &str) -> ExtractedRelation {
        ExtractedRelation {
            source: s.into(),
            target: t.into(),
            relation: "relates_to".into(),
            confidence: "INFERRED".into(),
            confidence_score: 0.5,
        }
    }

    #[test]
    fn clean_relations_keeps_only_edges_between_known_entities() {
        let ents = vec![ent("Auth Service"), ent("Token Store")];
        let rels = vec![
            rel("Auth Service", "Token Store"), // both known → keep
            rel("Auth Service", "Unknown Thing"), // target unknown → drop
            rel("Auth Service", "Auth Service"), // self-loop → drop
            rel("", "Token Store"),              // empty endpoint → drop
        ];
        let out = clean_relations(&ents, rels);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "Auth Service");
        assert_eq!(out[0].target, "Token Store");
    }

    #[test]
    fn slugify_makes_url_safe_slugs() {
        assert_eq!(slugify("Auth Service"), "auth-service");
        assert_eq!(slugify("Acme, Inc."), "acme-inc");
        assert_eq!(entity_path("Stripe"), "/memories/stripe.md");
    }

    #[test]
    fn slugify_keeps_non_ascii_names_distinct() {
        // Names that reduce to an empty ASCII slug must not collapse onto the
        // same graph node — distinct names get distinct hash-based slugs.
        let tokyo = slugify("東京");
        let kyoto = slugify("京都");
        let rocket = slugify("🚀");
        assert!(!tokyo.is_empty() && !kyoto.is_empty() && !rocket.is_empty());
        assert_ne!(tokyo, kyoto);
        assert_ne!(tokyo, rocket);
        // Stable: same input → same slug across calls.
        assert_eq!(tokyo, slugify("東京"));
        assert_ne!(entity_path("東京"), entity_path("京都"));
    }

    #[test]
    fn strip_code_fence_handles_fenced_json() {
        assert_eq!(strip_code_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_code_fence("{\"a\":1}"), "{\"a\":1}");
    }

    /// Gated live test: OPENROUTER_API_KEY. The LLM finds the org + tool in prose
    /// that has no explicit links — something the old regex extractor couldn't do.
    #[test]
    fn extract_entities_finds_entities_in_prose() {
        let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
            eprintln!("skipping: OPENROUTER_API_KEY not set");
            return;
        };
        let client = LlmClient::openrouter(key);
        let ents = extract_entities(
            &client,
            "We decided to use Stripe for billing on the Phoenix project; Dana owns it.",
        )
        .unwrap();
        eprintln!("entities: {ents:?}");
        let names: Vec<String> = ents.iter().map(|e| e.name.to_lowercase()).collect();
        assert!(
            names.iter().any(|n| n.contains("stripe")),
            "expected Stripe among {names:?}"
        );
    }
}
