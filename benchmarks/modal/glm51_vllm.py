"""GLM-5.1-FP8 served from Modal via vLLM — a Codex-compatible eval backend.

Why this exists
---------------
The eval harness (Workspace-Bench / evo) drives the **Codex CLI**, which speaks the
OpenAI *Responses* API (`/v1/responses`). vLLM's OpenAI-compatible server exposes that
endpoint (plus `/v1/chat/completions` and `/v1/models`), so pointing Codex at this
deployment lets us benchmark `zai-org/GLM-5.1-FP8` exactly like any OpenAI provider.

Shape
-----
  download_weights()  CPU function — pulls the ~860GB FP8 repo into a Modal Volume ONCE.
  serve()             8xH200 web server — `vllm serve`, scale-to-zero, one box per wave.

Model facts that constrain this file
------------------------------------
  * GLM-5.1-FP8 is a 754B MoE; FP8 weights ~860GB → needs 8xH200 (won't fit 8xH100/640GB).
  * vLLM 0.19.0 stable is the recipe-recommended runtime; FP8 wants DeepGEMM.
  * Recipe serve flags: TP=8, --tool-call-parser glm47, --reasoning-parser glm45,
    --enable-auto-tool-choice, MTP speculative decode.

Usage
-----
  modal deploy benchmarks/modal/glm51_vllm.py                 # register app + web endpoint
  modal run    benchmarks/modal/glm51_vllm.py::download_weights   # stage weights (once, long)
  # endpoint:  https://<workspace>--glm51-vllm-serve.modal.run
See benchmarks/modal/GLM51_RUNBOOK.md for the full operational guide.
"""

import os
import subprocess

import modal

# --- identity / pins -------------------------------------------------------
APP_NAME = "glm51-vllm"
MODEL_NAME = "zai-org/GLM-5.1-FP8"          # 754B MoE, FP8
SERVED_NAME = "glm-5.1"                       # the name Codex/clients ask for
VLLM_IMAGE_TAG = "glm51"                       # GLM-5.1-specific image: vLLM 0.19.0 +
                                               # transformers>=5.4.0 (knows glm_moe_dsa) +
                                               # DeepGEMM. The generic v0.19.0 tag has a
                                               # too-old transformers and fails on the arch.
MODELS_DIR = "/models"                         # HF cache home, backed by the Volume
N_GPU = 8                                       # 8xH200 — dictated by the 860GB weights
VLLM_PORT = 8000

MINUTES = 60

app = modal.App(APP_NAME)

# 860GB of FP8 weights live here so the GPU box never re-downloads on cold start.
weights_volume = modal.Volume.from_name("glm51-weights", create_if_missing=True)

# DeepGEMM JIT-compiles CUDA kernels on first inference and writes them here.
# Persisting across cold starts eliminates the ~13 min warmup on subsequent boots.
kernel_cache_volume = modal.Volume.from_name("glm51-kernels", create_if_missing=True)

# vLLM's official OpenAI-compatible server image, pinned to the stable release the
# GLM-5 recipe calls for. We clear the image entrypoint so Modal controls the process,
# and point HF at the Volume so weights resolve from cache.
vllm_image = (
    modal.Image.from_registry(
        f"vllm/vllm-openai:{VLLM_IMAGE_TAG}",
        # The vLLM image ships its interpreter as `python3`; Modal's builder calls
        # `python`. Symlink it BEFORE Modal's setup runs so the build stays inside
        # vLLM's own env (keeps DeepGEMM/FP8 support; `add_python` would shadow it).
        setup_dockerfile_commands=["RUN ln -sf $(which python3) /usr/local/bin/python"],
    )
    .pip_install("huggingface_hub[hf_transfer]")
    # Use the glm51 image's BUNDLED vLLM (0.19.1.dev1) — its /v1/chat/completions + GLM tool
    # calls are stable (verified). Codex needs /v1/responses, which crashes here, so a
    # separate LiteLLM proxy translates responses->chat in front of this endpoint.
    # Modal's injected client requirements downgrade typing_extensions to a version lacking
    # `Sentinel` → breaks pydantic_core → vLLM import. Restore it LAST. --no-deps = surgical.
    .run_commands(
        "python -m pip install --no-deps --force-reinstall typing_extensions==4.15.0"
    )
    .env(
        {
            "HF_HOME": MODELS_DIR,
            "HF_HUB_ENABLE_HF_TRANSFER": "1",   # fast multi-connection downloads
            "VLLM_USE_DEEP_GEMM": "1",           # FP8 grouped-GEMM fast path
            "DEEP_GEMM_CACHE_DIR": "/root/.deep_gemm",  # persisted via kernel_cache_volume
        }
    )
    .entrypoint([])
)


