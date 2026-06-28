//! Summary-augmented retrieval for tabular content.
//!
//! Bare number cells embed to nothing, so a data table is far harder to retrieve
//! than its relevance warrants (see `tickets/summary-augmented-table-retrieval/`).
//! For each spreadsheet sheet we generate a short natural-language summary with
//! `gpt-4.1-mini` and weave it ahead of the raw cells. The summary becomes the
//! embedded retrieval key (and the reranker's input — short prose fits its
//! window); the raw table is still returned verbatim so the agent computes the
//! answer from ground truth, never from a possibly-hallucinated summary.
//!
//! Key-gated on `OPENROUTER_API_KEY` exactly like OCR: no key ⇒ fall back to the
//! flattened raw cells (today's behavior), never blocking the import. Summaries
//! are cached by content hash so re-seeds are cheap and reproducible.

use std::path::PathBuf;

/// On-disk content-addressed cache of per-sheet summaries. One file per key
/// (`<dir>/<hash>`), so re-summarizing an unchanged table is a cheap file read
/// instead of an LLM call — and re-seeds reproduce byte-identical content.
struct SummaryCache {
    dir: PathBuf,
}

impl SummaryCache {
    fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Return the cached summary for `key`, or `None` if absent/unreadable.
    fn get(&self, key: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(key)).ok()
    }

    /// Persist `summary` under `key`. Best-effort: a write failure (e.g. no cache
    /// dir) just means the next run re-summarizes — never an error to the caller.
    fn put(&self, key: &str, summary: &str) {
        let _ = std::fs::create_dir_all(&self.dir);
        if let Err(e) = std::fs::write(self.dir.join(key), summary) {
            tracing::debug!(key, "summary cache write failed (will re-summarize): {e}");
        }
    }
}

/// Produce the searchable content for a workbook's sheets, summarizing each
/// sheet's table with `gpt-4.1-mini` to make numeric data findable. Key-gated on
/// `OPENROUTER_API_KEY`: no key ⇒ falls back to flattened raw cells. `None` only
/// when there are no sheets (caller accounts it as unindexed). The blocking LLM
/// calls mean the caller MUST run this on the blocking pool.
pub fn summarize_workbook(
    filepath: &str,
    sheets: &[crate::extract::spreadsheet::Sheet],
) -> Option<String> {
    let cache = default_cache();
    summarize_with_key(api_key(), filepath, sheets, cache.as_ref())
}

/// The configured OpenRouter key, or `None` if unset/blank (mirrors `ocr.rs`).
fn api_key() -> Option<String> {
    std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
}

/// Default on-disk cache (`<os-cache>/semfs/summaries/`), or `None` if no cache
/// dir is resolvable — in which case summaries simply aren't persisted.
fn default_cache() -> Option<SummaryCache> {
    directories::ProjectDirs::from("", "", "semfs")
        .map(|d| SummaryCache::new(d.cache_dir().join("summaries")))
}

/// Gate + cache + summarize, split from env/network so the key-gate is testable.
/// With a key: each sheet's summary is served from cache or generated then
/// cached. Without a key: every sheet falls back to its raw cells.
fn summarize_with_key(
    key: Option<String>,
    filepath: &str,
    sheets: &[crate::extract::spreadsheet::Sheet],
    cache: Option<&SummaryCache>,
) -> Option<String> {
    let (base, model, key_override) = summary_endpoint();
    // Endpoint key (SEMFS_SUMMARY_LLM_KEY) wins; else the caller's key (OPENROUTER).
    let key = key_override.or(key).filter(|k| !k.trim().is_empty());
    build_content(sheets, |name, text| {
        let key = key.as_deref()?; // no key ⇒ None ⇒ raw-cell fallback
        let ck = cache_key(&model, filepath, name, text);
        if let Some(c) = cache {
            if let Some(hit) = c.get(&ck) {
                return Some(hit);
            }
        }
        let summary = summarize_one(&base, &model, key, filepath, name, text)?;
        if let Some(c) = cache {
            c.put(&ck, &summary);
        }
        Some(summary)
    })
}

/// Cap on the cell text we send to the summarizer. A long sheet's head is enough
/// for the model to describe the table; sending megabytes wastes tokens/latency.
const MAX_SUMMARY_INPUT_BYTES: usize = 16 * 1024;

