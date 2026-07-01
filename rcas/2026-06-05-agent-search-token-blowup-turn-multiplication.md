# RCA — why semfs+sqlite burns 4× the tokens of Supermemory on case 289 (agent behavior)

- **Date:** 2026-06-05
- **Ticket:** `tickets/explore-agent-search-behavior`
- **Model:** codex GPT-5.4, Workspace-Bench case 289 (`"best-selling product"`)
- **Status:** RCA from reading the two collected traces (sqlite-rrffix, plain-codex). Supermemory trace
  still uncollected — its mechanism inferred, not yet trace-confirmed.
- **Thinking models used:** Scientific Method (hypothesis→test), Theory of Constraints (the constraint
  is *turn count*, not search payload), Systems Thinking (token cost is a re-send loop, not a per-call cost).

## TL;DR — the ticket's premise was wrong; the real cause is turn-count × uncached re-send

The ticket assumed the gap was **ranking** (answer at cross-encoder #6, so codex can't grab it in one
shot). **The trace shows otherwise:**

1. The first **two** `semfs grep` calls **timed out** — `daemon search failed (search timed out after
   25s); falling back to cloud search (sqlite/pgvector degraded-dependency path)` → **"no results."**
   The agent got *nothing* from its first two searches.
2. When search finally worked (the 3rd `semfs grep`), the answer file
   `best_selling_product_core_data_list.txt` came back at **rank #1**, full verbatim content. Not #6.
3. Starved of search, the agent fell back to brute-force FS exploration. Its `python os.walk` dumped
   **51,154 B** (~13 K tokens) of directory tree into context on call #3.
4. **No prompt caching** (`cache_read=0, cache_write=0`). Every turn re-sends the whole transcript,
   billed fresh. The 51 KB walk, carried across the ~10 remaining turns, accounts for **~128 K of the
   143 K prompt tokens (~88%)** — confirmed by re-modeling cumulative context (159 K modeled vs 143 K
   actual).

So the cost is not "semfs returns big payloads" and not "answer ranked #6." It is:
**failed early searches → agent brute-forces the FS → one huge dump → re-sent uncached every turn.**

## The actual 12 logical tool calls (the "24" double-counts start+finish events)

| # | kind | out | what |
|--:|------|----:|------|
| 1 | shell | 1.8 KB | `pwd && ls -la && cat profile.md` (orient) |
| 2 | **semfs** | 372 B | grep q1 → **TIMEOUT 25s → no results** |
| 3 | python | **51 KB** | `os.walk('.')` full tree dump ← the token sink |
| 4 | **semfs** | 343 B | grep q2 → **TIMEOUT 25s → no results** |
| 5 | python | 4.8 KB | keyword walk |
| 6–10 | python | <0.4 KB ea | 5 attempts to parse `top10_product_status_table.xlsx` — **all fail** |
| 11 | **semfs** | 2.3 KB | grep q3 → **works, answer file at rank #1** |
| 12 | write | 469 B | copy `best_selling_product_core_data_list.txt` → done |

Calls 2–10 (nine calls) are pure waste, caused by (a) the two timeouts and (b) fixation on the wrong
`.xlsx`. Once search worked (11), the agent finished in one step (12) — i.e. when semfs works it behaves
like the 1-call Supermemory run.

## Secondary finding — corrupt ingestion: .xlsx stored as 403 HTML

The 3rd grep's other results show the `.xlsx` files were ingested as **403 Forbidden HTML pages**
(`<title>403 Forbidden</title> ... openresty`), not spreadsheet content. That is why the agent's 5
attempts (calls 6–10) to open `top10_product_status_table.xlsx` all failed — the bytes on disk are a 403
page. Separate bug from the token issue, but it amplified the wasted calls.

## Theory of Constraints framing

Token cost ≈ `turns × avg_context_per_turn` (because uncached re-send). Two multiplicands:
- **turns** — inflated by failed searches (timeouts) forcing exploratory fallback.
- **avg_context** — inflated by one 51 KB walk that then persists in every later turn.

The **binding constraint is turn count**: Supermemory's 1 good search at #1 collapses turns → collapses
the re-send multiplication. The win was never a smaller payload; it was *finishing sooner*.

## Hypotheses + how to prove/disprove (each tied to a token lever)

