# Bug/Tech-debt: local search fails CLOSED to empty under CPU load — should degrade to best-effort RRF hits

- **Type:** Bug + robustness (local search / sqlite backend)
- **Status:** Fix #1 IMPLEMENTED 2026-06-04 (fail-open + rerank cap, tested). #2-remainder / #3 / under-load re-run deferred — see Implementation status.
- **Created:** 2026-06-04
- **Component:** `semfs-core::backend::sqlite_vec::search_blocking` (the `SEARCH_DEADLINE`
  cancellation points); secondary: L5 rerank CPU cost + background L7 contention.
- **Branch context:** `feat/backend-agnostic-store`
- **Root cause / evidence:** `rcas/2026-06-04-semfs-codex-clean-seed-timeout-poor-local-search-recall.md`
  (see the CORRECTION section — authoritative).

---

## Problem

Under CPU contention the local search **bails to empty/error instead of returning the results it
already has** — so an agent sees "0 hits" for a query that actually matches, abandons semantic
search, and degrades to brute-force filesystem exploration.

`search_blocking` has a 25 s `SEARCH_DEADLINE` with cancellation points. The **pre-connection bail**
(sqlite_vec.rs:772) fires if the query-embed alone exceeds the deadline and returns
`Err`/empty **before any retrieval runs**:

```rust
if Instant::now() >= deadline {
    anyhow::bail!("sqlite search exceeded its {}s deadline before acquiring the connection; …");
}
```

When the daemon is CPU-starved, the **local query-embed is slow enough to blow 25 s**, so the search
returns nothing — even though, run on an idle daemon, the *same query returns 72–84 hits*.

Note the inconsistency vs the **rerank-stage** deadline path (sqlite_vec.rs:862), which correctly
*skips rerank but still returns RRF-ranked hits*. The two deadline points disagree: one degrades
gracefully, the other fails closed.

## Evidence (from the RCA)

- codex case-289 run: 4 of 6 `semfs grep`s returned **0 bytes**; daemon pegged **~300–357 % CPU for
  33 min**; agent fell back to 88 manual `ls`/`cat`/`os.walk` commands → **2000 s timeout, 0 tokens**.
- Instrumented repro of the **exact** queries on the **same seed**, idle daemon:
  `qvec_len=384 → vec_n=80, code_n=80, fts_n=80 → rrf_files=72 → final_hits=72` (and 84 for the
  Chinese query, **top hit = the answer file**). Recall/FTS/rerank are all fine.
- Conclusion: the empties were a **load-induced deadline bail to empty**, not a retrieval defect.

## Fix

1. **Never fail closed when candidates exist.** Restructure `search_blocking` so the deadline can
   only ever *reduce work*, never *zero the result*:
   - Pre-connection bail (L772): instead of `bail!`, proceed to retrieval if possible, or if the
     embed genuinely couldn't complete, return an explicit typed "degraded/timeout" signal the
     caller can distinguish from "0 matches" — never silent empty.
   - Make all cancellation points consistent with the rerank path (L862): skip the *expensive*
     stage, keep + return the cheaper-stage hits (RRF).
