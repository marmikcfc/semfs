# RCA: semfs gives Claude Code no benefit — it never uses semantic search

**Date:** 2026-05-27
**Host:** ubuntu@13.201.35.159 (EC2 i-0c491c7cc23de8555)
**Scope:** Workspace-Bench case 289 (chanpin), semfs-claudecode vs semfs-codex.

## Symptom
semfs cut **codex** tokens ~75% (it called `search_v4`), but **Claude Code** got
**no benefit** — token totals ≈ plain Claude, and `search_v4` never fired. Across three
semfs-claude runs Claude only ever crawled (`ls`/`find`/`Glob`/ripgrep-`Grep`/`Read`):

| run | tokens (cache-incl.) | tool calls | `semfs grep` | notes |
|----|---:|---:|---:|---|
| 4 | 279,564 | 59 | 0 | no hint delivered |
| 5 | 154,431 | 40 | 0 | hint code present but marker-path bug → not delivered |
| 6 | 356,827 | 21 | 0 | **hint delivered (confirmed), still ignored** |

semfs-codex, by contrast: ran `semfs grep` ~12× → `search_v4` → −75%.

## Why codex worked and Claude didn't (two independent layers)

**Layer 1 — delivery.** `agent_hint.rs` writes the *"use `semfs grep`…"* block to home-level
`~/.codex/AGENTS.md` / `~/.claude/CLAUDE.md` on mount.
- **Codex**: runner keeps real `HOME`; the Codex CLI loads `~/.codex/AGENTS.md` → hint reached it.
- **Claude**: `ClaudeCode.js:228` sets `HOME=<workdir>`, **and** the Claude Agent SDK (v0.2.107)
  defaults `settingSources: []` → loads **no** `CLAUDE.md` at all. So the home-level hint was
  orphaned (run 4/5 transcripts: 0 occurrences of `semfs grep`).

**Layer 2 — compliance.** Fixed delivery by injecting the hint via the SDK's `appendSystemPrompt`
(HOME- and settingSources-independent), gated on the `.semfs` marker. Note: semfs writes that
marker to the **parent** of the mount (`daemon_runtime.rs:71`, `mount_path.parent().join(".semfs")`)
because it can't drop a plain file inside its own FUSE fs — so detection must **walk up** from cwd
(mirroring `read_semfs_marker_for_path`), not check the mount root. After that fix, run 6 stderr
confirms delivery (`[semfs] semantic-search hint injected … (mount=…)`), **0 permission denials**,
yet Claude **still ran 0 `semfs grep`** and crawled with native tools.

## Root cause
1. (fixed) Delivery: the file-based hint never loaded under the SDK; use `appendSystemPrompt`.
2. (open, the real blocker) **Affordance mismatch.** semfs exposes semantic search as a **shell
   command** (`semfs grep`) + a zsh `grep` wrapper. Claude Code is a **tool-driven** loop whose
   native `Grep`/`Glob`/`Read` tools (a) bypass the shell wrapper entirely (in-process ripgrep) and
   (b) out-compete a prose instruction to shell out. A system-prompt steer is "a steer, not a
   contract" (per `agent_hint.rs`'s own docstring) and loses to native tools.

## Fix (recommended, not yet done)
Put semantic search in the **same affordance class as `Grep`**:
- **Best:** expose it as an **MCP tool** (`semantic_search(query)`) — Claude calls provided tools
  natively. (`ClaudeCode.js` `query()` supports `mcpServers`.)
- **Forcing alternative:** `disallowedTools: ['Grep','Glob']` + deny `find`/`grep`/`rg` in
  `canUseTool`, so `semfs grep` is the only search path. Heavy-handed; changes agent capability.
- A **virtual search path** (`.semfs/search/<q>`) does NOT fix this — it addresses *discovery*, but
  the blocker is *compliance*; Claude was told and ignored it.

## Also fixed this session (adjacent)
- Claude token capture was undercounting ~50× — `_parse_usage_from_stdout` dropped
  `cache_read_input_tokens`/`cache_creation_input_tokens` (see the token-accounting work).
- Added `SEMFS_FRESH=1` to `run_workspace_bench.sh` to wipe cache + output between runs.

## Status
`appendSystemPrompt` delivery fix is committed to `ClaudeCode.js` (local + EC2). The compliance
fix (MCP tool / disallowedTools) is the next decision point.
