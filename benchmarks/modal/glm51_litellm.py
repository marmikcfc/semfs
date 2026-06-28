"""LiteLLM proxy: gives Codex a working /v1/responses backed by the GLM-5.1 vLLM endpoint.

Why
---
Codex 0.139 ONLY speaks the OpenAI Responses API (/v1/responses). vLLM's /v1/responses
crashes on GLM-5.1 tool calls (glm4_moe_tool_parser FunctionTool bug on stable; 500s on
nightly), but vLLM's /v1/chat/completions + GLM tool calls work fine. LiteLLM bridges:

    Codex (E2B) --/v1/responses--> LiteLLM (here, CPU) --/v1/chat/completions--> vLLM (8xH200)

`use_chat_completions_api: true` is the critical flag — it forces LiteLLM to TRANSLATE the
responses request to chat-completions, instead of forwarding it to vLLM's broken /responses.

Runs on a cheap CPU container, SEPARATE from the GPU app so vLLM's readiness/lifecycle stays
clean (Modal "port open == model ready" only holds if vLLM is its own web endpoint).

Usage
-----
  modal deploy benchmarks/modal/glm51_litellm.py
  # endpoint:  https://<workspace>--glm51-litellm-serve.modal.run
  # Codex config: base_url = "<that>/v1", wire_api = "responses", env_key sends MODAL_VLLM_API_KEY
"""

import subprocess

import modal

APP_NAME = "glm51-litellm"
# The GPU app's OpenAI endpoint (chat completions live here).
VLLM_BASE = "https://ada-diffusion-llm--glm51-vllm-serve.modal.run/v1"
PORT = 4000
MINUTES = 60

app = modal.App(APP_NAME)

# `use_chat_completions_api: true` => LiteLLM translates /v1/responses -> /v1/chat/completions.
# Same key (MODAL_VLLM_API_KEY) is the proxy master_key AND the vLLM backend key, so Codex
# authenticates to LiteLLM and LiteLLM authenticates to vLLM with one shared secret.
CONFIG_YAML = f"""model_list:
  - model_name: glm-5.1
    litellm_params:
      model: openai/glm-5.1
      api_base: {VLLM_BASE}
      api_key: os.environ/MODAL_VLLM_API_KEY
      use_chat_completions_api: true
litellm_settings:
  drop_params: true
general_settings:
  master_key: os.environ/MODAL_VLLM_API_KEY
"""

litellm_image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("litellm[proxy]")
)


@app.function(
    image=litellm_image,
    secrets=[modal.Secret.from_name("glm-vllm-key")],  # provides MODAL_VLLM_API_KEY
    min_containers=0,             # cheap CPU; cold start is seconds, so scale-to-zero is fine
    max_containers=4,             # headroom to fan out under a parallel eval wave
    scaledown_window=5 * MINUTES,
    timeout=60 * MINUTES,
)
@modal.concurrent(max_inputs=100)  # async proxy soaks many requests per container
@modal.web_server(port=PORT, startup_timeout=5 * MINUTES)
def serve():
    with open("/tmp/litellm_config.yaml", "w") as f:
        f.write(CONFIG_YAML)
    cmd = f"litellm --config /tmp/litellm_config.yaml --host 0.0.0.0 --port {PORT}"
    print("starting litellm proxy:\n" + cmd)
    print("--- config ---\n" + CONFIG_YAML)
    subprocess.Popen(cmd, shell=True)
