"""LiteLLM proxy: gives both Codex and Claude Code a working backend on GLM-5.1-NVFP4.

Same pattern as glm51_litellm.py but points at the NVFP4 (4×B200) endpoint.

  Codex:       --/v1/responses--> LiteLLM --/v1/chat/completions--> vLLM (4×B200 NVFP4)
  Claude Code: --/v1/messages--> LiteLLM --/v1/chat/completions--> vLLM (4×B200 NVFP4)

vLLM's own /v1/messages rejects the system role that Claude Code injects into the messages
array. LiteLLM handles the Anthropic→chat translation cleanly. Model aliases for Claude
model IDs let ANTHROPIC_DEFAULT_*_MODEL env vars route without model-name overrides.
"""

import subprocess

import modal

APP_NAME  = "glm51-nvfp4-litellm"
VLLM_BASE = "https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run/v1"
PORT      = 4000
MINUTES   = 60

# The name vLLM serves under (its --served-model-name). LiteLLM forwards THIS upstream or vLLM 404s.
VLLM_SERVED_NAME = "glm-5.1-nvfp4"

# LiteLLM's default request timeout is 120s — too short for GLM's long reasoning generations
# under 20-way parallel load (a single agentic turn can run minutes). Raise both the regular
# and streaming (Claude Code streams /v1/messages) timeouts so long turns aren't cut off.
REQUEST_TIMEOUT_S = 1800   # 30 min

app = modal.App(APP_NAME)

_VLLM_MODEL_PARAMS = f"""
      model: openai/{VLLM_SERVED_NAME}
      api_base: {VLLM_BASE}
      api_key: os.environ/MODAL_VLLM_API_KEY
      use_chat_completions_api: true
      timeout: {REQUEST_TIMEOUT_S}
      stream_timeout: {REQUEST_TIMEOUT_S}"""

# Claude model ID aliases: Claude Code sends the model name from $ANTHROPIC_DEFAULT_*_MODEL;
# each alias here routes to the served vLLM model via chat/completions translation.
_CLAUDE_ALIASES = [
    "claude-opus-4-8",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
]

CONFIG_YAML = "model_list:\n"
for _name in ["glm-5.1-nvfp4"] + _CLAUDE_ALIASES:
    CONFIG_YAML += f"  - model_name: {_name}\n    litellm_params:{_VLLM_MODEL_PARAMS}\n"
CONFIG_YAML += f"""litellm_settings:
  drop_params: true
  request_timeout: {REQUEST_TIMEOUT_S}
general_settings:
  master_key: os.environ/MODAL_VLLM_API_KEY
"""

litellm_image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("litellm[proxy]")
)


@app.function(
    image=litellm_image,
    secrets=[modal.Secret.from_name("glm-vllm-key")],
    min_containers=0,
    max_containers=4,
    scaledown_window=5 * MINUTES,
    timeout=60 * MINUTES,
)
@modal.concurrent(max_inputs=100)
@modal.web_server(port=PORT, startup_timeout=5 * MINUTES)
def serve():
    with open("/tmp/litellm_config.yaml", "w") as f:
        f.write(CONFIG_YAML)
    cmd = f"litellm --config /tmp/litellm_config.yaml --host 0.0.0.0 --port {PORT}"
    print("starting litellm proxy (NVFP4):\n" + cmd)
    print("--- config ---\n" + CONFIG_YAML)
    subprocess.Popen(cmd, shell=True)