const SUMMARY_SYSTEM: &str = "You write one short retrieval summary (2–4 sentences) for a \
spreadsheet sheet so a search engine can match user questions to it. Describe WHAT KIND of \
information the sheet conveys and WHAT CAN BE FOUND OUT from it, about which entities — i.e. \
the topics, metrics, and entities it covers and the questions it could answer (for example \
'which products sell best', 'sales by region', 'monthly revenue trend'). Use the file path \
and sheet name as strong hints to the domain. Name the temporal granularity (daily, weekly, \
monthly, quarterly, yearly, fiscal periods) when present. Do NOT compute, rank, or state any \
specific answer, total, extreme, or value — describe the information that is available, never \
a conclusion drawn from it. Use natural language a user might search for, and output only the \
summary with no preamble.";

/// Summarize one sheet's cell text via `gpt-4.1-mini`. `None` on key-absent
/// (caller already gated), empty input, or any request failure — the caller then
/// falls back to the raw cells, so a flaky summarizer never blocks indexing.
fn summarize_one(
    base_url: &str,
    model: &str,
    key: &str,
    filepath: &str,
    sheet_name: &str,
    table_text: &str,
) -> Option<String> {
    if table_text.trim().is_empty() {
        return None;
    }
    let head = head_bytes(table_text, MAX_SUMMARY_INPUT_BYTES);
    // File path/title is a strong domain hint (often more telling than sparse cells).
    let user = format!("File path: {filepath}\nSheet name: {sheet_name}\n\nCells:\n{head}");
    let body = serde_json::json!({
        "model": model,
        "temperature": 0.0,
        "max_tokens": 256,
        "messages": [
            { "role": "system", "content": SUMMARY_SYSTEM },
            { "role": "user", "content": user },
        ],
    });
    let resp: serde_json::Value = crate::http::timeout_agent()
        .post(&format!("{}/chat/completions", base_url.trim_end_matches('/')))
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .ok()?
        .into_json()
        .ok()?;
    let text = resp["choices"][0]["message"]["content"]
        .as_str()?
        .trim()
        .to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Borrow at most `max` bytes of `s`, truncated to a UTF-8 char boundary.
fn head_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Build the INDEXED (embedded) content for a workbook's sheets. `summarize(name,
/// text)` returns a sheet's summary, or `None` to skip (no key, cache miss + LLM
/// failure, etc.).
///
/// WEAVE: when a summary exists we index the **summary followed by the raw cells**
/// — the summary is the semantic retrieval key (makes the table findable), and the
/// raw cells preserve the file's chunk-mass + ground-truth content for `cat`/answers.
/// This deliberately replaces the old **embed-only** behavior, which A/B'd WORST on
/// case 289 (RRF #72) precisely because stripping the raw cells stripped the table's
/// chunk-mass (`tickets/summary-augmented-table-retrieval/` OUTCOME +
/// `rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md`). An un-summarized
/// sheet falls back to its raw cells (no-key path, no regression). `None` only when
/// there are no sheets.
fn build_content<F>(sheets: &[crate::extract::spreadsheet::Sheet], summarize: F) -> Option<String>
where
    F: Fn(&str, &str) -> Option<String>,
{
    if sheets.is_empty() {
        return None;
    }
    let parts: Vec<String> = sheets
        .iter()
        .map(|s| match summarize(&s.name, &s.text) {
            Some(sum) => format!("{sum}\n{}", s.text), // weave: summary key + raw cells
            None => s.text.clone(),
        })
        .collect();
    Some(parts.join("\n"))
}

/// Model + prompt the cache key is bound to: bump `PROMPT_VERSION` whenever the
/// prompt or model changes so stale summaries don't survive a re-seed.
const SUMMARY_MODEL: &str = "openai/gpt-4.1-mini";
/// Bump on ANY prompt/model/input change so stale summaries don't survive a
/// re-seed. v2: coverage-style prompt ("what info is conveyed / can be found
/// out", no computed answers) + file path & sheet name now fed to the model.
const PROMPT_VERSION: u32 = 2;

fn nonempty(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Resolve the summary LLM endpoint from optional overrides, falling back to
/// OpenRouter + `gpt-4.1-mini` so the path is byte-identical when unset. Pure
/// (no env) so it's unit-testable; `summary_endpoint` reads the env into it.
fn resolve_summary_endpoint(
    base: Option<String>,
    model: Option<String>,
    key: Option<String>,
) -> (String, String, Option<String>) {
    (
        base.and_then(nonempty)
            .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string()),
        model.and_then(nonempty).unwrap_or_else(|| SUMMARY_MODEL.to_string()),
        key.and_then(nonempty),
    )
}

/// Summary LLM endpoint from `SEMFS_SUMMARY_LLM_{BASE_URL,MODEL,KEY}` (mirrors
/// `build_kg`'s `SEMFS_GRAPH_LLM_*`). Lets a self-hosted Qwen drop in via config;
/// unset ⇒ the existing OpenRouter + gpt-4.1-mini path.
fn summary_endpoint() -> (String, String, Option<String>) {
    resolve_summary_endpoint(
        std::env::var("SEMFS_SUMMARY_LLM_BASE_URL").ok(),
        std::env::var("SEMFS_SUMMARY_LLM_MODEL").ok(),
        std::env::var("SEMFS_SUMMARY_LLM_KEY").ok(),
    )
}

/// Content-addressed cache key for a sheet's summary: hashes the model + prompt
/// version + file path + sheet name + cell text. The path/sheet are now inputs
/// to the summary, so they must key the cache; a prompt/model bump invalidates
/// every entry. `model` is the RESOLVED model so a Qwen re-seed never reuses
/// gpt-4.1-mini-cached summaries.
fn cache_key(model: &str, filepath: &str, sheet: &str, table_text: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(model.as_bytes());
    h.update(&PROMPT_VERSION.to_le_bytes());
    h.update(b"\0");
    h.update(filepath.as_bytes());
    h.update(b"\0");
    h.update(sheet.as_bytes());
    h.update(b"\0");
    h.update(table_text.as_bytes());
    h.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::spreadsheet::Sheet;

    #[test]
    fn cache_key_is_stable_and_content_sensitive() {
        let m = "openai/gpt-4.1-mini";
        let a = cache_key(m, "/f.xlsx", "S1", "Widget\t1978\n");
        assert_eq!(
            a,
            cache_key(m, "/f.xlsx", "S1", "Widget\t1978\n"),
            "same inputs → same key"
        );
        assert_ne!(
            a,
            cache_key(m, "/f.xlsx", "S1", "Widget\t1979\n"),
            "changed cell → new key"
        );
        assert_ne!(
            a,
            cache_key(m, "/other.xlsx", "S1", "Widget\t1978\n"),
            "changed path → new key"
        );
        assert_ne!(
            a,
            cache_key(m, "/f.xlsx", "S2", "Widget\t1978\n"),
            "changed sheet → new key"
        );
        assert_ne!(
            a,
            cache_key("qwen3.6-27b", "/f.xlsx", "S1", "Widget\t1978\n"),
            "changed model → new key (Qwen re-seed won't reuse gpt-4.1-mini summaries)"
        );
        // 32-byte blake3 → 64 hex chars.
        assert_eq!(a.len(), 64, "expected hex-encoded blake3 digest");
    }

    #[test]
    fn resolve_summary_endpoint_falls_back_then_overrides() {
        // Unset ⇒ existing OpenRouter + gpt-4.1-mini path (no regression).
        let (b, m, k) = resolve_summary_endpoint(None, None, None);
        assert_eq!(b, "https://openrouter.ai/api/v1");
        assert_eq!(m, SUMMARY_MODEL);
        assert_eq!(k, None);
        // Set ⇒ a self-hosted Qwen drops in via config.
        let (b, m, k) = resolve_summary_endpoint(
            Some("http://qwen:8000/v1".into()),
            Some("qwen3.6-27b".into()),
            Some("sk-x".into()),
        );
        assert_eq!(b, "http://qwen:8000/v1");
        assert_eq!(m, "qwen3.6-27b");
        assert_eq!(k.as_deref(), Some("sk-x"));
        // Blank/whitespace overrides fall back, never produce an empty endpoint/key.
        let (b, _, k) = resolve_summary_endpoint(Some("  ".into()), None, Some(String::new()));
        assert_eq!(b, "https://openrouter.ai/api/v1");
        assert_eq!(k, None);
    }

    #[test]
    fn cache_round_trips_and_misses_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SummaryCache::new(dir.path().to_path_buf());
        assert_eq!(cache.get("k1"), None, "absent key → None");
        cache.put("k1", "a summary");
        assert_eq!(cache.get("k1"), Some("a summary".to_string()));
    }

    fn sheet(name: &str, text: &str) -> Sheet {
        Sheet {
            name: name.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn build_content_falls_back_to_raw_cells_when_summary_absent() {
        // When the summarizer yields None for every sheet (no key, cache miss +
        // LLM failure), the output is the raw cells verbatim — no summary marker,
        // matching today's flattened extraction so the no-key path never regresses.
        let sheets = vec![sheet("S1", "a\t1\n"), sheet("S2", "b\t2\n")];
        let out = build_content(&sheets, |_, _| None).unwrap();
        assert!(
            out.contains("a\t1") && out.contains("b\t2"),
            "raw cells present"
        );
        assert!(
            !out.contains("summary:"),
            "no summary marker on the fallback path"
        );
    }

    #[test]
    fn build_content_weaves_summary_and_raw_cells() {
        // WEAVE: a summarized sheet's indexed text is the SUMMARY (retrieval key)
        // followed by the RAW cells (chunk-mass + ground truth). NOT embed-only —
        // keeping the raw cells is what avoids the #72 chunk-mass collapse.
        let sheets = vec![sheet("Sales", "Widget\t1978\n")];
        let out = build_content(&sheets, |name, _| {
            Some(format!("{name}: product sales coverage"))
        })
        .unwrap();
        assert!(out.contains("product sales coverage"), "summary present");
        assert!(out.contains("1978"), "raw cells kept (weave, not embed-only)");
    }

    #[test]
    fn build_content_none_when_no_sheets() {
        assert_eq!(build_content(&[], |_, _| Some("x".into())), None);
    }

    #[test]
    fn no_key_falls_back_to_raw_cells_without_network() {
        // The acceptance criterion mirrors OCR: absent key ⇒ raw cells, never a
        // network call. (summarize_one is unreachable when the gate is closed.)
        let sheets = vec![sheet("S1", "Widget\t1978\n")];
        let out = summarize_with_key(None, "/f.xlsx", &sheets, None).unwrap();
        assert!(out.contains("1978"), "no-key path indexes raw cells");
    }

    #[test]
    fn warm_cache_is_used_and_avoids_the_network() {
        // With a key set but a pre-warmed cache, the cached summary is used and
        // the (bogus-key) network is never hit — proving the cache short circuits.
        // A miss would fall through to a failing request → raw-cell fallback.
        let dir = tempfile::tempdir().unwrap();
        let cache = SummaryCache::new(dir.path().to_path_buf());
        let fp = "/f.xlsx";
        let table = "Widget\t1978\n";
        cache.put(
            &cache_key(SUMMARY_MODEL, fp, "Sales", table),
            "CACHED product-sales coverage summary",
        );
        let sheets = vec![sheet("Sales", table)];
        let out = summarize_with_key(Some("bogus-key".into()), fp, &sheets, Some(&cache)).unwrap();
        assert!(
            out.contains("CACHED product-sales coverage summary"),
            "cache hit used"
        );
        assert!(
            out.contains("1978"),
            "weave: cached summary is woven WITH the raw cells, not replacing them"
        );
    }

    #[test]
    fn head_bytes_truncates_on_char_boundary() {
        let big = "中".repeat(100); // 3 bytes each
        let h = head_bytes(&big, 10);
        assert!(h.len() <= 10);
        assert!(
            h.chars().all(|c| c == '中'),
            "must not split a multibyte char"
        );
    }

    /// Live test (skips without `OPENROUTER_API_KEY`): a numeric sheet summarizes
    /// to non-empty prose that the raw cells alone could never produce.
    #[test]
    fn summarize_one_produces_prose_live() {
        if api_key().is_none() {
            eprintln!("skipping: OPENROUTER_API_KEY not set");
            return;
        }
        let key = api_key().unwrap();
        let s = summarize_one(
            "https://openrouter.ai/api/v1",
            SUMMARY_MODEL,
            &key,
            "/desktop/product-sales/q3_sales.xlsx",
            "Q3 Sales",
            "Product\tUnits\nWidget\t1978\nGadget\t1335\n",
        )
        .expect("summary with a key set");
        assert!(!s.trim().is_empty(), "summary should be non-empty prose");
    }
}
