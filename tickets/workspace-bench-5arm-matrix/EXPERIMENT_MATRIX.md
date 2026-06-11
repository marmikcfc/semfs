# EXPERIMENT MATRIX — every configuration, every layer, every knob (2026-06-10 → 06-11)

> One row per distinct configuration. Constants for ALL runs unless a row says otherwise:
> **agent** codex/GPT-5.4 (OpenRouter "ripbench", single-turn, `cached_input=0`) ·
> **judge** Seed-2.0-Lite (`agent_eval`/`agent_as_a_judge`) · **box** EC2 m7i.xlarge ·
> **corpus** chanpin persona, 1,452 files · **common semfs env** `GREP_INLINE=on`,
> `RETURN_MODE=snippet`, `REWRITE=1`, `NO_PUSH=1`, `NO_SYNC=1`, fresh seed copy →
> `chanpin-matrix` tag per run. Layers: **INFRA** (disk/gates) → **INDEX** (seed/embedder)
> → **SEARCH** (rewrite/fusion/rerank) → **DELIVERY** (what grep renders) → **AFFORDANCE**
> (hint + KG surfaces) → **AGENT** → **JUDGE**.

## Phase 0 — original 5-arm matrix (2026-06-10, 5 cases × 5 arms, n=1/cell)

⚠️ INFRA BROKEN for all local arms: disk ~95% (ENOSPC page-tears), 716 `.semfs-error.txt`
contamination in seed, fastembed cache pollution (mount hangs), stale daemon (vec0
corruption), silent cloud-fallback on timeout. No health gates. 13/15 local runs infra-failed.

| arm | index/seed | hint (AGENTS.md) | SEARCH_ONLY | KG / GRAPH_FS | delivery | result (Σ 5 cases) |
|---|---|---|---|---|---|---|
| plain | — (real files) | — | — | — | — | **46% @ 89K mean** |
| nokg | chanpin-gemma-q4 (contaminated) | v1: "read kg/ FIRST" + "trust excerpt, don't re-open" (baked; /kg/ files baked too) | **on** | off/off (but /kg/ baked) | uncapped grep (cap knobs INERT, ~300KB blobs) · RESULT_LIMIT=8 | 3% @ 247K + 45-min timeout |
| gfs_off | same | same | **on** | **on**/off | same | 10% @ 178K |
| gfs_on | same | same + /by-topic/ section | **on** | **on**/**on** | same | 14% @ 471K (worst) |
| cloud | Supermemory `workspace-bench-chanpin` (~74% coverage, summaries indexed) | v1 | **on** | off/off | server-side render | 27% @ 93K · won case 95 (12/12) |

## Phase 1 — E1–E5 on cleaned infra (2026-06-11 day; case 289 unless noted)

All rows: seed **chanpin-clean** (716 sidecars removed via FUSE rm), health-gated driver
(`run_case_e.sh`: disk guard ≥6G, `PRAGMA quick_check`, dummy SM key, `.fastembed_cache`
strip, daemon-inner kill). Hint still v1 (baked, read-only).

| exp | what varied (vs Phase 0 nokg) | knobs | result | verdict |
|---|---|---|---|---|
| E1 infra-clean | infra fixed, nothing else | SO=**off**, uncapped grep, RLIM=8 | 111K/5 · 145K/6 (vs timeout/0 before) | **H1 confirmed**: catastrophe was infra |
| E2 SO A/B | SEARCH_ONLY flipped | SO=**on**, same seed | >30-min flail (vs 114s at off) | **H2 confirmed**: =off is the safety floor |
| E3 summaries (case 44) | index representation | seed **chanpin-sum** (summary-embedded); first build INVALID (tables discarded); dual-store rebuild for 44's 3 files | sum-dualstore 2/16 @ 125K vs raw 4/16 @ 130K | token-neutral; case names its files → summaries structurally untestable here |
| E4 delivery caps | render size | `DOC_RETURN_CAP`/`RESULT_LIMIT` via env → **INERT on CLI** (capped 372K ≈ uncapped 316K chars) | 289 "compact" still 139K | knob plumbing bug found |
| E4′ grep-cap patch | **code**: `SEMFS_GREP_RESULT_CAP` (new) | cap=6KB rlim=8 → 122K/**6** · cap=3KB rlim=3 → **76.8K**/5 and 97.8K/5 | first sub-plain token run | **H4 confirmed** via code, variance remains |
| E5 KG ablation | KG cost attribution | trace breakdown: 35.7K kg-read + 22.4K os.walk per run; fresh KG-off seed build BLOCKED (½-indexed in 3h) | — | **H5 confirmed**: KG surfaces net-negative |
| leanhint | **hint v2** via fs_data surgery (`chanpin-leanhint.db`): "ONE grep → read top hit → no crawl, no kg/" | + cap, SO=off | 78.4K/5 | token frontier reached; acc still −1 |

## Phase 2 — E6 clip calibration (2026-06-11 evening; no semfs involved)

codex 0.133.0, marker-line files, model self-reports surviving window.

