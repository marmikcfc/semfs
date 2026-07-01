# GLM-5.1 on Modal — Codex + Claude Code eval backend (runbook)

**What this is:** GLM-5.1 (754B MoE) served on Modal via vLLM, made **Codex and Claude Code-compatible**
by a thin LiteLLM proxy. Verified end-to-end: Codex and Claude Code complete agentic tool-use tasks.

Two variants are running:
- **FP8** (8×H200, `$36.32/hr`) — `glm51-vllm` + `glm51-litellm`
- **NVFP4** (4×B200, `$25.00/hr`) — `glm51-nvfp4-vllm` + `glm51-nvfp4-litellm` ← **recommended for benchmarks**

**Why the proxy exists (the one big lesson):** Codex 0.139 only speaks the OpenAI **Responses
API** (`/v1/responses`). vLLM's `/v1/responses` **crashes** on GLM-5.1 tool calls (parser bug on
stable, 500s on nightly) — but vLLM's `/v1/chat/completions` works perfectly. So we don't fix
vLLM; we put **LiteLLM** in front to translate `responses → chat`.

```
  Codex (--disable multi_agent, wire_api=responses)
     │  /v1/responses
     ▼
  LiteLLM proxy   (Modal app: glm51-litellm, CPU, scale-to-zero)   use_chat_completions_api: true
     │  /v1/chat/completions
     ▼
  vLLM GLM-5.1    (Modal app: glm51-vllm, 8×H200, stable glm51 image)
```

---

## Quick start

```bash
# 0. one-time: secret + weights (already done — here for reproduction)
#    modal secret create glm-vllm-key MODAL_VLLM_API_KEY=sk-glm51-...
#    modal run benchmarks/modal/glm51_vllm.py::download_weights   # ~800GB into Volume, once

# 1. deploy the GPU model server (cold start ~30–43 min: weight load + DeepGEMM warmup)
modal deploy benchmarks/modal/glm51_vllm.py

# 2. wait until it serves, then deploy the LiteLLM proxy (seconds)
KEY=$(cat /tmp/glm_vllm_key.txt)   # = the glm-vllm-key secret value
curl -s -H "Authorization: Bearer $KEY" \
  https://ada-diffusion-llm--glm51-vllm-serve.modal.run/v1/models   # wait for HTTP 200
modal deploy benchmarks/modal/glm51_litellm.py

# 3. point Codex at the PROXY and run with multi_agent disabled
mkdir -p ~/.codex-glm && cp benchmarks/modal/codex_glm51_config.toml ~/.codex-glm/config.toml
export MODAL_VLLM_API_KEY="$KEY"
CODEX_HOME=~/.codex-glm codex exec --disable multi_agent "Create hello.txt containing: hi"
```

## The two non-obvious requirements

1. **Codex must point at LiteLLM, not vLLM** — `base_url = <glm51-litellm>/v1`, `wire_api="responses"`.
   LiteLLM's `use_chat_completions_api: true` bridges responses→chat (skips vLLM's broken `/responses`).
2. **`codex exec --disable multi_agent`** — Codex 0.139 sends a `type:"namespace"` tool
   (`multi_agent_v1`, for sub-agents) that vLLM's chat schema rejects with **400** (`Input should be
   'function'`). `drop_params` can't strip it (it's a malformed tool entry). Disable the feature.

---

## Components

| Thing | Where | Notes |
|---|---|---|
| GPU model server | Modal app `glm51-vllm` (`glm51_vllm.py`) | 8×H200, stable `vllm/vllm-openai:glm51` image, `--enforce-eager`, chat path |
| Responses→chat proxy | Modal app `glm51-litellm` (`glm51_litellm.py`) | CPU, scale-to-zero, LiteLLM, `use_chat_completions_api: true`, `drop_params: true` |
| Weights | Modal Volume `glm51-weights` | ~800GB FP8, staged once; image rebuilds never touch it |
| Secret | `glm-vllm-key` → `MODAL_VLLM_API_KEY` | shared: Codex→LiteLLM auth AND LiteLLM→vLLM auth |
| Codex config | `codex_glm51_config.toml` | base_url=LiteLLM, wire_api=responses |
| Live status | `inference_state.json` | source-of-truth `status` + endpoints (can go stale — verify live) |

**Endpoints (workspace `ada-diffusion-llm`):**
- vLLM:    `https://ada-diffusion-llm--glm51-vllm-serve.modal.run`  (`/v1/chat/completions`)
- LiteLLM: `https://ada-diffusion-llm--glm51-litellm-serve.modal.run`  (`/v1/responses` for Codex)

---

## Operate

```bash
# is it up?
curl -s -H "Authorization: Bearer $KEY" .../glm51-vllm-serve.modal.run/v1/models   # 200 = serving
modal container list | grep glm51-vllm                                             # running = warm

# logs
modal app logs glm51-vllm        # GPU / model
modal app logs glm51-litellm     # proxy

# STOP the GPU (halt the ~$36/hr burn) — proxy is cheap, can stay
modal app stop glm51-vllm --yes
#   then set inference_state.json "status": "stopped"
```

