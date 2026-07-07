---
description: Turn tokopt ON (route to cheaper tiers + compress prose) and open the backend config page
allowed-tools: Bash(python3:*)
---
Turn tokopt on by running its helper **with the Bash tool** (not inline), as a single clean command so the PreToolUse permission gate matches it and you get a deterministic approval prompt:

    python3 "<PATH>/_tokopt.py" on

where `<PATH>` is this plugin's `hooks/` dir. Use `${CLAUDE_PLUGIN_ROOT}/hooks/_tokopt.py`; if that isn't a real absolute path when it reaches you, first locate the script read-only (e.g. `~/.claude/plugins/*/tokopt/hooks/_tokopt.py`, or the dev checkout) and use that absolute path. Do NOT wrap it in `$(...)`, pipes, or extra commands — run the plain `python3 "…/_tokopt.py" on` form.

Then relay the command's output to the user verbatim, keeping any ⚠ restart warning and the config-page line.
