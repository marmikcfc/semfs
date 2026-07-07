//! Per-model $/million-token pricing, used only to estimate the $ delta
//! between the model Claude Code originally asked for and the model routing
//! actually picked. These are EXAMPLE DEFAULTS, not verified live prices —
//! Anthropic's actual current rates should be confirmed and this table
//! updated (or overridden via `TOKOPT_PRICING_JSON`) before trusting the
//! dollar figures `/usage` reports for anything beyond a rough sense of scale.

use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy)]
pub struct ModelPrice {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

/// Placeholder figures — update against real, current Anthropic pricing.
static DEFAULT_PRICING: &[(&str, ModelPrice)] = &[
    ("claude-haiku-4-5-20251001", ModelPrice { input_per_mtok: 1.0, output_per_mtok: 5.0 }),
    ("claude-sonnet-5", ModelPrice { input_per_mtok: 3.0, output_per_mtok: 15.0 }),
    ("claude-opus-4-8", ModelPrice { input_per_mtok: 15.0, output_per_mtok: 75.0 }),
];

static PRICING: LazyLock<HashMap<String, ModelPrice>> = LazyLock::new(|| {
    if let Ok(path) = std::env::var("TOKOPT_PRICING_JSON") {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(parsed) = serde_json::from_str::<HashMap<String, ModelPrice>>(&contents) {
                return parsed;
            }
        }
    }
    DEFAULT_PRICING.iter().map(|(k, v)| (k.to_string(), *v)).collect()
});

impl<'de> serde::Deserialize<'de> for ModelPrice {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Raw {
            input_per_mtok: f64,
            output_per_mtok: f64,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(ModelPrice { input_per_mtok: raw.input_per_mtok, output_per_mtok: raw.output_per_mtok })
    }
}

pub fn price_for(model: &str) -> Option<ModelPrice> {
    PRICING.get(model).copied()
}

/// Cost in USD for the given real token counts under `model`'s pricing, or
/// `None` if the model isn't in the table (unpriced models don't get compared).
pub fn cost_usd(model: &str, input_tokens: i64, output_tokens: i64) -> Option<f64> {
    let p = price_for(model)?;
    Some(
        (input_tokens as f64 / 1_000_000.0) * p.input_per_mtok
            + (output_tokens as f64 / 1_000_000.0) * p.output_per_mtok,
    )
}
