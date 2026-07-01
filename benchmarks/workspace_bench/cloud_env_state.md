# Cloud / Test Environment State

Snapshot of the EC2 benchmark box + Supermemory cloud as of **2026-06-05**. Pairs with
`SEMFS_BENCHMARK_RUNBOOK.md` (seeding/dataset) and `SEMFS_TESTING_RUNBOOK.md` (how to run targets).
This file = *what currently exists and where*; the runbooks = *how to operate it*.

> Verify before trusting: this is a point-in-time snapshot. Re-run the probe at the bottom to refresh.

## The box

| | |
|---|---|
| Host | `ubuntu@13.201.35.159` (key `~/.ssh/semfs-benchmark`) |
| Instance | `i-0c491c7cc23de8555` — ap-south-1, `m7i.xlarge` (4 vCPU / 15 GiB RAM, **no swap**) |
| OS | Linux 6.17 aws x86_64 |
| Disk | `/srv` 193 G, ~47 G free (76% used) — **watch this**, seeds + workdirs are large |
| Source | `/srv/semfs-benchmark/semantic-filesystem` (Rust workspace) |
| WB harness | `/srv/semfs-benchmark/Workspace-Bench/evaluation` |
| FUSE | `fuse3`/`fusermount3` (Linux) — not macFUSE |

⚠️ The box is **shared / single-instance**; only 15 GiB RAM with no swap. The cross-encoder rerank was
OOM-killed once at batch≈45 × seq-1024 (now batch-capped). Don't reboot without explicit user OK.

## The binary

