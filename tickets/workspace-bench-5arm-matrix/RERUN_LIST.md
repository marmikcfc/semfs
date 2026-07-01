# Rerun manifest — NVFP4 7-arm matrix (fill to n=3)

Model: **GLM-5.1-NVFP4** · endpoint: litellm (`glm-5.1-nvfp4`) · binary: `semfs-fixed` (arms 1-7) ·
seed: `chanpin-4arm.db` · generated after the r1-only matrix (r2 timed out under the model-name 404, r3 never ran).

## Reps OK now (per arm × case)

| arm | c15 | c44 | c53 | c95 | c175 | need n=2 | need n=3 |
|---|---|---|---|---|---|---|---|
| 1 plain (knob: —) | 1 | 1 | 1 | 1 | 1 | 5 | 10 |
| 2 compress (compress_only_clean) | 1 | 1 | 1 | **0** | 1 | 6 | 11 |
| 3 comp+dedup (compress_dedup_clean) | 1 | 1 | 1 | **0** | 1 | 6 | 11 |
| 4 best (best_exp0002) | 1 | **0** | 1 | **0** | 1 | 7 | 12 |
| 5 hkg-edges (best_exp0002) | 1 | **0** | 1 | 1 | 1 | 6 | 11 |
| 6 hkg-rerank (best_exp0002) | 1 | **0** | 1 | **0** | 1 | 7 | 12 |
| 7 hkg-l7 (best_exp0002) | 1 | 1 | 1 | 1 | 1 | 5 | 10 |
| **TOTAL** | | | | | | **42** | **77** |

Notes:
- case **95** = the over-explorer (plain r1 = 2.09M tokens); timed out for 4/7 arms → highest-risk to rerun.
- case **44** = floor case; timed out for best/edges/hkg.
- case-44 **hkg-l7** r1 completed but produced an **empty deliverable** (0% accuracy) — counts as a completed rep but is a degenerate result; consider one extra rep.

## Config per arm (same as original 7-arm run)

| arm | `--arms` | `--knobs` |
|---|---|---|
| 1 plain | `plain` | (none) |
| 2 compress | `nokg` | `knobs/compress_only_clean.json` |
| 3 comp+dedup | `nokg` | `knobs/compress_dedup_clean.json` |
| 4 best | `best` | `knobs/best_exp0002.json` |
| 5 hkg-edges | `hiddenkg_edges` | `knobs/best_exp0002.json` |
| 6 hkg-rerank | `hiddenkg` | `knobs/best_exp0002.json` |
| 7 hkg-l7 | `hiddenkg_l7` | `knobs/best_exp0002.json` |

Shared env (litellm endpoint + non-stale binary + clean seed):
```
WB_E2B_TEMPLATE=semfs-baked-v2 WB_MODAL_GLM=1
WB_MODAL_BASE=https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1
WB_MODAL_MODEL=glm-5.1-nvfp4
WB_FIXED_BIN=benchmarks/e2b/assets/semfs-fixed         # arms 1-7 (has dedup+hidden-KG, NOT retrieval)
WB_E2B_SEED_NOKG=/opt/chanpin-4arm.db                  # route nokg off the contaminated default
WB_AGENT_TIMEOUT=2700 WB_CELL_TIMEOUT=3000             # 45-min leash
```

## Execution plan

**n=2 (frugal, 42 cells):** run **one** more full rep of all 7 arms (rep `rb1`), then a targeted refill of
the cells that still have <2 (the r1-timeouts: 95 compress/dedup/best/hkg, 44 best/edges/hkg).

**n=3 (design, 77 cells):** run **two** more full reps (`rb1`, `rb2`) of all 7 arms, then refill any cell
still <3 (the r1-timeouts get their final rep).

Launch identical to `run_7arm_n3_modal.sh` but with the litellm base + a fresh rep prefix.
Reuses the same arm/knob wiring; just bump `--rep`.

## Status
- [ ] rerun rep rb1 (35 cells)
- [ ] rerun rep rb2 (35 cells)  ← only for n=3
- [ ] targeted refill of remaining <target cells
- [ ] judge all rerun cells (Seed-2.0, GPU-free)
- [ ] final 7-arm table at n≥2 (accuracy × tokens, equal coverage)
