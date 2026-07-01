#!/usr/bin/env python3
"""Plain/FS baseline on SEPARATE per-persona templates (plain-xafs-<dp>).

Each persona has its OWN E2B template (corpus-only, no semfs seed). Per cell we boot that
persona's template, extract its corpus, and run the agent with real find/grep/cat (cell_driver
--arm plain). This is the literal "13 separate plain templates" structure (vs the earlier
symlink-on-one-combined-template approach). Writes _xafs_perdp_plain.json → dashboard plain col.
"""
import os, sys, json, time, argparse, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import run_judge_xafs as J
from run_xafs_slice import sh, save_results, load_slice, CELL_DRIVER, CODEX_AUTH
from e2b import Sandbox

REPO = pathlib.Path(__file__).resolve().parents[2]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dps", default=",".join(f"dp_{i:03d}" for i in range(1, 14)))
    ap.add_argument("--agent-timeout", default="600")
    args = ap.parse_args()
    orkey = os.environ["OPENROUTER_API_KEY"]
    model = os.environ.get("WB_CODEX_MODEL", "gpt-5.4-mini")
    dp_set = [d.strip() for d in args.dps.split(",") if d.strip()]
    cells = [c for c in load_slice() if c["dp"] in dp_set]
    print(f"xAFS plain (separate templates): {len(cells)} cells · agent=codex/{model}", flush=True)

    rp = REPO / "benchmarks/e2b/_xafs_perdp_plain.json"
    results, done = [], set()
    if rp.exists():
        for r in json.loads(rp.read_text()).get("results", []):
            if r.get("status") not in (None, "ERROR"):
                results.append(r); done.add((r["dp"], r["qid"]))
        print(f"resume: {len(done)} good cells already saved", flush=True)

    for i, c in enumerate(cells):
        if (c["dp"], c["qid"]) in done:
            print(f"[{i+1}/{len(cells)}] {c['dp']}_{c['qid']} — done, skip", flush=True)
            continue
        dp = c["dp"]; label = f"{dp}_{c['qid']}"; tmpl = f"plain-xafs-{dp}"
        t0 = time.time(); sbx = None
        try:
            sbx = Sandbox.create(template=tmpl, timeout=int(args.agent_timeout) + 600)
            print(f"\n[{i+1}/{len(cells)}] {label} [{c['family']}] template={tmpl} sbx={sbx.sandbox_id}", flush=True)
            sh(sbx, "mkdir -p ~/ws ~/run ~/cases ~/.codex && ln -sfn /opt/wb ~/wb && "
                    "ln -sfn /opt/gemma_q4 ~/gemma_q4", timeout=120)
            sbx.files.write("/home/user/cell_driver.py", CELL_DRIVER.read_text())
            sbx.files.write("/home/user/.codex/auth.json", CODEX_AUTH.read_text())
            # extract THIS persona's raw corpus (arcname {dp}_standard) → real file tree
            sh(sbx, "mkdir -p ~/ws/plain_all && tar xzf /opt/corpus.tgz -C ~/ws/plain_all", timeout=600)
            sh(sbx, f"ln -sfn /home/user/ws/plain_all/{dp}_standard /home/user/ws/plain", timeout=30)
            root, _ = sh(sbx, "ls /home/user/ws/plain/ 2>&1 | head -5")
            print(f"   plain tree: {root.strip()[:120]!r}", flush=True)
            cenv = {"OPENROUTER_API_KEY": orkey, "WB_CODEX_MODEL": model,
                    "WB_OUTPUT_FILES": "answer.md", "WB_AGENT_TIMEOUT": args.agent_timeout,
                    "WB_READ_PATHS": "/home/user/ws/plain"}
            task = (f"{c['prompt']}\n\n[Scope] This question is about the workspace under "
                    f"/home/user/ws/plain/ — focus your search there. Write your final "
                    f"answer (the value asked for) to model_output/answer.md.")
            sbx.files.write(f"/home/user/cases/{label}.task", task)
            print(f"   running codex…", flush=True)
            o, e = sh(sbx, f"cd /home/user && python3 cell_driver.py --label {label} "
                           f"--agent codex --case {label} --arm plain 2>&1 | tail -3",
                      timeout=int(args.agent_timeout) + 300, env=cenv)
            res = {}
            for line in (o + e).splitlines():
                if line.startswith("RESULT="):
                    try: res = json.loads(line[len("RESULT="):])
                    except Exception: pass
            ans, _ = sh(sbx, f"cat /home/user/run/{label}/model_output/answer.md 2>/dev/null || true", timeout=60)
            ans = ans.strip()
            v = J.grade_one(c["prompt"], c["gold_answer"], ans, api_key=orkey) if ans else \
                {"correct": False, "reason": "no answer.md"}
            rec = {"dp": dp, "qid": c["qid"], "family": c["family"], "arm": "plain",
                   "status": res.get("status"), "tokens": res.get("tokens") or 0,
                   "calls": res.get("calls"), "candidate_answer": ans[:300], "gold": c["gold_answer"],
                   "correct": v["correct"], "wall_s": round(time.time() - t0), "template": tmpl}
            results.append(rec); save_results(rp, "plain", model, results)
            print(f"   tokens={rec['tokens']} calls={rec['calls']} wall={rec['wall_s']}s "
                  f"correct={rec['correct']} | ans={ans[:60]!r}", flush=True)
        except Exception as ex:
            rec = {"dp": dp, "qid": c["qid"], "family": c["family"], "arm": "plain",
                   "status": "ERROR", "tokens": 0, "error": repr(ex)[:200], "correct": False,
                   "wall_s": round(time.time() - t0), "template": tmpl}
            results.append(rec); save_results(rp, "plain", model, results)
            print(f"   CELL FAILED: {repr(ex)[:160]}", flush=True)
        finally:
            if sbx is not None:
                try: sbx.kill()
                except Exception: pass

    good = [r for r in results if r.get("status") != "ERROR"]
    cor = sum(1 for r in good if r.get("correct"))
    tot = sum(r.get("tokens") or 0 for r in good)
    print("\n=== xAFS PER-DP SEARCH SUMMARY (plain, separate templates) ===", flush=True)
    print(json.dumps({"arm": "plain", "n": len(good), "n_correct": cor,
                      "accuracy": cor / max(len(good), 1), "total_tokens": tot}, indent=2), flush=True)


if __name__ == "__main__":
    main()
