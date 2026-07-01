#!/usr/bin/env bash
# SEM: plain arm baseline, WB-Lite 4 personas (chanpin/kaifa/houqin/yunying),
# codex on self-hosted GLM-5.1-NVFP4, n=3. GPU (glm51-nvfp4-vllm, 4xB200) is
# FENCED to this run and auto-stopped on any exit. Plain arm = agent in the raw
# persona workspace (corpus baked into each semfs-mount-{persona} template's
# /opt/corpus.tgz, extracted in-sandbox). NO semfs mount/seed. ALL runs on E2B.
#
# Run:  bash tickets/wblite-plain-4persona-n3/run_plain.sh
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a

MODAL=modal
GLM_VLLM=benchmarks/modal/glm51_nvfp4_vllm.py
GLM_LITELLM=benchmarks/modal/glm51_nvfp4_litellm.py
GLM_VLLM_APP=glm51-nvfp4-vllm
GLM_LITELLM_APP=glm51-nvfp4-litellm
VLLM_MODELS=https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run/v1/models

# Key: .env has MODAL_GLM_VLLM_API_KEY; the harness expects MODAL_VLLM_API_KEY.
export MODAL_VLLM_API_KEY="${MODAL_GLM_VLLM_API_KEY:-${MODAL_VLLM_API_KEY:-}}"
KEY="$MODAL_VLLM_API_KEY"

# codex → litellm proxy → vLLM (the canonical GLM-5.1-NVFP4 path, GLM51_RUNBOOK)
export WB_MODAL_GLM=1
export WB_MODAL_BASE=https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1
export WB_MODAL_MODEL=glm-5.1-nvfp4
export WB_LITE_DIR=/Users/marmikpandya/semantic-filesystem/benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en
export WB_AGENT_TIMEOUT="${WB_AGENT_TIMEOUT:-2700}" WB_CELL_TIMEOUT="${WB_CELL_TIMEOUT:-3000}"
unset WB_FORCE_OPENROUTER 2>/dev/null || true

# macOS /bin/bash is 3.2 (no associative arrays) → per-persona vars + indirect expansion.
PAR="${PAR:-8}"
PERSONAS=(${WB_PERSONAS:-chanpin kaifa houqin yunying})   # WB_PERSONAS=chanpin → canary
chanpin_tmpl=semfs-mount-chanpin
kaifa_tmpl=semfs-mount-kaifa
houqin_tmpl=semfs-mount-houqin
yunying_tmpl=semfs-mount-yunying
# 289 EXCLUDED: answer-file leak — best_selling_product_core_data_list.txt (finished top-10) is
# baked into the corpus AND the rubric wants a "403 Forbidden / source inaccessible" report.
# Matches run_matrix CASES_FULL. (audit 2026-06-22)
chanpin_cases="15,44,45,53,55,95,171,175,386,388"
kaifa_cases="3,7,91,92,94,226,242,266,286,300,311"
houqin_cases="23,35,37,47,54,72,79,83,85,87,100,102,116,207,251,255,258,267,274,276,314,328,329,337,354,357,358,372,373,374"
yunying_cases="33,38,107,108,137,139,143,146,154,158,159,160,161,191,192,224,244,269,277,278,284,287,288,291,306,334,340,346,359,380,381"
# --force ONLY chanpin: its generic _r{1,2,3} labels collide with stale ChatGPT/OpenRouter
# cells from prior runs (status=ok → would be skipped → pollute the GLM baseline). The other 3
# personas have no priors, so no force needed (avoids re-running on resume). (audit 2026-06-22)
chanpin_force="--force"; kaifa_force=""; houqin_force=""; yunying_force=""

stop_gpu(){ $MODAL app stop $GLM_VLLM_APP --yes 2>&1 || echo "!! could not stop $GLM_VLLM_APP — STOP MANUALLY"; }
trap stop_gpu EXIT   # GPU comes down on success, error, OR interrupt

echo "######## deploy GLM-5.1-NVFP4 vLLM — GPU STARTS $(date +%H:%M:%S) ########"
$MODAL deploy $GLM_VLLM 2>&1 | tail -3
echo "waiting for vLLM /v1/models (cold start ~30-43 min)..."
warm=0
for i in $(seq 1 100); do  # 100 x 30s = 50 min cap
  code=$(curl -s -m 15 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $KEY" "$VLLM_MODELS" 2>/dev/null)
  [ "$code" = "200" ] && { warm=1; break; }
  echo "  warming ($i/100) http=$code $(date +%H:%M:%S)"; sleep 30
done
[ "$warm" -ne 1 ] && { echo "!! GLM vLLM never warm in 50min — stopping GPU"; exit 1; }
echo "GLM vLLM warm $(date +%H:%M:%S)"
$MODAL deploy $GLM_LITELLM 2>&1 | tail -2   # proxy (CPU, idempotent) — ensure it points at the warm vLLM

for persona in "${PERSONAS[@]}"; do
  tvar="${persona}_tmpl"; cvar="${persona}_cases"; fvar="${persona}_force"
  export WB_E2B_TEMPLATE="${!tvar}"
  for rep in 1 2 3; do
    # Refresh the litellm proxy each rep: it degrades under sustained parallel load
    # (canary2 rep2 hang — cells stalled upstream of an idle vLLM). Fresh proxy per
    # rep keeps the codex→GLM path healthy. ~15s CPU redeploy. (diag 2026-06-23)
    echo "  refresh litellm proxy $(date +%H:%M:%S)"
    $MODAL app stop $GLM_LITELLM_APP --yes 2>&1 | tail -1 || true
    $MODAL deploy $GLM_LITELLM 2>&1 | tail -1
    echo "=== [plain/$persona] rep=$rep force='${!fvar}' $(date +%H:%M:%S) ==="
    python3 benchmarks/e2b/run_matrix.py --cases "${!cvar}" \
      --agents codex --arms plain --rep "$rep" --parallel "$PAR" ${!fvar} 2>&1 \
      || echo "!! [plain/$persona rep $rep] FAILED — inspect above"
  done
done

echo "######## stop GLM-5.1-NVFP4 vLLM — GPU SHUTS DOWN $(date +%H:%M:%S) ########"
stop_gpu; trap - EXIT
echo "######## PLAIN RUN DONE $(date +%H:%M:%S) ########"
$MODAL app list 2>&1 | grep -iE "glm51-nvfp4-vllm.*deployed" && echo "!! WARNING: vLLM still deployed — VERIFY STOPPED" || echo "vLLM not deployed = GPU stopped ✓"
