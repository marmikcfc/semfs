---
description: Clean teardown before removing tokopt — unwire ANTHROPIC_BASE_URL, stop the proxy, wipe state
allowed-tools: Bash(python3:*)
---
Tear tokopt down for uninstall by running its helper **with the Bash tool** (not inline), as a single clean command so the PreToolUse permission gate matches it and you get a deterministic approval prompt:

    python3 "<PATH>/_tokopt.py" uninstall

where `<PATH>` is this plugin's `hooks/` dir. Use `${CLAUDE_PLUGIN_ROOT}/hooks/_tokopt.py`; if that isn't a real absolute path when it reaches you, first locate the script read-only (e.g. `~/.claude/plugins/*/tokopt/hooks/_tokopt.py`, or the dev checkout) and use that absolute path. Do NOT wrap it in `$(...)`, pipes, or extra commands — run the plain `python3 "…/_tokopt.py" uninstall` form.

Then relay the output verbatim. Run this BEFORE `/plugin uninstall tokopt` — Claude Code runs no code at uninstall time, so this is the only thing that removes the ANTHROPIC_BASE_URL pointer and stops the gateway. Keep any restart warning.
