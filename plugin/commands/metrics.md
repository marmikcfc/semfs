---
description: Show tokopt savings — tokens + estimated $ saved by routing and compression
allowed-tools: Bash(python3:*)
---
Show tokopt savings by running its helper **with the Bash tool** as a single clean command:

    python3 "<PATH>/_tokopt.py" metrics

where `<PATH>` is this plugin's `hooks/` dir. Use `${CLAUDE_PLUGIN_ROOT}/hooks/_tokopt.py`; if that isn't a real absolute path when it reaches you, first locate the script read-only (e.g. `~/.claude/plugins/*/tokopt/hooks/_tokopt.py`, or the dev checkout) and use that absolute path. Do NOT wrap it in `$(...)`, pipes, or extra commands — run the plain `python3 "…/_tokopt.py" metrics` form (read-only; it won't prompt).

Then present the savings report in a clean, readable form. Keep the numbers exactly as reported; make clear that routing $ are estimates and compression tokens are measured.