### Cost / scaling knobs (`glm51_vllm.py`, `serve()`)
| knob | current | meaning |
|---|---|---|
| `min_containers` | `1` (testing) | always-warm. **Set 0** for scale-to-zero (no cost when idle). |
| `max_containers` | `1` | hard cap — high RPS queues, never spins a 2nd 8×H200. Your cost guardrail. |
| `@modal.concurrent(max_inputs=32)` | `32` | one box soaks the eval fan-out (vLLM batches). |
| `scaledown_window` | `120` | idle buffer before teardown. Raise to ~`1200` for eval waves; min_containers=1 ignores it. |

**Boot time:** ~30–43 min cold (variable — weight load 8–27 min off the Volume + ~13 min DeepGEMM
warmup). It's weight-*load*, not re-download (the 84-min HF download happened once). Cold start
exceeds Modal's ~150s request hold, so **poll for readiness, don't rely on one blocking request.**

---

## Gotchas (all fixed in the code — here so you don't re-discover them)

- **Image build `python: not found`** → `vllm/vllm-openai` ships `python3`; fixed with a
  `setup_dockerfile_commands` symlink (runs before Modal's setup).
- **`typing_extensions` import crash** → Modal's injected client deps downgrade it below what
  pydantic_core needs; re-pin `==4.15.0` as the last build layer.
- **Scaledown death loop** → at `scaledown_window=2m`, the box was reaped right after a ~30-min
  boot before it served. `min_containers=1` (or a long scaledown) avoids it.
- **Don't co-locate LiteLLM in the GPU container** → Modal would mark it "ready" when LiteLLM
  binds (instant) instead of when vLLM loads (~30 min). Keep them as separate apps.
- **vLLM nightly (0.23) fixed the parser but added flashinfer ABI breaks + responses 500s** — a
  dead end. The stable image + LiteLLM proxy is the working path. (Don't chase vLLM versions.)

---

## NVFP4 variant (4×B200, $25/hr) — recommended for benchmarks

```
  Codex / Claude Code
     │  /v1/responses (Codex) or /v1/messages (Claude Code)
     ▼
  LiteLLM proxy   (Modal: glm51-nvfp4-litellm, CPU, scale-to-zero)   use_chat_completions_api: true
     │  /v1/chat/completions
     ▼
  vLLM NVFP4      (Modal: glm51-nvfp4-vllm, 4×B200, vllm-openai:v0.19.1)
```

### Quick start (NVFP4)

```bash
# 1. deploy (weights already staged in Volume glm51-nvfp4-weights)
modal deploy benchmarks/modal/glm51_nvfp4_vllm.py

# 2. wait for ready (cold start ~15-20 min on Blackwell — faster than FP8)
KEY=$(cat /tmp/glm_vllm_key.txt)
until curl -sf -H "Authorization: Bearer $KEY" \
  https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run/health > /dev/null; do
  echo "not ready..."; sleep 60; done

# 3. deploy the LiteLLM proxy
modal deploy benchmarks/modal/glm51_nvfp4_litellm.py

# 4a. Claude Code — point at LiteLLM proxy (NOT vLLM directly; vLLM /v1/messages is buggy)
ANTHROPIC_BASE_URL=https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run \
ANTHROPIC_API_KEY=$KEY \
ANTHROPIC_DEFAULT_SONNET_MODEL=claude-sonnet-4-6 \
claude -p "Hello"

# 4b. Codex — same proxy, same key, disable multi_agent
CODEX_HOME=~/.codex-glm codex exec --disable multi_agent "Create hello.txt"
```

**Claude Code gotcha: ANTHROPIC_BASE_URL must NOT end with `/v1`** — Claude Code appends `/v1/messages`
itself. Setting `ANTHROPIC_BASE_URL=.../v1` causes double-pathing `/v1/v1/messages` → 404.

**Why not use vLLM's /v1/messages directly?** vLLM's Anthropic messages endpoint rejects the system
role that Claude Code injects into the messages array
(`400: Input should be 'user' or 'assistant', input: 'system'`). LiteLLM handles this cleanly.

### NVFP4 Endpoints
- vLLM:    `https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run`
- LiteLLM: `https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run`

---

## Files
- `glm51_vllm.py` — FP8 GPU model server (8×H200)
- `glm51_litellm.py` — FP8 LiteLLM proxy (Codex)
- `glm51_nvfp4_vllm.py` — NVFP4 GPU model server (4×B200)
- `glm51_nvfp4_litellm.py` — NVFP4 LiteLLM proxy (Codex + Claude Code)
- `codex_glm51_config.toml` — Codex provider config (points at FP8 LiteLLM)
- `inference_state.json` — is-it-up source of truth
- design doc: `docs/superpowers/specs/2026-06-17-glm51-fp8-modal-vllm-codex-design.md`
- Linear: SEM-36
