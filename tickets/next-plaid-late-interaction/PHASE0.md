# Phase 0 — shared prerequisites (live tracker)

_Started 2026-06-29. Gate before the 3 parallel builds (Phase 1). See `SUBTICKETS.md` for the full roadmap._

## Status board

| # | Item | Status | Result |
|---|---|---|---|
| 0.2 | ONNX availability of the 3 models | ✅ **DONE** | `LateOn` ✅ ships ONNX · `LateOn-Code` ✅ ships ONNX · **LFM2 ❌ no ONNX → must export** |
| 0.1 | LFM2 → ONNX export (the gate) | ✅ **DONE** | 4 attempts: token_type_ids heuristic + missing onnxscript + dynamo-exporter KV-cache guard → **`dynamo=False` legacy export worked**. `model.onnx` (1.41GB) + `model_int8.onnx` (354MB) on vol `np-lfm2-onnx`, inputs `[input_ids, attention_mask]`, out `[.,.,128]`. Script: `phase0_export_lfm2_patched.py`. |
| 0.3 | Corpus preflights (composition + language) | ✅ **DONE** | see below — **changes the design** |
| 0.4 | colgrep "loads custom LFM2 ONNX?" | ✅ **YES (from source)** | `model.rs:28` — `ensure_model` returns a local dir as-is → `colgrep init --model /path/to/lfm2-onnx`. (Default model = `LateOn-Code-edge`; named HF models auto-download.) Remaining: grab the prebuilt x86_64-linux binary + a smoke `colgrep init` (mechanical). |

## Findings

**0.2 — ONNX availability (HF file listings):**
- `lightonai/LateOn` (xAFS doc lane): `model.onnx` (616MB) + `model_int8.onnx` + `onnx_config.json` → **ready, no export.**
- `lightonai/LateOn-Code` (xAFS + kaifa code lane): `model.onnx` (597MB) + `model_int8.onnx` + `onnx_config.json` → **ready, no export.**
- `LiquidAI/LFM2-ColBERT-350M` (kaifa doc lane + houqin): only `model.safetensors` (1.41GB) + `1_Dense/` (PyLate ColBERT) — **no ONNX → export required.**
- **Implication:** **xAFS-C needs zero export** (both lanes shipped). Only **kaifa-C doc lane + houqin-A** depend on the LFM2 export (0.1).

**Export mechanism** (from the clone, `next-plaid-onnx/python`): `pip install pylate-onnx-export` → `pylate-onnx-export LiquidAI/LFM2-ColBERT-350M -o <dir>` (deps pylate≥1.3.3 + onnx + onnxruntime; produces `model.onnx` + `model_int8.onnx`).

**Environment recon:**
- Modal authed (`~/.modal.toml`); **builds/export run on Modal** (off-box, matches the project pattern). No local torch/pylate.
- Seeds on `semfs-bench-data:/seeds/` — `kaifa-gemma-q4.db`, `houqin-gemma-q4.db`, `xafs-gemma-q4.db` (+ chanpin/yunying/research). Only chanpin is in-repo.
- colgrep / next-plaid not installed locally (will build x86_64-linux for E2B in 0.4).

**0.3 — preflight — v2 CONTENT-VERIFIED.** _(v1 bug, caught by user: classified on the **trailing** extension, so `*.py.md` code-named files were miscounted. v2 strips `.md`/`.txt` wrappers → inner ext, **then samples content**. `phase0_preflight_v2.py`.)_

| persona | files | **real code** (content-checked) | language | verdict |
|---|---|---|---|---|
| **kaifa** | 2415 | **912** ✅ real (`.py/.go/.js/.java/.tf/.sql` — `def load_medical_data()`, `package handlers`, `const express=require`) | English (cjk 0.0) | **Config C** — LateOn-Code well-founded (real English code) |
| **houqin** | 2313 | **43** (~2%, real English `#!/usr/bin/env python3` placeholder `gen_*.py`) | Chinese docs (cjk 0.148) | **Config A / LFM2** confirmed (doc-heavy) |
| **xafs** | 19,170 | **0** — the 9 `.py.md`/`.ipynb.md` "code"-named files are **markdown prose writeups** (TOC/author/date, `looks_like_code=False`); rest ≈ 18.9K English `.md`/`.txt` docs + emails/csv | English (cjk 0.0) | **Config A / LateOn** confirmed (doc-only, no real code) |

**Design implications (xAFS now content-confirmed):**
1. **xAFS → Config A with `LateOn`** — content-verified **0 real code** (the `.py.md` files are prose). No code lane; no LFM2 dependency. ~19K English docs = a large index.
2. **kaifa → Config C** — 912 **real English** code files → `LateOn-Code` is the right tool (the earlier "Chinese-code risk" was wrong; kaifa code is English).
3. **houqin → Config A / LFM2** — only genuinely-Chinese cell (0.148), doc-heavy. LFM2 export matters most here.
**Lesson:** classify by content, not extension (`.X.md` wrappers fooled v1).

## Go / no-go — PHASE 0 COMPLETE ✅
- [x] **0.1 LFM2 export** → `ok=True, dim128=True` (dynamo=False). kaifa-C + houqin-A encoders unblocked.
- [x] **0.2 LateOn / LateOn-Code ONNX** present (shipped); LFM2 exported.
- [x] **0.3 preflight** → xAFS English + **0 code** · kaifa code **English** · houqin Chinese doc-heavy.
- [x] **0.4 colgrep loads custom ONNX** (local dir path, `model.rs:28`). _(remaining: fetch binary + smoke — mechanical.)_

## Decisions before Phase 1 fan-out
1. **xAFS-C → xAFS-A** (0 code files) — RECOMMENDED, awaiting confirm (SEM-44 rewrite held).
2. _(optional)_ kaifa is English overall → its doc lane could use `LateOn` instead of LFM2 (drops kaifa's LFM2 dep). kaifa-C as specced (LFM2 docs) is also fine.

→ **Phase 1 = fan out 3 parallel builds** (xAFS · kaifa-C · houqin-A) via `colgrep init --model <dir>` → bake into E2B templates.

---

## Phase 1 — builds ✅ COMPLETE (2026-06-29)

All indices built via `colgrep init` (`phase1_build.py`), on Modal volume `np-indexes`. `init_code=0` all.

| Index | Model | Files (code/doc) |
|---|---|---|
| `houqin_all` | LFM2 (local ONNX) | 2313 (43 / 2270) |
| `kaifa_code` | `lightonai/LateOn-Code` (shipped ONNX) | 930 / 0 |
| `kaifa_doc` | LFM2 (local ONNX) | 0 / 1485 |
| `xafs_all` | `lightonai/LateOn` (shipped ONNX) | 19170 (5 / 19165) |

**Pipeline bug found+fixed on the houqin shakedown:** PyPI `pylate-onnx-export` saved `config_sentence_transformers.json` but colgrep requires `onnx_config.json` (version skew) → builder copies it (idempotent, persisted to `np-lfm2-onnx`). Materialization = chunks→files, code at raw ext (tree-sitter), else `.extracted.md` (text path). **colgrep keys indexes by (project, model)** → every query must name `--model`.

**Process note:** the 3 fan-out builds were launched with `&` inside a background shell (double-detach) → ran on Modal but lost harness tracking; a polling watcher recovered the completion signal. (Lesson: `run_in_background` XOR `&`, not both.)

→ **Phase 2** = (2a) write `rrf_merge.py` + bake indices into E2B templates [no E2B cost] · (2b) wire `next_plaid_*` arms + run n=2 A/B vs `ppr_on`/`plain` on E2B [E2B + OpenRouter cost].
