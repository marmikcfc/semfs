//! GLiNER2 knowledge-graph extractor (doc lane) — a **GPU-free, deterministic**
//! alternative to the LLM [`super::graph::extract_graph`]. Wraps the `gliner2`
//! crate (Candle backend, `fastino/gliner2-large-v1`) and produces the same
//! [`GraphExtraction`] so it drops into `build_kg`'s doc lane unchanged.
//!
//! Gated behind the `gliner-kg` feature (pulls Candle). Candle uses its own CPU
//! kernels — **no ONNX Runtime** — so it coexists with `fastembed` (ort) in one
//! binary with no `-sys` conflict.
//!
//! Entities + typed relations are extracted in **one combined-schema forward
//! pass**, so relation endpoints stay consistent with the entity set (verified
//! on the sftpgo probe: `clean_relations` kept 5/5, dropped 0). Two separate
//! calls would disagree on span boundaries and `clean_relations` would nuke
//! every edge.

use anyhow::{Context, Result};
use gliner2::config::{download_model, ExtractorConfig};
use gliner2::{
    CandleExtractor, EntityTypesInput, ExtractOptions, RelationTypesInput, Schema, SchemaTransformer,
};

use super::graph::{
    clean_relations, ExtractedEntity, ExtractedRelation, GraphExtraction,
};

/// Default GLiNER2 model (HuggingFace repo id). Override with `SEMFS_KG_GLINER_MODEL`
/// (e.g. `fastino/gliner2-base-v1` for a faster/smaller build).
const DEFAULT_MODEL: &str = "fastino/gliner2-large-v1";

/// Default **dev-workspace** entity labels — concrete, surface-level types GLiNER2
/// classifies well (unlike the abstract LLM `ONTOLOGY`, which it flounders on).
/// Covers code + PM/planning. Override per workspace with a comma-separated
/// `SEMFS_KG_GLINER_ENTITY_LABELS` (swap in a sales/ops/founder pack, no code change).
const DEFAULT_ENTITY_LABELS: &str = "person,team,company,customer,product,feature,ticket,epic,\
sprint,goal,software,service,component,module,library,database,codebase,bug,decision,document";

/// Default dev-workspace relation labels — natural phrases (GLiNER2 handles these
/// better than snake_case). Override with `SEMFS_KG_GLINER_RELATION_LABELS`.
const DEFAULT_RELATION_LABELS: &str = "assigned to,owns,part of,depends on,blocks,implements,\
calls,imports,uses,fixes,relates to,works on,requested by,affects,mentions";

/// Kind assigned to entities that appear only as a relation endpoint (see
/// [`union_relation_endpoints`]) — the model found them as a relation participant
/// but not in the entity pass, so we know they are entities but not their type.
const ENDPOINT_KIND: &str = "entity";

