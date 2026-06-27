#!/usr/bin/env python3
"""E2B WB-PM matrix orchestrator (re-authored 2026-06-14; the /tmp original was lost).

Reuses the LIVE `semfs-baked` template (built 2026-06-13, already current: adaptive-K
binary + chanpin-gemma-q4 seed + gemma embedder + WB harness + cases all baked — verified,
no rebuild needed). Per cell it runs the PATCHED ~/cell_driver.py (Claude→OpenRouter,
codex→ChatGPT-subscription).

Boot-prep mirrors E2B_RUNBOOK.md §3/§5/§7/§10:
  symlinks → copy seed → fuse.conf → semfs mount → override ClaudeCode.js with the
  parity-fixed repo version → upload cell_driver.py + codex auth → (plain) upload corpus.

Run:   python3 run_matrix.py --smoke         # 2 cells (claude+codex plain, case 15)
       python3 run_matrix.py --full          # plain/nokg/nokgAK × {claude,codex} × 10 cases
Creds come from .env (sourced by the caller): OPENROUTER_API_KEY, E2B_API_KEY.
Codex ChatGPT auth from ./codex_auth.json.
"""
import argparse, json, os, sys, time, pathlib, threading, queue, subprocess, concurrent.futures
from e2b import Sandbox

_JSONL_LOCK = threading.Lock()   # serialize results.jsonl appends across parallel workers

REPO = pathlib.Path(__file__).resolve().parents[2]
CLAUDECODE_JS = REPO / "benchmarks/vendor/Workspace-Bench/evaluation/baselines/ClaudeCode.js"
CELL_DRIVER = REPO / "benchmarks/e2b/cell_driver.py"
SEMFS_MAP = REPO / "benchmarks/e2b/semfs_map.py"   # workspace-map generator (ppr_map arm)
CODEX_AUTH = REPO / "codex_auth.json"
# Optional semfs binary override. Use ONLY when explicitly requested via WB_FIXED_BIN;
# the baked template binary is the default/stable path for routine E2B runs.
FIXED_BIN = pathlib.Path(os.environ["WB_FIXED_BIN"]) if os.environ.get("WB_FIXED_BIN") else None
KNOBS = {}   # SEMFS_* knob overrides for the optimization sweep (loaded from --knobs JSON)
WB_LITE = pathlib.Path(os.environ.get("WB_LITE_DIR") or (REPO / "benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en"))  # judge metadata (output_files etc.) — the COMPLETE all-persona set (the chanpin-only assets/wb_lite/ copy lacked houqin/yunying output_files → filename-hint missed → ~15% nested-output zeroing, 2026-06-26). WB_LITE_DIR still overrides.


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
# E2B template. `semfs-baked-v2` bakes the plain corpus tarball (/opt/corpus.tgz) plus the q4,
# clean, leanhint3, and 4arm seeds so plain/best/hiddenkg can boot without large per-sandbox uploads.
# Falls back to upload only if /opt/corpus.tgz is absent.
TEMPLATE = os.environ.get("WB_E2B_TEMPLATE", "semfs-baked-v2")
OUT = (pathlib.Path(os.environ["WB_OUT"]).resolve() if os.environ.get("WB_OUT")
       else REPO / "tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs")
OUT.mkdir(parents=True, exist_ok=True)
CASES_FULL = ["15", "44", "45", "53", "55", "95", "171", "175", "386", "388"]   # 289 excluded (seed leak)
DEFAULT_ARMS = ["plain", "best", "hiddenkg"]
SUPPORTED_ARMS = {"plain", "cloud", "kg", "nokg", "nokgAK", "best", "hiddenkg", "hiddenkg_edges", "hiddenkg_l7", "hiddenkg_retrieval", "hiddenkg_retrieval_l7", "ppr_off", "ppr_on", "ppr_map"}
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


_DEADLINE_POOL = concurrent.futures.ThreadPoolExecutor(max_workers=96)


def _with_deadline(fn, hard_secs):
    """Run an e2b SDK call with a HARD wall-clock ceiling. A dead socket / SSL-handshake
    hang otherwise blocks ~20 min (no command-timeout covers CONNECTION setup) and wedges
    the worker → stalls the whole run. On overshoot we abandon the (leaked) call and raise
    so the worker fails the cell and moves on. (network-resilience, 2026-06-24)"""
    fut = _DEADLINE_POOL.submit(fn)
    try:
        return fut.result(timeout=hard_secs)
    except concurrent.futures.TimeoutError:
        raise TimeoutError(f"e2b call exceeded hard deadline {hard_secs}s (network hang)")


def sh(sbx, cmd, timeout=120, env=None):
    # hard ceiling = command timeout + 90s margin (covers connect + exec); a network hang
    # now fails fast instead of stalling ~20 min on a dead socket.
    r = _with_deadline(lambda: sbx.commands.run(cmd, timeout=timeout, envs=(env or {})), timeout + 90)
    return (r.stdout or ""), (r.stderr or "")

def is_sandbox_dead(exc):
    s = repr(exc) + str(exc)
    return any(x in s for x in ("SandboxNotFoundException","404","not found","CLOSED","LocalProtocolError","ConnectionInputs"))


