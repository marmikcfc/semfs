# Kaifa code-KG smoke — outcome (KG-on vs KG-off)

Date: 2026-06-16 · Platform: **E2B** (real FUSE mount) · Agent: claude (via OpenRouter) · Seed: kaifa code-KG (gemma-q4)
Raw data: `summary.json`, `kaifa_claude_3_{kg,nokg}.json` (traces), `kaifa_claude_3_{kg,nokg}_out.tgz` (deliverables)
Task: `project_dependency_deduplication_list.md` (dependency dedup over the kaifa code workspace)

## What ran

n=1 per arm, one task, two arms on the same E2B mount + same code-KG seed; the only delta is the
KG hint/render (`kg` = KG steered on, `nokg` = off).

| arm | status | tokens | calls | semfs grep | wall_s | auth | deliverable |
|-----|--------|--------|-------|:---:|--------|------|-------------|
| KG-on  | ok | 431,443 | 12 | ✓ | 182 | openrouter | `project_dependency_deduplication_list.md` (119 lines) |
| KG-off | ok | 372,446 | 10 | ✓ | 167 | openrouter | `project_dependency_deduplication_list.md` (156 lines) |

## Outcome — what we can and cannot say

**Verified (from the traces):**
- Both arms completed (`status: ok`) and both wrote the deliverable. The pipeline + code-KG seed
  work end-to-end on E2B.
- KG-on cost **more**, not less: +16% tokens (431K vs 372K) and +2 tool calls (12 vs 10).
- KG-on's deliverable is **shorter** (119 vs 156 lines).

**NOT measured → no winner:**
- **Accuracy is UNJUDGED.** There is no rubric/judge score for either deliverable, so we cannot
  tell whether KG-on's shorter output is *tighter* (good) or *missing dependencies* (bad). Per
  `analyze-benchmark-results`: a token number without a paired accuracy number is half a result —
  and the dangerous half. **Verdict = PENDING-ACCURACY.**
- **n=1.** A single per-arm run is a coin flip on this project (call count swings ±, tokens ±30%).
  No trend is claimable.

**Honest headline:** on this one smoke, steering the code-KG **on** bought *higher* token cost with
*no demonstrated accuracy benefit* — consistent with the project-wide finding that semfs's KG layer
has not shown a token-or-accuracy win in a broad-workspace arena (see memory `e8-honest-headline-result`,
`wb-5arm-matrix-result`). This is a **smoke**, not a verdict.

## To turn this into a result
1. Run the rubric judge on both deliverables (kaifa task rubrics) → get the accuracy pair.
2. n≥2–3 per arm to clear variance.
3. Only then compare lexicographically (accuracy first, then tokens).

Seed caveat (from memory `ast-kg-code-lane`): the kaifa code-KG seed's `calls` edges are
over-connected (name-based resolution), so KG-steered exploration may chase spurious edges — a
plausible mechanism for the higher call/token count, **untested here**.
