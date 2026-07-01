"""Gemma-4-31B-IT-NVFP4 (NVIDIA quantized) on Modal via vLLM — KG-extraction backend.

Why this exists
---------------
nvidia/Gemma-4-31B-IT-NVFP4 is NVIDIA Model Optimizer's FP4 quantization of Google's
Gemma-4-31B-IT instruct model. For semfs KG extraction it's the right tool: an INSTRUCT
model (answers directly, no reasoning-token overhead like GLM-5.1), 140+ languages,
function-calling/structured-output native, ~21 GB post-quant → fits 1×B200 with room.

Unlike the GLM path, build_kg talks plain OpenAI /v1/chat/completions, which vLLM serves
natively — so NO LiteLLM proxy is needed. Point SEMFS_GRAPH_LLM_BASE_URL at this serve URL:
  https://<workspace>--gemma4-31b-nvfp4-vllm-serve.modal.run/v1   (model: gemma-4-31b-nvfp4)

Shape
-----
  download_weights()  CPU function — pulls the NVFP4 snapshot into a Modal Volume once.
  serve()             1×B200 web server — vllm serve, TP=1 (31B NVFP4 fits one GPU).

Usage
-----
  modal run    benchmarks/modal/gemma4_31b_nvfp4_vllm.py::download_weights   # stage weights (once)
  modal deploy benchmarks/modal/gemma4_31b_nvfp4_vllm.py                     # register endpoint
"""

import os
import subprocess

import modal

# --- identity / pins -------------------------------------------------------
APP_NAME    = "gemma4-31b-nvfp4-vllm"
MODEL_NAME  = "nvidia/Gemma-4-31B-IT-NVFP4"
SERVED_NAME = "gemma-4-31b-nvfp4"
# NVIDIA's card tested vLLM 0.17.2rc1.dev (2026-03-19); v0.19.1 (the GLM image, newer)
# carries Gemma-4 + ModelOpt-NVFP4 support. Bump if vLLM rejects the architecture.
VLLM_IMAGE_TAG = "v0.19.1"
MODELS_DIR  = "/models"
N_GPU       = 1          # 30.7B @ NVFP4 ≈ 15 GB weights → 1×B200 (180 GB) fits with huge KV headroom
N_REPLICAS  = int(os.environ.get("GEMMA_REPLICAS", "4"))   # smoke/compression: GEMMA_REPLICAS=1
GPU_TYPE    = os.environ.get("GEMMA_GPU", "B200")          # compression run: GEMMA_GPU=RTX-PRO-6000
VLLM_PORT   = 8000

MINUTES = 60

app = modal.App(APP_NAME)

weights_volume = modal.Volume.from_name("gemma4-31b-nvfp4-weights", create_if_missing=True)
# NVFP4 Blackwell kernels (FlashInfer/CUTLASS FP4) JIT-compile on first inference; persist
# the cache so subsequent cold starts skip the warmup.
kernel_cache_volume = modal.Volume.from_name("gemma4-31b-nvfp4-kernels", create_if_missing=True)
KERNEL_CACHE_DIR = "/root/.cache/vllm"

vllm_image = (
    modal.Image.from_registry(
        f"vllm/vllm-openai:{VLLM_IMAGE_TAG}",
        setup_dockerfile_commands=["RUN ln -sf $(which python3) /usr/local/bin/python"],
    )
    .pip_install("huggingface_hub[hf_transfer]")
    .run_commands(
        "python -m pip install --no-deps --force-reinstall typing_extensions==4.15.0"
    )
    .env(
        {
            "HF_HOME": MODELS_DIR,
            "HF_HUB_ENABLE_HF_TRANSFER": "1",
            "VLLM_CACHE_ROOT": KERNEL_CACHE_DIR,
        }
    )
    .entrypoint([])
)


@app.function(
    image=vllm_image,
    volumes={MODELS_DIR: weights_volume},
    secrets=[modal.Secret.from_name("hf-token")],
    cpu=8.0,
    memory=32768,
    timeout=4 * 60 * MINUTES,
)
def download_weights():
    """Stage nvidia/Gemma-4-31B-IT-NVFP4 into the Volume. Idempotent."""
    from huggingface_hub import snapshot_download

    print(f"downloading {MODEL_NAME} into {MODELS_DIR} ...", flush=True)
    path = snapshot_download(MODEL_NAME)
    weights_volume.commit()
    n = sum(len(files) for _, _, files in os.walk(path))
    print(f"done. snapshot at: {path} ({n} files)", flush=True)


@app.function(
    image=vllm_image,
    gpu=f"{GPU_TYPE}:{N_GPU}",
    volumes={MODELS_DIR: weights_volume, KERNEL_CACHE_DIR: kernel_cache_volume},
    secrets=[modal.Secret.from_name("glm-vllm-key")],  # reuse MODAL_VLLM_API_KEY
    timeout=60 * MINUTES,
    min_containers=int(os.environ.get("GEMMA_MIN", "0")),   # 0 → self-scale to 0 when idle (frees GPU if a run is killed)
    max_containers=N_REPLICAS,                              # autoscale up to N under the batch's request load
    scaledown_window=int(os.environ.get("GEMMA_SCALEDOWN_MIN", "5")) * MINUTES,
)
@modal.concurrent(max_inputs=64)  # per replica; Modal load-balances across the N_REPLICAS
@modal.web_server(port=VLLM_PORT, startup_timeout=45 * MINUTES)
def serve():
    """Launch vLLM with Gemma-4-31B-IT-NVFP4 on 1×B200 (TP=1), data-parallel across replicas.

    CUDA graphs ON (no --enforce-eager): ~2-3× decode throughput vs eager, at the cost of a
    one-time graph-capture (~minutes) at boot. The NVFP4-path-eager habit was a startup
    optimization, not a correctness need — for a multi-hour batch the throughput wins."""
    api_key = os.environ["MODAL_VLLM_API_KEY"]
    cmd = (
        f"vllm serve {MODEL_NAME}"
        f" --served-model-name {SERVED_NAME}"
        f" --quantization modelopt"            # NVFP4 via NVIDIA ModelOpt (per model card)
        f" --tensor-parallel-size {N_GPU}"
        f" --trust-remote-code"
        f" --gpu-memory-utilization 0.9"
        f" --max-model-len 16384"              # KG docs are ≤6000 chars; no need for 256K context
        f" --enable-chunked-prefill"
        f" --max-num-batched-tokens 8192"
        f" --max-num-seqs 64"
        # APC is on by default (V1); this flag makes the server REPORT cache hits as
        # usage.prompt_tokens_details.cached_tokens so token accounting can see them.
        f" --enable-prompt-tokens-details"
        f" --host 0.0.0.0 --port {VLLM_PORT}"
        f" --api-key {api_key}"
    )
    print("starting vLLM (Gemma-4-31B-NVFP4):\n" + cmd.replace(api_key, "***"), flush=True)
    subprocess.Popen(cmd, shell=True)
