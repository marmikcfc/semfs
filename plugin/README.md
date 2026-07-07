# tokopt — model-router + compression plugin

**Status:** simplified end-to-end (2026-07-06). No subagent hooks — every
request goes through one path: `ANTHROPIC_BASE_URL` → the gateway → the real
API. See `tickets/proxy-gateway-rs/README.md` for the full history.

## Current shape

- `crates/tokopt-gateway/` — Rust binary, bundled prebuilt at `plugin/bin/`.
  Every request to a `/messages` path is intercepted: the latest message's
  text gets compressed and a real Claude model is chosen for it, then the
  (possibly rewritten) request is forwarded to `api.anthropic.com` and the
  response streamed back untouched. Everything else passes through unmodified.
  - Code safety: if the latest message contains a fenced ``` block, compression
    is skipped for that message entirely (routing still runs) — safer than
    trying to carve out and reassemble code and prose separately.
  - After every `/messages` response: one row written to a local SQLite
    (`~/.tokopt/usage.db`) and a fire-and-forget report to the backend, so
    aggregate stats aren't limited to requests that hit the backend directly.
  - `GET /usage` — local usage summary (read by `/tokopt-metrics`).
  - `GET /health` — which router/compressor backend is actually active.
  - `GET /config` — built-in HTML config page (set backend URL/keys/models);
    `GET /config.json` returns current values, `POST /config` saves them to
    `~/.tokopt/config.json`. Config is resolved **per request**, so a save
    takes effect on the next request with no gateway restart. `POST` requires
    `Content-Type: application/json` (basic CSRF guard for a loopback service).
- `plugin/hooks/session_start.py` / `user_prompt_gate.py` — when tokopt is **on**,
  launch the gateway and wire `ANTHROPIC_BASE_URL` into `settings.json`. When
  tokopt is **off** they do nothing (and defensively re-unwire): off is a full
  teardown — `settings.json` unwired and the gateway stopped — so a hook must
  not silently relaunch or re-wire it. The on/off flag lives in
  `~/.tokopt/state.json`.
  - `user_prompt_gate.py` also compares this *already-running* session's live
    `ANTHROPIC_BASE_URL` against what `settings.json` says it should be. If
    they differ — install happened mid-session, or `/reload-plugins` ran, but
    this process was never restarted — it surfaces a restart reminder on every
    prompt until you do. This is the one hook that reliably re-fires after
    `/reload-plugins`; `SessionStart` doesn't, since installing a plugin
    mid-session never triggers an actual session start.
- `plugin/hooks/_tokopt.py` — shared state + gateway launch/stop, settings.json
  wire/unwire, and reads the gateway's `/health`/`/usage`.
- `commands/{status,on,off,uninstall,metrics}.md`.

## On / off / uninstall

- `/tokopt:on` — flip on, wire `settings.json`, start the gateway, and **open
  the config page** (`http://127.0.0.1:8787/config`) in your browser.
- `/tokopt:off` — **full teardown**: remove `ANTHROPIC_BASE_URL` from
  `settings.json` (only if it points at our gateway — never touches a base_url
  you set yourself) and stop the gateway. Because env is read once at startup,
  the *current* session must be restarted after this (its next request would
  otherwise hit the now-stopped port); new sessions are clean immediately.
- `/tokopt:uninstall` — same teardown plus wiping `~/.tokopt`. Run it **before**
  `/plugin uninstall tokopt`: Claude Code runs no code at uninstall time (there
  is no uninstall hook), so this is the only thing that removes the dangling
  `ANTHROPIC_BASE_URL` pointer and stops the proxy.

## Router / compressor backend selection

Resolved independently per concern, highest precedence first:

1. **BYO env** — `ROUTER_MODEL`/`ROUTER_API_KEY`/`ROUTER_BASE_URL` and
   `COMPRESSOR_MODEL`/`COMPRESSOR_API_KEY`/`COMPRESSOR_BASE_URL`. If either
   `_API_KEY` or `_BASE_URL` is set for a concern, that one switches to BYO.
2. **Config page** — `~/.tokopt/config.json` `backend_url` (set via `/config`).
3. **`TOKOPT_ENV=dev`** (and no config) — the local `backend-server/` at
   `http://127.0.0.1:8788`.
4. **default-hosted** (and no config) — our hosted endpoint, overridable via
   `TOKOPT_DEFAULT_BACKEND_URL`.

Cases 2–4 are all one endpoint answering both concerns via a single `POST
/optimize` call (not two round trips). See `backend-server/README.md`.

BYO router still speaks real OpenAI-compatible `/chat/completions` (needed to
interop with an arbitrary third-party provider); BYO compressor speaks the
same `/optimize` contract as the default backend, just with `route: false`.

No hosted default is deployed yet — the case-4 URL is empty out of the box, so
with no env/config this fails open: heuristic routing, verbatim passthrough.
Use the config page or `TOKOPT_ENV=dev` (with `backend-server/` running) to
point it somewhere real.

## Install

```bash
cd /path/to/this/repo
python3 plugin/hooks/_tokopt.py setup   # ONE-TIME, run at a terminal, before `claude`
claude
/plugin marketplace add .
/plugin install tokopt@tokopt-local
```

The `setup` step matters: without it, the very first restart after installing
is the one that discovers `ANTHROPIC_BASE_URL` needs to be set, writes it to
`settings.json`, and can't use it — env vars are read once at process startup,
and a hook can't rewrite its own already-running parent's environment. That
needs a *second* restart to actually take effect. Running `setup` first writes
the setting before any session ever starts, so the first launch already has it
and is the only restart you need. (If you skip it, everything still works —
`session_start.py` does the same wiring automatically — you'll just see the
two-restart message once, on first install only.)

## Running the gateway by hand

```
cargo run -p tokopt-gateway -- --port 8787 --upstream https://api.anthropic.com
curl localhost:8787/health
curl localhost:8787/usage
```
