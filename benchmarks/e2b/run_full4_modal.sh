#!/bin/bash
# FULL 4-ARM WB-Lite run on Modal GLM-5.1 — all 20 cells/rep in PARALLEL.
#   arms: plain | best(compress+dedup+oc+prompt) | sufficiency | KG-on(kg + prompt)
#   5 cases × n=2 = 40 cells, run as 2 waves of 20 concurrent sandboxes (4 arms × 5 cases each).
# PREREQUISITES (verify before running):
#   - glm51-vllm WARM (curl /v1/models = 200)  ·  semfs-baked-v2 template  ·  rebuilt semfs-fixed (has SEMFS_SUFFICIENCY)
# Distinct rep PREFIXES per arm so best & sufficiency (both nokg) don't collide on cell label.
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_E2B_TEMPLATE=semfs-baked-v2
export WB_MODAL_GLM=1
export MODAL_VLLM_API_KEY=$(cat /tmp/glm_vllm_key.txt)
export WB_AGENT_TIMEOUT=3600 WB_CELL_TIMEOUT=3900   # raised: 20-parallel = ~21s/call, over-explorers need ~60min
# Parallelism per arm (4 arms run concurrently → 4×PAR sandboxes total). PAR=5 → 20 concurrent.
PAR="${WB_PAR:-5}"
unset WB_FORCE_OPENROUTER
CASES=15,44,53,95,175
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs

# "rep_prefix  arm  knob(or -)"
ARMS=(
  "frpl    plain  -"
  "frcdoc  nokg   compress_dedup_oc.json"
  "frsuf   nokg   sufficiency.json"
  "frkg    kg     prompt_only.json"
)

run_arm(){ # $1=prefix $2=arm $3=knob $4=rep
  local kargs=""; [ "$3" != "-" ] && kargs="--knobs benchmarks/e2b/knobs/$3"
  python3 benchmarks/e2b/run_matrix.py --cases "$CASES" --agents codex --arms "$2" $kargs \
    --rep "$1$4" --parallel "$PAR" 2>&1 | sed "s/^/[$1$4] /"
}

echo "FULL4 START @ $(date +%H:%M:%S)  (template=$WB_E2B_TEMPLATE, modal-glm51, 20-in-parallel)"
for r in 1 2; do
  echo "=== WAVE rep=$r : 4 arms × 5 cases = 20 concurrent @ $(date +%H:%M:%S) ==="
  pids=()
  for spec in "${ARMS[@]}"; do
    set -- $spec
    run_arm "$1" "$2" "$3" "$r" &
    pids+=("$!")
  done
  for p in "${pids[@]}"; do wait "$p"; done
  echo "=== WAVE rep=$r DONE @ $(date +%H:%M:%S) ==="
done
echo "ALL 40 CELLS DONE @ $(date +%H:%M:%S) — run run_judge.py next for accuracy."