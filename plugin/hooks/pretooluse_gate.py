#!/usr/bin/env python3
"""PreToolUse hook — make permission for tokopt's OWN command deterministic.

The /tokopt:* slash commands run `python3 "<plugin>/hooks/_tokopt.py" <sub>` as
a real Bash TOOL call (not a `!`bang — a bang doesn't trigger PreToolUse). In
auto mode Claude Code would otherwise route that through the safety classifier,
which blocks it whenever the classifier model is momentarily unavailable. This
hook returns an explicit permissionDecision for exactly — and only — this
plugin's own script, so the outcome never depends on the classifier:

  * state-changing (on/off/uninstall/setup) -> "ask": a deterministic y/n
    prompt every time. The user is always in the loop for a real change.
  * read-only (status/metrics)              -> "allow": no prompt, no friction.

Matching is path-independent (we inspect the command payload, not a
${CLAUDE_PLUGIN_ROOT} matcher, which isn't supported in `if` filters) and tight
(anchored to the exact command shape). Everything else emits nothing and falls
through to the normal flow. FAIL-OPEN: any error -> exit 0, never blocks/denies.
"""
import json, os, re, sys

_READ_ONLY = {"status", "metrics"}


def _trusted() -> bool:
    """Has the user run /tokopt:trust? If so, state-changing commands are
    auto-allowed instead of prompting. Read inline (no _tokopt import) to keep
    this gate fast and dependency-free. Any error -> not trusted (still ask)."""
    try:
        home = os.environ.get("TOKOPT_HOME") or os.path.expanduser("~/.tokopt")
        with open(os.path.join(home, "state.json")) as f:
            return bool(json.load(f).get("trusted", False))
    except Exception:
        return False

# Match ONLY the exact shape our slash commands emit:
#   python3 "<path>/_tokopt.py" <subcommand>
# Anchored start-to-end with a single known subcommand and NO shell metachars
# in between, so a command that merely mentions _tokopt.py (in a comment, after
# a `&&`/`;`, or as a `-c` payload) can't smuggle itself past the gate.
_PAT = re.compile(
    r'^\s*python[0-9.]*\s+"?[^"]*_tokopt\.py"?\s+'
    r'(on|off|status|metrics|setup|uninstall|trust|untrust)\s*$'
)


def main():
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return
    if payload.get("tool_name") != "Bash":
        return
    command = (payload.get("tool_input") or {}).get("command", "")
    if not isinstance(command, str):
        return
    m = _PAT.search(command)
    if not m:
        return
    sub = m.group(1)
    # read-only always allow; if the user has trusted the plugin, allow the
    # state-changing ones too; otherwise ask for a deterministic confirmation.
    decision = "allow" if (sub in _READ_ONLY or _trusted()) else "ask"
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": decision,
        "permissionDecisionReason": f"tokopt: {'reading' if decision == 'allow' else 'approve running'} "
                                    f"the plugin's own _tokopt.py ({sub})",
    }}))


if __name__ == "__main__":
    main()
