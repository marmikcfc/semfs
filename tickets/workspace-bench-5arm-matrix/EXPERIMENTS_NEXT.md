# EXPERIMENTS E6–E14 — validate/invalidate the opinions (2026-06-11)

> Continues E1–E5 ([`ANALYSIS.md`](ANALYSIS.md) → [`RESULTS.md`](RESULTS.md)). Each
> experiment targets a numbered opinion in [`OPINIONS.md`](OPINIONS.md) and was generated
> under the constraints in [`constraint-based-creativity.md`](constraint-based-creativity.md).
> **Protocol for ALL experiments** (lessons already paid for):
> - n≥3 per cell before quoting any number (±30% token / ±1-pt variance).
> - Health-gated driver `run_case_e.sh` (disk guard, quick_check, dummy SM key,
>   .fastembed_cache strip, daemon-inner kill).
> - One artifact dir per run label — never share `<case>_<arm>` dirs (the E3 overwrite trap).
> - Archive per-call breakdowns (`trace_breakdown.py`) and, for cloud arms, the daemon log
>   (`~/.semfs/logs/<tag>.log`) — scores aren't archived otherwise.
> - Record binary md5 + knob env in the results JSONL line.

## Priority order & decision tree

```
E6 (clip calibration, 30min) ──► E7a (hint-render feasibility, 10min)
                                      │
                       ┌──────────────┴───────────────┐
                       ▼                              ▼
              E7b hint ladder ×3            E7c KG-digest arm
                       └──────────────┬───────────────┘
                                      ▼
                         E8 SCOUT STACK (headline, 5 cases × n=3)
                          │ win (≥3/5 both-axes)        │ miss (<2/5)
                          ▼                             ▼
                E9 delivery duel (refine)      O8 path: E11 discovery-stressed
                E14 slice tool (extend)        cases + cross-lingual (re-aim arena)
   Anytime, cheap, parallel: E13 (workspace map)
   Descoped 2026-06-11 (input-tokens-only focus): E10 (caching is automatic API-side) · E12 (output-side)
```

---

### E6 — Calibrate the codex clip layer  *(tests O1's foundation; RUN FIRST)*

The 95_cloud anomaly (830K chars emitted → 138K tokens billed) proves codex truncates tool
output before the model sees it; the 289-compact run (142K-char grep in a 139K-token run)
says the clip is not small/constant. Until measured, every delivery A/B is confounded.

- **Method:** on the box, run codex on a trivial task whose tool calls `cat` controlled
  files of 1K/4K/16K/64K/256K/1M chars (ASCII and CJK variants); read `turn.completed`
  usage + what the model echoes back. No semfs involved.
- **Prediction:** a per-call clip exists in the 8–32K-char band; CJK halves the char budget.
- **Decides:** how much of the grep-cap win is "fewer bytes" vs "better bytes"; whether
  `SEMFS_GREP_RESULT_CAP=6KB` should move (cap just under the clip = semfs chooses the bytes).
- **Kill condition for O1's mechanism:** if codex clips at ≤4K chars, payload knobs above
  4K are dead weight — delivery optimization collapses into "which 4K".
