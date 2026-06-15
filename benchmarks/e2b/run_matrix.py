#!/usr/bin/env python3
"""E2B WB-PM matrix orchestrator (re-authored 2026-06-14; the /tmp original was lost).

Reuses the LIVE `semfs-baked` template (built 2026-06-13, already current: adaptive-K
binary + chanpin-gemma-q4 seed + gemma embedder + WB harness + cases all baked — verified,
no rebuild needed). Per cell it runs the PATCHED ~/cell_driver.py (Claude→OpenRouter,
codex→ChatGPT-subscription).

Boot-prep mirrors E2B_RUNBOOK.md §3/§5/§7/§10:
  symlinks → copy seed → fuse.conf → semfs mount → override ClaudeCode.js with the
  parity-fixed repo version → upload cell_driver.py + codex auth → (plain) upload corpus.

Run:   python3 run_matrix.py --smoke         # 2 cells (claude+codex nokg, case 15)
       python3 run_matrix.py --full          # plain/nokg/nokgAK × {claude,codex} × 10 cases
Creds come from .env (sourced by the caller): OPENROUTER_API_KEY, E2B_API_KEY.
Codex ChatGPT auth from ./codex_auth.json.
"""
import argparse, json, os, sys, time, pathlib, threading, queue, subprocess
from e2b import Sandbox

_JSONL_LOCK = threading.Lock()   # serialize results.jsonl appends across parallel workers

REPO = pathlib.Path(__file__).resolve().parents[2]
CLAUDECODE_JS = REPO / "benchmarks/vendor/Workspace-Bench/evaluation/baselines/ClaudeCode.js"
CELL_DRIVER = REPO / "benchmarks/e2b/cell_driver.py"
CODEX_AUTH = REPO / "codex_auth.json"
FIXED_BIN = REPO / "benchmarks/e2b/assets/semfs-fixed"   # Modal-built x86_64 binary w/ timeout fix (pushed at boot)
WB_LITE = REPO / "benchmarks/e2b/assets/wb_lite/task_lite_clean_en"  # judge metadata (output_files etc.)


def expected_output_files(case):
    """The exact filename(s) the judge grades against (WB metadata `output_files`), which
    upstream never tells the agent. We inject them as a prompt hint (cell_driver) so a correct
    deliverable isn't zeroed by the filename lottery — and to test instruction-following."""
    mp = WB_LITE / str(case) / "metadata.json"
    if not mp.exists():
        return ""
    try:
        d = json.loads(mp.read_text())
        of = d.get("output_files") or ([d["output_file"]] if d.get("output_file") else [])
        return ",".join(os.path.basename(str(x)).strip() for x in of if str(x).strip())
    except Exception:
        return ""
CORPUS = str(REPO / "benchmarks/e2b/assets/chanpin_standard")   # plain-arm tree (persistent; pulled from Modal 2026-06-15)
OUT = REPO / "tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs"
CASES_FULL = ["15", "44", "45", "53", "55", "95", "171", "175", "386", "388"]   # 289 excluded (seed leak)
ARMS = ["plain", "nokg", "nokgAK"]
# codex (OpenAI, on the ChatGPT subscription = no per-token $) runs FIRST; Claude
# (OpenRouter = real $) only after all codex cells complete. Override with --agents.
AGENTS = ["codex", "claude"]
ORKEY = os.environ.get("OPENROUTER_API_KEY", "")
# Claude native OAuth token (subscription) — injected per cell so claude bills the Claude
# subscription, not OpenRouter. From claude_auth_config.json {"token": ...}.
try:
    CLAUDE_OAUTH = json.loads((REPO / "claude_auth_config.json").read_text()).get("token", "")
except Exception:
    CLAUDE_OAUTH = ""


def sh(sbx, cmd, timeout=120, env=None):
    r = sbx.commands.run(cmd, timeout=timeout, envs=(env or {}))
    return (r.stdout or ""), (r.stderr or "")

def is_sandbox_dead(exc):
    s = repr(exc) + str(exc)
    return any(x in s for x in ("SandboxNotFoundException","404","not found","CLOSED","LocalProtocolError","ConnectionInputs"))


