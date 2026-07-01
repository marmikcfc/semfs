#!/usr/bin/env python3
"""xAFS slice runner — Codex (gpt-5.4-mini, ChatGPT subscription) agent on the baked
semfs-mount-xafs template, ONE arm per sandbox (the 5.9 GB seed is too big to keep two
copies on the 11 GB disk, so we `mv` it to the writable runtime path → single arm).

Reuses the UNMODIFIED cell_driver.py (it's prompt-file driven: reads ~/cases/<case>.task)
+ the fixed grep/rg shims (so Codex's bare `grep` routes to semantic search) + the baked
WB Codex harness. Per cell: write the xAFS question as the task, run cell_driver, pull
model_output/answer.md, judge with run_judge_xafs (Gemini, Supermemory-faithful).

Emits per-cell tokens (from cell_driver RESULT) + a slice summary (accuracy + total tokens).

Run:  set -a; . ./.env; set +a
      python3 benchmarks/e2b/run_xafs_slice.py --arm ppr_on --limit 1     # smoke 1 Q
      python3 benchmarks/e2b/run_xafs_slice.py --arm ppr_on               # all 13
"""
import argparse
import glob
import json
import os
import pathlib
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run_judge_xafs as J

from e2b import Sandbox

REPO = pathlib.Path(__file__).resolve().parents[2]
CASES_DIR = "/private/tmp/claude-501/-Users-marmikpandya-semantic-filesystem/4fa8bea0-f33f-4007-8c23-60009ba683a0/scratchpad/xafs_cases"
SHIMS = REPO / "benchmarks/workspace_bench/semfs-shims"
CELL_DRIVER = REPO / "benchmarks/e2b/cell_driver.py"
SEMFS_MAP = REPO / "benchmarks/e2b/semfs_map.py"
CODEX_AUTH = REPO / "codex_auth.json"
SEED_OPT = "/opt/xafs-gemma-q4.db"
SEED_RT = "/home/user/.semfs/chanpin.db"   # cell_driver/do_mount expect tag 'chanpin'

# ppr_on daemon+agent env (== hiddenkg_l7 + PPR diffusion). Passed to BOTH the mount and
# the cell_driver process so the agent's `semfs grep` calls get the PPR-ranked daemon.
def arm_env(arm):
    e = {"SEMFS_EMBED_MODEL": "gemma-q4", "SEMFS_EMBED_ONNX_DIR": "/home/user/gemma_q4",
         "SUPERMEMORY_API_KEY": "dummy-local", "SEMFS_NO_PUSH": "1", "SEMFS_NO_SYNC": "1",
         "SEMFS_SEARCH_ONLY": "off", "SEMFS_RESULT_LIMIT": "5",
         "SEMFS_GREP_RESULT_CAP": "6144", "SEMFS_GREP_TOTAL_CAP": "10240",
         "SEMFS_KG": "off", "SEMFS_COMENTION": "on", "SEMFS_HIDDEN_KG": "on",
         "SEMFS_HIDDEN_KG_RETRIEVAL": "off", "SEMFS_GRAPH_FS": "off",
         "SEMFS_KG_PPR": "on" if arm in ("ppr_on", "ppr_map") else "off",
         "SEMFS_PPR_RESTART": "0.5", "SEMFS_PPR_ITERS": "30"}
    return e


def load_slice():
    out = []
    for qf in sorted(glob.glob(f"{CASES_DIR}/*/question.json")):
        dp = os.path.basename(os.path.dirname(qf))
        q = json.load(open(qf))[0]   # q01 — one per workspace
        out.append({"dp": dp, "qid": q["id"], "prompt": q["prompt"],
                    "gold_answer": q["gold_answer"], "family": q["family"]})
    return out


def sh(sbx, cmd, timeout=300, env=None):
    r = sbx.commands.run(cmd, timeout=timeout, envs=(env or {}))
    return (r.stdout or ""), (r.stderr or "")


