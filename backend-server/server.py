#!/usr/bin/env python3
"""
tokopt backend server — the hosted "default" endpoint for the tokopt-gateway plugin.

One job, one endpoint: `POST /optimize` takes the text of the user's latest
message and returns a compressed version of it plus which real Claude model is
the right fit for it. That's it — no separate /compress and /route calls to
keep in sync, no caller-supplied model to validate against an allowlist (the
routing decision is always OUR OWN internal call to OUR OWN configured model,
so there's nothing for a caller to smuggle through).

Endpoints:
  GET  /health   -> which pieces are actually configured/loaded
  POST /optimize  {text, compress?, route?, threshold?}
                  -> {compressed_context, relevant_model, stats}
                  Set compress=false / route=false to skip either half (used
                  when only one side of the caller's config is default-hosted
                  and the other is BYO'd elsewhere).
  POST /usage     ingest a usage event reported by a gateway instance's local
                  SQLite (no auth — these are aggregate counters, not content).
  GET  /usage     aggregate metrics; requires `Authorization: Bearer
                  <ADMIN_TOKEN>` — disabled entirely if ADMIN_TOKEN unset.

Config via env / ./.env:
  PORT                        default 8788
  HOST                        default 127.0.0.1 (loopback-only; widening to
                               0.0.0.0 is a deploy-time decision, not a default)
  KOMPRESS_MODEL               default chopratejas/kompress-v2-base
  KOMPRESS_ONNX                default onnx/kompress-fp32.onnx
  KOMPRESS_THRESHOLD           default 0.5
  UPSTREAM_ROUTER_BASE_URL     default https://api.openai.com/v1
  UPSTREAM_ROUTER_API_KEY      the operator's real key — MUST be set at deploy
                               time, never fabricated or defaulted here.
  UPSTREAM_ROUTER_MODEL        default gpt-4.1-nano — the model THIS backend
                               uses internally to make the routing decision.
  RATE_LIMIT_PER_MIN           per-IP sliding window, default 30
  ADMIN_TOKEN                  gates GET /usage; unset -> 404
  METRICS_DB                   default ./usage.db

Run: python3 server.py
"""
from __future__ import annotations
import json, logging, os, re, threading, time, urllib.request
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import metrics

HERE = Path(__file__).resolve().parent


def load_dotenv(path: Path) -> None:
    if not path.exists():
        return
    for raw in path.read_text().splitlines():
        line = raw.strip()
        if line and not line.startswith("#") and "=" in line:
            k, _, v = line.partition("=")
            os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))


load_dotenv(HERE / ".env")
logging.basicConfig(level=os.environ.get("LOG_LEVEL", "INFO"),
                    format="%(asctime)s  %(levelname)-5s  %(message)s")
log = logging.getLogger("tokopt-backend")

PORT = int(os.environ.get("PORT", "8788"))
# Loopback by default — /optimize is unauthenticated (the rate limit bounds
# cost, it doesn't gate access). Widening this is a real deploy-time decision.
HOST = os.environ.get("HOST", "127.0.0.1")
KOMPRESS_MODEL = os.environ.get("KOMPRESS_MODEL", "chopratejas/kompress-v2-base")
KOMPRESS_ONNX = os.environ.get("KOMPRESS_ONNX", "onnx/kompress-fp32.onnx")
KOMPRESS_THRESHOLD = float(os.environ.get("KOMPRESS_THRESHOLD", "0.5"))
UPSTREAM_ROUTER_BASE_URL = os.environ.get("UPSTREAM_ROUTER_BASE_URL", "https://api.openai.com/v1").rstrip("/")
UPSTREAM_ROUTER_API_KEY = os.environ.get("UPSTREAM_ROUTER_API_KEY", "")
UPSTREAM_ROUTER_MODEL = os.environ.get("UPSTREAM_ROUTER_MODEL", "gpt-4.1-nano")
RATE_LIMIT_PER_MIN = int(os.environ.get("RATE_LIMIT_PER_MIN", "30"))
ADMIN_TOKEN = os.environ.get("ADMIN_TOKEN", "")
METRICS_DB = os.environ.get("METRICS_DB", str(HERE / "usage.db"))

METRICS = metrics.Metrics(METRICS_DB)
POOL = ThreadPoolExecutor(max_workers=8, thread_name_prefix="optimize")

# Real Anthropic model IDs the router can pick between. Kept here (not just on
# the gateway) because /optimize returns a REAL model id ready to drop into an
# api.anthropic.com request — the caller shouldn't need its own copy of this
# table for the default-hosted path.
TIERS = {
    "cheap": "claude-haiku-4-5-20251001",
    "mid": "claude-sonnet-5",
    "strong": "claude-opus-4-8",
}