def boot_prep(sbx, need_plain, need_mount=True):
    print(f"  boot-prep… (mount={need_mount}, plain={need_plain})", flush=True)
    sh(sbx, "mkdir -p ~/ws ~/run ~/.semfs ~/.codex && "
            "ln -sfn /opt/wb ~/wb && ln -sfn /opt/cases ~/cases && "
            "ln -sfn /opt/gemma_q4 ~/gemma_q4 && ln -sfn /opt/semfs-shims ~/semfs-shims"
            + (" && cp /opt/chanpin-gemma-q4.db ~/.semfs/chanpin.db" if need_mount else ""),
       timeout=180)
    sh(sbx, "echo user_allow_other | sudo tee -a /etc/fuse.conf >/dev/null")
    # Push the FIXED semfs binary (built on Modal, x86_64-linux) over the baked one,
    # BEFORE any mount so the daemon spawns from it. Carries the timeout fix
    # (rcas/2026-06-15-grep-timeout-cloud-fallback-panic.md): no cloud-fallback panic +
    # 120s search bound. Skipped if the local artifact is absent (falls back to baked).
    if FIXED_BIN.exists():
        sbx.files.write("/tmp/semfs-fixed", FIXED_BIN.read_bytes())
        o, _ = sh(sbx, "sudo cp /tmp/semfs-fixed /usr/local/bin/semfs && sudo chmod +x /usr/local/bin/semfs && /usr/local/bin/semfs --version")
        print("  semfs binary:", o.strip()[:60], "(FIXED)", flush=True)
    # upload patched files
    sbx.files.write("/home/user/cell_driver.py", CELL_DRIVER.read_text())
    sbx.files.write("/home/user/ClaudeCode.js", CLAUDECODE_JS.read_text())
    sh(sbx, "sudo cp /home/user/ClaudeCode.js /opt/wb/evaluation/baselines/ClaudeCode.js")
    if CODEX_AUTH.exists():
        sbx.files.write("/home/user/.codex/auth.json", CODEX_AUTH.read_text())
    # real ripgrep for the claude shim to delegate to — MUST be the linux variant
    # (the SDK also ships an arm64-darwin rg that won't exec in the linux sandbox).
    rg_out, _ = sh(sbx, "( find /opt/wb -path '*ripgrep*linux*/rg' 2>/dev/null; "
                        "command -v rg 2>/dev/null; echo rg ) | head -5")
    real_rg = next((l.strip() for l in rg_out.splitlines() if l.strip()), "rg")
    print("  real rg:", real_rg, flush=True)
    # mount semfs (only needed for nokg/nokgAK; cloud + plain don't use the local mount)
    if need_mount:
        mount_env = {"SEMFS_EMBED_MODEL": "gemma-q4", "SEMFS_EMBED_ONNX_DIR": "/home/user/gemma_q4",
                     "SUPERMEMORY_API_KEY": "dummy-local", "SEMFS_NO_PUSH": "1", "SEMFS_NO_SYNC": "1",
                     "SEMFS_SEARCH_ONLY": "on",
                     # dump RRF→RERANK→FINAL ranked order (rank/score/filepath) per query into
                     # the daemon log. Search runs in the DAEMON, so this must be set HERE at
                     # mount (not in cell_driver). Captured via semfs_logs/chanpin.log on pull.
                     "SEMFS_DEBUG_RANKING": "1",
                     # timeout fix (rcas/2026-06-15-grep-timeout-cloud-fallback-panic.md):
                     # raise the search bound to ~2min so a lock-contended search completes
                     # instead of timing out → cloud fallback → panic → retry-storm.
                     "SEMFS_SEARCH_TIMEOUT_SECS": "120", "SEMFS_GREP_CLIENT_WAIT_SECS": "140",
                     "SEMFS_SEARCH_DEADLINE_SECS": "90"}
        o, e = sh(sbx, "semfs mount chanpin --path /home/user/ws/mnt --backend fuse "
                       "--key dummy-local --no-sync --no-push 2>&1 || true", timeout=240, env=mount_env)
        print("  mount:", (o + e).strip()[-200:], flush=True)
        ls, _ = sh(sbx, "ls /home/user/ws/mnt 2>&1 | head -5")
        print("  mount ls:", ls.strip()[:200], flush=True)
    if need_plain:
        import subprocess, tempfile
        tf = tempfile.mktemp(suffix=".tgz")
        subprocess.run(["tar", "czf", tf, "-C", os.path.dirname(CORPUS), os.path.basename(CORPUS)], check=True)
        data = pathlib.Path(tf).read_bytes()
        # CHUNKED upload: a single 442MB files.write hangs (E2B single-call ceiling).
        # Split into ~32MB parts, write each, reassemble in the sandbox. RCA 2026-06-15.
        CHUNK = 32 * 1024 * 1024
        nparts = (len(data) + CHUNK - 1) // CHUNK
        print(f"  uploading plain corpus ({len(data)//(1024*1024)}MB in {nparts} chunks)…", flush=True)
        sh(sbx, "rm -f /tmp/corpus.tgz /tmp/corpus.part_*", timeout=30)
        for i in range(nparts):
            sbx.files.write(f"/tmp/corpus.part_{i:03d}", data[i * CHUNK:(i + 1) * CHUNK])
            print(f"    part {i + 1}/{nparts}", flush=True)
        sh(sbx, "cat /tmp/corpus.part_* > /tmp/corpus.tgz && rm -f /tmp/corpus.part_*", timeout=120)
        sh(sbx, "mkdir -p ~/ws/plain && tar xzf /tmp/corpus.tgz -C ~/ws/plain --strip-components=1", timeout=300)
        n, _ = sh(sbx, "find ~/ws/plain -type f | wc -l")
        print("  plain files:", n.strip(), flush=True)
    return real_rg


