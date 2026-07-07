# Rust proxy gateway — restart of the router+compression plugin

**Linear:** [SEM-58](https://linear.app/semfs/issue/SEM-58/proxy-gateway-rust-binary-restart-of-routercompression-plugin) — supersedes [SEM-52](https://linear.app/semfs/issue/SEM-52/model-router-compression-plugin-claude-code-and-codex)
**Status:** end-to-end functional + packaged as plugin **v0.5.0**; committed to the repo 2026-07-07 (milestones 1–5 shipped 2026-07-06; config page / lifecycle / deterministic permissions added 2026-07-07 — see the dated sections below)
**Date:** 2026-07-06 (updated 2026-07-07)

## Why this ticket exists

SEM-52 scaffolded the router+compression plugin around a **hooks-only front door**:
`SessionStart` to override the model, `UserPromptSubmit` to intercept/rewrite the
prompt, then relay a subagent's answer by blocking the main loop.

That front door doesn't work. Verified against the live Claude Code hooks docs
(2026-07-06):

- `SessionStart`'s `model` field is **read-only** — no hook return value changes it.
- `UserPromptSubmit` can block (exit 2) or append `additionalContext`, but **cannot
  rewrite** the prompt text — the original reaches the model unchanged.
- **No hook substitutes a full assistant turn** without the model API being called —
  blocking cancels the turn, it doesn't let you splice in a subagent's answer instead.

Full comparison + hook capability table: see the artifact linked from the SEM-52 issue
thread, or `plugin/DESIGN.md` §"Why a plugin can do this" (already reached this
conclusion on 2026-07-02 — this ticket acts on it).

## What stays, what gets rebuilt

| Piece | Verdict | Reason |
|---|---|---|
| `plugin/hooks/route_subagent.py` (`PreToolUse` matcher `Agent`) | **keep** | `updatedInput{model,prompt}` is real interception — reaches every subagent spawn |
| `plugin/hooks/compress_output.py` (`PostToolUse` matcher `Read\|Grep\|Agent`) | **keep** | `updatedToolOutput` is real interception — reaches served files + subagent answers |
| `server/server.py` (Python HTTP server: route + kompress + OpenRouter call) | **replace** | becomes the Rust binary below |
| `plugin/hooks/session_start.py` / `_tokopt.py` (health-check + launch pattern) | **port** | same pattern, now launching the Rust binary instead of a Python process |
| Front door: intercepting the **main loop's own turn** (SEM-52's non-goal, this ticket's goal) | **new** | only reachable via `ANTHROPIC_BASE_URL` → local proxy, not via hooks |

## Architecture (v2)

```
Claude Code / Codex CLI
   │  ANTHROPIC_BASE_URL=http://127.0.0.1:<port>   (set once, session start)
   ▼
tokopt-gateway (Rust binary)                        ← new, this ticket
   1. terminate/originate the Messages API shape (SSE passthrough for the happy path)
   2. split request → PROSE | CODE
   3. compress PROSE (kompress-v2-base, ONNX) · CODE passed through byte-identical
   4. ROUTER: cheap-model prompt → route → model tier
   5. forward to Anthropic / OpenRouter, stream the response back untouched
   ▼
CLI receives a normal-shaped response — main loop, subagents, nested workflows
all resolve through the same door, no hook coordination needed
```

Existing `PreToolUse(Agent)` / `PostToolUse(Read|Grep)` hooks keep running alongside
this — they're cheap, already correct, and cover cases the proxy sees but a hook can
act on with less latency (no need to round-trip a subagent spawn through the gateway
if a hook can rewrite `updatedInput` directly).

## Why Rust for the gateway (not Python)

- Ships as a **single static binary** — no interpreter/venv to bootstrap on first run,
  which matters for a "download the plugin and go" experience.
- Needs to be a **long-lived, self-supervising local daemon** (auto-restart on crash,
  low idle footprint) sitting in the hot path of every model call — a compiled
  binary is the right shape for that, not a Python process tree.
- Reuses the existing Cargo workspace conventions (`tokio`, `clap`, `tracing` already
  workspace deps in the root `Cargo.toml`).

## Scope of the cleanup

1. Retire `server/` (Python HTTP server) — the routing/compression logic it holds
   moves into the new crate; kompress inference moves to ONNX runtime (Rust) or an
   embedded call-out, TBD during implementation.
2. Keep `plugin/hooks/route_subagent.py` and `compress_output.py` as-is.
3. Port the health-check/launch pattern from `plugin/hooks/_tokopt.py` +
   `session_start.py` to point at the new binary instead of the Python server.
4. New: `UserPromptSubmit` hook used **only as a readiness gate** (health-check →
   launch-and-wait if down → pass the prompt through unmodified) — not as a content
   rewriter, since that capability doesn't exist.
5. New: the gateway implements the Anthropic Messages API request/response shape
   (including SSE streaming) closely enough that the CLI can't tell the difference
   except for the routed model / compressed prose.

## Proposed source layout

- New crate: `crates/tokopt-gateway/` (added to the root `Cargo.toml` workspace
  `members` when implementation starts).
- `plugin/` keeps the hooks + MCP tool wiring; `server/` (Python) is removed once the
  Rust crate reaches parity.

## Shipped (2026-07-06)

- **Decision on the open ONNX question below: not needed.** Compression is served
  via a **backend API**, not local ONNX inference in the gateway — dropped the
  `ort`/`tokenizers` integration path entirely (was mid-research when redirected).
  `crates/tokopt-gateway/src/compressor.rs` is a pure HTTP client.
- **Router** (`src/router.rs`): `POST /route` picks a tier via an OpenAI-compatible
  chat-completion call (Structured Outputs, enum-locked to the real route names),
  default model `gpt-4.1-nano`. Falls back to a ported keyword heuristic on any
  failure or when unconfigured.
- **Compressor** (`src/compressor.rs`): `POST /compress` — prose split from code
  (fenced blocks + code-extension files) before anything reaches a backend; code
  is never sent anywhere. Default model `chopratejas/kompress-v2-base`.
- **Env resolution** (`src/config.rs`): `ROUTER_MODEL`/`ROUTER_API_KEY`/`ROUTER_BASE_URL`
  and `COMPRESSOR_MODEL`/`COMPRESSOR_API_KEY`/`COMPRESSOR_BASE_URL`, independently
  resolved — model override applies regardless of endpoint; `_API_KEY`/`_BASE_URL`
  presence switches that backend to BYO. Absent both, falls back to semfs's
  **hosted default endpoint, which has no real backend deployed yet** — base URL
  empty out of the box, `/health` reports `reachable: false`, and both `/route` and
  `/compress` fail open (heuristic / verbatim passthrough) rather than invent a host.
  See `tickets/compression-ladder-rule-based/TICKET.md` for the prior-art
  `SEMFS_API_KEY` hosted-tier concept this mirrors.
- **Hooks reconnected**: `route_subagent.py` (`PreToolUse(Agent)`) and
  `compress_output.py` (`PostToolUse(Read|Grep|Glob|Agent)`) ported from the
  archive, retargeted at the new gateway's `/route`/`/compress` (same JSON
  contract, so the scripts barely changed). `_tokopt.py` metrics/`on`/`off`/
  `status`/`metrics` commands restored — meaningful again now that there's real
  routing/compression to measure.
