# Feature: Caveman output-token compression — make the agent's *replies* terse

- **Type:** Feature / token efficiency (reduce the downstream agent's generated tokens)
- **Status:** PROPOSED — design approved 2026-06-11; not yet implemented.
- **Created:** 2026-06-11
- **Component:** `semfs-core::agent_hint` (the only place semfs instructs the agent).
  No change to retrieval, storage, ranking, or the bytes `semfs grep` returns.
- **Branch context:** `feat/backend-agnostic-store`
- **Inspired by:** [juliusbrussee/caveman](https://github.com/juliusbrussee/caveman) — a
  prompt-engineering skill that tells an LLM to drop filler and speak telegraphically
  ("why use many token when few token do trick"), cutting ~65% of *generated* tokens
  while preserving technical accuracy. **It is not byte compression** — there is no
  decode step; the model just writes terser, still-readable prose.

## Problem this solves

In the Workspace-Bench matrix, an arm's cost is `agent input tokens + agent output tokens`.
A lot of work has gone into the *input* side (retrieval quality, summaries, KG, the bytes
`semfs grep` returns). The *output* side — the tokens the agent **generates** while it
narrates, reasons aloud, and writes its deliverable — is currently uncompressed and
unaddressed by semfs.

caveman targets exactly that stream: make the agent answer tersely → fewer output tokens
billed → lower cost and faster turns, ideally with no accuracy loss.

## Why this is a *semfs* feature (and what it is NOT)

semfs is a **tool the agent calls** — it has no hook on the agent's generation stream, so
it cannot post-process the agent's output. What it *can* do is the one thing it already
does: **instruct the agent**. On `semfs mount`, `agent_hint.rs` writes a path-scoped
instruction block into `~/.claude/CLAUDE.md` / `~/.codex/AGENTS.md` / `~/.gemini/GEMINI.md`,
and the KG refresh materializes a whole-file `AGENTS.md` inside the mount
(`render_workspace_root`). A caveman directive is **one more conditional section** appended
to those two render functions.

Explicitly **out of scope** (rejected interpretations):

- **Byte compression (gzip/zstd/brotli + base64).** Counterproductive for tokens:
  compressed bytes base64-encode into gibberish that tokenizes *worse* than the original
  and the model can't read it. The output must stay human/LLM-readable, like caveman.
- **Compressing the text `semfs grep` *returns*** (the tool-output → agent-*input* stream).
  Different lever, different chokepoint (`grep.rs` render loop). Not this ticket.

## Proposal

Add a gated caveman directive to the agent hint, mirroring the existing `graph_fs` / `kg`
toggle convention (env-var gated, read at hint-render time).

### Flags

| Env var | Values | Default | Controls |
|---|---|---|---|
| `SEMFS_CAVEMAN` | `off`/`0`/`false`/`no` · `lite` · `full` (= `1`/`on`/`true`/`yes`) · `ultra` | `off` | On/off **and** intensity |
| `SEMFS_CAVEMAN_SCOPE` | `safe` · `all` | `safe` | Whether terseness spares the graded deliverable |

Invalid / unrecognized values fall back to the safe default (`off` for level, `safe` for
scope). Parsing trims whitespace and lowercases, exactly like `graph_fs_enabled()`.

**Why env vars, not a `--caveman` CLI flag:** every capability toggle in semfs (KG via
`SEMFS_KG`, graph-fs via `SEMFS_GRAPH_FS`) is env-gated and read at render time, and the
WB driver sets them per-arm. Matching that keeps it one mechanism that drops straight into
the matrix. A `--caveman` mount flag is possible later sugar; not needed for v1.

### What the directive says

`caveman_directive(level, scope)` returns instruction text (empty string when `Off`):

- **Core (all levels):** "RESPONSE STYLE — terse. Drop filler, courtesy phrases, hedging,
  restated questions. Fragments OK. Substance over eloquence."
- **Always-preserve clause (all levels, both scopes):** "Keep EXACT and complete: numbers,
  identifiers, code, file paths, citations, command output, units."
- **Scope = `safe` (default):** "Terseness applies to your narration / reasoning / chat
  ONLY. The final deliverable must stay COMPLETE — never drop required fields or data to
  be brief."
- **Scope = `all`:** terseness applies to deliverable prose too — but the always-preserve
  clause still holds (never drop required data).
- **Level tunes intensity:** `lite` (trim obvious filler) → `full` (telegraphic) → `ultra`
  (maximally telegraphic).

### Critical design rule: compress the *talking*, never the *data*

The whole risk of this feature is that terseness bleeds into the graded deliverable and
drops required content, turning a token win into an accuracy loss (WB grades the deliverable
on completeness; rubric ceiling ≈ 10/15). The always-preserve clause + `scope=safe` default
exist precisely to prevent that. `scope=all` is the deliberate, opt-in "compress everything"
mode for measuring the trade-off — it is not the default.

## Where it lives (single module: `crates/semfs-core/src/agent_hint.rs`)

1. `caveman_level() -> CavemanLevel { Off, Lite, Full, Ultra }` — parse `SEMFS_CAVEMAN`
   (same parsing style as `graph_fs_enabled()`).
2. `caveman_scope() -> CavemanScope { Safe, All }` — parse `SEMFS_CAVEMAN_SCOPE`, default
   `Safe`.
3. `caveman_directive(level, scope) -> String` — the instruction text (empty when `Off`).
4. Append `caveman_directive(...)` into **both** `render_block()` (home-level tagged block)
   and `render_workspace_root()` (in-mount `AGENTS.md`), after the existing search/KG
   guidance, before the closing footer/delimiter.

## Data flow (unchanged plumbing, one new conditional)

```
semfs mount / KG-refresh
  └─ render_block() / render_workspace_root()
       └─ reads caveman_level() + caveman_scope() from env   ← NEW conditional
       └─ appends caveman_directive() text                   ← NEW
  └─ block written into agent instruction files
       └─ agent reads it at session start
            └─ agent responds tersely  → fewer OUTPUT tokens billed
```

Because the directive sits *inside* the existing tagged block, the current `strip_block`
uninstall logic already removes it on `semfs unmount` — no new cleanup code.

## Tests (mirror the existing env-locked `agent_hint` tests)

- `caveman_level()` parsing: `off` / unset / `lite` / `full` / `1` / `ultra` / garbage → defaults.
- `caveman_scope()` parsing: default `safe`, `all`, garbage → `safe`.
- `render_block()` contains the directive markers when `SEMFS_CAVEMAN=full`; **byte-identical
  to today when `off`/unset** (no-regression guard).
- `render_workspace_root()` — same on/off assertions.
- `scope=safe` block contains the "deliverable must stay COMPLETE" clause; `scope=all` does not.
- Round-trip `install` → `uninstall` strips the directive cleanly (assert no leftover).

## Benchmark integration

- Add arm(s) to the WB driver, e.g. `caveman_safe_full` (`SEMFS_CAVEMAN=full`,
  `SEMFS_CAVEMAN_SCOPE=safe`) and optionally `caveman_all_ultra` for the upper bound.
- Set the env vars **before `semfs mount`** so they propagate to the daemon and bake into
  the rendered hint.
- Measure: **agent output tokens (primary)**, total tokens, rubric score (accuracy
  guardrail). Compare against the matched non-caveman arm.

## Acceptance criteria

- `SEMFS_CAVEMAN` off/unset → rendered hint **byte-identical** to today.
- `SEMFS_CAVEMAN=full` → directive present in both render paths; install/uninstall round-trips clean.
- Levels + scope parse per spec; invalid → safe defaults.
- `cargo test` (incl. new `agent_hint` tests) and `cargo clippy` green.
- A WB arm shows measurable agent-output-token reduction vs. its matched non-caveman arm,
  with negligible rubric loss (recorded in `RESULTS.md` once run).

## Non-goals / YAGNI

- No byte compression (gzip/zstd) — see "out of scope" above.
- No `--caveman` CLI flag in v1 (env var suffices; add later if ergonomics demand).
- No compression of the bytes `semfs grep` returns — different lever, not this ticket.
- No `wenyan` (classical-Chinese) level — English-only for now.

## Risks & caveats

- **Compliance is best-effort.** Like all `CLAUDE.md`/`AGENTS.md` hints, there is no
  guarantee the agent obeys (semfs already concedes this in `agent_hint.rs`). The directive
  is a steer, not a contract — its effect must be *measured*, not assumed.
- **Accuracy regression** if terseness leaks into the deliverable — mitigated by the
  always-preserve clause + `scope=safe` default + the rubric guardrail in the benchmark.

## Follow-ups (post-v1, only if data warrants)

- `--caveman` mount flag as ergonomic sugar over the env var.
- `wenyan` / other intensity levels.
- Tune directive wording per level using the benchmark output-token deltas.
