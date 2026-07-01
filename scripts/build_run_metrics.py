#!/usr/bin/env python3
"""
Build a per-run metrics JSONL for the EC2 (E2B) Claude-Code matrix.

Token ground-truth  : openrouter_logs.csv  (grouped per run via the Claude Code `user` hash)
Run label (case/arm): fetched from the dashboard via scripts/or_log.sh  (work_dir in the prompt)
Accuracy            : rubrics_judge--seed-2.0-lite-judge.json per run dir (passed / total)
Run metadata        : result.json per run dir (status, calls, semfs-grep, deliverables, harness usage)

One JSON object per run. Hash->label mapping is cached so re-runs don't re-fetch.
"""
import csv, json, os, re, subprocess, sys, glob
from collections import defaultdict
from datetime import datetime

GAP_S = 300  # >5min gap between a cell's gens => a separate (retry) attempt


def final_cluster(rs):
    """Given a label's gens, return only the LAST time-cluster (the final attempt)."""
    if not rs:
        return rs
    rs = sorted(rs, key=lambda r: r["created_at"])
    last = [rs[0]]
    clusters = [last]
    for a, b in zip(rs, rs[1:]):
        gap = (datetime.fromisoformat(b["created_at"]) - datetime.fromisoformat(a["created_at"])).total_seconds()
        if gap > GAP_S:
            last = [b]; clusters.append(last)
        else:
            last.append(b)
    return clusters[-1], len(clusters)

ROOT = subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip()
CSV = os.path.join(ROOT, "openrouter_logs.csv")
RUNS = os.path.join(ROOT, "tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs")
ORLOG = os.path.join(ROOT, "scripts/or_log.sh")
CACHE = os.path.join(ROOT, "scripts/.run_hash_labels.json")
GEN_CACHE = os.path.join(ROOT, "scripts/.gen_labels.json")
OUT = os.path.join(RUNS, "run_metrics.jsonl")
LABEL_RE = re.compile(r"pm_(claude|codex)_(\d+)_([A-Za-z]+)_r(\d+)")


def ival(x):
    try:
        return int(float(x))
    except (TypeError, ValueError):
        return 0


def fval(x):
    try:
        return float(x)
    except (TypeError, ValueError):
        return 0.0


def load_cache():
    return json.load(open(CACHE)) if os.path.exists(CACHE) else {}


def save_cache(c):
    json.dump(c, open(CACHE, "w"), indent=1)


def fetch_label(gen_id):
    """Return pm_..._rN label for a generation by reading its prompt work_dir."""
    try:
        raw = subprocess.run([ORLOG, gen_id, "--raw"], capture_output=True, text=True, timeout=40).stdout
    except subprocess.TimeoutExpired:
        return None
    m = LABEL_RE.search(raw)
    return m.group(0) if m else None


