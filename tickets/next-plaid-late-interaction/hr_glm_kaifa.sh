#!/usr/bin/env bash
# headroom × GLM kaifa test — codex(local,API-key) → headroom proxy(litellm-hosted_vllm) → GLM vLLM.
# Stage 1: deploy GLM + smoke 1 cell (verify chain + headroom savings on GLM). Stage 2: full 11 (gated).
# GPU fenced. Run: bash tickets/next-plaid-late-interaction/hr_glm_kaifa.sh smoke|full
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
MODE="${1:-smoke}"
HR=/Users/marmikpandya/.pyenv/versions/3.10.13/bin/headroom
CODEX=/Users/marmikpandya/.superset/bin/codex
WS=/private/tmp/claude-501/-Users-marmikpandya-semantic-filesystem/5dae4701-6852-41e6-b515-9c2e503c84db/scratchpad/hr_kaifa
GLM_VLLM_APP=glm51-nvfp4-vllm
VLLM_BASE=https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run/v1
export MODAL_VLLM_API_KEY="${MODAL_GLM_VLLM_API_KEY:-${MODAL_VLLM_API_KEY:-}}"
KEY="$MODAL_VLLM_API_KEY"

stop_gpu(){ modal app stop $GLM_VLLM_APP --yes 2>&1 | tail -1 || echo "!! STOP $GLM_VLLM_APP MANUALLY"; }
cleanup(){ kill "${HRPID:-0}" 2>/dev/null; stop_gpu; }
trap cleanup EXIT

echo "######## deploy + warm GLM vLLM $(date +%H:%M:%S) ########"
modal deploy benchmarks/modal/glm51_nvfp4_vllm.py 2>&1 | tail -2
warm=0; for i in $(seq 1 60); do
  c=$(curl -s -m 15 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $KEY" "$VLLM_BASE/models" 2>/dev/null)
  [ "$c" = "200" ] && { warm=1; break; }; echo "  warming $i http=$c"; sleep 30; done
[ "$warm" = 1 ] || { echo "!! GLM never warm"; exit 1; }
echo "GLM warm $(date +%H:%M:%S)"

echo "######## start headroom proxy (litellm-hosted_vllm → GLM) ########"
export HOSTED_VLLM_API_BASE="$VLLM_BASE" HOSTED_VLLM_API_KEY="$KEY"
$HR proxy --backend litellm-hosted_vllm --port 8799 > /tmp/hr_glm_proxy.log 2>&1 &
HRPID=$!
for i in $(seq 1 20); do curl -s -m 3 -o /dev/null http://127.0.0.1:8799/v1/models 2>/dev/null && break; sleep 1; done
echo "headroom proxy pid=$HRPID; doctor:"; $HR doctor 2>&1 | tail -3

# codex in API-KEY mode (NOT ChatGPT) → headroom; mirror cell_driver GLM setup
unset CODEX_USE_CHATGPT
export OPENAI_BASE_URL=http://127.0.0.1:8799/v1 OPENAI_API_KEY="$KEY"
run_case(){ # $1=task
  cd "$WS/corpus"; mkdir -p model_output
  timeout 700 "$CODEX" exec --skip-git-repo-check -c model=hosted_vllm/glm-5.1-nvfp4 \
    -c model_provider=openai "$1" 2>&1 | tail -15
}
TASK226='Based on the files that have not been debugged, identify the bugs in the code and generate a bug report named bug_report.txt under ./model_output/bug_report.txt and print the path.'

echo "######## STAGE: $MODE — codex→headroom→GLM (kaifa) $(date +%H:%M:%S) ########"
run_case "$TASK226"
echo "######## headroom savings (GLM) ########"
$HR savings 2>&1 | tail -8; $HR perf 2>&1 | grep -iE "reduction|saved|hit rate|cache" | head -6
echo "######## stop GPU $(date +%H:%M:%S) ########"
