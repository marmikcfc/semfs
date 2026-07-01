#!/usr/bin/env python3
"""Score E2B matrix deliverables with the EXISTING Workspace-Bench rubric judge
(`agent_eval.py`) using Seed-2.0-Lite via OpenRouter. Thin glue only — assembles a
task_dir (rubrics metadata.json + the cell's deliverable) per cell and invokes the
real judge; resume-safe (skips cells already scored); retries the Seed-2.0
parse-fail/429 infra errors (runbook: "parse failed for all rubrics" = infra, re-run).

Rubrics: /tmp/wb_lite/task_lite_clean_en/<case>/metadata.json (HF Workspace-Bench-Lite).
Run:  python3 run_judge.py <label> [<label> ...]      # specific cells
      python3 run_judge.py --codex                     # all codex local cells
      python3 run_judge.py --all                        # every cell with a deliverable
Emits rubrics_judge--seed-2.0-lite-judge.json into each cell dir + prints pass/total.
"""
import json, os, sys, shutil, subprocess, pathlib

REPO = pathlib.Path(__file__).resolve().parents[2]
EVAL = REPO / "benchmarks/vendor/Workspace-Bench/evaluation"
JUDGE_YAML = EVAL / "runs/judge.yaml"
RUNS = (pathlib.Path(os.environ["WB_OUT"]).resolve() if os.environ.get("WB_OUT")
        else REPO / "tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs")
WBLITE = pathlib.Path("/tmp/wb_lite/task_lite_clean_en")

env = dict(os.environ)
# .env is gitignored → ABSENT in evo worktrees. Be tolerant: augment from .env when present
# (main repo), else fall back to os.environ (the evo harness injects OPENROUTER_API_KEY there).
_envf = REPO / ".env"
if _envf.exists():
    for line in open(_envf):
        if "=" in line and not line.strip().startswith("#"):
            k, _, v = line.partition("="); env[k.strip()] = v.strip()
ORKEY = env.get("OPENROUTER_API_KEY", "")
# agent_eval.py reads the yaml values LITERALLY (no env expansion), so write a RESOLVED
# config with real values (mirrors the EC2 /tmp/judge_seed.yaml), not the ${..} template.
RESOLVED_YAML = pathlib.Path("/tmp/judge_seed_resolved.yaml")
RESOLVED_YAML.write_text(
    'model_name: "seed-2.0-lite-judge"\n'
    'baseUrl: "https://openrouter.ai/api/v1"\n'
    'model: "bytedance-seed/seed-2.0-lite"\n'
    f'apiKey: "{ORKEY}"\n'
)
JENV = env


def parse_failed(jf):
    """True if every rubric came back as a judge parse error (infra, not a real 0)."""
    try:
        d = json.load(open(jf)); items = d.get("rubrics") or d.get("results") or []
        if not items:
            return True
        ev = " ".join(str(it.get("evidence", "")) for it in items).lower()
        return "parse fail" in ev or "parse failed" in ev
    except Exception:
        return True


def judge(label):
    parts = label.split("_")              # pm_codex_15_nokg_r1
    case = parts[2]
    cell = RUNS / label
    md = WBLITE / case / "metadata.json"
    deliv = cell / "model_output"
    if not md.exists():
        return label, "no-rubrics", None
    if not deliv.exists() or not any(deliv.iterdir()):
        return label, "no-deliverable", None
    existing = list(cell.glob("rubrics_judge--*.json"))
    if existing and not parse_failed(existing[0]):
        s = json.load(open(existing[0])).get("summary", {})
        return label, "cached", s
    # assemble task_dir: metadata.json at root + deliverable under work/ + result.json (trace)
    td = pathlib.Path(f"/tmp/judge/{label}")
    if td.exists():
        shutil.rmtree(td)
    (td / "work").mkdir(parents=True)
    shutil.copy(md, td / "metadata.json")
    if (cell / "result.json").exists():
        shutil.copy(cell / "result.json", td / "result.json")
    # recurse: agents sometimes nest deliverables in subdirs (e.g. report/fy2019/x.docx).
    # the judge matches on basename, so flatten the whole tree into work/ (prefix on collision)
    # — a top-level-only copy silently zeroed nested-directory deliverables.
    seen = set()
    for f in sorted(deliv.rglob("*")):
        if f.is_file():
            name = f.name
            if name in seen:
                name = f"{f.parent.name}__{f.name}"
            seen.add(name)
            shutil.copy(f, td / "work" / name)
    # run the real judge, retry the Seed-2.0 infra flakes
    for attempt in range(4):
        subprocess.run(["python3", "src/agent_eval.py", "--task-dir", str(td),
                        "--eval-yaml", str(RESOLVED_YAML), "--overwrite"],
                       cwd=str(EVAL), env=JENV, capture_output=True, text=True, timeout=420)
        jf = list(td.glob("rubrics_judge--*.json"))
        if jf and not parse_failed(jf[0]):
            shutil.copy(jf[0], cell / jf[0].name)
            return label, f"judged(try{attempt+1})", json.load(open(jf[0])).get("summary", {})
    return label, "PARSE-FAIL-x4", None


def main():
    args = sys.argv[1:]
    if "--codex" in args:
        labels = sorted(p.parent.name for p in RUNS.glob("pm_codex_*/result.json") if "_cloud_" not in p.parent.name)
    elif "--all" in args:
        labels = sorted(p.parent.name for p in RUNS.glob("pm_*/result.json"))
    else:
        labels = args
    print(f"judging {len(labels)} cells with Seed-2.0-Lite via OpenRouter\n")
    rows = []
    for lbl in labels:
        l, status, s = judge(lbl)
        pr = f"{s.get('passed')}/{s.get('total')}" if s else "—"
        print(f"  {l:34} {status:16} rubrics={pr}", flush=True)
        rows.append((l, status, s))
    print("\n=== summary (pass-rate) ===")
    for l, status, s in rows:
        if s and s.get("total"):
            print(f"  {l:34} {s['passed']}/{s['total']}  ({100*s['passed']/s['total']:.0f}%)")


if __name__ == "__main__":
    main()
