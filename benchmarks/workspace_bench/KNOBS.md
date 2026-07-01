# semfs knobs — full reference (core product + benchmark harness)

_Last updated: 2026-06-10. Every entry verified against source (file:line cited).
Two layers: **CORE** = the Rust product (`crates/`), read at mount/search time.
**HARNESS** = the Workspace-Bench driver (`benchmarks/`), read at run time._

---

## The two you asked about

### `SEMFS_RESULT_LIMIT` (2 → 8)

**What it is.** How many ranked files `semfs grep` / search *returns* to the agent.
Default is **10** (`backend/sqlite_vec.rs:52`). We've been running the matrix with `2`.

**The mechanism (sqlite_vec.rs:63, 1416, 1444).** Search always retrieves a wide
*pool*, reranks it (cross-encoder), then `hits.truncate(result_limit())` chops the
list down to the top-N *before* it reconstructs and attaches each file's text. So
this knob controls **how many answers the agent sees**, not how hard search works.

```
retrieve (vec+fts+code) → RRF fuse → rerank POOL (RERANK_CANDIDATES)
                                          │
                                          ▼  truncate(result_limit)   ← THIS KNOB
                                      top-N hits  → attach text → return to agent
```

**Why 2 hurt case 15.** The trace showed both `semfs grep` calls returned the
correct table inline — but only **2 results**. Case 15 is a *survey/production*
task over ~14 financial spreadsheets. With only 2 hits the agent couldn't see the
other candidates, so it fell back to `find` + 7 full `openpyxl` workbook dumps
(~300K of the 437K tokens). A snippet costs ~1–2K tokens; a workbook dump costs
tens of K.

**Why 8 (not 2, not 10).** `2` is tuned for single-answer lookups (case 289:
"find THE best-selling product"). `8` gives multi-file tasks enough candidates to
avoid the openpyxl fallback while still capping the payload. `10` (the default)
is fine too; `8` is just a conservative "enough for survey, not a flood."

> **A/B caution:** changing this *and* the seed fix at once confounds the
> experiment. The matrix keeps `2` (the validated config); `8` is the first
> post-matrix follow-up so the delta is attributable.

### `SKIP_PREPARE=1`

**What it is.** A harness flag (`run_workspace_bench.sh:14`, used at `:373`) that
**skips the per-run `copytree` of the 1452-file persona workspace.**

**The mechanism.** Normally the harness runs
`prepare_workdirs_for_run.py` → `make_filesys()`, which `shutil.copytree`s the
pristine persona master into the run workdir (so the agent starts from clean
state). That copy took **364s** in our last run — 62% of total wall time.

**Why it's safe to skip for semfs arms.** semfs *mounts over* that workdir; the
FUSE mount **shadows** the copied files and serves content from the seed DB
(`~/.semfs/<tag>.db`) instead. The agent never reads the copied bytes. So for
`gfs_on`/`gfs_off` the copytree is pure waste — the only thing semfs needs is for
the **mountpoint directory to exist**.

```
SKIP_PREPARE=0 (default)         SKIP_PREPARE=1 (semfs arms)
  copytree 1452 files (364s)       reuse existing workdir (0s)
  semfs mounts over it ─┐          semfs mounts over it ─┐
  agent reads DB  ◀─────┘          agent reads DB  ◀─────┘
  (copy was never read)            (nothing to waste)
```

**The plain arm must NOT skip it** — baseline codex reads real files on disk, so
it needs the fresh copy. Matrix v2 order per case: `plain` (preps the workdir) →
`gfs_on` (skip) → `gfs_off` (skip). 33 prepares become 11. Est. ~3.3h saved.

---

## CORE — retrieval & ranking (`backend/sqlite_vec.rs`, `backend/rank.rs`)

