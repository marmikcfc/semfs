#!/usr/bin/env python3
"""Path 4 — derive the oracle route (cheapest passing tier) per task. Runs free.

  python3 derive_oracle.py <out_dir>
reads <out_dir>/results_judged.jsonl + tasks_prompt.jsonl -> tasks_prompt_labeled.jsonl
"""
import json, sys
from collections import defaultdict
from _common import PRICE, cost_usd

TIER_ORDER = ["haiku", "sonnet", "fable", "opus"]


def derive_oracle(rows, price=PRICE):
    """rows = judged result rows for ONE task. Cheapest scoring==1 tier by computed cost."""
    passed = [r for r in rows if r.get("score") == 1]
    if not passed:
        return {"oracle_route": "none", "oracle_effort": None, "label_confidence": None}
    for r in passed:
        r["_cost"] = cost_usd(r["model"], r.get("input_tokens", 0) or 0, r.get("output_tokens", 0) or 0, price)
    best = min(passed, key=lambda r: (r["_cost"], TIER_ORDER.index(r["model"])))
    conf = best.get("n_judges")
    return {"oracle_route": best["model"], "oracle_effort": best.get("effort"), "label_confidence": conf}


def main():
    out = sys.argv[1]
    by = defaultdict(list)
    for line in open(f"{out}/results_judged.jsonl"):
        r = json.loads(line)
        by[r["task_id"]].append(r)
    tasks = [json.loads(l) for l in open(f"{out}/tasks_prompt.jsonl")]
    dist = defaultdict(int)
    with open(f"{out}/tasks_prompt_labeled.jsonl", "w") as f:
        for t in tasks:
            oc = derive_oracle(by.get(t["task_id"], []))
            t.update(oc)
            dist[oc["oracle_route"]] += 1
            f.write(json.dumps(t) + "\n")
    print("oracle_route distribution:", dict(dist))


if __name__ == "__main__":
    main()
