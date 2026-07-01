#!/usr/bin/env bash
# headroom ARM × GLM kaifa — run_matrix(kaifa, plain) with WB_HEADROOM on/off.
# Chain: codex → chat-adapter → headroom Modal proxy (compress) → GLM vLLM. GPU fenced.
# Usage: bash hr_arm_glm.sh smoke   (1 cell, WB_HEADROOM=1)
#        bash hr_arm_glm.sh full    (11 cases × on+off)
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
MODE="${1:-smoke}"
GLM_VLLM_APP=glm51-nvfp4-vllm
VLLM_MODELS=https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run/v1/models
export MODAL_VLLM_API_KEY="${MODAL_GLM_VLLM_API_KEY:-${MODAL_VLLM_API_KEY:-}}"
KEY="$MODAL_VLLM_API_KEY"
export HEADROOM_GLM_BASE="https://ada-diffusion-llm--headroom-glm-proxy-serve.modal.run/v1"
export WB_MODAL_GLM=1 WB_MODAL_MODEL=glm-5.1-nvfp4
export WB_E2B_TEMPLATE=semfs-mount-kaifa WB_PERSONA=kaifa
export WB_AGENT_TIMEOUT=1800 WB_CELL_TIMEOUT=2100 WB_INLINE_JUDGE=1
KAIFA="3,7,91,92,94,226,242,266,286,300,311"
HRSTATS="$HEADROOM_GLM_BASE/../stats"

stop_gpu(){ modal app stop $GLM_VLLM_APP --yes 2>&1 | tail -1 || echo "!! STOP $GLM_VLLM_APP"; }
trap stop_gpu EXIT

echo "######## deploy + warm GLM $(date +%H:%M:%S) ########"
modal deploy benchmarks/modal/glm51_nvfp4_vllm.py 2>&1 | tail -2
warm=0; for i in $(seq 1 60); do
  c=$(curl -s -m 15 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $KEY" "$VLLM_MODELS" 2>/dev/null)
  [ "$c" = 200 ] && { warm=1; break; }; echo "  warming $i http=$c"; sleep 30; done
[ "$warm" = 1 ] || { echo "!! GLM never warm"; exit 1; }
echo "GLM warm $(date +%H:%M:%S)"

if [ "$MODE" = smoke ]; then CASES=266; REPS="1"; OUT=hr_smoke; ARMS_HR="1"
else CASES="$KAIFA"; REPS="1"; OUT=hr_full; ARMS_HR="1 0"; fi
export WB_OUT="$PWD/tickets/next-plaid-late-interaction/artifacts/$OUT"
[ "$MODE" = smoke ] && rm -rf "$WB_OUT"   # smoke must re-run the cell (don't skip a done cell)
WB_LITE_SRC="$PWD/benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en"
[ -d "$WB_LITE_SRC" ] && { mkdir -p /tmp/wb_lite && rm -rf /tmp/wb_lite/task_lite_clean_en; cp -r "$WB_LITE_SRC" /tmp/wb_lite/task_lite_clean_en; }

for HRON in $ARMS_HR; do
  export WB_HEADROOM="$HRON"
  echo "=== [$MODE] kaifa plain · WB_HEADROOM=$HRON · cases=$CASES $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases "$CASES" --agents codex \
    --arms plain --reps "$REPS" --parallel 6 2>&1 || echo "!! run_matrix FAILED (hr=$HRON)"
done

echo "######## headroom proxy stats (compression) ########"
curl -s -m 20 -H "Authorization: Bearer $KEY" "$HRSTATS" 2>&1 | python3 -m json.tool 2>/dev/null | head -30 || \
  curl -s -m 20 -H "Authorization: Bearer $KEY" "$HRSTATS" 2>&1 | head -c 600
echo; echo "######## stop GPU $(date +%H:%M:%S) ########"
