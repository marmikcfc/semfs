"""Live re-judger for the PPR A/B run.

The in-process inline judge imported run_judge BEFORE the WB_OUT fix, so it grades
against the wrong dir → all 'no-deliverable'. This separate process uses the fixed
run_judge (RUNS honors WB_OUT), judges every cell that has a deliverable (cached →
fast on re-pass), and ATOMICALLY rewrites judged.jsonl with real scores so the live
dashboard shows the true A/B. Merges persona/arm/rep/tokens from results.jsonl.

Run: WB_OUT=<dir> python3 tickets/wblite-ppr-ab/rejudge_loop.py
"""
import os, sys, json, time, pathlib
from concurrent.futures import ThreadPoolExecutor

REPO = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "benchmarks/e2b"))
os.environ.setdefault("WB_OUT", str(REPO / "tickets/wblite-ppr-ab/artifacts/e2b_runs"))
import run_judge  # noqa: E402  (RUNS now resolves from WB_OUT)
OUT = run_judge.RUNS
print(f"rejudge_loop: OUT={OUT}", flush=True)


def results_index():
    m = {}
    f = OUT / "results.jsonl"
    if f.exists():
        for ln in f.read_text().splitlines():
            try:
                r = json.loads(ln)
                m[r["label"]] = r
            except Exception:
                pass
    return m


def judge_one(label):
    try:
        _, status, score = run_judge.judge(label)
        if isinstance(score, dict) and score.get("total"):
            return label, status, int(score["passed"]), int(score["total"])
    except Exception as e:
        return label, f"err:{repr(e)[:40]}", None, None
    return label, status, None, None


while True:
    idx = results_index()
    # cells that produced a deliverable (judge only those; cached ones return instantly)
    labels = []
    for p in OUT.glob("pm_*/result.json"):
        d = p.parent
        mo = d / "model_output"
        if mo.is_dir() and any(mo.iterdir()):
            labels.append(d.name)
    rows = []
    with ThreadPoolExecutor(max_workers=4) as ex:
        for label, status, passed, total in ex.map(judge_one, sorted(labels)):
            if passed is None or total is None:
                continue
            r = idx.get(label, {})
            rows.append({"label": label, "persona": r.get("persona", ""),
                         "agent": r.get("agent", "codex"), "case": r.get("case", ""),
                         "arm": r.get("arm", ""), "rep": r.get("rep", ""),
                         "judge_status": status, "passed": passed, "total": total,
                         "score": f"{passed}/{total}"})
    # no-deliverable DONE cells (timeout / silent no-output) = genuine task FAILURES → 0/total.
    # Score them so accuracy reflects ALL done cells, not only the ones that produced output
    # (excluding them flatters whichever arm failed to deliver). Rubric total comes from a
    # judged sibling of the same case.
    case_total = {}
    for x in rows:
        if x["total"]:
            case_total[str(x["case"])] = x["total"]
    judged_labels = set(x["label"] for x in rows)
    for label, r in idx.items():
        if label in judged_labels:
            continue
        mo = OUT / label / "model_output"
        if mo.is_dir() and any(mo.iterdir()):
            continue  # has a deliverable but judge errored this pass → retry next pass
        ct = case_total.get(str(r.get("case")))
        if not ct:
            continue  # unknown rubric total → cannot score
        rows.append({"label": label, "persona": r.get("persona", ""), "agent": r.get("agent", "codex"),
                     "case": r.get("case", ""), "arm": r.get("arm", ""), "rep": r.get("rep", ""),
                     "judge_status": "no_deliverable", "passed": 0, "total": ct, "score": f"0/{ct}"})

    tmp = OUT / "judged.jsonl.tmp"
    tmp.write_text("".join(json.dumps(x, ensure_ascii=False) + "\n" for x in rows))
    tmp.replace(OUT / "judged.jsonl")
    # quick A/B line to the log
    agg = {}
    for x in rows:
        a = agg.setdefault(x["arm"], [0, 0])
        a[0] += x["passed"]; a[1] += x["total"]
    summ = "  ".join(f"{k}={v[0]}/{v[1]}={v[0]/v[1]*100:.0f}%" for k, v in sorted(agg.items()) if v[1])
    print(f"{time.strftime('%H:%M:%S')} judged {len(rows)} cells | {summ}", flush=True)
    if "once" in sys.argv:
        break
    time.sleep(120)
