#!/usr/bin/env bash
# MAP-IN-CONTEXT smoke — does a cached workspace map rescue the cases ranking buried?
# Persona: houqin (big/decoy, where plain beat ppr). Arms: ppr_on vs ppr_map (= ppr_on
# retrieval + cached map injected → map is the ONLY variable). Cases: the decisive failures
# 358 (thematic burial), 357 (under-exploration), 251 (filename-artifact control), 267 (control
# where ranking already works → must not regress). n=2. Plain = SEM-40 reference (not re-run).
# Filename-hint confound is FIXED (WB_LITE → complete metadata) so this measures retrieval.
# Run: WB_SKIP_GLM_DEPLOY=1 bash tickets/wblite-ppr-ab/run_map_smoke.sh
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

export WB_MODAL_GLM=1
export WB_MODAL_BASE=https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1
export WB_MODAL_MODEL=glm-5.1-nvfp4
unset WB_FORCE_OPENROUTER 2>/dev/null || true

export WB_FIXED_BIN=/Users/marmikpandya/semantic-filesystem/benchmarks/e2b/assets/semfs-fixed
export WB_OUT=/Users/marmikpandya/semantic-filesystem/tickets/wblite-ppr-ab/artifacts/map_smoke
export WB_AGENT_TIMEOUT="${WB_AGENT_TIMEOUT:-2000}" WB_CELL_TIMEOUT="${WB_CELL_TIMEOUT:-2300}"
export WB_MOUNT_STARTUP_TIMEOUT="${WB_MOUNT_STARTUP_TIMEOUT:-240}"
export WB_SEARCH_ONLY="${WB_SEARCH_ONLY:-off}"
export WB_INLINE_JUDGE=1
KNOBS=benchmarks/e2b/knobs/ppr_ab.json
PAR="${PAR:-8}"
CASES="358,357,251,267"
ARMS=(ppr_on ppr_map)
mkdir -p "$WB_OUT" /tmp/bake_logs

# rubrics for the inline judge
WB_LITE_SRC=/Users/marmikpandya/semantic-filesystem/benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en
[ -d /tmp/wb_lite/task_lite_clean_en ] || { mkdir -p /tmp/wb_lite && cp -r "$WB_LITE_SRC" /tmp/wb_lite/task_lite_clean_en; }

export WB_E2B_TEMPLATE=semfs-mount-houqin
export WB_E2B_SEED_DEFAULT=/opt/houqin-gemma-q4.db
export WB_BOOT_SEED=/opt/houqin-gemma-q4.db
export WB_PERSONA=houqin

stop_gpu(){ $MODAL app stop $GLM_VLLM_APP --yes 2>&1 || echo "!! stop $GLM_VLLM_APP MANUALLY"; }
trap stop_gpu EXIT

echo "######## GLM warm-up $(date +%H:%M:%S) ########"
if [ "${WB_SKIP_GLM_DEPLOY:-0}" = "1" ]; then echo "skip deploy (warm-check only)"; else $MODAL deploy $GLM_VLLM 2>&1 | tail -3; fi
warm=0
for i in $(seq 1 100); do
  code=$(curl -s -m 15 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $KEY" "$VLLM_MODELS" 2>/dev/null)
  [ "$code" = "200" ] && { warm=1; break; }
  echo "  warming ($i/100) http=$code $(date +%H:%M:%S)"; sleep 30
done
[ "$warm" -ne 1 ] && { echo "!! GLM never warm"; exit 1; }
echo "GLM warm $(date +%H:%M:%S)"

for arm in "${ARMS[@]}"; do
  echo "  refresh litellm proxy $(date +%H:%M:%S)"
  $MODAL app stop $GLM_LITELLM_APP --yes 2>&1 | tail -1 || true
  $MODAL deploy $GLM_LITELLM 2>&1 | tail -1
  echo "=== [SMOKE houqin/$arm] cases=$CASES reps=1,2 $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases "$CASES" \
    --agents codex --arms "$arm" --reps 1,2 --parallel "$PAR" --knobs "$KNOBS" 2>&1 \
    || echo "!! [SMOKE houqin/$arm] FAILED"
done

echo "######## stop GPU $(date +%H:%M:%S) ########"; stop_gpu; trap - EXIT
echo "######## MAP SMOKE DONE $(date +%H:%M:%S) ########"
echo "compare: ppr_on vs ppr_map on 358/357 (burial) + 267 (control). Plain ref: 358=47 357=73 251=45 267=88."