def save_results(rp, arm, model, results):
    agg = J.aggregate(results)
    tot = sum(r.get("tokens", 0) for r in results)
    out = {"arm": arm, "model": model, "n": len(results),
           "accuracy": agg["accuracy"], "n_correct": agg["n_correct"],
           "total_tokens_cumulative": tot,
           "tokens_per_question": round(tot / len(results)) if results else 0,
           "results": results}
    rp.write_text(json.dumps(out, indent=2))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", default="ppr_on")
    ap.add_argument("--limit", type=int, default=0, help="0 = all 13")
    ap.add_argument("--agent-timeout", default="600")
    args = ap.parse_args()
    orkey = os.environ["OPENROUTER_API_KEY"]
    model = os.environ.get("WB_CODEX_MODEL", "gpt-5.4-mini")
    cells = load_slice()
    if args.limit:
        cells = cells[:args.limit]
    print(f"xAFS slice: {len(cells)} cells · arm={args.arm} · agent=codex/{model}", flush=True)

    rp = REPO / f"benchmarks/e2b/_xafs_slice_{args.arm}.json"
    results, done = [], set()
    if rp.exists():
        for r in json.loads(rp.read_text()).get("results", []):
            if r.get("status") not in (None, "ERROR"):   # resume good cells; re-run failures
                results.append(r); done.add((r["dp"], r["qid"]))
        print(f"resume: {len(done)} good cells already saved → skipping", flush=True)
    sbx = Sandbox.create(template="semfs-mount-xafs", timeout=3600)
    print("sandbox:", sbx.sandbox_id, flush=True)
    try:
        # ---- boot prep (cell_driver deps) ----
        sh(sbx, "mkdir -p ~/ws/mnt ~/run ~/.semfs ~/.codex ~/cases ~/maps && "
                "ln -sfn /opt/wb ~/wb && ln -sfn /opt/gemma_q4 ~/gemma_q4 && "
                "ln -sfn /opt/semfs-shims ~/semfs-shims", timeout=120)
        sh(sbx, "echo user_allow_other | sudo tee -a /etc/fuse.conf >/dev/null")
        for shim in ("grep", "rg", "_fmt.py"):
            p = SHIMS / shim
            if p.exists():
                sbx.files.write(f"/tmp/shim_{shim}", p.read_text())
                sh(sbx, f"sudo cp /tmp/shim_{shim} /opt/semfs-shims/{shim}")
        sh(sbx, "sudo chmod +x /opt/semfs-shims/grep /opt/semfs-shims/rg")
        sbx.files.write("/home/user/cell_driver.py", CELL_DRIVER.read_text())
        sbx.files.write("/home/user/semfs_map.py", SEMFS_MAP.read_text())
        sbx.files.write("/home/user/.codex/auth.json", CODEX_AUTH.read_text())
        if args.arm == "plain":
            # FS BASELINE: extract the raw corpus, NO semfs seed/mount. The agent navigates the
            # real file tree with find/grep/cat (cell_driver's plain arm, no semantic grep shim).
            sh(sbx, "mkdir -p ~/ws/plain_all && tar xzf /opt/corpus.tgz -C ~/ws/plain_all", timeout=600)
            print("  plain: corpus extracted → ~/ws/plain_all/xafs/", flush=True)
            cenv = {"OPENROUTER_API_KEY": orkey, "WB_CODEX_MODEL": model,
                    "WB_OUTPUT_FILES": "answer.md", "WB_AGENT_TIMEOUT": args.agent_timeout,
                    "WB_READ_PATHS": "/home/user/ws/plain"}
        else:
            # MOVE the baked seed to the writable runtime path (rename, no 5.9 GB copy)
            o, _ = sh(sbx, f"sudo mv {SEED_OPT} {SEED_RT} && sudo chown user:user {SEED_RT} && "
                           f"ls -la {SEED_RT}; df -h / | tail -1", timeout=120)
            print("  seed moved:", o.strip().splitlines()[-2:], flush=True)
            # ---- mount (ppr daemon) ----
            o, e = sh(sbx, "semfs mount chanpin --path /home/user/ws/mnt --backend fuse "
                           "--key dummy-local --no-sync --no-push --startup-timeout 240 2>&1 || true",
                      timeout=320, env=arm_env(args.arm))
            time.sleep(3)
            root, _ = sh(sbx, "ls /home/user/ws/mnt 2>&1 | head")
            print("  mount root:", root.strip()[:120], flush=True)
            cenv = {**arm_env(args.arm), "OPENROUTER_API_KEY": orkey,
                    "WB_CODEX_MODEL": model, "WB_OUTPUT_FILES": "answer.md",
                    "WB_AGENT_TIMEOUT": args.agent_timeout}
        for i, c in enumerate(cells):
            if (c["dp"], c["qid"]) in done:
                print(f"[{i+1}/{len(cells)}] {c['dp']}_{c['qid']} — already done, skip", flush=True)
                continue
            label = f"{c['dp']}_{c['qid']}"
            t0 = time.time()
            try:
                cell_env = dict(cenv)
                if args.arm == "plain":
                    # point PLAIN at THIS dp's raw tree (cell_driver plain arm reads /home/user/ws/plain)
                    sh(sbx, f"ln -sfn /home/user/ws/plain_all/xafs/{c['dp']} /home/user/ws/plain", timeout=30)
                    scope = "/home/user/ws/plain/"
                else:
                    scope = f"/home/user/ws/mnt/{c['dp']}/"
                    # ppr_map = ppr_on + a cached PER-DP workspace map (cell_driver reads WB_WORKSPACE_MAP)
                    if args.arm == "ppr_map":
                        mapf = f"/home/user/maps/{c['dp']}.txt"
                        mo, _ = sh(sbx, f"python3 /home/user/semfs_map.py {SEED_RT} --prefix /{c['dp']} "
                                        f"--out {mapf} 2>&1 || true", timeout=120)
                        cell_env["WB_WORKSPACE_MAP"] = mapf
                        print(f"   map[{c['dp']}]: {(mo or '').strip().splitlines()[-1:]}", flush=True)
                task = (f"{c['prompt']}\n\n[Scope] This question is about the workspace under "
                        f"{scope} — focus your search there. Write your final "
                        f"answer (the value asked for) to model_output/answer.md.")
                sbx.files.write(f"/home/user/cases/{label}.task", task)
                print(f"\n[{i+1}/{len(cells)}] {label} [{c['family']}] running codex…", flush=True)
                o, e = sh(sbx, f"cd /home/user && python3 cell_driver.py --label {label} "
                               f"--agent codex --case {label} --arm {args.arm} 2>&1 | tail -3",
                          timeout=int(args.agent_timeout) + 300, env=cell_env)
                res = {}
                for line in (o + e).splitlines():
                    if line.startswith("RESULT="):
                        try: res = json.loads(line[len("RESULT="):])
                        except Exception: pass
                ans, _ = sh(sbx, f"cat /home/user/run/{label}/model_output/answer.md 2>/dev/null || true",
                            timeout=60)
                ans = ans.strip()
                v = J.grade_one(c["prompt"], c["gold_answer"], ans, api_key=orkey) if ans else \
                    {"correct": False, "reason": "no answer.md written"}
                rec = {"dp": c["dp"], "qid": c["qid"], "family": c["family"], "arm": args.arm,
                       "status": res.get("status"), "tokens": res.get("tokens") or 0,
                       "calls": res.get("calls"), "candidate_answer": ans[:300],
                       "gold": c["gold_answer"], "correct": v["correct"], "wall_s": round(time.time()-t0)}
                print(f"   status={rec['status']} tokens={rec['tokens']} calls={rec['calls']} "
                      f"wall={rec['wall_s']}s correct={rec['correct']} | ans={ans[:80]!r}", flush=True)
            except Exception as ex:
                rec = {"dp": c["dp"], "qid": c["qid"], "family": c["family"], "arm": args.arm,
                       "status": "ERROR", "tokens": 0, "calls": None, "candidate_answer": "",
                       "gold": c["gold_answer"], "correct": False,
                       "wall_s": round(time.time()-t0), "error": str(ex)[:200]}
                print(f"   CELL FAILED ({str(ex)[:100]}) — continuing", flush=True)
            results.append(rec)
            save_results(rp, args.arm, model, results)   # checkpoint after EVERY cell
    finally:
        try: sbx.kill()
        except Exception: pass

    out = save_results(rp, args.arm, model, results)
    print("\n=== xAFS SLICE SUMMARY ===")
    print(json.dumps({k: out[k] for k in
                      ("arm", "model", "n", "accuracy", "n_correct",
                       "total_tokens_cumulative", "tokens_per_question")}, indent=2))
    print("saved:", rp)


if __name__ == "__main__":
    main()
