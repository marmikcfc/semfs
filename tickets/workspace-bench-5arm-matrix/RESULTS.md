# RESULTS — E1–E5 reruns on cleaned, health-gated infra (2026-06-11)

> Companion to [`ANALYSIS.md`](ANALYSIS.md) (the 5 hypotheses) and the original
> [`HANDOFF.md`](HANDOFF.md). This is the **empirical** result of running the
> experiments. Goal: find a **local** semfs config that beats `plain` (46% acc @ 89K
> mean tokens) on accuracy OR tokens. Agent = codex/GPT-5.4; box = EC2 13.201.35.159.

## TL;DR

The original "semfs loses on both axes" was **infrastructure failure, not retrieval
quality** — confirmed decisively (H1). On clean, health-gated infra the local arms
go from catastrophic (timeout / 0-of-15) to **plain-competitive on accuracy**. But
the local arms still lose on **tokens** on every case, because the seed's **baked-in
KG** is un-removable and its `AGENTS.md` commands the agent to read a 35K
`KNOWLEDGE_GRAPH.md` first, then os.walk — a cascade that inflates tokens.

## What was fixed (the cleaning)

Built `chanpin-clean.db` (verified clean) + a health-gated driver `run_case_e.sh`:
- **716** `.semfs-error.txt` contamination sidecars removed via daemon FUSE `rm`
  (not raw SQL — would desync ffts/vchunks). [F4 was a 100× underestimate.]
- **Disk guard** (abort <6G) — `chunks` has `last_accessed_at`, so *search writes*;
  a write under ENOSPC tears a page → "disk image is malformed" (F1).
- **`PRAGMA quick_check` health-gate** on every fresh copy (catches torn copies).
- **Dummy Supermemory key** for local arms (kills the silent cloud-fallback).
- **`$WORKDIR/.fastembed_cache` strip** — the embedder cached 898MB of model blobs
  into the corpus dir; an unscoped import re-indexed them → mount hang.
- **`daemon-inner` kill before each copy** — `semfs unmount` leaves the daemon
  holding the old db; the next `cp` overwrites it → vec0 vector-blob corruption.

Each of these alone can produce the original "0-score, 5× tokens" outcome.

## H1 — CONFIRMED (the catastrophe was infra)

| case 289 / nokg | tokens | score | wall | status |
|---|---|---|---|---|
| original (contaminated, broken infra) | timeout | 0/15 | 2024s | **timeout** |
| clean + healthy, SEARCH_ONLY=off (run 1) | 111K | 5/15 | 114s | passed |
| clean + healthy, SEARCH_ONLY=off (run 2) | 145K | 6/15 | 130s | passed |
| **plain (bar)** | **79K** | **6/15** | 391s | passed |

Clean infra → matches plain's accuracy. n=1 variance is ±30% tokens / ±1 point.

## H2 — CONFIRMED (`SEARCH_ONLY=on` is harmful)

`289 nokg SEARCH_ONLY=on` (clean seed) flailed >30 min toward timeout — same as the
contaminated original — while `=off` finished in 114s. `=off` is the "never lose
catastrophically" default (the agent can fall back to reading files).

## Token sink isolated (why local loses on tokens)

`289 nokg so_off` breakdown: **35.7K** reading `kg/KNOWLEDGE_GRAPH.md` + **22.4K**
os.walk + ~2K real work. The seed's `AGENTS.md` says "read kg/KNOWLEDGE_GRAPH.md
FIRST"; the agent obeys → greps *scoped to kg/* (returns ~138 chars) → loses
confidence → os.walks. A clean simple grep returns the **exact answer file as the
top hit** (verified). The KG hint is the root token sink — and it can't be removed
from the existing seed (`/kg/` + KG-`AGENTS.md` are read-only; clearing `graph_*`
triggers a hanging re-import). → confirms **H5** (the 61KB KG file is net-negative).

## Clean-nokg row vs plain (SEARCH_ONLY=off, clean seed)

