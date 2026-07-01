#!/usr/bin/env python3
"""xAFS PER-DP scoped-search runner — fixes the combined-seed confound.

Template: semfs-perdp-xafs (per-dp seeds baked at /opt/<dp>-gemma-q4.db). Per question it
mounts THAT question's dp seed (cp small seed → ~/.semfs/chanpin.db, remount), so `semfs grep`
searches ONLY that workspace — true per-dp isolation, apples-to-apples vs the per-dp-scoped plain.

Arm = 'search' (pure vector search, NO KG — the per-dp seeds are embed+fs only). Agent =
codex/gpt-5.4-mini (subscription). Judge = run_judge_xafs (Gemini). Per-cell try/except +
incremental checkpoint + resume (reuses run_xafs_slice helpers).

Run:  set -a; . ./.env; set +a; python3 benchmarks/e2b/run_xafs_perdp.py --dps dp_009   # smoke
      python3 benchmarks/e2b/run_xafs_perdp.py                                            # all built
"""
import argparse
import json
import os
import pathlib
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run_judge_xafs as J
from run_xafs_slice import (sh, save_results, load_slice, arm_env as ppr_arm_env,
                            SHIMS, CELL_DRIVER, CODEX_AUTH, SEED_RT, SEMFS_MAP)
from e2b import Sandbox

REPO = pathlib.Path(__file__).resolve().parents[2]

