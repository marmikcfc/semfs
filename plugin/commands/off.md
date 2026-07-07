---
description: Turn tokopt OFF — full teardown (unwire ANTHROPIC_BASE_URL + stop the proxy)
allowed-tools: Bash(python3:*)
---
Turn tokopt off by running its helper **with the Bash tool** (not inline), as a single clean command so the PreToolUse permission gate matches it and you get a deterministic approval prompt:

    python3 "<PATH>/_tokopt.py" off

where `<PATH>` is this plugin's `hooks/` dir. Use `${CLAUDE_PLUGIN_ROOT}/hooks/_tokopt.py`; if that isn't a real absolute path when it reaches you, first locate the script read-only (e.g. `~/.claude/plugins/*/tokopt/hooks/_tokopt.py`, or the dev checkout) and use that absolute path. Do NOT wrap it in `$(...)`, pipes, or extra commands — run the plain `python3 "…/_tokopt.py" off` form.

Then relay the output verbatim. If it includes a restart warning, keep it — this session still points at the now-stopped gateway, so it must be restarted before it can make requests again.