MOUNT_ARMS = {"kg", "nokg", "nokgAK", "best", "hiddenkg", "hiddenkg_edges", "hiddenkg_l7", "hiddenkg_retrieval", "hiddenkg_retrieval_l7", "ppr_off", "ppr_on", "ppr_map"}   # arms that depend on a live semfs FUSE mount
SURFACE_OFF_ARMS = {"nokg", "nokgAK", "best", "hiddenkg", "hiddenkg_edges", "hiddenkg_l7", "hiddenkg_retrieval", "hiddenkg_retrieval_l7", "ppr_off", "ppr_on", "ppr_map"}
DEFAULT_SEED_SOURCES = {
    "kg": "/opt/chanpin-gemma-q4.db",
    "nokg": "/opt/chanpin-clean.db",
    "nokgAK": "/opt/chanpin-clean.db",
    "best": "/opt/chanpin-4arm.db",
    "hiddenkg": "/opt/chanpin-4arm.db",
    "hiddenkg_edges": "/opt/chanpin-4arm.db",
    "hiddenkg_l7": "/opt/chanpin-4arm.db",
    "hiddenkg_retrieval": "/opt/chanpin-4arm.db",
    "hiddenkg_retrieval_l7": "/opt/chanpin-4arm.db",
    # PPR A/B: per-persona seed supplied via WB_E2B_SEED_DEFAULT; this is the fallback.
    "ppr_off": "/opt/chanpin-gemma-q4.db",
    "ppr_on": "/opt/chanpin-gemma-q4.db",
    "ppr_map": "/opt/chanpin-gemma-q4.db",
}


def arm_mount_env(arm):
    me = {
        "SEMFS_EMBED_MODEL": "gemma-q4",
        "SEMFS_EMBED_ONNX_DIR": "/home/user/gemma_q4",
        "SUPERMEMORY_API_KEY": "dummy-local",
        "SEMFS_NO_PUSH": "1",
        "SEMFS_NO_SYNC": "1",
        # Runbook + measured E2B guidance: keep the normal tree available.
        # WB_SEARCH_ONLY=on for personas whose seed has no materialized fs_* tree
        # (seed_dir-built seeds, e.g. kaifa): the mount serves `semfs grep` only.
        "SEMFS_SEARCH_ONLY": os.environ.get("WB_SEARCH_ONLY", "off"),
        "SEMFS_DEBUG_RANKING": "1",
        "SEMFS_SEARCH_TIMEOUT_SECS": "120",
        "SEMFS_GREP_CLIENT_WAIT_SECS": "140",
        "SEMFS_SEARCH_DEADLINE_SECS": "90",
        # Surface is OFF unless an arm explicitly wants it.
        "SEMFS_GRAPH_FS": "off",
    }
    if arm in {"nokg", "nokgAK", "best"}:
        me["SEMFS_KG"] = "off"
        me["SEMFS_COMENTION"] = "off"
        me["SEMFS_HIDDEN_KG"] = "off"
        me["SEMFS_HIDDEN_KG_RETRIEVAL"] = "off"
    elif arm == "hiddenkg":
        me["SEMFS_KG"] = "off"
        me["SEMFS_COMENTION"] = "off"
        me["SEMFS_HIDDEN_KG"] = "on"
        me["SEMFS_HIDDEN_KG_RETRIEVAL"] = "off"
    elif arm == "hiddenkg_edges":
        me["SEMFS_KG"] = "off"
        me["SEMFS_COMENTION"] = "on"
        me["SEMFS_HIDDEN_KG"] = "off"
        me["SEMFS_HIDDEN_KG_RETRIEVAL"] = "off"
    elif arm == "hiddenkg_l7":
        me["SEMFS_KG"] = "off"
        me["SEMFS_COMENTION"] = "on"
        me["SEMFS_HIDDEN_KG"] = "on"
        me["SEMFS_HIDDEN_KG_RETRIEVAL"] = "off"
    elif arm in {"ppr_off", "ppr_on", "ppr_map"}:
        # PPR A/B — identical to hiddenkg_l7 (hidden KG + co-mention, no surface);
        # the ONLY difference is the graph-prior algorithm: 1-hop (off) vs PPR (on).
        # ppr_map == ppr_on retrieval PLUS a cached workspace map injected into the prompt
        # (run_cell) — so map-vs-no-map is the only variable isolated against ppr_on.
        me["SEMFS_KG"] = "off"
        me["SEMFS_COMENTION"] = "on"
        me["SEMFS_HIDDEN_KG"] = "on"
        me["SEMFS_HIDDEN_KG_RETRIEVAL"] = "off"
        me["SEMFS_KG_PPR"] = "on" if arm in ("ppr_on", "ppr_map") else "off"
    elif arm == "hiddenkg_retrieval":
        me["SEMFS_KG"] = "off"
        me["SEMFS_COMENTION"] = "off"
        me["SEMFS_HIDDEN_KG"] = "on"
        me["SEMFS_HIDDEN_KG_RETRIEVAL"] = "on"
    elif arm == "hiddenkg_retrieval_l7":
        me["SEMFS_KG"] = "off"
        me["SEMFS_COMENTION"] = "on"
        me["SEMFS_HIDDEN_KG"] = "on"
        me["SEMFS_HIDDEN_KG_RETRIEVAL"] = "on"
    elif arm == "kg":
        me["SEMFS_KG"] = "on"
        me["SEMFS_COMENTION"] = "on"
        me["SEMFS_HIDDEN_KG"] = "off"
        me["SEMFS_HIDDEN_KG_RETRIEVAL"] = "off"
    if arm == "nokgAK":
        me["SEMFS_ADAPTIVE_K"] = "on"
    if ORKEY:
        me["OPENROUTER_API_KEY"] = ORKEY
    if os.environ.get("WB_OR_MODEL"):
        me["WB_OR_MODEL"] = os.environ["WB_OR_MODEL"]
    me.update(KNOBS)
    return me


