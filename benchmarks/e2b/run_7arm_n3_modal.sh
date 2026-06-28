#!/bin/bash
# 7-ARM × 5-case × n=3 matrix on Modal GLM-5.1 = 105 cells.
#   Arms (per EXPERIMENTS_9_ARM.md, arms 1-7; 8-9 not implemented):
#     1 plain                          --arms plain
#     2 compress only                  --arms nokg --knobs compress_only_clean.json   (NOT plain: compress needs a mount)
#     3 compress + dedup               --arms nokg --knobs compress_dedup_clean.json
#     4 cdp, L7 off                    --arms best          --knobs best_exp0002.json
#     5 cdp, L7 on                     --arms hiddenkg_edges --knobs best_exp0002.json
#     6 cdp + hidden-KG rerank, L7 off --arms hiddenkg       --knobs best_exp0002.json
#     7 cdp + hidden-KG rerank, L7 on  --arms hiddenkg_l7    --knobs best_exp0002.json
# Distinct rep PREFIXES so arms 2&3 (both nokg) don't collide on cell label.
# Parallelism: 4 knob-jobs launched concurrently. Peak = PB + 3*PS (default 10 + 3*3 = 19 ≥15).
#   Arms 4-7 share best_exp0002.json → one run_matrix call (20 cells/rep) at PB.
# Timeout RAISED to 90/95 min so 20-parallel ~18-21s/call over-explorers finish (no timeout confound).
# PREREQ: glm51-vllm WARM; run `run_matrix.py --preflight` once first to confirm seeds surface-clean.
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_E2B_TEMPLATE=semfs-baked-v2 WB_MODAL_GLM=1
# Modal endpoint = the NVFP4 deploy; served model name is now `glm-5.1-nvfp4` (matches the weights).
# The model under test is GLM-5.1-NVFP4 — report it as such (NVFP4 quant, NOT the bf16/fp8 GLM-5.1).
export WB_MODAL_BASE="${WB_MODAL_BASE:-https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1}"
export WB_MODAL_MODEL="${WB_MODAL_MODEL:-glm-5.1-nvfp4}"
# The BAKED template binary is stale (missing SEMFS_DEDUP_WINDOW + SEMFS_HIDDEN_KG) → push the
# verified one so dedup (arms 3-7) and hidden-KG (arms 6-7) actually fire. (Arms 8-9 = retrieval-
# proper need SEMFS_HIDDEN_KG_RETRIEVAL which NO binary has yet → not in this run.)
export WB_FIXED_BIN="${WB_FIXED_BIN:-benchmarks/e2b/assets/semfs-fixed}"
# nokg's default seed (chanpin-clean.db) is surface-contaminated; route arms 2-3 to the
# verified surface-clean chanpin-4arm.db → ALL semfs arms share one seed (KG tables inert for nokg).
export WB_E2B_SEED_NOKG="${WB_E2B_SEED_NOKG:-/opt/chanpin-4arm.db}"
export MODAL_VLLM_API_KEY=$(cat /tmp/glm_vllm_key.txt)
export WB_AGENT_TIMEOUT="${WB_AGENT_TIMEOUT:-5400}"   # 90 min (raised from 60)
export WB_CELL_TIMEOUT="${WB_CELL_TIMEOUT:-5700}"     # 95 min
unset WB_FORCE_OPENROUTER
CASES="${WB_CASES:-15,44,53,95,175}"
PB="${WB_PAR_BULK:-10}"     # arms 4-7 (best_exp0002), 20 cells/rep — the bulk
PS="${WB_PAR_SMALL:-3}"     # plain / compress-only / compress-dedup, 5 cells/rep each
NB=benchmarks/e2b/knobs
RM="python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex"

run_job(){ # $1=prefix $2=arms $3=knob(or -) $4=parallel — loops n=3 reps
  local kargs=""; [ "$3" != "-" ] && kargs="--knobs $NB/$3"
  for r in 1 2 3; do
    $RM --arms "$2" $kargs --rep "$1$r" --parallel "$4" 2>&1 | sed "s/^/[$1$r] /"
  done
}

echo "7-ARM n=3 START @ $(date +%H:%M:%S) — peak ~$((PB + 3*PS)) concurrent, timeout ${WB_AGENT_TIMEOUT}s, template=$WB_E2B_TEMPLATE"
run_job a47 "best,hiddenkg_edges,hiddenkg,hiddenkg_l7" "best_exp0002.json"        "$PB" &
run_job a1p "plain"  "-"                          "$PS" &
run_job a2c "nokg"   "compress_only_clean.json"   "$PS" &
run_job a3d "nokg"   "compress_dedup_clean.json"  "$PS" &
wait
echo "ALL 105 CELLS DONE @ $(date +%H:%M:%S) — run run_judge.py next for accuracy."