- **Cost:** ~30 min, no code.
- **✅ PARTIALLY ANSWERED FROM DOCS (2026-06-11):** [openai/codex#6426](https://github.com/openai/codex/issues/6426)
  documents the mechanism: **256 lines OR 10 KiB per tool call, whichever first, head+tail
  (128+128 lines, middle dropped)**, applied to exec + MCP tools since v0.56; a ~25K-token
  limit is proposed, not implemented. E6 narrows to: (a) confirm the box's codex version
  behaves per the docs (~10 min); (b) measure the **behavioral vs payload split** of the
  grep-cap win — with blobs always clipped to ≤10 KiB visible, most of 111–145K→76.8K must
  be *turn-count behavior*, not bytes. Design rules adopted now: payloads ≤10 KiB AND
  ≤256 lines; answer-bearing content in the HEAD (head+tail keeps both ends); watch the
  line cap on ranked-path lists.

- **✅✅ RUN (2026-06-11, codex 0.133.0 on the box) — empirical numbers differ from BOTH
  the docs and the prediction.** Marker-file probes (`/tmp/e6/` on the box, traces
  `runA/runB/runCD.jsonl`):
  | probe | size / lines | result |
  |---|---|---|
  | B | 5.4 KB / 300 lines | **passed whole** (⇒ no 256-line cap on this build) |
  | C | 9.8 KB / 200 lines | **passed whole** |
  | D | 15.5 KB / 330 lines | passed with a trailing truncation notice (boundary) |
  | A | 49 KB / 1000 lines | clipped to lines 1–49 + 952–1000 ≈ **~1.2K visible tokens**, notice "…11050 tokens truncated…" (token-denominated) |
  **Design conclusions:** (1) pass-through budget ≈ **≤10 KB is safe**, cliff lands
  ~15 KB+; (2) **overflow is catastrophic, not graceful** — only ~0.5K+0.5K tokens survive
  a clip, so a 12 KB "slightly over" payload loses ~85% of its content silently;
  (3) `SEMFS_GREP_RESULT_CAP=6144` is comfortably inside the window — keep it;
  (4) put answer content first (head survives); (5) the notice is token-denominated —
  this build already implements (a variant of) the proposal in #6426.
  Also observed: `cached_input_tokens > 0` on these codex-exec probes — prompt caching is
  live on this path (the WB harness's `cached_input=0` is endpoint-specific, per the
  scope decision we ignore caching either way).

### E7 — Hint surgery ladder  *(tests O2, O7)*

- **E7a (feasibility, 10 min):** patch one word in `agent_hint.rs`/`render_workspace_root`,
  rebuild, mount the existing clean seed, `cat AGENTS.md` in the mount. Question: does the
  mount re-render the baked hint from the new binary, or is the seed copy authoritative?
  (RESULTS assumed un-fixable; the KG-refresh materialization path suggests otherwise.)
  If seed-authoritative → fallback: test via codex harness-level AGENTS.md in `$WORKDIR`
  (higher-precedence instruction file), which needs no semfs change at all.
- **E7b (ladder, case 289 + 15, n=3 each):**
  | arm | hint |
  |---|---|
  | H-current | baked KG-first + trust-the-excerpt |
  | H-scout | "grep 2–4 key terms → READ the single top hit for exact values → never crawl, never read kg/" |
  | H-zero | no hint at all (the C3 control — never measured) |
- **E7c:** KG as ≤4KB digest (topic→dir table + inaccessible-files list) vs no KG.
- **Prediction (O2):** H-scout cuts ≥25% tokens vs H-current at accuracy ≥ parity;
  H-zero lands between (mount affordances alone are worth something — O8 input).
- **Falsifier:** <10% spread across hint arms → O2 dies, behavior isn't instruction-driven,
  pivot to E13 (affordances) as the behavior lever.
- **Cost:** 1 evening if E7a says binary-rendered; +seed rebuild risk if not.

### E8 — The scout stack (headline)  *(tests O3 — pre-registered kill condition)*

- **Config:** clean seed + `SEARCH_ONLY=off` + `SEMFS_GREP_RESULT_CAP` per E6 +
  `RESULT_LIMIT=3` + H-scout hint (E7 winner) + KG digest-or-off (E7c winner).
- **Run:** all 5 cases × n=3, vs plain's bar (46% @ 89K mean; per-case bars in issue.md §4).
- **Prediction (O3, ~60%):** ≥3/5 cases mean tokens < plain at accuracy ≥ plain−1;
  289 lands ≤70K (the discovery-replacement math in TOKEN_ECONOMY §1.3).
- **Kill condition (pre-committed):** <2/5 cases → **stop optimizing WB-chanpin tokens**;
  declare grep-friendly small corpora out-of-thesis, execute O8 via E11.
- **Cost:** ~1 day of box time (15 runs + reps).

### E9 — Delivery-form duel: capped-inline vs two-tier vs path-first  *(tests O1 vs PwC's warning)*

- **Arms (same scout stack otherwise):** (a) inline 6KB (E8 default); (b) two-tier
  (top-1 hit ~800B excerpt + paths for 2–5 + stop-signal line); (c) path-first 1KB
  (paths + 1-line snippets only — the C1 pure form); (d) **caveman-compressed excerpts**
  (same 6KB budget as (a), but the excerpt prose is telegraphically compressed at render —
  denser facts per surviving byte/line under the codex 10 KiB/256-line clip; verbatim
  numbers/names/versions preserved; spreadsheets exempt — raw rows only).
- **Why:** PwC measured codex collapsing on file-based delivery (93.1→55.2) — (c) flirts
  with that failure mode but `SEARCH_ONLY=off` keeps reads cheap. This decides whether the
  excerpt should carry the answer or just point at it.
- **Prediction:** (b) wins tokens with accuracy parity; (c) loses ≥1 rubric point on
  exact-value cases (15/44) — matching the paper.
- **Cost:** 2 cases (289, 15) × 3 arms × n=3.

### E10 — Cache-adjusted re-scoring  *(tests O5; analysis-only, zero runs)*

> **⏸ DESCOPED 2026-06-11 (user decision):** focus is INPUT tokens only. OpenAI prompt
> caching is automatic at the API layer (https://developers.openai.com/api/docs/guides/prompt-caching)
> — no engineering action on our side; raw input tokens remain the benchmark metric.
> Revisit only if a production-cost story is needed later.

- **Method:** re-price every archived run: `cost = unique_input×1.0 + repeated_input×0.1 +
  output×4` (approximating production cache pricing). Repeated-input is reconstructable
  per-turn from the traces (context prefix = scaffold + prior turns).
- **Prediction:** high-turn arms' penalty shrinks ~5–8×; KG/blob arms keep theirs (unique
  bytes don't cache-discount); at least one arm ordering flips.
- **Falsifier:** all orderings unchanged → drop the metric complaint, raw tokens stand.
- **Cost:** ~2h scripting against existing JSONLs/traces.

### E11 — Discovery-stressed case set (+ the only valid summary test)  *(tests O4, O8)*

- **Method:** author 3–5 new WB-style cases on the existing chanpin corpus where the
  answer file is **not named** in the task and lives among ~200 similar xlsx ("find the
  sheet that tracks Q3 returns for apparel" style), incl. ≥1 **cross-lingual** task
  (zh task ↔ en filenames — grep cannot bridge this; semfs rewrite is proven #417→#1).
  Arms: plain / scout-stack raw / scout-stack + dual-store summaries (the 2026-06-11 fix:
  summary FINDS, `.extracted.md` raw table ANSWERS).
- **Prediction (O4/O8):** plain's find|grep degrades hard (filename semantics gone);
  summary arm beats raw arm by ≥2 rubric points on ≥2 cases; cross-lingual case is a
  clean semfs win on BOTH axes.
- **Falsifier:** summary ≤ raw on these cases kills O4; semfs ≤ plain here kills the
  discovery half of O8 (the problem would be deeper than arena choice).
- **Cost:** 1 day authoring + rubrics, 1 day runs. Highest strategic value per O8.

### E12 — Caveman output-compression A/B  *(bounds O6; existing ticket)*

> **⏸ DEPRIORITIZED 2026-06-11 (user decision):** caveman targets the OUTPUT-token stream;
> current focus is input tokens only. Keep the ticket; don't schedule runs for it now.

- **Method:** per `tickets/caveman-agent-output-compression/issue.md` — env-gated hint
  section; 2 cases × n=3 on the scout stack.
- **Prediction (O6):** ≤8% total-token delta on WB; accuracy unchanged.
- **Falsifier:** >10% delta → output stream bigger than traces show; promote it.
- **Cost:** small (hint-section + reruns) once E7a establishes the hint path.

### E13 — ≤1KB workspace map  *(O2's alternative if hints underperform; C2/C3 child)*

- **Method:** inject a ~1KB map (top-level dirs, file-type census, "where things live"
  one-liners) into the hint/AGENTS.md; optionally per-dir 3-line README manifests.
  Measure discovery turns (find/ls/grep-probe count) vs without.
- **Prediction:** discovery probes 4–8 → 1–2 on plain-like behavior; helps *both* arms
  (it's retrieval-free — pure S-for-T trade: +1KB scaffold to delete ~2 turns).
- **Falsifier:** probe count unchanged → maps don't steer codex; drop.
- **Cost:** half-day.

### E14 — `semfs slice` / artifact pattern prototype  *(O1 extension; gated on E9)*

- **Method:** `semfs grep` stores the full ranked result, returns ≤1KB + a result-id;
  new `semfs slice <id> --file N --range a:b | --grep term` retrieves precise pieces
  (Firetiger artifacts: 94% one-shot; Anthropic code-exec: filter before context, −98.7%).
- **Gate:** build only if E9 shows path-first/two-tier ≥ inline on tokens without accuracy
  loss — slice is the affordance that makes small-payload delivery safe.
- **Prediction:** the 289-class QA flow becomes search(1KB) → slice(2KB) → write ≈ 25–30K
  tokens — the first floor-adjacent run (TOKEN_ECONOMY §1.3).
- **Cost:** the only real code investment on this list; scope after E9.

### E15 — Caveman-compressed `.extracted.md` siblings for prose documents  *(input-side caveman; proposed 2026-06-11; gated on E6 + one synthesis-case pilot)*

> Distinct from the parked E12: E12 compresses what the agent *writes* (output stream);
> E15 compresses what the agent *reads* (input stream) — squarely in scope.

- **Idea (user, 2026-06-11):** for **non-spreadsheet** documents (docx/pdf/html prose —
  anything whose readable form semfs materializes), store a caveman-compressed rendition
  (~35–65% of original; telegraphic but fact-preserving) so a single clipped read carries
  the whole document. Spreadsheets stay EXEMPT — the dual-store lesson stands: rubrics
  need exact rows; tables are returned verbatim, never compressed.
- **Why the clip makes this matter:** codex clips every read at 10 KiB/256 lines head+tail.
  A 30 KB prose doc loses its middle on every `cat`; compressed to ~12 KB it nearly fits —
  the agent gets the *whole* document in one clipped read instead of paging with `sed`
  (each page = a new turn that re-pays all context).
- **Where it should win / lose:** wins on synthesis cases that need whole-document
  understanding (95/175-shape); should NOT help (and must not hurt) localized-answer QA
  (289-shape) where selection beats compression. Plain-text corpus files are NOT mutated —
  compression lives in semfs-materialized renditions only; the original stays readable.
- **Risks (pre-registered):** (1) compression drops an exact value a rubric needs — the
  format-trap failure reborn; mitigation: instruct verbatim preservation of all numbers/
  names/versions/dates + spot-check 10 docs before any run. (2) Index-time LLM pass cost +
  the summary-seed silent-drop failure mode — dual-store discipline mandatory (original
  recoverable). (3) Embedding the compressed text changes retrieval — decouple: embed
  unchanged, compress only the returned rendition.
- **Prediction:** on a 95-style case, acquisition tokens drop 30–50% with accuracy held;
  **kill condition:** ANY exact-value rubric regression vs raw siblings on the pilot case,
  or E9(d) showing compressed excerpts already losing accuracy at the delivery layer.
- **Cost:** prompt + render-path change + pilot reruns; do AFTER E9 signals (d) is safe.

---

## Opinion ↔ experiment map

| Opinion | Credence | Validated/killed by |
|---|---|---|
| O1 delivery form = #1 token lever | 85% | E6 (mechanism), E9, E14, E15 |
| O2 hint outweighs the pipeline | 80% | E7a/b, E13 (alternative) |
| O3 scout stack beats plain both-axes ≥3/5 | 60% | **E8 (pre-registered kill)** |
| O4 summaries = accuracy-only lever, unmeasurable on current cases | 75% | E11 |
| O5 cache-blind metric distorts | 70% | E10 |
| O6 caveman ≤8% on WB | 70% | E12 |
| O7 KG/by-topic net-negative; digest-or-kill | 85% | E7c |
| O8 wrong arena; win = format-trap + cross-lingual + discovery-stress | 70% | E8 miss + E11 |