def arm_seed_source(arm):
    env_name = f"WB_E2B_SEED_{arm.upper()}"
    if os.environ.get(env_name):
        return os.environ[env_name]
    if os.environ.get("WB_E2B_SEED_DEFAULT"):
        return os.environ["WB_E2B_SEED_DEFAULT"]
    return DEFAULT_SEED_SOURCES.get(arm, "/opt/chanpin-gemma-q4.db")


def do_mount(sbx, arm):
    # --startup-timeout: the 30s default watchdog is too tight for the larger seeds
    # (houqin's materialized seed is ~1.24 GB → daemon passes `configuring_api` >30s).
    st = os.environ.get("WB_MOUNT_STARTUP_TIMEOUT", "240")
    o, e = sh(sbx, "semfs mount chanpin --path /home/user/ws/mnt --backend fuse "
                   f"--key dummy-local --no-sync --no-push --startup-timeout {st} 2>&1 || true",
              timeout=max(300, int(st) + 60), env=arm_mount_env(arm))
    return (o + e).strip()


def mount_live(sbx):
    """Mount-health gate (SEM-35): a DEAD FUSE mount silently yields garbage scores that
    confound the semfs arms (the daemon can die mid-run — OOM/panic — so this is checked
    PER CELL, not just at boot). Live ⇔ semfs reports an active mount AND /ws/mnt serves files."""
    lst, _ = sh(sbx, "semfs list 2>&1 || true", timeout=30)
    if "no active mounts" in lst.lower():
        return False
    # Search-only mounts (seed_dir-built seeds have no materialized fs_* tree) serve
    # `semfs grep` but expose no browsable files, so the file-count gate below is N/A —
    # an active mount is the liveness signal; preflight_arm still exercises a real grep.
    if os.environ.get("WB_SEARCH_ONLY") == "on":
        return True
    n, _ = sh(sbx, "ls -1 /home/user/ws/mnt 2>/dev/null | wc -l", timeout=30)
    try:
        return int(n.strip()) > 0
    except Exception:
        return False


