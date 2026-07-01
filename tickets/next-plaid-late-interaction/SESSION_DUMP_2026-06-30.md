# Session dump — 2026-06-30 → 07-01

Record of everything done this session: tests run, code changes, and the full mode taxonomy.
Threads: (A) finish next_plaid/late-interaction, (B) PPR data analysis + anomalies, (C) headroom
compression experiment + harness integration, (D) SWE-Atlas QnA ticket.

---

## 1. Modes / arms we have (taxonomy)

| mode (arm) | what it is | seed needed? | status this session |
|---|---|---|---|
| **plain** | agent crawls raw workspace (`grep`/`find`/`cat`), no semfs | **no** | baseline; per-persona numbers in the PPR dashboard |
| **ppr_off** | semfs retrieval, 1-hop (PageRank OFF) | yes (semfs .db) | done all 4 WB personas |
| **ppr_on** | semfs retrieval + Personalized PageRank graph prior | yes | done all 4 WB personas |
| **ppr_map** | ppr_on + workspace-map injected in context | yes + map | houqin only |
| **late on** (next_plaid) | colgrep late-interaction (ColBERT/PLAID); LFM2 docs + LateOn-Code code, RRF-merged; agent uses `semfs grep`, colgrep underneath | **yes** (colgrep index per corpus) | houqin (partial) + kaifa (n=1) |
| **Headroom + plain** | plain + headroom compression proxy on the model side (compresses codex's prompt before the LLM) | no | **integration built; kaifa smoke only** |
| **cloud** | Supermemory cloud backend | server-side | older (5-arm matrix) |
| **nokg / kg / hiddenkg\*** | semfs local search / KG variants | yes | older (5-arm matrix, chanpin) |
| **model routing** | UNDEFINED — no artifacts, no ticket in project | ? | not a real mode yet |

Persona name map: **logistics=houqin, ops=yunying, dev=kaifa, pm=chanpin.**

---

## 2. Tests / experiments run this session

### A. next_plaid (late interaction)
- **Baked E2B templates**: `np-houqin-all` (LFM2 single index) and `np-kaifa-C` (dual: kaifa_code=LateOn-Code + kaifa_doc=LFM2, RRF). 3 packaging bugs fixed (`__file__.parents`, colgrep hidden-dir locate, missing `file_context_path`), then kaifa-C **disk fight**: runtime writable overflow → build-tar overflow → fixed by baking lfm2 as a **read-only image dir** (dir-copy, no tar) + dropping model_int8.
- **Routing validated** (`phase2_routing_check.py`, no GPU): agent's `grep` → semfs shim → `semfs-np` → **colgrep** (needed `/etc/profile.d` login-PATH fix so `bash -lc` resolves the shim).
- **Merge validated** (`phase2_merge_check.py`, no GPU): kaifa-C fuses BOTH lanes (code + doc paths interleaved by RRF).
- **houqin next_plaid_A** (GLM): **stopped at 15/60 cells** (n=1, first-half cases) — `14.9%` over its cases.
- **kaifa next_plaid_C** (GLM, n=1, 11 cells): **31.4%** acc, 7.86M tokens (tok/correct 178K).

### B. PPR analysis (no new PPR runs — analysis of existing data + the converged dashboard)
- **The "9.7%" was an artifact** (confirmed independently by Codex): houqin next_plaid's 9.7% was a mid-run snapshot over the *harder first-15 cases*, mean-weighted, vs ppr_on's full-30. Same-cases: NP 14.9% ≈ ppr_on 14.5%.
- **Dashboard (`judge_ckpt`, converged judge, n=2–3) is authoritative** — the raw `judged.jsonl` is the OLD judge (undercounts ppr by ~16pp on kaifa). Per-persona: kaifa ppr_off 35.8/ppr_on 33.5/plain 17.5; houqin plain 27.9/ppr_on 21.8/ppr_map 25.7/ppr_off 17.5.
- **Token distribution is a universal fat tail** — median ~250K everywhere, outliers 2–4.4M in EVERY persona×arm (not kaifa-specific). "kaifa token bump" = small-n + mean-vs-median.
- **Two anomalies** (PPR vs plain): tokens↑ only kaifa, acc↓ only houqin. Validated user's "plain is maxed there" hypothesis (correlation exact); extended it (workspace structure as common cause; mean-artifact). → `../wblite-ppr-ab/anomalies-explained-v2.html` (linked in `dashboard_fresh.html`).
- **RCA**: `rcas/2026-06-30-kaifa-token-bump-doc-layer-loop.md` — case 226 (4M tok, 0%) = agent looped in the doc layer (87 unique reads / 91 re-reads / 85 docs / 0 code), no surfaced prefix-cache → quadratic.

### C. Headroom (context compression)
- **Local `headroom wrap codex` (gpt-5.4)** on kaifa case-226: worked, but **2% savings** — codex/gpt-5.4 caches 86%, so little to compress.
- **Backends**: `--backend openrouter` / `litellm-<provider>` (incl `litellm-hosted_vllm` = our GLM) confirmed in source. But `wrap codex` on a non-OpenAI model fails codex's **ChatGPT-account model allowlist** → need API-key mode (which our cell_driver's GLM path already does: pops `CODEX_USE_CHATGPT`, custom provider, chat-adapter).
- **Harness integration** (the real deliverable): `headroom_glm_proxy.py` (Modal web service, headroom in front of GLM) + `WB_HEADROOM=1` flag in cell_driver (points codex's chat-adapter `baseUrl` at headroom). **No codex.py change.** Chain validated sans-GPU (auth/routing/model all pass; only the down-GLM upstream errored).
- **kaifa `plain` smoke via headroom→GLM**: **20/20 judged** ✅ but **0% compression** — default is prefix-frozen/conservative. Set **`--target-ratio 0.5`** → case-266 dropped **1.4M → 328K tokens (~77%)** — but the run was terminated before the judge, so **accuracy-at-ratio unverified**.

### D. SWE-Atlas QnA
- Ticket **SEM-50** + `tickets/swe-atlas-qa/` created. Recommended `plain` (baseline) + `late on` (bet). Judge = **Claude Opus 4.5** (configurable). Gated on reading paper 2605.08366.

---

## 3. Code changes this session

| file | change |
|---|---|
| `benchmarks/e2b/cell_driver.py` | `next_plaid` arm branch (semfs-grep→colgrep affordance); `WB_HEADROOM=1` → codex baseUrl = headroom proxy |
| `benchmarks/e2b/run_matrix.py` | next_plaid arm wiring (SUPPORTED_ARMS, `NP_CELLS`, `setup_nextplaid`, `.semfs` marker, profile.d PATH, dual-lane); `WB_HEADROOM`/`HEADROOM_GLM_BASE` env passthrough |
| `benchmarks/modal/bake_nextplaid.py` | bake np-<cell> templates; multi-cell (kaifa-C dual index); `lfm2_in_image` dir-copy (disk fix) |
| `benchmarks/modal/headroom_glm_proxy.py` | **NEW** — headroom Modal proxy (`--backend litellm-hosted_vllm --mode token --target-ratio`) in front of GLM |
| `tickets/next-plaid-late-interaction/` | `rrf_merge.py`, `semfs_np_wrapper.sh`, `np_grep_shim.sh`, `phase2_routing_check.py`, `phase2_merge_check.py`, `np_glm_*.sh`, `hr_glm_kaifa.sh`, `hr_arm_glm.sh`, `anomalies-explained-v2.html` (in wblite-ppr-ab), this dump |
| `rcas/2026-06-30-kaifa-token-bump-doc-layer-loop.md` | **NEW** RCA |
| `tickets/wblite-ppr-ab/dashboard_fresh.html` | anomalies-v2 link |
| `tickets/swe-atlas-qa/` | **NEW** ticket (SEM-50) |

---

## 4. Open threads (what's unfinished)
- **late on**: houqin needs the missing 15 cases + rep2 (25%→100%) + converged re-judge; kaifa n=1 needs re-judge. Then a trustworthy late-on-vs-ppr number.
- **Headroom+plain**: verify **accuracy at ratio 0.5** (one clean judged cell), then kaifa 11-case on/off A/B, then the other personas. Headroom likely wants its own ticket.
- **Completeness matrix**: 13/56 cells done. xafs (seeds only), SWE-Atlas, Terminal bench not started. "model routing" undefined.
- **Judge caveat**: never mix judge versions (old jsonl vs converged) or mean-vs-median — the recurring artifact trap.

## 5. Data locations
- next_plaid runs: `tickets/next-plaid-late-interaction/artifacts/{houqin_glm,kaifa_glm,hr_smoke}/`
- PPR/dashboard: `tickets/wblite-ppr-ab/` (`dashboard_fresh.html` = authoritative converged judge; raw `judged.jsonl` = OLD judge)
- headroom proxy endpoint: `https://ada-diffusion-llm--headroom-glm-proxy-serve.modal.run/v1`
