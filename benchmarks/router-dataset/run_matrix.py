#!/usr/bin/env python3
"""Path 2 — label the matrix: each task through the model×effort grid via the OAuth token.
Task-by-task ATOMIC checkpoint -> quota exhaustion loses tasks, never leaves half-labeled ones.

  python3 run_matrix.py <out_dir> [--limit N] [--per-dim N] [--skip-dims a,b]
reads <out_dir>/tasks_prompt.jsonl -> results.jsonl (resumable). No judge (deferred).

Effort is model-gated: `output_config.effort` works on sonnet/opus only; haiku has no effort
axis (API: "This model does not support the effort parameter"), so it runs once.
"""
import json, sys, time, urllib.error
from collections import defaultdict
from _common import read_env, MODELS, CC_SYSTEM

OAUTH = read_env("CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN")
HDR = {"authorization": f"Bearer {OAUTH}", "anthropic-version": "2023-06-01",
       "anthropic-beta": "oauth-2025-04-20", "content-type": "application/json"}
EFFORTS = {"haiku": [None], "sonnet": ["low", "medium", "high"], "opus": ["low", "medium", "high"]}


def already_done(path):
    done = set()
    try:
        for line in open(path):
            done.add(json.loads(line)["task_id"])
    except FileNotFoundError:
        pass
    return done


def select_tasks(tasks, per_dim, skip):
    by = defaultdict(list)
    for t in tasks:
        if t["dimension"] in skip:
            continue
        by[t["dimension"]].append(t)
    return [t for ts in by.values() for t in ts[:per_dim]]


def call_oauth(model_key, system, context, prompt, effort=None, max_tokens=2048):
    import urllib.request
    content = f"{context}\n\n---\n\n{prompt}" if context else prompt
    payload = {"model": MODELS[model_key], "max_tokens": max_tokens, "system": system,
               "messages": [{"role": "user", "content": content}]}
    if effort:
        payload["output_config"] = {"effort": effort}
    body = json.dumps(payload).encode()
    t0 = time.monotonic()
    for attempt in range(5):
        try:
            req = urllib.request.Request("https://api.anthropic.com/v1/messages", data=body, headers=HDR)
            with urllib.request.urlopen(req, timeout=240) as r:
                d = json.load(r)
            u = d.get("usage", {})
            ans = "".join(b.get("text", "") for b in d.get("content", []) if b.get("type") == "text")
            return {"answer": ans, "input_tokens": u.get("input_tokens", 0),
                    "cached_tokens": (u.get("cache_read_input_tokens", 0) or 0) + (u.get("cache_creation_input_tokens", 0) or 0),
                    "output_tokens": u.get("output_tokens", 0),
                    "latency_ms": int((time.monotonic() - t0) * 1000), "error": None}
        except urllib.error.HTTPError as e:
            if e.code == 429 or e.code >= 500:
                time.sleep(2 ** attempt); continue
            return {"answer": "", "error": f"HTTP {e.code}: {e.read()[:200].decode('utf-8','replace')}",
                    "latency_ms": int((time.monotonic() - t0) * 1000)}
        except Exception as e:
            return {"answer": "", "error": str(e)[:200], "latency_ms": int((time.monotonic() - t0) * 1000)}
    return {"answer": "", "error": "429/5xx after retries", "latency_ms": int((time.monotonic() - t0) * 1000)}


def main():
    out = sys.argv[1]; a = sys.argv
    limit = int(a[a.index("--limit") + 1]) if "--limit" in a else None
    per_dim = int(a[a.index("--per-dim") + 1]) if "--per-dim" in a else None
    skip = set(a[a.index("--skip-dims") + 1].split(",")) if "--skip-dims" in a else (
        {"perf_optimization"} if per_dim else set())
    rpath = f"{out}/results.jsonl"
    done = already_done(rpath)
    tasks = [json.loads(l) for l in open(f"{out}/tasks_prompt.jsonl")]
    if per_dim:
        tasks = select_tasks(tasks, per_dim, skip)
    elif limit:
        tasks = tasks[:limit]
    per_task = sum(len(EFFORTS[m]) for m in ("haiku", "sonnet", "opus"))
    print(f"pilot: {len(tasks)} tasks x {per_task} calls = {len(tasks) * per_task} rows  (skip={sorted(skip)})")
    with open(rpath, "a") as f:
        for i, t in enumerate(tasks):
            if t["task_id"] in done:
                continue
            batch = []
            for mk in ("haiku", "sonnet", "opus"):
                for eff in EFFORTS[mk]:
                    r = call_oauth(mk, CC_SYSTEM, t.get("prior_context", ""), t.get("prompt", ""), effort=eff)
                    batch.append({"task_id": t["task_id"], "dimension": t["dimension"],
                                  "difficulty": t["difficulty"], "context_bucket": t["context_bucket"],
                                  "split": t["split"], "model": mk, "effort": eff or "none", **r,
                                  "total_tokens": (r.get("input_tokens", 0) or 0) + (r.get("output_tokens", 0) or 0)})
                    time.sleep(0.4)
            for row in batch:               # atomic: commit all rows for this task together
                f.write(json.dumps(row) + "\n")
            f.flush()
            errs = sum(1 for b in batch if b.get("error"))
            print(f"[{i+1}/{len(tasks)}] {t['dimension'][:18]:18s} …{t['task_id'][-8:]} ({errs} err)")
    print("results ->", rpath)


if __name__ == "__main__":
    main()
