# Ticket: Evaluate `caveman` for OUTPUT-token compression

**Folder:** `tickets/caveman-output-compression/`
**Upstream:** https://github.com/juliusbrussee/caveman (a Claude Code / Codex / 30+ agent **skill**)
**Routing:** mirror to Linear (team `SemFS`, key `SEM`) per CLAUDE.md §0.

## What caveman is (and how it differs from semfs compress)

caveman is an **OUTPUT**-token compressor: a skill that makes the *agent* write more tersely
(telegraphic style — drop filler/preamble/conjunctions, sentence fragments, **keep code/commands/
errors/IDs/numbers verbatim**). It does NOT touch input or reasoning tokens. Levels: `lite`,
`full`, `ultra`, `wenyan` (classical Chinese). Reported: **65% avg output-token reduction
(22–87%), 100% technical accuracy, ~3× speed** across 10 tasks.

This is **orthogonal** to semfs's `SEMFS_GREP_COMPRESS`, which compresses **input** (grep excerpts
before they enter the agent's context). The two could stack: caveman trims the agent's output,
semfs trims the agent's input.

## Why it might (or might not) help OUR benchmark — read before trusting the result

Our WB token metric is **total (input + output)**, and in **headless** codex tool-use the tokens
are **input-dominated** (re-fed context + grep blobs); the agent emits little prose. caveman targets
output prose, so its effect here may be **much smaller than its 65% chatty-session claim** — and
there's an accuracy risk if terseness bleeds into the graded deliverable. The 65% figure is for
interactive/chatty sessions, not headless agents. Judge the result on that basis.

## Eval plan

1. **Proper test (preferred):** install the real caveman skill into the codex agent in the E2B
   sandbox (one-line install), run cases, measure output-token delta + accuracy. Integration work
   (ensure codex loads + auto-activates the skill headlessly).
2. **Approximation (fast, now):** inject a caveman-STYLE output-terseness directive into the agent
   prompt (knob `caveman.json` = best config + "be telegraphic in your OWN messages; keep the
   deliverable, IDs, numbers, dates VERBATIM"). Tests the *idea*, not the tuned skill.
3. **Stacking:** caveman (output) + semfs compress (input) together — the full token-reduction stack.

## Success criteria

- [ ] Measured **output-token** reduction vs prompt-only baseline (isolate output from input).
- [ ] **No accuracy regression** (the deliverable stays complete — caveman must not terse the graded file).
- [ ] Honest read: report whether the input-dominated headless setting makes caveman material here.
- [ ] If material: consider shipping the directive in the semfs affordance; if not: note it's an
      interactive-session win, not a headless-agent one.

## First test (this run)

Approximation arm (#2) added to the PM-10 n=2 validation run: `prompt-only + caveman-style terseness`,
compared against plain / prompt-only / compress+dedup+prompt. Real-skill test (#1) is a follow-up.