def run_cell(sbx, agent, case, arm, rep, real_rg):
    label = f"pm_{agent}_{case}_{arm}_r{rep}"
    sbx.set_timeout(3600)
    print(f"  ▶ {label} …", flush=True)
    # One daemon log accumulates ALL cells on a sandbox; record the line offset NOW so the
    # pull can slice out just THIS cell's RANKDUMP/pipeline-counts lines (per-query rank order).
    _ls, _ = sh(sbx, "wc -l < /home/user/.cache/semfs/logs/chanpin.log 2>/dev/null || echo 0")
    try:
        log_start = int((_ls or "0").split()[0]) + 1
    except Exception:
        log_start = 1
    env = {"OPENROUTER_API_KEY": ORKEY, "WB_REAL_RG": real_rg, "HOME": "/home/user",
           "CLAUDE_CODE_OAUTH_TOKEN": CLAUDE_OAUTH,
           "SUPERMEMORY_API_KEY": os.environ.get("SUPERMEMORY_API_KEY", ""),
           "SUPERMEMORY_API_URL": os.environ.get("SUPERMEMORY_API_URL", ""),
           "WB_OUTPUT_FILES": expected_output_files(case)}  # filename hint → cell_driver prompt
    o, e = sh(sbx, f"cd /home/user && python3 cell_driver.py --label {label} --agent {agent} "
                   f"--case {case} --arm {arm} 2>>/tmp/{label}.err", timeout=1750, env=env)
    res = None
    for line in o.splitlines():
        if line.startswith("RESULT="):
            res = json.loads(line[len("RESULT="):])
    if res is None:
        err, _ = sh(sbx, f"tail -8 /tmp/{label}.err 2>/dev/null")
        res = {"label": label, "agent": agent, "case": case, "arm": arm, "status": "DRIVER_NO_RESULT",
               "stderr_tail": err.strip()[-500:]}
    # persist + pull deliverables immediately (ephemeral sandbox — hard rule #2)
    d = OUT / label; d.mkdir(parents=True, exist_ok=True)
    (d / "result.json").write_text(json.dumps(res, ensure_ascii=False, indent=2))
    # Pull EVERYTHING per cell, immediately (EC2 parity+): deliverables (model_output) +
    # agent.json (trace.executionTrace WITH tool outputs) + harness raw (codex_stdout.jsonl,
    # stdout/stderr) + semfs daemon log + driver stderr. Ephemeral sandbox → hard rule #2.
    try:
        sh(sbx, f"rm -rf /tmp/pull/{label} && mkdir -p /tmp/pull/{label}/sandbox_raw && "
                f"cp -a /home/user/run/{label}/. /tmp/pull/{label}/ 2>/dev/null; "
                f"cp -a /tmp/sbx_{label}_* /tmp/pull/{label}/sandbox_raw/ 2>/dev/null; "
                f"cp -a /home/user/.cache/semfs/logs /tmp/pull/{label}/semfs_logs 2>/dev/null; "
                f"tail -n +{log_start} /home/user/.cache/semfs/logs/chanpin.log 2>/dev/null "
                f"  > /tmp/pull/{label}/ranking_this_cell.log; "
                f"cp /tmp/{label}.err /tmp/pull/{label}/driver_stderr.txt 2>/dev/null; "
                f"tar czf /tmp/{label}_full.tgz -C /tmp/pull {label} 2>/dev/null", timeout=200)
        (d / "full.tgz").write_bytes(sbx.files.read(f"/tmp/{label}_full.tgz", format="bytes"))
        # Clear accumulating dirs first — `tar xzf` overwrites same-named files but never
        # DELETES stale ones, so a prior run's deliverables linger in model_output and the
        # judge grades a MIX (confound that muddied case 55). Wipe before extracting fresh.
        import shutil
        for sub in ("model_output", "sandbox_raw", "semfs_logs"):
            shutil.rmtree(d / sub, ignore_errors=True)
        subprocess.run(["tar", "xzf", str(d / "full.tgz"), "-C", str(d), "--strip-components=1"], check=False)
    except Exception as ex:
        print(f"    artifact pull failed: {repr(ex)[:140]}", flush=True)
    with _JSONL_LOCK:
        with open(OUT / "results.jsonl", "a") as f:
            f.write(json.dumps({k: res.get(k) for k in
                    ("label", "agent", "case", "arm", "auth_used", "status", "tokens", "calls",
                     "used_semfs_grep", "deliverables")}, ensure_ascii=False) + "\n")
    print(f"    {res.get('status')}  tokens={res.get('tokens')} calls={res.get('calls')} "
          f"semfs_grep={res.get('used_semfs_grep')} auth={res.get('auth_used')}", flush=True)
    return res