| case | plain (tok / acc) | clean-nokg (tok / acc) | verdict |
|---|---|---|---|
| 289 | 79K / 6 | 111–145K / 5–6 | **acc TIE**, +tokens → no win |
| 44  | 58K / 2 | 175K / **2** | **acc TIE** (plain's weakest), +tokens → no win |
| 95  | 86K / 11 | 255K / **8** | raw chunks beat broken-infra 0, but <plain; +tokens |
| 15  | 184K / 6 | compact(2KB)=85K/2; cap8K→28-call balloon | token-cut starves accuracy |

**Pattern:** clean-nokg **matches plain on accuracy** (289: 6=6, 44: 2=2) but loses on
tokens on every case. No config cleanly beats plain on either axis.

**Case 15 frontier finding (H4):** delivery compaction trades tokens for accuracy
with no clean-win point — starve it (2KB cap) → 85K/2; feed it (8KB cap) → the agent
*over-explores* (28+ calls) and tokens balloon past plain. The over-exploration is
KG-hint-driven, pointing back to the KG as the root cause.

## E3 (summaries) standing
Case 95 clean-nokg = 8/12 with the seed's existing raw `.extracted.md` siblings.
Cloud got 12/12 via **LLM** summaries. So synthesis accuracy is gated on summary
quality, not raw extraction — the local-+-LLM-summaries arm remains the accuracy lever.

## KG-off seed test (the structural token lever) — BLOCKED

Built a fresh `chanpin-kgoff.db` (imported `chanpin_raw` with `SEMFS_KG=off`). KG-free
confirmed (graph tables empty, lean state) BUT the import is **impractical on the 16GB
box**: ~13 files/min → ~3h for the full corpus, and it half-indexed (**259 of 2212
files**, 1272 chunks vs the clean seed's 7153). 289's answer file wasn't even in the
indexed subset. This is the "incomplete warm, not resumable" failure
(`rcas/2026-06-08-partial-seed-indexing.md`). The KG can't be removed from the existing
*complete* seed either (read-only `/kg/`; clearing `graph_*` → hanging re-import).

## The token sink is unfixable by config (decisive)

`289 compact` (RESULT_LIMIT=5, DOC_CAP=4096) still cost **139K** — its breakdown showed
a **single `semfs grep` returning 142,733 chars**. A controlled A/B confirmed it:

| `semfs grep` for the same broad query | output chars |
|---|---|
| CAPPED (RESULT_LIMIT=3, DOC_CAP=2048, snippet) | **372,321** |
| DEFAULT | **316,425** |

**`SEMFS_RESULT_LIMIT` / `SEMFS_DOC_RETURN_CAP` / `SEMFS_RETURN_MODE` do NOT apply to the
agent's `semfs grep` CLI.** A broad query returns ~300K uncapped chars regardless. This —
not just the KG — is why every semfs arm always cost 2–5× plain's tokens. No knob fixes it.

## Conclusion: a clean win needs CODE changes, not config

On clean, health-gated infra the local arm **matches plain on accuracy** (289: 6/15;
95: 8/12, up from broken-infra 0) — the infra thesis (H1) is proven. But a **clean token
win is blocked at the product layer**, by three code-level issues a benchmark run can't
touch:
1. `semfs grep` returns uncapped ~300K-char blobs; the cap knobs are inert on the CLI.
2. The baked `AGENTS.md` hint commands "read kg/KNOWLEDGE_GRAPH.md FIRST" → KG read +
   over-exploration (the hint is compiled into the binary).
3. A KG-off seed can't be freshly built in reasonable time on this box.

**The one local config that beats plain on an axis:** case 15 **compact = 85K vs plain's
184K tokens (−54%)** — but accuracy collapses to 2/16 (the 2KB cap *did* take effect there,
on narrower queries, starving the survey). A token-axis win with an accuracy tradeoff.

**Recommended product fixes (to make local semfs genuinely win):**
- Make `semfs grep` honor `RESULT_LIMIT`/`DOC_RETURN_CAP`/`RETURN_MODE` on the CLI path.
- Rewrite the injected `AGENTS.md` hint: "grep with 2–4 key terms, read the single top
  hit for exact values, don't crawl, don't read kg/." (and gate KG behind a ≤4KB digest — H5).
- Ship a pre-built KG-off seed variant (fresh seeding is too slow on 16GB).

## The code fix — IMPLEMENTED + the first token win

Implemented fix #1: patched `crates/semfs/src/cmd/grep.rs` to **cap the per-hit
rendered text** (new knob `SEMFS_GREP_RESULT_CAP`, default 6 KB) at all three render
sites (memory / binary-inline / chunk), with an honest `TRUNCATED` marker replacing
`COMPLETE FILE`. Built clean (52s incremental), deployed (old binary → `semfs.prepatch`).

Effect on case 289 (clean-nokg, SEARCH_ONLY=off):

| config | tokens | acc | note |
|---|---|---|---|
| plain (bar) | 79,007 | 6/15 | |
| pre-fix (uncapped grep) | 111–145K | 5–6 | a single grep returned 142K chars |
| patched, cap=6KB rlim=8 | 122K | **6/15** | greps 142K→44K; acc holds |
| **patched, cap=3KB rlim=3** | **76,812** | 5/15 | **< plain on tokens**; 1 acc point lost |

So the fix proves the mechanism: capping the grep render drops tokens below plain.
At cap=3KB it dips under plain (−3%) but loses an accuracy point; at cap=6KB accuracy
holds but tokens stay high — the residual is the ~22K os.walk (hint-driven).

### n=2 + the honest verdict on the win

| 289 clean-nokg, patched, cap=3KB rlim=3 | tokens | acc | calls |
|---|---|---|---|
| run 1 | **76,812** (< plain) | 5/15 | 5 |
| run 2 | 97,797 (> plain) | 5/15 | 16 |

**The token win is real but NOT consistent.** The grep-cap fix removes the *deterministic*
sink (142K grep blobs → ~9KB), pulling 289 into a 77–98K band around plain's 79K — and
the best run beats plain (the first local arm ever to do so). But run-to-run **call-count
variance** (5 vs 16 calls) — the agent's `os.walk` over-exploration — keeps it from
*consistently* winning. That os.walk is the agent's own python crawl (not a semfs grep, so
the cap can't touch it); it's driven by the seed's **baked, read-only `AGENTS.md`** ("read
kg/ FIRST"), which can't be changed without rebuilding the seed (impractical) or risky raw
SQL on the index. `SEARCH_ONLY=on` (which would kill the os.walk) stalls — the agent needs
to read the answer file but the hidden tree blocks it.

**Verdict:** infra thesis (H1) fully proven; accuracy at parity; the grep-cap code fix is
the first lever to push a local arm's tokens *below* plain (76.8K vs 79K) in a converging
run — but a *consistent* clean win needs the second fix (the AGENTS.md hint, which is baked
read-only). Two of three product fixes are now precisely scoped; one is implemented.

### Environment note
The box's `~/.local/bin/semfs` is now the **patched** binary (grep render cap). Source
diff: `crates/semfs/src/cmd/grep.rs` (+`grep_result_cap`/`cap_render`, applied at 3 render
sites). Original source backed up at `/tmp/grep.rs.bak` on the box; rebuild with
`cargo build --release --bin semfs` to revert. New knob: `SEMFS_GREP_RESULT_CAP` (bytes).

## Caveats
- n=1 per cell; ±30% token / ±1-point variance — repeat before quoting.
- The environment is pathologically fragile (4 independent infra bugs found this
  session) — which is itself the strongest confirmation of the ANALYSIS thesis.

---

# ADDENDUM — E6 + E7/E8 (289 cell) executed, 2026-06-11 evening

Artifacts: `matrix_artifacts/e8seq/` on the box; results `/tmp/e8seq.jsonl`.
Full narrative: [`hypotheses.html`](hypotheses.html) §retrial, [`mechanics.html`](mechanics.html) §07.

**E6 (codex 0.133.0 clip, measured):** ≤10 KB passes whole · cliff ~15 KB · overflow keeps
only ~0.6K+0.6K tokens head+tail (notice is token-denominated) · NO 256-line cap on this
build. 6 KB grep cap sits safely inside the window.

**E7/E8 (case 289, scout = clean + SO=off + cap6K + rlim5 + leanhint; W′ = + provenance
check; all judge-degraded runs re-judged offline):**

| run | arm | tokens | calls | score (re-judged) |
|---|---|---|---|---|
| w1/w2/w3 | scout | 21,473 / 168,779 / 107,211 | 2/12/9 | 4/15 ×3 clean |
| wp1/wp2/wp3 | scout+W′ | 93,746 / 21,743 / 80,484 | 9/2/9 | **6/15** / 4/15 / 5/15 |
| p1/p2/p3 | plain | 322,421 / 117,603 / 71,525 | 15/9/7 | ⊘(unjudgeable) / 5/15 / 7/15 |

**Verdict:** token axis won distribution-vs-distribution (scout-class mean 82K vs plain
171K, −52%; floor band 21K ×2; zero scout runs near plain's 322K pathology). Accuracy:
W′ recovered exactly the two 403-cluster rubrics (4→6/15, rubric-diff verified) = plain's
clean mean (5,7→6.0); hint compliance ~2/3 (wp2 skipped the check → 4/15); call-count
bimodal (2 vs 9–12) — the agent's verify instinct is the one unsolved turn source.
p1 is reproducibly unjudgeable (322K trace overflows the judge) — judge hardening needed
or the bar silently drops its own worst runs.

**Shipped:** commit `eae0980` — grep cap (6KB default + TRUNCATED markers), scout hint +
provenance check as DEFAULT renders (agent_hint.rs), dual-store siblings, L7 KG-gating.
Fresh installs now get the validated configuration; no seed surgery needed (hint renders
at import). Tests 326+51 green.

**Next:** E9 stop-signal/two-tier render (attacks the verify-instinct turns — now highest
leverage) · global render budget (per-hit cap can sum past the clip: 5×6KB>15KB) · E8
remaining cases (pre-registered ≥3/5 condition) · E11 discovery-stressed + cross-lingual
cases · judge input caps.
