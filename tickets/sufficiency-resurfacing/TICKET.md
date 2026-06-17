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
