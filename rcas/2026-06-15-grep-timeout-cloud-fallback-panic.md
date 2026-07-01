# RCA: local `semfs grep` timeout → cloud fallback → PANIC → agent retry-storm (token blowup)

**Date:** 2026-06-15 · **Severity:** high (inflates local-arm tokens, masks as "retrieval cost")
**Found via:** mining codex traces for the WB-PM 5-case re-run (15,44,45,53,55 × nokg).

## Symptom
On the cases where local `nokg` cost MORE tokens than `plain` (15, 55, 45), the agent
re-issued similar `semfs grep` queries repeatedly. Per-cell grep-reliability scan: cases
15 and 55 had **43–50% of greps return a ~346-byte error blob** instead of results; cases
53 and 44 (0% errors) were exactly where `nokg` *beat* plain. Perfect correlation:
errors → retries → token premium.

## What the agent actually received (verbatim, from `codex_stdout.jsonl`)
```
WARN daemon search failed (search timed out after 50s);
     falling back to cloud search (sqlite/pgvector degraded-dependency path)
thread 'main' panicked at crates/semfs-core/src/api/mod.rs:495:18:
request must be cloneable for retry
```

## Root cause (verified against code, two compounding defects)
1. **Local search blocks past the 50s daemon bound.** `search_blocking` (sqlite_vec.rs)
   acquires the shared `Mutex<Connection>` (`conn.lock()`); during the **post-mount
   indexing burst** the indexer holds it for write txns, so the search waits >50s. The
   cooperative 20s `SEARCH_DEADLINE` degrade (return RRF, skip rerank) can't help — the
   block is on acquiring the lock, before/around the deadline checks, and `spawn_blocking`
   can't be cancelled. The daemon's `SEARCH_TIMEOUT` (50s, `ipc.rs`) fires → returns a
   typed `SearchError` "search timed out after 50s". (This was already bumped 25s→50s on
   2026-06-05 for the same reason; 50s was still too tight under load.)
2. **The cloud-fallback path then PANICS.** On the daemon timeout error, the grep client
   falls back to a cloud search → `Api::send_with_retry` (api/mod.rs) did
   `builder.try_clone().expect("request must be cloneable for retry")`. `try_clone()`
   returns `None` for a non-cloneable (streaming) body → `.expect()` **panics** → the grep
   returns only the panic text (~346 B). The agent sees no results and retries with a new
   phrasing → same timeout+panic → retry-storm (3× on case 15, 3× on case 55).

So "local burns more tokens than plain" was **80% infra**, not retrieval: a lock-contention
timeout + a fallback panic, not bad ranking. (The reranker that makes nokg *accurate* is
unrelated to this token waste — the block is `conn.lock()`, not rerank.)

## Fix (shipped 2026-06-15)
- **A — no panic on fallback** (`api/mod.rs`): hold the original builder in an `Option`; if
  `try_clone()` is `None`, send ONCE (no retry) instead of `.expect()`; retry guards gated
  on `owned.is_some()`. Replaced the post-loop `unreachable!()` with a typed error.
- **B — raise the bounds so a slow-but-working search COMPLETES, env-overridable:**
  `SEARCH_TIMEOUT` 50→**120s** (`SEMFS_SEARCH_TIMEOUT_SECS`), client wait 60→**140s**
  (`SEMFS_GREP_CLIENT_WAIT_SECS`), `SEARCH_DEADLINE` 20→**90s** (`SEMFS_SEARCH_DEADLINE_SECS`).
  Env-overridable so future tuning needs no rebuild.
- Binary rebuilt on Modal (x86_64-linux, matches E2B), pushed into the sandbox at `boot_prep`.

## The real (deeper) fix — not done here
Lock contention is the true root: a search shouldn't wait on the indexer's write txns.
Ticket `search-throughput-readpath-isolation` (dedicated read connection) is the throughput
fix; the timeout bump is a HEADROOM mitigation (same lever as the 25→50→120 progression).
Also relevant: SEM-32 (rerank RAM spikes cap concurrency).

## Verification plan
Re-run codex `nokg` on 15,44,45,53,55 with the fixed binary; expect: zero "timed out
after"/panic blobs, grep error-rate → 0, nokg tokens on 15/55 drop toward plain, accuracy
held (RRF/rerank results returned instead of error). Kill condition: if errors persist,
the block isn't the 50s bound (re-check embed/rerank latency).