def main():
    rows = list(csv.DictReader(open(CSV)))
    cc = [r for r in rows if r["app_name"] == "Claude Code"]
    by_user = defaultdict(list)
    for r in cc:
        by_user[r["user"]].append(r)

    # Authoritative grouping: each generation labeled by its OWN work_dir
    # (scripts/.gen_labels.json, built by label_all_gens.py). Falls back to the
    # per-hash cache only for gens that weren't individually labeled.
    gen_labels = json.load(open(GEN_CACHE)) if os.path.exists(GEN_CACHE) else {}
    cache = load_cache()
    label_rows = defaultdict(list)
    unlabeled = []
    for r in cc:
        lbl = gen_labels.get(r["generation_id"]) or cache.get(r["user"])
        (label_rows[lbl] if lbl else unlabeled).append(r)
    if unlabeled:
        print(f"  WARN {len(unlabeled)} gens unlabeled (skipped from token totals)", file=sys.stderr)
    if not gen_labels:
        print("  NOTE .gen_labels.json missing — run scripts/label_all_gens.py first for exact attribution", file=sys.stderr)

    def agg(rs):
        cached = sum(ival(r["tokens_cached"]) for r in rs)
        prompt = sum(ival(r["tokens_prompt"]) for r in rs)
        out = {
            "n_generations": len(rs),
            "cached_tokens": cached,
            "non_cached_input_tokens": prompt - cached,
            "input_tokens": prompt,
            "output_tokens": sum(ival(r["tokens_completion"]) for r in rs),
            "reasoning_tokens": sum(ival(r["tokens_reasoning"]) for r in rs),
            "cost_total_usd": round(sum(fval(r["cost_total"]) for r in rs), 6),
            "by_model": {},
        }
        bm = defaultdict(lambda: defaultdict(int))
        for r in rs:
            m = r["model_permaslug"].split("/")[-1]
            bm[m]["gens"] += 1
            bm[m]["cached"] += ival(r["tokens_cached"])
            bm[m]["input"] += ival(r["tokens_prompt"])
            bm[m]["output"] += ival(r["tokens_completion"])
        out["by_model"] = {m: dict(v) for m, v in bm.items()}
        return out

    def accuracy(label):
        jf = os.path.join(RUNS, label, "rubrics_judge--seed-2.0-lite-judge.json")
        if not os.path.exists(jf):
            return None
        d = json.load(open(jf))
        rubs = d.get("rubrics", [])
        passed = sum(1 for x in rubs if x.get("passed"))
        total = len(rubs)
        return {
            "passed": passed, "total": total,
            "fraction": round(passed / total, 4) if total else None,
            "judge": "seed-2.0-lite", "summary": d.get("summary"),
        }

    def run_meta(label):
        rf = os.path.join(RUNS, label, "result.json")
        if not os.path.exists(rf):
            return {}
        d = json.load(open(rf))
        return {
            "status": d.get("status"), "wall_s": d.get("wall_s"), "calls": d.get("calls"),
            "used_semfs_grep": d.get("used_semfs_grep"),
            "deliverables": d.get("deliverables"),
            "harness_tokens": d.get("tokens"), "harness_usage": d.get("usage"),
            "err": (d.get("err") or "")[:200] or None,
        }

    # universe of runs = every claude run dir + any label seen in the gens
    labels = (set(gen_labels.values()) | set(cache.values())) - {None}
    for d in glob.glob(os.path.join(RUNS, "pm_claude_*")):
        labels.add(os.path.basename(d))

    out_rows = []
    for label in sorted(labels):
        m = LABEL_RE.search(label)
        agent, case, arm, rep = (m.group(1), m.group(2), m.group(3), m.group(4)) if m else (None, None, None, None)
        all_rs = label_rows.get(label, [])
        n_attempts = 0
        orec = None
        if all_rs:
            final_rs, n_attempts = final_cluster(all_rs)
            orec = agg(final_rs)
            orec["n_attempts_in_csv"] = n_attempts
            orec["final_attempt_window"] = [final_rs[0]["created_at"], final_rs[-1]["created_at"]]
        meta = run_meta(label)
        # canonical token block: CSV final-cluster when present, else harness usage
        hu = meta.get("harness_usage") or {}
        if orec:
            tokens = {"cached": orec["cached_tokens"], "non_cached_input": orec["non_cached_input_tokens"],
                      "output": orec["output_tokens"], "reasoning": orec["reasoning_tokens"], "source": "openrouter_csv"}
        elif hu and (hu.get("prompt_tokens") or hu.get("completion_tokens") or hu.get("cache_read")):
            tokens = {"cached": (hu.get("cache_read") or 0) + (hu.get("cache_write") or 0),
                      "non_cached_input": hu.get("prompt_tokens") or 0,
                      "output": hu.get("completion_tokens") or 0, "reasoning": None, "source": "harness_fallback"}
        else:
            tokens = None
        rec = {
            "label": label, "agent": agent, "case": case, "arm": arm, "repeat": int(rep) if rep else None,
            "tokens": tokens,
            "accuracy": accuracy(label),
            "openrouter": orec,
            **meta,
        }
        out_rows.append(rec)

    with open(OUT, "w") as f:
        for r in out_rows:
            f.write(json.dumps(r) + "\n")
    print(f"\nwrote {len(out_rows)} runs -> {OUT}")
    # quick summary
    with_tok = [r for r in out_rows if r["openrouter"]]
    with_acc = [r for r in out_rows if r["accuracy"]]
    print(f"  runs with OpenRouter tokens: {len(with_tok)} | with accuracy: {len(with_acc)}")


if __name__ == "__main__":
    main()
