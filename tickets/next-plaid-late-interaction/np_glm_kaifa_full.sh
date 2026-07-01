#!/usr/bin/env bash
# Full houqin next_plaid_A on GLM-5.1 — 11 cases × n=2, PAR=8, np-kaifa-C template
# (colgrep underneath semfs grep). Inline-judged. GPU fenced (auto-stop on exit).
# A/B vs ppr_on (PPR-run data) + plain (references) is assembled after.
# Run: WB_SKIP_GLM_DEPLOY=1 bash tickets/next-plaid-late-interaction/np_glm_houqin_full.sh  (if GLM warm)
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a

MODAL=modal
GLM_VLLM=benchmarks/modal/glm51_nvfp4_vllm.py
GLM_LITELLM=benchmarks/modal/glm51_nvfp4_litellm.py
GLM_VLLM_APP=glm51-nvfp4-vllm
VLLM_MODELS=https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run/v1/models
export MODAL_VLLM_API_KEY="${MODAL_GLM_VLLM_API_KEY:-${MODAL_VLLM_API_KEY:-}}"
KEY="$MODAL_VLLM_API_KEY"

export WB_MODAL_GLM=1
export WB_MODAL_BASE=https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1
export WB_MODAL_MODEL=glm-5.1-nvfp4
unset WB_FORCE_OPENROUTER WB_OR_MODEL 2>/dev/null || true

export WB_E2B_TEMPLATE=np-kaifa-C WB_PERSONA=kaifa
export WB_OUT="$PWD/tickets/next-plaid-late-interaction/artifacts/kaifa_glm"
export WB_AGENT_TIMEOUT="${WB_AGENT_TIMEOUT:-1800}" WB_CELL_TIMEOUT="${WB_CELL_TIMEOUT:-2100}"
export WB_INLINE_JUDGE=1
PAR="${PAR:-8}"
CASES="3,7,91,92,94,226,242,266,286,300,311"
mkdir -p "$WB_OUT"
WB_LITE_SRC="$PWD/benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en"
[ -d "$WB_LITE_SRC" ] && { mkdir -p /tmp/wb_lite && rm -rf /tmp/wb_lite/task_lite_clean_en; cp -r "$WB_LITE_SRC" /tmp/wb_lite/task_lite_clean_en; }

stop_gpu(){ $MODAL app stop $GLM_VLLM_APP --yes 2>&1 || echo "!! STOP $GLM_VLLM_APP MANUALLY"; }
trap stop_gpu EXIT

echo "######## GLM-5.1-NVFP4 vLLM $(date +%H:%M:%S) ########"
if [ "${WB_SKIP_GLM_DEPLOY:-0}" = "1" ]; then echo "skip deploy (warm-check only)"; else $MODAL deploy $GLM_VLLM 2>&1 | tail -3; fi
warm=0
for i in $(seq 1 100); do
  code=$(curl -s -m 15 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $KEY" "$VLLM_MODELS" 2>/dev/null)
  [ "$code" = "200" ] && { warm=1; break; }
  echo "  warming ($i/100) http=$code $(date +%H:%M:%S)"; sleep 30
done
[ "$warm" -ne 1 ] && { echo "!! GLM never warm"; exit 1; }
echo "GLM warm $(date +%H:%M:%S)"
$MODAL deploy $GLM_LITELLM 2>&1 | tail -2

echo "=== [RUN] next_plaid_kaifa_C · 11 cases · reps 1,2 · PAR=$PAR $(date +%H:%M:%S) ==="
SECONDS=0
python3 benchmarks/e2b/run_matrix.py --cases "$CASES" --agents codex \
  --arms next_plaid_kaifa_C --reps 1 --parallel "$PAR" 2>&1 || echo "!! run_matrix FAILED"
echo "=== run wall: ${SECONDS}s ($((SECONDS/60))m) $(date +%H:%M:%S) ==="

echo "######## stop GPU $(date +%H:%M:%S) ########"; stop_gpu; trap - EXIT
# quick accuracy line
python3 - "$WB_OUT/judged.jsonl" <<'PY' 2>/dev/null || true
import json,sys,collections
rows=[json.loads(l) for l in open(sys.argv[1])] if __import__('os').path.exists(sys.argv[1]) else []
p=t=0
for r in rows:
    if isinstance(r.get('passed'),(int,float)) and r.get('total'): p+=r['passed']; t+=r['total']
print(f"next_plaid_kaifa_C: {p}/{t} = {100*p/t:.1f}%  ({len(rows)} judged cells)" if t else "no judged rows")
PY
echo "######## NP GLM HOUQIN FULL DONE $(date +%H:%M:%S) ########"
