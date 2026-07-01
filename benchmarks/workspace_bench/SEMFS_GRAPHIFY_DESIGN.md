# semfs × graphify — Comprehensive Design, Algorithms & Experiments

**What this is.** A single reference for anyone who wants to understand: **what** we're trying to solve, **where** our solution is today, **what** the remaining problems are, and **how** a knowledge-graph layer (à la graphify) can solve them — with the underlying algorithms, technologies, diagrams, and our experiment log spelled out.

**Date:** 2026-05-27. Sources: a read of the `semfs` codebase, the graphify README, and 12 Workspace-Bench runs on case 289.

---

## Table of contents
- [Part I — What are we solving?](#part-i--what-are-we-solving)
- [Part II — Underlying technologies & algorithms](#part-ii--underlying-technologies--algorithms)
- [Part III — Where we are (current architecture)](#part-iii--where-we-are-current-architecture)
- [Part IV — The problems (efficiency bottlenecks)](#part-iv--the-problems-efficiency-bottlenecks)
- [Part V — The experiments (what we ran and learned)](#part-v--the-experiments)
- [Part VI — How graphify solves it](#part-vi--how-graphify-solves-it)
- [Part VII — Roadmap](#part-vii--roadmap)

---

## Part I — What are we solving?

### I.1 The one-sentence problem
> A coding agent dropped into a large, unfamiliar workspace **doesn't know where anything is**, so it **explores** — listing directories, guessing filenames, grepping, opening files — and that exploration burns **enormous numbers of tokens**. We want a filesystem that lets the agent *ask by meaning* and *consume clean, relevant content*, so exploration collapses from dozens of steps to a few.

### I.2 Why agents crawl (first principles)
Exploration is **rational under uncertainty**. Given "the answer is in 1 of 2,128 files; I don't know which; filenames are ambiguous and multilingual; some files lie about their format," the optimal strategy *is* to enumerate, broad-search, and probe. **You cannot instruct this away** — it's the correct response to the agent's information state. The lever is the *environment*: change what the agent can ask and what it gets back.

The deepest cause is a property of plain filesystems:
```
A POSIX directory listing (readdir) is an UNRANKED, UNFILTERED enumeration.
→ every file looks equally (ir)relevant
→ the agent must read/guess to find relevance
→ relevance-filtering happens in the AGENT (= tokens), not in the index.
```

### I.3 The token economics that make this expensive
Agents run in a **loop**: each turn re-sends the system prompt + the entire accumulated transcript (tool calls + their outputs) to the model. With **prompt caching**, that replayed prefix is billed as `cache_read`. So:

```
real cost  ≈  Σ over turns ( everything read so far )
          ≈  (content put into context)  ×  (number of turns)
```

A 30 KB file read on turn 10 is re-charged (as `cache_read`) on turns 11…N. **This is why our Claude runs were 95% `cache_read`** and why shrinking per-read artifacts matters far more than it first appears: you don't save the cost once, you save it on every replay.

```
 turns →   1    2    3    ...                          31
 ctx  →   [s]  [s+a][s+a+b] ...                  [s+a+b+...+z]   ← each re-sent (cache_read)
 plain:    big additions (ls dumps, 30KB HTML, parse output) accumulate → ~600k
 semantic: tiny additions (ranked excerpt, clean table), ~4 turns        → ~35k
```

---

## Part II — Underlying technologies & algorithms

This part explains each moving piece. Read it once; the rest of the doc assumes it.

### II.1 FUSE (Linux) and NFSv3 (macOS) — how the mount works
A **filesystem in userspace**: the kernel forwards file operations to our daemon as callbacks; we answer them.

```
   agent: open()/read()/readdir()        ← ordinary syscalls
        │
   ┌────▼─────────────┐   Linux: kernel fuse.ko → /dev/fuse → our daemon
   │ kernel VFS layer │   macOS: kernel NFS client → localhost:port → our NFS server
   └────┬─────────────┘
        │ callback (lookup, getattr, read, readdir, write, …)
   ┌────▼─────────────┐
   │ semfs daemon     │  crates/semfs-core/src/mount/{fuse.rs, nfs.rs}
   │ implements VfsOps │  19 ops; NO xattr/access
   └──────────────────┘
```
- **Linux** uses real FUSE (`fuse.ko` + the `fuser` Rust crate).
- **macOS** avoids the macFUSE kext entirely: semfs runs a **pure-Rust NFSv3 server on localhost** and asks macOS to mount it as an NFS share. Zero install beyond the binary.
- The 19 ops we implement: `lookup, getattr, setattr, readlink, mkdir, rmdir, opendir, readdir, open, create, read, write, flush, release, fsync, unlink, rename, symlink, link, statfs`. There is **no semantic hook in any of them** — they're literal. (This is the heart of bottleneck B1.)

### II.2 The local cache — SQLite + WAL, content-addressed
The mount is backed by a SQLite DB at `~/.cache/semfs/<org>/<tag>.db`:

| table | holds | role |
|---|---|---|
| `fs_inode` | per-file metadata (mode, size, times) | the stat() data |
| `fs_dentry` | (parent_ino, name) → child ino | the directory tree |
| **`fs_data`** | **file bytes, chunked into 4 KB BLOBs** | **SOURCE OF TRUTH** |
| `fs_symlink` | symlink targets | links |
| `fs_remote` | ino → Supermemory document id | sync mapping |
| `push_queue` | pending writes (latest-wins coalescing) | write path |
| `sync_meta` | watermarks for delta pull | sync bookkeeping |
| `chunks`, `ffts`(fts5), `vchunks`(vec0) | **local semantic index** (Phase 2/4) | search |

- **WAL (write-ahead logging):** readers never block the single writer; survives restarts → offline reads.
- **Chunked content (4 KB):** files are stored as fixed-size BLOB chunks, so partial reads and large files work without loading everything.
- **Why SQLite:** one embeddable file, transactional, no server, and `sqlite-vec` plugs a vector index into the *same* DB.

### II.3 Embeddings & vector search — the core of "semantic"
**An embedding** is a function `text → ℝᵈ` (a d-dimensional float vector, e.g. d=384 for all-MiniLM, 768, or 1536) produced by a neural model trained so that *semantically similar text lands nearby*. Similarity is **cosine**:
```
sim(a,b) = (a·b) / (|a||b|)      ∈ [-1, 1]   (1 = same meaning)
```
**Semantic search** = embed the query, find the nearest chunk-vectors:
```
 query "best selling products"
   │ embed
   ▼
 q ∈ ℝ³⁸⁴ ──► k-nearest-neighbours over all chunk vectors ──► top-k chunks
                                                              (ranked by cosine)
```
Two ways to do the KNN:
- **Brute force (exact):** compute cosine vs *every* vector, sort. `O(n·d)`. Simple, exact; fine to ~50K vectors, then latency hurts. **`sqlite-vec`'s `vec0` virtual table does this** (exact KNN).
- **ANN / HNSW (approximate):** *Hierarchical Navigable Small-World* graph — a multi-layer proximity graph you greedily descend; `~O(log n)`, scales to millions, slightly approximate. **`pgvector` offers HNSW/IVFFlat.**

> semfs's local backend (`SqliteVecStore`, `--offline`) uses sqlite-vec brute force; the cloud backend (Supermemory) does the ANN/hybrid server-side.

### II.4 Chunking & line-range pointers
Whole documents are too coarse to embed usefully, so each file is split into **chunks** (token windows, often overlapping). Each chunk is embedded *and* its **line range** in the source file is recorded. So search returns:
```
filepath : line_start-line_end : <verbatim chunk text>
```
The chunk text is **verbatim** (a real slice, not a paraphrase) and the line range is a **pointer back into `fs_data`** (the source of truth) — so the agent can read the exact lines if it needs more. (This is why excerpts are "verbatim but partial," and the full file is "verbatim and complete.")

### II.5 Hybrid retrieval — vector + lexical (BM25)
Pure vector search misses exact tokens (IDs, rare terms); pure keyword misses meaning. **Hybrid** runs both and fuses:
```
 vector KNN (vchunks/vec0)  ─┐
                             ├─► fuse (e.g. Reciprocal Rank Fusion) ─► final ranking
 BM25 keyword (ffts/fts5)   ─┘
```
- **BM25** = the classic TF-IDF-style relevance score (term frequency saturated, length-normalized). fts5 is SQLite's full-text engine.
- **RRF** (reciprocal rank fusion): `score(d) = Σ 1/(k + rank_i(d))` across the two result lists — robust, parameter-light.

### II.6 The Supermemory API & R2 object storage
The cloud backend speaks three endpoints, plus object storage:
```
 POST /v3/documents   push a file (create/update)              → 200/402/409/400
 POST /v4/search      hybrid semantic search (returns chunks)  ← `semfs grep`
 POST /v4/profile     synthesize a user/personal memory digest ← profile.md
 R2 (Cloudflare) bucket  raw file bytes, fetched via PRESIGNED URLs
```
- **Presigned URL (S3/R2 SigV4):** a time-limited URL with the signature in the query string (`X-Amz-Signature`, `X-Amz-Expires`). The client downloads raw bytes **without holding any cloud credentials** — Supermemory mints the URL; semfs just GETs it. (These R2 GETs are *object-storage egress*, **not** billable search API calls — a distinction that cost us a wrong "140 API calls" reading until we checked the dashboard.)
- **"Rehydration":** on first read of a file whose bytes aren't in `fs_data` yet (esp. binaries), semfs fetches them from R2.

### II.7 Prompt caching mechanics (why the bills look the way they do)
Anthropic prompt caching has four usage counters:
```
 input_tokens                 fresh, uncached input               1.00×
 cache_creation_input_tokens  written to cache (first sight)      1.25×
 cache_read_input_tokens      replayed from cache                 0.10×   ← cheap, but counts
 output_tokens                generation                          ~5×
```
- `input_tokens` is the **uncached remainder only** — when caching is on, the bulk of the prompt moves into `cache_read`. (Summing only `input+output` undercounts ~50× — a bug we hit and fixed; see Part V.)
- The cache has a **~5-minute TTL**; agent loops keep it warm, so the system prompt + transcript ride as `cache_read` every turn → it dominates (95% in our runs).

### II.8 The agent tool-loop (codex / Claude)
```
 loop:
   model emits a tool call (Bash/Read/Grep/…)        ← decided from current context
   harness runs it, appends {call, output} to context
   context (system + prompt + all prior {call,output}) re-sent next turn  ← cache_read
 until the model emits a final answer
```
Every tool output **persists in context** and **replays**. So an agent that makes 31 tool calls with fat outputs pays for those outputs ~31 times.

### II.9 tree-sitter AST (graphify's code path)
**tree-sitter** is an incremental parser that turns source code into a **concrete syntax tree** using a per-language grammar. A small query language extracts nodes (functions, classes, imports) **deterministically and locally — no API, no model**. This is why graphify can graph code for free.

### II.10 Leiden community detection (graphify's clustering)
Given a graph, **community detection** partitions nodes into densely-connected groups. **Leiden** (an improvement over Louvain) iteratively moves nodes between communities to maximize **modularity** (edges-inside vs expected-by-chance), with a refinement step that *guarantees well-connected communities*. A **resolution** parameter tunes granularity (higher = more, smaller communities). The most-connected node in a community is a **hub / "god node."**
```
        ·   ·                          ╭───── community A ─────╮
      · · · · ·        Leiden          │  ·  · (★hub) ·  ·      │
     ·  · · · ·   ───────────────►     ╰───────────────────────╯
        · ·                             ╭── community B ──╮
         ·                              │   · (★hub) ·    │
                                        ╰─────────────────╯
```
For a workspace, communities ≈ topics and god nodes ≈ the central concepts → **a computed digest of "what this place is about."**

### II.11 Property graph & confidence
A **property graph**: nodes (entities with properties) + edges (typed relationships with properties). graphify tags each edge **`EXTRACTED`** (read from code structure), **`INFERRED`** (LLM-semantic), or **`AMBIGUOUS`** — so a consumer knows what to trust vs verify.

---

## Part III — Where we are (current architecture)

### III.1 System diagram
```
            agent (codex / claude / shell)
                     │  POSIX syscalls  +  `semfs grep`
        ┌────────────▼─────────────┐
        │  MOUNT  (fuse.rs/nfs.rs)  │  19 literal POSIX ops
        └────────────┬─────────────┘
                     │ VfsOps
        ┌────────────▼─────────────┐   SQLite @ ~/.cache/semfs/<org>/<tag>.db
        │  LOCAL CACHE              │   fs_inode · fs_dentry
        │   fs_data = BYTES ◄───────┼── SOURCE OF TRUTH (verbatim, 4KB chunks)
        │   fs_remote · push_queue  │
        │   chunks·ffts·vchunks ────┼── derived semantic index (sqlite-vec, Phase 2/4)
        └───┬───────────────┬───────┘
            │ pull/push      │ search (SemanticIndex trait)
   ┌────────▼──────┐   ┌─────▼──────────────┐
   │ SYNC ENGINE   │   │ BACKEND            │
   │ A/C/F pull    │   │  CloudIndex ───────┼─► Supermemory  /v3 /v4/search /v4/profile
   │ D/E push+poll │   │  SqliteVecStore    │                 + R2 (presigned URLs)
   └───────────────┘   │   (--offline)      │
                       └────────────────────┘
```

### III.2 The three planes
1. **Literal POSIX serve** — `ls/cat/find/…` → FUSE → `fs_data` (verbatim; lazy R2 rehydration). *No meaning.*
2. **Semantic** — `semfs grep "<q>"` → `SemanticIndex::search` → CloudIndex (`/v4/search`) or local sqlite-vec. Returns `file:lines:chunk`. **The only semantic verb.**
3. **Sync/write-through** — writes → `push_queue` → `/v3/documents` → embedded (disabled under `--no-push`).

### III.3 Build status
| Capability | State |
|---|---|
| FUSE/NFS mount, SQLite cache, lazy R2 rehydration | ✅ shipped |
| Cloud semantic search (`semfs grep` → `/v4/search`) | ✅ shipped |
| Sync engine (pull loops A/C/F, push D/E) | ✅ shipped |
| `profile.md` virtual file | ✅ shipped — **but empty for document workspaces** |
| Transcription siblings (extracted text for binaries) | ⚠️ partial (PDFs; not steered-to) |
| Backend-agnostic `SemanticIndex` trait | ✅ Phase 1 |
| **Local** sqlite-vec backend (`--offline`) | 🚧 Phase 2/4 in progress |
| Typed-edge **graph** / community digest | ❌ not built |

---

## Part IV — The problems (efficiency bottlenecks)

```
 agent's task question: "what are the best-selling products + their conversion rates?"
        │
        ├─ B1 DISCOVERY ── ls/find/grep crawl (guess filenames, EN+other languages) ── only `grep` is rerouted, & only if used
        ├─ B2 CONSUME ──── cat raw file → FORMAT TRAP (xlsx is HTML) → openpyxl→pandas→HTMLParser ── no clean read path steered-to
        ├─ B3 ORIENT ───── crawl to understand the workspace ── profile.md is EMPTY
        ├─ B4 RELATE ───── "files related to X?" → filename guessing ── no edge layer
        └─ B5 REPLAY ───── every read replays in cache_read each turn ── cost = content × turns
```

| # | Bottleneck | Mechanism | Root cause |
|---|---|---|---|
| **B1** | Discovery crawl | `ls`×8 + `find`×~14 guessing names | no semantic `find`/`ls`; only `grep` rerouted |
| **B2** | Consumption + format trap | 30 KB HTML read + parse-fight (3/4 runs) | no agent-facing normalized-content read path |
| **B3** | Orientation | crawl to map the workspace | `profile.md` empty (user-profile feature, not workspace digest) |
| **B4** | Relations | filename guessing for related files | **no graph/edge layer** (embeddings ≠ relations) |
| **B5** | `cache_read` replay multiplier | content re-sent every turn | verbose outputs × many turns → 200–600k |
| **B6** | Delivery (now fixed) | agent never used `semfs grep` (runs 4–10) | hint never reached Claude — see Part V |

**Structural summary.** semfs answers *"where does this string/name appear?"* (literal) and *"what's similar to this query?"* (`semfs grep`). It does **not** answer *"what is this workspace about?"* (B3), *"what's related to this?"* (B4), or provide a *cheap clean read of a located file* (B2). Those are where the tokens go.

---

## Part V — The experiments

Task: Workspace-Bench **case 289** ("best-selling products"), PM/chanpin workspace (2,128 files). The answer lives in `top10_product_status_table.xlsx` (which is **HTML disguised as .xlsx**) and `best_selling_product_core_data_list.txt`. EC2 `m7i.xlarge`. Token totals are cache-inclusive for Claude, OpenRouter-counted for codex — **compare command counts and within-model, not raw cross-model totals.**

### V.1 Baselines & the headline result
| Variant | Commands | Semantic search? | Tokens | Note |
|---|---:|---|---:|---|
| plain codex | 9 | ❌ | 143,837 | find→openpyxl/pandas/zipfile-XML→file→sed→cp (hit the format trap) |
| **semfs-codex** | **3** | ✅ 1 query | **35,763** | `cat profile.md` (empty) → **one** `semfs grep "…" .` → write (answer from excerpts). **−75%** vs plain codex |
| plain claude (run 3 / run 9) | 113 / 31 | ❌ | ~257k / 206,941 | heavy crawl; also hit the format trap (openpyxl→xlrd→pip) |
| semfs-claude (runs 4–8) | 21–59 | ❌ (0 `semfs grep`) | 154k–617k | crawled regardless — **delivery bug** |

**Codex's winning trace (3 commands):**
```
1. pwd && ls -la && cat profile.md            (profile.md was empty → no real orientation)
2. semfs grep "best-selling product data … transaction amount conversion rate"  .
3. mkdir model_output && cat > …list.txt <<EOF   (the answer rows came straight from the excerpts)
```
Codex never opened a data file — the `semfs grep` excerpts carried the answer, so it **skipped the entire format trap**. That single semantic query *is* the −75%.

### V.2 The delivery investigation (runs 4–12) — the core saga
We spent runs 4–12 discovering **why Claude never used `semfs grep`**, and it was not what it looked like:

1. **Runs 4–8 (semfs-claude):** 0 `semfs grep`, always crawled. First hypothesis (wrong): "Claude ignores the hint."
2. **`agent_hint.rs` writes the hint to `~/.claude/CLAUDE.md`.** But: (a) `ClaudeCode.js` sets `HOME=<workdir>` → `~/.claude` is the wrong dir; (b) the **Claude Agent SDK loads no `CLAUDE.md` by default** (`settingSources` defaults to `[]`). So the file-based hint was orphaned. Codex worked because the **Codex CLI auto-loads `~/.codex/AGENTS.md`** (same hint text, different loader).
3. **Tried `appendSystemPrompt`** (run 6→10) + a **canary**: the hint instructed Claude to begin its first line with `[SEMFS-ACK]`. **Run 10: `[SEMFS-ACK]` count = 0** → the hint *never reached the model*. Discovery: **`appendSystemPrompt` is not a real SDK option** — silently dropped. (Our stderr breadcrumb had only proven *we set a key*, not that the SDK honored it. The canary is what exposed the truth.)
4. **Run 11 — prompt-prepend:** put the hint in the **user prompt**. `[SEMFS-ACK]` = 6, **`semfs grep` used (7 queries)**. *It works.* But 599,529 tokens (over-queried + re-read raw files).
5. **Run 12 — project `CLAUDE.md`** (what we actually wanted): write the hint to `<cwd>/CLAUDE.md`, set **`settingSources:['project']`** + `systemPrompt:{preset:'claude_code'}`. Reverted the prompt-prepend so the canary lives **only in CLAUDE.md**. **`[SEMFS-ACK]` = 6** (∴ the SDK loaded the project CLAUDE.md), `semfs grep` used, **220,612 tokens** (~3× cheaper than prompt-prepend). **Parity with codex achieved**, via the SDK-correct file/config.

**Lessons:** (1) delivery ≠ "we set an option" — verify the *model received it* (canary); (2) the Claude SDK loads **project** CLAUDE.md only, only with `settingSources:['project']` + the `claude_code` preset; (3) "Claude ignores hints" was a *delivery* artifact, not a compliance fact.

### V.3 The transparent-shim experiment (runs 7–8)
Idea: replace `rg`/`grep` with a shim so Claude's *normal* tools become semantic, no compliance needed. The docs gave the lever: **`USE_BUILTIN_RIPGREP=0`** makes Claude's native `Grep` tool resolve `rg` from PATH (instead of the SDK's bundled binary).
- **Worked:** the native Grep tool **did call our shim** (8 `rg` invocations).
- **But routed 0:** those 8 were Claude Code **startup `--files` scans** of its own `.claude/` config dirs (correctly passed through), and in those runs Claude **didn't content-grep at all** (used `ls`/`find`/`Read`). Also the Grep tool uses `rg --json`/`-l` modes, which our v1 shim passed through. We then taught the shim to emulate `--json`/`-l`/`-c` and map paths — verified on a fake mount — but a *live* Grep-heavy semfs run never coincided to exercise it.
- **Bug found & fixed (flagged in review):** the shim's marker-parse used `grep`, which re-entered the shim (it's first on PATH) → recursion; fixed with a pure-bash parse.

### V.4 The token-accounting fix
`_parse_usage_from_stdout` summed only `input_tokens + output_tokens`, dropping `cache_read`/`cache_creation`. Result: a Claude run reported **5,010** when the true total was **257,596** (~50× undercount). Fixed to sum all four fields. (This is why §II.7 matters: `input_tokens` is the *uncached remainder*, not the prompt size.)

### V.5 What `profile.md` actually contained
158 bytes — **just a boilerplate header, no content.** `POST /v4/profile` synthesizes a *user/personal* memory profile; a document workspace has none → empty. So codex's "orient via profile.md" was a no-op; its win was purely the one `semfs grep`. (⇒ B3: orientation has no working answer today.)

### V.6 The codex-vs-claude behavioral difference (once both used semfs)
| | semfs-codex | semfs-claude (run 11/12) |
|---|---|---|
| `semfs grep` queries | **1** (rich, whole-workspace) | **~7** (narrower, iterative) |
| trusted the excerpts? | ✅ wrote answer from them | ❌ re-read the raw file (format trap) |
| commands | 3 | ~20 |
**Both now use semfs; the gap is discipline:** codex does *orient → one query → trust → done*; Claude does *many queries → verify → re-read*. The re-read is *correct* for full-data tasks (the excerpt is partial) — but it should land on **normalized content**, not the raw binary. (⇒ B2.)

---

## Part VI — How graphify solves it

### VI.1 graphify pipeline (recap with the algorithms)
```
 files ─► EXTRACT ──────────► CONNECT ─────────► CLUSTER ───────► PERSIST ──────► QUERY
          code: tree-sitter   property graph     Leiden           graph.json      get_neighbors
            AST (local,free)   nodes + edges      communities      (committed,     shortest_path
          non-code: LLM        EXTRACTED/         + god nodes      auto-rebuild)   query_graph
            → entities +       INFERRED/                                           via MCP, --budget
            normalized text    AMBIGUOUS
```
Everything expensive happens **once at ingest**; queries are cheap traversals with **small results** and a **token budget**.

### VI.2 The borrow map (bottleneck → graphify element → integration)
| semfs bottleneck | borrow | integrate at | effect |
|---|---|---|---|
| **B2 consumption/trap** | **normalized content + entity extraction** | extend transcription → structured tables; store beside `fs_data`; steer reads to it | the *necessary* re-read = a ~480-token clean table, not 30 KB HTML + parse-fight — **biggest single win** |
| **B3 orientation** | **Leiden community digest (god nodes)** | compute at ingest; **back `profile.md`** with it | one digest read replaces the crawl; fixes empty profile.md |
| **B4 relations** | **typed, confidence-tagged edges + `get_neighbors`/`shortest_path`** | **add an edge table to the cache** (none today); build edges local-AST-first + LLM | "related to X" = one hop; trust tags cut re-verify reads |
| **B1 discovery** | edges + ranking → rankable `ls`/`find` | a `neighbors(node, rank=task)` op | ranked shortlist instead of a crawl |
| **B5 replay** | **`--budget` caps + small sub-answers** | cap `semfs grep` output; graph returns nodes/edges not files | smaller per-turn content → less `cache_read` (the 95%) |
| **B6 delivery** | **MCP graph tools** | expose semfs ops as MCP tools | first-class tools land in the channel agents reliably use |
| ingest cost | **local-AST-first + dedup/cache** | code locally; LLM only for unstructured media | cheap seeding; emits the normalized-content artifact |

### VI.3 The unifying principle
graphify's efficiency is an **inversion**: move work *out of the per-query loop* into a *once-at-ingest build*, then answer by traversal with capped, small results.
```
 Claude (today):   cost = (re-read + re-crawl) PER QUERY      → 31 cmds × 600k
 graphify-style:   cost = build ONCE at ingest, then cheap lookups → 3 cmds × 35k
```
semfs **already does this for *search*** (embed once → query). graphify extends the same pattern to **structure** (edges → B4/B1), **orientation** (community digest → B3), and **clean reads** (normalized content → B2).

### VI.4 Worked intuition — how the graph forms on case 289
```
 EXTRACT (LLM, once):
   top10_…xlsx        → Doc + concept[best-selling] + metric[txn amount] + metric[conv rate] + 10 product entities
   best_seller_list   → Doc + concept[best-selling]  (SHARED) + products (SHARED) + metrics (SHARED)
   02_sales_perf.json → Doc + concept[sales-performance]

 CONNECT: shared nodes link the two product docs strongly; sales-perf weakly.
 CLUSTER (Leiden):
        [txn amount] [conv rate]
              \        /
  best_seller ─[ BEST-SELLING PRODUCTS ]★god─ top10      ← community "product sales analytics"
              \____[ products ×10 ]____/
                       ┆ (weak)
              [ 02_sales_performance.json ]               ← neighbor community
 QUERY "best selling products" → lands on the god node → returns the whole cluster (top10 + list).
```
No one says "these files are related" — they *become* related because the same entity (`best-selling products`) was extracted from both. The concept node is the join — the thing Claude faked with a dozen filename `find`s.

---

## Part VII — Roadmap

**Ordering principle:** cheapest, highest-leverage, smallest blast radius first.

1. **(B2) Normalized/structured content for binaries.** Biggest token sink; extends the existing transcription seam; no graph needed. Produce clean table/text per binary at ingest, store beside `fs_data`, and steer the source-of-truth read to it via the now-working `CLAUDE.md`/`AGENTS.md` hint (*"read the normalized sibling, never the raw binary"*).
2. **(B5) Cap `semfs grep` output** (top-k + truncation, à la `--budget`). One change, helps every run (cuts the `cache_read` 95%).
3. **(B3) Community-digest `profile.md`.** Build a graph + Leiden at ingest; back `profile.md` with the god-node summary. Fixes orientation.
4. **(B4) Typed-edge graph layer.** The genuinely new "knowledge graph" capability — add an edge table to the cache, extract edges local-AST-first (code) + LLM (docs) with confidence tags, expose `get_neighbors`/`shortest_path` as **MCP tools**.
5. **(cross-cutting) Local-first ingestion** (graphify's split) to keep the build cheap as workspaces scale, and **MCP** as the delivery channel so the agent reliably reaches every semantic/graph op.

**The end state:** the same five POSIX verbs (`ls find grep cat stat`) answered by *meaning + relations* — `ls`→ranked, `find`→by-meaning, `grep`→passages, `cat`→clean chunk, plus the graph-only ops (`neighbors`, `summarize`) POSIX can't express — turning the 31-command, 600k-token crawl into a 3-command, 35k-token lookup for *any* agent.

---

### Companion visual docs (this folder)
`semfs_worked_example.html` (case-289 plain-vs-semantic + node/edge determination + token ledger) · `graphify_explained.html` (6 steps × efficiency) · `semfs_questions.html` (commands-as-questions + data-to-precompute) · `semfs_execution_comparison.html` (4-way traces) · `semfs_ai_native_commands.html` · `semfs_posix_filemgmt.html` · `claude_token_accounting.html`. RCAs in `rcas/`.
