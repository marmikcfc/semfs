#!/usr/bin/env python3
"""SessionStart hook — make tokopt download-and-go.

Two jobs, both fail-open (any error -> exit 0, session starts normally):
  1. Launch the gateway if it isn't already running.
  2. Make sure ANTHROPIC_BASE_URL in settings.json points at it. This can only take
     effect on the *next* session — env is read once at CLI startup, a hook can't
     rewrite its own parent process's environment — so if this just changed
     something, tell the user a restart is needed.

When tokopt is toggled OFF it means a full teardown — settings.json unwired and
the gateway stopped — so this hook must NOT launch or wire anything while off,
or it would silently undo the teardown on the next session. When off it only
makes sure we stay unwired (self-heal), then gets out of the way.
"""
import json, sys
import _tokopt


def main():
    try:
        json.load(sys.stdin)                 # drain hook payload (unused)
    except Exception:
        pass

    # Off = torn down. Don't relaunch/re-wire; just make sure nothing lingered.
    if not _tokopt.is_enabled():
        try:
            _tokopt.unwire_base_url()
        except Exception:
            pass
        return

    try:
        h = _tokopt.start_gateway()
    except Exception:
        h = None
    try:
        just_wired = _tokopt.ensure_base_url_configured()
    except Exception:
        just_wired = False

    if not h and not just_wired:
        return

    parts = []
    if h:
        router, compressor = h.get("router", {}), h.get("compressor", {})
        parts.append(
            f"tokopt gateway is up at {_tokopt.GATEWAY_URL}. "
            f"Router: {router.get('model')} via {router.get('source')}"
            f"{'' if router.get('reachable') else ' (NOT reachable — no endpoint configured, falls back to heuristic routing)'}. "
            f"Compressor: {compressor.get('model')} via {compressor.get('source')}"
            f"{'' if compressor.get('reachable') else ' (NOT reachable — no endpoint configured, falls back to passthrough)'}."
        )
    else:
        parts.append("tokopt gateway did not start (binary not found) — traffic is "
                      "unaffected, going straight to the real API.")
    if just_wired:
        parts.append("settings.json was just updated to route ANTHROPIC_BASE_URL "
                      "through the gateway — restart the session for this to take effect.")
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "SessionStart", "additionalContext": " ".join(parts)}}))


if __name__ == "__main__":
    main()
