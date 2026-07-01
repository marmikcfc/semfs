# Token + accuracy reduction — hypothesis ledger

**Goal:** improve accuracy AND drive codex case-289 tokens < 100K, via researched
hypotheses (no harness coaching). First sub-goal: **reduce tool-call count**.
**Method:** scientific method — falsifiable hypotheses, test order = ease × likelihood,
validate/invalidate on EC2 (kg_on, clean prompt, Seed-2.0-Lite rubric judge).
**Frame:** Breunig, *How Contexts Fail / How to Fix Your Context* (2025-06-22 / -26).

---

## Observation (the measured current state — kg4, clean, rubric-judged)
- kg4: **203.7K tok / 11 calls / 7-15 rubrics.** Trace = **1 `semfs grep` → 8 crawl
  commands** (`os.walk`/`ls -R`/`find`×4) → found decoy → **fabricated** deliverable.
- Token law (proxy has `cached_input_tokens=0`): **tokens ≈ turns × accumulated context.**
  Turn count is the driver; turn count is driven by crawling.
- Two SEPARABLE crawl drivers:
  - **(B-pre) os.walk-FIRST** — reflexive enumerate before any grep (p0all2 92K pattern).
  - **(A-post) distrust-crawl** — greps, then opens files / `find`-sweeps because it does
    not trust the excerpt (the kg4 1-grep-then-8-crawl pattern — THE CURRENT bottleneck).

