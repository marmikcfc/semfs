#!/usr/bin/env python3
"""tokopt shared state + gateway health/launch/wire-up helpers.

State lives in ~/.tokopt/ so it survives reinstalls and is shared across sessions:
  state.json  -> {"enabled": true}
  usage.db    -> the gateway's own SQLite usage log (Rust side, see usage.rs) —
                 this file just reads it back out via the gateway's GET /usage,
                 it never touches the db directly.
  gateway.log -> stdout/stderr of the launched binary

CLI (used by plugin/commands/*.md):
  python3 _tokopt.py on|off|status|metrics
  python3 _tokopt.py setup   -- run once at a terminal, BEFORE first launching
                                `claude`, to avoid the two-restarts-on-first-
                                install problem (see plugin/README.md).
"""
import json, os, shutil, signal, subprocess, sys, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
PLUGIN_ROOT = os.path.dirname(HERE)                      # .../plugin
REPO_ROOT = os.path.dirname(PLUGIN_ROOT)                 # dev checkout only; absent once packaged

STATE_DIR = os.environ.get("TOKOPT_HOME") or os.path.expanduser("~/.tokopt")
STATE_FILE = os.path.join(STATE_DIR, "state.json")
PID_FILE = os.path.join(STATE_DIR, "gateway.pid")
GATEWAY_PORT = os.environ.get("TOKOPT_GATEWAY_PORT", "8787")
GATEWAY_URL = os.environ.get("TOKOPT_GATEWAY_URL", f"http://127.0.0.1:{GATEWAY_PORT}")

# Overridable for tests — real installs always use ~/.claude/settings.json.
CLAUDE_SETTINGS_PATH = os.environ.get("TOKOPT_CLAUDE_SETTINGS") or os.path.expanduser(
    "~/.claude/settings.json"
)


def _ensure_dir():
    os.makedirs(STATE_DIR, exist_ok=True)


def _read_state() -> dict:
    try:
        with open(STATE_FILE) as f:
            d = json.load(f)
            return d if isinstance(d, dict) else {}
    except Exception:
        return {}


def _write_state(d: dict):
    _ensure_dir()
    with open(STATE_FILE, "w") as f:
        json.dump(d, f)


def is_enabled() -> bool:
    return bool(_read_state().get("enabled", True))     # default ON (download-and-go)


def set_enabled(on: bool):
    d = _read_state()
    d["enabled"] = bool(on)
    _write_state(d)                     # merge — never drop the trust flag


def is_trusted() -> bool:
    return bool(_read_state().get("trusted", False))    # default: ask each time


def set_trusted(on: bool):
    d = _read_state()
    d["trusted"] = bool(on)
    _write_state(d)


def health(timeout=3):
    try:
        with urllib.request.urlopen(GATEWAY_URL + "/health", timeout=timeout) as r:
            return json.load(r)
    except Exception:
        return None


def live_matches_gateway() -> bool:
    """Does THIS ALREADY-RUNNING process's actual environment have
    ANTHROPIC_BASE_URL pointed at the gateway? Distinct from whether
    settings.json *says* it should be — a hook subprocess inherits its parent
    session's real env as of that session's own startup, so this tells us
    whether the wiring has actually taken effect for the session the user is
    in right now, or only exists on disk for a future one."""
    return os.environ.get("ANTHROPIC_BASE_URL") == GATEWAY_URL


def usage(timeout=3):
    """Usage now lives entirely in the gateway's own SQLite (usage.rs) — this
    just reads the summary back out over HTTP, no local file to maintain."""
    try:
        with urllib.request.urlopen(GATEWAY_URL + "/usage", timeout=timeout) as r:
            return json.load(r)
    except Exception:
        return None


CONFIG_URL = GATEWAY_URL + "/config"


def _open_url(url: str) -> bool:
    """Best-effort open a URL in the user's default browser. Detached and
    fail-open — never raises, never blocks the command."""
    try:
        if sys.platform == "darwin":
            cmd = ["open", url]
        elif sys.platform.startswith("linux"):
            cmd = ["xdg-open", url]
        elif sys.platform.startswith("win"):
            cmd = ["cmd", "/c", "start", "", url]
        else:
            return False
        subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return True
    except Exception:
        return False


