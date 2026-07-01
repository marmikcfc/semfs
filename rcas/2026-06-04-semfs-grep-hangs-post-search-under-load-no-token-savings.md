# RCA: `semfs grep` HANGS after a successful search under daemon load — agent gets no results → no token savings

- **Date:** 2026-06-04
- **Severity:** high — semfs delivers ~0 token benefit on the local backend because the agent's
  searches hang and return nothing, so it falls back to brute-force exploration (138,307 tokens ≈
  plain codex's 143,837 on case 289).
- **Component:** `semfs grep` CLI post-search path (`cmd/grep.rs`) + daemon resource contention
  (FUSE read / IPC response under a CPU-saturated daemon).
- **Status:** root-caused; **fix #1 + #3 IMPLEMENTED 2026-06-04** — grep's line-range read is
  now time-bounded with a circuit breaker (never hangs on the mount); `SEARCH_DEADLINE` dropped
  to 20s (< `SEARCH_TIMEOUT` 25s). Tested. Under-load EC2 repro still pending. Fix #2 (decouple
  FUSE serving from search CPU) deferred — see `tickets/search-deadline-fails-closed-to-empty/`.

> Supersedes the load-induced-*search*-timeout theory in
> `2026-06-04-semfs-codex-clean-seed-timeout-poor-local-search-recall.md`. That RCA's search-level
> fixes are correct but NOT the active cause here — see below.

## Symptom

semfs+sqlite codex run on the clean seed (fixed build): **status=passed, totalTokens=138,307** —
i.e. it completed but used **as many tokens as plain codex (143,837)**: no semantic-search benefit.
codex issued 7 `semfs grep`s and got nothing useful from them, then found the answer by manual
`ls`/`cat`/`os.walk`.

## What was DISPROVEN (with evidence)

The instrumented per-stage logging + the codex trace ruled out the obvious causes:

1. **Not recall / not empty search.** The daemon's `search pipeline counts` log for codex's exact
   queries: `vec_n=80, code_n=80, fts_n=80 → rrf_files=72 → final_hits=72` (and 65/90/103/**324**
   for the others). **Every search returned 65–324 hits.**
2. **Not scope.** One query logged `scope="/chanpin/"` and still returned **324 hits** — the path→VFS
   scope resolution works.
3. **Not the deadline / not a search timeout.** Zero `deadline`/`timed out`/`skipping rerank`
   markers in the daemon log. `reranked=false` only because **no reranker is attached** to this
   store, not a deadline skip. The searches completed cleanly and fast.

## Root cause (verified direction)

**The `semfs grep` processes HANG *after* the search succeeds.** In the codex event stream every
`semfs grep` command has an `item.started` but **NO `item.completed`** — they never returned (status
stuck `in_progress`, 0 output, no exit code). Meanwhile 8 *other* commands (ls/cat/mkdir) completed
normally. The daemon logged the hits; the grep never emitted them.

So the failure is **downstream of a successful search, in the grep CLI's post-search work**, and it
blocks **indefinitely** (the command never completes — not a clean 30s client timeout, which would
fall back). The two candidates, both of which block with NO timeout on a CPU-starved daemon:

- **(leading) FUSE line-range read.** To format each hit as `<file>:<line_start>-<line_end>:<chunk>`,
  grep reads the hit's file off the FUSE mount: `read_local_or_sidecar` → `std::fs::read_to_string`
  (`cmd/grep.rs:687`) — **a blocking FUSE read with no timeout**, run for the first hit. The semfs
  daemon serves BOTH the IPC search and the FUSE filesystem; under contention (the run pegged the
  daemon ~350% CPU, with background L7 + the search holding the `Mutex<Connection>`), a FUSE `read()`
  to the same daemon can block. grep then hangs forever in formatting.
- **(secondary) IPC response delivery.** The daemon logs hits inside `search_blocking`, then builds
  + writes `Response::SearchHits`. If the daemon's async runtime is starved by the `spawn_blocking`
  searches, writing the reply is delayed; the client (`client.rs:133`, 30s) would *time out* though —
  which yields a fallback, not the observed indefinite hang. So this alone doesn't explain it.

The indefinite hang (no completion ever) points at the **un-timed-out blocking FUSE read in grep's
formatting** as the primary culprit.

## Why this matters / why no token savings

codex's searches return hits, but the grep that would surface them to codex **never returns**, so
codex sees nothing from semfs and reverts to manual exploration — burning plain-codex-level tokens.
The whole local-backend value prop is negated not by bad retrieval but by a **post-search hang**.

## Proposed fixes

1. **grep formatting must not block on FUSE.** Bound `read_local_or_sidecar` with a timeout (or read
   via a non-FUSE path / the daemon's own content), and **fall back to printing `<file>:<chunk>`
   without the line range** when the read is slow — the result (filepath + chunk) is what the agent
   needs; the line range is a nicety. Never let output formatting hang on the mount it's searching.
2. **Decouple FUSE serving from search CPU.** The daemon must keep serving `read()` promptly while a
   search runs — bound the search's CPU (rerank threads, candidate cap — already partly done) and
   throttle background L7 so the FUSE path isn't starved (ties to
   `tickets/search-deadline-fails-closed-to-empty/` #2 and `tickets/parallelize-l7/`).
3. **Make `SEARCH_DEADLINE < SEARCH_TIMEOUT`** (currently both 25s) so the cooperative degrade always
   wins the race — defensive, even though the deadline wasn't the active cause here.

## Implemented (2026-06-04)

- **Fix #1 — grep formatting can't hang on FUSE (DONE, tested).** `cmd/grep.rs`:
  the line-range read is now `read_file_timed` (blocking `read_to_string` on a
  throwaway thread + `recv_timeout`, 2s budget) returning `Content | Missing |
  TimedOut`. A timeout short-circuits the sidecar fan-out, and a per-invocation
  **circuit breaker** (`mount_reads_ok`) trips on the first timeout so the whole
  grep pays at most one 2s budget, then prints `<file>:<chunk>` (no line range —
  the nicety) for every remaining hit. The chunk+filepath the agent needs always
  reaches it. Test: `read_file_timed_times_out_on_blocking_fifo` (a writer-less
  FIFO blocks `read_to_string` in `open()` forever → the bound fires) +
  `read_file_timed_content_and_missing`.
- **Fix #3 — `SEARCH_DEADLINE` 25s → 20s (DONE).** Now strictly under
  `daemon::ipc::SEARCH_TIMEOUT` (25s) so the in-search cooperative degrade wins
  the race against the daemon's hard timeout.
- **Fix #2 — decouple FUSE serving from search CPU — DEFERRED.** Bounding rerank
  (candidate cap done; ONNX intra-op threads pending) + throttling background L7
  so FUSE `read()` isn't starved. Lives with
  `tickets/search-deadline-fails-closed-to-empty/` #2 + `tickets/parallelize-l7/`.

`cargo test` green (semfs 45, semfs-core 266); clippy clean.

## Confirmation step (pending)

Reproduce under load: mount the seed, drive the daemon to high CPU (concurrent searches +/or L7),
and run one `semfs grep` from inside the mount — observe whether it hangs in `read_local_or_sidecar`
(a `strace`/stack of the grep PID would show a blocked FUSE `read`). The idle repro returns 381 bytes
(works), so the hang only manifests under contention.

## Related
- `rcas/2026-06-04-semfs-codex-clean-seed-timeout-poor-local-search-recall.md` (search-level fixes;
  not the active cause here)
- `tickets/search-deadline-fails-closed-to-empty/` (the CPU-contention half)
- `tickets/parallelize-l7/` (background-L7 contention)
