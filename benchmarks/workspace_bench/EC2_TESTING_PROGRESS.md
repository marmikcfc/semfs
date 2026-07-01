# EC2 Testing Progress — Context Dump for Next Session

**Last updated:** 2026-06-01. **Purpose:** a single starting point for the next agent/session continuing semfs benchmark + local-backend testing on EC2. Read this first; it captures the live state, every experiment, every bug + fix, the experiment setup (how to run + verify), and the open threads. Companion docs cross-referenced at the end.

---

## 0. TL;DR — where we are right now

- **4-variant smoke matrix (cloud index): all PASS.** codex/claude × {plain, semfs} on case 289 all pass and use `semfs grep`. The local backend (fastembed) works end-to-end for both agents; cloud-embed (OpenRouter) works too.
- **Token comparisons are NOT yet trustworthy** — three confounds were found and two fixed:
  1. **HOME pollution** (Claude read its own session transcripts) — **FIXED** in `ClaudeCode.js` (HOME moved outside the workspace). Dropped claude-cloud 1.23M → 603K (−51%).
  2. **Partial index (~19% / 50% coverage)** — the local index is never fully built because of an OOM (below). Confounds local-vs-cloud token numbers.
  3. **Token-accounting parser** dropped cache fields — **FIXED** earlier (sum all 4 usage fields).
- **BLOCKER: local pre-warm OOMs the daemon at ~15.6 GB** mounting the *full* container (deterministic). This blocks building a complete local index, which blocks fair lite runs. **Root cause not yet pinned** (3 hypotheses eliminated; see §5). **This is the #1 thing to fix next.**
- **Box is currently IDLE** (no daemon, no runs). Local index persisted at **~50% (583/1,172 indexable text files)**.

**Immediate next step:** isolate the OOM — mount the full container with local indexing OFF vs ON (§5.4) to pin the bug to pull/import vs embed, then fix it (likely stream/bound memory in `sync/pull.rs` or the indexer).

---

## 1. The machine & access

- **EC2** `i-0c491c7cc23de8555` (`semfs-benchmark-host`), **`m7i.xlarge` (4 vCPU / 16 GB, NO GPU)**, `ap-south-1`, Ubuntu 24.04. Public IP `13.201.35.159`.
- SSH: `ssh -i ~/.ssh/semfs-benchmark ubuntu@13.201.35.159`. SG `sg-0aa6ffbdc8cbc852f` opens :22 to `0.0.0.0/0`.
- **`semfs` is on the LOGIN-shell PATH only** → always `ssh … 'bash -lc "…"'` OR use the full path `/home/ubuntu/.local/bin/semfs` (the latter is required inside `setsid bash -c "…"`, which is NOT a login shell — a real bug that silently fails the daemon).
- **NEVER put `( )` in a remote `bash -lc "echo === … ==="`** — parens break the remote shell (`syntax error near unexpected token '('`). Hit this ~20× this session. Use plain words.
- Long runs: launch detached with `setsid … > /tmp/x.log 2>&1 < /dev/null &` so SSH drops don't kill them.
- **Monitors drop on idle SSH** (exit 255). Always add `-o ServerAliveInterval=15 -o ServerAliveCountMax=40`. The detached run is unaffected; just relaunch the monitor.

---

## 2. Host layout (`/srv/semfs-benchmark/`)

```
semantic-filesystem/                 ← rsync'd Rust source (NOT a git repo on EC2; scp/rsync from laptop)
  crates/{semfs, semfs-core, e2e}    ← build target
  benchmarks/aws/run_workspace_bench.sh        ← entrypoint
  benchmarks/workspace_bench/semfs{codex,claudecode}.py  ← semfs adapters
  target/release/semfs               ← build output → installed to ~/.local/bin/semfs
Workspace-Bench/evaluation/
  baselines/ClaudeCode.js            ← Claude Agent SDK driver (HOME fix + CLAUDE.md delivery live here)
  src/agents/{codex,claudecode}.py   ← agent harnesses (token parser in claudecode.py)
  src/agent_runner.py                ← orchestrator + grader (returned_paths_exist)
  filesys/<persona>_workdir_<Agent>_<Model>/   ← the mounted workspace per run
  tasks_lite/289/                    ← PINNED to case 289 (smoke = first dir; we pinned it)
  output/<Label>/289/agent.json      ← results (status, metrics, tokens)
benchmark.env                        ← OPENROUTER_API_KEY, RELACE_API_KEY, SUPERMEMORY_API_KEY (NO OPENAI key)
cache-cloud/                         ← XDG_CACHE_HOME for cloud-embed runs (isolated sqlite index)
~/.cache/semfs/<org>/<tag>.db        ← LOCAL index + content cache (the persisted index)
~/.cache/semfs/logs/<tag>.log        ← per-container daemon log
~/.cache/semfs/startup/<tag>.json    ← live startup phase (read this to see mount progress)
```

