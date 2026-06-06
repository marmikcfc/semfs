# Search throughput: isolate the read path so searches don't block behind the indexer

- **Type:** Tech debt / performance (throughput + tail-latency)
- **Status:** OPEN — stopgap shipped (timeout 25s→50s), real fix not started
- **Created:** 2026-06-05
- **Priority:** deferred (latency/throughput is **not** an immediate priority — this ticket captures the
  proper fix + test plan for when it is)
- **Component:** `semfs-core` daemon search path (`cache/db.rs`, `backend/sqlite_vec.rs`, `daemon/ipc.rs`)
- **Branch context:** `feat/backend-agnostic-store`

## Background — what we already know (don't re-investigate)

From `tickets/explore-agent-search-behavior` + `rcas/2026-06-05-agent-search-token-blowup-turn-multiplication.md`:

- On Workspace-Bench case 289, semfs+sqlite took **24 tool calls / ~146K tokens** vs Supermemory's
  **1 call / ~36K**. Root cause: the agent's **first two `semfs grep` calls timed out** → fell back to
  an empty cloud result → the agent abandoned semantic search and brute-forced the filesystem (a 51 KB
  `os.walk` dump that, re-sent uncached every turn, was ~88% of the prompt tokens). When search finally
  worked (3rd call) it returned the answer at #1 and the agent finished in one step.
- **The timeout is the trigger.** Confirmed (trace + code + EC2 repro). Recall is fine — idle, the exact
  queries return 10 hits in ~5s.
- **Refined mechanism (the throughput bug this ticket fixes):** the daemon serializes **all** DB access
  — indexer writes AND search reads — through **one `Mutex<Connection>`**
  (`cache/db.rs:45`: `pub struct Db { conn: Mutex<Connection> }`). `journal_mode = WAL` is enabled
  (`db.rs:73`), which *would* allow a concurrent reader + writer at the SQLite level, **but the
  application-level mutex nullifies it.** During the post-mount indexing burst (local embed + L7 graph
  writes of freshly-mounted files; ~300–357% CPU for ~33 min in the observed run), a search embeds fine
  (model is eager-loaded at mount), then **blocks on `conn.lock()` (`sqlite_vec.rs:880`) behind the
  indexer's write transactions** past the IPC timeout → `SearchError "timed out"` → cloud fallback →
  empty → brute-force. By the time indexing drains, searches are instant again. This is why the *first*
  searches fail but later ones succeed.

### EC2 dose-response (2026-06-05, `m7i.xlarge`, warm chanpin, 12,335 chunks)
Direct-path grep latency vs synthetic CPU load (`stress-ng`): idle ~5s → 8 thr ~13s → 16 thr ~23s →
24 thr ~34s. CPU contention alone scales latency monotonically; the lock contention is the additional
factor that makes the *first* searches specifically blow the bound.

## Stopgap already shipped (2026-06-05) — NOT the fix

Raised the search timeout to buy headroom so a search blocked on the lock has more time to acquire it:
- `daemon/ipc.rs` `SEARCH_TIMEOUT` **25s → 50s**
- `daemon/client.rs` client response give-up **30s → 60s** (must stay > server bound)
- `backend/sqlite_vec.rs` `SEARCH_DEADLINE` left at 20s (cooperative degrade still returns best-effort
  RRF hits; invariant `20 < 50 < 60` holds)

This only widens the window; under a long-enough indexing burst a search can still exceed 50s. The
real fix is read-path isolation below.

## The fix — read-path isolation (proposals, pick during design)

The invariant we want: **a search read never waits on an indexer write.** Options, roughly in order of
preference:

1. **Dedicated read-only connection(s) for search.** Open one (or a small pool of) additional
   `Connection`(s) in read-only mode against the same WAL DB; route `SemanticIndex::search` through it
   instead of the shared write mutex. WAL already guarantees readers see a consistent snapshot while a
   writer commits. This is the minimal, highest-leverage change.
   - Care: the sqlite-vec (`vec0`) and FTS5 extensions must be loaded on the read connection too (the
     `sqlite3_auto_extension` hook in `db.rs:12` already installs on every connection opened after it —
     verify it covers the new conn).
   - Care: embedder identity guard / stamp reads currently go through `Db`; keep those on the write conn
     or make them read-conn-safe.
2. **Read-connection pool** (e.g. 2–4) if a single read conn becomes the bottleneck under concurrent
   agents. Bounded so we don't reintroduce CPU oversubscription.
