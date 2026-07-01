# next_plaid — late-interaction (ColBERT/PLAID) retrieval arm on our general-purpose corpus

_Status: **PLANNING / pre-implementation** (2026-06-29). Companions: `REFERENCE.md` (source-grounded specs), `plan.html` (visual EM + flows), **`SUBTICKETS.md` (the 3 test cells + steps)**. Linear: [SEM-43](https://linear.app/semfs/issue/SEM-43) + children **[SEM-44](https://linear.app/semfs/issue/SEM-44)** (xAFS·C), **[SEM-45](https://linear.app/semfs/issue/SEM-45)** (kaifa·C), **[SEM-46](https://linear.app/semfs/issue/SEM-46)** (houqin·A). Folder: `tickets/next-plaid-late-interaction/`._

> One sentence: stand up LightOn's **real** next-plaid / PLAID late-interaction engine over **our** WB-Lite document corpus (PM/ops/logistics, binary office docs — **not** code), as a new benchmark arm `next_plaid`, and measure it head-to-head vs `ppr_on` / `plain`.

---

## 1. Goal & question

We rank files today with **RRF over 5 lanes + a bounded KG/PPR additive prior + a cross-encoder rerank** (see `REFERENCE.md §OURS`). Late interaction is a *different* bet: keep **one vector per token** and score with **MaxSim** (`Σ_q max_d q·d`) instead of collapsing each chunk to one pooled vector. LightOn's `next-plaid` (Apache-2.0, Rust) is a production PLAID engine; `ColGREP` is its agent-facing CLI.

**Q:** On *our* general-purpose corpus, does late interaction (next-plaid) beat our current `ppr_on` retrieval on **accuracy** and **tokens**? (Same metric discipline as the PPR ticket: every accuracy number paired with a token number; all runs on **E2B**.)

**"Utilize next plaid" = use their actual code, not a reimplementation.** "Match 100%" = reproduce their pipeline faithfully (algorithm + what-gets-embedded), with only two *deliberate* deviations forced by our data: a **text** ColBERT model instead of the code model, and our **extraction layer** as the document front-end instead of tree-sitter.

---

## 2. The headline decision (resolve before implementing)

Three ways to "utilize next plaid". This choice forks the entire work breakdown.

| Option | What | Fidelity | Effort | Integrated into semfs? |
|---|---|---|---|---|
| **A — ColGREP over extracted text** (recommended Phase 0) | Materialize `.extracted.md` for the corpus → run their **real `colgrep` binary** + **GTE-ModernColBERT-v1** → agent uses `colgrep` as its search tool | **100%** (their binary + their model) | **LOW** (no Rust; harness + corpus materialization) | No — parallel tool, clean A/B |
| **B — vendor the `next-plaid` crate into `semfs-core`** (Phase 1) | Add `next-plaid` + `next-plaid-onnx` as deps; new `SemanticIndex` backend `NextPlaidStore`; build `MmapIndex` at seed-bake; route `semfs grep` to it | High (their algorithm) | **MED–HIGH** (Rust + ONNX-runtime reconcile + seed build + x86 cross-build) | Yes — same grep/delivery interface as `ppr_on` |
| **C — reimplement PLAID in `semfs-core`** | Port the algorithm ourselves | Re-derivation risk | **HIGH** | Yes |

**Recommendation: A then B.** Option A answers the *science* question fastest with the *highest* fidelity (it literally is their code) — and ColGREP **is** Option-A productized, so it's the canonical way to "use next-plaid in an agent." Only if A is promising do we pay for B (production integration + apples-to-apples on the identical interface). **C is rejected** — the engine is Apache-2.0; reimplementing only invites divergence. The §8 parity checklist applies to B (and would be the spec for C if ever forced).

> **Code changes (seed-build + final test) and the A-vs-B call given the multi-config matrix are enumerated in `SUBTICKETS.md`.** Short version: **A stays for the test** (zero `semfs-core` changes), but the dual-model cells (Config C) make A's implementation heavier — a custom RRF-merge shim over two next-plaid indices, not pure `colgrep` — which is exactly the logic that would port into **B** as the eventual productionization (B also removes the delivery-format confound). So running both configs tilts the *future* path toward B, not the *test* path.

> **The bridge that makes A cheap:** ColGREP natively indexes **markdown/text** (11 text formats). Our **`.extracted.md` sidecar** layer (the format-trap fix) already turns every binary office doc into text. So materialized extracted text → ColGREP indexes our corpus with **zero reimplementation**. The filename is preserved in ColGREP's embedded `File:` field — and our own PPR finding says filename is the single biggest accuracy lever.

---

## 3. THEIRS — reference, condensed (full detail in `REFERENCE.md §THEIRS`)

`next-plaid` v1.6.0 = Cargo workspace: `next-plaid` (CPU PLAID engine), `next-plaid-onnx` (ColBERT encoder), `next-plaid-api` (axum server), `colgrep` (app). CPU-first; CUDA only for k-means/quantization.

**Pipeline:** encode → k-means centroids → PQ residual-compress → IVF postings → mmap store → PLAID multi-stage search → MaxSim.

| Component | Key facts (defaults) |
|---|---|
| Encoder | ColBERT ONNX, **dim 128**, prefixes `[Q] `/`[D] `, `query_length=48` (MASK-expanded), `document_length=300`, projection + per-token L2 **inside the ONNX graph** |
| K-means | `K = 2^floor(log2(16·√(avg_toklen·N_docs)))`, iters **4**, `max_points_per_centroid=256`, seed **42**, centroids L2-normalized; clusters **tokens** |
| Product quantization | **nbits=4** (default), `residual = emb − nearest_centroid`, `bucket_cutoffs`=quantiles `i/2^nbits`, `bucket_weights`=quantiles `(i+0.5)/2^nbits`, MSB-first bit-packing |
| IVF | `centroid → deduped, sorted DOC-ids` (one entry per doc, not per token) |
| Search (9 stages) | `n_ivf_probe=8` → centroid prune `threshold=0.4` → candidate union → cheap "bag-of-centroids" approx → keep `n_full_scores=4096` → decompress `max(1024, top_k)` (chunk 128) → exact **MaxSim** → `top_k=10` |
| MaxSim | `score = Σ_{i∈q} max_{j∈d} (q_i · d_j)`; SIMD AVX2/NEON + BLAS; always CPU |
| Fusion (app/API) | hybrid semantic + FTS5 keyword; `alpha=0.75` semantic weight; `fusion ∈ {relative_score, rrf}` |
| Structured text (ColGREP) | `Function: … Signature: … Description: … Parameters: … Returns: … Calls: … Variables: … Uses: … Code: … File: [normalized path]` — **filename embedded** |
| Path normalization | separators→spaces, split `snake_case`/`CamelCase` (`HttpClient`→`"http client"`) **before embedding** |
| Models | code: `LateOn-Code-edge` (17M) / `LateOn-Code` (130M); **text: `GTE-ModernColBERT-v1`** ← we use this; `answerai-colbert-small-v1`, `mxbai-edge-colbert-32m` |
| Deletes / updates | hard compaction + dense doc-id renumber; incremental add (buffer / centroid-expansion / rebuild). **N/A for a static benchmark seed.** |

---

## 4. OURS — current state, condensed (full detail in `REFERENCE.md §OURS`)

A persona seed (`<persona>-gemma-q4.db`) = 3 layers: **search index** (`chunks`+`vchunks` vec0 + `ffts` BM25), **FUSE tree** (`fs_*`), **KG** (`edges`/`graph_*`). Daemon mounts it; agent runs `semfs grep` → daemon → `SqliteVecStore::search_blocking`: 5 lanes → RRF → KG prior (PPR) → cross-encoder rerank → top-10.

| Thing we have | Where | Note |
|---|---|---|
| Extracted plain text per file | `chunks.text`; `Db::get_extracted_text` (`db.rs:855`) | **the re-embed substrate** for general docs |
| Extraction layer | `extract/` (docx/pptx/xlsx/pdf/ocr/soffice) | binary office → text (our tree-sitter substitute) |
| `.extracted.md` sidecars | `SEMFS_EXTRACT_SIBLING` (`db.rs:883`, `file.rs:339`) | **the ColGREP bridge** |
| Single-vector dense index | `vchunks` vec0 `float[768]`, gemma-q4 **pooled** | one vector/chunk |
| Rerank seam | `sqlite_vec.rs:1423-1438`; `Reranker` trait | candidate-text + query in scope |
| Arm plumbing | `run_matrix.py` (`SUPPORTED_ARMS:58`, `MOUNT_ARMS:97`, `arm_mount_env:116-186`, `mount_sig:645`) | env-flag → arm |
| **Per-token / multi-vector storage** | ❌ **MISSING** | vec0 is 1 vec/rowid; invariant `chunk_n==text_n+code_n` (`sqlite_vec.rs:244`) fails closed |
| **ColBERT model (token output)** | ❌ MISSING | embedder emits pooled `sentence_embedding` only |
| **MaxSim scorer** | ❌ MISSING | we RRF over ranks; no `Σ_q max_d` |

---

## 5. THEIRS ↔ OURS — the parity map (match / adapt / skip)

| Their component | Our action for general docs | Why |
|---|---|---|
| PLAID engine (k-means/PQ/IVF/search/MaxSim) | **MATCH** — use their crate/binary verbatim | This is the algorithm; reimplementing = divergence |
| ColBERT **code** model (LateOn-Code) | **ADAPT → text model `GTE-ModernColBERT-v1`** | Our docs aren't code; this is their own text-retrieval model |
| Tree-sitter unit extraction (function/class) | **ADAPT → our extraction layer** (`extract/` → `.extracted.md`); unit = file or ~`document_length=300`-token window | We have no code; we have extracted prose |
| Structured-text template (code fields) | **ADAPT** — drop `Signature/Parameters/Calls/Variables/Uses`; keep `Description`, `Code`(=content), **`File:[normalized path]`** | Code fields are meaningless for docs; filename field is the proven lever |
| Path normalization (snake/Camel split) | **MATCH** (caveat: Chinese filenames won't split — harmless no-op) | Cheap, preserves their behavior |
| Hybrid fusion (`alpha=0.75` + FTS5) | **MATCH** | Their keyword+semantic blend |
| SQL `WHERE` metadata pre-filter | **SKIP** | Not needed for the benchmark |
| int8 ONNX weights / CUDA / incremental add+delete | **SKIP** (note for production) | Static seed; CPU encode in E2B |

**Net: 3 adaptations (model, front-end, template), everything else matched, 4 features skipped.**

---

## 6. General-dataset adaptation — the concrete changes

**Our corpus has TWO file populations — handle them differently (NOT everything via extraction):**

| Population | Front-end | ColGREP path |
|---|---|---|
| **Native code/text** (`.go .py .ts .md .yaml .json …`) — **kaifa is full of these** | feed **RAW** (no extraction) | tree-sitter AST → code units (signature, calls, imports) — **ColGREP's home turf, keep it** |
| **Binary office docs** (`.pptx .docx .xlsx .pdf …`) — chanpin/houqin/yunying | `extract/` → `<file>.extracted.md` sidecar | document-level text unit |

ColGREP **auto-routes each file by type** (25 code langs via tree-sitter, 11 text/config formats via document extraction), so one index covers both. `.extracted.md` is the bridge **only for binaries**; code files must stay raw.

1. **Front-end:** their `tree-sitter → code unit` ports verbatim for our code files; our `extract_text → .extracted.md` substitutes only for binary docs. One doc-unit = one file (chunk to `document_length=300` if longer).
2. **Model — `LiquidAI/LFM2-ColBERT-350M` (one model, all personas).** Our corpus is **Chinese**; LFM2-ColBERT is a multilingual late-interaction ColBERT (8 langs incl **zh**), **dim 128** (matches next-plaid), MaxSim, doc 512 / query 32 tokens, PyLate-compatible, beats GTE-ModernColBERT-v1 on multilingual. One multilingual model handles Chinese **docs** *and* Chinese **code/comments** → **removes the per-persona-model confound** (no code-model-vs-text-model split) and the wrong-language confound. _(GTE-ModernColBERT / LateOn-Code = English-centric fallbacks only.)_
   - **Encoder prerequisite (one-time):** LFM2 ships PyLate/PyTorch (+GGUF), **NOT ONNX**. Two paths: **(a) ONNX-export** via `pylate-onnx-export` (required for ColGREP/Option A) — ⚠️ verify-gate, LFM2's hybrid conv+attn backbone may have export quirks; **(b) PyLate-direct** — embed in PyTorch and feed precomputed vectors to next-plaid's API (no ONNX). The **smoke (Tier 1) uses path (b)** to dodge the export risk entirely.
3. **Structured text (keep the filename!):** code files keep ColGREP's code template (`Signature/Calls/Uses/…/File:`); doc files use `Description: <lead> … Code: <extracted text> … File: <normalized path>`. The `File:` field is non-negotiable — PPR `FINDINGS.md`: a filename string was the largest single accuracy lever (houqin `ppr_on` 9.7%→17.1%).

---

## 7. Exact work breakdown (with verify gates — CLAUDE.md §4)

### PHASE 0 — ColGREP-over-extracted-text arm (Option A; faithful, low-effort, FIRST)

| # | Step | Verify gate |
|---|---|---|
| 0.0 | **Corpus composition preflight (esp. kaifa).** Count code files vs binary docs in each persona seed → decides kaifa's model (code vs text) and confirms code files are present as raw files in the FUSE tree (`fs_data`). | per-persona `{code_files, doc_files}` table; kaifa model chosen. |
| 0.1 | **Index over the FUSE mount + sidecars (NOT a flat text dump).** Mount the seed with `SEMFS_EXTRACT_SIBLING=on` so each binary doc gets a `<file>.extracted.md` sibling; **code/text files stay raw** for tree-sitter. (Code files must NOT be converted to `.extracted.md`.) | mount shows raw `.go/.py/...` + `.extracted.md` next to each binary; sidecar count == binary-doc count. |
| 0.2 | **Encoder = `LFM2-ColBERT-350M`.** Tier-1 smoke: **PyLate-direct** (PyTorch embed, no ONNX). For ColGREP/Option A: `pylate-onnx-export` LFM2 → ONNX (⚠️ verify export of the hybrid backbone), build/install `colgrep` (x86_64-linux), `colgrep init <mount>` (`--code-only` OFF). | PyLate path: model loads + embeds a string to `[n_tok,128]`. ColGREP path: `colgrep "<phrase>"` hits the right file; export produces a valid `model.onnx`. |
| 0.3 | **Wire the `next_plaid` arm.** In `run_matrix.py`: add `next_plaid` to `SUPPORTED_ARMS` (`:58`). Bake the ColGREP index into the E2B template (preferred) or build at cell start. Make the agent's search tool = `colgrep` (PATH shim, or `colgrep --install-claude-code` / codex hook). | A single smoke cell shows the agent invoking `colgrep` and producing a judged deliverable. |
| 0.4 | **Smoke + A/B.** houqin decisive cases `358,357,251,267`, n=2, arms `ppr_on` vs `next_plaid` (reuse `run_map_smoke.sh` pattern; agent LLM = GLM on Modal, search = colgrep on E2B CPU). | `results.jsonl` + `judged.jsonl` land; report **accuracy AND tokens** for both arms (analyze-benchmark-results skill). |
| 0.5 | If promising → full houqin (30 cases) n≥2 matched, then decide on Phase 1. | Matched-n A/B; mine actual responses, not just scores. |

### PHASE 1 — vendor the engine into semfs (Option B; only if Phase 0 wins or we want same-interface A/B)

| # | Step | Verify gate |
|---|---|---|
| 1.1 | **Deps + ONNX reconcile.** Add `next-plaid` + `next-plaid-onnx` (path/git). Reconcile `ort 2.0-rc.11` (theirs) vs our fastembed ONNX usage. | `cargo build` for `x86_64-unknown-linux-gnu` on Modal succeeds. |
| 1.2 | **Per-token encoder.** Wire `next-plaid-onnx::Colbert` with GTE-ModernColBERT (dim 128, `[Q]`/`[D]`, qlen 48, dlen 300). | encode a string → `[n_tokens, 128]`, rows L2-normed. |
| 1.3 | **Backend `NextPlaidStore`** impl `SemanticIndex` (`backend/mod.rs:39`): build `MmapIndex` at seed-bake; `search()` → `MmapIndex::search(top_k=10, n_ivf_probe=8, threshold=0.4)`; bypass the `chunk_n==text_n+code_n` invariant (`sqlite_vec.rs:244`) for this backend. | unit test: index 100 docs, query → expected top-1; matches a direct next-plaid run. |
| 1.4 | **Structured-text builder** (parity §6) incl. path normalization. | golden-string test of the embedded text for a sample file. |
| 1.5 | **Seed build + bake.** Add a next-plaid index phase to `semfs_modal.py` (`~:947`); bake into per-persona E2B templates. | `inspect_seed`-style check: next-plaid index dir present + non-empty; FUSE+KG layers intact. |
| 1.6 | **Arm wiring (daemon-side, like `ppr_*`).** `next_plaid` → `arm_mount_env` sets the backend flag; add the flag to `mount_sig` (`:645`) so workers re-mount; `semfs grep` routes to `NextPlaidStore`. | smoke cell: grep returns next-plaid results in the normal delivery format. |
| 1.7 | **Full A/B** `next_plaid` vs `ppr_on` vs `plain`, matched n, on E2B. | accuracy + tokens table; honest no-deliverable scoring (reuse `rejudge_loop.py`). |

---

## 8. 100% parity checklist (the "match their implementation" gate)

Tick each before claiming fidelity. MATCH = identical to source; ADAPT = deliberate doc-driven change; SKIP = out of scope (note in results).

**Engine (MATCH — verbatim from their crate):**
- [ ] k-means: `K=2^floor(log2(16·√(avg_toklen·N)))`, iters 4, max_pts 256, seed 42, centroids L2-normalized
- [ ] PQ: nbits **4**, residual=emb−centroid, cutoffs=quantiles `i/2^nbits`, weights=quantiles `(i+0.5)/2^nbits`, MSB-first packing
- [ ] IVF: centroid→deduped sorted **doc-ids**
- [ ] search: `n_ivf_probe=8`, `centroid_score_threshold=0.4`, `n_full_scores=4096`, `n_decompress=max(1024,top_k)`, `DECOMPRESS_CHUNK_SIZE=128`, `top_k=10`
- [ ] MaxSim: `Σ_q max_d (q·d)` (CPU)

**Encoder (MATCH except model choice):**
- [ ] dim 128; prefixes + mask-token from the model's own pylate config (not next-plaid defaults); projection + per-token L2 in-graph
- [ ] ADAPT: model = **`LiquidAI/LFM2-ColBERT-350M`** (multilingual, Chinese); set `document_length=512`, `query_length=32` per LFM2's config
- [ ] encoder setup done: ONNX-exported (ColGREP path) **or** PyLate-direct embeddings (library/smoke path) — export is a verify-gate (hybrid backbone)
- [ ] corpus fully embedded+indexed once (fresh 128-d per-token index; NOT a reuse of gemma `vchunks`)

**Input / what-gets-embedded (ADAPT — split by file population):**
- [ ] **code/text files fed RAW** → tree-sitter code units (kaifa) — MATCH their code template (`Signature/Calls/Uses/…/File:`)
- [ ] **binary docs only** → `.extracted.md` → document unit, windowed to ≤`document_length`; doc template keeps `Description`, `Code`(=content), **`File:[normalized path]`**, drops code-only fields
- [ ] path normalization: separators→spaces, snake/Camel split (Chinese paths: no-op, fine)
- [ ] **one model per index** chosen per-persona by composition (kaifa code-model vs doc personas text-model); compare within-persona

**Fusion (MATCH):**
- [ ] hybrid: semantic + FTS5 keyword, `alpha=0.75`, `fusion` mode chosen
- [ ] ⚠️ **confirm the exact ColGREP fusion constant/formula from `colgrep/src`** (gap: the document-layer agent was stopped before quoting it verbatim)

**Skipped (note in writeup, not failures):** SQL `WHERE` filter · int8 ONNX weights · CUDA · incremental add/delete (static seed).

---

## 9. Risks & open questions

- **Mixed corpus / code files (kaifa).** Code files must be indexed RAW (tree-sitter), not extracted; only binary docs get `.extracted.md`. ColGREP auto-routes by type, so one index handles both — but it uses **one model per index**, so kaifa needs a model decision (code vs text) that the §0.0 composition preflight resolves. Compare within-persona to avoid the per-persona-model confound.
- **CPU encode latency in E2B.** The ColBERT model runs CPU-only in a 4-vCPU sandbox; encoding the corpus + per-query may be slow. Mitigate by baking the index (Phase 0.3) so only query-encode is online. Measure it.
- **Unit granularity.** Whole-file vs `document_length=300` windowing changes recall and the doc-id→file mapping (one file may become N units → need a unit→file rollup for delivery/judging).
- **The arena caveat still stands.** PPR `FINDINGS.md`: WB-Lite hands the agent the whole workspace, so reorder-only mechanisms barely gate accuracy. Late interaction is also a ranking mechanism → it may inherit the same ceiling. Consider whether a **discovery** arena is the truer test (echoes E8/E11). Flag, don't pre-decide.
- **LFM2 ONNX export risk.** LFM2-ColBERT ships PyLate/PyTorch + GGUF, no ONNX. `pylate-onnx-export` of its hybrid conv+attn backbone may need work — verify before relying on the ColGREP path. Smoke uses PyLate-direct to avoid this. _(Newer `LFM2.5-ColBERT-350M`, June 2026, 11 langs — consider as an upgrade.)_
- **Late interaction is embed-first.** A full corpus embedding pass is mandatory before any query (per-token vectors → centroids → PQ → IVF index); bake it into the seed/template. Fresh 128-d index, not a reuse of gemma `vchunks`. 350M-on-CPU cost is bake-time, not query-time.
- **Open:** confirm ColGREP's text-file chunking (whole-file vs windows) from source; confirm LFM2 doc/query prefixes + mask-token from its pylate config.
- **DECISION NEEDED before code:** confirm **A-then-B** (vs jump straight to B, or A-only). This sets the whole branch.