SEARCH_ENV = {
    "SEMFS_EMBED_MODEL": "gemma-q4", "SEMFS_EMBED_ONNX_DIR": "/home/user/gemma_q4",
    "SUPERMEMORY_API_KEY": "dummy-local", "SEMFS_NO_PUSH": "1", "SEMFS_NO_SYNC": "1",
    "SEMFS_SEARCH_ONLY": "off", "SEMFS_RESULT_LIMIT": "5",
    "SEMFS_GREP_RESULT_CAP": "6144", "SEMFS_GREP_TOTAL_CAP": "10240",
    "SEMFS_KG": "off", "SEMFS_COMENTION": "off", "SEMFS_HIDDEN_KG": "off",
    "SEMFS_HIDDEN_KG_RETRIEVAL": "off", "SEMFS_KG_PPR": "off", "SEMFS_GRAPH_FS": "off",
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", default="search", help="search | ppr_on | ppr_map | ppr_off")
    ap.add_argument("--template", default="")
    ap.add_argument("--dps", default="dp_001,dp_002,dp_003,dp_004,dp_005,dp_006,dp_007,dp_008,dp_009")
    ap.add_argument("--agent-timeout", default="600")
    args = ap.parse_args()
    orkey = os.environ["OPENROUTER_API_KEY"]
    model = os.environ.get("WB_CODEX_MODEL", "gpt-5.4-mini")
    arm = args.arm
    template = args.template or ("semfs-ppr-xafs" if arm.startswith("ppr") else "semfs-perdp-xafs")
    base_env = dict(SEARCH_ENV) if arm == "search" else ppr_arm_env(arm)
    dp_set = [d.strip() for d in args.dps.split(",") if d.strip()]
    cells = [c for c in load_slice() if c["dp"] in dp_set]
    print(f"xAFS per-dp: {len(cells)} cells · arm={arm} · template={template} · agent=codex/{model}", flush=True)

    rp = REPO / f"benchmarks/e2b/_xafs_perdp_{arm}.json"
    results, done = [], set()
    if rp.exists():
        for r in json.loads(rp.read_text()).get("results", []):
            if r.get("status") not in (None, "ERROR"):
                results.append(r); done.add((r["dp"], r["qid"]))
        print(f"resume: {len(done)} good cells already saved", flush=True)

    sbx = Sandbox.create(template=template, timeout=3600)
    print("sandbox:", sbx.sandbox_id, flush=True)
    cenv = {**base_env, "OPENROUTER_API_KEY": orkey, "WB_CODEX_MODEL": model,
            "WB_OUTPUT_FILES": "answer.md", "WB_AGENT_TIMEOUT": args.agent_timeout}
    try:
        sh(sbx, "mkdir -p ~/ws/mnt ~/run ~/.semfs ~/.codex ~/cases && ln -sfn /opt/wb ~/wb && "
                "ln -sfn /opt/gemma_q4 ~/gemma_q4 && ln -sfn /opt/semfs-shims ~/semfs-shims", timeout=120)
        sh(sbx, "echo user_allow_other | sudo tee -a /etc/fuse.conf >/dev/null")
        for shim in ("grep", "rg", "_fmt.py"):
            p = SHIMS / shim
            if p.exists():
                sbx.files.write(f"/tmp/shim_{shim}", p.read_text())
                sh(sbx, f"sudo cp /tmp/shim_{shim} /opt/semfs-shims/{shim}")
        sh(sbx, "sudo chmod +x /opt/semfs-shims/grep /opt/semfs-shims/rg")
        sbx.files.write("/home/user/cell_driver.py", CELL_DRIVER.read_text())
        sbx.files.write("/home/user/.codex/auth.json", CODEX_AUTH.read_text())
        if arm == "ppr_map":
            sbx.files.write("/home/user/semfs_map.py", SEMFS_MAP.read_text())
            sh(sbx, "mkdir -p /home/user/maps")

        for i, c in enumerate(cells):
            if (c["dp"], c["qid"]) in done:
                continue
            label = f"{c['dp']}_{c['qid']}"
            t0 = time.time()
            try:
                # mount THIS dp's seed (per-dp isolation): remount + cp small seed → writable runtime
                sh(sbx, f"semfs unmount chanpin --force 2>/dev/null; "
                        f"fusermount -u /home/user/ws/mnt 2>/dev/null; sleep 1; "
                        f"cp /opt/{c['dp']}-gemma-q4.db {SEED_RT}", timeout=120)
                o, e = sh(sbx, "semfs mount chanpin --path /home/user/ws/mnt --backend fuse "
                               "--key dummy-local --no-sync --no-push --startup-timeout 120 2>&1 || true",
                          timeout=180, env=cenv)
                time.sleep(2)
                # ppr_map: per-dp workspace map injected into the prompt (seed is already scoped to this dp)
                cell_env = dict(cenv)
                if arm == "ppr_map":
                    mapf = f"/home/user/maps/{c['dp']}.txt"
                    sh(sbx, f"python3 /home/user/semfs_map.py {SEED_RT} --out {mapf} 2>&1 || true", timeout=120)
                    cell_env["WB_WORKSPACE_MAP"] = mapf
                task = (f"{c['prompt']}\n\nThe workspace is mounted at /home/user/ws/mnt/. Search it "
                        "and write your final answer (the value asked for) to model_output/answer.md.")
                sbx.files.write(f"/home/user/cases/{label}.task", task)
                print(f"\n[{i+1}/{len(cells)}] {label} [{c['family']}] codex…", flush=True)
                o2, e2 = sh(sbx, f"cd /home/user && python3 cell_driver.py --label {label} "
                                 f"--agent codex --case {label} --arm {arm} 2>&1 | tail -3",
                            timeout=int(args.agent_timeout) + 300, env=cell_env)
                res = {}
                for line in (o2 + e2).splitlines():
                    if line.startswith("RESULT="):
                        try: res = json.loads(line[len("RESULT="):])
                        except Exception: pass
                ans, _ = sh(sbx, f"cat /home/user/run/{label}/model_output/answer.md 2>/dev/null || true",
                            timeout=60)
                ans = ans.strip()
                v = J.grade_one(c["prompt"], c["gold_answer"], ans, api_key=orkey) if ans else \
                    {"correct": False, "reason": "no answer.md"}
                rec = {"dp": c["dp"], "qid": c["qid"], "family": c["family"], "arm": arm,
                       "status": res.get("status"), "tokens": res.get("tokens") or 0,
                       "calls": res.get("calls"), "candidate_answer": ans[:300],
                       "gold": c["gold_answer"], "correct": v["correct"], "wall_s": round(time.time()-t0)}
                print(f"   tokens={rec['tokens']} calls={rec['calls']} wall={rec['wall_s']}s "
                      f"correct={rec['correct']} | ans={ans[:70]!r}", flush=True)
            except Exception as ex:
                rec = {"dp": c["dp"], "qid": c["qid"], "family": c["family"], "arm": arm,
                       "status": "ERROR", "tokens": 0, "calls": None, "candidate_answer": "",
                       "gold": c["gold_answer"], "correct": False,
                       "wall_s": round(time.time()-t0), "error": str(ex)[:200]}
                print(f"   CELL FAILED ({str(ex)[:100]}) — continuing", flush=True)
            results.append(rec)
            save_results(rp, arm, model, results)
    finally:
        try: sbx.kill()
        except Exception: pass

    out = save_results(rp, arm, model, results)
    print("\n=== xAFS PER-DP SEARCH SUMMARY ===")
    print(json.dumps({k: out[k] for k in
                      ("arm", "model", "n", "accuracy", "n_correct",
                       "total_tokens_cumulative", "tokens_per_question")}, indent=2))
    print("saved:", rp)


if __name__ == "__main__":
    main()