def boot_prep(sbx, need_plain, need_mount=True):
    print(f"  boot-prep… (mount={need_mount}, plain={need_plain})", flush=True)
    sh(sbx, "mkdir -p ~/ws ~/run ~/.semfs ~/.codex && "
            "ln -sfn /opt/wb ~/wb && ln -sfn /opt/cases ~/cases && "
            "ln -sfn /opt/gemma_q4 ~/gemma_q4 && ln -sfn /opt/semfs-shims ~/semfs-shims"
            + (f" && cp {os.environ.get('WB_BOOT_SEED', '/opt/chanpin-gemma-q4.db')} ~/.semfs/chanpin.db" if need_mount else ""),
       timeout=180)
    sh(sbx, "echo user_allow_other | sudo tee -a /etc/fuse.conf >/dev/null")
    # Office writer libs (python-docx/pptx/openpyxl): the agent's /usr/bin/python3 has no pip,
    # and .pptx/.docx cases can't emit their deliverable without them (GLM loops on
    # `pip install python-pptx`). Baked into the v3 lineage originally; the rebuilt semfs-baked
    # (new E2B account) lacks them → install at boot (network ok; Ubuntu 24.04 PEP-668 →
    # --break-system-packages). Idempotent + best-effort. RCA 2026-06-23. Disable: WB_BOOT_WRITER_LIBS=0.
    if os.environ.get("WB_BOOT_WRITER_LIBS", "1") == "1":
        have, _ = sh(sbx, "python3 -c 'import docx,pptx,openpyxl' 2>/dev/null && echo HAVE || echo NEED", timeout=30)
        if "NEED" in have:
            o, _ = sh(sbx, "sudo apt-get update -qq && sudo apt-get install -y -qq python3-pip && "
                           "sudo python3 -m pip install --break-system-packages --no-cache-dir -q "
                           "python-docx python-pptx openpyxl && "
                           "python3 -c 'import docx,pptx,openpyxl; print(\"writerlibs OK\")' || echo 'writerlibs FAILED'",
                      timeout=600)
            print("  writer libs:", (o.strip().splitlines() or ["(no output)"])[-1], flush=True)
        else:
            print("  writer libs: already baked", flush=True)
    # Push the FIXED semfs binary (built on Modal, x86_64-linux) over the baked one,
    # BEFORE any mount so the daemon spawns from it. Carries the timeout fix
    # (rcas/2026-06-15-grep-timeout-cloud-fallback-panic.md): no cloud-fallback panic +
    # 120s search bound. Skipped if the local artifact is absent (falls back to baked).
    if FIXED_BIN and FIXED_BIN.exists():
        sbx.files.write("/tmp/semfs-fixed", FIXED_BIN.read_bytes())
        o, _ = sh(sbx, "sudo cp /tmp/semfs-fixed /usr/local/bin/semfs && sudo chmod +x /usr/local/bin/semfs && /usr/local/bin/semfs --version")
        print("  semfs binary:", o.strip()[:60], "(FIXED)", flush=True)
    # Push the FIXED grep/rg shims over the baked ones. The baked shims ship mode 644
    # (chmod a+rX never made them executable) AND carry the old `! -t 0` tty guard + no
    # recursive-flag routing — so a baked-only run leaves codex's `grep` literal. These
    # repo copies fix all three; chmod +x so the PATH-prepend actually resolves them.
    for _shim in ("grep", "rg", "_fmt.py"):
        _p = REPO / "benchmarks/workspace_bench/semfs-shims" / _shim
        if _p.exists():
            sbx.files.write(f"/tmp/shim_{_shim}", _p.read_text())
            sh(sbx, f"sudo cp /tmp/shim_{_shim} /opt/semfs-shims/{_shim}")
    sh(sbx, "sudo chmod +x /opt/semfs-shims/grep /opt/semfs-shims/rg")
    print("  semfs-shims: fixed grep/rg/_fmt pushed + chmod +x", flush=True)
    # upload patched files
    sbx.files.write("/home/user/cell_driver.py", CELL_DRIVER.read_text())
    sbx.files.write("/home/user/semfs_map.py", SEMFS_MAP.read_text())   # workspace-map generator (ppr_map)
    # Pre-ship a known-good workspace map (depends only on the seed → identical for every cell of a
    # persona) so the ppr_map arm SKIPS the fragile per-sandbox gen, which exited 2 on ~79% of houqin
    # cells (RCA 2026-06-27). Shipping once is robust AND scientifically cleaner (byte-identical map).
    _pmap = os.environ.get("WB_PRESHIP_MAP", "")
    if _pmap and pathlib.Path(_pmap).exists():
        sbx.files.write("/home/user/workspace_map.txt", pathlib.Path(_pmap).read_text())
        print(f"  pre-shipped workspace_map.txt ({len(pathlib.Path(_pmap).read_text()) // 4} tok)", flush=True)
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
    if need_plain:
        # Prefer the corpus tarball BAKED into the template (/opt/corpus.tgz on semfs-baked-v2):
        # extract in-sandbox → 0 upload. Fall back to the chunked upload on the old semfs-baked.
        baked, _ = sh(sbx, "test -f /opt/corpus.tgz && echo BAKED || echo NOBAKE", timeout=20)
        if "BAKED" in baked:
            sh(sbx, "mkdir -p ~/ws/plain && tar xzf /opt/corpus.tgz -C ~/ws/plain --strip-components=1", timeout=300)
            n, _ = sh(sbx, "find ~/ws/plain -type f | wc -l")
            print("  plain files (baked /opt/corpus.tgz, 0-upload):", n.strip(), flush=True)
        else:
            import subprocess, tempfile
            tf = tempfile.mktemp(suffix=".tgz")
            subprocess.run(["tar", "czf", tf, "-C", os.path.dirname(CORPUS), os.path.basename(CORPUS)], check=True)
            data = pathlib.Path(tf).read_bytes()
            pathlib.Path(tf).unlink(missing_ok=True)   # free the 442MB local tarball now (ENOSPC leak fix)
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


def seed_exists(sbx, src):
    out, _ = sh(sbx, f"if test -f {src}; then echo OK; else echo MISSING; fi", timeout=30)
    return "OK" in out


def verify_seed_inventory(sbx, arms):
    checked, missing = [], []
    for arm in arms:
        if arm not in MOUNT_ARMS:
            continue
        src = arm_seed_source(arm)
        checked.append((arm, src))
        if not seed_exists(sbx, src):
            missing.append((arm, src))
    if checked:
        print("  seed inventory:", flush=True)
        for arm, src in checked:
            print(f"    {arm:<8} {src}", flush=True)
    if missing:
        msg = ", ".join(f"{arm}→{src}" for arm, src in missing)
        raise RuntimeError(f"required seed missing in template/sandbox: {msg}")


def reset_runtime_seed(sbx, arm):
    # Restore the runtime DB from the arm-specific seed before EACH semfs-arm mount
    # so one arm's cleanup cannot contaminate the next arm.
    src = arm_seed_source(arm)
    if not seed_exists(sbx, src):
        raise RuntimeError(f"seed missing for arm={arm}: {src}")
    sh(sbx, f"cp {src} ~/.semfs/chanpin.db", timeout=180)


def unmount_semfs(sbx):
    sh(sbx, "semfs unmount chanpin --force 2>/dev/null || true; "
            "fusermount -u /home/user/ws/mnt 2>/dev/null || true; sleep 2", timeout=60)


