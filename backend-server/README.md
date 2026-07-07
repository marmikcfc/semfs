# tokopt backend server

The hosted "default" endpoint `crates/tokopt-gateway` falls back to when a user
hasn't set `ROUTER_*`/`COMPRESSOR_*` env vars themselves. Not something end users
install — this is a centrally-run service, unlike the per-user local gateway.

One endpoint does both jobs: `POST /optimize`. No separate compress/route calls
to keep in sync, no caller-supplied model to validate (the routing decision is
always our own model, chosen internally — nothing for a caller to smuggle through).

## Run it

```
cd backend-server
pip install -r requirements.txt
cp .env.example .env   # fill in UPSTREAM_ROUTER_API_KEY for real routing
python3 server.py
curl localhost:8788/health
```

## Endpoints

- `GET /health` — reports whether the compressor loaded and whether a router
  upstream key is configured. Never fabricates a working state.
- `POST /optimize` `{text, compress?, route?, threshold?}` →
  `{compressed_context, relevant_model, stats}`. Runs compression (real
  kompress-v2-base ONNX) and routing (a real LLM call, mapped to a real
  `claude-*` model id) concurrently. Pass `compress: false` or `route: false`
  to skip either half — used when only one side of a caller's config is
  default-hosted and the other is BYO'd elsewhere.
- `POST /usage` — a gateway instance reports its own local usage counters here
  (no auth; counters only, no request content) so aggregate stats cover BYO
  users too, not just requests that hit this backend directly.
- `GET /usage` — aggregate metrics (requests/status/latency/chars by endpoint,
  1h/24h/7d windows). Requires `Authorization: Bearer <ADMIN_TOKEN>`; 404s
  entirely if `ADMIN_TOKEN` is unset, rather than being readable by default.

Rate-limited per IP (`RATE_LIMIT_PER_MIN`) since `/optimize` spends our own
router API key on behalf of anyone who reaches it without bringing their own.

## Not done here

**Deployment.** This is built and tested to run locally; it isn't deployed anywhere.
Standing it up somewhere reachable (Railway, Modal, a VPS — whatever) is a separate
decision: it's an always-on service with a real ongoing cost and a real API key to
protect, not a one-off job. Pick a platform and say go when ready.
