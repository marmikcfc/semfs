---
description: Always-allow tokopt's own commands — stop asking for confirmation on /tokopt:on|off|uninstall
allowed-tools: Bash(python3:*)
---
Grant standing trust to tokopt's own commands by running its helper **with the Bash tool** as a single clean command:

    python3 "<PATH>/_tokopt.py" trust

where `<PATH>` is this plugin's `hooks/` dir. Use `${CLAUDE_PLUGIN_ROOT}/hooks/_tokopt.py`; if that isn't a real absolute path when it reaches you, first locate the script read-only (e.g. `~/.claude/plugins/*/tokopt/hooks/_tokopt.py`, or the dev checkout) and use that absolute path. Do NOT wrap it in `$(...)`, pipes, or extra commands — run the plain `python3 "…/_tokopt.py" trust` form.

This one run still asks for confirmation (you're not trusted yet); approve it once and from then on `/tokopt:on`, `/tokopt:off`, and `/tokopt:uninstall` run without prompting. Relay the output verbatim.
