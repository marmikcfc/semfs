#!/bin/bash
# Backfill ONE rep of the 7-arm matrix (arms 1-7) to refill timed-out cells → n>=2/3.
# Throttled (low --parallel) so it can coexist with a concurrent arms-8-9 run under
# E2B's ~20-sandbox cap; knob-groups run SEQUENTIALLY so peak footprint = PAR.
#   Usage:  bash benchmarks/e2b/run_backfill_rep.sh <reptag> [PAR]   e.g. rb1 4
# Config mirrors run_7arm_n3_modal.sh (same arms+knobs) but on the litellm endpoint.
# Distinct rep suffix per knob-group so the two nokg arms don't collide on label.
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_E2B_TEMPLATE=semfs-baked-v2 WB_MODAL_GLM=1
export WB_MODAL_BASE=https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1
export WB_MODAL_MODEL=glm-5.1-nvfp4
export WB_FIXED_BIN=benchmarks/e2b/assets/semfs-fixed        # arms 1-7 (dedup+hidden-KG, NOT retrieval)
export WB_E2B_SEED_NOKG=/opt/chanpin-4arm.db                 # route nokg off the contaminated default
export MODAL_VLLM_API_KEY=$(cat /tmp/glm_vllm_key.txt)
export WB_AGENT_TIMEOUT="${WB_AGENT_TIMEOUT:-2700}" WB_CELL_TIMEOUT="${WB_CELL_TIMEOUT:-3000}"
unset WB_FORCE_OPENROUTER
T="$1"; PAR="${2:-4}"; CASES="${WB_CASES:-15,44,53,95,175}"
NB=benchmarks/e2b/knobs
RM="python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --parallel $PAR"
echo "BACKFILL rep $T (PAR=$PAR) START $(date +%H:%M:%S)"
# NOTE: do NOT pipe through sed — a pipe block-buffers stdout so progress is invisible until exit.
echo "[${T}m] === best-group ==="; $RM --arms best,hiddenkg_edges,hiddenkg,hiddenkg_l7 --knobs $NB/best_exp0002.json --rep "${T}m" 2>&1
echo "[${T}p] === plain ===";      $RM --arms plain                                    --rep "${T}p" 2>&1
echo "[${T}c] === compress ===";   $RM --arms nokg --knobs $NB/compress_only_clean.json  --rep "${T}c" 2>&1
echo "[${T}d] === comp+dedup ==="; $RM --arms nokg --knobs $NB/compress_dedup_clean.json --rep "${T}d" 2>&1
echo "BACKFILL rep $T DONE $(date +%H:%M:%S)"
