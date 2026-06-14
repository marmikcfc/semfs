#!/usr/bin/env python3
"""E2B kaifa (Backend Developer) code-KG smoke — KG-on vs KG-off, ONE backend-dev
WB-Lite case, Claude via OpenRouter, real FUSE mount.

Validates the new AST code lane end-to-end on E2B (the bench-on-E2B HARD RULE):
the kaifa gemma-q4 seed (built on Modal, code KG + Louvain communities) is mounted,
and the agent runs WB-Lite case 3 (project-dependency discovery) twice:
  - arm=kg   : SEMFS_KG default → /kg/ overlay built from the code graph, hint points to it
  - arm=nokg : SEMFS_KG=off     → no /kg/ overlay, hint omits it
Same seed/chunks/vectors/grep both times — the ONLY difference is the code KG overlay.

Creds from .env (source first): OPENROUTER_API_KEY, E2B_API_KEY.
Run:  set -a; . ./.env; set +a; python3 benchmarks/e2b/run_kaifa_smoke.py
"""
import json, os, pathlib, time
from e2b import Sandbox

REPO = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
ORKEY = os.environ["OPENROUTER_API_KEY"]
SEED_GZ = pathlib.Path("/tmp/kaifa_e2b/kaifa-gemma-q4.db.gz")
TASK = pathlib.Path("/tmp/kaifa_e2b/cases/3.task")
CELL = HERE / "cell_driver.py"
CLAUDEJS = REPO / "benchmarks/vendor/Workspace-Bench/evaluation/baselines/ClaudeCode.js"
OUT = REPO / "tickets/ast-kg-code-lane/e2b_smoke"
ARMS = ["kg", "nokg"]
CASE = "3"


