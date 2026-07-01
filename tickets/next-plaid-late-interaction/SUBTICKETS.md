# next_plaid — experiment subtickets (the 3 test cells)

_Children of [SEM-43](https://linear.app/semfs/issue/SEM-43). Each cell = (persona × config × model-set) run as a `next_plaid` arm on **E2B**, agent LLM on **OpenRouter**, vs `ppr_on` / `plain`. Configs A/B/C defined in `README.md` §"mixed corpus" + `plan.html` §4._

## The matrix

| Cell | Linear | Persona | Config | Models (lanes) | Why this cell |
|---|---|---|---|---|---|
| 1 | **[SEM-44](https://linear.app/semfs/issue/SEM-44)** | **xAFS** | **C** — split, 2 models | code: `lightonai/LateOn-Code` · docs: `lightonai/LateOn` | best-case **English** dual-model — both ColBERTs at full strength, no language handicap |
| 2 | **[SEM-45](https://linear.app/semfs/issue/SEM-45)** | **kaifa** (has code) | **C** — split, 2 models | docs: `LiquidAI/LFM2-ColBERT-350M` · code: `lightonai/LateOn-Code` | the **unsolved target** (PPR ran high); gives code its own LateOn-Code index + LFM2 docs |
| 3 | **[SEM-46](https://linear.app/semfs/issue/SEM-46)** | **houqin** (docs) | **A** — unified, 1 model | `LiquidAI/LFM2-ColBERT-350M` | LFM2 on its **home turf** (Chinese docs) — cleanest read of late-interaction vs ppr on documents |

**Config recap:** A = 1 model, 1 index, no merge. B = 1 model (LFM2), 2 indices, RRF merge. C = **2 models**, 2 indices, RRF merge. ("2 models" = C only.)

**Encoder decision (2026-06-29): ONNX for both.** LFM2 → exported via `pylate-onnx-export`; `LateOn`/`LateOn-Code` → shipped ONNX. The build encoders and the in-cell query shim use **onnxruntime** for every lane — **no PyTorch in the E2B cell.**

---

## Readiness — shared Phase 0 before the 3 parallel builds

The 3 seed builds are independent (different personas/indices/templates) and **do parallelize** — but they share tooling and one high-risk prerequisite, so do a short **Phase 0 (~0.5–1d) once**, then fan out.

**Phase 0 (shared, do once):**
1. **LFM2 → ONNX export spike — HIGHEST RISK.** `pylate-onnx-export` LFM2-ColBERT-350M; verify `onnxruntime` loads it and emits `[n,128]` with projection + per-token L2 in-graph, `[Q]`/`[D]` markers, `query_length=32` / `document_length=512`. **Gates kaifa-C + houqin-A.** If it fails → fall back to a baked PyLate encoder sidecar for the LFM2 lanes.
2. **Confirm `LateOn` + `LateOn-Code` ONNX** — shipped (LateOn-Code-edge is) or export. **Gates xAFS-C + kaifa-C code lane.**
3. **Preflights (validate the model choices):** xAFS **English**? · kaifa code **language** (Chinese → LateOn-Code is the risk under test) · houqin **code presence** (≈none → houqin-A is correct).
4. **Write the shared `build_nextplaid_index.py`** (one script, params: persona/config/models) + stand up next-plaid (install the client + run the server, or build the binary).

**Then fan out (parallel):** xAFS-C · kaifa-C · houqin-A builds → bake each into its E2B template.

**Go/no-go checklist:**
- [ ] LFM2 ONNX export verified (onnxruntime loads, correct shape)
- [ ] `LateOn` + `LateOn-Code` ONNX available
- [ ] preflights pass (xAFS English · kaifa code language · houqin code count)
- [ ] `build_nextplaid_index.py` written + next-plaid installed
- [ ] bake paths fixed (`/data/nextplaid/<persona>_<config>/` → template `/opt/nextplaid/<persona>/`)

**Honest status:** the *plan* is complete (specs, 8-step pipeline, dual-model mechanism verified, file anchors). Not yet done: the **3 Phase-0 checks aren't run** and `rrf_merge.py` isn't written. So we don't press "go" on 3 cold-parallel builds — knock out Phase 0 (above all the LFM2 export), then the 3 builds fan out cleanly.

**Do we need build scripts first? — mostly no.** The index build is **`colgrep init`** (their CLI) once the ONNX models + corpus dirs exist; the only guaranteed-custom code is **`rrf_merge.py`** (Config C, **test-time** → does not gate building). The fork: if `colgrep init` can't load our custom exported LFM2 ONNX, the fallback is the next-plaid **API + a tiny feeder script** (`/update` precomputed vectors) — so Phase 0 verifies "colgrep ingests LFM2 ONNX" to avoid writing any build script.

## Execution roadmap — now → results on all 3

Wall-clock **≈ 3.5–5 days** (cells share tooling + parallelize), with tails from the LFM2 export + E2B infra.

| Phase | What | Parallel? | Gate |
|---|---|---|---|
| **0 · Shared prereqs** (~0.5–1d) | LFM2 ONNX export spike · confirm `LateOn`/`LateOn-Code` ONNX · 3 preflights (xAFS English · kaifa code lang · houqin code count) · install colgrep (x86_64-linux) · **verify `colgrep init` loads custom LFM2 ONNX** · materialize+route helper | once | all ONNX load via onnxruntime; preflights pass; colgrep ingests LFM2 ONNX |
| **1 · Build 3 seeds** (~1d) | 1a xAFS-C: `colgrep init` ×2 (LateOn-Code + LateOn) · 1b kaifa-C: ×2 (LateOn-Code + LFM2) · 1c houqin-A: ×1 (LFM2) → bake each into its E2B template. (`rrf_merge.py` written here, in parallel) | **3 builds parallel** | per cell: offline sanity (known query → gold file) + index baked |
| **2 · Wire arms + E2B run** (~1–1.5d) | shared once: `run_matrix.py` arms (`next_plaid_*`) + `cell_driver` tool-swap (colgrep / `rrf_merge.py`) + OpenRouter env. Then run each cell's cases, n=2, vs `ppr_on`/`plain` on E2B | wiring once → **runs parallel** | results.jsonl + judged.jsonl per cell |
| **3 · Judge + analyze** (~0.5d) | per cell: accuracy+tokens table; cross-cell read — kaifa-C vs xAFS-C (language penalty) · kaifa-C vs houqin-A (code-lane value) | per cell | 3 tables + cross-cell conclusions = **final results** |

**Critical path:** Phase 0 (LFM2 export) → Phase 1 (slowest build) → Phase 2 (E2B run + debug) → Phase 3.

---

## Shared pipeline — how each cell reaches a final result

Same 8 steps for every cell; the **bold per-cell deltas** are below. Each step has a verify gate (CLAUDE.md §4). All on E2B; index build is off-box artifact prep, baked into the E2B template (honors the all-benchmarks-on-E2B rule).

| # | Step | Verify gate |
|---|---|---|
| 1 | **Preflight** — corpus composition (code vs doc file counts) + **language** check vs the cell's models + model availability (ONNX shipped vs PyLate-only) | per-type counts + language + encoder path chosen |
| 2 | **Materialize corpus** — mount the persona seed with `SEMFS_EXTRACT_SIBLING=on`: code files **raw**, binary docs → `.extracted.md`. **[Config C: route into a code-dir + a doc-dir]** | mount shows raw code + `.extracted.md`; (C) routed counts == indexed files |
| 3 | **Encoder setup (ONNX for both)** — `LateOn` / `LateOn-Code` shipped ONNX; **LFM2 → ONNX-exported via `pylate-onnx-export`**. All lanes encode via `onnxruntime` (build + in-cell query); no PyTorch in the cell | each model embeds → `[n_tok, 128]` via onnxruntime |
| 4 | **Build index/indices** — **[A: 1 colgrep init over the unified dir]** · **[C: 2 colgrep inits — code-dir w/ code model, doc-dir w/ doc model]** | index(es) built; state counts match file counts |
| 5 | **[Config C only] Query merge** — dual-encode the query, search both indices, **RRF-merge** → top-k. (A: single-index query, no merge.) | known code query → code index, known doc query → doc index; merged top-k correct |
| 6 | **Sanity** — a known query returns the expected file in top-k (no agent) | top-1/top-k hit |
| 7 | **Bake** — index(es) + colgrep + (C) the RRF-merge shim into the persona's E2B template | template boots; index(es) present; FUSE corpus intact |
| 8 | **Wire arm + run** — arm `next_plaid_<persona>_<config>` in `run_matrix.py`; agent uses colgrep/merge-shim; LLM = **OpenRouter**; run the persona's cases, **n=2**, vs `ppr_on` / `plain` on E2B | `results.jsonl` + `judged.jsonl` land for all arms |
| 9 | **Judge + analyze** — Seed-2.0-Lite rubrics + honest no-deliverable=0 (`rejudge_loop.py`) → **final table: accuracy AND tokens** vs `ppr_on`/`plain`; **mine the actual responses** (analyze-benchmark-results) | one table; responses read |

**Config A vs C differs in exactly three steps:** C routes the corpus (step 2), builds two indices (step 4), and adds the RRF merge (step 5). A does none of these. Cells: **xAFS-C** and **kaifa-C** are Config C; **houqin-A** is Config A.

---

## Per-cell specifics

### Cell 1 — xAFS · Config C · LateOn + LateOn-Code ([SEM-44](https://linear.app/semfs/issue/SEM-44))
- **Goal:** ideal-conditions dual-model — both LightOn ColBERTs are English-centric, so this only makes sense if **xAFS is English** (step-1 gate).
- **Models:** both ship ONNX → colgrep encodes internally (no PyLate needed); confirm `LateOn` (text) ONNX in preflight.
- **Run:** xAFS 13 cases, n=2.
- **Risks:** only 13 cases → low n (report variance); RRF weight tuning; xAFS must be English.

### Cell 2 — kaifa · Config C · LFM2 (docs) + LateOn-Code (code) ([SEM-45](https://linear.app/semfs/issue/SEM-45))
- **Goal:** the unsolved, high-token target — give kaifa's **code its own `LateOn-Code` index** + LFM2 for docs, RRF-merged. **Watch tokens** (kaifa's pain is exploration cost; the win is fewer tool-calls from better code retrieval). _(Updated 2026-06-29: moved from Config A → C — the LFM2+LateOn-Code dual-model now lands on the persona that actually has code.)_
- **Models:** mixed paths — docs → LFM2 (PyLate/export); code → `LateOn-Code` (shipped ONNX). Two indices, RRF merge.
- **Run:** kaifa cases, n=2.
- **Risks:** **`LateOn-Code` is English** → kaifa's Chinese comments/identifiers may not match well (the key thing this cell measures); RRF merge tuning; LFM2 no ONNX. **Fallback if the code lane underperforms:** Config B (code lane = LFM2) or Config A (unified LFM2).

### Cell 3 — houqin · Config A · LFM2 ([SEM-46](https://linear.app/semfs/issue/SEM-46))
- **Goal:** LFM2 on its **home turf** (Chinese documents) — the cleanest read of "does late interaction beat `ppr_on`/`plain` on doc retrieval?" _(Updated 2026-06-29: dropped the Config-C LateOn-Code code lane — houqin is doc-heavy and its code, if any, is Chinese, so the English code model was the wrong pick. Just LFM2, unified.)_
- **Models:** LFM2 only, no ONNX → PyLate-direct or export. One unified index, no merge.
- **Run:** houqin `358/357/251/267` (+ full 30 if promising), n=2; `plain` refs already exist (`358=47 357=73 251=45 267=88`).
- **Risks:** LFM2 no ONNX → PyLate-direct or export; doc-heavy so the centroid-dilution caveat is minimal; any code present is embedded as text by LFM2 (fine).

---

## Cross-cell reading
- **kaifa-C vs xAFS-C** — both Config C with a `LateOn-Code` code lane; they differ in the **doc lane + persona language**: kaifa = LFM2 docs (Chinese), xAFS = LateOn docs (English). Together they isolate **how much `LateOn-Code` (English) is hurt by a Chinese vs English codebase** — same code model, two language regimes.
- **kaifa-C vs houqin-A** — kaifa adds a code lane (LateOn-Code) on top of LFM2 docs; houqin is LFM2 docs only. Isolates **whether the code lane earns its keep** on the persona that has code (kaifa, the unsolved high-token target) vs the doc-only baseline (houqin, LFM2's home turf).
- **xAFS-C** is the **best-case English dual-model** — both ColBERTs at full strength, no language handicap.
- **If kaifa-C's `LateOn-Code` lane underperforms** (English model on Chinese code), the documented fallback is **Config B** (code lane = LFM2) or **Config A** (unified LFM2).
- Every result is read **within-persona** vs `ppr_on`/`plain`, accuracy **and** tokens together (never a token number alone).

---

## Code changes needed

Integration = **Option A (parallel path)**: next-plaid lives outside semfs; **zero `semfs-core` Rust changes**. The existing persona seed (`<persona>-gemma-q4.db`) is **unchanged** — it still provides the FUSE mount the agent reads from. We *add* a next-plaid index artifact alongside, and swap the agent's search tool. (File anchors from the architecture map in `REFERENCE.md §OURS`.)

> **Path alignment (load-bearing):** build the next-plaid index over the **same file tree the agent will see in the cell** (the mounted seed tree, with `SEMFS_EXTRACT_SIBLING=on` so `.extracted.md` siblings exist), so colgrep's returned paths resolve to real files in the agent's workdir.

### A. Seed creation (build + bake the next-plaid index)

| Change | File | What |
|---|---|---|
| **NEW** | `benchmarks/modal/build_nextplaid_index.py` | Materialize corpus from the mounted seed → **[Config C: route code-dir / doc-dir]** → embed (LFM2 via **PyLate-direct**; `LateOn`/`LateOn-Code` via **shipped ONNX**) → build the index/indices (next-plaid API `update` with precomputed vectors, or `colgrep init --model …`). Output `/data/nextplaid/<persona>_<config>/` (1 dir for A, 2 for C, + manifest). |
| **NEW** | `tickets/next-plaid-late-interaction/rrf_merge.py` (Config C only) | Query shim: dual-encode the query, search **both** indices, **RRF-merge** → top-k. Reused in-cell as the search tool. |
| **NEW (artifact)** | — | `colgrep` / next-plaid binary built for **x86_64-linux** (E2B) + the ONNX model files. |
| **EDIT** | `benchmarks/modal/bake_e2b_persona.py` (~`:109`) | Copy the next-plaid index dir(s) + the binary + ONNX models + `rrf_merge.py` into the E2B template at `/opt/nextplaid/<persona>/`. Seed bake itself unchanged. |

### B. Final test (arms + cell wiring + OpenRouter)

| Change | File:line | What |
|---|---|---|
| **EDIT** | `run_matrix.py:58` `SUPPORTED_ARMS` | add `next_plaid_xafs_C`, `next_plaid_kaifa_C`, `next_plaid_houqin_A` (or one `next_plaid` arm reading persona/config from env) |
| **EDIT** | `run_matrix.py:97` `MOUNT_ARMS` | add them — still need the FUSE mount for the agent's **file reads** (search is colgrep, reads are the mount) |
| **EDIT** | `run_matrix.py:116-186` `arm_mount_env` | new branch: keep the seed mount; set `WB_SEARCH_TOOL=colgrep` + `WB_NEXTPLAID_DIR=/opt/nextplaid/<persona>` (+ `WB_NEXTPLAID_MERGE=1` for C); set **OpenRouter** env (`WB_FORCE_OPENROUTER=1` + base/model), unset the GLM/Modal vars |
| **EDIT** | `run_matrix.py:645` `mount_sig` | include the new flags so workers re-mount per arm |
| **EDIT** | `benchmarks/e2b/cell_driver.py` | new branch: install `colgrep` (Config A) or `rrf_merge.py` (Config C) as the agent's search tool — PATH shim `/opt/semfs-shims/grep` → colgrep/merge, or the codex hook — instead of `semfs grep` |

**Per-cell:** **houqin-A** can use **`colgrep` directly** (single index, no merge — the simplest wiring). **xAFS-C / kaifa-C** need the **`rrf_merge.py`** shim over two indices (colgrep is one-model-per-index → it won't merge two models for us).

### Integration A vs B — does anything change now that we run both A and C configs?

**No — A (parallel path) stays for the final test.** B's expensive part (vendor the `next-plaid` crate into `semfs-core`, reconcile `ort 2.0-rc.11`, write a `SemanticIndex` backend, integrate the seed-build pipeline, x86 cross-build) is **not justified before a result**, and Config C's merge is cheap in A (a Python shim over the next-plaid API). Honest nuance: **Config C makes A heavier than "just use their binary"** — for the C cells we use the next-plaid API + a custom `rrf_merge.py`, not pure `colgrep`. Only **houqin-A** is "pure colgrep." After a positive result, **B becomes the more-justified productionization** (the merge shim ports in, B removes the delivery-format confound by serving results through `semfs grep`, and B can reuse semfs's existing tree-sitter AST code lane) — so running both configs tilts the *eventual* path toward B, not the *test* path.

### Dual-model (Config C) inside Option A — verified feasible (from next-plaid source)

Confirmed from the clone — three facts make a two-model, two-index, RRF-merged setup clean **with zero semfs-core changes**:
1. **next-plaid is a model-*optional* pure multi-vector store** — `AppState::with_model(Option<Colbert>)` + a `#[cfg(not(feature="model"))]` path (`next-plaid-api/src/state.rs:284`). We own encoding; the per-server-single-model limit is moot.
2. **Many named indices per server** — `indices: RwLock<HashMap<String, Arc<IndexSlot>>>` (`state.rs:226`). A `code` index + a `doc` index coexist.
3. **`/search` and `/update` accept precomputed vectors** — `to_ndarray` decodes `embeddings_b64 + shape` → `Array2<f32>` (`handlers/search.rs:29`). So both build-docs and query are encoded **externally**, per-lane, by their own model.

**Mechanism:**
- **Build (off-box, baked):** code → LateOn-Code (ONNX) → `/update` → code index · docs → LFM2 (PyLate) → `/update` → doc index.
- **Query (in-cell `rrf_merge.py`):** encode the query with **both** models → `/search` each index → **RRF-merge** the two ranked lists → top-k.

**Why it's robust:** RRF is **rank-based** (`1/(k+rank)`) → scale-invariant, so the two models' MaxSim magnitudes need not be comparable (this is the thing that breaks naive multi-model merges). Dims need not match across indices either — each is scored against its own centroids; RRF merges lists.

**The one wrinkle — DECIDED: ONNX for both.** In-cell query encoding of two models runs via `onnxruntime` (LFM2 exported via `pylate-onnx-export`; LateOn(-Code) shipped ONNX) → no PyTorch in the cell. Doc encoding is all bake-time; only the short query is encoded online. **Gating risk: the LFM2 ONNX export must be proven first (Phase 0 §1)**; fallback if it fails = a baked PyLate encoder sidecar for the LFM2 lanes.
