# Build plan — fresh seeds (SEM-38)

Implementation map for `REQUIREMENTS.md`. Machinery is ~45% there; KG is **done**. Everything below is
**additive** (~850–1200 LOC), TDD per component (`superpowers:test-driven-development`).

Entry point that already exists: **`benchmarks/modal/semfs_modal.py:893 build_corpus_seed(corpus, out)`**
= seed_dir → build_kg → materialize_kg (resume-aware, shard workers). We extend it with the new flags.

## Components, status, entry points (from code map)

| # | Component | Status | Key file:line | Gap (LOC) |
|---|---|---|---|---|
| C1 | **Pluggable LLM endpoints** (summary + vision via env) | 🔴 hardcoded OpenRouter | `extract/summary.rs:131`, `extract/ocr.rs`, `llm.rs:17` (already pluggable) | ~120 |
| C2 | **csv/json summarizer** routing | 🔴 xlsx-only | `extract/summary.rs:49`, `extract/mod.rs:78` | ~220 |
| C3 | **Image summary via Qwen-VL** (OpenRouter) | 🟡 vision tier exists, gpt-4.1-mini hardwired | `extract/ocr.rs`, `extract/mod.rs:131,207` | ~130 |
| C4 | **Gemma code lane** | 🟡 lane exists, defaults to **jina** | `cmd/resolve.rs:25,203`, `sqlite_vec.rs:593`, seed_dir env `:842` | ~50 (knob) **+ AST chunking ~350 (D-AST)** |
| C5 | **Offline `fs_*` builder** (`materialize_fs.rs`) | 🔴 only daemon/FUSE writes it | `cache/db.rs` fs_* schema, `cache/fs.rs:118` | ~180 |
| C6 | **Failure ledger** | 🔴 per-file logs only, not aggregated | seed_dir loop, `sqlite_vec.rs:index`, build_kg | ~130 |
| C7 | **Modal wiring** — thread new env/flags into `build_corpus_seed` + add `build_chanpin_seed` | 🟡 kaifa-specific exists | `semfs_modal.py:812,893` | ~60 |
| — | **KG** (entity + embedding-kNN k=6 + Leiden over kNN graph + god-nodes) | ✅ **DONE** | `cache/graph_file.rs:74,164,223`, `examples/build_kg.rs`, `materialize_kg.rs` | 0 |

## Build order (dependency-aware)

1. **C1** pluggable endpoints (everything downstream needs it). Env: `SEMFS_SUMMARY_LLM_{BASE_URL,MODEL,KEY}` (→ self-hosted Qwen3.6-27B vLLM) + `SEMFS_VISION_LLM_{BASE_URL,MODEL,KEY}` (→ OpenRouter Qwen-VL). Fall back to OpenRouter when unset (no regression).
2. **C2** csv/json summarizer (new `extract/csv.rs`+`json.rs` → reuse `summarize_with_key` weave path).
3. **C3** image → Qwen-VL caption via C1's vision endpoint → woven summary chunk.
4. **C4** code lane: add `SEMFS_CODE_EMBED_MODEL=gemma-q4` knob so `build_code_embedder` uses gemma. **AST chunking = D-AST decision** (see below).
5. **C5** `materialize_fs.rs` — walk corpus, build `fs_inode/fs_dentry/fs_data` full-content, NO re-embed. Idempotent. (This is the exact thing whose absence made the kaifa mount flail.)
6. **C6** failure ledger — `EMBED_FAILURES.<seed>.jsonl` ({filepath, stage, reason, bytes, ts}); reconcile `indexed+failed+skipped == walked`.
7. **C7** Modal: thread C1–C6 flags through `build_corpus_seed`; add a chanpin entry point.
8. **Build** both seeds (gemma embed = no GPU; Qwen RTX-PRO-6000 up only for summary + KG passes).
9. **Verify** (REQUIREMENTS §5) → **bake** `semfs-baked-{chanpin,kaifa}` → **E2B mount smoke** → **export** to Drive.

## Open scope decision

- **D-AST — code chunking depth.** C4 minimal (~50 LOC) = code goes into the **gemma code lane** but with
  the existing recursive chunker (still satisfies "not text-lane'd, embedded with gemma"). Full **tree-sitter
  AST chunking** (~350 LOC, reuse `backend/graph_ast.rs`'s 14-lang machinery) gives function/class-aligned
  code chunks — better code retrieval, but it's ~⅓ of the whole build. **Pick: minimal gemma-lane now (AST as
  a follow-up), or full AST now?**

## Per-component acceptance (TDD)
Each Cn: failing test first (e.g. "csv routed through summarizer emits a summary chunk + raw rows", "code
file lands in vchunks_code not vchunks", "materialize_fs builds a browsable tree + full cat", "a failed
embed appears in the ledger"), then minimal code to pass, crate suite + clippy green before moving on.
