# Semantic readdir: `ls` → Knowledge-Graph orientation

**Status:** exploration · **Opened:** 2026-06-06 · **Parent:** `tickets/case289-retrieval-investigation/`
**Origin:** Marmik's idea — "`ls` should return a graphify-style KG so the model understands the dataset
and constructs a relevant query; then `grep` searches the KB and returns."

## ⚠️ SCOPE CORRECTION (2026-06-06) — when/why/where the KG actually helps

After interrogating the architecture from first principles, the KG is **NOT the lever for pinpoint
retrieval tasks like case 289** — and we should be honest about that:

**Why NOT case 289:** `semfs grep` IS the semantic retrieval; for a well-specified lookup it already
returns the answer excerpt. A knowledge graph is a higher-level *map*, not a better answer. Once the query
is right (the **rewrite** handles cross-lingual query formation), **grep alone suffices — the KG is
redundant**, and a KG-header on the grep result (old approach C) is just extra payload on a result that
already has the answer. The case-289 token blowups came from (a) cross-lingual recall miss [fixed by
rewrite] and (b) codex **distrusting the grep excerpt → opening source files → the format trap** — neither
of which a KG fixes. The case-289 lever is the **trust fix** (per-hit COMPLETE/partial completeness
annotation on grep output; see `tickets/case289-retrieval-investigation/` + the trust-fix spec), NOT the KG.

**We do NOT suppress `os.walk`.** A 62KB walk is ~15K tokens, not the catastrophe; the catastrophe is the
format trap *after* it. Let the agent crawl if it wants — the trust fix makes the crawl harmless (it ends
up trusting a complete grep excerpt and stops). Drop any "don't crawl" framing.

**WHERE the KG DOES earn its keep (the real value prop):**
1. **Orientation when the agent doesn't yet know *what* to query** — exploratory entry into an unfamiliar
   corpus ("what is this workspace about?").
2. **Query formation / domain+language cues** — surfacing that topics are e.g. Chinese e-commerce so the
   agent queries in-language (note: the rewrite already covers the language part for grep).
3. **Aggregation / multi-file / multi-hop tasks** — "summarize the workspace", "which files relate to X",
   "give the org structure" — where one grep is not enough and corpus structure matters.

**WHERE to add it (not the grep result for pinpoint queries):** the KG belongs as an **orientation
artifact** for the above tasks — surfaced via (a) the `AGENTS.md` FS-contract framing ("this mount exposes
a knowledge graph + semantic grep") and (b) a cat-able virtual file (`_graph.md`) the agent reads *when it
needs corpus understanding*. NOT injected into every grep result (redundant payload when grep already works).

**Measure it on the right test:** evaluate the KG on **exploratory / corpus-understanding tasks**, not on
case 289 (a pinpoint lookup grep already solves). Building it for 289 would be a solution looking for a
problem; building it for exploration is justified.

## Why this matters (the lever)
Token cost on case 289 is dominated by the agent's **first move**. We proved (cmplocal vs p0all2, *identical
config*): when codex **greps-first** it's 35,241 tokens / 4 calls (≈ cloud's 26,598 / 3); when it **os.walk-first**
it's 92,591 / 7. The agent enumerates first *by default* and the design doc says you **cannot instruct that away**
— so the fix is to make the agent's reflexive orientation move *return semantic relevance* instead of an unranked
file dump. That converts the lucky-35K into the reliable-35K.

