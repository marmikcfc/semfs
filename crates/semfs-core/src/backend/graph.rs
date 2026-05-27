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
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_makes_url_safe_slugs() {
        assert_eq!(slugify("Auth Service"), "auth-service");
        assert_eq!(slugify("Acme, Inc."), "acme-inc");
        assert_eq!(entity_path("Stripe"), "/memories/stripe.md");
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
