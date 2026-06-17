# Ticket: Sufficiency re-surfacing — stop the agent's over-exploration

**Folder:** `tickets/sufficiency-resurfacing/`
**Origin:** evo glm-5.1 session (2026-06-17). Over-exploration is the #1 unsolved token sink.
**Routing:** mirror to Linear (team `SemFS`, key `SEM`) per CLAUDE.md §0.

## Problem (artifact-grounded)

The agent re-runs `semfs grep` 30–190 times per task (171: 78–188 calls; 95: 134; plain hit
**3.5M tokens / 122–188 calls** on hard cases). Root cause: **the agent is never told it has the
COMPLETE relevant set, so it keeps probing.**

semfs's existing stop-signal (`grep.rs::confidence_line`) is **dominance-based**: it fires HIGH only
when ONE hit dominates `(s1−s2)/(s1−sN) ≥ T`. That works for single-answer tasks (53) but the score
curve is **flat on broad/aggregation tasks** (171/95) → margin never clears threshold → verdict is
never HIGH → the agent never hears "you're done."

Worse, **dedup (SEM-19) backfired**: it STRIPPED re-sent content and returned a pointer, so the agent
felt it had *lost* the content → re-searched MORE (exp_0008: dedup doubled the call count, ~78 vs ~36;
trace shows the "not resending" pointer fired 64×). The agent over-explores when it's **not confident
it has everything** — "if the model is happy, it won't over-explore."

## Solution — a compact, session-aware SUFFICIENCY signal (the anti-dedup)

Repurpose the existing `SessionCache` (built for dedup) from "strip what you've seen" → "**affirm you
have the complete set**." No KG dump (too token-heavy for a vast workspace); re-uses content already in
the agent's context.

1. **Track the retrieved set across turns** (SessionCache already does this).
2. **On a re-search that mostly overlaps the already-retrieved set**, emit a STRONG affirmation instead
   of stripping:
   > `✓ COMPLETE SET — your earlier searches already surfaced all top matches for this (files A,B,C
   > above). Further searches return the same. STOP and build the deliverable from what you have.`
3. **Coverage-based verdict for broad tasks:** when the score curve flattens (diminishing returns) AND
   the agent has retrieved the head of it, that IS the "done" signal — say so, replacing the current
   "mixed → keep refining" line that *invites* more searching.

Compact (no extra tokens — re-affirms content already paid for), deterministic, value-safe.

## Deliverables

- Rust: extend `confidence_line` with a coverage/sufficiency branch (flat-curve + head-retrieved → DONE).
- Rust: a session-overlap check (SessionCache) → re-search-overlap affirmation line.
- Knob: `SEMFS_SUFFICIENCY` (off|on) to A/B vs the current dominance-only verdict.
- Replace/disable the backfiring dedup-strip; keep the SessionCache as the coverage tracker.

## Success criteria

- [ ] On 171/95/386: median `semfs grep` calls drop sharply (target < plain's, ideally < the prompt-only ~36)
      with **no accuracy regression** (it must affirm sufficiency, not hide content).
- [ ] No token blowups (no > plain-token cells driven by re-search).
- [ ] A/B vs prompt-only + vs dedup on E2B (53/171 + a discovery case), glm-5.1, n≥3.

## Relation to other tickets

- Anti-dedup: this **supersedes** the SEM-19 dedup-strip (which made over-exploration worse).
- Orthogonal to compression (input/output token trimming) and the KG (structure map) tickets.

---

## Test plan — PREPPED 2026-06-17, **NOT launched** (awaiting explicit go)

Per user (2026-06-17): "KG on should by default use this [the dense Leiden+kNN KG]; we should have a
sufficiency-resurfacing knob added. Don't test yet, but ideally test **sufficiency-resurfacing** and
**sufficiency-resurfacing + KG**." The dense KG is now the **default** KG build (`graph_file.rs`
unconditionally uses Leiden+kNN — see `tickets/kg-quality/`), so "KG on" already means the dense KG.

**Knob added:** `benchmarks/e2b/knobs/sufficiency.json` = the winning `prompt_only` base (turnbrake +
caps + rewrite-off) **+ `SEMFS_SUFFICIENCY=on`**. Deliberately NO `SEMFS_DEDUP_WINDOW` — sufficiency
is the anti-dedup alternative (daemon sets `dedup_strip=false` when sufficiency is on). This tests
whether sufficiency *adds on top of the established winner*, not in isolation.

**Arm matrix** (isolates the two focal levers as marginal effects over the prompt-only winner):

| arm | KG | sufficiency | invocation | needs dense-KG seed? |
|---|---|---|---|---|
| plain | — | — | `--arms plain` | no |
| prompt-only (current winner) | off | off | `--arms nokg --knobs prompt_only.json` | no |
| **sufficiency** ⭐ | off | on | `--arms nokg --knobs sufficiency.json` | no |
| KG+prompt | on | off | `--arms kg --knobs prompt_only.json` | **yes** |
| **sufficiency + KG** ⭐ | on | on | `--arms kg --knobs sufficiency.json` | **yes** |

⭐ = the two arms the user named. The other three are the baselines needed to read them (you can't
attribute a win to sufficiency or KG without prompt-only and KG+prompt as controls).

**Prerequisite for the KG arms (the `--arms kg` rows):** the shipping seed
`benchmarks/e2b/assets/chanpin-gemma-q4.db` still holds the **old Louvain** projection (173 comms,
38% singletons). Before any KG-arm run it must be re-materialized with the new code
(`cargo run --release -p semfs-core --example materialize_kg -- <seed>` → 32 comms, 3% singletons),
done on a **copy** then swapped in (destructive-edit-on-copies rule), or rebuilt fresh on Modal x86_64.
Otherwise the KG arm would test the *fragmented* KG — a confound.

**Run config (to finalize at launch):** glm-5.1 (`WB_OR_MODEL=z-ai/glm-5.1`, `WB_FORCE_OPENROUTER=1`),
53/171 + a discovery case, n≥3, E2B real-FUSE only ([[all-benchmark-tests-on-e2b]]). The Modal binary
must be rebuilt with the sufficiency + Leiden+kNN code before the run.