def clean_surface_artifacts(sbx):
    # The canonical seed may already contain baked KG artifacts. Remove them via
    # the mounted FS so surface-off arms are actually surface-off.
    cmd = (
        "rm -rf /home/user/ws/mnt/kg 2>/dev/null || true; "
        "rm -f /home/user/ws/mnt/AGENTS.md /home/user/ws/mnt/CLAUDE.md "
        "/home/user/ws/mnt/AGENTS.md.semfs-error.txt /home/user/ws/mnt/CLAUDE.md.semfs-error.txt 2>/dev/null || true"
    )
    sh(sbx, cmd, timeout=60)


def surface_artifacts_present(sbx):
    kg_dir, _ = sh(sbx, "test -e /home/user/ws/mnt/kg && echo PRESENT || echo ABSENT", timeout=20)
    agents, _ = sh(sbx, "find /home/user/ws/mnt -maxdepth 1 \\( -name 'AGENTS.md' -o -name 'CLAUDE.md' \\) -print",
                   timeout=20)
    return ("PRESENT" in kg_dir) or bool(agents.strip())


def ensure_mount_for_arm(sbx, arm):
    if arm not in MOUNT_ARMS:
        return
    unmount_semfs(sbx)
    reset_runtime_seed(sbx, arm)
    tail = do_mount(sbx, arm)[-200:]
    print(f"    mount({arm}): {tail}", flush=True)
    if not mount_live(sbx):
        raise RuntimeError(f"mount failed for arm={arm}")
    if arm in SURFACE_OFF_ARMS:
        clean_surface_artifacts(sbx)
        if surface_artifacts_present(sbx):
            raise RuntimeError(
                f"surface contamination persists for arm={arm}; rebuild or replace the seed"
            )


def preflight_arm(sbx, arm):
    ensure_mount_for_arm(sbx, arm)
    root, _ = sh(sbx, "ls -1 /home/user/ws/mnt | head -15", timeout=30)
    kg, _ = sh(sbx, "ls -1 /home/user/ws/mnt/kg 2>&1 | head -10", timeout=30)
    grep, _ = sh(sbx, "semfs grep 'best selling product' /home/user/ws/mnt 2>&1 | head -25",
                 timeout=180, env=arm_mount_env(arm))
    grep_head = "\n".join(grep.strip().splitlines()[:3]).lower()
    auth_error = (
        "error: auth failed" in grep_head
        or "401 unauthorized" in grep_head
        or "http 401" in grep_head
    )
    if auth_error:
        status, _ = sh(sbx, "semfs status chanpin 2>&1 | head -40", timeout=30)
        marker, _ = sh(sbx, "cat /home/user/ws/.semfs 2>&1 | head -40", timeout=30)
        log_tail, _ = sh(
            sbx,
            "tail -80 /home/user/.cache/semfs/logs/chanpin.log 2>&1",
            timeout=30,
        )
        print(f"=== PREFLIGHT {arm} FAILURE ===", flush=True)
        print("[status]", flush=True)
        print(status.strip(), flush=True)
        print("[marker]", flush=True)
        print(marker.strip(), flush=True)
        print("[log_tail]", flush=True)
        print(log_tail.strip(), flush=True)
        raise RuntimeError(f"preflight grep auth failed for arm={arm}: {grep.strip()[:200]}")
    print(f"=== PREFLIGHT {arm} ===", flush=True)
    print("[root]", flush=True)
    print(root.strip(), flush=True)
    print("[kg]", flush=True)
    print(kg.strip(), flush=True)
    print("[grep]", flush=True)
    print(grep.strip(), flush=True)


def print_seed_contract(arms):
    semfs_arms = [a for a in arms if a in MOUNT_ARMS]
    if not semfs_arms:
        return
    print("seed contract:", flush=True)
    for arm in semfs_arms:
        print(f"  {arm:<8} {arm_seed_source(arm)}", flush=True)