# ----------------------------- compression ------------------------------------
class Compressor:
    """kompress-v2 ONNX (local to this process). No heuristic fallback here — if it
    can't load, /optimize reports compression unconfigured rather than silently
    degrading, since the gateway already has its own passthrough for that case."""
    def __init__(self):
        self.backend = None
        self.tok = None
        self.sess = None
        self._np = None
        self._inputs: set[str] = set()
        self._try_load()

    def _try_load(self):
        try:
            import onnxruntime as ort
            import numpy as np
            from huggingface_hub import hf_hub_download
            from transformers import AutoTokenizer
            path = hf_hub_download(KOMPRESS_MODEL, KOMPRESS_ONNX)
            self.sess = ort.InferenceSession(path, providers=["CPUExecutionProvider"])
            self.tok = AutoTokenizer.from_pretrained(KOMPRESS_MODEL)
            self._np = np
            self._inputs = {i.name for i in self.sess.get_inputs()}
            self.backend = "kompress-onnx"
            log.info("compressor loaded: %s (%s), inputs=%s", KOMPRESS_MODEL, KOMPRESS_ONNX, self._inputs)
        except Exception as e:  # noqa: BLE001
            log.warning("compressor load failed (%s) -> /optimize will report unconfigured", str(e)[:200])

    def compress(self, text: str, threshold: float) -> str:
        np = self._np
        enc = self.tok(text, return_tensors="np", truncation=True, max_length=8192)
        feed = {k: v for k, v in enc.items() if k in self._inputs}
        raw = np.array(self.sess.run(None, feed)[0])[0]  # [seq] P(keep)
        keep = np.clip(raw, 0, 1) if (raw.min() >= 0 and raw.max() <= 1) else 1.0 / (1.0 + np.exp(-raw))
        ids = enc["input_ids"][0].tolist()
        kept = [int(i) for i, k in zip(ids, (keep >= threshold).tolist()) if k]
        out = self.tok.decode(kept, skip_special_tokens=True).strip()
        return out or text


COMPRESSOR = Compressor()


# ------------------------------- routing ---------------------------------------
def heuristic_route(task: str) -> str:
    """Fail-open backstop when the upstream LLM call isn't configured or fails."""
    t = (task or "").lower()
    strong = ("architecture", "refactor", "debug", "root cause", "design", "multi-file",
              "concurrency", "race condition", "migrate", "security", "optimize", "why",
              "investigate")
    cheap = ("rename", "typo", "format", "comment", "docstring", "small", "simple",
             "list ", "read ", "print", "add a test", "boilerplate")
    if any(w in t for w in strong):
        return "strong"
    if any(w in t for w in cheap) or len(t) < 80:
        return "cheap"
    return "mid"