@app.function(
    image=vllm_image,
    volumes={MODELS_DIR: weights_volume},
    cpu=8.0,
    memory=32768,
    timeout=6 * 60 * MINUTES,   # the 860GB pull can take a while; give it 6h headroom
)
def download_weights():
    """Stage the FP8 repo into the Volume once. Idempotent: re-runs resume the cache."""
    from huggingface_hub import snapshot_download

    print(f"downloading {MODEL_NAME} into {MODELS_DIR} (HF cache) ...")
    path = snapshot_download(MODEL_NAME)
    weights_volume.commit()
    print(f"done. snapshot at: {path}")
    # quick sanity: list the safetensors shard count
    n = sum(len(files) for _, _, files in os.walk(path))
    print(f"files in snapshot: {n}")


@app.function(image=vllm_image, cpu=2.0, timeout=900)
def check_vllm():
    """CPU preflight: verify the nightly upgrade landed AND the GLM tool parser is patched
    for the responses FunctionTool path — before paying for the 8xH200 boot."""
    import importlib
    import inspect

    import vllm

    print("VLLM_VERSION:", vllm.__version__)
    import importlib.metadata as _md

    for _pkg in ("flashinfer-python", "flashinfer-jit-cache"):
        try:
            print(f"FLASHINFER {_pkg}:", _md.version(_pkg))
        except Exception as _e:  # noqa: BLE001
            print(f"FLASHINFER {_pkg}: <missing> ({_e})")
    mod = None
    for path in (
        "vllm.tool_parsers.glm4_moe_tool_parser",
        "vllm.entrypoints.openai.tool_parsers.glm4_moe_tool_parser",
    ):
        try:
            mod = importlib.import_module(path)
            print("PARSER_MODULE:", path)
            break
        except Exception as e:  # noqa: BLE001
            print("no", path, "->", e)
    if mod:
        src = inspect.getsource(mod)
        i = src.find("_is_string_type")
        print("--- _is_string_type region ---")
        print(src[max(0, i - 40):i + 700] if i >= 0 else "(_is_string_type not found)")
        print("PARSER_BUGGY (tool.function.name present):", "tool.function.name" in src)
        print("PARSER_FIXED (find_tool_properties present):", "find_tool_properties" in src)

    # Does this vLLM have the OpenAI Responses API at all?
    found_responses = False
    for rpath in (
        "vllm.entrypoints.openai.serving_responses",
        "vllm.entrypoints.openai.responses.serving",
        "vllm.entrypoints.openai.responses.api_router",
    ):
        try:
            importlib.import_module(rpath)
            print("RESPONSES_API present via:", rpath)
            found_responses = True
            break
        except Exception:  # noqa: BLE001
            pass
    print("RESPONSES_API_AVAILABLE:", found_responses)


@app.function(
    image=vllm_image,
    gpu=f"H200:{N_GPU}",
    volumes={MODELS_DIR: weights_volume, "/root/.deep_gemm": kernel_cache_volume},
    secrets=[modal.Secret.from_name("glm-vllm-key")],
    timeout=60 * MINUTES,        # cover long agentic eval requests
    min_containers=1,             # TEMP: keep one box warm for testing/Codex/concurrency.
                                  # The ~26-min cold start + 2-min scaledown caused a
                                  # death loop (box reaped right after it became ready).
                                  # Revert to 0 for the scale-to-zero demo once tests pass.
    max_containers=1,             # one 8xH200 box max — never fan out a second during a wave
    scaledown_window=2 * MINUTES, # idle buffer (moot while min_containers=1)
)
@modal.concurrent(max_inputs=32)  # one container soaks up the eval fan-out (vLLM batches)
@modal.web_server(port=VLLM_PORT, startup_timeout=45 * MINUTES)
def serve():
    """Launch vLLM's OpenAI server. Modal proxies the port and health-checks startup."""
    api_key = os.environ["MODAL_VLLM_API_KEY"]
    cmd = (
        f"vllm serve {MODEL_NAME}"
        f" --served-model-name {SERVED_NAME}"
        f" --tensor-parallel-size {N_GPU}"
        f" --tool-call-parser glm47"
        f" --reasoning-parser glm45"
        f" --enable-auto-tool-choice"
        f" --speculative-config '{{\"method\":\"mtp\",\"num_speculative_tokens\":3}}'"
        f" --chat-template-content-format string"
        # Cold-cache torch.compile of a 754B+MTP model with sparse attention either takes
        # 20-30 min or wedges across 8 ranks — too fragile for serverless cold start.
        # Eager skips compile + cudagraph capture: boot = weight-load only (~10 min).
        # Trade: ~10-20% lower throughput. Revisit compiled+VLLM_CACHE_ROOT for eval perf.
        f" --enforce-eager"
        f" --host 0.0.0.0 --port {VLLM_PORT}"
        f" --api-key {api_key}"
    )
    print("starting vLLM:\n" + cmd.replace(api_key, "***"))
    subprocess.Popen(cmd, shell=True)