- **Installability fixed**: found via `claude plugin validate` + the plugins
  reference docs that the gateway binary must live in `plugin/bin/` (the one path
  that survives a marketplace cache-copy) — `_find_binary()` now checks
  `${CLAUDE_PLUGIN_ROOT}/bin/tokopt-gateway` first. Release binary built and
  bundled; verified end-to-end with only `CLAUDE_PLUGIN_ROOT` set (no dev-repo
  shortcuts) that the bundled binary is what actually launches.
- **E2E-tested against a mock backend** (not just unit-level): unconfigured mode
  (heuristic route + passthrough, `/health` correctly reports `reachable: false`),
  BYO mode (mock forces a non-heuristic route to prove the LLM path fired,
  mock strips vowels to prove real compression + code/prose split together,
  distinct `Authorization` headers per backend confirmed not cross-contaminated),
  and the full hook chain end to end producing correct `updatedInput`/
  `updatedToolOutput`.

**Deliberately not done:** did not point this machine's real `~/.claude/settings.json`
at the gateway (machine-wide change, needs explicit user go-ahead — see SEM-58).
No real hosted default endpoint exists yet for either router or compressor —
that's a separate, later piece of work (deploying and securing an actual service),
not fabricated here.

## Backend server shipped (2026-07-06, later same day)

