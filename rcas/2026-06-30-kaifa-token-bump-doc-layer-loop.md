# RCA 2026-06-30 — kaifa next_plaid token bump: agent loops in the doc layer + unsurfaced prefix-cache

**Component:** benchmarks/e2b cell_driver (agent behavior) + GLM-vLLM↔codex usage accounting · next-plaid-late-interaction (kaifa-C arm) · **Severity:** medium (token blowup; distorts experiment interpretation, no data loss) · **Status:** diagnosed — levers identified, no fix shipped

## Symptom
`next_plaid_kaifa_C` (n=1, 11 cells) burned **7.86M reported tokens**, ~2× the per-cell rate of houqin. The spend is wildly skewed: **case 226 alone = 4.04M tokens / 260 calls → 0%** (51% of the arm's entire token budget on one failed cell). Cases 311 (1.29M) and 242/300 (0.55–0.66M) are secondary blowups.

## Root cause (four layers)

**1. The bump is re-sent INPUT, not generation.** Per-cell usage decomposition: **98.6% of tokens are `prompt_tokens`, 1.4% completion.** The agent isn't writing — it's re-sending a growing transcript.

**2. No prefix-cache is surfaced → the harness bills the full prompt every turn.** `cache_read = 0` on every cell. Cross-referenced with the prior finding (memory: GLM-vLLM prefix-cache / codex integration): the vLLM *engine* prefix-caches ~96.8%, but that hit-rate is **not surfaced to codex's usage accounting**, so the harness counts the full (growing) prompt per turn. Consequence: each turn's re-sent context is billed in full → tokens accumulate ~quadratically in turn count. **Real compute is ~30× less than the reported number** — but the under-reporting is uniform across all arms, so *relative* comparisons still hold; only the absolute "4M" is mostly cache the engine already had.

**3. The upstream driver is the agent looping/crawling, not generation or retrieval volume.** Case 226 trace (178 `cat` reads):
- **87 unique files read, but 178 total reads → 91 RE-READS.** The prompt explicitly says *"Do NOT repeat a search you already ran"*; the agent re-read already-seen files **91 times.** It looped instead of progressing.
- **85 of 87 unique reads are docs (`.md`); ~0 are code.** Plus 70 `ls`/`find` directory walks, only 18 `grep` searches.
- The task is *"identify the bugs **in the code** … generate bug_report.txt"* — yet the agent **never read the code in `project-code/`.** It read meeting minutes, work task lists, OKR syncs, code-review *guidelines*, ADRs. It scored 0% because it never did the actual work.

**4. Why the agent got stuck in the doc layer.** "Which files have not been debugged" is **not a semantic-search question** — it's an enumeration over scattered project state (task lists, meeting notes imply what's pending debug). So the agent searched "not debugged"; colgrep's **doc lane (LFM2) kept returning more meeting-notes/task-lists that *mention* debugging**, each one another doc to read, reinforcing the loop. The **code lane (LateOn-Code) results went unread** — the dual-model surfaced code, but the agent stayed in docs chasing the unanswerable "which files" question.

**Why next_plaid is *worse* than PPR here (4.0M vs PPR's ~1.2M on 226):** the richer doc-lane retrieval gives the agent *more* plausible docs to chase, deepening the loop. Better doc recall = a deeper rabbit hole on an enumeration task.

## Evidence
- `tickets/next-plaid-late-interaction/artifacts/kaifa_glm/pm_codex_226_next_plaid_kaifa_C_r1/result.json` — `tokens=4,039,225 prompt=4,026,119 completion=13,106 cache_read=0 calls=260`.
- Trace `…/pm_codex_226_…/full.tgz → codex_stdout.jsonl`: 178 cat (87 uniq / 91 repeat), 85/87 docs, 70 ls/find, 18 grep.
- Task text (agent.json executionTrace user turn): *"Based on the files that have not been debugged, identify the bugs in the code and generate a bug report…"*

## Levers (no fix shipped yet, in impact order)
1. **Surface the engine prefix-cache hit to codex's usage accounting.** Biggest, and it's a *harness/accounting* fix, not a retrieval fix — it collapses the reported quadratic (the re-sent context is already cached engine-side). Track the real fresh tokens, not the re-billed prompt.
2. **Enforce no-repeat / stateless context dedup.** The agent re-read 91×, ignoring the prompt hint. A hard dedup (don't re-serve already-read content) caps the transcript growth. Ref: ticket `grep-stateless-context-dedup`.
3. **Enumeration/discovery tasks need a non-semantic affordance.** colgrep (any semantic retriever) cannot answer "enumerate files by project state." On these tasks the agent will always fall back to crawling; the workspace map / a directory-structured affordance is the right tool, not late-interaction search.

## Lessons
1. **A "token bump" is two different things** — a real behavioral driver (the agent crawling/looping, arm-dependent) and a measurement amplifier (unsurfaced cache, ~30× inflation, arm-independent). Separate them before quoting absolute numbers.
2. **Better retrieval can make discovery tasks *worse*** — higher doc-lane recall deepened the loop. Recall is not universally good; for enumeration it feeds the rabbit hole.
3. **The agent ignored its own "no broad crawl / no repeat" instructions** — prompt hints don't bind; dedup must be enforced in the harness, not requested in the prompt (consistent with the semfs-claude-affordance finding that hints get ignored).
