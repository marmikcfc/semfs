# WB-Lite PPR / map-in-context — Findings

**Status:** CLOSED (negative result + a methodology reckoning) · **Last run:** 2026-06-27 · **Persona:** houqin · **Backend:** GLM-5.1-NVFP4 (Modal vLLM)

---

## TL;DR

1. **The in-context workspace map is a dead end.** ppr_map ≈ ppr_on on accuracy (**17.5% vs 17.1%**, tied), and the map **costs +37% tokens** despite −30% turns. No axis pays off.
2. **The real result is a measurement bug, not a retrieval win.** The same arm (ppr_on) scored **9.7% (old harness) → 17.1% (filename-fixed)** — ~1.75×, from matching deliverables correctly. The original *"PPR net-negative"* headline was **substantially a judge/metadata artifact.**
3. **Once measured cleanly, PPR ≈ plain (~17%).** Neither the KG prior nor the map beats vanilla codex on houqin. WB-Lite (full workspace handed to the agent) is the **wrong arena** for retrieval-ranking mechanisms.

---

## What we tested (arc)

| Step | What | Outcome |
|---|---|---|
| PPR A/B (prior) | ppr_off (1-hop) vs ppr_on (PPR diffusion), all personas | plain 17 > ppr_off 12.9 > ppr_on 11.9 → "PPR net-negative" **(under buggy harness)** |
| Diagnosis | trace dive on the loss | loss = filename artifact (~15% zeroed) + ranking dilution + under-exploration, **NOT** bad recall |
| Filename fix | `WB_LITE → wb_lite_all/lite_all` (complete metadata) | removes the nesting-artifact zeroing |
| Map hypothesis | cached ~4.8k-tok workspace map injected as prompt prefix (`ppr_map` = ppr_on retrieval + map) | test: does a map let the agent navigate to ranking-buried files? |
| Smoke (4 cases) | 358/357/251/267 × n=2 | promising 358 (+20) **but n=2 noise** |
| Full houqin n=2 | 30 cases × {ppr_on, ppr_map} × 2 reps | **the definitive run** |

---

## Final results — houqin, 30 cases, n=2 (2026-06-27, GLM, PAR=12)

### Accuracy (ppr_map vs ppr_on — clean A/B, both today, identical config)
```
aggregate:  ppr_on 17.1%   ppr_map 17.5%        → TIED
per-case:   map WINS 7 · LOSES 4 · TIES 19      → mean Δ +0.5pp (noise)
big swings cancel:  358 +20, 276 +15   vs   274 −25, 251 −16
358 (the smoke's "win"): 40% = 40% with full data → the smoke signal was a single-rep variance artifact
```

### Tokens — the map is a net COST
```
            turns    input    total
ppr_on       64      513K     521K
ppr_map      45 ↓    706K ↑   716K  (+37%)
```
Why fewer turns ≠ fewer tokens: total ≈ Σ(context resent each turn). The map is **re-sent every turn** (4.8k × 45 ≈ 216K) and the map induces **bigger per-turn reads** (cat whole files vs grep snippets) → **~2× per-turn context** (15.7K vs 8.0K), which beats the 30% turn reduction. Compounded by `cache_read=0` (vLLM prefix-cache benefit not surfaced through litellm/codex → re-sent map billed full price).

### Latency / GPU load (PAR=12)
```
latency: median 1.42s · p95 2.69s · max 3.16s
KV-cache: median 14% · max 37%   | peak concurrent 8
```
→ **PAR=12 was trivial for GLM. PAR=16 is safe too** (ceiling is the E2B ~20 sandbox cap, not GLM). The earlier StreamResets were peak-collision flukes; GLM concurrency was never the bottleneck.

---

## The big reframe — the filename fix, not retrieval

```
                houqin     prev plain   prev ppr_off   prev ppr_on  ‖  TODAY ppr_on   TODAY ppr_map
                              17.7%         10.7%          9.7%      ‖     17.1%           17.5%
```
Same arm, same model: **ppr_on 9.7% → 17.1%** purely from the filename fix (old run pointed at chanpin-only metadata → houqin deliverables nested/renamed → judge couldn't match → ~half the credit lost to a bug). Plain barely nests (~0%), so the bug hit the ppr arms specifically — which is why the old run *looked* like "PPR hurts." Fixed → PPR rises to ≈ plain.

See `comparison.html` (per-case + aggregate, prev vs today).

---

## Conclusions & recommendations

- **Park the map.** Neutral accuracy, +37% tokens; its only win (fewer turns) doesn't convert to token savings.
- **Metadata/judge correctness is the dominant lever** — it moved accuracy ~2× with zero model/retrieval change. The biggest swing in the whole investigation came from a filename string.
- **Restate prior PPR conclusions.** They were measured pre-fix; "PPR net-negative" should be read as "≈ measurement artifact; clean PPR ≈ plain."
- **WB-Lite is the wrong arena** for ranking mechanisms — the agent already has the whole workspace, so reorder-only changes don't gate accuracy. **Pivot to a discovery arena** (agent must find files it doesn't know exist) to test semfs value. Echoes prior findings (E8/E11: semfs value is discovery, not broad-workspace retrieval).
- **Optional:** fix the `cache_read=0` accounting (surface vLLM cache through litellm) if we keep tracking tokens.

---

## Caveats

- houqin only; n=2 (per-case swings like 274 −25 partly variance).
- **plain not re-run today** — the historical plain (old harness) is the comparator; "PPR ≈ plain" carries a small confound (the *within-arm* ppr_on 9.7→17.1 is clean, though).
- Token metric undercounts the map's GPU cache benefit (`cache_read=0`).

---

## Infra deliverables (reusable)

- **Modal off-local orchestrator** (`benchmarks/modal/smoke_orchestrator.py`) — runs E2B cells in Modal's cloud, immune to local flaky internet. Lean image (binary on `semfs-bin` volume), engine switch (openrouter for no-GPU validation | glm), out volume, post-run completeness check. Commits: `d84f77d`, `c8dfe22`.
- **Continuous queue** (`mount_sig` in run_matrix) — re-mount only on mount-config change; same-mount arms dequeue with no barrier.
- **Pre-ship map fix** (`c8dfe22`) — ship known-good map → skip the fragile per-sandbox gen (was exiting 2 on ~79% of cells; RCA `rcas/2026-06-27-*`).
- **Latency/GPU-load probe** — vLLM `/metrics` (kv_cache_usage_perc, num_requests_running/waiting) + litellm round-trip.

## Artifacts

- Runs: `tickets/wblite-ppr-ab/artifacts/map_smoke_glm/` (today, ppr_on/ppr_map) · `artifacts/e2b_runs/` (prev PPR A/B) · `workspace-bench-5arm-matrix/artifacts/e2b_runs/` (prev plain).
- Comparison page: `tickets/wblite-ppr-ab/comparison.html` (+ generator `build_comparison.py`).
- Explainers: `decouple-recall-explained.html`, `query-flow-ppr.html`, `workspace-map-explained.html`.