| # | Hypothesis | Prediction if true | Test | Lever |
|--:|-----------|--------------------|------|-------|
| H1 | **Daemon timeout** is the trigger: cold/degraded local search times out at 25 s on first calls. | Re-run with a warm daemon → 0 timeouts → ≤3 calls → tokens collapse toward Supermemory range. | Pre-warm daemon, re-run case 289 ×5, count `search timed out` warnings + total tokens. | Removes ~9 wasted calls. |

### H1 — EVALUATED 2026-06-05: CONFIRMED as the trigger (high confidence; final E2E pending)

**Verdict:** the timeout cascade IS the trigger of the brute-force fallback and the token blowup.
Two confounds ruled out; mechanism confirmed in code.

**Evidence chain**
1. **Trace:** q1 & q2 carry `daemon search failed (search timed out after 25s); falling back to
   cloud search` → cloud returns empty → agent dumps the 51 KB `os.walk` and brute-forces. q3 has
   **no** fallback warning → it took the `DaemonSearch::Hits` path = **served by the local daemon**,
   returned the answer at **rank #1**, and the agent finished in one more step. So: timeout ⇒ 9 wasted
   calls; local success ⇒ 1 call. The trigger is the timeout, not ranking.
2. **Confound A (local vs cloud) — resolved:** the `tracing::warn!` fallback line (grep.rs:656) is
   present only on q1/q2. Its absence on q3 proves q3 was local, not a lucky cloud hit.
3. **Confound B (timeout vs query text) — resolved by the 2026-06-04 instrumented repro:** the *exact*
   failing queries return **72–84 hits on an idle daemon** (and the Chinese query's top hit is the
   answer file). Same text, idle box → hits; loaded box → timeout→empty. So q1/q2's empties are
   **timing/contention, not recall or text quality.**
4. **Code mechanism (why even the post-fix run still timed out):** there are **two** 25 s-ish bounds:
   - in-search cooperative `SEARCH_DEADLINE = 20 s` (sqlite_vec.rs:40) — the 2026-06-04 fix; degrades
     by skipping rerank and returning best-effort RRF hits.
   - outer **IPC hard `SEARCH_TIMEOUT = 25 s`** (ipc.rs:163) wrapping the whole `index.search()`.
   The query-embed (`embedder.embed`, sqlite_vec.rs:826) runs **first, inside `spawn_blocking`, which
   Tokio cannot cancel**, and the only deadline checkpoint is *after* embed returns (sqlite_vec.rs:~870).
   Under CPU starvation the **embed alone exceeds 25 s** → the outer IPC timeout fires while the
   blocking thread is still stuck in embed → `SearchError "timed out after 25s"` → cloud fallback →
   empty. The 2026-06-04 best-effort-RRF fix sits *downstream of embed*, so it never runs. **The fix
   reduced 28→24 calls but cannot cover an embed that never returns in-window.**
5. **Why starved:** background L7 entity-graph extraction (per-case auto-import re-index) + per-query
   L5 cross-encoder rerank peg the shared box (~350% CPU in the 2026-06-04 run) exactly when the
   agent's first searches fire. q3 succeeds because by ~50 s in, that burst has subsided / embedder is warm.

**What's left to fully close (requires the EC2 seed; not on this laptop):**
- **Cheap, decisive:** on the box, run q1/q2/q3 offline against the clean seed under (a) idle and
  (b) synthetic ~350% CPU load; log embed-ms + total-ms + hit count. Predict: idle→all hit <25 s;
  loaded→q1/q2 blow 25 s in embed. (Largely already shown idle-side by the 2026-06-04 repro; the
  load-side embed-ms timing is the one missing measurement.)
- **E2E confirmation (billable):** re-run case 289 ×5 with daemon pre-warmed AND background L7
  throttled/paused during the agent; count `search timed out` warnings + total tokens. Predict:
  0 timeouts → ≤3 calls → tokens collapse toward the ~36 K Supermemory range.

**Fix direction implied (since H1 holds):** bound/cancel the embed itself (the uncancellable
`spawn_blocking` is the real gap), and/or pause background L7 + cap rerank threads while a foreground
search is in flight, and/or pre-warm the embedder at mount. Pre-warm alone is necessary but **not
sufficient** if per-case L7 re-index re-saturates the box each mount (see the per-case re-mount thrash
tickets) — so this connects H1 to the auto-import re-index work, not just a warm-up call.

### H1 — EC2 EMPIRICAL TEST (2026-06-05, `m7i.xlarge` 4 vCPU, warm chanpin sqlite, 12,335 chunks)