The "default-hosted" extension point above is no longer purely theoretical — built
and E2E-tested `backend-server/` (Python, separate from the per-user Rust gateway
since it's a centrally-run service, not something end users install):

- `POST /compress` — real `chopratejas/kompress-v2-base` ONNX inference (reused the
  proven loader from the archived v1 server). Verified against real prose: dropped
  filler words for real, not a stub (`"after much discussion"` → `"much discussion"`, etc.).
- `POST /v1/chat/completions` — OpenAI-shaped passthrough to a real upstream
  (`UPSTREAM_ROUTER_BASE_URL`/`UPSTREAM_ROUTER_API_KEY`), model-allowlisted
  (`ROUTER_MODEL_ALLOWLIST`, default `gpt-4.1-nano` only) and per-IP rate-limited
  (`RATE_LIMIT_PER_MIN`) — this endpoint spends semfs's own key on behalf of
  anyone hitting the free default tier, so both guards are load-bearing, not optional.
- `GET /usage` — SQLite-backed metrics (`backend-server/metrics.py`): per-request
  log (endpoint, model, status, latency, chars) + 1h/24h/7d aggregates. 404s
  entirely if `ADMIN_TOKEN` is unset rather than defaulting open.
- **Full chain E2E-verified for real** (not mocks): started the real backend-server
  and the real gateway with zero client-side `ROUTER_*`/`COMPRESSOR_*` env vars,
  pointed the gateway's `TOKOPT_DEFAULT_*_BASE_URL` at the local backend, and
  confirmed `/health` flips to `reachable: true`, `/compress` shows a real
  measured reduction (`backend: "backend"`, not passthrough), and `/route`
  correctly attempts the real backend before falling back to heuristic.
- Caught and fixed a real security issue during this pass: the auto-mode
  permission classifier flagged the first draft binding to `0.0.0.0` (exposing
  unauthenticated `/compress`/`/chat/completions` to the local network) — now
  defaults to `127.0.0.1`, widening it is an explicit deploy-time choice.

**Not done: deployment.** This runs and is tested locally; it isn't reachable from
anywhere else yet. That's a real ongoing-cost decision (which platform, a real
`UPSTREAM_ROUTER_API_KEY` to protect, a domain) that needs an explicit go-ahead,
not something to default into.

## Architecture simplified (2026-07-06, third pass same day)

User feedback: drop the subagent-hook layer entirely and put compression +
routing on the **main** request path instead — every message, not just
subagent spawns and file reads. Concretely:

- **Removed:** `plugin/hooks/route_subagent.py`, `compress_output.py`, and the
  `PreToolUse`/`PostToolUse` entries in `hooks.json`. No hook wiring left
  beyond `SessionStart` (launch + wire settings) and `UserPromptSubmit`
  (readiness gate).
- **The gateway's proxy path is now smart.** Every request to a `/messages`
  path is parsed; the latest message's text is compressed and routed to a real
  `claude-*` model id, then the rewritten request goes to the real API and
  streams back. Everything else still passes through untouched. Safety rule:
  a fenced code block anywhere in the message skips compression for that
  message entirely (routing still runs) — reassembling compressed-prose-around
  -untouched-code for arbitrarily interleaved content wasn't worth the risk.
