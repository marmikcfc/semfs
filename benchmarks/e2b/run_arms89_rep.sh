#!/bin/bash
# Arms 8-9 (hidden-KG RETRIEVAL: candidate injection into the pool, not just rerank) — ONE rep.
#   arm 8 hiddenkg_retrieval     L7 co-mention OFF
#   arm 9 hiddenkg_retrieval_l7  L7 co-mention ON
# 2 arms × 5 cases = 10 cells per rep. Launch 3× as SEPARATE bg tasks for n=3
# (do NOT inner-& — that orphans the jobs when wrapped in run_in_background).
#   Usage:  bash benchmarks/e2b/run_arms89_rep.sh <repnum>
# Endpoint = the NVFP4 LiteLLM deploy (served name glm-5.1-nvfp4). Binary = semfs-fixed-retrieval
# (the only build with SEMFS_HIDDEN_KG_RETRIEVAL=1). Seed = chanpin-4arm.db (default for these arms;
# preflight confirmed graph_entity+community tables are populated → candidate_count=66 injected).
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_E2B_TEMPLATE=semfs-baked-v2 WB_MODAL_GLM=1
export WB_MODAL_BASE=https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1
export WB_MODAL_MODEL=glm-5.1-nvfp4
export WB_FIXED_BIN=benchmarks/e2b/assets/semfs-fixed-retrieval
export MODAL_VLLM_API_KEY=$(cat /tmp/glm_vllm_key.txt)
export WB_AGENT_TIMEOUT="${WB_AGENT_TIMEOUT:-2700}"   # 45 min leash (preflight ran in 9 min w/ brake)
export WB_CELL_TIMEOUT="${WB_CELL_TIMEOUT:-3000}"
unset WB_FORCE_OPENROUTER
R="$1"
echo "ARMS8-9 rep a89$R START $(date +%H:%M:%S) — litellm endpoint, binary=semfs-fixed-retrieval"
python3 benchmarks/e2b/run_matrix.py --cases "${WB_CASES:-15,44,53,95,175}" --agents codex \
  --arms hiddenkg_retrieval,hiddenkg_retrieval_l7 \
  --knobs benchmarks/e2b/knobs/best_exp0002.json --rep "a89$R" --parallel "${WB_PAR:-6}" 2>&1
echo "ARMS8-9 rep a89$R DONE $(date +%H:%M:%S)"