Ran the 3 exact case-289 queries against the cache DB via the direct-sqlite grep path (no daemon),
idle vs escalating synthetic CPU load (`stress-ng --cpu N --cpu-method matrixprod`). Answer file
`best_selling_product_core_data_list.txt` IS indexed (1 chunk); embedder = `snowflake-arctic-embed-s`.

| CPU load (threads / 4 cores) | wall per grep | hits | crosses 25s? |
|---|---:|---:|---|
| idle | ~5.0s (first cold-cache call 19.5s) | 10 | no |
| 8 (≈200%) | ~12–14s | 10 | no |
| 16 (≈400%) | ~23s | 10 | edge |
| 24 (≈600%) | ~34s | 10 | **yes** |

**Findings:**
1. **Idle recall is fine** — all 3 queries return 10 hits in 5–7 s (re-confirms the 2026-06-04
   correction: not a recall bug).
2. **Dose-response is monotonic** — CPU contention scales search wall-time; at ~6× oversubscription a
   single grep takes 34 s, past both the 20 s cooperative deadline and the 25 s IPC hard timeout. So
   **CPU starvation can, in principle, blow the timeout.**
3. **But the bulk of CLI wall-time is per-process model-load**, not embed/search: the cooperative
   "exceeded deadline during query-embed" warning **never fired even at 34 s**, because the deadline
   clock starts *inside* `search_blocking`, *after* the embedder is constructed. Each fresh CLI grep
   reloads the arctic-s ONNX model; the daemon does NOT (see #4).
4. **Code check — the daemon loads the model EAGERLY at mount** (`daemon_runtime.rs:48 build_embedder`
   → `embed/local.rs:69 TextEmbedding::try_new`, fastembed loads ONNX on construction). So **the
   daemon's model is already warm before q1** — the "lazy first-search model-load" idea is FALSE, and a
   warm-daemon search (model-load excluded) is far cheaper, so pure ambient CPU is **unlikely** to push
   it past 25 s on its own.

**Refined mechanism (code-proven, supersedes "ambient CPU starvation" as the q1/q2-specific cause):**
The daemon serializes ALL DB access — indexing writes AND search reads — through **one
`Mutex<Connection>`** (`cache/db.rs:45-46`: `pub struct Db { conn: Mutex<Connection> }`), and the
search takes that lock at `sqlite_vec.rs:880`, **after** the embed. `journal_mode = WAL` is set
(`db.rs:73`) — which *would* allow a concurrent reader + writer at the SQLite level — but the
application-level mutex nullifies it. So during the **post-mount indexing burst** (local embed + L7
graph writes of the freshly-mounted files, the ~300–357% CPU / 33 min seen in the 2026-06-04 run), the
agent's q1/q2 searches embed fine, then **block on `conn.lock()` behind the indexer's write
transactions past the 25 s IPC timeout** → `SearchError "timed out after 25s"` → cloud fallback →
empty → brute-force. By q3 the indexing has drained → lock free → search returns the answer at #1.
This explains **q1/q2-fail-but-q3-succeed** (a time-varying lock, not steady ambient CPU — which would
hit all three equally and, per the dose-response, doesn't blow a warm search anyway).

**Verdict:** H1 CONFIRMED as the trigger. Dominant cause refined from "CPU-starved embed" (2026-06-04)
to **DB-connection lock contention from concurrent post-mount indexing** (CPU is a contributing, not
sufficient, factor).

**Corrected fix direction (sharper):**
1. **Give search a dedicated read-only WAL connection** so reads never queue behind the indexer's write
   lock. Highest leverage: searches succeed *during* indexing → no timeout → no brute-force → tokens
   collapse. (WAL is already enabled; the only change is to stop routing reads through the write mutex.)
2. **Make the embed/search cancellable** (today `spawn_blocking` can't be aborted, so a timed-out
   search keeps burning CPU after the client gives up).
3. Throttle/pause background L7 while a foreground search is in flight (secondary if #1 lands).
4. **Pre-warm is NOT the fix** — the model is already eager-loaded at mount; the original "warm the
   daemon" framing is wrong for this cause.

**Not yet done (optional, OOM-risky):** a live daemon repro (mount + concurrent indexing + grep) to
empirically catch the `conn.lock()` block — needs a scoped `--memory-paths` mount to dodge the
full-container mount OOM (BUG #8). The single-mutex code path + the trace pattern + the dose-response
ruling out pure-CPU-on-warm are strong convergent evidence without it.

### H1 — E2E SMOKE TEST (2026-06-05): CONFIRMED, but only ~43% of the gap

Shipped the 25s→50s timeout, rebuilt + deployed on EC2, and re-ran semfs-codex case 289 against a
throwaway copy of the config-#8 e5 seed (`SEMFS_NO_PUSH=1 SEMFS_NO_SYNC=1`; canonical seed md5 verified
unchanged before/after). Same case, same seed content, only the timeout changed:

| metric | OLD (25s) | NEW (50s) | Δ |
|---|---:|---:|---|
| timeout warnings | 6 | **0** | gone |
| "no results" | 6 | **0** | gone |
| total tokens | 145,696 | **82,653** | **−43%** |
| tool events | 24 | 19 | −21% |
| `semfs grep` (timed-out) | 3 (2 timed out) | 2 (0 timed out) | |
| status | passed | passed | |

**Verdict:** H1 is confirmed — removing the timeout eliminated the timeout→empty→brute-force cascade,
the first search now succeeds, and tokens dropped 43%. The agent no longer dumps a 51 KB `os.walk`
across every turn (the new walk is 15 KB and the run has fewer turns).

**But H1 is NOT the whole story — ~46K gap to Supermemory (~36K) remains**, from the new run's call
breakdown:
1. **Payload size (now the dominant lever).** The first `semfs grep` returned **211 KB** (whole-doc
   returns, `DOC_RETURN_CAP=64KB × up to 10 docs`). With caching off it re-replays — this is H3.
2. **Corrupt `.xlsx` (H5) still wastes ~6 calls.** The agent still chased
   `top10_product_status_table.xlsx` (the 403-HTML file) through openpyxl/pandas/zipfile/file/sed —
   all failing — even after the first search surfaced the answer. Fixing ingestion removes these.
3. **No prompt caching (H2).** `cache_read=0` still — the single biggest untapped lever.
4. **Ranking (H4).** Answer ~#6 on the e5 seed, so the agent verifies rather than grabbing #1.

Next levers in order: H3 (cap whole-doc return size) + H5 (fix 403-HTML ingestion) → then H2 (caching).
Throughput read-path isolation (`tickets/search-throughput-readpath-isolation`) makes the 50s itself
unnecessary by removing the lock-wait that the timeout was padding for.
| H2 | **Uncached re-send** is the multiplier: no `cache_read`. | Enabling prompt caching cuts prompt tokens far more than cutting any single output. | Turn on Anthropic/codex prompt caching (or measure `cache_read>0`) and re-run; compare prompt_tokens. | ~Largest single lever (88% of tokens are re-sent context). |
| H3 | **One 51 KB walk** dominates: agent's own FS dump, not semfs output, is the sink. | Capping/streaming tool output to ~4 KB cuts tokens ~80% even with same turn count. | Add a tool-output cap (head -c / truncation) in the bench harness; re-run, compare. | ~128 K → ~tens of K tokens. |
| H4 | **Ranking is NOT the cause** on this case (answer was #1 when search worked). | Forcing #1→#6 would *not* materially change tokens here; fixing #6→#1 won't help case 289. | Re-rank check: confirm answer rank across queries; compare token delta. | Rules out reranker as the case-289 lever. |
| H5 | **Corrupt .xlsx (403 HTML)** caused the 5 dead-end parse calls. | Re-ingesting real .xlsx bytes removes calls 6–10. | Verify stored bytes for `top10_product_status_table.xlsx`; re-seed; re-run. | Removes ~5 calls. |

## Recommended order (highest token-savings-per-effort)

1. **H2 prompt caching** — biggest lever, no behavior change. (Verify why `cache_read=0`.)
2. **H1 daemon warmth / raise-or-fix the 25 s timeout** — removes the trigger that starts the cascade.
3. **H3 cap tool output** in the agent harness — bounds worst case regardless of search quality.
4. **H5 fix ingestion** (403-HTML-as-xlsx) — correctness + removes dead-end calls.
5. **H4** — deprioritize reranker work *for this case*; it wasn't the cause here (revisit on cases where
   the answer genuinely ranks low).

## Related
- `rcas/2026-06-04-semfs-codex-clean-seed-timeout-poor-local-search-recall.md` — the 25 s timeout (corroborated here).
- `rcas/2026-05-29-semfs-grep-claude-token-blowup.md` — earlier token-blowup thread.
- `tickets/rrf-chunk-mass-and-lane-fusion/` — reranker work (H4 says: not the lever for case 289).