def run_cell(sbx, agent, case, arm, rep, real_rg, remount=True):
    label = f"pm_{agent}_{case}_{arm}_r{rep}"
    sbx.set_timeout(3600)
    print(f"  ▶ {label} …", flush=True)
    # MOUNT: full re-mount only when the arm changed (remount=True) — else just the cheap
    # SEM-35 health gate (mount_live), so the queue worker doesn't pay a ~30-60s unmount+
    # re-seed+remount on EVERY same-arm cell. A dead daemon still triggers a re-mount.
    if arm in MOUNT_ARMS and (remount or not mount_live(sbx)):
        # RETRY the (re)mount: the big houqin seed's daemon occasionally exits-1 on the
        # ppr_off→ppr_on re-mount; a single failure used to drop the cell to infra_fail
        # (38 houqin ppr_on cells lost on the 2026-06-25 resume). Retries usually self-heal.
        mounted = False
        for mt in range(3):
            try:
                ensure_mount_for_arm(sbx, arm); mounted = True; break
            except Exception as ex:
                print(f"    mount retry {mt+1}/3 {label}: {repr(ex)[:100]}", flush=True)
        if not mounted:
            print(f"    ✗ INFRA_FAIL {label}: mount failed after 3 tries", flush=True)
            res = {"label": label, "agent": agent, "case": case, "arm": arm, "status": "infra_fail_mount"}
            d = OUT / label; d.mkdir(parents=True, exist_ok=True)
            (d / "result.json").write_text(json.dumps(res, ensure_ascii=False, indent=2))
            return res
    # One daemon log accumulates ALL cells on a sandbox; record the line offset NOW so the
    # pull can slice out just THIS cell's RANKDUMP/pipeline-counts lines (per-query rank order).
    _ls, _ = sh(sbx, "wc -l < /home/user/.cache/semfs/logs/chanpin.log 2>/dev/null || echo 0")
    try:
        log_start = int((_ls or "0").split()[0]) + 1
    except Exception:
        log_start = 1
    # ppr_map arm: generate the cached workspace map from the seed ONCE per sandbox, then
    # inject it into the agent prompt (cell_driver reads WB_WORKSPACE_MAP). Same retrieval as
    # ppr_on → map-vs-no-map is the only variable.
    wsmap_path = ""
    if arm == "ppr_map":
        seed = arm_seed_source(arm)
        # Pre-shipped map (boot_prep) makes `test -f` pass → gen is skipped. If it's absent we fall
        # back to in-sandbox gen, but capture stderr to the exception (2>&1) so a failure is visible.
        sh(sbx, f"test -f /home/user/workspace_map.txt || python3 /home/user/semfs_map.py {seed} "
                f"--out /home/user/workspace_map.txt 2>&1", timeout=240)
        wsmap_path = "/home/user/workspace_map.txt"
    env = {"OPENROUTER_API_KEY": ORKEY, "WB_REAL_RG": real_rg, "HOME": "/home/user",
           "WB_WORKSPACE_MAP": wsmap_path,
           "CLAUDE_CODE_OAUTH_TOKEN": CLAUDE_OAUTH,
           "SUPERMEMORY_API_KEY": os.environ.get("SUPERMEMORY_API_KEY", ""),
           "SUPERMEMORY_API_URL": os.environ.get("SUPERMEMORY_API_URL", ""),
           "WB_FORCE_OPENROUTER": os.environ.get("WB_FORCE_OPENROUTER", ""),  # 1 = skip native, use OpenRouter
           "WB_OR_MODEL": os.environ.get("WB_OR_MODEL", ""),  # OpenRouter model override (e.g. z-ai/glm-5.1)
           # Modal self-hosted GLM-5.1 (vLLM) path: WB_MODAL_GLM=1 routes codex at the vLLM endpoint;
           # the WB codex harness's chat-adapter (auto-on for non-"gpt-" models) bridges responses->chat
           # AND drops the multi_agent namespace tool, so no LiteLLM hop / --disable multi_agent needed.
           "WB_MODAL_GLM": os.environ.get("WB_MODAL_GLM", ""),
           "MODAL_VLLM_API_KEY": os.environ.get("MODAL_VLLM_API_KEY", ""),
           "WB_MODAL_BASE": os.environ.get("WB_MODAL_BASE", ""),    # vLLM /v1 base override (e.g. nvfp4 endpoint)
           "WB_MODAL_MODEL": os.environ.get("WB_MODAL_MODEL", ""),  # served model name (e.g. claude-haiku-4-5-20251001)
           "WB_AGENT_TIMEOUT": os.environ.get("WB_AGENT_TIMEOUT", ""),  # propagate to in-sandbox cell_driver (else it defaults 1500s=25min)
           "WB_OUTPUT_FILES": expected_output_files(case)}  # filename hint → cell_driver prompt
    env.update(KNOBS)   # sweep overrides reach the grep client (caps/result-limit/rewrite)
    cell_timeout = int(os.environ.get("WB_CELL_TIMEOUT") or 1750)  # > WB_AGENT_TIMEOUT (driver wraps the agent)
    o, e = sh(sbx, f"cd /home/user && python3 cell_driver.py --label {label} --agent {agent} "
                   f"--case {case} --arm {arm} 2>>/tmp/{label}.err", timeout=cell_timeout, env=env)
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
    persona = os.environ.get("WB_PERSONA", "")
    with _JSONL_LOCK:
        with open(OUT / "results.jsonl", "a") as f:
            rec = {k: res.get(k) for k in
                   ("label", "agent", "case", "arm", "auth_used", "status", "tokens", "calls",
                    "used_semfs_grep", "deliverables")}
            rec["persona"] = persona
            rec["rep"] = rep
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    print(f"    {res.get('status')}  tokens={res.get('tokens')} calls={res.get('calls')} "
          f"semfs_grep={res.get('used_semfs_grep')} auth={res.get('auth_used')}", flush=True)
    # Inline judging (WB_INLINE_JUDGE=1, default on): grade THIS cell the moment it lands.
    # The Seed-2.0-Lite judge is a SEPARATE endpoint → no GLM contention, runs in the worker
    # thread. Scores stream to judged.jsonl (the live dashboard) and judging is already done
    # when the run ends. Best-effort: a judge failure never fails the cell.
    if os.environ.get("WB_INLINE_JUDGE", "1") == "1" and res.get("status") == "ok":
        try:
            import sys as _sys
            _sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
            from run_judge import judge as judge_cell
            _jl, jstatus, jscore = judge_cell(label)
            with _JSONL_LOCK:
                with open(OUT / "judged.jsonl", "a") as jf:
                    jf.write(json.dumps({"label": label, "persona": persona, "agent": agent,
                                         "case": case, "arm": arm, "rep": rep,
                                         "judge_status": jstatus, "score": jscore},
                                        ensure_ascii=False) + "\n")
            print(f"    judged: {jstatus} score={jscore}", flush=True)
        except Exception as ex:
            print(f"    inline judge failed: {repr(ex)[:120]}", flush=True)
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


