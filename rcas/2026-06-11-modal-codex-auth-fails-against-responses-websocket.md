# RCA: Modal codex invocation fails auth against Responses websocket

Date: 2026-06-11

## Symptom

The Modal `run_case(case="289", render_mode="two-tier")` smoke now reaches the
actual case metadata and runs the local seed-backed smoke checks successfully,
but the final codex invocation fails before producing any calls or tokens.

The captured stderr begins with:

```text
Reading additional input from stdin...
2026-06-11T18:47:03.534749Z ERROR codex_api::endpoint::responses_websocket: failed to connect to websocket: HTTP error: 401 Unauthorized, url: wss://api.openai.com/v1/responses
```

## What we verified

- Modal itself is reachable from the local shell.
- The shared volume is present and populated.
- `smoke_grep` passes in `inline`, `two-tier`, and `paths` modes.
- The task payload inside the container is non-empty.
- `OPENROUTER_API_KEY` is present inside the Modal container.
- The case metadata resolver now selects the correct case-289 metadata file.

## Most likely cause

The codex CLI inside Modal is still configured to talk to the OpenAI Responses
endpoint instead of the intended provider path, or it is reading a malformed
credential/config combination from the mounted `codex/config.toml`.

The `401 Unauthorized` response indicates this is an auth/configuration
problem, not a Modal network availability problem.

## Next fix

Inspect the mounted `codex/config.toml` and the environment seen by the Modal
container, then align the provider config with the secret that is actually being
injected. After that, rerun the same `e9w2_smoke` path without reseeding.
