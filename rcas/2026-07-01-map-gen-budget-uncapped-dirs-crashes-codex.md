# RCA — `semfs_map.py` budget capped clusters but not the DIR skeleton → oversized map crashes codex

**Date:** 2026-07-01
**Component:** `benchmarks/e2b/semfs_map.py` (workspace-map generator for the `ppr_map` arm)
**Status:** Fixed
**Severity:** Medium — silently broke `ppr_map` on large workspaces (dp_012), inflated cost 13× on dp_011.

## Symptom
`ppr_map` cells for **dp_012** (4,998 files) failed with a distinctive signature: `tokens=0 calls=0
wall≈30s`, empty answer. The agent (codex) never made a single API call. `ppr_on` on the *same* seed
worked fine (312K tokens), so the mount/seed were healthy — the failure was specific to `ppr_map`.
Separately, **dp_011** `ppr_map` "worked" but cost **1.1M tokens** (vs ~80K for comparable cells).

## Root cause
`ppr_map` injects a cached workspace map into the prompt via `WB_WORKSPACE_MAP`. The map has two parts:
a **DIRECTORIES** skeleton (one line per depth-2 dir) and a **TOPIC CLUSTERS** overlay (the KG layer).
`build_map()` enforced its `budget` (default 4,800 chars ≈ tokens) by trimming **only the cluster
overlay tail** — the directory list was emitted in full, unbounded:

```python
def render(ov):
    return "\n".join(head + ["## DIRECTORIES"] + fs + [...] + ov)   # fs ALWAYS full
ov = overlay
while ov and len(render(ov)) // 4 > budget:  # trims ov only, never fs
    ov = ov[:-1]
```

For a workspace with thousands of depth-2 dirs the DIR list alone blew the budget:
- dp_011: 964 dirs → bloated map (~big) → codex survived but paid 1.1M tokens/turn.
- dp_012: **3,306 dirs → ~100K-token map (408 KB)** → codex choked on the oversized prompt **before its
  first API call** → `0 calls / 30s / error`.

The budget "cap" was a no-op for the dominant term.

## Fix
Cap the directory list to top-N by file count (fs is already sorted desc), then trim overlay/dir tails
to the budget — and cap dirs *first* so the KG clusters (ppr_map's distinctive value) keep budget room:

```python
def build_map(..., max_dirs=40):
    ...
    ov, fs_ = overlay, fs[:max_dirs]
    while ov and len(render(fs_, ov)) // 4 > budget: ov = ov[:-1]
    while len(fs_) > 1 and len(render(fs_, ov)) // 4 > budget: fs_ = fs_[:-1]
```

Result on dp_012: map **2,252 tokens, 40 dirs + all 40 clusters** (was ~100K). Re-runs:
- dp_012 ppr_map: **81K tokens, 3 calls, ✓** (was crash).
- dp_011 ppr_map: **71K tokens, ✓** (was 1.1M ✓) — 15× cheaper, same correct answer.

## Why it mattered for the experiment
The bloated map made the `ppr_map` column non-comparable across personas (dp_001–010 had few dirs so
their maps were incidentally within budget; dp_011/012 did not). Without the fix, dp_012 ppr_map would
have recorded a spurious failure and dp_011 an inflated cost — both corrupting the arm comparison.

## Lessons
- A "budget" that only bounds one of several additive sections is not a budget. Bound the whole render.
- Prioritise the *distinctive* content (KG clusters) over the *bulk* content (dir list) when trimming.
- A 0-token / 0-call / ~fixed-wall agent result = the agent died on input handling, **before** any LLM
  call — look upstream (prompt assembly, injected context size), not at the model.

## Related
`rcas/2026-06-27-map-gen-exit2-per-sandbox.md` (a different map-gen failure mode). SEM-47,
`tickets/wb-xafs-ppr-ab/`.