| Knob | Default | Values | What it does |
|---|---|---|---|
| `SEMFS_RESULT_LIMIT` | `10` | positive int | Top-N files returned (see above). `sqlite_vec.rs:63` |
| `SEMFS_DOC_RETURN_CAP` | `65536` (64KB) | bytes | Per-file byte ceiling on attached text. Bounds IPC payload. `sqlite_vec.rs:58` |
| `SEMFS_RETURN_MODE` | whole-doc | `snippet` \| `chunk` | Return ONLY the matched chunk vs the whole reconstructed document. Snippet = big token cut on large-doc corpora. `sqlite_vec.rs:89` |
| `SEMFS_RERANK_CHUNKS` | (rank default) | int | How many candidate chunks the cross-encoder reranks. `rank.rs:49` |
| `SEMFS_PATH_LANE` | on | `off` to disable | Filename/path-match retrieval lane (so "find X.xlsx" works lexically). `sqlite_vec.rs:1015` |
| `SEMFS_INTEGRITY_LANE` | on | `off` to disable | Surfaces corrupt/error source pages so the agent can report breakage instead of silently missing data. `sqlite_vec.rs:1092` |
| `SEMFS_SALIENCE` | on | `off` to disable | Post-rerank salience ranking stage. `off` = deterministic raw rerank order. `sqlite_vec.rs:102,1315` |
| `SEMFS_COMENTION` | on | `off` to disable | Post-rerank co-mention ranking stage. `sqlite_vec.rs:102,1315` |
| `SEMFS_REWRITE` | off | `1`/`on`/`true`/`yes` | Cross-lingual query rewrite via LLM (needs `OPENROUTER_API_KEY`); appends target-language terms. Fails open to original query. `grep.rs:629` |
| `SEMFS_DEBUG_RANKING` | off | set to enable | Verbose ranking/scoring debug logs. `sqlite_vec.rs` |

> Note: `model_output/` is **always** dropped from results (`is_agent_output_path`,
> `sqlite_vec.rs:1444`) — a prior run's deliverable must never be retrieved as a
> source. Not a knob; a hard rule (the case-289 fabrication guard).

---

## CORE — embedding & rerank backend (`cmd/resolve.rs`)

| Knob | Default | Values | What it does |
|---|---|---|---|
| `SEMFS_EMBED_BACKEND` | `local` | `local` \| `openai` \| `openrouter` | Where embeddings are computed. `resolve.rs:55` |
| `SEMFS_EMBED_MODEL` | `multilingual-e5-small` | `embeddinggemma`/`gemma`, `e5-small`, `arctic-s` | Local fastembed registry model. Changing dims needs a **re-seed** (new index identity). `resolve.rs:27` |
| `SEMFS_EMBED_ONNX_DIR` | — | path | BYO-ONNX embedder directory (e.g. our gemma-q4 at `/home/ubuntu/gemma_q4`). |
| `SEMFS_EMBED_ONNX_BASE` | `model` | base name | ONNX file basename inside that dir (e.g. `model_q4`). |
| `SEMFS_RERANK_BACKEND` | `local` | `local` \| `cohere` \| `relace` \| `none` | Cross-encoder reranker. `local` = JINA reranker v2 int8 ONNX. `resolve.rs:59` |
| `SEMFS_RERANK_MODEL` | `cohere/rerank-v3.5` | any Cohere `/rerank` slug | Cloud reranker model slug. `resolve.rs:61` |
| `SEMFS_RERANK_BASE_URL` | OpenRouter `/api/v1` | URL | Base URL for the cohere-schema cloud reranker. `resolve.rs` |

---

## CORE — storage backend (`cmd/resolve.rs`, `cmd/grep.rs`)

| Knob | Default | Values | What it does |
|---|---|---|---|
| `SEMFS_STORAGE_BACKEND` | `sqlite` | `sqlite` \| `pgvector` \| `pglite` \| `cloud` | **The local-vs-cloud-search axis** (not the embedder). `cloud` = Supermemory embeds+searches, no local index. `resolve.rs:63` |
| `SEMFS_PG_URL` | — | conn string | Postgres connection for `pgvector` (needs `pg` build feature). `resolve.rs` |

---

## CORE — graph / knowledge-graph overlays (`cache/graph_fs.rs`, `agent_hint.rs`)

| Knob | Default | Values | What it does |
|---|---|---|---|
| `SEMFS_GRAPH_FS` | off | `on`/`1` | `/by-topic` Louvain-community directory overlay in the mount. |
| `SEMFS_KG` | off | `on`/`1` | Materializes `/kg/` (`KNOWLEDGE_GRAPH.md`, `GRAPH_REPORT.md`, `graph.json`). |
| `SEMFS_GRAPH_TOP_TOPICS` | `30` | positive int | How many top communities `/by-topic` exposes. `graph_fs.rs:49` |
| `SEMFS_GRAPH_FILES_PER_NODE` | `25` | positive int | Max files listed per topic node. `graph_fs.rs:50` |

> `SEMFS_GRAPH_FS` and `SEMFS_KG` are **distinct** — same `graph_*` tables, two
> different surfaces (a browsable dir tree vs materialized markdown/json artifacts).

---

## CORE — extraction & format-trap delivery (`cmd/grep.rs`, `cache/file.rs`, `extract/mod.rs`)