fn labels_from_env(var: &str, default: &str) -> Vec<String> {
    std::env::var(var)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn entity_labels() -> Vec<String> {
    labels_from_env("SEMFS_KG_GLINER_ENTITY_LABELS", DEFAULT_ENTITY_LABELS)
}

fn relation_labels() -> Vec<String> {
    labels_from_env("SEMFS_KG_GLINER_RELATION_LABELS", DEFAULT_RELATION_LABELS)
}

/// Add any relation endpoint that isn't already an extracted entity to the entity
/// set (typed [`ENDPOINT_KIND`]). GLiNER2's relation head/tail are entity mentions
/// by construction, but their spans can differ from the entity pass (`"sqlite"` vs
/// `"sqlite driver"`); without this, strict [`clean_relations`] drops ~half the
/// edges. After this, `clean_relations` only removes self-loops. Pure/testable.
fn union_relation_endpoints(entities: &mut Vec<ExtractedEntity>, relations: &[ExtractedRelation]) {
    let mut known: std::collections::HashSet<String> =
        entities.iter().map(|e| e.name.trim().to_string()).collect();
    for r in relations {
        for endpoint in [r.source.trim(), r.target.trim()] {
            if !endpoint.is_empty() && known.insert(endpoint.to_string()) {
                entities.push(ExtractedEntity {
                    name: endpoint.to_string(),
                    kind: ENDPOINT_KIND.to_string(),
                });
            }
        }
    }
}

/// A loaded GLiNER2 extractor: the Candle model + its schema/tokenizer transformer.
/// Load once (downloads + caches on first use, CPU), then reuse across files.
pub struct GlinerExtractor {
    extractor: CandleExtractor,
    transformer: SchemaTransformer,
}

impl GlinerExtractor {
    /// Load the model named by `SEMFS_KG_GLINER_MODEL` (default
    /// [`DEFAULT_MODEL`]). GPU-free; the first call downloads + caches the weights.
    pub fn load() -> Result<Self> {
        let repo =
            std::env::var("SEMFS_KG_GLINER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        Self::load_repo(&repo)
    }

    /// Load a specific GLiNER2 model by HF repo id.
    pub fn load_repo(repo: &str) -> Result<Self> {
        let files =
            download_model(repo).with_context(|| format!("download gliner2 model `{repo}`"))?;
        let transformer = SchemaTransformer::new(
            files
                .tokenizer
                .to_str()
                .context("gliner2 tokenizer path is not valid UTF-8")?,
        )?;
        let config: ExtractorConfig = serde_json::from_str(
            &std::fs::read_to_string(&files.config).context("read gliner2 config.json")?,
        )
        .context("parse gliner2 config.json")?;
        let vocab = transformer.tokenizer.get_vocab_size(true);
        let extractor = CandleExtractor::load_cpu(&files, config, vocab)
            .context("load gliner2 CandleExtractor")?;
        Ok(Self {
            extractor,
            transformer,
        })
    }

    /// Extract a typed KG (entities + typed entity→entity relations) from prose —
    /// the GLiNER2 analog of [`super::graph::extract_graph`]. One combined-schema
    /// forward pass over the workspace [`ONTOLOGY`] + [`RELATION_TYPES`]. **Fail-open**
    /// is the caller's job (treat `Err` as "no graph"), matching the LLM path.
    pub fn extract_graph(&self, content: &str) -> Result<GraphExtraction> {
        let mut schema = Schema::new();
        schema.entities(EntityTypesInput::Many(entity_labels()));
        schema.relations(RelationTypesInput::Many(relation_labels()));
        let (schema_val, meta) = schema.build();

        let out = self
            .extractor
            .extract(
                &self.transformer,
                content,
                &schema_val,
                &meta,
                &ExtractOptions::default(),
            )
            .context("gliner2 extract")?;
        let value = serde_json::to_value(&out).context("serialize gliner2 output")?;

        let (mut entities, relations) = parse_extraction(&value);
        // GLiNER2 relation endpoints are entities by construction — fold any whose
        // span differs from the entity pass back in, so clean_relations keeps them.
        union_relation_endpoints(&mut entities, &relations);
        let relations = clean_relations(&entities, relations);
        Ok(GraphExtraction {
            entities,
            relations,
        })
    }
}

/// Parse GLiNER2's combined-schema output into our typed entities + relations.
/// **Pure** (no model) so it is unit-testable. Expected shape (from a single
/// `extract()` over an entities+relations schema):
///
/// ```json
/// {
///   "entities": {"<Type>": ["name", ...] | [{"text": "name", ...}, ...]},
///   "relation_extraction": {"<relation>": [["head", "tail"], ...]}
/// }
/// ```
///
/// Entity `kind` is the schema label (a member of [`ONTOLOGY`]); relation names
/// are members of [`RELATION_TYPES`]. Relations are marked `EXTRACTED` / 1.0
/// (GLiNER2 already threshold-filters; surviving edges are treated as explicit).
fn parse_extraction(value: &serde_json::Value) -> (Vec<ExtractedEntity>, Vec<ExtractedRelation>) {
    let mut entities = Vec::new();
    if let Some(groups) = value.get("entities").and_then(|v| v.as_object()) {
        for (kind, items) in groups {
            let Some(items) = items.as_array() else {
                continue;
            };
            for item in items {
                // With ExtractOptions::default() names are bare strings; with
                // include_confidence/spans they are `{"text": ...}` objects.
                let name = item
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| item.get("text").and_then(|t| t.as_str()).map(str::to_string));
                if let Some(name) = name {
                    if !name.trim().is_empty() {
                        entities.push(ExtractedEntity {
                            name,
                            kind: kind.clone(),
                        });
                    }
                }
            }
        }
    }

    let mut relations = Vec::new();
    if let Some(groups) = value
        .get("relation_extraction")
        .and_then(|v| v.as_object())
    {
        for (relation, pairs) in groups {
            let Some(pairs) = pairs.as_array() else {
                continue;
            };
            for pair in pairs {
                let Some(pair) = pair.as_array() else {
                    continue;
                };
                let source = pair.first().and_then(|v| v.as_str()).unwrap_or("");
                let target = pair.get(1).and_then(|v| v.as_str()).unwrap_or("");
                if !source.trim().is_empty() && !target.trim().is_empty() {
                    relations.push(ExtractedRelation {
                        source: source.to_string(),
                        target: target.to_string(),
                        relation: relation.clone(),
                        confidence: "EXTRACTED".to_string(),
                        confidence_score: 1.0,
                    });
                }
            }
        }
    }

    (entities, relations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combined_output_and_keeps_consistent_relations() {
        // Mirrors the real sftpgo combined-schema output (bare-string entities).
        let value = serde_json::json!({
            "entities": {
                "Person": ["Nicola Murino"],
                "Concept": ["Go"],
                "Artifact": ["SFTPGo", "sqlite driver"],
                "Organization": []
            },
            "relation_extraction": {
                "part_of": [["sqlite driver", "SFTPGo"]],
                "references": [["SFTPGo", "Nicola Murino"]]
            }
        });
        let (entities, relations) = parse_extraction(&value);
        assert_eq!(entities.len(), 4);
        assert!(entities
            .iter()
            .any(|e| e.name == "Nicola Murino" && e.kind == "Person"));
        let relations = clean_relations(&entities, relations);
        assert_eq!(relations.len(), 2, "both endpoints are entities → kept");
        assert!(relations
            .iter()
            .any(|r| r.source == "SFTPGo" && r.target == "Nicola Murino" && r.relation == "references"));
    }

    #[test]
    fn drops_relation_to_unknown_endpoint() {
        let value = serde_json::json!({
            "entities": {"Artifact": ["A"]},
            "relation_extraction": {"calls": [["A", "B"]]} // B is not an entity
        });
        let (entities, relations) = parse_extraction(&value);
        let relations = clean_relations(&entities, relations);
        assert!(relations.is_empty(), "edge to unknown endpoint B is dropped");
    }

    #[test]
    fn accepts_object_form_entities_with_confidence() {
        let value = serde_json::json!({
            "entities": {"Artifact": [{"text": "SFTPGo", "confidence": 0.86}]},
            "relation_extraction": {}
        });
        let (entities, _) = parse_extraction(&value);
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].name, "SFTPGo");
        assert_eq!(entities[0].kind, "Artifact");
    }

    #[test]
    fn tolerates_missing_or_empty_sections() {
        let (e, r) = parse_extraction(&serde_json::json!({}));
        assert!(e.is_empty() && r.is_empty());
        let (e, r) = parse_extraction(&serde_json::json!({"entities": {}, "relation_extraction": {}}));
        assert!(e.is_empty() && r.is_empty());
    }

    #[test]
    fn union_recovers_endpoint_only_entities_so_clean_keeps_the_edge() {
        // entity pass gave "sqlite"; relation endpoints are the fuller spans.
        let mut entities = vec![ExtractedEntity {
            name: "sqlite".into(),
            kind: "database".into(),
        }];
        let relations = vec![ExtractedRelation {
            source: "dataprovider".into(),
            target: "sqlite driver".into(),
            relation: "depends on".into(),
            confidence: "EXTRACTED".into(),
            confidence_score: 1.0,
        }];
        union_relation_endpoints(&mut entities, &relations);
        assert!(entities
            .iter()
            .any(|e| e.name == "dataprovider" && e.kind == ENDPOINT_KIND));
        assert!(entities
            .iter()
            .any(|e| e.name == "sqlite driver" && e.kind == ENDPOINT_KIND));
        // with endpoints now entities, clean_relations keeps the edge (was dropped before)
        assert_eq!(clean_relations(&entities, relations).len(), 1);
    }

    #[test]
    fn dev_pack_labels_parse_and_are_nonempty() {
        assert!(entity_labels().contains(&"ticket".to_string()));
        assert!(entity_labels().contains(&"service".to_string()));
        assert!(relation_labels().contains(&"depends on".to_string()));
        assert!(entity_labels().len() >= 15 && relation_labels().len() >= 10);
    }

    /// Real end-to-end: load the actual GLiNER2 model and run the full
    /// inference → parse → clean path over the workspace ONTOLOGY + RELATION_TYPES.
    /// Ignored by default (downloads ~1.7 GB on first run); run explicitly:
    /// `cargo test -p semfs-core --features gliner-kg -- --ignored e2e`.
    #[test]
    #[ignore = "downloads the gliner2 model + runs CPU inference"]
    fn e2e_extracts_graph_from_sftpgo_prose() {
        let gliner = GlinerExtractor::load().expect("load gliner2 model");
        let text = "SFTPGo is an SFTP server written in Go, created by Nicola Murino. \
                    The dataprovider package depends on the sqlite driver and calls the \
                    migration module. The httpd server authenticates requests with the JWT middleware.";
        let g = gliner.extract_graph(text).expect("extract_graph");
        eprintln!("e2e entities={:?}", g.entities);
        eprintln!("e2e relations={:?}", g.relations);
        assert!(!g.entities.is_empty(), "expected entities from sftpgo prose");
        assert!(!g.relations.is_empty(), "expected relations from sftpgo prose");
        // every surviving relation's endpoints must be extracted entities (clean_relations invariant)
        let names: std::collections::HashSet<&str> =
            g.entities.iter().map(|e| e.name.as_str()).collect();
        for r in &g.relations {
            assert!(names.contains(r.source.as_str()) && names.contains(r.target.as_str()));
        }
    }
}
