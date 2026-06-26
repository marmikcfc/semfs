#!/usr/bin/env bash
# PPR A/B — QUEUE harness. ONE run_matrix invocation PER PERSONA (not per arm/rep): all
# reps x arms go through a global queue, each sandbox boots ONCE and re-mounts only on arm
# change. Kills the boot tax (24→4 boots) + the per-arm long-tail barrier + per-cell remount.
# Resume-safe (done cells skip). Network-resilient (deadline wrapper). GPU fenced.
# Run: WB_SKIP_GLM_DEPLOY=1 bash tickets/wblite-ppr-ab/run_ppr_ab_queue.sh
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
export WB_OUT=/Users/marmikpandya/semantic-filesystem/tickets/wblite-ppr-ab/artifacts/e2b_runs
export WB_AGENT_TIMEOUT="${WB_AGENT_TIMEOUT:-2000}" WB_CELL_TIMEOUT="${WB_CELL_TIMEOUT:-2300}"
export WB_MOUNT_STARTUP_TIMEOUT="${WB_MOUNT_STARTUP_TIMEOUT:-240}"
export WB_SEARCH_ONLY="${WB_SEARCH_ONLY:-off}"
export WB_INLINE_JUDGE=1
KNOBS=benchmarks/e2b/knobs/ppr_ab.json
PAR="${PAR:-16}"
mkdir -p "$WB_OUT" /tmp/bake_logs

# stage rubrics for the inline judge
WB_LITE_SRC=/Users/marmikpandya/semantic-filesystem/benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en
if [ -d "$WB_LITE_SRC" ] && [ ! -d /tmp/wb_lite/task_lite_clean_en ]; then
  mkdir -p /tmp/wb_lite && cp -r "$WB_LITE_SRC" /tmp/wb_lite/task_lite_clean_en
fi

PERSONAS=(${WB_PERSONAS:-chanpin kaifa houqin yunying})
chanpin_tmpl=semfs-mount-chanpin;  chanpin_seed=/opt/chanpin-gemma-q4.db
kaifa_tmpl=semfs-mount-kaifa;      kaifa_seed=/opt/kaifa-gemma-q4.db
houqin_tmpl=semfs-mount-houqin;    houqin_seed=/opt/houqin-gemma-q4.db
yunying_tmpl=semfs-mount-yunying;  yunying_seed=/opt/yunying-gemma-q4.db
chanpin_cases="15,44,45,53,55,95,171,175,386,388"
kaifa_cases="3,7,91,92,94,226,242,266,286,300,311"
houqin_cases="23,35,37,47,54,72,79,83,85,87,100,102,116,207,251,255,258,267,274,276,314,328,329,337,354,357,358,372,373,374"
yunying_cases="33,38,107,108,137,139,143,146,154,158,159,160,161,191,192,224,244,269,277,278,284,287,288,291,306,334,340,346,359,380,381"

# manifest (idempotent — keeps total at the full plan)
python3 - "$WB_OUT" "${PERSONAS[*]}" "$chanpin_cases" "$kaifa_cases" "$houqin_cases" "$yunying_cases" <<'PY'
import json,sys,time,pathlib
out,personas=sys.argv[1],sys.argv[2].split()
cm={"chanpin":sys.argv[3],"kaifa":sys.argv[4],"houqin":sys.argv[5],"yunying":sys.argv[6]}
cases={p:[c for c in cm[p].split(",") if c] for p in personas}
total=sum(len(cases[p]) for p in personas)*3*2
m={"run_id":"wblite-ppr-ab","started_at":time.time(),"agent":"codex","arms":["ppr_off","ppr_on"],
   "personas":personas,"reps":3,"cases":cases,"total_cells":total,"out_dir":out}
pathlib.Path(out,"manifest.json").write_text(json.dumps(m,indent=2))
print(f"manifest: {total} cells planned (QUEUE harness, per-persona batch)")
PY

stop_gpu(){ $MODAL app stop $GLM_VLLM_APP --yes 2>&1 || echo "!! could not stop $GLM_VLLM_APP — STOP MANUALLY"; }
trap stop_gpu EXIT

echo "######## GLM-5.1-NVFP4 vLLM $(date +%H:%M:%S) ########"
if [ "${WB_SKIP_GLM_DEPLOY:-0}" = "1" ]; then echo "WB_SKIP_GLM_DEPLOY=1 → warm-check only";
else $MODAL deploy $GLM_VLLM 2>&1 | tail -3; fi
warm=0
for i in $(seq 1 100); do
  code=$(curl -s -m 15 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $KEY" "$VLLM_MODELS" 2>/dev/null)
  [ "$code" = "200" ] && { warm=1; break; }
  echo "  warming ($i/100) http=$code $(date +%H:%M:%S)"; sleep 30
done
[ "$warm" -ne 1 ] && { echo "!! GLM vLLM never warm — stopping GPU"; exit 1; }
echo "GLM vLLM warm $(date +%H:%M:%S)"

# PER-PERSONA-PER-ARM batches: each batch boots fresh + mounts ONE arm (all 3 reps in the
# queue), so there is NO in-place ppr_off→ppr_on re-mount. That re-mount crashed the daemon
# on the big houqin/yunying seeds (2026-06-25: lost 125 ppr_on cells). One arm per batch =
# no transition = no crash, while still collapsing reps. Done cells skip → backfill-safe.
ARMS=(${WB_ARMS:-ppr_off ppr_on})
for persona in "${PERSONAS[@]}"; do
  tvar="${persona}_tmpl"; svar="${persona}_seed"; cvar="${persona}_cases"
  export WB_E2B_TEMPLATE="${!tvar}" WB_E2B_SEED_DEFAULT="${!svar}" WB_BOOT_SEED="${!svar}" WB_PERSONA="$persona"
  for arm in "${ARMS[@]}"; do
    echo "  refresh litellm proxy $(date +%H:%M:%S)"
    $MODAL app stop $GLM_LITELLM_APP --yes 2>&1 | tail -1 || true
    $MODAL deploy $GLM_LITELLM 2>&1 | tail -1
    echo "=== [BATCH $persona/$arm] reps=1,2,3 $(date +%H:%M:%S) ==="
    python3 benchmarks/e2b/run_matrix.py --cases "${!cvar}" \
      --agents codex --arms "$arm" --reps 1,2,3 --parallel "$PAR" --knobs "$KNOBS" 2>&1 \
      || echo "!! [BATCH $persona/$arm] FAILED — inspect above"
  done
done

echo "######## stop GLM vLLM $(date +%H:%M:%S) ########"
stop_gpu; trap - EXIT
echo "######## PPR A/B (QUEUE) DONE $(date +%H:%M:%S) ########"