- **Backend API collapsed from two endpoints to one.** Was `/compress` +
  `/v1/chat/completions`; now a single `POST /optimize` returns both
  `compressed_context` and `relevant_model` (with `compress`/`route` boolean
  flags to get just one side — used when only one of router/compressor is
  BYO'd elsewhere). BYO router still needs real OpenAI-compatible
  `/chat/completions` to interop with an arbitrary third-party provider — that
  didn't change. Also dropped the model allowlist: `/optimize` never takes a
  caller-supplied model for its internal LLM call, so there's nothing to
  validate against an allowlist anymore.
- **Local usage moved from a Python jsonl to the gateway's own SQLite**
  (`crates/tokopt-gateway/src/usage.rs`, `~/.tokopt/usage.db`), and every event
  is also fire-and-forget POSTed to the backend's new `/usage` ingestion
  endpoint, so aggregate stats cover BYO users too. `_tokopt.py`'s
  `/tokopt-metrics` now reads this back via the gateway's `GET /usage` instead
  of a local file.
- **Real bug caught mid-change:** the on/off toggle used to gate whether the
  Python hooks even launched the gateway. Once the gateway became load-bearing
  for *every* request (not just optional subagent routing), that would have
  meant `/tokopt-off` breaks every single API call (nothing listening on the
  port `ANTHROPIC_BASE_URL` points at). Fixed: hooks now launch the gateway
  unconditionally; the on/off state (`~/.tokopt/state.json`) is checked inside
  the gateway itself, gating only the rewrite logic.
- **E2E-verified for real** (three real processes: gateway, real backend-server,
  mock Anthropic API): model field genuinely rewritten end to end, code fence
  preserved byte-identical while routing still ran, non-`/messages` paths
  passed through with the model completely unchanged, disabling tokopt made
  `/messages` pass through unchanged too, and both usage endpoints (gateway
  local + backend-ingested) showed matching real counts afterward.

**Known trade-off, stated plainly:** this now adds a real network round-trip
(to the router/compressor backend) before every single message reaches
Anthropic — a meaningful latency cost on every turn, not just subagent spawns.
And routing can silently override a model the user explicitly picked. Both are
direct consequences of the simplification that was asked for; noting them here
rather than quietly eating the trade-off.

## 2026-07-07 — config page, lifecycle, deterministic permissions, packaging

Fourth pass, driven by real install/use feedback. Packaged the plugin (v0.2.0 →
**v0.5.0**) and committed the whole tree (`crates/tokopt-gateway/`,
`backend-server/`, `plugin/`) to the repo for the first time.

- **Built-in config page.** The gateway now serves `GET /config` (an inline HTML
  form), `GET /config.json` (current values), and `POST /config` (save to
  `~/.tokopt/config.json`). `src/configweb.rs` + handlers in `main.rs`. The api
  key is never returned in the clear (only a boolean "set"), and `POST` requires
  `Content-Type: application/json` — a cheap CSRF guard for a loopback service
  that can rewrite where prompts are sent (a cross-origin "simple" POST can't set
  that content type without a preflight we never answer). Returns **415** otherwise.
- **Config is resolved per request, not frozen at startup.** `config.rs`'s
  `resolve_router_config()`/`resolve_compressor_config()` re-read env + config.json
  on every request, and `main.rs` resolves fresh inside `smart_proxy`/`health`
  instead of baking it into `Arc<Config>`. So a save in the config page takes
  effect on the very next request — **no gateway restart**. New precedence
  (highest first): BYO env → `~/.tokopt/config.json` (`config`) → `TOKOPT_ENV=dev`
  → `localhost:8788` (`dev-localhost`) → hosted default (`default-hosted`, still
  undeployed → empty → fail-open). The old `source == "default-hosted"` checks in
  `smart_proxy` became `!= "byo"`, since `config`/`dev-localhost`/`default-hosted`
  are all `/optimize`-style backends and only BYO changes call shape.
- **`/tokopt:on` opens the config page** in the browser (`_open_url` in
  `_tokopt.py`, `open`/`xdg-open`/`start` by platform, fail-open).
- **`/tokopt:off` = full teardown** (per explicit user decision "off = kill the
  proxy"): removes `ANTHROPIC_BASE_URL` from `settings.json` **only if it points at
  our gateway** (never clobbers a user's own value) and stops the gateway process
  (PID file + `lsof` fallback). Consequence, documented not hidden: the *current*
  session must restart afterward — its env still points at the now-stopped port
  (env is read once at startup), so its next request gets `ConnectionRefused` until
  restart. New sessions are clean immediately. The `SessionStart`/`UserPromptSubmit`
  hooks now **respect the off flag** (early-return + defensive re-unwire) instead of
  relaunching unconditionally — otherwise the next prompt would silently re-wire and
  undo the teardown.
- **`/tokopt:uninstall`.** Confirmed against the docs (via the claude-code-guide
  agent) that **Claude Code runs no code at plugin-uninstall time** — there is no
  uninstall/`SessionEnd`-for-uninstall hook. So a self-cleaning post-uninstall
  script is impossible; the portable answer is a user-run teardown *before*
  removal. `/tokopt:uninstall` does the `off` teardown plus wiping `~/.tokopt`, and
  prints "safe to `/plugin uninstall tokopt` now."
- **Deterministic permissions — the real fight.** Root-caused (against the auto-mode
  permission docs) why `/tokopt:on` intermittently failed with "classifier
  unavailable": in **auto** mode Claude Code **drops wildcarded-interpreter allow
  rules** like the command's own `Bash(python3:*)`, so every run fell through to the
  safety **classifier** — which transiently 5xx'd on the opus model. Verified it
  was *not* our proxy (base_url unset, gateway down — the classifier call goes
  straight to Anthropic). Fix, in two parts:
  1. A `PreToolUse` hook (`pretooluse_gate.py`) returns an explicit
     `permissionDecision` for **only** the exact `python3 "<any>/_tokopt.py" <sub>`
     shape — `ask` for state-changing (`on`/`off`/`uninstall`/`setup`), `allow` for
     read-only (`status`/`metrics`). Path-independent (matches the command payload,
     not a `${CLAUDE_PLUGIN_ROOT}` matcher, which isn't supported) and injection-
     hardened (anchored start-to-end, single known subcommand, no shell metachars —
     `&&`/`;`/`$(...)`/`-c` payloads/lookalikes all fall through, never auto-approved).
  2. The slash commands were converted from `!`bang-execution to **real Bash-tool
     calls** — a `!`bang doesn't trigger `PreToolUse` at all (that's why an earlier
     hook attempt never fired). Now the gate fires and the outcome never depends on
     the classifier. Confirmed working (the deterministic y/n prompt with our custom
     reason string appears).
     *Caveat, stated plainly:* the docs don't **explicitly** promise a `PreToolUse`
     decision bypasses the classifier during an outage (they do guarantee it for an
     MCP tool marked `_meta["anthropic/requiresUserInteraction"]`, which is the
     fallback if this ever proves insufficient). Chosen with eyes open as the
     lighter option.
- **Always-allow via a trust flag.** `/tokopt:trust` sets `"trusted": true` in
  `~/.tokopt/state.json` (merged, never clobbering `enabled`); the gate then returns
  `allow` for state-changing commands too — no prompt. `/tokopt:untrust` reverts.
  The injection guard still holds when trusted (trust only relaxes the prompt for the
  exact command shape, nothing else).
- **Packaging.** `plugin/.claude-plugin/plugin.json` at v0.5.0; the 7 MB bundled
  gateway binary is a **build artifact** — gitignored, rebuilt via `plugin/build.sh`
  (and embedded in the `dist/tokopt-<ver>.tgz` distributables, also gitignored).
  `backend-server/usage.db*` runtime files gitignored too. Committed source only:
  the Rust crate (+ its workspace `Cargo.toml`/`Cargo.lock` wiring), the Python
  backend, and the plugin (hooks/commands/manifests/README/build.sh).

**Backend status at commit time:** `backend-server/` runs locally (`:8788`, real
kompress-v2 ONNX compressor loaded) but routing is **heuristic** — no
`UPSTREAM_ROUTER_API_KEY` set, so the gpt-4.1-nano LLM route can't fire. Still
undeployed (see below).

## Open questions (remaining)

- Deploy `backend-server/` somewhere reachable and point the compiled gateway's
  default `TOKOPT_DEFAULT_ROUTER_BASE_URL`/`TOKOPT_DEFAULT_COMPRESSOR_BASE_URL`
  at it for real — platform choice + cost + a real upstream API key, all pending.
- Exact SSE re-framing behavior needed to stay transparent to the CLI's own retry/
  streaming logic under real load — smoke-tested with a dribbling mock upstream;
  a real multi-turn Claude Code session against it is still open.
- Self-supervision mechanism: in-process watchdog vs. relying on the
  `UserPromptSubmit` gate alone (currently the latter).

## Milestones

1. ✅ Linear ticket + this planning doc.
2. ✅ `crates/tokopt-gateway` skeleton: transparent Messages-API proxy, no rewriting.
3. ✅ Compression + routing inside the gateway (backend-API based, see "Shipped").
4. ✅ `SessionStart` launch + `UserPromptSubmit` readiness gate + `settings.json` wiring.
5. ✅ `PreToolUse(Agent)`/`PostToolUse(Read|Grep|Glob|Agent)` hooks reconnected;
   `server/` (Python) fully retired from the live tree (still in the archive).