def llm_route(task: str) -> tuple[str, str]:
    """Ask UPSTREAM_ROUTER_MODEL to pick a tier. Returns (tier, source)."""
    if not UPSTREAM_ROUTER_API_KEY:
        return heuristic_route(task), "heuristic"
    try:
        names = list(TIERS.keys())
        schema = {"type": "object", "additionalProperties": False,
                  "properties": {"tier": {"type": "string", "enum": names}}, "required": ["tier"]}
        body = json.dumps({
            "model": UPSTREAM_ROUTER_MODEL, "max_tokens": 20, "temperature": 0.0,
            "messages": [
                {"role": "system", "content": "Pick the single best tier for this task: "
                                               "cheap=simple/small/factual, mid=moderate multi-step, "
                                               "strong=architecture/debugging/ambiguous."},
                {"role": "user", "content": task},
            ],
            "response_format": {"type": "json_schema",
                                "json_schema": {"name": "tier", "strict": True, "schema": schema}},
        }).encode()
        req = urllib.request.Request(
            UPSTREAM_ROUTER_BASE_URL + "/chat/completions", data=body,
            headers={"Authorization": f"Bearer {UPSTREAM_ROUTER_API_KEY}", "Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=20) as r:
            resp = json.load(r)
        content = resp["choices"][0]["message"]["content"]
        tier = json.loads(content).get("tier")
        return (tier if tier in TIERS else heuristic_route(task)), "llm"
    except Exception as e:  # noqa: BLE001
        log.warning("llm_route failed (%s) -> heuristic", str(e)[:160])
        return heuristic_route(task), "heuristic"


# ------------------------------- rate limiting ---------------------------------
class RateLimiter:
    """Sliding-window per-IP limiter. Simple and in-memory — this process is a
    single instance, not a fleet; good enough to bound worst-case spend."""
    def __init__(self, limit_per_min: int):
        self.limit = limit_per_min
        self._hits: dict[str, list[float]] = {}
        self._lock = threading.Lock()

    def allow(self, key: str) -> bool:
        now = time.time()
        cutoff = now - 60.0
        with self._lock:
            hits = [t for t in self._hits.get(key, []) if t > cutoff]
            if len(hits) >= self.limit:
                self._hits[key] = hits
                return False
            hits.append(now)
            self._hits[key] = hits
            return True


RATE_LIMITER = RateLimiter(RATE_LIMIT_PER_MIN)


# ------------------------------- HTTP ----------------------------------------
class Handler(BaseHTTPRequestHandler):
    def _send(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *a):
        log.info("%s - %s", self.address_string(), fmt % a)

    def _body(self):
        n = int(self.headers.get("Content-Length", "0") or "0")
        return json.loads(self.rfile.read(n) or b"{}") if n else {}

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/health":
            self._send(200, {
                "status": "ok",
                "compressor": {"model": KOMPRESS_MODEL, "backend": COMPRESSOR.backend or "unconfigured"},
                "router": {
                    "upstream_base_url": UPSTREAM_ROUTER_BASE_URL,
                    "upstream_model": UPSTREAM_ROUTER_MODEL,
                    "key_loaded": bool(UPSTREAM_ROUTER_API_KEY),
                    "tiers": TIERS,
                },
            })
        elif path == "/usage":
            if not ADMIN_TOKEN:
                return self._send(404, {"error": "not found"})
            auth = self.headers.get("Authorization", "")
            if auth != f"Bearer {ADMIN_TOKEN}":
                return self._send(401, {"error": "unauthorized"})
            self._send(200, METRICS.summary())
        else:
            self._send(404, {"error": "not found", "path": self.path})

    def do_POST(self):
        path = self.path.split("?")[0]
        try:
            req = self._body()
        except Exception as e:  # noqa: BLE001
            return self._send(400, {"error": f"invalid JSON: {e}"})

        if path == "/optimize":
            return self._handle_optimize(req)
        if path == "/usage":
            return self._handle_usage_ingest(req)
        self._send(404, {"error": "not found", "path": self.path})

    def _handle_optimize(self, req):
        start = time.monotonic()
        text = req.get("text", "")
        want_compress = bool(req.get("compress", True))
        want_route = bool(req.get("route", True))
        threshold = float(req.get("threshold", KOMPRESS_THRESHOLD))
        client_ip = self.client_address[0]

        if not RATE_LIMITER.allow(client_ip):
            METRICS.record("optimize", model="", status="rate_limited", latency_ms=0, chars_in=len(text), chars_out=0)
            return self._send(429, {"error": "rate limit exceeded on the hosted default endpoint"})

        compressed = None
        route_error = None
        compress_error = None
        futures = {}
        if want_compress and text.strip():
            if COMPRESSOR.backend:
                futures["compress"] = POOL.submit(COMPRESSOR.compress, text, threshold)
            else:
                compress_error = "compressor unconfigured on this backend"
        if want_route and text.strip():
            futures["route"] = POOL.submit(llm_route, text)

        tier, route_source, relevant_model = None, None, None
        for name, fut in futures.items():
            try:
                if name == "compress":
                    compressed = fut.result(timeout=25)
                elif name == "route":
                    tier, route_source = fut.result(timeout=25)
                    relevant_model = TIERS[tier]
            except Exception as e:  # noqa: BLE001
                log.warning("%s failed (%s)", name, str(e)[:160])
                if name == "compress":
                    compress_error = str(e)[:160]
                else:
                    route_error = str(e)[:160]

        latency_ms = int((time.monotonic() - start) * 1000)
        status = "ok" if not (compress_error or route_error) else "partial"
        METRICS.record("optimize", model=relevant_model or "", status=status, latency_ms=latency_ms,
                       chars_in=len(text), chars_out=len(compressed) if compressed else 0)

        self._send(200, {
            "compressed_context": compressed,
            "relevant_model": relevant_model,
            "stats": {
                "orig_chars": len(text),
                "kept_chars": len(compressed) if compressed else None,
                "ratio": round(len(compressed) / max(1, len(text)), 3) if compressed else None,
                "tier": tier,
                "route_source": route_source,
                "compress_error": compress_error,
                "route_error": route_error,
            },
        })

    def _handle_usage_ingest(self, req):
        """A gateway instance reports its own local usage here so aggregate stats
        aren't limited to requests that actually hit this backend (BYO users'
        local savings count too). No auth — these are just counters."""
        METRICS.record(
            req.get("endpoint", "gateway_local"),
            model=req.get("model", ""), status=req.get("status", "ok"),
            latency_ms=int(req.get("latency_ms", 0)),
            chars_in=int(req.get("chars_in", 0)), chars_out=int(req.get("chars_out", 0)),
        )
        self._send(200, {"ok": True})


def main():
    log.info("tokopt-backend on %s:%d | compressor=%s | router upstream=%s model=%s (key_loaded=%s)",
             HOST, PORT, COMPRESSOR.backend or "unconfigured", UPSTREAM_ROUTER_BASE_URL,
             UPSTREAM_ROUTER_MODEL, bool(UPSTREAM_ROUTER_API_KEY))
    ThreadingHTTPServer((HOST, PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