| probe | size / lines | outcome |
|---|---|---|
| B | 5.4 KB / 300 | whole (⇒ no 256-line cap) |
| C | 9.8 KB / 200 | whole (⇒ ≤10 KB safe) |
| D | 15.5 KB / 330 | boundary truncation notice |
| A | 49 KB / 1000 | **~1.2K tokens survive** (head+tail, token-denominated notice) |

## Phase 3 — E7/E8 evening batch (case 289, `matrix_artifacts/e8seq/`)

Common: binary `1f4cf280` (dual-store + grep cap; hint v1 compiled but **seed hint wins**),
SO=**off**, `GREP_RESULT_CAP=6144`, `RESULT_LIMIT=5`, KG=off, GFS=off. All degraded scores
**re-judged offline**.

| runs | seed (= hint version) | unique knob delta | tokens / calls / score |
|---|---|---|---|
| w1·w2·w3 (scout) | chanpin-leanhint (hint v2) | — | 21.5K/2/**4** · 168.8K/12/4 · 107.2K/9/4 |
| wp1·wp2·wp3 (W′) | chanpin-leanhint2 (hint v3 = v2 + PROVENANCE CHECK) | — | **93.7K/9/6** · 21.7K/2/4 (check skipped) · 80.5K/9/5 |
| p1·p2·p3 (plain) | — (real workdir, prep on) | no semfs | 322.4K/15/**⊘ unjudgeable** · 117.6K/9/5 · 71.5K/7/**7** |

Aggregate: scout-class mean 82K vs plain mean 171K (−52%); W′ clean = plain clean mean (6.0).

## Phase 4 — E9 wave 1 (case 289, `matrix_artifacts/e9w1/`, binary `2cd0a507`)

Common: seed chanpin-leanhint2 (hint v3), SO=off, `GREP_RESULT_CAP=6144`, `RESULT_LIMIT=5`,
**`SEMFS_GREP_TOTAL_CAP=10240`** (new global budget), KG/GFS off.

| runs | `SEMFS_GREP_RENDER_MODE` | render shape | tokens / calls / score |
|---|---|---|---|
| e9b1-3 | **two-tier** | top hit full (≤6KB) + paths/snippets + confidence verdict | 22.5K/2/4 · 93.9K/10/5 · 22.5K/2/⊘* |
| e9c1-2 | **paths** | path + 160-char snippet only (~1KB total) | **85.5K/8/6 · 86.7K/7/6** (±1K — most consistent arm ever) |
| (control) | inline | = Phase 3 w/wp distribution | 21–169K, bimodal |

*e9b3 deliverable byte-identical to e9b1; judge parse-fails reproducibly (2nd unjudgeable
case after p1). Verdict miscalibration found: RRF score compression ⇒ MIXED always ⇒ the
HIGH stop-signal path is still untested → wave 2 = spread-normalized margin + COMPLETE-FILE
gate.

## Knob → layer reference (every knob that appeared, with its tested verdict)

| knob | layer | values tested | verdict |
|---|---|---|---|
| (infra gates: disk guard, quick_check, SM dummy key, fastembed strip, daemon kill) | INFRA | absent → present | mandatory; absence fakes "retrieval losses" (H1) |
| seed: gemma-q4 / clean / sum / sum-dualstore / leanhint / leanhint2 | INDEX | all | clean=baseline; dual-store=summaries safe; leanhint2=current best |
| `SEMFS_STORAGE_BACKEND` | INDEX | local sqlite / cloud | cloud = coverage-dependent wins only |
| `SEMFS_REWRITE` | SEARCH | 1 (always) | enables cross-lingual rank fix |
| `SEMFS_SEARCH_ONLY` | DELIVERY | on / off | **off, always** — on causes catastrophic flail (H2) |
| `SEMFS_GREP_INLINE` | DELIVERY | on (always) | inline excerpts beat file-pointers for codex (PwC) — except see paths arm |
| `SEMFS_RESULT_LIMIT` | DELIVERY | 8 / 5 / 3 | 5 = current; was INERT pre-patch |
| `SEMFS_DOC_RETURN_CAP` / `RETURN_MODE` | DELIVERY | various | **INERT on CLI path** (E4) — superseded by GREP_RESULT_CAP |
| `SEMFS_GREP_RESULT_CAP` | DELIVERY | ∞ / 6KB / 3KB | 6KB: acc holds; 3KB: −1 acc; default 6KB shipped |
| `SEMFS_GREP_TOTAL_CAP` | DELIVERY | ∞ / 10KB | global budget (E6: per-hit caps can sum past the ~15KB clip) — shipped default 10KB |
| `SEMFS_GREP_RENDER_MODE` | DELIVERY | inline / two-tier / paths | paths = most consistent (86K/6/15 ×2); two-tier verdict needs recalibration |
| hint v1 / v2 / v3 | AFFORDANCE | all | v1 (KG-first + trust-excerpt) = −58K/run harm; v3 shipped as default |
| `SEMFS_KG` / `SEMFS_GRAPH_FS` | AFFORDANCE | on/off matrix | both **off** for codex-class agents (H5); /by-topic/ = worst tokens in matrix |
| judge re-run (`agent_as_a_judge --overwrite`) | JUDGE | — | required after 429s; p1 & e9b3 reproducibly unjudgeable → harden inputs |