Helper scripts left on EC2 (`/tmp/`): `dbcheck.py`, `pmcoverage.py`, `indexable.py`, `embedtok.py`, `filesizes.py`, `deepvol.py`/`whatread.py` (tool-output attribution). Re-`scp` from laptop `/tmp/` if missing.

---

## 3. Experiment setup — how to run + verify (the recipe)

### 3.1 Build & deploy the binary (local backends need the NEW binary)
The local-model code is on the **laptop** working tree (branch `feat/backend-agnostic-store`); EC2 had an old cloud-only binary. To deploy:
```bash
# from laptop repo root:
rsync -az -e "ssh -i ~/.ssh/semfs-benchmark" --exclude target --exclude .git --exclude .fastembed_cache --exclude '*.db' \
  crates Cargo.toml Cargo.lock rust-toolchain.toml ubuntu@13.201.35.159:/srv/semfs-benchmark/semantic-filesystem/
# on EC2 (login shell):
cd /srv/semfs-benchmark/semantic-filesystem && cargo build --release -p semfs   # ~2.5 min
cp target/release/semfs ~/.local/bin/semfs
strings ~/.local/bin/semfs | grep -ciE "fastembed|SnowflakeArctic|SEMFS_EMBED_BACKEND"   # >0 = local-capable
```
**Build prereq (BUG #1):** `cargo build` fails with `openssl-sys`/`EXIT=101` on a fresh box — the local build pulls OpenSSL. Fix: `sudo apt-get install -y libssl-dev pkg-config`. (Cloud-only build didn't need it.)

### 3.2 Run a smoke test (case 289, PM/chanpin)
```bash
ssh -i ~/.ssh/semfs-benchmark ubuntu@13.201.35.159 'bash -lc "
  cd /srv/semfs-benchmark
  rm -rf Workspace-Bench/evaluation/output/<LABEL>/289     # avoid stale-resume skip
  setsid env DATASET=smoke RUN_STAMP=<run> \
    SEMFS_CONTAINER_TAG=workspace-bench-chanpin SEMFS_NO_PUSH=1 \
    SEMFS_STARTUP_TIMEOUT_SEC=1800 SEMFS_MOUNT_TIMEOUT_SEC=2400 \
    [USE_CLAUDE_LONG_RUNNING_TOKEN=1] \
    [SEMFS_EMBED_BACKEND=openrouter SEMFS_RERANK_BACKEND=cohere XDG_CACHE_HOME=/srv/semfs-benchmark/cache-cloud] \
    /srv/semfs-benchmark/semantic-filesystem/benchmarks/aws/run_workspace_bench.sh <target> \
    > /tmp/<run>.log 2>&1 < /dev/null & echo launched"'
```
- `<target>` ∈ `codex | semfs-codex | claudecode | semfs-claudecode`.
- `<LABEL>` dirs: `Codex--GPT-5.4--Smoke` · `SEMFSCodex--GPT-5.4--Smoke-SEMFS` · `ClaudeCode--Claude-Sonnet-4.6--Smoke` · `SEMFSClaudeCode--Claude-Sonnet-4.6--Smoke-SEMFS`.
- **Auth:** codex → OpenRouter (in `benchmark.env`); claude → `USE_CLAUDE_LONG_RUNNING_TOKEN=1` (OAuth subscription).
- **Local vs cloud embed:** local = defaults (nothing). cloud = `SEMFS_EMBED_BACKEND=openrouter SEMFS_RERANK_BACKEND=cohere` + a **separate `XDG_CACHE_HOME`** so the two sqlite indexes don't collide (see §4 isolation).
- **Timeouts (BUG #2/#3):** the daemon's startup watchdog defaults to **30 s of no-progress** → local indexing's silent CPU-embed phase trips it. Pass `SEMFS_STARTUP_TIMEOUT_SEC` (adapter forwards `--startup-timeout`) AND a large `SEMFS_MOUNT_TIMEOUT_SEC` (the python subprocess timeout, must be ≥ startup).

### 3.3 Monitor (keepalive!)
```bash
ssh -i ~/.ssh/semfs-benchmark -o ServerAliveInterval=15 -o ServerAliveCountMax=40 ubuntu@13.201.35.159 'bash -lc "
  for i in \$(seq 1 120); do
    aj=/srv/semfs-benchmark/Workspace-Bench/evaluation/output/<LABEL>/289/agent.json
    [ -f \$aj ] && { echo DONE; grep -oE \"status...:...[a-z]+|totalTokens...:.[0-9]+\" \$aj; break; }
    pgrep -f run_workspace_bench.sh >/dev/null || { echo GONE; tail -20 /tmp/<run>.log; break; }
    echo tick \$i; sleep 25
  done"'
```

### 3.4 Verify the SIGNALS, not just status (the golden rule)
A green `status:passed` only proves the pipeline ran. To prove semfs actually worked + delivery + cost:
| Question | Proof (NOT a green check) |
|---|---|
| Did Claude READ the CLAUDE.md hint? | **`[SEMFS-ACK]` canary count** in the transcript (≥1). |
| Did the agent USE semfs? | `grep -roF "semfs grep" <dir>` count > 0; + R2 `rehydrated raw file` in daemon log. |
| Supermemory API calls? | the **dashboard** (the daemon log does NOT log `/v4/*` at INFO — only R2 fetches). |
| True token cost? | sum all 4 usage fields (`input + cache_creation + cache_read + output`); 95% is `cache_read`. |
| Local (not cloud fallback)? | `used_local` + 0 `/v4/search` on dashboard + populated `chunks`/`vchunks`. |
| Index coverage? | `python3 /tmp/indexable.py` → embedded / 1,172 indexable text files. |

---

## 4. Experiments run + results

### 4.1 The 4-variant cloud-index matrix (case 289) — pipeline validation
| Run | Agent | Embed | Status | `semfs grep` | `[SEMFS-ACK]` | tokens | notes |
|-----|-------|-------|--------|-------------:|-------------:|-------:|-------|
| 3 | codex | local (arctic-s/jina) | ✅ pass | 12 | — | 34,945 | first fully-local pass |
| 1 | claude | local | ✅ pass | 34 | 6 | 422,668 | |
| 4 | codex | cloud (OpenRouter+Cohere) | ✅ pass | 24 | — | 174,722 | isolated `cache-cloud` index |
| 2 | claude | cloud | ✅ pass | 37–39 | 6 | 1,229,994 → **603,506** after HOME fix | |
| — | plain codex | — | ✅ pass | 0 | — | 143,837 | baseline |
| — | plain claude | — | ✅ pass | 0 | — | 206,941 | baseline |

**Interpretation:** all configs work; semfs helps **codex** (−76%, 144k→35k: it trusts the grep excerpt and never reads files) but **NOT claude** (it grep'd *and* still read files → additive overhead). See §4.4.

### 4.2 Isolation (multi-config on one container) — WORKS
- Each mount's sqlite is keyed by container tag at `~/.cache/semfs/<org>/<tag>.db`. To run two embed backends against the **same seeded container** without collision, override **`XDG_CACHE_HOME`** per run (`cache_dir()` honors it) → two independent DBs. Verified: local index in `~/.cache/semfs`, cloud index in `/srv/semfs-benchmark/cache-cloud`, no collision, no embedder-identity-guard conflict.
- An **embedder-identity guard** (`sqlite_vec.rs:48`) *refuses* to open a writer under a different model than the one that stamped the index — so you cannot mix backends in one DB; separate tags or separate `XDG_CACHE_HOME` is mandatory.

### 4.3 Index reuse across agents — WORKS
codex built the chunks; claude's *different workdir* mount of the *same tag* reused the same DB and just continued the (partial) embed. So the index is a per-container asset shared across agents — **embed once per container, reuse**. (Caveat: it's never fully built — §5.)

### 4.4 Token deep-dive (claude semfs) — the central finding
- claude-cloud: **95% of tokens are `cache_read`** (1,170,780 / 1,229,994); output only 6,659.
- **In an agent loop, cost ≈ (bytes read into context) × (turns they survive).** Reads replay every turn. So the expensive op is **reading, not searching**. `semfs grep` output was terse (~570 tok); the blowup was **file Reads**.
- **codex won by NOT reading files** (554 chars total tool output, 0 raw reads — the grep excerpt was the answer). **claude lost by reading files on top of grep.**
- → semfs grep is NOT the wrong idea; it only pays off if it **replaces** reading. Fix direction: serve normalized/transcription content on the FS read path so a Read is cheap, and/or get the agent to trust excerpts.
- Full RCA + interactive report: `rcas/2026-05-29-semfs-grep-claude-token-blowup.md`, `semfs_token_blowup_rca.html`.

### 4.5 Embedding token cost (for cloud-embed budgeting)
- chanpin full corpus ≈ **3–5M embedding tokens** (`SUM(LENGTH(text))/~3` over `chunks`; ~9.7M chars at 50% coverage). Overlap (`overlap_words:30`) inflates ~1.5×. Cloud embed (`OpenAiEmbedder`) does NOT capture usage → read from OpenRouter dashboard.

---

## 5. Bugs faced & fixes (chronological, with status)

| # | Bug | Symptom | Root cause | Fix | Status |
|---|-----|---------|-----------|-----|--------|
| 1 | **openssl build fail** | `cargo build` EXIT=101, `openssl-sys` not found | local build pulls OpenSSL (model-download HTTP) | `apt-get install libssl-dev pkg-config` | ✅ |
| 2 | **30s startup watchdog kills mount** | `semfs mount failed`, daemon stuck in `initial_sync` | `DEFAULT_STARTUP_INACTIVITY_TIMEOUT_SECS=30`; local embed is a long silent phase | adapter now forwards `--startup-timeout` from `SEMFS_STARTUP_TIMEOUT_SEC` (both `semfscodex.py`/`semfsclaudecode.py`) | ✅ |
| 3 | **subprocess timeout** | mount killed at ~33min mid-embed | python `SEMFS_MOUNT_TIMEOUT_SEC` < embed time | raise to 6000s; embed RESUMES from persisted chunks on remount (verified 2269→…) | ✅ (workaround) |
| 4 | **Claude token blowup (1.23M)** | semfs claude 6× plain | `ClaudeCode.js` set `HOME=task.cwd` → Claude's `~/.claude/projects/*.jsonl` transcripts written INTO the searchable workspace → Claude `Read` its own transcripts (47K+36K chars) → replay ×22 turns | `ClaudeCode.js`: HOME = sibling dir OUTSIDE workspace (`<filesys>/.cchome_<workdir>`). 1.23M→603K | ✅ |
| 5 | **token parser undercount** | claude reported 5,010 vs true 257k | `_parse_usage_from_stdout` summed only input+output | sum all 4 fields incl cache_read/creation | ✅ (earlier session) |
| 6 | **CLAUDE.md hint never delivered** | claude never ran `semfs grep` (runs 4–10) | `appendSystemPrompt` is NOT a real SDK option (silently dropped); SDK loads CLAUDE.md only with `settingSources:['project']` | write project `<cwd>/CLAUDE.md` + `settingSources:['project']` + `systemPrompt:{preset:'claude_code'}`. Verified via `[SEMFS-ACK]` canary | ✅ |
| 7 | **partial index / mount starvation** | local index only ~19% then ~50% | indexing is **flush-triggered (on file access), not eager**; benchmark mounts→runs→unmounts before embed finishes; binaries skipped; 749 files are genuinely 0-byte | pre-warm: mount + read all files. BUT this hits BUG #8 | ⚠️ open |
| 8 | **PRE-WARM OOM (15.6 GB)** | daemon OOM-killed mounting the FULL container | **NOT pinned.** Eliminated: not L7 (OOMs with `OPENROUTER_API_KEY` unset), not embedding (chunks flat during balloon), not big files (max 100MB), not the bounded hydration worker (semaphore=4 already), pull IS paged. **OOM is at MOUNT/`initial_sync`, deterministic ~15,634,xxx kB ≈ 25× the 621MB corpus.** Correlation: benchmark mounts use `--memory-paths` (scoped, 3 files) and NEVER OOM; pre-warm mounts the whole container and ALWAYS does. | **UNRESOLVED — #1 next task** | ❌ |

---

## 6. The OPEN BLOCKER (Bug #8) — full pre-warm OOM

**Why it matters:** a complete local index is the prerequisite for (a) fair local-vs-cloud token comparison and (b) running workbench-lite (each lite case needs *its* files embedded; at 50% coverage recall is a coin-flip). Pre-warm = mount the whole container and let it embed to 100% — but that OOMs.

**What we know (validated):**
- OOM happens at **mount / `initial_sync`** (before any read-all), deterministic ~15.6 GB.
- L7 OFF still OOMs → not the entity-graph LLM path.
- `embedded` count stays flat while RAM balloons → not the embedder.
- Largest file 100 MB, total 621 MB → not big-file buffering.
- Background hydration worker is already bounded (semaphore = `HYDRATION_CONCURRENCY=4`).
- `sync/pull.rs` pages the doc list (~98/page, dropped per page) — BUT `list_page` uses `include_content: true` (full content per page).
- **Scoped mounts (`--memory-paths`) never OOM; full-container mounts always do.**

**Hypotheses still live:**
- **H-pull/import:** processing ALL 983 docs / 2,128 files in `initial_sync` accumulates content/inode/transcription buffers that aren't freed per-page (`include_content:true` + import + transcription-sibling creation). 983 docs × ~16MB raw ≈ 15.7GB ≈ exact match.
- **H-indexer:** the local indexer (`SqliteVecStore` + ONNX) accumulates across the whole-container index pass (ort memory-arena growth, or a retained buffer).

**The DECISIVE next experiment (do this first, ~5 min):**
Mount the full container with **local indexing OFF vs ON** to split pull/import from embed:
```bash
# OFF: force the hash embedder (no real index) or a config that disables local indexing,
#      OR mount the OLD cloud-only binary. If it STILL OOMs → bug is in pull/import (shared).
# ON: the local binary as-is. If only this OOMs → bug is in the embed/index path.
```
Then instrument with RSS-vs-progress sampling (read `~/.cache/semfs/startup/<tag>.json` `loaded`/`total` vs `free -m`) to see the growth shape. **Likely fix:** stream/bound memory in `initial_sync`/import (don't hold all docs), or cap the indexer's working set. Consider `include_content: false` + lazy content fetch.

**Workarounds if the fix is deferred:**
- Pre-warm on a **bigger box** (resize to 64GB for the one-time warm; the persisted index moves back).
- Batched sequential reads with daemon restarts (frees memory between batches) — fragile, we saw it partially work (50%).

---

## 7. Parallelization / speed (separate from the OOM)

- Embedding is **already batched + multi-threaded**: `index()` embeds a file's chunks in one `embedder.embed(&chunks)` call; fastembed/ONNX uses ~3 of 4 cores (measured 300% CPU). Indexing is concurrent (JoinSet) but serializes on `Mutex<TextEmbedding>`.
- **On this 4-vCPU box, a worker-pool buys ~1.3× at best** (cores already saturated). Real speedup needs MORE CORES: an embedder pool sized to `available_parallelism()` gives ~4× on 16 vCPU, GPU gives 10–100×.
- **Conclusion:** parallelize the embed ONLY makes sense paired with a bigger box. For *completeness* you don't need parallelism — you need the OOM fixed + an uninterrupted embed. (Don't conflate the two.)
- **Delete-index-on-unmount** already exists (ephemeral in-memory cache mode) — but it's WRONG for benchmarking (forces re-embed every mount). Keep persist + pre-warm-once-reuse.

---

## 8. Model config (MODELS.md is authoritative)

- **Defaults (local, fastembed registry, compile-time consts in `resolve.rs`):** text = `Snowflake/snowflake-arctic-embed-s` (384d, `vchunks`), code = `jinaai/jina-embeddings-v2-base-code` (768d, `vchunks_code`), rerank = `jina-reranker-v2-base-multilingual` int8.
- **Backend family = env, no rebuild:** `SEMFS_EMBED_BACKEND=local|openai|openrouter|hash`, `SEMFS_RERANK_BACKEND=local|cohere|relace|none`. Specific model = compile-time const (rebuild to change). Arbitrary BYO-ONNX (`from_dir`) is NOT wired (deferred).
- **`benchmark.env` has OPENROUTER + RELACE + SUPERMEMORY keys, NO OPENAI key** → "cloud embed" = `SEMFS_EMBED_BACKEND=openrouter` (not `openai`).
- **Note:** default text embedder `arctic-s` is English-centric; the corpus has Chinese content (the reranker is multilingual, partly compensates). A multilingual text embedder (e.g. `paraphrase-multilingual-MiniLM-L12-v2`, mean-pool, 384d) would improve CJK recall but needs a const change + rebuild.

---

## 9. The roadmap (priority order)

1. **FIX BUG #8 (pre-warm OOM)** — run the isolation experiment (§6), pin pull/import vs embed, fix the memory growth. Unblocks everything.
2. **Full pre-warm chanpin to 100%** (once #1 fixed) → complete persisted local index.
3. **Re-run the 4-variant matrix on the COMPLETE index + HOME fix** → first genuinely uncontaminated token numbers. Compare within-agent (codex local vs cloud; claude semfs vs plain).
4. **Workbench-lite** — pre-warm each persona container (chanpin done, then kaifa) per backend; run lite cases. Only meaningful after #2.
5. **Token-efficiency fixes for claude** (from §4.4): serve transcription on the FS read path (the highest-leverage semfs change), line-scoped reads, cap grep top-k.
6. **Parallel embed** (only if moving to a bigger box) + **decouple indexing from mount** (`semfs index <tag>` standalone job, or background-index-after-ready) — the proper fix for the flush-triggered/starvation issue.
7. Smaller: `codex.py` mislabels `cache_write ← reasoning_output_tokens` (fix telemetry); add cost-weighted token column (cache_read is ~0.1×, so raw totals overstate cost ~6×; claude-cloud ~216K cost-equiv vs codex ~175K — much closer than raw).

---

## 10. Companion docs (all in `benchmarks/workspace_bench/` unless noted)
- `SEMFS_TESTING_RUNBOOK.md` — run + verify procedures, failure modes, the canary methodology.
- `SEMFS_BENCHMARK_RUNBOOK.md` — dataset, personas, seeding, machine.
- `SEMFS_GRAPHIFY_DESIGN.md` — architecture, algorithms, experiment log, graphify borrow-map.
- `MODELS.md` (repo root) — embedding/rerank model selection (authoritative).
- HTML explainers: `semfs_token_blowup_rca.html`, `semfs_local_indexing_architecture.html`, `semfs_worked_example.html`, `graphify_explained.html`, `semfs_questions.html`, `semfs_execution_comparison.html`, `claude_token_accounting.html`, `semfs_ai_native_commands.html`, `semfs_posix_filemgmt.html`, `semfs_virtual_search_path.html`.
- RCAs (`rcas/`): `2026-05-30-semfs-local-prewarm-oom-unbounded-rehydration.md` (NOTE: this RCA's root-cause section is SUPERSEDED — the worker semaphore already exists; OOM is at mount/initial_sync, see §6), `2026-05-29-semfs-grep-claude-token-blowup.md`, `2026-05-27-semfs-claude-ignores-semantic-search.md`.

---

## 11. Quick-reference env vars

| Var | Purpose |
|-----|---------|
| `SEMFS_CONTAINER_TAG` | which seeded container (e.g. `workspace-bench-chanpin`) |
| `SEMFS_NO_PUSH=1` | read-only (agent writes discarded at unmount; no seed contamination) |
| `SEMFS_STARTUP_TIMEOUT_SEC` | → `--startup-timeout` (beat the 30s watchdog during local embed) |
| `SEMFS_MOUNT_TIMEOUT_SEC` | python subprocess timeout (≥ startup; raise for cold-start embeds) |
| `SEMFS_EMBED_BACKEND` | `local`(default)\|`openrouter`\|`openai`\|`hash` |
| `SEMFS_RERANK_BACKEND` | `local`(default)\|`cohere`\|`relace`\|`none` |
| `XDG_CACHE_HOME` | per-config cache root → isolate sqlite indexes (must be ubuntu-writable, e.g. `/srv/semfs-benchmark/cache-cloud`, NOT bare `/srv`) |
| `USE_CLAUDE_LONG_RUNNING_TOKEN=1` | claude via OAuth subscription |
| `SEMFS_FRESH=1` | (run_workspace_bench.sh) wipe cache DB + output before run — forces cold rebuild |
| `DATASET=smoke` | task_limit=1 (the single dir in `tasks_lite/`, pinned to 289) |
```
```
