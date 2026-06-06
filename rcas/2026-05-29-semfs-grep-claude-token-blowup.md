# RCA: semfs grep made Claude's tokens explode (1.23M) while shrinking codex's (35K)

**Date:** 2026-05-29
**Host:** ubuntu@13.201.35.159 (EC2 i-0c491c7cc23de8555)
**Scope:** Workspace-Bench case 289 (PM/chanpin). Local-model smoke runs.

## Symptom
With semfs, **Claude used 6× MORE tokens than without** (semfs-cloud 1,229,994 vs plain 206,941; semfs-local 422,668). Yet **codex used 4× FEWER** with semfs (35,763 vs 143,837). Same `semfs grep` tool, opposite outcomes.

## Evidence (from the transcripts)
claude-cloud token composition: **cache_read 1,170,780 (95%)**, cache_creation 52,527, output **6,659**, input 28. The agent *generated* almost nothing — 95% is its own context replaying every turn.

Tool-output attribution (claude-cloud):
- **`Read` = 84,781 chars (87% of tool output)** — two reads of **47,651 + 36,483 chars**.
- **`semfs grep` = 2,281 chars (~570 tokens) — terse.**

codex-local contrast: **554 chars total tool output, ZERO raw-file reads → 34,945 tokens.**

## Root cause (NOT what it looked like)
**It is not semfs grep.** Its output was tiny (570 tokens). The blow-up is Claude **`Read`-ing the raw files semfs grep pointed at** — which are the **HTML-disguised `.xlsx`** (the format trap), ~40K chars each. In an agent loop `cache_read ≈ (bytes in context) × (turns they survive)`, so those two big reads landed early and replayed ~22 turns → 1.17M cache_read.

- **codex** consumed the grep excerpt and **never opened a file** (search REPLACED reading) → 35K.
- **claude** distrusted the excerpt and **Read whole raw files on top of** the grep (search ADDED to reading) → paid search + reads + extra turns → 1.23M.
- The **partial 19% index** amplified it: weak hits reinforced "verify by reading the real file," and the real file is 40K of HTML.

## The principle
In an agent loop the expensive op is **READING, not searching** (reads replay × turns in cache_read). Semantic search pays off **only if it REPLACES file reading**; if the agent re-reads the source, search is **pure additive overhead → worse than no search**.

## Is semantic grep wrong? No.
codex proves the win is real (−76%). The thesis sharpens: the goal isn't *better search*, it's **fewer bytes read**. Search must make reading the source **unnecessary** (excerpt = answer) or **cheap** (normalized content) — not merely point at files.

## Fixes (ranked)
1. **Serve normalized content on the FS read path** (semfs) — a `Read` of a binary/HTML file returns its transcription (~2K clean) not 40K raw. Highest leverage: shrinks the read ~20× *regardless of agent trust*. Defends against the behavior instead of training it away.
2. **Line-scoped reads** (bench steer) — grep returns `file:lineA-lineB`; read only those lines. The grep header already instructs this (`grep.rs:482`); Claude ignored it.
3. **Full index (pre-warm) + parallel embed** — strong hits → trust → stop reading raw.
4. **Cap grep top-k** (semfs, grep.rs:488 prints uncapped ~80 files) — latent risk, not this run's cause.
5. **Make the excerpt sufficient** + steer "the excerpt is the answer" (codex's pattern).

## Corrections logged
- Earlier hypothesis "semfs grep dumps verbose chunks" was **wrong** — grep output was terse (2,281 chars). Verified by per-tool output attribution.
- Report: `benchmarks/workspace_bench/semfs_token_blowup_rca.html`. Related: [[semfs-claude-affordance]], the local-indexing starvation RCA, the 19%-coverage finding.