2. **Bound per-query CPU so one agent can't starve itself:**
   - Cap L5 rerank candidate count and ONNX intra-op threads (it was the ~350 % hog).
   - Cache/reuse the query embedding within a search.
   - Throttle/pause background **L7 entity-graph extraction** while foreground searches are in
     flight (the per-case mount's L7 queue running with OPENROUTER on is a prime contention source).
3. **(orthogonal) Raise the Workspace-Bench agent timeout** (2000 s was at the edge; a completing
   run was ~2430 s) so a slow-but-progressing agent isn't cut off.

## Implementation status (2026-06-04)

Landed in `backend::sqlite_vec::search_blocking` + a candidate cap.

- **Fix #1 — never fail closed (DONE, tested).** The pre-connection point
  (sqlite_vec.rs ~L778) no longer `bail!`s on the deadline. The query-embed has
  already completed (we hold `qvec`) and retrieval (vec/code/fts KNN) is cheap
  bounded SQLite, so past the deadline the search **proceeds and returns
  best-effort RRF hits**, only SKIPPING the expensive cross-encoder rerank (the
  stage the deadline really guards). The two deadline points now agree — both
  shed work, neither zeroes the result. `search_blocking` takes `deadline` as a
  param so it's forceable in a test. Test: `search_past_deadline_degrades_to_hits_not_empty`
  (proven RED→GREEN: with the old `bail!` the past-deadline search errors; now it
  returns the matching file).
- **Fix #2 — rerank candidate cap (DONE, tested).** `RERANK_CANDIDATES = 50`:
  only the top RRF candidates feed the cross-encoder (the ~350% hog), the tail
  keeps RRF order. Test: `rerank_candidate_count_is_capped` (60 matching files →
  reranker sees ≤ 50).
- **Fix #2 — deferred pieces:**
  - *ONNX intra-op thread cap* — the real CPU lever, but it lives in fastembed
    model construction (`rerank/local.rs` / `embed/local.rs`) and is a global
    tuning that affects indexing too; needs target testing. Separate change.
  - *Throttle background L7 during foreground search* — separate worker subsystem;
    belongs with `tickets/parallelize-l7/`.
  - *"Reuse the query embedding"* — N/A here: the text-lane and code-lane embeds
    are different model spaces (not redundant), and the cross-encoder doesn't use
    `qvec`. No redundant same-space embed exists to cache within one search.
- **Fix #3 (raise the bench agent timeout)** — orthogonal Workspace-Bench config,
  not core code. Deferred.
- **Acceptance #2 (codex case-289 under load completes)** — needs the EC2 clean-seed
  re-run; enabled by fix #1, not yet verified on host.

**Update 2026-06-04 — the ACTUAL cause of the "no token savings" run was downstream.**
A clean-seed re-run with the fail-open fix *completed* but still spent plain-codex-level
tokens: the searches succeeded (65–324 hits) but every `semfs grep` HUNG **after** the
search, in its post-search line-range formatting — a blocking FUSE `read_to_string` with
no timeout, starved by the same CPU-saturated daemon. Root-caused + fixed in
`rcas/2026-06-04-semfs-grep-hangs-post-search-under-load-no-token-savings.md`:
- grep's read is now time-bounded + circuit-broken (`cmd/grep.rs`, `read_file_timed`) →
  never hangs; falls back to `<file>:<chunk>`.
- `SEARCH_DEADLINE` 25s → **20s** (< `SEARCH_TIMEOUT` 25s) so the cooperative degrade wins
  the race (this ticket's fix #3, done here).
The search-level fail-open + rerank cap above remain correct and necessary, but were not
the active cause of that run; the grep-hang fix is what restores the token benefit.

## Acceptance criteria

- A search that matches content **never returns empty due to the deadline** — under simulated CPU
  load it returns at least the RRF/vector hits (degraded, not zero). Add a test that forces the
  deadline and asserts non-empty results when candidates exist.
- `semfs grep` of the case-289 product query returns the product files **even while the daemon is
  under load** (re-run the codex case-289 benchmark on the clean seed → completes, non-zero tokens).
- A single agent's search load does not peg the box for tens of minutes (rerank threads/candidates
  bounded; background L7 yields to foreground search).

## Why it matters

- **Benchmark validity:** this fail-closed behavior is exactly what made the semfs+sqlite codex run
  time out with 0 tokens (couldn't fill the metric). Fix #1 alone would likely have let that run
  complete.
- **Product:** an agent that gets "0 results" for a query that matches loses all trust in semfs
  search and reverts to slow brute-force exploration — negating the whole value prop.
- The retrieval is good; it's **fragile under load and fails closed** — the worst failure mode for a
  search tool.

## Related
- `rcas/2026-06-04-semfs-codex-clean-seed-timeout-poor-local-search-recall.md` (root cause + repro)
- The IPC search review rounds that introduced the cooperative deadline (`SEARCH_DEADLINE` /
  `SEARCH_TIMEOUT`) — this ticket tightens its *fail-open* behavior.
- `tickets/parallelize-l7/` (L7 worker — the background-contention half of fix #2).
