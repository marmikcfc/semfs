#!/usr/bin/env bash
# SEM-38: build the two fresh seeds (chanpin + kaifa) with the gemma-uniform recipe,
# FENCING the gemma-4-31b KG GPU to its own window so no GPU time is wasted:
#
#   PHASE 1  embed     (no GPU)  PARALLEL: N-shard fan-out of seed_dir (extract +
#                                summarize via OpenRouter gemma-4-31b-it + embed gemma-q4
#                                ONNX text+code lanes) → merge partials — both seeds
#   ── deploy gemma-4-31b vLLM, wait warm ──
#   PHASE 2  kg        (GPU up)  build_kg: gemma-4-31b entity extraction — both seeds
#   ── stop gemma vLLM ──
#   PHASE 3  finalize  (no GPU)  materialize_kg (Leiden/kNN) + materialize_fs (tree) — both
#
# Orchestration only — Rust never knows about phases (see semfs_modal.py::build_corpus_seed).
# Prereqs on the Modal volume: corpus/{chanpin_standard,kaifa_standard}, models/gemma_q4,
# gemma-4-31b-nvfp4 weights (download_weights if missing). Secrets: openrouter, glm-vllm-key.
#
# Run:  bash benchmarks/modal/build_fresh_seeds.sh
set -uo pipefail
cd "$(dirname "$0")/../.."   # repo root

MODAL=modal
SM=benchmarks/modal/semfs_modal.py
GEMMA=benchmarks/modal/gemma4_31b_nvfp4_vllm.py
GEMMA_APP=gemma4-31b-nvfp4-vllm
GEMMA_HEALTH=https://ada-diffusion-llm--gemma4-31b-nvfp4-vllm-serve.modal.run/health
export SEMFS_SEED_ONLY=1   # build runs need only openrouter (+ glm-vllm-key) secrets

# (corpus_name  out_seed) — same recipe both, so the seeds are comparable.
SEEDS=(
  "chanpin_standard chanpin-gemma-q4.db"
  "kaifa_standard   kaifa-gemma-q4.db"
)
NSHARDS=12   # parallel seed_dir workers per seed — embeds AND OpenRouter summaries fan out N-wide

run_embed_sharded () {  # PHASE 1: parallel embed via shard fan-out + merge (no GPU)
  for pair in "${SEEDS[@]}"; do
    set -- $pair   # $1=corpus_name $2=out_seed
    echo "=== [embed x$NSHARDS] $1 → $2 ($(date +%H:%M:%S)) ==="
    "$MODAL" run "$SM"::embed_sharded --corpus-name "$1" --out-name "$2" --n-shards "$NSHARDS" \
      || echo "!! [embed] $1 FAILED — inspect above"
  done
}

run_phase () {  # $1 = kg|finalize (single process; build_kg is internally 8-worker concurrent)
  local phase="$1"
  for pair in "${SEEDS[@]}"; do
    set -- $pair   # $1=corpus_name $2=out_seed  (phase saved above)
    echo "=== [$phase] $1 → $2 ($(date +%H:%M:%S)) ==="
    "$MODAL" run "$SM"::index_corpus --corpus-name "$1" --out-name "$2" --phase "$phase" \
      || echo "!! [$phase] $1 FAILED — inspect above"
  done
}

echo "######## PHASE 1: EMBED (sharded x$NSHARDS, no GPU) ########"
run_embed_sharded

echo "######## deploy gemma-4-31b vLLM — KG GPU window opens ########"
"$MODAL" deploy "$GEMMA"
echo "waiting for gemma vLLM /health (max ~45 min) ..."
warm=0
for i in $(seq 1 90); do   # 90 × 30s = 45 min (the vLLM startup_timeout)
  if curl -s -m 10 -o /dev/null -w '%{http_code}' "$GEMMA_HEALTH" 2>/dev/null | grep -q 200; then
    warm=1; break
  fi
  echo "  ...booting ($i/90) $(date +%H:%M:%S)"; sleep 30
done
if [ "$warm" -ne 1 ]; then
  echo "!! gemma vLLM never became healthy in 45 min — STOPPING GPU to avoid runaway cost"
  "$MODAL" app stop "$GEMMA_APP" --yes
  exit 1
fi
echo "gemma vLLM warm at $(date +%H:%M:%S)."

echo "######## PHASE 2: KG (GPU up) ########"
run_phase kg

echo "######## stop gemma-4-31b vLLM — KG GPU window closes ########"
"$MODAL" app stop "$GEMMA_APP" --yes || echo "!! failed to stop $GEMMA_APP — STOP IT MANUALLY"

echo "######## PHASE 3: FINALIZE (no GPU) ########"
run_phase finalize

echo "######## DONE $(date +%H:%M:%S) — both seeds built. Next: verify §5 → bake → E2B smoke. ########"