def _should_skip(label, force):
    if force:
        return False
    done = OUT / label / "result.json"
    if not done.exists():
        return False
    try:
        return json.loads(done.read_text()).get("status") == "ok"
    except Exception:
        return False


def worker(wid, cells_q, need_plain, need_mount, rep):
    """One sandbox; pull cells from the shared queue until empty; reboot on death."""
    sbx = Sandbox.create(template="semfs-baked", timeout=3600)
    print(f"[w{wid}] sandbox {sbx.sandbox_id}", flush=True)
    try:
        real_rg = boot_prep(sbx, need_plain, need_mount)
    except Exception as ex:
        print(f"[w{wid}] boot-prep failed: {repr(ex)[:150]} — retrying once", flush=True)
        try: sbx.kill()
        except Exception: pass
        sbx = Sandbox.create(template="semfs-baked", timeout=3600)
        real_rg = boot_prep(sbx, need_plain, need_mount)
    try:
        while True:
            try:
                ag, c, arm = cells_q.get_nowait()
            except queue.Empty:
                break
            label = f"pm_{ag}_{c}_{arm}_r{rep}"
            try:
                for attempt in range(3):
                    try:
                        run_cell(sbx, ag, c, arm, rep, real_rg)
                        break
                    except Exception as ex:
                        if not is_sandbox_dead(ex) or attempt == 2:
                            print(f"[w{wid}] CELL ERROR {label} (try {attempt+1}): {repr(ex)[:140]}", flush=True)
                            break
                        print(f"[w{wid}] sandbox dead — rebooting (try {attempt+1}) for {label}", flush=True)
                        try: sbx.kill()
                        except Exception: pass
                        try:
                            sbx = Sandbox.create(template="semfs-baked", timeout=3600)
                            real_rg = boot_prep(sbx, need_plain, need_mount)
                        except Exception as ex2:
                            print(f"[w{wid}] reboot failed: {repr(ex2)[:120]}", flush=True)
            finally:
                cells_q.task_done()
    finally:
        try: sbx.kill()
        except Exception: pass
        print(f"[w{wid}] done, sandbox killed.", flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--smoke", action="store_true")
    ap.add_argument("--full", action="store_true")
    ap.add_argument("--cases", default=None,
                    help="comma list of case ids to run (e.g. '45,53,55,171,175'). Overrides --smoke/--full.")
    ap.add_argument("--rep", default="1")
    ap.add_argument("--agents", default=None,
                    help="comma list to restrict/order agents, e.g. 'codex' or 'claude'.")
    ap.add_argument("--arms", default=None,
                    help="comma list to restrict arms, e.g. 'cloud' or 'plain,nokg'. Default: plain,nokg,nokgAK.")
    ap.add_argument("--parallel", type=int, default=1, help="number of concurrent sandboxes (pool size).")
    ap.add_argument("--force", action="store_true", help="re-run cells even if a prior ok result.json exists.")
    args = ap.parse_args()
    if not ORKEY:
        sys.exit("OPENROUTER_API_KEY not in env — `set -a; . ./.env; set +a` first")

    agents = [a.strip() for a in args.agents.split(",")] if args.agents else AGENTS
    arms = [a.strip() for a in args.arms.split(",")] if args.arms else ARMS
    if args.cases:
        sel = [c.strip() for c in args.cases.split(",") if c.strip()]
        cells = [(ag, c, arm) for ag in agents for c in sel for arm in arms]
    elif args.smoke:
        cells = [(ag, "15", arms[0]) for ag in agents]
    elif args.full:
        cells = [(ag, c, arm) for ag in agents for c in CASES_FULL for arm in arms]
    else:
        sys.exit("pass --cases, --smoke, or --full")
    cells = [c for c in cells if not _should_skip(f"pm_{c[0]}_{c[1]}_{c[2]}_r{args.rep}", args.force)]
    need_plain = any(arm == "plain" for _, _, arm in cells)
    need_mount = any(arm in ("nokg", "nokgAK") for _, _, arm in cells)

    OUT.mkdir(parents=True, exist_ok=True)
    if not cells:
        print("nothing to run (all skipped).", flush=True); return
    n = max(1, min(args.parallel, len(cells)))
    print(f"running {len(cells)} cells across {n} parallel sandbox(es) (force={args.force}, plain={need_plain})", flush=True)
    cells_q = queue.Queue()
    for c in cells:
        cells_q.put(c)
    threads = [threading.Thread(target=worker, args=(i, cells_q, need_plain, need_mount, args.rep), daemon=True)
               for i in range(n)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    print("ALL WORKERS DONE.", flush=True)


if __name__ == "__main__":
    main()
