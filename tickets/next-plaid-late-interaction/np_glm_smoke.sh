#!/usr/bin/env bash
# next_plaid GLM-5.1 smoke — houqin-A, 1 cell. Deploys GLM vLLM (4xB200) + litellm,
# runs next_plaid_houqin_A case 267 via the np-houqin-all template (colgrep underneath
# semfs grep), judges, auto-stops the GPU. Times one cell → extrapolate the full test.
# Run: WB_SKIP_GLM_DEPLOY=1 bash tickets/next-plaid-late-interaction/np_glm_smoke.sh  (if GLM warm)
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a

MODAL=modal
GLM_VLLM=benchmarks/modal/glm51_nvfp4_vllm.py
GLM_LITELLM=benchmarks/modal/glm51_nvfp4_litellm.py
GLM_VLLM_APP=glm51-nvfp4-vllm
GLM_LITELLM_APP=glm51-nvfp4-litellm
VLLM_MODELS=https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run/v1/models
export MODAL_VLLM_API_KEY="${MODAL_GLM_VLLM_API_KEY:-${MODAL_VLLM_API_KEY:-}}"
KEY="$MODAL_VLLM_API_KEY"

# GLM path (NOT OpenRouter): codex → litellm proxy → vLLM
export WB_MODAL_GLM=1
export WB_MODAL_BASE=https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1
export WB_MODAL_MODEL=glm-5.1-nvfp4
unset WB_FORCE_OPENROUTER WB_OR_MODEL 2>/dev/null || true

export WB_E2B_TEMPLATE=np-houqin-all WB_PERSONA=houqin
export WB_OUT="$PWD/tickets/next-plaid-late-interaction/artifacts/smoke_glm"
export WB_AGENT_TIMEOUT="${WB_AGENT_TIMEOUT:-2000}" WB_CELL_TIMEOUT="${WB_CELL_TIMEOUT:-2300}"
export WB_INLINE_JUDGE=1
rm -rf "$WB_OUT"; mkdir -p "$WB_OUT"
# stage rubrics for the inline judge
WB_LITE_SRC="$PWD/benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en"
[ -d "$WB_LITE_SRC" ] && { mkdir -p /tmp/wb_lite && rm -rf /tmp/wb_lite/task_lite_clean_en; cp -r "$WB_LITE_SRC" /tmp/wb_lite/task_lite_clean_en; }

stop_gpu(){ $MODAL app stop $GLM_VLLM_APP --yes 2>&1 || echo "!! STOP $GLM_VLLM_APP MANUALLY"; }
trap stop_gpu EXIT

echo "######## GLM-5.1-NVFP4 vLLM $(date +%H:%M:%S) ########"
if [ "${WB_SKIP_GLM_DEPLOY:-0}" = "1" ]; then echo "skip deploy (warm-check only)"; else $MODAL deploy $GLM_VLLM 2>&1 | tail -3; fi
warm=0
for i in $(seq 1 100); do
  code=$(curl -s -m 15 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $KEY" "$VLLM_MODELS" 2>/dev/null)
  [ "$code" = "200" ] && { warm=1; break; }
  echo "  warming ($i/100) http=$code $(date +%H:%M:%S)"; sleep 30
done
[ "$warm" -ne 1 ] && { echo "!! GLM never warm"; exit 1; }
echo "GLM warm $(date +%H:%M:%S)"
$MODAL deploy $GLM_LITELLM 2>&1 | tail -2

echo "=== [SMOKE] next_plaid_houqin_A case 267 (GLM-5.1) $(date +%H:%M:%S) ==="
SECONDS=0
python3 benchmarks/e2b/run_matrix.py --cases 267 --agents codex \
  --arms next_plaid_houqin_A --reps 1 --parallel 1 2>&1 || echo "!! run_matrix FAILED"
echo "=== cell wall: ${SECONDS}s $(date +%H:%M:%S) ==="

echo "######## stop GPU $(date +%H:%M:%S) ########"; stop_gpu; trap - EXIT
echo "######## NP GLM SMOKE DONE $(date +%H:%M:%S) ########"
