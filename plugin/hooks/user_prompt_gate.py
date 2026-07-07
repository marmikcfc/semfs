#!/usr/bin/env python3
"""UserPromptSubmit hook — readiness gate, plus the one reliable place to warn
"this session hasn't picked up the gateway yet, restart" — because unlike
SessionStart, this hook actually re-fires after `/reload-plugins`, which is
the path a mid-session `/plugin install` takes (no real session start happens
at install time, so SessionStart never gets a chance to run at all until the
user restarts anyway).

Hooks cannot rewrite the model/prompt for the main turn (see
tickets/proxy-gateway-rs/README.md) — that's the whole reason this plugin runs a
proxy behind ANTHROPIC_BASE_URL instead of trying to do it from hooks. This hook
never touches prompt content and always lets the prompt through.
FAIL-OPEN: never block the user's prompt over a gateway problem.

When tokopt is toggled OFF it means a full teardown (settings.json unwired, the
gateway stopped), so this hook stays out of the way while off — it must not
relaunch or re-wire, or it would silently undo the teardown. The on/off state
lives in ~/.tokopt/state.json.
"""
import json, sys
import _tokopt


def main():
    try:
        json.load(sys.stdin)                 # drain hook payload (unused)
    except Exception:
        pass

    if not _tokopt.is_enabled():             # off = torn down; do nothing
        return

    try:
        _tokopt.start_gateway(wait_s=8.0)     # no-op if already healthy
    except Exception:
        pass                                  # never block the prompt on gateway trouble

    try:
        _tokopt.ensure_base_url_configured()  # wire settings.json if not already
    except Exception:
        pass

    try:
        live_ok = _tokopt.live_matches_gateway()
    except Exception:
        live_ok = True                        # fail-open: don't nag if we can't tell

    if not live_ok:
        print(json.dumps({"hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": (
                "tokopt: this session hasn't picked up the gateway yet — "
                f"settings.json points ANTHROPIC_BASE_URL at {_tokopt.GATEWAY_URL}, "
                "but this already-running session started before that took "
                "effect (env is only read once, at startup). Nothing is broken; "
                "just restart Claude Code once to activate routing + compression."
            )}}))


if __name__ == "__main__":
    main()
