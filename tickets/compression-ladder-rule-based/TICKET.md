# Ticket: Rule-based compression + a key-agnostic compression ladder

**Folder:** `tickets/compression-ladder-rule-based/`
**Origin:** evo `/optimize` glm-5.1 session (2026-06-17) — see `.evo/project.md`, `evo-glm51-grep-delivery-result` memory.
**Routing:** mirror to Linear (team `SemFS`, key `SEM`) per CLAUDE.md §0; keep this folder name in the issue body.

## Why

`SEMFS_GREP_COMPRESS` today makes a **per-query generative LLM call on semfs's own `OPENROUTER_API_KEY`**
(default `gpt-4.1-mini`). Three problems surfaced in the evo run:
1. It needs **semfs's own key** — friction for a plug-and-play install.
2. It's **per-grep + synchronous** → latency → timeouts on grep-heavy cases (case 386 timed out).
3. **Generative rewrite can drop/reword a graded value** (the exact thing the rubric checks).

Goal: make compression **key-agnostic, cheap, and safe-by-default** — reuse whatever key the user
already has, fall back to a local no-key method, and never corrupt a value.

## Deliverable 1 — Compression backend ladder (priority order)

Select the compression backend at runtime by availability, highest first:

1. **`SEMFS_API_KEY`** → call semfs's hosted compression service (our endpoint). *Highest priority.*
2. **Installed harness CLI (NO key needed — reuses the CLI's own auth):** prefer **`codex exec "<prompt>"`**;
   if codex is not installed/authenticated, fall back to **`claude -p "<prompt>"`**. Pass a small-model flag.
   **HARD CONSTRAINT: ingest/seed-build or session-cache ONLY — NEVER per-grep.** Spawning a CLI process
   per hit adds startup + round-trip latency that caused the query-time timeouts (case 386). Fine when
   paid once per file; fatal on the agent's critical path.
3. **Cloud key present** → direct API call with a small model, in order:
   - `ANTHROPIC_API_KEY` → Claude Haiku · `OPENAI_API_KEY` → `gpt-4.1-mini`/`gpt-5-mini` · `OPENROUTER_API_KEY` → current
4. **None present** → **local, no-key**: rule-based compression (Deliverable 2); later, an optional
   small **local scorer** model (LLMLingua/Bear-2 style — *deletes* low-value tokens, never rewrites).

Fail-open at every tier (a failure → render the original excerpt, never block the grep).
Knob to force a tier / disable: `SEMFS_GREP_COMPRESS` (off|on|rules|cli|cloud|api).
**Query-time vs ingest-time:** at *query* time prefer only the fast tiers (cached result, or local rules);
the CLI-shell + cloud tiers populate the cache at *ingest/first-access*, not per grep.

## Deliverable 2 — Rule-based compression (the no-key default; experiment)

Deterministic, extractive, **verbatim-safe** (copies lines, never rewrites → cannot corrupt a value).

- **REMOVE (filler):** qualifier/intensifier words (*approximately, roughly, basically, proposed,
  detailed, various*), hedging/meta phrases (*"it should be noted," "please note," "we are pleased
  to," "after extensive deliberation"*), back-references (*"as mentioned above"*), pleasantries/
  boilerplate, exact repetition, redundant whitespace.
- **PRESERVE verbatim:** any line/token with a digit (numbers/dates/money/%/versions), identifiers
  (`DES-0006`, `PO_4`), `key: value` lines, table rows, proper names, decisions/commitments.
- Two flavors to test: **word-level** (strip filler words in place, keep sentences — matches Bear-2
  "Low") vs **line-level** (keep value-bearing lines, drop pure-filler paragraphs — more aggressive).
- **Benchmark target:** match Bear-2 "Low" (≈ obvious-filler removal, accuracy-neutral-or-up). Ref:
  thetokencompany.com/blog/coqa (Bear-2 Low τ=0.05 ≈ 0.1% cut +1.3pp acc; Medium τ=0.2 ≈ 8.2% cut).

## Deliverable 3 — Code-file compress gate (safety, ships now)

`is_spreadsheet_ext` exempts `.xlsx/.csv/...` but NOT code. Add a code-extension exemption (`.py
.rs .js .ts .go .java .c .cpp .h .rb .php .sh .sql ...`, incl. `.extracted.md` siblings) so source
files are **never** sent to the compressor. Small, surgical, independent — ship regardless of 1/2.

## Success criteria

- [ ] Ladder selects the right backend by env; fail-open verified at each tier; no key → rule-based runs.
- [ ] Rule-based: ≥X% token reduction on chanpin prose excerpts with **0 dropped graded values**
      (assert via regex that every digit/date/ID present in input survives in output).
- [ ] Code files never compressed (gate test).
- [ ] A/B on E2B (cases 53+171, glm-5.1) vs current generative compress: token + accuracy parity or better,
      and **no per-grep latency timeouts**.

## Notes / open questions

- The small **local scorer** (LLMLingua/Bear-2 style) is the verbatim-safe upgrade over generative — a
  *ranker* not a generator (your ONNX instinct, repurposed). Likely candle/GGUF in Rust, not ONNX-genai.
- Best durable design (separate): move compression to **ingest/seed-build time** (compress once, store a
  telegraphic sidecar) so there is zero per-query LLM call — kills the latency entirely.
- `SEMFS_API_KEY` hosted tier implies a service + sends user file content to our servers (privacy) — scope separately.
