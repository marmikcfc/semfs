#!/usr/bin/env python3
"""Config-C query shim — RRF-merge two colgrep indices (code lane + doc lane).

The agent's `semfs grep` routes here (via semfs-np with WB_NP_MERGE=1) for dual-model
cells (kaifa-C). It queries each lane's index with its OWN model AND its own index store
(XDG_DATA_HOME), then fuses the two ranked lists with Reciprocal Rank Fusion (rank-based →
scale-invariant, so the two models' MaxSim scales need not match). Output is the colgrep
human format — `abspath:start-end` lines, ranked (top = best) — so the agent reads it
exactly like a single-lane colgrep result and `cat`s the absolute paths directly.

Env:
  NP_CODE_DIR, NP_CODE_MODEL, NP_CODE_XDG   code-lane corpus dir + model + index store
  NP_DOC_DIR,  NP_DOC_MODEL,  NP_DOC_XDG    doc-lane  corpus dir + model + index store
  NP_TOPK (default 10) · NP_RRF_K (default 60) · NP_COLGREP (default 'colgrep')
Usage: rrf_merge.py "your query"
"""
import os, sys, json, subprocess

K = int(os.environ.get("NP_RRF_K", "60"))
TOPK = int(os.environ.get("NP_TOPK", "10"))
COLGREP = os.environ.get("NP_COLGREP", "colgrep")
LANES = [
    ("code", os.environ.get("NP_CODE_DIR"), os.environ.get("NP_CODE_MODEL"), os.environ.get("NP_CODE_XDG")),
    ("doc",  os.environ.get("NP_DOC_DIR"),  os.environ.get("NP_DOC_MODEL"),  os.environ.get("NP_DOC_XDG")),
]


def run_lane(project, model, xdg, query):
    if not project or not model:
        return []
    env = dict(os.environ)
    if xdg:
        env["XDG_DATA_HOME"] = xdg          # each lane's index lives under its OWN store
    r = subprocess.run([COLGREP, "--model", model, "--json", query],
                       cwd=project, capture_output=True, text=True, env=env)
    try:
        out = json.loads(r.stdout)
        return out if isinstance(out, list) else out.get("results", [])
    except Exception:
        sys.stderr.write(f"[rrf_merge] lane {project} parse fail: {r.stderr[:300]}\n")
        return []


def unit_of(res):
    return res.get("unit", res) if isinstance(res, dict) else {}


def file_of(res):
    u = unit_of(res)
    return u.get("file") or u.get("path") or u.get("qualified_name")


def range_of(res):
    u = unit_of(res)
    s = u.get("start_line") or u.get("start") or u.get("line_start")
    e = u.get("end_line") or u.get("end") or u.get("line_end")
    if (s is None or e is None) and isinstance(u.get("lines"), (list, tuple)) and u["lines"]:
        s, e = u["lines"][0], u["lines"][-1]
    return s, e


def main():
    query = " ".join(sys.argv[1:]).strip()
    if not query:
        print(""); return
    scores, best = {}, {}
    for lane, project, model, xdg in LANES:
        for rank, res in enumerate(run_lane(project, model, xdg, query), 1):
            f = file_of(res)
            if not f:
                continue
            key = (project, f)                      # dedup within a lane; both lanes compete in RRF
            scores[key] = scores.get(key, 0.0) + 1.0 / (K + rank)
            best.setdefault(key, res)
    ranked = sorted(scores, key=lambda k: -scores[k])[:TOPK]
    out = []
    for (project, f) in ranked:
        s, e = range_of(best[(project, f)])
        ap = f if str(f).startswith("/") else os.path.join(project, f)   # absolute → agent cats directly across both corpora
        out.append(f"{ap}:{s}-{e}" if s and e else ap)
    print("\n".join(out))


if __name__ == "__main__":
    main()