def create_sandbox(retries=7):
    """Sandbox.create with backoff on E2B 429 (concurrency / create-rate cap). At higher
    PAR the worker pool issues a burst of creates; without this a 429 kills the worker
    thread and silently drops its cells. Backoff lets the burst drain. (2026-06-24)"""
    delay = 8.0
    for a in range(retries):
        try:
            # hard 200s ceiling so a network/SSL hang during create fails fast (retryable)
            return _with_deadline(lambda: Sandbox.create(template=TEMPLATE, timeout=3600), 200)
        except Exception as ex:
            s = (repr(ex) + str(ex)).lower()
            transient = ("429" in s or "rate" in s or "concurren" in s or "too many" in s
                         or "hard deadline" in s or "network hang" in s or "timed out" in s)
            if transient and a < retries - 1:
                print(f"  [create] transient ({repr(ex)[:50]}) — backoff {delay:.0f}s (try {a+1}/{retries})", flush=True)
                time.sleep(delay); delay = min(delay * 1.8, 90)
                continue
            raise


def worker(wid, cells_q, need_plain, need_mount, rep):
    """One sandbox; pull cells from the shared queue until empty; reboot on death."""
    sbx = create_sandbox()
    print(f"[w{wid}] sandbox {sbx.sandbox_id}", flush=True)
    try:
        real_rg = boot_prep(sbx, need_plain, need_mount)
        queued_arms = list({arm for _, _, arm in list(cells_q.queue)})
        verify_seed_inventory(sbx, queued_arms)
    except Exception as ex:
        print(f"[w{wid}] boot-prep failed: {repr(ex)[:150]} — retrying once", flush=True)
        try: sbx.kill()
        except Exception: pass
        sbx = create_sandbox()
        real_rg = boot_prep(sbx, need_plain, need_mount)
        queued_arms = list({arm for _, _, arm in list(cells_q.queue)})
        verify_seed_inventory(sbx, queued_arms)
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
                            sbx = create_sandbox()
                            real_rg = boot_prep(sbx, need_plain, need_mount)
                        except Exception as ex2:
                            print(f"[w{wid}] reboot failed: {repr(ex2)[:120]}", flush=True)
            finally:
                cells_q.task_done()
    finally:
        try: sbx.kill()
        except Exception: pass
        print(f"[w{wid}] done, sandbox killed.", flush=True)


def mount_sig(arm):
    """Mount identity of an arm — arms with the SAME signature share a FUSE mount, so the queue
    worker need NOT re-mount between them (e.g. ppr_on vs ppr_map differ only in the prompt-level
    map, not the mount). Re-mount fires only when this changes → continuous dequeue across
    same-mount arms (no batch barrier) AND no needless big-seed re-mount (the crash, 2026-06-25)."""
    if arm not in MOUNT_ARMS:
        return ("plain",)
    e = arm_mount_env(arm)
    return (arm_seed_source(arm), e.get("SEMFS_KG"), e.get("SEMFS_COMENTION"),
            e.get("SEMFS_HIDDEN_KG"), e.get("SEMFS_HIDDEN_KG_RETRIEVAL"), e.get("SEMFS_KG_PPR"))


