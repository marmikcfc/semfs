"""Modal-hosted E2B orchestrator — runs WB-Lite cells in the cloud, OFF the local Mac, so a
flaky local connection can't kill the run (it died 4/16 on a blip, 2026-06-26).

LEAN image: the 35 MB semfs binary lives on a Modal VOLUME (uploaded once via `modal volume put`),
NOT bundled in the image — so the image is tiny and builds in seconds. Modal's network drives the
E2B sandboxes; cells write results + deliverables to an output Volume (committed every 90s so
progress is pullable mid-run). Judging is done LOCALLY post-hoc.

ENGINE switch (gate GPU spend): engine="openrouter" runs the agent via OpenRouter (NO GPU) for a
validation smoke; engine="glm" uses the self-hosted GLM vLLM (deploy + warm it first).

Prereq (one-time):  modal volume put semfs-bin benchmarks/e2b/assets/semfs-fixed /semfs-fixed
Smoke (no GPU):     modal run --detach benchmarks/modal/smoke_orchestrator.py --engine openrouter --cases 358 --arms ppr_map --reps 1
Full (GPU):         modal run --detach benchmarks/modal/smoke_orchestrator.py --engine glm --cases 358,357,251,267 --arms ppr_on,ppr_map --reps 1,2
Pull results:       modal volume get wblite-map-smoke-out /map_smoke <local_dir>
"""
import modal, pathlib

try:
    REPO = pathlib.Path(__file__).resolve().parents[2]   # local (repo root) — used at build time
except IndexError:
    REPO = pathlib.Path("/work")                          # in-cloud: image already built, paths exist
A = "/work/benchmarks/e2b"
VENDOR_JS = "benchmarks/vendor/Workspace-Bench/evaluation/baselines/ClaudeCode.js"

app = modal.App("wblite-map-smoke")

# Tiny image: code + e2b SDK + the small judge metadata. NO 35 MB binary (that's on a volume).
image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("e2b==2.28.2", "requests", "httpx")
    .add_local_file(str(REPO / "benchmarks/e2b/run_matrix.py"), f"{A}/run_matrix.py")
    .add_local_file(str(REPO / "benchmarks/e2b/cell_driver.py"), f"{A}/cell_driver.py")
    .add_local_file(str(REPO / "benchmarks/e2b/semfs_map.py"), f"{A}/semfs_map.py")
    .add_local_file(str(REPO / "benchmarks/e2b/run_judge.py"), f"{A}/run_judge.py")
    .add_local_file(str(REPO / "benchmarks/e2b/knobs/ppr_ab.json"), f"{A}/knobs/ppr_ab.json")
    .add_local_dir(str(REPO / "benchmarks/e2b/assets/wb_lite_all"), f"{A}/assets/wb_lite_all")
    .add_local_file(str(REPO / VENDOR_JS), f"/work/{VENDOR_JS}")
    .add_local_file(str(REPO / "codex_auth.json"), "/work/codex_auth.json")
)

out_vol = modal.Volume.from_name("wblite-map-smoke-out", create_if_missing=True)
bin_vol = modal.Volume.from_name("semfs-bin", create_if_missing=True)  # holds /semfs-fixed (35MB)


@app.function(image=image, secrets=[modal.Secret.from_dotenv()],
              volumes={"/out": out_vol, "/binvol": bin_vol}, timeout=3 * 3600, cpu=4.0)
def run_smoke(engine="openrouter", cases="358", arms="ppr_map", reps="1", par="8",
              or_model="z-ai/glm-5.1", persona="houqin"):
    import os, subprocess, threading, time
    os.chdir("/work")
    e = dict(os.environ)
    e.update({
        "WB_FIXED_BIN": "/binvol/semfs-fixed",               # from the volume, not the image
        "WB_OUT": "/out/map_smoke",
        "WB_LITE_DIR": f"{A}/assets/wb_lite_all/lite_all/task_lite_clean_en",
        "WB_E2B_TEMPLATE": f"semfs-mount-{persona}",
        "WB_E2B_SEED_DEFAULT": f"/opt/{persona}-gemma-q4.db",
        "WB_BOOT_SEED": f"/opt/{persona}-gemma-q4.db",
        "WB_PERSONA": persona,
        "WB_SEARCH_ONLY": "off",
        "WB_AGENT_TIMEOUT": "2000", "WB_CELL_TIMEOUT": "2300", "WB_MOUNT_STARTUP_TIMEOUT": "240",
    })
    if engine == "openrouter":
        e["WB_FORCE_OPENROUTER"] = "1"          # agent runs via OpenRouter — NO GPU
        e["WB_OR_MODEL"] = or_model
        e["WB_MODAL_GLM"] = ""
    else:  # glm (self-hosted vLLM — must be deployed + warm)
        e["WB_MODAL_GLM"] = "1"
        e["WB_MODAL_BASE"] = "https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1"
        e["WB_MODAL_MODEL"] = "glm-5.1-nvfp4"
        e["MODAL_VLLM_API_KEY"] = e.get("MODAL_GLM_VLLM_API_KEY") or e.get("MODAL_VLLM_API_KEY", "")
        e["WB_FORCE_OPENROUTER"] = ""
    os.makedirs("/out/map_smoke", exist_ok=True)
    stop = {"v": False}
    def committer():
        while not stop["v"]:
            time.sleep(90)
            try: out_vol.commit()
            except Exception: pass
    threading.Thread(target=committer, daemon=True).start()
    cmd = ["python3", f"{A}/run_matrix.py", "--cases", cases, "--agents", "codex",
           "--arms", arms, "--reps", reps, "--parallel", par, "--knobs", f"{A}/knobs/ppr_ab.json"]
    print(f"ENGINE={engine}  RUN: {' '.join(cmd)}", flush=True)
    r = subprocess.run(cmd, env=e)
    stop["v"] = True
    out_vol.commit()
    print(f"######## SMOKE (modal, {engine}) DONE exit={r.returncode} ########", flush=True)


@app.local_entrypoint()
def main(engine: str = "openrouter", cases: str = "358", arms: str = "ppr_map", reps: str = "1",
         par: str = "8", or_model: str = "z-ai/glm-5.1", persona: str = "houqin"):
    run_smoke.remote(engine=engine, cases=cases, arms=arms, reps=reps, par=par,
                     or_model=or_model, persona=persona)