## Breunig mapping (why this is a context-engineering problem, not a retrieval one)
| our symptom | Breunig failure mode | the fix he names |
|---|---|---|
| 62KB unranked `os.walk` dump; every file looks equal | **Context Confusion** ("junk drawer influences your response") | RAG / **Context Pruning** at the source |
| tokens balloon as turns accumulate (>100K) | **Context Distraction** ("favors repeating actions over novel plans") | **Summarization / Pruning / Offloading** |
| greps, distrusts excerpt, re-opens files | **Context Confusion** (can't tell if excerpt is authoritative) | atomic, **complete** delivery |
| AGENTS.md "don't crawl" vs agent's "not found yet" | **Context Clash** (read-but-ignored) | don't argue with context — change what ops RETURN |

## ⭐ Mechanism finding (root cause of the local↔cloud first-class gap)
The trust marker **already exists in code** and is **silently disabled on local.**
- `grep.rs:756-762`: `if let Some(memory) = result.memory { print "path:dump"; continue }`
  — short-circuits **before** the line-range + `# ^ COMPLETE FILE — …use it directly,
  do not open it` marker (`grep.rs:848-853`) and before `present_excerpt`.
- `sqlite_vec.rs:1438-1463`: local **always** fills `memory` — snippet mode = capped chunk
  (1445), default = stitched whole doc (1458). Cloud leaves `memory=None`.
- ⇒ **cloud** (`memory=None`) flows through the chunk path → emits line-ranges + COMPLETE
  marker → agent trusts → stops (3-4 calls). **local** (`memory=Some`) emits bare
  `path:dump`, **no completeness signal** → agent distrusts → opens files → crawls (11+).

This is the answer to "does supermemory send a stop marker the local lacks": **the marker
is the cloud chunk-path's `# ^ COMPLETE FILE` line; local bypasses it via the `memory`
short-circuit.** The fix is a PRODUCT change (grep output), identical prompt — not gaming.

---

## Hypothesis ledger
| id | hypothesis (falsifiable) | predicts if TRUE | predicts if FALSE | test | ease | targets |
|---|---|---|---|---|:--:|---|
| **H1** | Local bypasses the COMPLETE-FILE trust marker (`grep.rs:756` `memory` short-circuit). Routing local through the marker (line-ranges + `# ^ COMPLETE FILE`) makes codex trust the excerpt and **stop**, cutting (A-post) distrust-crawl. | tool-calls ↓ toward cloud (3-5); tokens ↓; rubrics ≥ kg4 (still sees 403) | calls ~unchanged → distrust is not the (A-post) driver; bottleneck is elsewhere | flip snippet path to `memory=None`+chunk (or add marker to memory path); 1 EC2 run | **HIGH** | A-post |
| **H2** | (B-pre) os.walk-FIRST is a residual driver. Graph-as-FS (`readdir`/`lookup`/`getattr` → bounded beam-BFS over god-nodes) makes the reflexive crawl return ranked/bounded structure (Context Pruning at source), so a walk-first run still orients cheaply. | walk-first runs no longer blow up; bounded `ls -R`; calls ↓ even when it doesn't grep-first | if kg4 doesn't walk-first anyway, no measurable delta → H2 is solving a non-present problem on this config | build graph-as-FS (schema+ops); A/B `SEMFS_GRAPH_FS` on/off | LOW (~600-800 LOC) | B-pre |
| **H3** | Agent under-trusts/under-uses semfs because the FS-contract doesn't explain *how each call behaves* (grep = ranked semantic excerpts w/ COMPLETE markers; readdir semantics; "don't re-open a COMPLETE hit"). Sharper `agent_hint` raises trust/compliance. | calls ↓, grep-first ↑ | no change (consistent with "read-but-ignored" — passive text inert) | edit `agent_hint.rs`; 1 EC2 run | HIGH | A+B |
| H4 | Accuracy (honesty) gap: agent sees 403, fabricates. NOT FS-fixable (behavioral). Out of scope for token goal; tracked separately. | — | — | — | — | accuracy |

## Recommended experiment order (ease × likelihood, test the CURRENT bottleneck first)
1. **H1** — nearly free, targets the *measured* kg4 failure (post-grep distrust-crawl), and
   directly answers the supermemory-marker question. Establishes a clean baseline.
2. **H3** — also cheap; pairs with H1 (a marker only helps if the agent knows to trust it).
3. **H2 (graph-as-FS)** — build for the **residual** (B-pre) crawl that survives H1+H3.
   De-risked: symlinks/readlink **confirmed implemented** (`fs.rs:1735-1834`, fuse bridge
   wired), synthetic inode headroom free (`ino≥1<<48`), integration point = the
   SEARCH_ONLY branch (`fs.rs:1282`). One blocker: persist the Louvain projection into
   `graph_community` + `graph_god_node` (today ephemeral in `build_digest`).

## UPDATE 2026-06-08 — graph-as-FS measured + local↔cloud grep capture

**Graph-as-FS (H2) result (n=3):** built+verified; gfs1=87K/5calls/10-rubrics/HONEST ✅
but gfs2=490K, gfs3=686K. Necessary-not-sufficient. **Tail = format trap** (codex
parsing `.xls` candidates) — the post-grep distrust H1 targets, NOT the pre-grep crawl.

**Captured raw grep responses (local sqlite vs cloud supermemory, same query):**
| signal | LOCAL (sqlite) | CLOUD (supermemory) |
|---|---|---|
| line ranges `:1-10:` | ❌ bare `path:content` | ✅ |
| `# ^ COMPLETE FILE — …do not open it` | ❌ never (`grep.rs:756` `memory` short-circuit) | ✅ |
| `[semfs: SOURCE INACCESSIBLE …403…]` | ✅ surfaced | ❌ absent |

**Refined H1 (better than "make local = cloud"):** route local through the chunk
presenter so it emits line-ranges + the COMPLETE-FILE marker, **while keeping local's
403 surfacing** → local+H1 has BOTH the trust marker (kills format trap) AND the honesty
signal (cloud has only the former). Also tighten 14–27KB grep payloads. Plus a
data-hygiene bug: stale fabricated `/model_output/...` is indexed and ranks #1 → invites
fabrication; exclude `model_output/` from the index.

## UPDATE 2026-06-08 (pm) — Composio/Exa research on tool-call reduction + seed corruption

**Literature (Exa, citation-backed) confirms the diagnosis + names the problem:**
- Tokens = turns × accumulated context, quadratic WITHOUT prompt caching (LogDx
  agent-trajectory-token-anatomy). ⇒ the benchmark's `cached_input_tokens=0` inflates
  cost; production caching would shrink the turn penalty (measurement-validity caveat).
- "Over-search" is a studied problem. Mitigations: no-progress / marginal-insight
  detection (stop when results overlap previous), OverSearchGuard BEA (stop when output
  stabilizes), SAAS/Stop-RAG/CoDE-Stop (confidence/value early-stop), dynamic turn
  control (−12–24% turns), full-horizon planning (2–3× fewer tokens).
- **Constraint filter:** we cannot modify codex → model-side methods (RL, control tokens,
  context compression of agent history, planning policies) are OUT; hard turn caps are
  harness-gaming (= the removed `_SEMFS_PROTOCOL`). Only ENVIRONMENT-side factual signals
  are legit (the H1 class).

**H4 — Evidence-stabilization signal on `semfs grep` (NEW, testable, env-side).** When a
grep's top-k overlaps the agent's previous grep, prepend a FACTUAL note "⟳ same top
results as your previous search — no new evidence surfaced." Over-search BEA applied at
the retrieval layer; factual + general (not answer-injection). Targets the measured
behavior (codex reformulates + re-greps 4–6×, gfsh1/gfs2). Falsifiable: grep/turn count
on a CLEAN seed. ⚠ borderline (nudges "stop"); on the H1 side of the line, pending judgment.

**Seed corruption found (chanpin-e5-nosum, 725 files):** ~50 (~7%) junk —
`model_output/` (3, incl. the fabricated list that ranks #1 = the fabrication bait),
`.semfs-error*` (5, one recursively nested = a bug), `/.venv/` (37), generated kg/AGENTS/
CLAUDE; + 29 failed embeds (~4% missing). **Clean reseed > gemma-q4** for the immediate
step: removes the proven fabrication cause + dilution, low-risk, isolates one variable;
embedder is NOT the token lever (q4 = detour). Clean reseed fixes ACCURACY/dilution, NOT
the turn-count token lever (which H4 + the no-caching caveat address).

**Scientific honesty note (Feynman's "don't fool yourself"):** the current clean config
(kg4) crawls *after* grepping (A-post), not *before* (B-pre). H2/graph-as-FS targets B-pre.
So H1 must run first to confirm whether B-pre even dominates once trust is fixed — else we'd
build 600 LOC for a sub-failure this config may not exhibit. Graph-as-FS stays the plan; the
question is whether it's experiment #1 or #3.
