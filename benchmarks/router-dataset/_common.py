"""Shared helpers for the router-dataset generation harness (SEM-52)."""
import json, os, urllib.error, urllib.request

CC_SYSTEM = "You are Claude Code, Anthropic's official CLI for Claude."
MODELS = {"haiku": "claude-haiku-4-5-20251001", "sonnet": "claude-sonnet-5",
          "opus": "claude-opus-4-8", "fable": "claude-fable-5"}
# published $/Mtok (in, out) — RANKING ONLY (OAuth bills flat). Order: haiku<sonnet<fable<opus.
PRICE = {"haiku": (0.8, 4.0), "sonnet": (3.0, 15.0), "fable": (5.0, 25.0), "opus": (15.0, 75.0)}
JUDGE_MODEL = "deepseek/deepseek-v3.2"  # DeepSeek flagship (no literal "pro" id); swappable


def read_env(name, *alts):
    for k in (name,) + alts:
        v = os.environ.get(k)
        if v:
            return v
    return ""


def count_tokens(s):
    s = s or ""
    try:
        import tiktoken
        return len(tiktoken.get_encoding("cl100k_base").encode(s))
    except Exception:
        return max(1, len(s) // 4)


def http_json(url, body=None, headers=None, method=None, timeout=120):
    """GET (no body) or POST (json body). Raises urllib.error.HTTPError on non-2xx."""
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, headers=headers or {},
                                 method=method or ("POST" if data else "GET"))
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.load(r)


def cost_usd(model, in_tok, out_tok, price=PRICE):
    pi, po = price[model]
    return in_tok / 1e6 * pi + out_tok / 1e6 * po
