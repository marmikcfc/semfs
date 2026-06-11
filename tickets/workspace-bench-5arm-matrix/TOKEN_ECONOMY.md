# Token economy — first-principles decomposition + five-whys (2026-06-11)

> Companion to [`RESULTS.md`](RESULTS.md) (E1–E5 outcomes) and [`ANALYSIS.md`](ANALYSIS.md) (H1–H5).
> Method: first-principles decomposition of the cost function, verified against actual traces,
> then five-whys-plus root-cause chains on both axes. Feeds [`OPINIONS.md`](OPINIONS.md) and
> [`EXPERIMENTS_NEXT.md`](EXPERIMENTS_NEXT.md).

---

## 1. The cost equation (the physics, not the convention)

For a single-turn codex session with `cached_input=0`, every turn re-pays the entire
accumulated context. Stripping all conventions away, total cost is exactly:

```
TotalTokens = Σₜ inputₜ + Σₜ outputₜ           t = 1..T (tool-call turns)

inputₜ = S + Σ_{s<t} (aₛ + oₛ)
  S  = static scaffold (system + task prompt + injected AGENTS.md hint)  — re-paid EVERY turn
  aₛ = assistant text/tool-call at turn s
  oₛ = MODEL-VISIBLE tool output at turn s   (≠ what the tool emitted — see §1.2)

⇒  TotalTokens ≈ T·S  +  Σₛ (T−s)·(oₛ+aₛ)  +  G        G = generated tokens
```

Three structural consequences — these are laws, not knobs:

1. **Turn count T multiplies the scaffold** (`T·S`). Every avoided turn saves a full
   scaffold re-read.
2. **A tool output at turn s is re-paid (T−s) times.** Early big reads are the most
   expensive reads in the whole run. The baked hint commands the 35.7K KG read at
   **turn 1** — the worst possible position.
3. **Output tokens G are a separate, small stream** on WB (measured: 1.0K–3.0K vs
   78K–138K input). The input side is ~96% of the bill at equal pricing.

### 1.1 Verified against traces

| run | T | tool-output chars emitted | input tokens | output tokens |
|---|---|---|---|---|
| `289_plain` | 7 | 42,457 | 77,966 | 1,041 |
| `95_cloud` (12/12 win) | 6 | **830,726** | 138,268 | 3,033 |
| `289 nokg so_off` (RESULTS) | — | 35.7K KG read + 22.4K os.walk + ~2K useful | 111–145K | — |

Reproduce: `python3 .claude/skills/analyze-benchmark-results/scripts/trace_breakdown.py artifacts/run5arm/<run>/output/raw/codex_stdout.jsonl`

**289_plain decomposed:** its first two `find` probes returned 16.6K + 23.2K chars at
turns 1–2 of 7 — re-paid 5–6 times each. **~60–70% of plain's 79K is plain re-paying its
own discovery probes.** Plain is not efficient; it is merely *less wasteful than broken semfs*.

### 1.2 NEW finding — the codex clip layer (changes how we read every number)

`95_cloud` emitted **830K chars** of tool output (one `semfs grep` returned **510,402
chars**) yet the whole turn cost only 138K input tokens. That is arithmetically impossible
if the model saw those bytes (≥200K tokens of CJK from one call alone, re-paid twice).
**Conclusion: the codex harness truncates tool output before the model sees it.**

- There is a hidden, *uncontrolled* truncation layer between `semfs grep` and the model.
- The grep-cap fix (`SEMFS_GREP_RESULT_CAP`, RESULTS §"code fix") is therefore not just
  "fewer bytes" — it is **taking control of WHICH bytes survive**. Codex clips blindly
  (and may cut the ranked list or the answer row); semfs capping at 6KB chooses the best 6KB.
- BUT: the `289 compact` run (RESULTS) shows a 142,733-char grep inside a 139K-token run —
  so the clip threshold is not tiny, not constant, or char→token ratios differ. **The clip
  behavior is unmeasured → calibration experiment E6 (EXPERIMENTS_NEXT.md) before any
  further delivery A/B.** This also matches the PwC harness paper (§RESEARCH_NOTES):
  the harness layer reshapes results more than the retriever does.
