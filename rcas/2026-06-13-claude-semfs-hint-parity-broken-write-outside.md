# RCA: Claude does NOT get codex-equivalent semfs instructions (write-outside breaks the Claude harness)

Status: RESOLVED 2026-06-13 (option B shipped + verified) · Found during E2B WB-PM matrix prep
Trigger: user asked "did we ensure Claude gets similar instructions like codex on semfs?"

## RESOLUTION (option B — patch ClaudeCode.js, codex untouched)
Patched `baselines/ClaudeCode.js` so the semfs kit is driven by an explicit env, not by
cwd-under-mount: (1) `SEMFS_MOUNT_PATH` sets the mount independent of cwd; (2) the project
`CLAUDE.md` is written to **cwd** (work_dir), not into the mount — SDK loads it via
`settingSources:['project']`, no leak; (3) the canUseTool guard now allows READ/SEARCH of the
mount (`isReadable`) while WRITES stay confined to cwd; (4) the rg/grep shim activates from
`SEMFS_SHIM_DIR`. cell_driver exports `SEMFS_MOUNT_PATH`, `SEMFS_SHIM_DIR`, `SEMFS_BIN`,
`SEMFS_REAL_RG`, `SEMFS_REAL_HOME` for semfs arms — codex.py ignores all of them.

**Verified (sandbox ilvaul7umi64dy798jooj, SEARCH_ONLY=off, native auth, conc=1):**
- Claude: `[semfs] wrote project CLAUDE.md` + `shim enabled`, ZERO `[deny]` lines, used
  `semfs grep` (1 clean hit, found the target files), real xlsx deliverable. status=ok.
- codex: unchanged — status=ok, 6× `semfs grep`, real xlsx. NOT broken.
- `SEARCH_ONLY=off` peak RSS 6558 MB / min-free 111 MB → fits 8 GB at conc=1 (principle honored).
- Residual: Claude total 912K is 90% cache_read from an `openpyxl`-dependency hunt (7 pip/find
  turns), NOT semfs (only 19.6K completion, 1 grep). Fix = bake openpyxl/xlsxwriter/pandas/
  python-docx into the image (done in `/tmp/e2b_build_baked.py`) so neither agent hunts deps.

## Verdict
**No.** The two agents' semfs-instruction channels are asymmetric, and the current
**write-outside** cell-driver design (cwd = `/home/user/run/<label>`, mount at
`/home/user/ws/mnt`) silently disables ALL of Claude's purpose-built semfs affordance —
and, worse, the Claude harness's permission guard DENIES Claude from touching the mount
at all.

## The two instruction channels, per agent

| channel | codex | Claude (write-outside, current) |
|---|---|---|
| in-prompt `SEMFS_HINT` (cell_driver `wrapped`) | ✅ gets it | ✅ gets it (identical text) |
| home-level config hint (`~/.codex/AGENTS.md` / `~/.claude/CLAUDE.md`) | ✅ `~/.codex/AGENTS.md` written at mount (parent dir exists) AND codex auto-loads it regardless of cwd | ❌ `~/.claude` didn't exist at mount → not written; **AND the SDK ignores user-level `~/.claude/CLAUDE.md` anyway** (settingSources default `[]`) |
| project `CLAUDE.md` + `settingSources:['project']` + claude_code preset | n/a | ❌ ClaudeCode.js writes it ONLY when cwd is under the mount → **never fires** (cwd is outside) |
| rg/grep shim (native Grep tool → `semfs grep`) | n/a (codex calls `semfs grep` from the hint) | ❌ enabled ONLY when cwd under mount → **never fires**; Claude's native Grep/Glob crawl literally |
| can Claude even READ the mount? | codex `--cd work_dir`, sandbox=danger-full-access → reads mount fine | ❌ **canUseTool DENIES** Read/Grep/LS/Glob/Bash for any path outside cwd → all access to `/home/user/ws/mnt` is denied |

## Root cause (grounded in code)
- `crates/semfs-core/src/agent_hint.rs:217-234` `install()`: writes hint to an agent file only
  `if parent.exists()`. `~/.codex/` existed (we wrote auth.json) → `AGENTS.md` written. `~/.claude/`
  did NOT exist → `CLAUDE.md` skipped. (Mount log confirmed: "Updated ~/.codex/AGENTS.md" only.)
- `benchmarks/.../baselines/ClaudeCode.js:281-330`: the entire semfs delivery (project CLAUDE.md
  line 313, `settingSources:['project']` + claude_code preset line 339, rg/grep shim lines 323-328)
  is gated on `if (mountPath)`, where `mountPath` is found by walking UP from cwd for a `.semfs`
  marker whose `mount_path=` CONTAINS cwd. With cwd=`/home/user/run/<label>` (write-outside),
  cwd is NOT under `/home/user/ws/mnt` → `mountPath = null` → nothing fires.
- `ClaudeCode.js:309-311` (author's own comment): "the SDK loads CLAUDE.md ONLY with
  settingSources:['project']; **it IGNORES the user-level ~/.claude/CLAUDE.md that semfs writes**."
  So semfs's home-level Claude delivery is structurally dead for the SDK.
- `ClaudeCode.js:253-270, 356-371` canUseTool/`isUnderCwd`: denies file tools and Bash commands
  referencing absolute paths outside cwd. Write-outside ⇒ the mount is outside cwd ⇒ denied.

## Why the morning run (09:53) still produced output
That run PRE-DATES the write-outside change (`update_driver_v4.py` @ 10:43). Its cwd was the
mount, so `mountPath` was detected → Claude got the full affordance (project CLAUDE.md + shim)
and could access the tree. It still flailed (61–74 calls, 1.2–2.0M tokens, 93–95% cache_read) —
but that flail was `SEARCH_ONLY=on` (empty-looking dirs), a SEPARATE issue. The current
write-outside driver would break Claude in a NEW way: outright mount-access denial.

## Net
- codex: in-prompt hint + auto-loaded `~/.codex/AGENTS.md`, full mount access → properly instructed.
- Claude (current write-outside): in-prompt hint ONLY, no project CLAUDE.md, no grep shim, and the
  mount is access-DENIED by the harness. Not parity; not even runnable as intended.

## Fix options
- **A. cwd = mount (harness's intended design).** Restores project CLAUDE.md + shim + access.
  Cost: writes land in the mount (the leak write-outside was meant to avoid) → remount between
  cells to reload the DB. Symmetric-ish to codex.
- **B. Patch ClaudeCode.js for explicit `SEMFS_MOUNT_PATH`** (cwd-independent, mirrors how codex's
  AGENTS.md works): when set, write project CLAUDE.md into cwd, set settingSources+preset, enable
  the shim pointed at the mount, AND add the mount path to the canUseTool read/search allowlist
  (writes still confined to work_dir). Preserves write-outside + gives parity + keeps the shim.
- **C. MCP tool for `semfs grep`** (the durable fix per memory `semfs-claude-affordance`: "Claude
  ignores the hint; fix = MCP tool, not a hint"). Largest change; best Claude compliance.

## Refs
- `crates/semfs-core/src/agent_hint.rs:217-234,58-79`
- `benchmarks/vendor/Workspace-Bench/evaluation/baselines/ClaudeCode.js:281-339,253-271,356-371`
- `benchmarks/vendor/Workspace-Bench/evaluation/src/agents/claudecode.py:225-246` (sets cwd=work_dir)
- morning artifacts: `/tmp/e2b_matrix/artifacts/pm_claude_15_{plain,nokg,nokgAK}_r1/result.json`
- memory: `semfs-claude-affordance`, `confidence-adaptive-delivery-direction`
