# Research: Agent Filesystem Isolation
_Date: 2026-06-09 | 110 agents, 27 sources, 25 claims verified (17 confirmed / 8 killed)_

## The Problem

When an AI coding agent (Codex, Claude Code) runs inside a semfs-mounted workspace, it can
read the entire host filesystem — including `~/.codex/AGENTS.md` and `~/.claude/CLAUDE.md`.
This causes:
1. **Duplicate instructions** — home-level semfs hints + workspace-root AGENTS.md both read
2. **Stale/conflicting context** — user's old home-level config bleeds into the session
3. **Wasted tokens** — home-level block re-sent every turn (codex re-injects it per-turn)
4. **No containment** — agent can wander outside the workspace

## How Production Agents Solve This Today

### Linux: bubblewrap (bwrap) — used by Codex CLI and Claude Code

Both Codex CLI and Claude Code use **bubblewrap** on Linux. The mechanism is Linux
unprivileged user namespaces (`CLONE_NEWUSER`) — no root, no setuid required.

**Their actual bwrap pattern:**
```bash
bwrap \
  --ro-bind / /              # entire host filesystem: READ-ONLY
  --bind <workspace> <workspace>  # workspace: writable
  --ro-bind .git .git        # re-pin .git as read-only inside workspace
  --ro-bind .codex .codex    # same for .codex, .agents
  --unshare-user --unshare-pid
  -- <agent>
```

**Critical limitation:** `~/.codex/AGENTS.md` and `~/.claude/CLAUDE.md` are still
**readable** — the whole filesystem is mounted read-only but accessible. Their goal
is write containment, not read isolation. This does NOT solve our problem.

Codex bundles its own `bwrap` binary (v0.11.2) as a fallback if not on PATH.

### macOS: Apple Seatbelt (sandbox-exec)

Both tools use `sandbox-exec` with dynamically generated SBPL policies.
**Problem:** `sandbox-exec` is officially **deprecated** in macOS 15.4+. Also, by design
"read access cannot be restricted — sandboxed processes always have full disk read."
CVE-2025-59532 was an active bypass patched only in Codex v0.39.0.

### Claude Code default policy
> "Default read behavior: read access to the **entire computer**, except certain denied
> directories. Note that this default still allows reading credential files such as
> `~/.aws/credentials` and `~/.ssh/`."  
> — Official Claude Code sandboxing docs

`denyRead` is unreliable: it blocks shell-based reads but not Claude Code's built-in
Read tool (GitHub issue #32226, closed as not-planned).

## Confirmed Findings (Adversarially Verified, 3-0 vote unless noted)

| # | Claim | Vote |
|---|---|---|
| 1 | bwrap is the canonical unprivileged Linux sandbox, uses `CLONE_NEWUSER` | 3-0 ✓ |
| 2 | Both Codex + Claude Code do `--ro-bind / /` + `--bind workspace workspace` | 3-0 ✓ |
| 3 | Codex bundles bwrap 0.11.2 as fallback if not on PATH | 3-0 ✓ |
| 4 | macOS: both tools use Seatbelt (`sandbox-exec`) with per-invocation SBPL policies | 3-0 ✓ |
| 5 | Claude Code default = full filesystem read, writes restricted to cwd | 3-0 ✓ |
| 6 | Ubuntu 24.04+ AppArmor blocks unprivileged user namespaces by default | 3-0 ✓ |
| 7 | Sandlock (2026): Landlock LSM + seccomp-BPF, no root/cgroups/containers | 3-0 ✓ |
| 8 | Protected paths (`.git`, `.codex`, `.agents`) re-pinned read-only inside writable roots | 2-1 ✓ |

## Killed Claims (Refuted, 0-3 or 1-2)

| Claim | Why killed |
|---|---|
| "bwrap creates empty tmpfs, invisible to host" | bwrap gives a VIEW of host filesystem via mount namespaces, not a separate rootfs |
| "`--unshare-user` + `--unshare-pid` = full isolation" | Namespaces alone don't restrict reads of `~/.codex/` |
| "Isolation applies to all subprocesses" | Main Codex/Claude process runs unsandboxed; sandbox wraps tool calls only |
| "denyRead blocks Claude Code's Read tool" | Only blocks shell-based reads, not the built-in Read tool |
| "Codex sandbox restricts reads to workspace" | Read access is full-disk by default; writable_roots is write-only config |

## What Actually Solves Our Problem

**Minimal bwrap mount** — not what Codex/Claude Code do, but how bwrap *can* be used:

```bash
bwrap \
  --bind /abc/pqr/lmnop /    # workspace IS /, nothing else mounted
  --proc /proc \
  --dev /dev \
  --setenv HOME / \
  -- codex "your task"
```

Inside this process:
- `pwd` → `/`
- `ls ~` → workspace root
- `cat ~/.codex/AGENTS.md` → `No such file or directory`
- Agent physically cannot read anything outside the workspace

**Prior art:** `CaptainMcCrank/SandboxedClaudeCode` community hardening script does exactly
this — mounts only `~/.claude` and `$PWD`, leaves everything else unmounted.

## Ubuntu 24.04 Caveat (EC2 check)

```bash
unshare --user --mount true && echo "namespaces OK" || echo "BLOCKED"
```

Amazon Linux 2023 and Ubuntu ≤22.04: OK. Ubuntu 24.04+: blocked by AppArmor default.
Fix: add `/etc/apparmor.d/bwrap` granting `userns` capability to `/usr/bin/bwrap`.

## Emerging Alternative: Sandlock (2026)

- `github.com/multikernel/sandlock` — ASPLOS 2026 Agentic OS Workshop paper
- Uses **Linux Landlock LSM** (in kernel since 5.13) + seccomp-BPF
- Can enforce **read restrictions** (unlike bwrap's default)
- No mount namespace, no root, no containers
- Requires Linux **6.12+** for full ABI v6 — EC2 probably not there yet
- Not production-hardened yet but the right long-term direction

## Decision for semfs

| Approach | Solves our problem | Root needed | EC2 today |
|---|---|---|---|
| Codex/Claude default bwrap (`--ro-bind / /`) | ❌ reads everything | No | ✅ |
| **Minimal bwrap** (`--bind workspace /`) | ✅ | No | ✅ |
| Landlock/Sandlock | ✅ | No | ⚠️ needs 6.12+ |

**Recommended:** wrap codex spawn in `semfscodex.py` with minimal bwrap.
The home-level `install()`/`uninstall()` in `agent_hint.rs` can then be deleted —
the home-level files are physically absent from the agent's namespace.

## Open Questions

1. Does wrapping codex spawn in bwrap break codex's own internal bwrap sandbox
   (nested namespaces)? Likely fine on Linux but needs a test run.
2. Does Codex need to read any files outside the workspace to function (auth tokens,
   config, model endpoint)? If yes, those paths need explicit `--ro-bind` in the
   minimal mount list.
3. Can we use Landlock directly in the semfs daemon (Rust, kernel 6.12+) instead of
   wrapping the agent externally?