| Knob | Default | Values | What it does |
|---|---|---|---|
| `SEMFS_GREP_INLINE` | **on** | `off`/`0`/`false` to disable | Serves extracted text **inline** in grep results (reads `chunks.text`), killing the format trap for *reading* binaries. `grep.rs:768` |
| `SEMFS_EXTRACT_SIBLING` | off | `on` | Materializes read-only `<file>.extracted.md` siblings so the agent `cat`s a few lines instead of getting the whole file inlined. The *production*-side delivery. `file.rs:30` |
| `SEMFS_VLM_DESCRIBE` | off | `on` | Opt-in VLM fallback: transcribe-or-describe images/PDF pages that have no extractable text. `extract/mod.rs:175` |
| `SEMFS_SEARCH_ONLY` | off | `1`/`on`/`true`/`yes` | `ls` shows directory + KG, not a flat dump — cuts the os.walk token sink. `fs.rs:1089` |

> `SEMFS_GREP_INLINE` (read) and `SEMFS_EXTRACT_SIBLING` (author) are the **two
> delivery paths** for the same source (`chunks.text`). Inline = whole text in
> every grep hit; sibling = on-demand `cat`. Don't need both.

---

## CORE — misc

| Knob | Purpose |
|---|---|
| `SEMFS_INSTALL_DIR` | Override semfs install dir (binary/asset resolution). |
| `SUPERMEMORY_API_KEY` | Supermemory API auth (mount/sync). |
| `OPENROUTER_API_KEY` | LLM key for `SEMFS_REWRITE` + cloud rerank/embed. |
| `RELACE_API_KEY` | Auth for `SEMFS_RERANK_BACKEND=relace`. |
| `CLAUDE_CONFIG_DIR` | Where the `/kg` + grep hint is injected (`~/.claude/CLAUDE.md`). |

---

## HARNESS — run driver (`benchmarks/aws/run_workspace_bench.sh`)

| Knob | Default | What it does |
|---|---|---|
| `SKIP_PREPARE` | `0` | Skip the per-run workspace copytree (see above). `:14,373` |
| `SEMFS_FRESH` | `0` | Cold start: wipe the semfs local cache DB before the run (forces fresh pull). `:15` |
| `SEMFS_CACHE_ROOT` | `~/.cache/semfs` | Where semfs cache DBs / sockets live. `:19` |
| `SEMFS_CONTAINER_TAG` | — | Which seed tag to mount (e.g. `chanpin-gemma-q4`). |
| `DATASET` | — | Task set selector (e.g. `smoke`). |
| `BENCH_ROOT` / `REPO_ROOT` / `EVAL_ROOT` / `FILESYS_ROOT` | `/srv/...` | Path roots for the box layout. |
| `RUN_STAMP` | — | Per-run log/telemetry tag. |
| `XDG_CACHE_HOME` | — | Cache dir override (note: mounts still open `~/.semfs/<tag>.db`, not this). |

## HARNESS — codex agent adapter (`evaluation/src/agents/codex.py`)

| Knob | Default | What it does |
|---|---|---|
| `CODEX_USE_CHATGPT` | unset | Use ChatGPT OAuth (slow, ~25min) instead of the OpenRouter responses API. `:620` |
| `CODEX_WIRE_API` | `responses` | codex wire protocol. `chat` is rejected by codex 0.133. `:292` |
| `CODEX_BASE_URL` | — | Provider base URL (ripbench/OpenRouter). `:603` |
| `CODEX_API_KEY` / `OPENAI_API_KEY` | — | Provider creds (popped when `CODEX_USE_CHATGPT`). `:610,611` |
| `CODEX_CHAT_ADAPTER` | — | Enable the chat-completions adapter shim. `:305` |
| `CODEX_CHAT_ADAPTER_MAX_TOKENS` | — | Token cap for that adapter. `:403` |
| `CODEX_SANDBOX_MODE` | — | codex sandbox policy. `:105` |

---

## The config the matrix actually uses (per arm)

```
plain:    target=codex (no semfs)          SKIP_PREPARE=0  (preps the workdir)
gfs_on:   SEMFS_CONTAINER_TAG=chanpin-matrix (fresh copy of chanpin-gemma-q4)
          SEMFS_EMBED_MODEL=gemma-q4  SEMFS_EMBED_ONNX_DIR=/home/ubuntu/gemma_q4
          SEMFS_GREP_INLINE=on  SEMFS_RETURN_MODE=snippet  SEMFS_RESULT_LIMIT=2
          SEMFS_SEARCH_ONLY=on  SEMFS_REWRITE=1  SEMFS_KG=on  SEMFS_GRAPH_FS=on
          SKIP_PREPARE=1
gfs_off:  …identical… but SEMFS_GRAPH_FS=off,  SKIP_PREPARE=1
```