## The original idea, judged
**Steelman:** rides the move you can't suppress; meets the agent where it already looks (no hint compliance);
exposes in-language entities so the query is formed correctly (fixes cross-lingual); truest "semantic FS."
**Strawman:** raw `ls`/readdir is load-bearing POSIX — `cp -r`, globbing, tab-completion, and **the agent writing
its output to `model_output/`** all need REAL entries. Replacing readdir with graph-text means `cat`/`cp`/`cd` on
"nodes" fail → MORE thrash. A graph also doesn't map onto per-directory readdir. And agents parse `ls` as filenames
→ format-confusion can backfire (cf. the strengthened grep header: +53%).
**Verdict:** brilliant *kernel* (ride the reflexive move), wrong *vehicle* (don't break readdir). Keep readdir
POSIX-clean; deliver the KG through a channel the agent already uses. Three POSIX-clean realizations below.

---

## The 3 approaches

### A. Graphify digest in `profile.md`
At ingest: L7 entity extraction → KG (`edges` table) → **Leiden community detection** → god-node topic summaries →
write to the virtual `profile.md`. The mount hint already points the agent at `profile.md`.
- **Pros:** orthodox channel (purpose-built virtual file); fully semantic; reuses the graphify design (§B3).
- **Cons:** agent must *choose* to read it before walking (we saw codex walk first ~half the time); needs a rich KG
  (today only 21/595 files have entities → must run comprehensive L7 first).

### B. Ranked + annotated readdir (semantic `ls`)
readdir returns the **real entries** (so `cp`/`cat`/writes work) but **ordered by relevance** and **annotated** with a
one-line semantic summary per dir — via a sidecar `_map.md` surfaced first, or extended attributes.
- **Pros:** rides the reflexive `ls`; POSIX-safe (real names); relevance moves into the index (the doc's core ask).
- **Cons:** annotations on `ls` output are non-standard (need sidecar/xattr plumbing); agent may ignore annotations;
  ordering readdir is unusual and could confuse globbing assumptions.

### C. Map-header on the first `grep` ⭐ (recommended)
The **first** `semfs grep` response is prepended with a compact corpus map (topics + structure + in-language terms),
then the ranked answer. Orient **and** answer in one call.
- **Pros:** rides the **unavoidable** move (the agent *must* grep to get content); orient+answer in a single call =
  cloud's `→done` shape; zero reliance on `cat profile.md` or annotated `ls`; reuses `build_local_profile()` digest.
- **Cons:** adds a little payload to the first grep; only helps once the agent greps (but it always does eventually).

**Ranking: C > B > A > raw-`ls`-overload.** C captures all of the steelman's "ride the unavoidable move" power while
dodging every strawman, and attacks B2 (what the agent *gets back*) — the lever the data keeps pointing at.

---

## Projected tool-call traces (current vs ideal)

### Current — WALK-FIRST (the failure mode), ~92K tokens / 7–19 calls
```
1  os.walk('.')                          → 62 KB of unranked paths (every file looks equal)
2  pandas/openpyxl on a guessed file     → 0–300 B (wrong file / format trap)
3  file / sed probes                     → small, inconclusive
4  semfs grep "best-selling product..."  → answer present, but ~30 KB whole-doc dump
5  cat / sed the answer to verify        → re-read
6  cp → model_output/...                 → write
   (each big output re-replays every turn → ~92 K)
```

### Ideal — A (digest in profile.md), ~30K / 3 calls
```
1  cat profile.md
     → # Container map (Leiden communities)
       ★ product-sales-analytics: best-seller list, 成交金额, 转化率  → /desktop/fashion_ecommerce/…
       ★ taobao-campaigns · ★ PM-docs(chanpin/) · ★ finance(flagship_store/)
       SEARCH FIRST: semfs grep "<in-language query>"
2  semfs grep "畅销产品 商品标题 成交金额 转化率"
     → /desktop/.../best_selling_product_core_data_list.txt:1-10: <chunk, ranked #1>   (≈3 KB)
3  cp answer → model_output/…
```

### Ideal — B (annotated `ls`), ~28K / 3 calls
```
1  ls -la
     desktop/fashion_ecommerce/   # 畅销产品 sales data: title·成交金额·转化率   [HOT]
     taobao_campaigns/            # campaign rules & summaries
     chanpin/                     # PM docs: strategy, requirements, QA
     model_output/                # (your output dir)            ← real, writable
2  semfs grep "畅销产品 成交金额 转化率"   → answer chunk, ranked #1   (≈3 KB)
3  cp answer → model_output/…
```

### Ideal — C (map-header on first grep), ~18–24K / 2–3 calls ⭐
```
1  semfs grep "best-selling product data"
     → ┌ CORPUS MAP ───────────────────────────────────────────────┐
       │ Chinese e-commerce PM workspace (595 files). Top topics:    │
       │  • product-sales-analytics (best-sellers, 成交金额, 转化率) │
       │  • taobao-campaigns · PM-docs · finance                     │
       │ tip: query in-language for best recall                      │
       └────────────────────────────────────────────────────────────┘
       RESULTS (ranked):
       #1 /desktop/.../best_selling_product_core_data_list.txt:1-10: <chunk>   ← answer
       #2 …  (compact chunk returns, ≈10 KB total)
2  cp answer → model_output/…
```

### Target (cloud, measured) — 26,598 / 3 calls
```
1  pwd && ls -la && cat profile.md   (2 KB)
2  semfs grep "..."                  → answer chunk #1 (10.7 KB)
3  write
```

## Comparison
| approach | rides which move | POSIX-safe | needs rich KG | proj. tokens | proj. calls | reliability |
|---|---|:--:|:--:|---:|---:|---|
| current walk-first | — | ✓ | — | ~92K | 7–19 | bimodal/bad |
| current grep-first (lucky) | grep | ✓ | no | 35K | 4 | unreliable |
| A profile digest | cat (opt-in) | ✓ | **yes** | ~30K | 3 | medium |
| B annotated ls | ls (reflexive) | ✓ | partial | ~28K | 3 | high |
| **C map-on-first-grep** | **grep (unavoidable)** | ✓ | partial | **~18–24K** | **2–3** | **highest** |
| raw ls→KG (original) | ls | ✗ breaks writes | yes | ~20K* | 2* | infeasible |

\* only if it worked; it breaks `model_output/` writes, so net worse.

## Experiments (this ticket)
- **K1** — Prototype C: prepend `build_local_profile()` digest to the first grep response only. Test E2E (does it
  push codex to grep-first + 1 call?). Cheap — reuses the existing digest; no re-seed.
- **K2** — Prototype B: sidecar `_map.md` at mount root + relevance-ranked root readdir. Measure if codex reads it.
- **K3** — Comprehensive L7 extraction (595 files) → rich KG → Leiden → real graphify digest (powers A and the
  *content* of B/C). Prereq for the semantic (vs structural) version.
- **K4** — Measure first-move distribution (full-P0 ×5) to quantify the walk-first vs grep-first rate the map must beat.

## Approved KG architecture (2026-06-06)
Graphify-style KG design APPROVED — see **`graphify_kg_architecture.md`**. Decisions: Rust-native
Louvain→Leiden · categorical confidence column · **reuse `profile.md`** (one file, swap content to the KG
digest) · MVP = communities+god-nodes, dynamic on add/remove, must beat the dir-map on tokens. DRY (reuse
edges/extraction/worker/profile.md) · YAGNI (no viz/report/query-API/AST/per-subdir) · SOLID (pure-fn
detection behind a trait). Implementation NOT started — phased plan P1–P5 in the architecture doc.

## Links
- Parent investigation: `tickets/case289-retrieval-investigation/` (issue.md §6, EXPERIMENTS.md E24/E25).
- Design basis: `benchmarks/workspace_bench/SEMFS_GRAPHIFY_DESIGN.md` (§II.10 Leiden, §B3 community digest).
- Digest generator already shipped: `crates/semfs-core/src/cache/profile.rs::build_local_profile`.
