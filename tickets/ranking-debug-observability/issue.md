# Ranking-debug observability — make per-stage rank inspection reliable (`grep --explain`)

- **Type:** Tech debt / DX (observability)
- **Status:** OPEN
- **Created:** 2026-06-05
- **Component:** `semfs grep` client + `backend/sqlite_vec.rs` (`SEMFS_DEBUG_RANKING` dump)

## What prompted this

While root-causing why the answer file ranks #10 for the local query on the Gemma seed
(`tickets/embedder-upgrade-gemma-qwen3`, `tickets/explore-agent-search-behavior`), I needed each
candidate's **RRF rank vs rerank rank**. The mechanism exists — `SEMFS_DEBUG_RANKING=1` makes
`search_blocking` emit `RANKDUMP` lines (stage=RRF / stage=RERANK, rank, score, fp) — but it is
**painful and flaky to actually use**.

## Finding: `SEMFS_DEBUG_RANKING` propagation is NOT broken

Verified empirically: a mount started with `SEMFS_DEBUG_RANKING=1` produced **154 RANKDUMP lines**.
`std::process::Command` (mount.rs:197) inherits the parent env by default, so the daemon-inner re-exec
**does** receive the var. My earlier "it's not propagating" reads were **test artifacts**, not a code bug:

1. **Log truncation race** — I ran `: > ~/.cache/semfs/logs/<tag>.log` *while the daemon held the file
   open*, which races the write offset and hides output.
2. **`NOT-FOUND` environ check** — `pgrep` grabbed the wrong PID (daemon-inner double-forks/`setsid`;
   the search runs in a different process than the one I inspected).
3. **ANSI-wrapped fields** — tracing emits `stage=\x1b[..mRRF`, so naive `grep 'stage=RRF'` misses it
   unless ANSI is stripped first.
4. **Deadline timing** — the slow query (~23.9 s) interacts with the 20 s in-search deadline, making
   capture timing-dependent.

So: nothing to "fix" in propagation. The real gap is **ergonomics** — debugging ranking currently
requires: re-mounting the daemon with an env var, not truncating the live log, stripping ANSI, and
isolating one query's lines from an append-only shared log. That is too fragile for routine use.

## Proposed fix — first-class `grep --explain`

Add `semfs grep --explain <query>` that returns the per-stage ranking **inline to the client** (no env
var, no daemon re-mount, no log parsing):

```
$ semfs grep --explain "best-selling product ..." .
# stage breakdown for top candidates (RRF rank → rerank rank → final)
  RRF#3  RERANK#10  best_selling_product_core_data_list.txt   vec=7 fts=- code=-
  RRF#1  RERANK#1   _activity_summary_taobaoonsite...xlsx     vec=20 fts=2 code=5
  ...
# target rank: RRF=#N  RERANK=#M  (lanes matched: vec only)
```

Implementation sketch:
- Plumb an `explain: bool` through the IPC `Search` request → daemon returns the per-stage rank table
  (RRF rank, rerank rank, per-lane ranks, final) alongside hits.
- `grep --explain` prints the table; default path unchanged.
- This reuses the data `SEMFS_DEBUG_RANKING` already computes — just returns it structurally instead of
  logging it.

### Secondary (cheap) improvements
- Have `SEMFS_DEBUG_RANKING` also write a clean, ANSI-free `<tag>.ranking.jsonl` (one JSON record per
  candidate per query) so offline analysis doesn't fight the tracing log.
- Document `SEMFS_DEBUG_RANKING` in the benchmark runbook (currently undocumented).

## Why it matters

Ranking is now the active battleground (embedder fixed via Gemma; the residual gap is fusion + query
specificity per `rcas/2026-06-05-...` analysis). Every ranking experiment needs per-stage rank
visibility; making it a one-flag inline call removes a large friction tax from that work.

## Related
- `tickets/embedder-upgrade-gemma-qwen3/` · `tickets/explore-agent-search-behavior/`
- `tickets/rrf-chunk-mass-and-lane-fusion/` (the fusion lever this observability serves)
- `backend/sqlite_vec.rs` (`SEMFS_DEBUG_RANKING` RANKDUMP) · `cmd/grep.rs` · `daemon/protocol.rs`