| | |
|---|---|
| Path | `/home/ubuntu/.local/bin/semfs` (`semfs 0.0.5`) |
| Current md5 | **`2cb5d6a5…`** (rebuilt 2026-06-05 from `feat/backend-agnostic-store`; was `4a529bc8…`) |
| Includes | multilingual-e5 text embedder; **RRF max/best-rank fix**; **offline decoupling** (org-free `~/.semfs` cache + `local_only` gates all cloud calls); **`node_modules`/`package-lock.json` corpus skip` |
| **NEW (2026-06-05 rebuild)** | **search timeout 25s→50s** (`daemon/ipc.rs SEARCH_TIMEOUT`) + **client give-up 30s→60s** (`daemon/client.rs`) — headroom for post-mount lock contention (see ticket `search-throughput-readpath-isolation`); **`HashEmbedder`/`StubEmbedder` removal + `StorageChoice::Cloud` IS now in this binary** (the prior ⚠️ no longer applies). |

Rebuild: `ssh … 'source ~/.cargo/env && cd /srv/semfs-benchmark/semantic-filesystem && cargo build --release -p semfs && cp target/release/semfs ~/.local/bin/semfs'` (the daemon must be unmounted first — text-busy otherwise).

## Cache layout — **changed** (org-free `~/.semfs/<tag>.db`)

The decoupling moved the local cache from `<xdg>/semfs/<org_id>/<tag>.db` → **`~/.semfs/<tag>.db`**
(no org, no `XDG_CACHE_HOME`). The current binary only looks at `~/.semfs/`.

**Live (new layout) — `~/.semfs/`:**
| db | size | what |
|---|---|---|
| `chanpin-e5-nosum.db` | 666 MB | **config #8** — e5, per-sheet extraction, no summary. The working seed (dashboard RRF #7 / cross-encoder #6). |
| `nmtest-corpus.db` | 1.7 MB | **leftover** from the `node_modules` corpus test — safe to delete. |

**Legacy (old org-scoped layout, `…/<tag>/CjeeM2…/`) — orphaned, NOT seen by the current binary:**
`e5-test/…/chanpin-e5.db` (e5+flattened) · `e5nosum-test/…/chanpin-e5-nosum.db` (pre-relocation copy) ·
`e5sum-test/…/chanpin-e5-sum.db` (descriptive summaries) · `e5sum2-test/…/chanpin-e5-sum2.db`
(coverage summaries, embed-only) · `extract-test/…/chanpin-extract-test.db` (arctic) ·
`warm/cache_{pglite,sqlite}` + `cache-cloud/…/workspace-bench-chanpin.db`.
→ To reuse any legacy seed with the current binary, **`mv` its `.db` to `~/.semfs/<tag>.db`** (that's
how `chanpin-e5-nosum` was relocated). One-time per seed.

## Supermemory cloud

| | |
|---|---|
| Org / plan | **Saral / pro** (`whoami`: `marmik.pepper@gmail.com`) |
| Live container | **`workspace-bench-chanpin`** — seeded, queryable; cloud `/v4/search` returns whole-doc results incl. the dashboard near the top. **No re-seed needed.** |
| Key source | resolved from **`~/.config/semfs/credentials.json`** (global creds) — every mount/`whoami` uses it automatically; **not** in `~/.semfs_seed_env`. |

## Keys / secrets

| Key | Where | Notes |
|---|---|---|
| `OPENROUTER_API_KEY` | `~/.semfs_seed_env` (sourced) | codex auth + summaries + L7 graph + query-rewrite |
| `SUPERMEMORY_API_KEY` | `~/.config/semfs/credentials.json` (creds; **not** in seed_env) | mount/cloud; the WB harness *also* requires it as an **env var** — feed a placeholder (mount auths from creds) or export the real one |

Never print these. Source `~/.semfs_seed_env`; let creds resolve the SM key.

## Running state
- **No daemons currently mounted** (idle).
- **Verified 2026-06-05 (new binary):** offline mount of `chanpin-e5-nosum` + the 3 exact case-289
  queries returned 10 hits each, **no timeouts**. On a *fresh* mount (`QUEUE=621`, background work
  active) the searches ran 17–26s — i.e. the real post-mount contention — and **q1 at 25.8s completed
  only because of the 25s→50s bump** (it would have hit the old 25s ceiling). Confirms both the
  contention bug and that the headroom mitigation is working. Retrieval ranking unchanged (answer file
  ~#6, not top-3 — the known reranker gap, separate issue).
- Mount the working seed offline (no key, no cloud):
  `env -u XDG_CACHE_HOME ~/.local/bin/semfs mount chanpin-e5-nosum --path <dir> --no-push --no-sync --foreground --no-inject-hint` → opens `~/.semfs/chanpin-e5-nosum.db`, zero network calls.

## Routing: local vs cloud search
- **Local sqlite** (config #8): a `--no-push --no-sync` mount with the seed at `~/.semfs/<tag>.db`. `grep` searches locally.
- **Cloud `/v4/search`** (the "Supermemory" baseline): ⚠️ **`SEMFS_EMBED_BACKEND=hash` NO LONGER WORKS** — the current binary (2026-06-05 rebuild) **errors** on it (`resolve.rs:115`: "SEMFS_EMBED_BACKEND=hash was removed"). Route the cloud baseline via the first-class **`SEMFS_STORAGE_BACKEND=cloud`** against `workspace-bench-chanpin` instead. ✅ **Verified working 2026-06-05** — `SEMFS_STORAGE_BACKEND=cloud semfs grep "…" --tag workspace-bench-chanpin` returned 10 results in 1.8s with the answer file `best_selling_product_core_data_list.txt` at **#1**. Container is alive + seeded; no re-seed needed.

## WB harness (semfs-codex E2E) — gotchas
- Targets: `codex | semfs-codex | claudecode | semfs-claudecode` (`run_workspace_bench.sh`). The old cloud "SMFS" target is gone.
- `DATASET=smoke` = the single pinned case **289** (`"best-selling product"`; answer file = the product-sales dashboard `.xlsx`).
- **Env gaps the harness needs** (daemon-side cloud blockers are now fixed in code):
  1. **`SEMFS_BIN=/home/ubuntu/.local/bin/semfs`** — harness `_semfs_bin()` searches `SEMFS_BIN`/PATH; `nohup` shells lack `~/.local/bin` on PATH.
  2. **Clear stale case output** — `rm -rf output/SEMFSCodex--GPT-5.4--Smoke-SEMFS/289` before each run, or the grader's stale-resume reuses the old `agent.json` (do **not** use `SEMFS_FRESH=1` — it wipes the cache).
  3. **`SEMFS_NO_PUSH=1 SEMFS_NO_SYNC=1`** for a local-only run (→ `local_only` → no cloud calls); leave sync on for a cloud run.
- **Prepare is ~6 min** (was; now lighter): `make_filesys` copies the workspace per run. `node_modules` is now excluded (workdir 2,128→1,368 files). `SKIP_PREPARE=1` reuses the workdir.
- Traces: `output/<Agent>--…/289/agent.json` records **every tool call + full output** (`executionTrace`) + raw codex stdout.

## Latest measured results (case 289, codex GPT-5.4)
| Config | Tokens | Tool events | `semfs grep` | Timeouts | Answer rank | Agent durationMs | Notes |
|---|---:|---:|---:|---:|---|---:|---|
| plain codex | 143,837 | — | 0 | 0 | (FS explore) | 47.5s | baseline |
| semfs+sqlite, **no RRF fix** | 186,884 | ~ | ~ | — | — | — | +30% worse than plain |
| semfs+sqlite, RRF fix, **25s timeout** | 145,696 | 24 | 3 (2 timed out) | yes | ~#6 | 92.4s | ≈ parity; the original config #8 |
| **semfs+sqlite e5, RRF fix, 50s timeout (H1 fix)** | **82,653** | 19 | 2 | **0** | ~#6 | 78.2s | −43% vs 25s; 2026-06-05 |
| **semfs+sqlite Gemma-300M fp32, 50s** | **87,216** | 18 | 3 | 0 | #2 (cloud-q) / #10 (local-q) | 80.3s | 2026-06-05; NO win vs e5 (see below) |
| **semfs+pglite e5** | **89,928** | 17 | 1 | 0 | ≈ sqlite-e5 | 62.5s | 2026-06-06; **backend parity** — pglite ≈ sqlite-e5, storage is not a lever |
| **semfs+Supermemory (cloud)** | **18,144** | **4** | **1** | **0** | **#1** | 168.4s | −87% vs plain; recaptured 2026-06-05 |

**Embedder finding (2026-06-05) — Gemma fixed recall but NOT agent tokens.** Gemma-300M moved the answer
from e5's #405/592 (pure-vector, unreachable) to #3–7 (retrievable) — but the **E2E is 87.2K ≈ e5's
82.7K, no win**. Why: (1) through the full pipeline the answer ranks **#2 (cloud-phrased query) / #10
(local-phrased)**, not #1, so codex doesn't trust one hit and **still brute-forces** (os.walk + chases
the 403-HTML `top10.xlsx`, ~5 dead-end calls); (2) the first `grep` dumps **~120 KB** of whole-docs that
re-replays in context. So the embedder was **necessary but not sufficient**; the binding levers are now
**ranking (RRF/lane fusion → get to #1), whole-doc payload cap, and the 403-xlsx ingestion bug** — none
of which the embedder touches. Root-cause analysis: `tickets/explore-agent-search-behavior/` +
`tickets/embedder-config-search/PLAN.md`. Gemma embed cost: ~82 min for 5,777 chunks (fp32, CPU) vs
e5 ~12 min — see `tickets/embedder-config-search` for quantized (fp16/int8) speed work.

### Seeds on the instance (KEEP INTACT)
| `~/.semfs/<tag>.db` | embedder | dims | chunks | note |
|---|---|---|---:|---|
| `chanpin-e5-nosum` | e5-small | 384 | 5,777 | config #8 working seed |
| `chanpin-gemma` | EmbeddingGemma-300M fp32 | 768 | 5,670 | 2026-06-05 |
| `workspace-bench-chanpin` | (cloud-run local cache) | — | — | from cloud E2E |
New configs MUST seed into NEW tags (`chanpin-gemma-fp16`, `chanpin-qwen3-int8`, …) — never overwrite these.

**Evaluation (2026-06-05):** H1 (timeout) is fixed in both local-50s and cloud → 0 timeouts in both. The
remaining **local↔cloud token gap (82.7K vs 18.1K, ~4.5×) is now almost entirely RETRIEVAL QUALITY**,
not timeouts: cloud ranks the answer **#1** (→ 1 search, whole-doc return, write, done in 2 calls),
while local ranks it **~#6** (→ more searches + verification + still chases the corrupt `top10…xlsx`
403-HTML). Caveats: (a) `durationMs` for semfs targets includes the mount — cloud's 168s is dominated
by the slow cloud-container FUSE mount, not search (~2s); plain has no mount; (b) `cache_read=0` in ALL
runs — prompt caching is universally untapped. **Local's edge = latency + offline/privacy; cloud's edge
= tokens/quality.**

## 2026-06-06 SYNTHESIS — the local↔cloud token gap is CODEX EXPLORATION COUNT, not retrieval/return tuning

Deep investigation (RCA: `rcas/2026-06-06-cross-lingual-recall-miss-case289.md`). Two findings, both proven:

**1. RCA — cross-lingual L1 recall miss (FIXED for correctness):** case-289 answer file is 100% Chinese
(`成交金额`,`转化率`), query 100% English → BM25 lane dead + e5 EN→ZH dense match too weak. Full-corpus
e5 vec rank of the answer: **English query #417, Chinese query #1** (of 592 files). Shipped fix = L4
translate-rewrite (`SEMFS_REWRITE=1`; the `rewrite_query` prompt now appends target-language terms) →
answer goes absent→**rerank #1 / final top-3** (RANKDUMP-verified). codex even adopts ZH queries itself.

**2. But fixing retrieval did NOT cut tokens.** E2E tool-event counts vs tokens (case 289, codex GPT-5.4):

| run | tokens | tool events | note |
|---|---:|---:|---|
| cloud | **18,144** | **4** | translates query + compact chunk returns; codex stops fast |
| h1smoke (e5, no rewrite) | 82,653 | 19 | local best — search FAILS (x-lingual); passes via lucky `ls model_output/` fallback |
| e5 + rewrite | 114,301 | 19 | retrieval now SUCCEEDS but codex still explores 19× + bigger ZH-doc payloads |
| e5 + rewrite + cap 6KB | 129,176 | 17 | cap → codex `os.walk`s the tree (62KB) |
| e5 + rewrite + limit 4 | 135,670 | — | 4 docs but each ~75KB (large ZH files) |
| e5 + rewrite + **snippet** | 133,428 | 17 | grep payload shrank to ~27KB but codex `os.walk`s (62KB) anyway |

**Conclusion:** every LOCAL config makes **17–19 tool calls**; cloud makes **4**. Tokens ≈ calls × payload.
The rewrite fixes RANK but not call-count. All payload knobs (cap/limit/snippet) BACKFIRE: starving grep
makes codex brute-`os.walk` the mounted corpus (62KB) which re-replays across turns. **The lever is codex's
exploration behavior on the local mount (why 19 calls vs cloud's 4), NOT embedder/backend/fusion/return-
format.** The embedder×backend permutation matrix is therefore moot for tokens — all share the same
codex-exploration regime. Best local stays 82,653 (−43% vs plain 143,837); cloud 18,144 (−87%).

**Open lever (needs design discussion, not a knob):** make codex stop early like on cloud — why does the
cloud mount yield 4 calls? Candidates: cloud presents fewer browsable files (less to `os.walk`), or a more
"authoritative" single hit. A mount-hint that discourages tree-walking is risky (the grep-header variant
`hdrE2E` = 133,756 — hints can backfire). KEEP `SEMFS_REWRITE` (correctness win: local search now actually
finds the answer instead of relying on the predictable-output-path `ls` luck). New shipped knobs:
`SEMFS_REWRITE`, `SEMFS_RETURN_MODE=snippet`.

## Known leftovers / cleanup
- `~/.semfs/nmtest-corpus.db` — delete (node_modules-test artifact).
- Legacy org-scoped caches under `e5-test/`, `e5sum-test/`, `e5sum2-test/`, `extract-test/`,
  `e5nosum-test/` — orphaned by the cache-path change; keep for reference or reclaim ~3–4 GB.
- EC2 binary lags local source (no hash-removal) — rebuild when testing that work.

## Refresh this snapshot
```
ssh -i ~/.ssh/semfs-benchmark ubuntu@13.201.35.159 '
  md5sum ~/.local/bin/semfs; ls -la ~/.semfs/*.db;
  pgrep -af "semfs mount" | grep -v pgrep;
  ~/.local/bin/semfs whoami --json | grep -E "org|plan"'
```

## Related
- `SEMFS_BENCHMARK_RUNBOOK.md` · `SEMFS_TESTING_RUNBOOK.md` — operating the harness.
- `tickets/decouple-sqlite-cache-scoping-from-supermemory/`, `tickets/local-mount-residual-cloud-calls/`,
  `tickets/remove-hash-embedder/`, `tickets/exclude-node-modules-from-wb-workspace/` — the changes that
  shaped this state.
- `rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md` — the ranking arc + holistic config table.
