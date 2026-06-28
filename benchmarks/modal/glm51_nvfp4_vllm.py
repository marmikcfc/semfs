"""GLM-5.1-NVFP4 (NVIDIA quantized) on Modal via vLLM — Codex-compatible eval backend.

Why this exists
---------------
nvidia/GLM-5.1-NVFP4 is NVIDIA Model Optimizer's FP4 quantization of ZhipuAI's GLM-5.1.
- 466 GB (vs 860 GB for FP8) — nearly half the weight footprint.
- Requires Blackwell GPUs (B200/B300) for NVFP4 tensor cores.
- Same LiteLLM responses→chat proxy as the FP8 path (vLLM /v1/responses still broken).

Shape
-----
  download_weights()  CPU function — pulls 466 GB into a Modal Volume once.
  serve()             4×B200 web server — vllm serve, TP=4 (HBM3e bandwidth wins for 20 parallel tests).

Cost vs FP8
-----------
  8×H200 (FP8):          8 × $4.54 = $36.32/hr
  8×RTX-PRO-6000 (NVFP4):8 × $3.03 = $24.24/hr  ← cheaper per-hour, but GDDR7 bandwidth hurts at 20 parallel
  4×B200 (NVFP4):        4 × $6.25 = $25.00/hr  ← chosen: HBM3e ~2.2× bandwidth, better cost-per-task at scale

Usage
-----
  modal run  benchmarks/modal/glm51_nvfp4_vllm.py::download_weights  # stage weights (once)
  modal deploy benchmarks/modal/glm51_nvfp4_vllm.py                  # register endpoint
See benchmarks/modal/GLM51_RUNBOOK.md for the LiteLLM proxy + Codex wiring (unchanged).
"""

import os
import subprocess

import modal

# --- identity / pins -------------------------------------------------------
APP_NAME        = "glm51-nvfp4-vllm"
MODEL_NAME      = "nvidia/GLM-5.1-NVFP4"
SERVED_NAME     = "glm-5.1-nvfp4"
# vLLM serves ONE clean name. Claude model ID aliases (claude-opus-4-8, etc.) live in the
# LiteLLM proxy, which maps them to glm-5.1-nvfp4 upstream — Claude Code does NOT validate
# model names client-side (earlier assumption was wrong), so vLLM needs no aliases.
# NVIDIA's model card recommends v0.19.1 — same generation as the FP8 glm51 image.
# No explicit --quantization flag: vLLM auto-detects NVFP4 from hf_quant_config.json.
VLLM_IMAGE_TAG  = "v0.19.1"
MODELS_DIR      = "/models"
N_GPU           = 4          # TP=4: 64 heads / 4 = 16 per GPU ✓, 256 experts / 4 = 64 per GPU ✓
GPU_TYPE        = "B200"     # 4×B200 = 768GB HBM3e; ~2.2× bandwidth vs 8×RTX-PRO-6000 GDDR7
VLLM_PORT       = 8000

MINUTES = 60

app = modal.App(APP_NAME)

weights_volume = modal.Volume.from_name("glm51-nvfp4-weights", create_if_missing=True)

# NVFP4 on Blackwell uses FlashInfer b12x MoE kernels + CUTLASS FP4 kernels that JIT-compile
# on first inference and cache under VLLM_CACHE_ROOT. Persisting this volume eliminates the
# kernel compilation warmup on subsequent cold starts (same trick as DeepGEMM cache for FP8).
kernel_cache_volume = modal.Volume.from_name("glm51-nvfp4-kernels", create_if_missing=True)
KERNEL_CACHE_DIR = "/root/.cache/vllm"
vllm_image = (
    modal.Image.from_registry(
        f"vllm/vllm-openai:{VLLM_IMAGE_TAG}",
        # Same python3→python symlink fix as the FP8 image (vLLM ships python3 only).
        setup_dockerfile_commands=["RUN ln -sf $(which python3) /usr/local/bin/python"],
    )
    .pip_install("huggingface_hub[hf_transfer]")
    # Same typing_extensions pin: Modal's injected deps can downgrade it below pydantic_core's
    # minimum, breaking vLLM import. Re-pin last, surgical (--no-deps).
    .run_commands(
        "python -m pip install --no-deps --force-reinstall typing_extensions==4.15.0"
    )
    .env(
        {
            "HF_HOME": MODELS_DIR,
            "HF_HUB_ENABLE_HF_TRANSFER": "1",
            "VLLM_CACHE_ROOT": KERNEL_CACHE_DIR,  # FlashInfer b12x + CUTLASS FP4 JIT cache
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
    timeout=6 * 60 * MINUTES,
)
def download_weights():
    """Stage nvidia/GLM-5.1-NVFP4 (466 GB) into the Volume. Idempotent."""
    from huggingface_hub import snapshot_download

    print(f"downloading {MODEL_NAME} into {MODELS_DIR} ...")
    path = snapshot_download(MODEL_NAME)
    weights_volume.commit()
    print(f"done. snapshot at: {path}")
    n = sum(len(files) for _, _, files in os.walk(path))
    print(f"files in snapshot: {n}")


@app.function(
    image=vllm_image,
    gpu=f"{GPU_TYPE}:{N_GPU}",
    volumes={MODELS_DIR: weights_volume, KERNEL_CACHE_DIR: kernel_cache_volume},
    secrets=[modal.Secret.from_name("glm-vllm-key")],
    timeout=60 * MINUTES,
    min_containers=1,          # keep warm during eval runs — 20 parallel tests need zero cold starts
    max_containers=1,
    scaledown_window=10 * MINUTES,
)
@modal.concurrent(max_inputs=64)
@modal.web_server(port=VLLM_PORT, startup_timeout=45 * MINUTES)
def serve():
    """Launch vLLM with NVFP4 on 4×B200 (TP=4). Advanced flags per NVIDIA model card."""
    api_key = os.environ["MODAL_VLLM_API_KEY"]
    cmd = (
        f"vllm serve {MODEL_NAME}"
        f" --served-model-name {SERVED_NAME}"
        f" --tensor-parallel-size {N_GPU}"
        f" --pipeline-parallel-size 1"
        f" --data-parallel-size 1"
        f" --enable-expert-parallel"
        f" --trust-remote-code"
        f" --gpu-memory-utilization 0.9"
        f" --reasoning-parser glm45"
        f" --tool-call-parser glm47"
        f" --enable-auto-tool-choice"
        f" --enable-chunked-prefill"
        f" --max-num-batched-tokens 8192"
        f" --max-num-seqs 1024"
        f" --model-loader-extra-config '{{\"enable_multithread_load\": true, \"num_threads\": 128}}'"
        f" --chat-template-content-format string"
        f" -cc.pass_config.fuse_allreduce_rms=False"
        # f" --enforce-eager"     # REMOVED 06-19: enable CUDA graphs + torch.compile for faster decode
                                  # on the long over-explorer cells. REVERT (re-add) if the NVFP4 boot
                                  # hangs or errors on torch.compile (the reason the FP8 path kept it).
        f" --host 0.0.0.0 --port {VLLM_PORT}"
        f" --api-key {api_key}"
    )
    print("starting vLLM (NVFP4):\n" + cmd.replace(api_key, "***"))
    subprocess.Popen(cmd, shell=True)