def _find_binary():
    """Locate the compiled gateway binary. Order matters:
    1. explicit override (tests)
    2. bundled binary in this install (${CLAUDE_PLUGIN_ROOT}/bin/) — the ONLY path
       that survives a real marketplace install, since installed plugins can't
       reference files outside their own directory (the cache-copy is isolated
       from any dev checkout's target/).
    3. dev-checkout-relative target/{release,debug}/ — only works when running
       via `--plugin-dir` against this repo directly, not once installed for real.
    4. PATH.
    None if not found — callers must fail open, never block the session."""
    override = os.environ.get("TOKOPT_GATEWAY_BIN")
    if override and os.path.exists(override):
        return override
    plugin_root = os.environ.get("CLAUDE_PLUGIN_ROOT") or PLUGIN_ROOT
    bundled = os.path.join(plugin_root, "bin", "tokopt-gateway")
    if os.path.exists(bundled):
        return bundled
    for profile in ("release", "debug"):
        candidate = os.path.join(REPO_ROOT, "target", profile, "tokopt-gateway")
        if os.path.exists(candidate):
            return candidate
    return shutil.which("tokopt-gateway")


def start_gateway(wait_s: float = 12.0):
    """Launch the gateway detached; return health dict once up (or None). Fail-open:
    a missing binary or launch failure must never block the session starting."""
    h = health()
    if h:
        return h
    binary = _find_binary()
    if not binary:
        return None
    _ensure_dir()
    log = open(os.path.join(STATE_DIR, "gateway.log"), "a")
    try:
        proc = subprocess.Popen(
            [binary, "--port", GATEWAY_PORT],
            stdout=log, stderr=subprocess.STDOUT,
            start_new_session=True, env={**os.environ},
        )
    except Exception:
        return None
    # Record the PID so `off` / uninstall can stop exactly this process later
    # (best-effort; stop_gateway also falls back to finding it by port).
    try:
        with open(PID_FILE, "w") as f:
            f.write(str(proc.pid))
    except Exception:
        pass
    deadline = time.time() + wait_s
    while time.time() < deadline:
        h = health()
        if h:
            return h
        time.sleep(0.3)
    return None


def ensure_base_url_configured() -> bool:
    """Idempotently merge ANTHROPIC_BASE_URL=<gateway> into settings.json's `env`,
    preserving every other key untouched. Returns True iff it just changed something
    (caller should tell the user to restart — env is read once at CLI startup, a
    hook cannot rewrite its own parent process's environment for the current
    session). Returns False if already correct, or on any failure — never corrupt
    or clobber the user's real settings file."""
    try:
        try:
            with open(CLAUDE_SETTINGS_PATH) as f:
                settings = json.load(f)
        except FileNotFoundError:
            settings = {}
        env = settings.setdefault("env", {})
        if env.get("ANTHROPIC_BASE_URL") == GATEWAY_URL:
            return False
        env["ANTHROPIC_BASE_URL"] = GATEWAY_URL
        os.makedirs(os.path.dirname(CLAUDE_SETTINGS_PATH) or ".", exist_ok=True)
        tmp = CLAUDE_SETTINGS_PATH + ".tokopt.tmp"
        with open(tmp, "w") as f:
            json.dump(settings, f, indent=2)
        os.replace(tmp, CLAUDE_SETTINGS_PATH)
        return True
    except Exception:
        return False


def unwire_base_url() -> bool:
    """Mirror image of ensure_base_url_configured: remove ANTHROPIC_BASE_URL from
    settings.json's `env` — but ONLY if it points at our gateway, never touch a
    base_url the user set to something else. Drops an `env` block left empty.
    Returns True iff it removed something. Never corrupts the settings file."""
    try:
        with open(CLAUDE_SETTINGS_PATH) as f:
            settings = json.load(f)
    except Exception:
        return False
    env = settings.get("env")
    if not isinstance(env, dict) or env.get("ANTHROPIC_BASE_URL") != GATEWAY_URL:
        return False
    try:
        del env["ANTHROPIC_BASE_URL"]
        if not env:
            settings.pop("env", None)
        tmp = CLAUDE_SETTINGS_PATH + ".tokopt.tmp"
        with open(tmp, "w") as f:
            json.dump(settings, f, indent=2)
        os.replace(tmp, CLAUDE_SETTINGS_PATH)
        return True
    except Exception:
        return False


