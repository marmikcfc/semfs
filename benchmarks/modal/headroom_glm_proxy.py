"""Headroom compression proxy in front of GLM-5.1-NVFP4 vLLM.

  Codex --/v1/responses--> [chat-adapter] --/v1/chat/completions--> Headroom (compress)
        --litellm-hosted_vllm--> vLLM (4xB200 NVFP4)

Exposes an OpenAI-compatible /v1 endpoint that compresses the prompt (Kompress + code-aware),
then forwards to GLM. The `headroom` arm points codex's provider base_url here (WB_HEADROOM=1)
instead of the GLM litellm endpoint — so it's the SAME GLM model, with headroom compression
inserted. Headroom-on vs off = clean A/B on token usage.

Deploy: modal deploy benchmarks/modal/headroom_glm_proxy.py
Endpoint: https://ada-diffusion-llm--headroom-glm-proxy-serve.modal.run/v1
"""
import modal

APP_NAME  = "headroom-glm-proxy"
VLLM_BASE = "https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run/v1"
PORT      = 8788
MINUTES   = 60

app = modal.App(APP_NAME)

hr_image = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("curl", "git")
    .pip_install("headroom-ai[all]")
    # warm the Kompress model into the image so the first request doesn't stall
    .run_commands("python -c 'import headroom' || true")
)


@app.function(
    image=hr_image,
    secrets=[modal.Secret.from_name("glm-vllm-key")],   # provides MODAL_VLLM_API_KEY
    timeout=60 * MINUTES,
    cpu=4,
    memory=16384,
    scaledown_window=20 * MINUTES,
)
@modal.web_server(port=PORT, startup_timeout=10 * MINUTES)
def serve():
    import os, subprocess
    key = os.environ.get("MODAL_VLLM_API_KEY", "")
    # litellm's hosted_vllm provider reads these for the upstream:
    os.environ["HOSTED_VLLM_API_BASE"] = VLLM_BASE
    os.environ["HOSTED_VLLM_API_KEY"] = key
    os.environ["OPENAI_API_KEY"] = key
    # --mode token + an explicit aggressive --target-ratio: the DEFAULT (unset ratio) is
    # Kompress-conservative → 0% on our GLM (cache not surfaced to billing, so freezing the
    # prefix buys nothing). Force real compression. Env-overridable to sweep the ratio.
    ratio = os.environ.get("HR_TARGET_RATIO", "0.5")
    cmd = (f"headroom proxy --backend litellm-hosted_vllm --mode token --target-ratio {ratio} "
           f"--host 0.0.0.0 --port {PORT} --no-telemetry")
    print("[headroom-glm] launching:", cmd, "→ upstream", VLLM_BASE, flush=True)
    subprocess.Popen(cmd, shell=True)
