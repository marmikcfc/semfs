#!/usr/bin/env python3
"""Collect ALL historical benchmark cells into one clean dataset for PostHog backfill.

Per cell we join:
  - result.json            → agent, arm, case, tokens, calls, status, used_semfs_grep, deliverables
  - rubrics_judge*.json     → summary.{passed,total} → accuracy

Emits benchmarks/e2b/experiments_dataset.jsonl (one event per cell) + prints an arm summary.
"""
import json, os, glob, datetime

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))  # benchmarks/
ART = os.path.join(ROOT, "..", "tickets", "workspace-bench-5arm-matrix", "artifacts")
ART = os.path.normpath(ART)
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "experiments_dataset.jsonl")


def judge_accuracy(cell_dir):
    """Return (passed, total) from any rubrics_judge*.json in the cell dir, else (None, None)."""
    for jf in glob.glob(os.path.join(cell_dir, "rubrics_judge*.json")):
        try:
            d = json.load(open(jf, encoding="utf-8"))
            s = d.get("summary") or {}
            if "total" in s:
                return s.get("passed"), s.get("total"), d.get("createdAt")
        except Exception:
            continue
    return None, None, None


def collect():
    rows = []
    # every cell dir = one with a result.json under artifacts/
    for rj in glob.glob(os.path.join(ART, "**", "result.json"), recursive=True):
        cell = os.path.dirname(rj)
        try:
            r = json.load(open(rj, encoding="utf-8"))
        except Exception:
            continue
        if not r.get("agent"):
            continue
        passed, total, judged_at = judge_accuracy(cell)
        acc = (passed / total) if (passed is not None and total) else None
        # run_set = the artifacts subdir family (e2b_runs / run2/e8 / run5arm ...)
        rel = os.path.relpath(cell, ART)
        run_set = rel.split(os.sep)[0] if os.sep in rel else "e2b_runs"
        if run_set == os.path.basename(cell):  # cell directly under artifacts
            run_set = "root"
        ts = judged_at or datetime.datetime.utcfromtimestamp(os.path.getmtime(rj)).isoformat() + "Z"
        rows.append({
            "run_set": run_set,
            "label": r.get("label") or os.path.basename(cell),
            "agent": r.get("agent"),
            "arm": r.get("arm"),
            "case": str(r.get("case")),
            "tokens": r.get("tokens"),
            "calls": r.get("calls"),
            "status": r.get("status"),
            "used_semfs_grep": r.get("used_semfs_grep"),
            "auth_used": r.get("auth_used"),
            "deliverable_count": len(r.get("deliverables") or []),
            "rubrics_passed": passed,
            "rubrics_total": total,
            "accuracy": round(acc, 4) if acc is not None else None,
            "judged": acc is not None,
            "timestamp": ts,
        })
    return rows


def main():
    rows = collect()
    with open(OUT, "w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    judged = [r for r in rows if r["judged"]]
    print(f"cells total={len(rows)}  judged={len(judged)}  → {OUT}")
    # arm × agent summary over JUDGED cells (mean accuracy, mean tokens)
    import collections
    agg = collections.defaultdict(lambda: {"n": 0, "acc": 0.0, "tok": 0})
    for r in judged:
        if r["tokens"] is None:
            continue
        k = (r["run_set"], r["agent"], r["arm"])
        agg[k]["n"] += 1
        agg[k]["acc"] += r["accuracy"]
        agg[k]["tok"] += r["tokens"]
    print(f"\n{'run_set':12s} {'agent':7s} {'arm':8s} {'n':>3s} {'mean_acc':>9s} {'mean_tok':>10s}")
    for k in sorted(agg):
        a = agg[k]
        print(f"{k[0]:12s} {k[1]:7s} {k[2]:8s} {a['n']:>3d} {100*a['acc']/a['n']:>8.1f}% {a['tok']//a['n']:>10,}")


if __name__ == "__main__":
    main()
