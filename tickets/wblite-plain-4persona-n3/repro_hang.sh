#!/usr/bin/env bash
# SEM-39 diagnosis: reproduce the rep-2 codex↔GLM hang SMALL + watched, with capture.
# 6 cases that hung in canary2 rep2, n=1, parallel 2, SHORT agent timeout so a hung
# cell is KILLED at 10min (cell_driver still writes a result → run_matrix pulls the
# codex/adapter trace) instead of hanging to the 45min default. Fresh litellm proxy
# first (tests the "proxy degraded after rep1 load" hypothesis). GPU fenced + auto-stop.
#
# Run: bash tickets/wblite-plain-4persona-n3/repro_hang.sh
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export MODAL_VLLM_API_KEY="${MODAL_GLM_VLLM_API_KEY:-${MODAL_VLLM_API_KEY:-}}"
KEY="$MODAL_VLLM_API_KEY"

export WB_MODAL_GLM=1
export WB_MODAL_BASE=https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1
export WB_MODAL_MODEL=glm-5.1-nvfp4
export WB_LITE_DIR=/Users/marmikpandya/semantic-filesystem/benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en
export WB_E2B_TEMPLATE=semfs-mount-chanpin
export WB_AGENT_TIMEOUT=600 WB_CELL_TIMEOUT=900   # bound hangs; capture artifacts
unset WB_FORCE_OPENROUTER 2>/dev/null || true

VLLM=benchmarks/modal/glm51_nvfp4_vllm.py
LITELLM=benchmarks/modal/glm51_nvfp4_litellm.py
VLLM_MODELS=https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run/v1/models
CASES="15,44,45,53,55,95"
PAR="${PAR:-2}"

stop_gpu(){ modal app stop glm51-nvfp4-vllm --yes 2>&1 | tail -1 || echo "!! stop glm51-nvfp4-vllm MANUALLY"; }
trap stop_gpu EXIT

echo "######## restart litellm proxy (fresh) $(date +%H:%M:%S) ########"
modal app stop glm51-nvfp4-litellm --yes 2>&1 | tail -1 || true

echo "######## deploy vLLM — GPU STARTS $(date +%H:%M:%S) ########"
modal deploy $VLLM 2>&1 | tail -2
echo "waiting for vLLM /v1/models ..."
warm=0
for i in $(seq 1 100); do
  code=$(curl -s -m 15 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $KEY" "$VLLM_MODELS" 2>/dev/null)
  [ "$code" = "200" ] && { warm=1; break; }
  echo "  warming ($i/100) http=$code $(date +%H:%M:%S)"; sleep 30
done
[ "$warm" -ne 1 ] && { echo "!! never warm — stopping"; exit 1; }
echo "GLM vLLM warm $(date +%H:%M:%S)"
modal deploy $LITELLM 2>&1 | tail -1

echo "######## repro: cases=$CASES n=1 parallel=$PAR rep=diag (live, no tail buffer) $(date +%H:%M:%S) ########"
python3 benchmarks/e2b/run_matrix.py --cases "$CASES" \
  --agents codex --arms plain --rep diag --parallel "$PAR" --force \
  || echo "!! run_matrix exited non-zero"

echo "######## stop GPU $(date +%H:%M:%S) ########"
stop_gpu; trap - EXIT
echo "######## REPRO DONE $(date +%H:%M:%S) ########"