- **2026-06-11 documentation found** ([openai/codex#6426](https://github.com/openai/codex/issues/6426)):
  codex truncates per tool call at **256 lines OR 10 KiB, whichever first, head+tail**
  (first 128 + last 128 lines; middle dropped); applied to exec AND MCP tools since v0.56.
  A ~25K-token limit is *proposed there, not implemented*. Consequences:
  (1) the clip is BYTE/LINE-based, so **density per byte and per line** is what buys
  information through the window — and the 256-line cap means many-short-lines renders
  (ranked path lists) can clip *before* 10 KiB;
  (2) head+tail ⇒ put the answer-bearing content FIRST (and optionally a recap line last);
  (3) re-reading the grep-cap win: with blobs always clipped to ≤10 KiB visible, the
  111–145K→76.8K drop cannot be mostly "fewer visible blob bytes" — it must be partly
  **behavioral** (cleaner results → fewer re-grep/crawl turns) plus payload 10 KiB→3 KB.
  E6 narrows to: verify the box's codex version + confirm the 256/10 KiB numbers
  empirically (~10 min), then measure the behavioral vs payload split.

### 1.3 The floor (what the task irreducibly costs)

Minimum viable run for a 289-class QA case: 1 search → 1 answer-file read → 1 write.
With S≈3K tokens, T=3, answer content ≈5K tokens, deliverable+narration ≈3K:

```
floor ≈ 3·3K + 5K·1 + 3K ≈ 17–22K tokens
```

plain pays **79K**; best semfs run pays **76.8K**. *Everyone* is ≥3.5× above the floor.
**Reframe: the contest is not "semfs vs plain ±3%" — it is "who closes the 4× gap to the
floor".** The gap is made of: discovery-probe re-payments (plain), hint-commanded KG read +
os.walk (semfs), and scaffold re-reads from extra turns (both).

---

## 2. Component tree (smallest independently-tweakable parts)

```
TOKENS
├── T  turn count
│   ├── discovery turns   (find/grep probes)        ← index quality, result trust
│   ├── acquisition turns (file reads)              ← .extracted.md readability
│   ├── verification turns (re-grep, os.walk)       ← HINT-induced confidence collapse
│   └── synthesis turns   (write deliverable)       ← irreducible (~1–2)
├── S  per-turn scaffold                            ← task prompt (fixed) + injected hint (ours)
├── oₛ per-call visible output                      ← SEMFS_GREP_RESULT_CAP × codex clip (E6)
│   └── position s of big outputs                   ← hint-commanded read ORDER (KG first = worst)
└── G  generated tokens                             ← narration verbosity (caveman ticket) + deliverable

ACCURACY
├── corpus       answer exists & readable           ← 403 stubs (289), .extracted.md present
├── discovery    right file surfaced                ← embedder / RRF / reranker / coverage
├── acquisition  full answer content reaches model  ← inline excerpt vs file read; CLIP RISK
└── synthesis    rubric-shaped deliverable          ← agent capability — NOT a retrieval lever
```

Cross-component coupling (why single-knob experiments kept failing): the **hint** drives
*verification turns* AND *read order* AND adds to *S*; the **clip layer** couples `oₛ` to
*acquisition accuracy*; `SEARCH_ONLY` couples *discovery* to *catastrophic T blowup* (R1/R2
loops, ANALYSIS §2).

---

## 3. Five-whys-plus chains

### Chain A — why do semfs arms still not beat plain on tokens (post grep-cap fix)?

1. **Why ≥plain?** Run-to-run call-count variance: 5 calls → 76.8K (<plain), 16 calls →
   97.8K (>plain). The extra 11 calls are os.walk crawls + re-greps.
   *Evidence:* RESULTS n=2 table; 22.4K os.walk in breakdown. **Confidence: high.**
2. **Why does the agent crawl despite the top hit being the answer file?** The baked
   `AGENTS.md` commands "read kg/KNOWLEDGE_GRAPH.md FIRST"; the kg-scoped grep returns
   ~138 chars of noise; the "trust the excerpt, don't re-open" hint contradicts rubrics
   that need exact cell values → confidence collapse → crawl.
   *Evidence:* RC3 in infra RCA; F6 in ANALYSIS. **Confidence: high.**
   *What else considered:* codex stochasticity (plain shows 5–14 calls too — but plain's
   variance doesn't include a commanded 35.7K read); FUSE latency (fixed in E1; ruled out).
3. **Why does the seed carry a counterproductive hint?** It was written to advertise
   capabilities and prevent the format trap, compiled into the binary, materialized
   read-only at seed build — for an idealized retrieval quality the local raw-chunk index
   doesn't deliver. *Evidence:* RESULTS "hint is compiled into the binary"; FUSE rm → EPERM.
4. **Why was a prompt component shipped untested?** The harness benchmarks semfs as a
   bundle (arm = mount+hint+KG+search+delivery); no per-component ablation existed before
   E5. *Evidence:* arm ladder in issue.md §2.
5. **Why bundled?** Product thesis "the filesystem IS the interface" treated affordances
   as inseparable from the mount. That is a **convention, not physics** — the hint is a
   ~2KB text artifact that could be versioned, A/B-tested, and rendered per-mount.

**Root cause:** an untested, immutable affordance layer (hint+KG) drives agent *behavior*,
and under `cached_input=0` agent behavior (T × oₛ × position) **is** the cost function.
→ actionable: E7 (hint surgery), E8 (scout stack), KG digest (H5).

### Chain B — why is everyone ≥3.5× above the token floor?

1. **Why?** Every turn re-pays the full context; discovery-probe outputs accumulate early
   and are re-paid (T−s) times. *Evidence:* §1.1 math on 289_plain.
2. **Why early-heavy?** Natural agent order: explore → read → write puts the biggest
   outputs first. Nobody (plain or semfs) orders reads cost-optimally ("late-big").
3. **Why does re-pay happen at all?** `cached_input=0` — the ripbench/OpenRouter endpoint
   serves no prompt cache. In production, cached input is ~10× cheaper (Manus, §RESEARCH).
4. **Why does the metric ignore that?** Benchmark design: raw token totals = the
   *no-cache worst case*. A WB "token win" may not be a production cost win and vice versa.

**Root causes (two):** (i) **delivery/read ordering** is a real, unexploited lever
(early-small/late-big — return paths first, read the answer file once, last);
(ii) **metric design** — report cache-adjusted cost alongside raw tokens (E10).

### Chain C — why is accuracy capped at parity (never above plain)?

1. **Why parity?** On clean infra, semfs surfaces the same files plain finds; identical
   content → identical synthesis → identical rubric score. *Evidence:* 289: 6/15 = 6/15;
   44: 2/16 = 2/16.
2. **Why can't better retrieval lift it?** Discovery is not the bottleneck on these cases:
   44 *names* its files; 289 is corpus-capped (403 stubs); 15/44 have rubric ceilings;
   95 plain already 11/12; 175 plain 8/12. Only 95 has demonstrated retrieval headroom
   (cloud 12/12 via summaries) — coverage-dependent. *Evidence:* RUN_MANIFEST case notes;
   summary RCA "no clean WB case offers that".
3. **Why does WB lack discovery-bottlenecked cases?** 1452 files with filename-semantic
   ground truth in clean txt/csv: `find | grep -Ei` is a near-perfect retriever (ANALYSIS §1).
   The corpus is too small and too well-named to stress discovery.
4. **Why benchmark here at all?** Inherited choice; the adapter ticket
   (`../benchmark-adapter/`) already plans the multi-benchmark escape.

**Root cause:** benchmark–product mismatch. Accuracy headroom above plain barely exists on
WB-chanpin. → actionable: E11 (discovery-stressed case set — unnamed answer files, many
similar siblings, cross-lingual queries) where retrieval CAN win; plus fix 95/175 via
summaries+coverage for the one case that discriminates.

### Counter-analysis (devil's advocate, per the skill)

- *Could variance alone explain Chain A?* n=2 is thin, but the os.walk attribution is a
  direct trace read and the KG-hint mechanism was verified by the 138-char kg-scoped grep.
  Still — every E8+ run uses n≥3.
- *Does the clip layer make grep blobs harmless?* If codex clips hard, capping grep adds
  little token saving (but still wins on *which* bytes survive). E6 resolves this before
  we celebrate the cap fix.
- *Confirmation-bias check:* we want semfs to win. The honest contrary evidence: the PwC
  paper + plain's WB performance both say grep-on-small-well-named-corpora is near-optimal.
  Opinion O8 (OPINIONS.md) takes this seriously instead of burying it.
