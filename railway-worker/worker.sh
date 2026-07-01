#!/usr/bin/env bash
set -euo pipefail

ROOT="${WORKSPACE_BENCH_ROOT:-/opt/Workspace-Bench}"
EVAL_ROOT="${WORKSPACE_BENCH_EVAL_ROOT:-$ROOT/evaluation}"
MODE="${BENCHMARK_MODE:-idle}"
HARNESS="${BENCHMARK_HARNESS:-codex}"
MODEL="${BENCHMARK_MODEL:-kimi-k2.5}"
MODEL_ID="${BENCHMARK_MODEL_ID:-}"
MODEL_NAME="${BENCHMARK_MODEL_NAME:-}"
ENV_PREFIX="${BENCHMARK_ENV_PREFIX:-}"
PROVIDER_TYPE="${BENCHMARK_PROVIDER_TYPE:-openai}"
DATASET="${BENCHMARK_DATASET:-smoke}"
RESULTS_ROOT="${BENCHMARK_RESULTS_ROOT:-/data/workspace-bench-results}"
IDLE_SLEEP_SECONDS="${IDLE_SLEEP_SECONDS:-3600}"
BOOTSTRAP_STAMP="${EVAL_ROOT}/.railway-bootstrap.done"

log() {
  printf '[worker] %s\n' "$*"
}

keep_alive() {
  log "entering idle loop"
  while true; do
    sleep "${IDLE_SLEEP_SECONDS}"
  done
}

ensure_bootstrap() {
  if [[ -f "${BOOTSTRAP_STAMP}" ]]; then
    log "bootstrap already completed"
    return
  fi

  log "installing Workspace-Bench python and node dependencies"
  python3 -m pip install -q --break-system-packages -e "${ROOT}/deepagents/libs/deepagents"
  python3 -m pip install -q --break-system-packages -e "${ROOT}/deepagents/libs/cli"
  npm install --prefix "${EVAL_ROOT}"
  npm install --prefix "${EVAL_ROOT}/baselines"
  touch "${BOOTSTRAP_STAMP}"
}

download_assets() {
  log "downloading assets for dataset=${DATASET}"
  cd "${EVAL_ROOT}"
  case "${DATASET}" in
    smoke|lite)
      python3 scripts/download_hf_assets.py --lite --workspaces
      ;;
    full)
      python3 scripts/download_hf_assets.py --full --workspaces
      ;;
    *)
      log "unsupported dataset: ${DATASET}"
      return 2
      ;;
  esac
}

configure_openrouter_env() {
  if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
    return
  fi

  export GPT54_API_KEY="${GPT54_API_KEY:-$OPENROUTER_API_KEY}"
  export GPT54_BASE_URL="${GPT54_BASE_URL:-https://openrouter.ai/api/v1}"
  export SONNET46_API_KEY="${SONNET46_API_KEY:-$OPENROUTER_API_KEY}"
  export SONNET46_BASE_URL="${SONNET46_BASE_URL:-https://openrouter.ai/api/v1}"
  export SONNET46_ANTHROPIC_BASE_URL="${SONNET46_ANTHROPIC_BASE_URL:-https://openrouter.ai/api}"
  export SONNET46_ANTHROPIC_MODEL="${SONNET46_ANTHROPIC_MODEL:-anthropic/claude-sonnet-4.6}"
}

prepare_run_config() {
  local dataset_flag
  if [[ "${DATASET}" == "smoke" ]]; then
    dataset_flag="smoke"
  elif [[ "${DATASET}" == "lite" ]]; then
    dataset_flag="lite"
  else
    dataset_flag="full"
  fi

  local args=(
    scripts/build_run_config.py
    --eval-root "${EVAL_ROOT}" \
    --harness "${HARNESS}" \
    --model "${MODEL}" \
    --dataset "${dataset_flag}" \
    --provider-type "${PROVIDER_TYPE}"
  )
  if [[ -n "${MODEL_ID}" ]]; then
    args+=(--model-id "${MODEL_ID}")
  fi
  if [[ -n "${MODEL_NAME}" ]]; then
    args+=(--model-name "${MODEL_NAME}")
  fi
  if [[ -n "${ENV_PREFIX}" ]]; then
    args+=(--env-prefix "${ENV_PREFIX}")
  fi

  cd "${EVAL_ROOT}"
  python3 "${args[@]}"
}

copy_results() {
  if [[ ! -d /data ]]; then
    log "no persistent /data volume mounted; skipping result archive"
    return
  fi

  mkdir -p "${RESULTS_ROOT}"
  local stamp
  stamp="$(date +%Y%m%d-%H%M%S)"
  local dest="${RESULTS_ROOT}/${HARNESS}-${MODEL}-${DATASET}-${stamp}"
  mkdir -p "${dest}"
  cp -R "${EVAL_ROOT}/output/." "${dest}/"
  log "copied results to ${dest}"
}

run_once() {
  if [[ "${BENCHMARK_USE_MOUNTED_SEMFS:-0}" == "1" ]]; then
    log "mounted semfs is not supported on Railway containers: /dev/fuse and SYS_ADMIN are unavailable"
    return 2
  fi

  ensure_bootstrap
  configure_openrouter_env
  download_assets

  local run_config
  run_config="$(prepare_run_config)"
  log "using run config ${run_config}"

  cd "${EVAL_ROOT}"
  python3 scripts/prepare_workdirs_for_run.py --run-config "${run_config}"
  python3 -u src/agent_runner.py --run-config "${run_config}"

  log "agent run completed; reports:"
  while IFS= read -r report; do
    log "report=${report}"
    printf '[worker-json] '
    python3 - "${report}" <<'PY'
import json
import sys

report_path = sys.argv[1]
with open(report_path, "r", encoding="utf-8") as f:
    report = json.load(f)

cases = report.get("cases") if isinstance(report.get("cases"), list) else []
config = report.get("config") if isinstance(report.get("config"), dict) else {}
api_provider = config.get("api_provider") if isinstance(config.get("api_provider"), dict) else {}

def sum_metric(key: str):
    total = 0
    seen = False
    for case in cases:
        if isinstance(case, dict) and isinstance(case.get(key), int):
            total += int(case[key])
            seen = True
    return total if seen else None

report["modelSummary"] = {
    "agentId": report.get("agentId"),
    "modelName": config.get("model_name"),
    "providerType": api_provider.get("provider_type"),
    "modelId": api_provider.get("model"),
    "baseUrl": api_provider.get("baseUrl"),
}
report["tokenSummary"] = {
    "promptTokens": sum_metric("promptTokens"),
    "completionTokens": sum_metric("completionTokens"),
    "totalTokens": sum_metric("totalTokens"),
}

print(json.dumps(report, ensure_ascii=False))
PY
  done < <(find "${EVAL_ROOT}/output" -name agent_runner_report.json -print | sort)
  copy_results
}

main() {
  log "mode=${MODE} harness=${HARNESS} model=${MODEL} model_id=${MODEL_ID:-<default>} dataset=${DATASET}"
  case "${MODE}" in
    idle)
      keep_alive
      ;;
    run-once)
      run_once
      keep_alive
      ;;
    *)
      log "unsupported BENCHMARK_MODE=${MODE}"
      return 2
      ;;
  esac
}

main "$@"
