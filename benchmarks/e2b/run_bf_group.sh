#!/bin/bash
# ONE backfill knob-group, for PARALLEL launch (4 groups concurrently to use freed capacity).
#   Usage:  bash run_bf_group.sh <rep> <parallel> <arms> [knobfile]   (launch as separate bg tasks)
# litellm endpoint, semfs-fixed binary (arms 1-7), clean seed. No sed pipe → visible progress.
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_E2B_TEMPLATE="${WB_E2B_TEMPLATE:-semfs-baked-v3}" WB_MODAL_GLM=1   # v3 = v2 + python-docx/pptx writers
export WB_MODAL_BASE=https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1
export WB_MODAL_MODEL=glm-5.1-nvfp4
export WB_FIXED_BIN="${WB_FIXED_BIN:-benchmarks/e2b/assets/semfs-fixed}"   # override with semfs-fixed-retrieval for the retrieval arm
export WB_E2B_SEED_NOKG=/opt/chanpin-4arm.db
export MODAL_VLLM_API_KEY=$(cat /tmp/glm_vllm_key.txt)
export WB_AGENT_TIMEOUT="${WB_AGENT_TIMEOUT:-2700}" WB_CELL_TIMEOUT="${WB_CELL_TIMEOUT:-3000}"
unset WB_FORCE_OPENROUTER
REP="$1"; PAR="$2"; ARMS="$3"; KNOB="${4:-}"
K=""; [ -n "$KNOB" ] && K="--knobs benchmarks/e2b/knobs/$KNOB"
echo "BF-GROUP rep=$REP arms=$ARMS PAR=$PAR START $(date +%H:%M:%S)"
python3 benchmarks/e2b/run_matrix.py --cases "${WB_CASES:-15,44,53,95,175}" --agents codex \
  --arms "$ARMS" $K --rep "$REP" --parallel "$PAR" 2>&1
echo "BF-GROUP rep=$REP DONE $(date +%H:%M:%S)"
