---
description: Revoke tokopt's standing trust — ask for confirmation again on state-changing commands
allowed-tools: Bash(python3:*)
---
Revoke tokopt's standing trust by running its helper **with the Bash tool** as a single clean command:

    python3 "<PATH>/_tokopt.py" untrust

where `<PATH>` is this plugin's `hooks/` dir. Use `${CLAUDE_PLUGIN_ROOT}/hooks/_tokopt.py`; if that isn't a real absolute path when it reaches you, first locate the script read-only (e.g. `~/.claude/plugins/*/tokopt/hooks/_tokopt.py`, or the dev checkout) and use that absolute path. Do NOT wrap it in `$(...)`, pipes, or extra commands — run the plain `python3 "…/_tokopt.py" untrust` form.

After this, `/tokopt:on`, `/tokopt:off`, and `/tokopt:uninstall` will ask for confirmation again. Relay the output verbatim.
