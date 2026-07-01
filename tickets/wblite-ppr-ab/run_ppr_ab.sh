#!/usr/bin/env bash
# PPR A/B on WB-Lite, 4 personas (chanpin/kaifa/houqin/yunying), codex on self-hosted
# GLM-5.1-NVFP4, n=3. Two arms, IDENTICAL except the hidden-KG graph-prior algorithm:
#   ppr_off = 1-hop bounded neighbor boost (control)
#   ppr_on  = in-memory Personalized PageRank diffusion (treatment)
# Both: input-compress (gpt-4.1-nano) + output-compress + dedup + turnbrake (ppr_ab.json),
# adaptive-K on (pool 10), instruction-less flagless-grep prompt, fixed PPR semfs binary.
# Inline per-cell judging (Seed-2.0-Lite) + live web dashboard. GPU fenced + auto-stopped.
# ALL on E2B. Run: bash tickets/wblite-ppr-ab/run_ppr_ab.sh
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

# codex → litellm proxy → vLLM (GLM-5.1-NVFP4 path)
export WB_MODAL_GLM=1
export WB_MODAL_BASE=https://ada-diffusion-llm--glm51-nvfp4-litellm-serve.modal.run/v1
export WB_MODAL_MODEL=glm-5.1-nvfp4
unset WB_FORCE_OPENROUTER 2>/dev/null || true

# ── PPR-run wiring ────────────────────────────────────────────────────────────
export WB_FIXED_BIN=/Users/marmikpandya/semantic-filesystem/benchmarks/e2b/assets/semfs-fixed  # x86_64-linux PPR binary
export WB_OUT=/Users/marmikpandya/semantic-filesystem/tickets/wblite-ppr-ab/artifacts/e2b_runs
export WB_AGENT_TIMEOUT="${WB_AGENT_TIMEOUT:-2000}"   # compress + 20-wide GLM contention → bigger cell budget
export WB_CELL_TIMEOUT="${WB_CELL_TIMEOUT:-2300}"     # > WB_AGENT_TIMEOUT (driver wraps the agent)
export WB_MOUNT_STARTUP_TIMEOUT="${WB_MOUNT_STARTUP_TIMEOUT:-240}"  # houqin's 1.24GB seed > 30s default watchdog
export WB_SEARCH_ONLY="${WB_SEARCH_ONLY:-off}"        # all 4 seeds now have fs_data (re-materialized)
export WB_INLINE_JUDGE=1                              # grade each cell as it lands → judged.jsonl (live)
KNOBS=benchmarks/e2b/knobs/ppr_ab.json
PAR="${PAR:-8}"
ARMS=(ppr_off ppr_on)                                # control then treatment, interleaved per (persona,rep)
mkdir -p "$WB_OUT"

# Stage WB-Lite rubrics where the inline judge (run_judge) expects them.
WB_LITE_SRC=/Users/marmikpandya/semantic-filesystem/benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en
if [ -d "$WB_LITE_SRC" ]; then
  mkdir -p /tmp/wb_lite && rm -rf /tmp/wb_lite/task_lite_clean_en
  cp -r "$WB_LITE_SRC" /tmp/wb_lite/task_lite_clean_en
  echo "rubrics staged → /tmp/wb_lite/task_lite_clean_en ($(ls /tmp/wb_lite/task_lite_clean_en | wc -l | tr -d ' ') cases)"
else
  echo "!! rubric source missing ($WB_LITE_SRC) — inline judging will fail; fix before launch"
fi

# macOS bash 3.2 → per-persona vars + indirect expansion (mirror run_plain.sh).
PERSONAS=(${WB_PERSONAS:-chanpin kaifa houqin yunying})
chanpin_tmpl=semfs-mount-chanpin;  chanpin_seed=/opt/chanpin-gemma-q4.db
kaifa_tmpl=semfs-mount-kaifa;      kaifa_seed=/opt/kaifa-gemma-q4.db
houqin_tmpl=semfs-mount-houqin;    houqin_seed=/opt/houqin-gemma-q4.db
yunying_tmpl=semfs-mount-yunying;  yunying_seed=/opt/yunying-gemma-q4.db
chanpin_cases="15,44,45,53,55,95,171,175,386,388"
kaifa_cases="3,7,91,92,94,226,242,266,286,300,311"
houqin_cases="23,35,37,47,54,72,79,83,85,87,100,102,116,207,251,255,258,267,274,276,314,328,329,337,354,357,358,372,373,374"
yunying_cases="33,38,107,108,137,139,143,146,154,158,159,160,161,191,192,224,244,269,277,278,284,287,288,291,306,334,340,346,359,380,381"