def worker_batch(wid, cells_q, need_plain, need_mount):
    """Per-persona QUEUE worker: boots the sandbox ONCE, then dequeues (ag,c,arm,rep) across
    ALL reps + arms, re-mounting ONLY when the arm changes (run_cell remount=...). Removes
    the per-(arm,rep) re-boot (~10 min each), the per-arm end-barrier (long-tail), and the
    per-cell unmount+re-seed+remount. A slow straggler no longer blocks others — they keep
    dequeuing. (queue harness, 2026-06-24)"""
    def boot(s):
        rg = boot_prep(s, need_plain, need_mount)
        queued_arms = list({arm for _, _, arm, _ in list(cells_q.queue)})
        verify_seed_inventory(s, queued_arms)
        return rg
    sbx = create_sandbox()
    print(f"[w{wid}] sandbox {sbx.sandbox_id}", flush=True)
    try:
        real_rg = boot(sbx)
    except Exception as ex:
        print(f"[w{wid}] boot-prep failed: {repr(ex)[:150]} — retrying once", flush=True)
        try: sbx.kill()
        except Exception: pass
        sbx = create_sandbox(); real_rg = boot(sbx)
    cur_sig = None  # mount signature currently mounted (None → must mount); re-mount only on change
    try:
        while True:
            try:
                ag, c, arm, rep = cells_q.get_nowait()
            except queue.Empty:
                break
            label = f"pm_{ag}_{c}_{arm}_r{rep}"
            try:
                for attempt in range(3):
                    try:
                        sig = mount_sig(arm)
                        res = run_cell(sbx, ag, c, arm, rep, real_rg, remount=(sig != cur_sig))
                        cur_sig = sig if res.get("status") != "infra_fail_mount" else None
                        break
                    except Exception as ex:
                        if not is_sandbox_dead(ex) or attempt == 2:
                            print(f"[w{wid}] CELL ERROR {label} (try {attempt+1}): {repr(ex)[:140]}", flush=True)
                            break
                        print(f"[w{wid}] sandbox dead — rebooting (try {attempt+1}) for {label}", flush=True)
                        try: sbx.kill()
                        except Exception: pass
                        try:
                            sbx = create_sandbox(); real_rg = boot_prep(sbx, need_plain, need_mount); cur_sig = None
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
    ap.add_argument("--preflight", action="store_true",
                    help="boot one sandbox and print mount/grep evidence for the selected semfs arms.")
    ap.add_argument("--smoke", action="store_true")
    ap.add_argument("--full", action="store_true")
    ap.add_argument("--cases", default=None,
                    help="comma list of case ids to run (e.g. '45,53,55,171,175'). Overrides --smoke/--full.")
    ap.add_argument("--rep", default="1")
    ap.add_argument("--reps", default=None,
                    help="comma list of reps for QUEUE/BATCH mode (e.g. '1,2,3'): ONE per-persona "
                         "invocation runs all reps x arms via a global queue, boot once/sandbox, "
                         "remount only on arm change. Requires --cases. Overrides --rep.")
    ap.add_argument("--agents", default=None,
                    help="comma list to restrict/order agents, e.g. 'codex' or 'claude'.")
    ap.add_argument("--arms", default=None,
                    help="comma list to restrict arms, e.g. 'cloud' or 'plain,nokg'. Default: plain,nokg,nokgAK.")
    ap.add_argument("--parallel", type=int, default=1, help="number of concurrent sandboxes (pool size).")
    ap.add_argument("--force", action="store_true", help="re-run cells even if a prior ok result.json exists.")
    ap.add_argument("--knobs", default=None, help="JSON file of SEMFS_* knob overrides for the optimization sweep.")
    args = ap.parse_args()
    if not ORKEY:
        sys.exit("OPENROUTER_API_KEY not in env — `set -a; . ./.env; set +a` first")
    if args.knobs:
        KNOBS.update({k: str(v) for k, v in json.loads(pathlib.Path(args.knobs).read_text()).items()})
        print(f"knob overrides: {KNOBS}", flush=True)

    agents = [a.strip() for a in args.agents.split(",")] if args.agents else AGENTS
    arms = [a.strip() for a in args.arms.split(",")] if args.arms else DEFAULT_ARMS
    bad_arms = [a for a in arms if a not in SUPPORTED_ARMS]
    if bad_arms:
        sys.exit(f"unsupported arms: {bad_arms}; supported={sorted(SUPPORTED_ARMS)}")
    if args.preflight:
        semfs_arms = [a for a in arms if a in MOUNT_ARMS]
        if not semfs_arms:
            sys.exit("--preflight needs at least one semfs arm")
        sbx = create_sandbox()
        print(f"[preflight] sandbox {sbx.sandbox_id}", flush=True)
        try:
            print_seed_contract(semfs_arms)
            boot_prep(sbx, need_plain=False, need_mount=True)
            verify_seed_inventory(sbx, semfs_arms)
            for arm in semfs_arms:
                preflight_arm(sbx, arm)
        finally:
            try:
                sbx.kill()
            except Exception:
                pass
            print("[preflight] done, sandbox killed.", flush=True)
        return
    # ---- QUEUE/BATCH mode: one per-persona invocation, all reps x arms in one queue ----
    if args.reps:
        if not args.cases:
            sys.exit("--reps requires --cases")
        reps = [r.strip() for r in args.reps.split(",") if r.strip()]
        sel = [c.strip() for c in args.cases.split(",") if c.strip()]
        # arm-ORDERED: each worker does all of one arm before the next → ≤1 remount/worker.
        cells = [(ag, c, arm, rep) for ag in agents for arm in arms for rep in reps for c in sel]
        cells = [t for t in cells if not _should_skip(f"pm_{t[0]}_{t[1]}_{t[2]}_r{t[3]}", args.force)]
        need_plain = any(arm == "plain" for _, _, arm, _ in cells)
        need_mount = any(arm in MOUNT_ARMS for _, _, arm, _ in cells)
        OUT.mkdir(parents=True, exist_ok=True)
        if not cells:
            print("nothing to run (all skipped).", flush=True); return
        n = max(1, min(args.parallel, len(cells)))
        print_seed_contract([arm for _, _, arm, _ in cells])
        print(f"[BATCH] {len(cells)} cells (reps={reps} arms={arms}) across {n} sandbox(es) — "
              f"boot once/sandbox, remount only on arm change", flush=True)
        cells_q = queue.Queue()
        for t in cells:
            cells_q.put(t)
        threads = [threading.Thread(target=worker_batch, args=(i, cells_q, need_plain, need_mount), daemon=True)
                   for i in range(n)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        print("ALL WORKERS DONE.", flush=True)
        return

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
    need_mount = any(arm in MOUNT_ARMS for _, _, arm in cells)

    OUT.mkdir(parents=True, exist_ok=True)
    if not cells:
        print("nothing to run (all skipped).", flush=True); return
    n = max(1, min(args.parallel, len(cells)))
    print_seed_contract([arm for _, _, arm in cells])
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