def sh(sbx, cmd, timeout=120, env=None):
    r = sbx.commands.run(cmd, timeout=timeout, envs=(env or {}))
    return (r.stdout or ""), (r.stderr or "")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    assert SEED_GZ.exists(), f"missing {SEED_GZ}"
    assert TASK.exists(), f"missing {TASK}"
    print("booting semfs-baked sandbox…", flush=True)
    sbx = Sandbox.create(template="semfs-baked", timeout=3600)
    print("  sandbox:", sbx.sandbox_id, flush=True)
    try:
        # ── boot prep ──
        sh(sbx, "mkdir -p ~/ws ~/run ~/.semfs ~/cases && "
                "ln -sfn /opt/wb ~/wb && ln -sfn /opt/gemma_q4 ~/gemma_q4 && "
                "ln -sfn /opt/semfs-shims ~/semfs-shims", timeout=180)
        sh(sbx, "echo user_allow_other | sudo tee -a /etc/fuse.conf >/dev/null")
        sbx.files.write("/home/user/cell_driver.py", CELL.read_text())
        sbx.files.write("/home/user/ClaudeCode.js", CLAUDEJS.read_text())
        sh(sbx, "sudo cp /home/user/ClaudeCode.js /opt/wb/evaluation/baselines/ClaudeCode.js")
        sbx.files.write(f"/home/user/cases/{CASE}.task", TASK.read_text())
        print("  uploading kaifa seed (35 MB gz)…", flush=True)
        sbx.files.write("/home/user/.semfs/kaifa.db.gz", SEED_GZ.read_bytes())
        o, e = sh(sbx, "gunzip -f /home/user/.semfs/kaifa.db.gz && ls -la /home/user/.semfs/kaifa.db", timeout=180)
        print("  seed:", o.strip()[-120:], flush=True)
        rg_out, _ = sh(sbx, "( find /opt/wb -path '*ripgrep*linux*/rg' 2>/dev/null; command -v rg; echo rg ) | head -5")
        real_rg = next((l.strip() for l in rg_out.splitlines() if l.strip()), "rg")
        print("  real rg:", real_rg, flush=True)

        results = {}
        for arm in ARMS:
            print(f"\n=== arm={arm} ===", flush=True)
            # unmount any prior mount, then (re)mount the SAME seed toggling SEMFS_KG
            sh(sbx, "semfs unmount kaifa --force 2>/dev/null; "
                    "fusermount -u /home/user/ws/mnt 2>/dev/null; sleep 2", timeout=60)
            menv = {"SEMFS_EMBED_MODEL": "gemma-q4", "SEMFS_EMBED_ONNX_DIR": "/home/user/gemma_q4",
                    "SUPERMEMORY_API_KEY": "dummy-local", "SEMFS_NO_PUSH": "1",
                    "SEMFS_NO_SYNC": "1", "SEMFS_SEARCH_ONLY": "on"}
            if arm == "nokg":
                menv["SEMFS_KG"] = "off"
            o, e = sh(sbx, "semfs mount kaifa --path /home/user/ws/mnt --backend fuse "
                           "--key dummy-local --no-sync --no-push 2>&1 || true", timeout=300, env=menv)
            print(f"  mount: {(o + e).strip()[-180:]}", flush=True)
            time.sleep(3)
            kgls, _ = sh(sbx, "echo '[root]'; ls /home/user/ws/mnt 2>&1 | head; "
                              "echo '[kg]'; ls /home/user/ws/mnt/kg 2>&1 | head")
            print(f"  mount tree:\n{kgls}", flush=True)

            label = f"kaifa_claude_{CASE}_{arm}"
            env = {"OPENROUTER_API_KEY": ORKEY, "WB_REAL_RG": real_rg, "HOME": "/home/user",
                   "SUPERMEMORY_API_KEY": "dummy-local"}
            print(f"  ▶ running {label} …", flush=True)
            o, e = sh(sbx, f"cd /home/user && python3 cell_driver.py --label {label} --agent claude "
                           f"--case {CASE} --arm {arm} 2>>/tmp/{label}.err", timeout=1750, env=env)
            res = None
            for line in o.splitlines():
                if line.startswith("RESULT="):
                    res = json.loads(line[len("RESULT="):])
            if res is None:
                err, _ = sh(sbx, f"tail -25 /tmp/{label}.err 2>/dev/null")
                res = {"status": "NO_RESULT", "stderr_tail": err.strip()[-1500:]}
            res["mount_kg_listing"] = kgls
            results[arm] = res
            (OUT / f"{label}.json").write_text(json.dumps(res, indent=2, ensure_ascii=False))
            # pull deliverable
            try:
                o2, _ = sh(sbx, f"cd /home/user/run/{label} 2>/dev/null && "
                                f"tar czf /tmp/{label}_out.tgz model_output 2>/dev/null && echo OK || echo NOFILES")
                if "OK" in o2:
                    (OUT / f"{label}_out.tgz").write_bytes(sbx.files.read(f"/tmp/{label}_out.tgz", format="bytes"))
            except Exception as ex:
                print(f"    deliverable pull failed: {repr(ex)[:120]}", flush=True)
            print(f"  ✓ {arm}: status={res.get('status')} tokens={res.get('tokens')} "
                  f"calls={res.get('calls')} semfs_grep={res.get('used_semfs_grep')} "
                  f"deliverables={res.get('deliverables')}", flush=True)

        (OUT / "summary.json").write_text(json.dumps(
            {a: {k: results[a].get(k) for k in
                 ("status", "tokens", "calls", "used_semfs_grep", "deliverables", "auth_used", "wall_s")}
             for a in results}, indent=2, ensure_ascii=False))
        print("\n=== SUMMARY ===")
        print(json.dumps(results.get("kg", {}).get("deliverables"), ensure_ascii=False))
        for a in ARMS:
            r = results.get(a, {})
            print(f"  {a:5s}  status={r.get('status')} tokens={r.get('tokens')} "
                  f"calls={r.get('calls')} semfs_grep={r.get('used_semfs_grep')} deliv={r.get('deliverables')}")
    finally:
        sbx.kill()
        print("sandbox killed.", flush=True)


if __name__ == "__main__":
    main()