# ── manifest.json (for the dashboard) ────────────────────────────────────────
python3 - "$WB_OUT" "${PERSONAS[*]}" "$chanpin_cases" "$kaifa_cases" "$houqin_cases" "$yunying_cases" <<'PY'
import json, sys, time, pathlib
out, personas = sys.argv[1], sys.argv[2].split()
casemap_all = {"chanpin": sys.argv[3], "kaifa": sys.argv[4], "houqin": sys.argv[5], "yunying": sys.argv[6]}
reps, arms = 3, ["ppr_off", "ppr_on"]
cases = {p: [c for c in casemap_all[p].split(",") if c] for p in personas}
total = sum(len(cases[p]) for p in personas) * reps * len(arms)
m = {"run_id": "wblite-ppr-ab", "started_at": time.time(), "agent": "codex",
     "arms": arms, "personas": personas, "reps": reps, "cases": cases,
     "total_cells": total, "out_dir": out}
pathlib.Path(out, "manifest.json").write_text(json.dumps(m, indent=2))
print(f"manifest: {total} cells planned ({len(personas)} personas × {reps} reps × {len(arms)} arms)")
PY

# ── live dashboard (background) ──────────────────────────────────────────────
PORT="${DASH_PORT:-8765}"
python3 benchmarks/e2b/dashboard.py --out "$WB_OUT" --port "$PORT" > /tmp/bake_logs/ppr_dashboard.log 2>&1 &
DASH_PID=$!
echo "dashboard: http://127.0.0.1:$PORT  (pid $DASH_PID)"

stop_gpu(){ $MODAL app stop $GLM_VLLM_APP --yes 2>&1 || echo "!! could not stop $GLM_VLLM_APP — STOP MANUALLY"; }
cleanup(){ stop_gpu; kill $DASH_PID 2>/dev/null || true; }
trap cleanup EXIT

echo "######## GLM-5.1-NVFP4 vLLM — GPU $(date +%H:%M:%S) ########"
# WB_SKIP_GLM_DEPLOY=1 on a RESUME when vLLM is already warm: do NOT redeploy (a redeploy
# cold-restarts vLLM and rapid redeploys cause 4xB200 GPU contention → engine-init crash,
# 2026-06-24). Always warm-CHECK below regardless, so we never run cells against a dead vLLM.
if [ "${WB_SKIP_GLM_DEPLOY:-0}" = "1" ]; then
  echo "WB_SKIP_GLM_DEPLOY=1 → skipping vLLM redeploy (warm-check only)"
else
  $MODAL deploy $GLM_VLLM 2>&1 | tail -3
fi
echo "waiting for vLLM /v1/models (cold start ~5-10 min)..."
warm=0
for i in $(seq 1 100); do
  code=$(curl -s -m 15 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $KEY" "$VLLM_MODELS" 2>/dev/null)
  [ "$code" = "200" ] && { warm=1; break; }
  echo "  warming ($i/100) http=$code $(date +%H:%M:%S)"; sleep 30
done
[ "$warm" -ne 1 ] && { echo "!! GLM vLLM never warm in 50min — stopping GPU"; exit 1; }
echo "GLM vLLM warm $(date +%H:%M:%S)"
$MODAL deploy $GLM_LITELLM 2>&1 | tail -2

for persona in "${PERSONAS[@]}"; do
  tvar="${persona}_tmpl"; svar="${persona}_seed"; cvar="${persona}_cases"
  export WB_E2B_TEMPLATE="${!tvar}"
  export WB_E2B_SEED_DEFAULT="${!svar}"   # arm_seed_source → ~/.semfs/chanpin.db before mount
  export WB_BOOT_SEED="${!svar}"          # initial boot seed matches the persona
  export WB_PERSONA="$persona"            # recorded into results.jsonl + judged.jsonl
  for rep in 1 2 3; do
    echo "  refresh litellm proxy $(date +%H:%M:%S)"   # degrades under sustained load (diag 2026-06-23)
    $MODAL app stop $GLM_LITELLM_APP --yes 2>&1 | tail -1 || true
    $MODAL deploy $GLM_LITELLM 2>&1 | tail -1
    for arm in "${ARMS[@]}"; do
      echo "=== [$persona/$arm] rep=$rep $(date +%H:%M:%S) ==="
      python3 benchmarks/e2b/run_matrix.py --cases "${!cvar}" \
        --agents codex --arms "$arm" --rep "$rep" --parallel "$PAR" --knobs "$KNOBS" 2>&1 \
        || echo "!! [$persona/$arm rep $rep] FAILED — inspect above"
    done
  done
done

echo "######## stop GLM-5.1-NVFP4 vLLM — GPU SHUTS DOWN $(date +%H:%M:%S) ########"
stop_gpu; trap - EXIT; kill $DASH_PID 2>/dev/null || true
echo "######## PPR A/B DONE $(date +%H:%M:%S) ########"
$MODAL app list 2>&1 | grep -iE "glm51-nvfp4-vllm.*deployed" && echo "!! WARNING: vLLM still deployed — VERIFY STOPPED" || echo "vLLM not deployed = GPU stopped ✓"
