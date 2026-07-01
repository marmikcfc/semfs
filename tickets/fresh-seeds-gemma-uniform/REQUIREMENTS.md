# Fresh seeds (chanpin + kaifa) — gemma-uniform + summaries + code-lane + KNN-KG

**Status:** REQUIREMENTS — awaiting user review before build.
**Created:** 2026-06-20
**Owner:** Marmik. **Folder:** `tickets/fresh-seeds-gemma-uniform/` (pair a Linear `SEM` issue to this).
**Supersedes the seed recipe in:** SEM-37 (`tickets/wb-xafs-seeds-e2b-ready/`) — those seeds were a fast
`seed_dir` build (no code lane, no `fs_*`, no summaries). This builds them *the way we want them*.

---

## 1. Goal

Build **two brand-new, from-scratch seeds** — `chanpin` (PM) and `kaifa` (backend) — using the **same
recipe** so they are directly comparable, and so each is **fully E2B-mount-ready for benchmarking**
(grep + cat + `/kg/` overlay all work on a real FUSE mount). The recipe finally deploys the document
summarizer, unifies on gemma embeddings (incl. a real code lane), builds the KG with kNN-based
communities, and records every embedding that fails.

Non-goal: running the benchmark itself (that's the follow-up, on E2B). This ticket ends when both
seeds pass the acceptance smoke.

---

## 2. The recipe (identical for both seeds)

| # | Component | Spec |
|---|---|---|
| R1 | **Text-lane embeddings** | **gemma-q4** (EmbeddingGemma-300M ONNX, 768d) for all prose/text/tabular chunks. **FULLY FRESH re-embed** — every chunk embedded from scratch; reuse NO existing vectors (no copy+swap-lane shortcut, no inheriting old seeds) so there is **zero cross-seed contamination**. |
| R2 | **Code lane** | Real code lane (`vchunks_code`): **tree-sitter / AST-aware chunking** for code files (14 langs), embedded with **gemma-q4** (uniform) — *not* jina. Code is NEVER flattened into the text lane as `.txt`. |
| R3 | **Summaries — ONLY for content that doesn't embed well** | LLM summary (Qwen, R4) embedded **as the retrieval key, woven alongside the raw chunks** (NOT embed-only). **Scope = tables (csv/json), spreadsheets (xlsx/xls), images** — content whose raw rows/bytes embed poorly. Summary makes them *findable*; raw chunks/cells stay for chunk-mass + `cat` ground-truth. (**Images need a vision model — see D5.**) |
| R3b | **Prose docs — embed the EXTRACTED text (no summary)** | **pdf / pptx / docx / md** → `extract_text` → embed the extracted content directly via gemma-q4. **No LLM summary** — their prose embeds fine as-is. Verify `extract_text` covers each format; log any extraction miss to R7. |
| R4 | **Summary + KG LLM** | **`qwen/Qwen3.6-27B` (`unsloth/Qwen3.6-27B-NVFP4`)** self-hosted on Modal vLLM. Flags: `--max-model-len 8192 --max-num-seqs 256 --gpu-memory-utilization <tuned>`. Used for (a) the R3 summaries (during extraction), then (b) **KG entity/relation extraction — which runs ONLY after the full embedding pass (R1+R2) is complete**, because R5's kNN graph needs the vectors. |
| R5 | **Knowledge graph (after embedding)** | **Runs only after R1+R2 finish.** Entity/relation extraction (R4 LLM) → **embedding-kNN edges** (built from the fresh gemma vectors) → **Leiden communities computed over the kNN graph** (kNN used for community detection, per `kg-quality-leiden-knn-result`). Populate `graph_entity`, `graph_relation`, `graph_community`, `graph_god_node`, `edges`. |
| R6 | **POSIX file tree (`fs_*`)** | Materialize `fs_inode`/`fs_dentry`/`fs_data` with **full file content** so the mount serves browsable files + `cat` (uncapped), on-demand via FUSE. Mountable like chanpin. |
| R7 | **Failure ledger** | Per-seed `EMBED_FAILURES.<seed>.jsonl` — one line per file/chunk that **fails or is skipped** at ANY stage: `{filepath, stage, reason, bytes, ts}`. `stage ∈ {extract, embed_text, embed_code, summary, kg_extract}`. Zero silent drops. |

**Embedding-cap note (R3 rationale):** the text index caps at 1 MiB/file (`MAX_INDEX_BYTES`). Large
tables/docs are only partially *searchable* via raw chunks; the **summary is what makes them findable**,
and **`fs_*` (R6) serves the full file via `cat`**. So R3 + R6 together close the large-file gap that
made the kaifa semfs arms flail.

---

## 3. Build pipeline (on Modal — data + GPUs live there)

Per seed (`chanpin`, `kaifa`), in order, with the failure ledger (R7) written throughout:

1. **Extract + chunk** each file from `/data/corpus/<persona>_standard`:
   - code → tree-sitter AST chunks (R2);
   - tables/excel (csv/json/xlsx/xls) → per-table extraction;
   - images → vision path (see D5);
   - pdf/pptx/docx/md → `extract_text` → prose chunks (R3b);
   - everything else → text chunks.
2. **Summaries (R3, Qwen UP)** — for **tables/excel/images ONLY**, call Qwen to write a retrieval
   summary (content-hash cached, blake3); weave the summary ahead of the raw chunks. **pdf/ppt/docx get
   NO summary** (R3b).
3. **Embed FRESH (R1/R2, Qwen can be DOWN)** — every chunk (text/table/summary/prose) → gemma-q4
   `vchunks`; code chunks → gemma-q4 `vchunks_code`. **From scratch, no reuse.** Log every failure to R7.
4. **KG (R4/R5, Qwen UP) — only AFTER step 3 is fully done** — Qwen entity/relation extraction → edges;
   build the embedding-kNN graph from the fresh vectors; Leiden over the kNN graph → communities + god-nodes.
5. **Materialize `fs_*` (R6)** — write the full corpus content into the seed's POSIX tree.
6. **Verify (acceptance, §5)** → **export** seed to Drive + stage for E2B.
7. **Bake** `semfs-baked-<persona>` E2B template (seed + cases + writer libs) → **mount smoke**.

Qwen GPU is UP only for **step 2 (summaries)** and **step 4 (KG)**; gemma embedding (step 3) needs no
LLM GPU. **Stop Qwen the moment each pass ends** (cost). **KG cannot start until embedding (step 3) is
complete** — the kNN graph needs the vectors.

---

## 4. GPU / cost discipline (user-mandated)

- Use **`unsloth/Qwen3.6-27B-NVFP4`** *(verify exact HF repo at build)*. NVFP4 ⇒ Blackwell GPU.
- **Pick the cost-optimal GPU**: 27B NVFP4 ≈ ~14 GB weights → fits **one** Blackwell GPU. Default plan:
  **1× RTX-PRO-6000 (~$3.03/hr, cheapest Blackwell)**; fall back to **1× B200 (~$6.25/hr)** only if KV
  cache for 256 seqs × 8192 ctx doesn't fit or throughput is too low. Set `--gpu-memory-utilization`
  as high as stable (~0.9) on the chosen GPU.
- **Start the GPU only when needed** for KG/summary generation; **stop it the instant** that pass finishes.
- **Only ever start/stop the `gemma4-31b-*` / new `qwen*` apps that are mine** — NEVER touch shared
  `semfs-bench` / `yunying` / `shard` / other users' apps.
- **No `--enforce-eager`** (CUDA graphs on) — per `rcas/2026-06-19-vllm-enforce-eager-throughput-collapse.md`.
- All agent **benchmark** runs stay on **E2B** (real FUSE) — Modal is build/prep only.

---

## 5. Definition of done (acceptance — BOTH seeds must pass)

For each of `chanpin`, `kaifa`, verify on the exported seed:
1. `chunks` > 0; **`vchunks` (text) AND `vchunks_code` (code) both populated** — code did NOT land in the text lane.
2. **Summaries present**: a sample xlsx/csv AND a sample pdf/md have a woven summary chunk (prose, not raw rows) as the embedded key; raw chunks still present.
3. `graph_entity` / `graph_relation` / `graph_community` / `graph_god_node` / `edges` all > 0; **communities built via kNN** (record the community count + method in a build report).
4. `fs_dentry` / `fs_inode` / `fs_data` populated; mounting the seed serves a **browsable tree** (`ls` > 0) and **`cat` of a >1 MiB file returns full content**.
5. `EMBED_FAILURES.<seed>.jsonl` exists and is reconciled: `indexed + failed + skipped == files_walked` (no silent drops).
6. **E2B mount smoke**: `semfs grep` returns ranked hits, `cat` reads a full file, `/kg/` overlay reads — one live cell per seed.
7. Seed exported to Drive `semfs/experiments/`; pointer linked from the Linear issue.

---

## 6. Open decisions — CONFIRM before build

- **D1 — KG/summary LLM:** ✅ **RESOLVED — `qwen/Qwen3.6-27B` (`unsloth/...-NVFP4`)** for both summaries
  and KG (user, 2026-06-20). Not Gemma-4-31B.
- **D2 — GPU choice:** ✅ **RESOLVED — 1× RTX-PRO-6000** for the self-hosted Qwen3.6-27B-NVFP4
  (summaries + KG). `--gpu-memory-utilization` tuned as high as stable (~0.9) on it.
- **D3 — embed-only re-test:** you chose *weave* (safer). Optional follow-up: after both seeds build,
  run the **embed-only A/B** on case 289 under the new gemma+RRF-fixed stack to settle whether your
  original embed-only idea now wins. → **kept as a post-build follow-up**, not part of this build.
- **D4 — summarizer routing:** ✅ in scope — add **csv/json** routing through the summarizer; **images**
  via D5; **pdf/ppt/docx NOT summarized** (R3b — embed extracted text). Small code change.
- **D5 — image summaries:** ✅ **RESOLVED — Qwen-VL via OpenRouter** (external vision API, NO extra GPU).
  Images → OpenRouter Qwen-VL caption/summary → embed (gemma) + woven with the image's `fs_*` bytes. So
  there are TWO LLM paths: **self-hosted Qwen3.6-27B (RTX-PRO-6000)** for text/table summaries + KG, and
  **OpenRouter Qwen-VL** for image summaries only.

---

## 7. Risks / notes

- **Summaries A/B'd WORSE in 2026-06-04** (`tickets/summary-augmented-table-retrieval/` OUTCOME) on e5 +
  old RRF + case 289 — embed-only hit #72. We mitigate with **weave** (R3) + the **RRF max-rank fix** +
  gemma; D3 re-tests it. This is a known risk, not a settled win.
- **Code-lane / RRF chunk-mass** (`rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md`): high-chunk
  code files dominate RRF-sum and bury answers — most acute on **kaifa** (code-heavy). The real code lane
  (R2) + RRF max-rank aggregation should blunt this; verify in the follow-up benchmark.
- **Qwen repo name** `Qwen3.6-27B-NVFP4` — verify it resolves on HF before staging weights.
- **`fs_*` materialization** may re-embed if done via the daemon write-path; prefer a direct builder that
  preserves the gemma vectors + KG to avoid a second embed pass.