3. **Make the blocking search cancellable.** Today the search runs in `spawn_blocking`, which Tokio
   cannot abort — a timed-out search keeps burning CPU after the client gives up
   (`sqlite_vec.rs` comment at the search wrapper). Add a cooperative cancel token checked at stage
   boundaries so an abandoned search stops promptly. (Complements, doesn't replace, #1.)
4. **Throttle / pause background L7 + indexing while a foreground search is in flight.** Secondary; #1
   should make it unnecessary, but it bounds CPU contention (the latency component).
5. **Do NOT pursue "pre-warm the embedder"** — it's already eager-loaded at mount
   (`daemon_runtime.rs:48` → `embed/local.rs:69 TextEmbedding::try_new`); the model is warm before the
   first search. This is a documented dead end.

## Observability to add (needed to test + monitor)

Per-search structured timing (the run that started this had *no* per-search timing, so we inferred):
- `embed_ms`, `lock_wait_ms` (time blocked on `conn.lock()`), `retrieval_ms`, `rerank_ms`, `total_ms`
- daemon background state at search time: `indexer_active` (bool), `push_queue_len`, current CPU%
- a WARN when `lock_wait_ms` exceeds a threshold (the smoking gun in production)

`lock_wait_ms` is the single metric that proves/disproves the contention mechanism in the wild.

## How to test it (the deliverable the throughput work must satisfy)

### 1. Unit / integration (fast, deterministic — gate on these)
- **Lock-contention regression test** (`backend/sqlite_vec.rs` tests): spawn a writer that holds a long
  write txn (or repeatedly indexes) on the shared DB; concurrently issue a `search`. **Assert the
  search returns hits and `lock_wait_ms` ≈ 0** (with the dedicated read conn) — it must NOT block on the
  writer. Without the fix this test should fail/timeout (write it RED first — see
  `superpowers:test-driven-development`).
- **Cancellation test:** start a search, drop the client / fire the timeout, assert the blocking task
  observes cancellation and stops within N ms (no CPU left spinning).
- **WAL snapshot correctness:** writer commits new chunks mid-search; assert the search returns a
  consistent snapshot (no torn reads, no `vec0`/FTS errors on the read conn).

### 2. Live daemon repro on EC2 (the empirical nail we deferred from the RCA)
Reproduce the *actual* trigger end-to-end:
- Host: `m7i.xlarge` 4 vCPU, `ssh -i ~/.ssh/semfs-benchmark ubuntu@13.201.35.159`, binary at
  `~/.local/bin/semfs` (login-shell PATH only — use `bash -lc` or full path).
- Mount chanpin (scoped `--memory-paths` to dodge the full-container mount OOM, **BUG #8** in
  `benchmarks/workspace_bench/EC2_TESTING_PROGRESS.md` — do NOT mount the whole container) so the daemon
  is up with the warm 12,335-chunk cache DB.
- Induce indexing/lock contention concurrently with the agent's searches (e.g. drop fresh files into a
  watched memory-path, or run the per-case auto-import that triggers re-index — see the per-case
  re-mount thrash tickets), and/or `stress-ng --cpu 16`.
- Run the **exact case-289 codex queries** (kept verbatim below) through the daemon and record:
  timeout count, `lock_wait_ms`, total wall, and whether the answer file surfaces.
- **Pass condition:** 0 timeouts, every query returns the answer file in the top results, `lock_wait_ms`
  stays bounded even while indexing.

### 3. Benchmark (the outcome metric that actually matters)
Re-run Workspace-Bench **case 289** E2E (codex GPT-5.4), `benchmarks/aws/run_workspace_bench.sh
semfs-codex`, ×5:
- **Target:** tool calls ≤ 3 (down from 24), total tokens → ~36K range (Supermemory parity), 0
  `search timed out` warnings in the daemon log.
- Compare against the baseline artifacts in `tickets/explore-agent-search-behavior/artifacts/`.

### 4. Load / tail-latency test
`stress-ng` + a driver issuing concurrent searches while indexing runs; report p50/p95/p99 search
latency and timeout rate, before vs after the fix. Define an SLO (e.g. p99 search < 5s under indexing).

## Exact case-289 codex queries (verbatim, for reproduction)
1. `best-selling product data file top selling product title transaction amount conversion rate`
2. `best selling products store transactions conversion rate title`
3. `top10 product status table best selling conversion rate transaction amount`

Idle on the new (50s) binary all three return 10 hits in ~5s with the answer file
(`best_selling_product_core_data_list.txt`) in the top 3 (rank #1 for query 2). The throughput fix must
keep this true **while the indexer is writing.**

## Related
- `tickets/explore-agent-search-behavior/` — the investigation that surfaced this.
- `rcas/2026-06-05-agent-search-token-blowup-turn-multiplication.md` — full RCA (H1 confirmed + refined).
- `rcas/2026-06-04-semfs-codex-clean-seed-timeout-poor-local-search-recall.md` — prior (CPU-starvation framing).
- `rcas/2026-06-04-semfs-grep-hangs-post-search-under-load-no-token-savings.md` — the cooperative-deadline degrade.
- `benchmarks/workspace_bench/EC2_TESTING_PROGRESS.md` — EC2 recipe + BUG #8 (mount OOM) to avoid.