def _gateway_pids() -> set:
    """PIDs of our gateway: the recorded PID file plus anything listening on the
    gateway port (lsof — Unix; the plugin targets macOS/Linux). Best-effort."""
    pids = set()
    try:
        with open(PID_FILE) as f:
            pids.add(int(f.read().strip()))
    except Exception:
        pass
    try:
        out = subprocess.run(
            ["lsof", "-nP", f"-iTCP:{GATEWAY_PORT}", "-sTCP:LISTEN", "-t"],
            capture_output=True, text=True, timeout=5,
        )
        for tok in out.stdout.split():
            pids.add(int(tok))
    except Exception:
        pass
    return pids


def stop_gateway() -> bool:
    """Stop the running gateway (SIGTERM). Returns True iff something was
    signalled. Removes the PID file. Fail-open — never raises."""
    stopped = False
    for pid in _gateway_pids():
        try:
            os.kill(pid, signal.SIGTERM)
            stopped = True
        except Exception:
            pass
    try:
        os.remove(PID_FILE)
    except Exception:
        pass
    return stopped


def _fmt_metrics() -> str:
    u = usage()
    if u is None:
        return "tokopt — savings so far\n" + "=" * 32 + "\ngateway is down; no usage data available."

    # Everything below is computed by the gateway itself now (usage.rs /
    # pricing.rs) from REAL token counts — a count_tokens call on the
    # pre-compression original vs. the real input_tokens the actual (routed,
    # compressed) call used, and real per-model $/Mtok pricing. Nothing here
    # is a chars/4 guess.
    by_model = u.get("by_model", [])  # [[model, count], ...]

    lines = ["tokopt — savings so far", "=" * 32]
    lines.append(f"enabled:            {'yes' if is_enabled() else 'NO (paused)'}")
    lines.append(f"gateway:            {'up' if health() else 'down'}")
    lines.append(f"total requests:     {u.get('total_requests', 0)}  " +
                 (", ".join(f"{m}×{c}" for m, c in by_model) or "—"))
    lines.append(f"rerouted to a different model: {u.get('requests_rerouted_to_a_different_model', 0)}")
    lines.append("")
    lines.append(f"tokens removed by compression (real, count_tokens vs. actual): "
                 f"{u.get('tokens_removed_by_compression', 0):,}")
    lines.append(f"chars removed by compression: {u.get('chars_saved_by_compression', 0):,}")
    lines.append("")
    lines.append(f"$ saved by routing (real per-token cost delta): "
                 f"${u.get('cost_saved_usd_from_routing', 0.0):.4f}")
    lines.append("")
    lines.append(f"({u.get('cost_note', '')})")
    return "\n".join(lines)


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "status"
    if cmd == "setup":
        # Run this ONCE at a terminal, BEFORE ever launching `claude` with the
        # plugin installed — not from inside a session. A hook can only write
        # settings.json from within an already-running session, which can
        # never benefit from its own write (env is read once at startup); this
        # collapses that to zero extra restarts by wiring it before the first
        # session ever starts, so that first launch already has the right env.
        just_wired = ensure_base_url_configured()
        h = start_gateway()
        if just_wired:
            print(f"settings.json wired: ANTHROPIC_BASE_URL={GATEWAY_URL}")
        else:
            print(f"settings.json already wired to {GATEWAY_URL}")
        print("gateway:", "up" if h else "down (will retry on first launch)")
        print("You can now run `claude` — this first launch will already route through the gateway.")
    elif cmd == "on":
        set_enabled(True)
        just_wired = ensure_base_url_configured()   # `off` removes it, so re-wire here
        h = start_gateway()
        print("tokopt ON. gateway:", "up" if h else "down (will retry next session)")
        if h:
            # Proxy is up → open the config page so backend models can be set.
            opened = _open_url(CONFIG_URL)
            print(f"  config page: {CONFIG_URL}"
                  + ("  (opening in your browser…)" if opened else "  (open it in your browser)"))
        if just_wired or not live_matches_gateway():
            print(
                "\n⚠  Restart Claude Code to activate it. settings.json now points "
                f"ANTHROPIC_BASE_URL at {GATEWAY_URL}, but env is only read once at "
                "startup — so this already-running session's traffic still goes straight "
                "to Anthropic. One restart and you're routed."
            )
    elif cmd == "off":
        # Full teardown: flip the flag, unwire settings.json, stop the proxy.
        set_enabled(False)
        unwired = unwire_base_url()
        stopped = stop_gateway()
        print("tokopt OFF — full teardown.")
        print("  settings.json:  " + ("ANTHROPIC_BASE_URL removed" if unwired
                                       else "nothing to remove (not wired to our gateway)"))
        print("  proxy/gateway:  " + ("stopped" if stopped else "was not running"))
        if live_matches_gateway():
            print(
                f"\n⚠  Restart Claude Code now. THIS session's env still has "
                f"ANTHROPIC_BASE_URL={GATEWAY_URL}, which is now stopped — further requests "
                "in this session would fail to connect until you restart. New sessions are "
                "already clean (settings.json unwired)."
            )
    elif cmd == "trust":
        set_trusted(True)
        print("tokopt: commands are now TRUSTED — /tokopt:on, /tokopt:off and "
              "/tokopt:uninstall will run without asking each time.")
        print("Revoke any time with /tokopt:untrust.")
    elif cmd == "untrust":
        set_trusted(False)
        print("tokopt: trust REVOKED — state-changing commands will ask for "
              "confirmation again before running.")
    elif cmd == "uninstall":
        # Claude Code has NO uninstall hook — nothing runs automatically when a
        # plugin is removed. So this is the thing to run BY HAND right before
        # `/plugin uninstall tokopt`: same teardown as `off`, plus wiping the
        # ~/.tokopt state dir so no trace (or dangling base_url) is left behind.
        unwired = unwire_base_url()
        stopped = stop_gateway()
        removed_state = False
        try:
            shutil.rmtree(STATE_DIR)
            removed_state = True
        except FileNotFoundError:
            removed_state = True
        except Exception:
            pass
        print("tokopt teardown for uninstall:")
        print("  settings.json:  " + ("ANTHROPIC_BASE_URL removed" if unwired
                                       else "nothing to remove (not wired to our gateway)"))
        print("  proxy/gateway:  " + ("stopped" if stopped else "was not running"))
        print("  ~/.tokopt:      " + ("removed" if removed_state else "left in place (could not remove)"))
        print("\nSafe to run `/plugin uninstall tokopt` now.")
        if live_matches_gateway():
            print(
                f"\n⚠  Restart Claude Code after uninstalling. THIS session still has "
                f"ANTHROPIC_BASE_URL={GATEWAY_URL} in its env (now stopped) — requests in "
                "this session would fail until you restart. New sessions are already clean."
            )
    elif cmd == "status":
        h = health()
        try:
            with open(CLAUDE_SETTINGS_PATH) as f:
                wired = json.load(f).get("env", {}).get("ANTHROPIC_BASE_URL") == GATEWAY_URL
        except Exception:
            wired = False
        extra = ""
        if h:
            r, c = h.get("router", {}), h.get("compressor", {})
            extra = (f" | router={r.get('model')}@{r.get('source')}"
                     f"(reachable={r.get('reachable')})"
                     f" | compressor={c.get('model')}@{c.get('source')}"
                     f"(reachable={c.get('reachable')})")
        print(f"tokopt {'ON' if is_enabled() else 'OFF'} | gateway {'up' if h else 'down'}"
              + extra + f" | settings.json base_url wired: {'yes' if wired else 'no'}")
    elif cmd == "metrics":
        print(_fmt_metrics())
    else:
        print("usage: _tokopt.py setup|on|off|uninstall|trust|untrust|status|metrics")


if __name__ == "__main__":
    main()
