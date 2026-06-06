#!/usr/bin/env bash
set -euo pipefail

BENCH_ROOT="${BENCH_ROOT:-/srv/semfs-benchmark}"
WB_ROOT="${WB_ROOT:-${BENCH_ROOT}/Workspace-Bench}"
REPO_ROOT="${REPO_ROOT:-${BENCH_ROOT}/semantic-filesystem}"
EVAL_ROOT="${WB_ROOT}/evaluation"
ENV_FILE="${ENV_FILE:-${BENCH_ROOT}/benchmark.env}"
DATASET="${DATASET:-smoke}"
OUTPUT_ROOT="${OUTPUT_ROOT:-${EVAL_ROOT}/output}"
FILESYS_ROOT="${FILESYS_ROOT:-${EVAL_ROOT}/filesys}"
TELEMETRY_ROOT="${TELEMETRY_ROOT:-${OUTPUT_ROOT}/_telemetry}"
RUN_STAMP="${RUN_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
SKIP_PREPARE="${SKIP_PREPARE:-0}"
# SEMFS_FRESH=1 → start every run cold: wipe the semfs local cache DB (forces a
# fresh pull from supermemory) and remove this target's prior output dir (avoids
# the grader's stale-resume skip). Off by default; warm cache is faster.
SEMFS_FRESH="${SEMFS_FRESH:-0}"
SEMFS_CACHE_ROOT="${SEMFS_CACHE_ROOT:-${HOME}/.cache/semfs}"

cleanup_stale_mounts() {
  local target=""
  while IFS= read -r target; do
    [[ -n "${target}" ]] || continue
    log "cleaning stale fuse mount at ${target}"
    fusermount3 -u -z "${target}" >/dev/null 2>&1 || umount -l "${target}" >/dev/null 2>&1 || true
  done < <(
    mount | awk -v root="${FILESYS_ROOT}" '
      $2 == "on" && index($3, root) == 1 && $4 == "type" && $5 == "fuse" { print $3 }
    '
  )
}

# Cold-start a run when SEMFS_FRESH=1: drop the semfs local cache (re-pull from
# supermemory) and remove this target's prior output dir. No-op otherwise.
fresh_clear() {
  [[ "${SEMFS_FRESH}" == "1" ]] || return 0
  local target="$1"

  # 1. semfs local cache DBs. Scope to SEMFS_CONTAINER_TAG when set, else all.
  local cache_glob="*.db"
  [[ -n "${SEMFS_CONTAINER_TAG:-}" ]] && cache_glob="${SEMFS_CONTAINER_TAG}.db"
  if [[ -d "${SEMFS_CACHE_ROOT}" ]]; then
    while IFS= read -r f; do
      [[ -n "${f}" ]] || continue
      log "fresh: rm cache ${f}*"
      rm -f "${f}" "${f}-shm" "${f}-wal"
    done < <(find "${SEMFS_CACHE_ROOT}" -type f -name "${cache_glob}" 2>/dev/null)
  fi

  # 2. prior output for this target's label prefix (anchored — plain labels do
  #    not match the SEMFS* prefixes, so plain runs never delete semfs output).
  local prefix=""
  case "${target}" in
    codex)            prefix="Codex--" ;;
    semfs-codex)      prefix="SEMFSCodex--" ;;
    claudecode)       prefix="ClaudeCode--" ;;
    semfs-claudecode) prefix="SEMFSClaudeCode--" ;;
  esac
  if [[ -n "${prefix}" ]]; then
    local d
    for d in "${OUTPUT_ROOT}/${prefix}"*/; do
      [[ -d "${d}" ]] || continue
      log "fresh: rm output ${d}"
      rm -rf "${d}"
    done
  fi
}

snapshot_workspace() {
  local label="$1"
  local output_file="$2"
  python3 "${REPO_ROOT}/benchmarks/aws/workspace_telemetry.py" \
    snapshot \
    --filesys-root "${FILESYS_ROOT}" \
    --label "${label}" \
    --output "${output_file}"
}

diff_workspace() {
  local before_file="$1"
  local after_file="$2"
  local output_file="$3"
  python3 "${REPO_ROOT}/benchmarks/aws/workspace_telemetry.py" \
    diff \
    --before "${before_file}" \
    --after "${after_file}" \
    --output "${output_file}"
}

narrative_workspace() {
  local telemetry_dir="$1"
  local output_prefix="$2"
  python3 "${REPO_ROOT}/benchmarks/aws/workspace_narrative.py" \
    --output-root "${OUTPUT_ROOT}" \
    --telemetry-dir "${telemetry_dir}" \
    --output-prefix "${output_prefix}"
}

usage() {
  cat <<'EOF'
Usage:
  run_workspace_bench.sh codex
  run_workspace_bench.sh semfs-codex
  run_workspace_bench.sh claudecode
  run_workspace_bench.sh semfs-claudecode

Environment:
  DATASET=smoke|lite|full
  BENCH_ROOT=/srv/semfs-benchmark
  ENV_FILE=/srv/semfs-benchmark/benchmark.env
EOF
}

log() {
  printf '[run] %s\n' "$*"
}

require_env() {
  if [[ ! -f "${ENV_FILE}" ]]; then
    printf 'missing env file: %s\n' "${ENV_FILE}" >&2
    exit 1
  fi
  set -a
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
  set +a
  if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
    printf 'OPENROUTER_API_KEY is required in %s\n' "${ENV_FILE}" >&2
    exit 1
  fi
  export CODEX_SANDBOX_MODE="${CODEX_SANDBOX_MODE:-danger-full-access}"
}

build_config() {
  local harness="$1"
  local model="$2"
  local model_id="$3"
  local model_name="$4"
  local env_prefix="$5"
  local provider_type="$6"

  python3 "${EVAL_ROOT}/scripts/build_run_config.py" \
    --eval-root "${EVAL_ROOT}" \
    --harness "${harness}" \
    --model "${model}" \
    --dataset "${DATASET}" \
    --model-id "${model_id}" \
    --model-name "${model_name}" \
    --env-prefix "${env_prefix}" \
    --provider-type "${provider_type}"
}

prepare_semfs_config() {
  local harness="$1"
  local model="$2"
  local dataset="$3"
  local provider_type="$4"
  local model_id="$5"
  local model_name="$6"
  local env_prefix="$7"
  python3 "${REPO_ROOT}/benchmarks/workspace_bench/setup_workspace_bench_semfs.py" \
    --workspace-bench-root "${WB_ROOT}" \
    --harness "${harness}" \
    --model "${model}" \
    --dataset "${dataset}" \
    --provider-type "${provider_type}" \
    --model-id "${model_id}" \
    --model-name "${model_name}" \
    --env-prefix "${env_prefix}" >/tmp/semfs-setup.log
  awk '/semfs config:/ {print $4}' /tmp/semfs-setup.log | tail -n 1
}

emit_summary() {
  local telemetry_dir="$1"
  local narrative_prefix="$2"
  local timing_json="$3"
  python3 - "${OUTPUT_ROOT}" "${telemetry_dir}" "${narrative_prefix}" "${timing_json}" <<'PY'
import json
import sys
from pathlib import Path

output_root = Path(sys.argv[1])
telemetry_dir = Path(sys.argv[2])
narrative_prefix = Path(sys.argv[3])
timing_json = Path(sys.argv[4])
reports = sorted(output_root.rglob("agent_runner_report.json"), key=lambda p: p.stat().st_mtime)
if not reports:
    raise SystemExit("no agent_runner_report.json found under " + str(output_root))

report_path = reports[-1]
report = json.loads(report_path.read_text())
cases = report.get("cases") if isinstance(report.get("cases"), list) else []
config = report.get("config") if isinstance(report.get("config"), dict) else {}
api_provider = config.get("api_provider") if isinstance(config.get("api_provider"), dict) else {}

enriched_cases = []
for case in cases:
    if not isinstance(case, dict):
        continue
    merged = dict(case)
    output_dir = merged.get("outputDir")
    if isinstance(output_dir, str) and output_dir:
        agent_json_path = Path(output_dir) / "agent.json"
        if agent_json_path.exists():
            try:
                agent_case = json.loads(agent_json_path.read_text())
            except Exception:
                agent_case = None
            if isinstance(agent_case, dict):
                for key in ("promptTokens", "completionTokens", "totalTokens", "turns", "trace"):
                    if merged.get(key) is None and agent_case.get(key) is not None:
                        merged[key] = agent_case.get(key)
                if merged.get("tracePath") is None and agent_case.get("trace") is not None:
                    merged["tracePath"] = str(agent_json_path)
    enriched_cases.append(merged)

def count_status(status: str) -> int:
    return sum(1 for case in enriched_cases if isinstance(case, dict) and case.get("status") == status)

def sum_metric(key: str):
    total = 0
    seen = False
    for case in enriched_cases:
        if isinstance(case, dict) and isinstance(case.get(key), int):
            total += int(case[key])
            seen = True
    return total if seen else None

total = len(enriched_cases)
passed = count_status("passed")
failed = count_status("failed")
error = count_status("error")
timeout = count_status("timeout")
total_duration_ms = sum_metric("durationMs")
timings = json.loads(timing_json.read_text()) if timing_json.exists() else {}

summary = {
    "reportPath": str(report_path),
    "agentId": report.get("agentId"),
    "modelSummary": {
        "modelName": config.get("model_name"),
        "providerType": api_provider.get("provider_type"),
        "modelId": api_provider.get("model"),
        "baseUrl": api_provider.get("baseUrl"),
    },
    "accuracySummary": {
        "total": total,
        "passed": passed,
        "failed": failed,
        "error": error,
        "timeout": timeout,
        "passRate": (passed / total) if total else None,
    },
    "latencySummary": {
        "totalDurationMs": total_duration_ms,
        "avgDurationMs": (total_duration_ms / total) if total and total_duration_ms is not None else None,
    },
    "tokenSummary": {
        "promptTokens": sum_metric("promptTokens"),
        "completionTokens": sum_metric("completionTokens"),
        "totalTokens": sum_metric("totalTokens"),
    },
    "cases": [
        {
            "caseId": case.get("caseId"),
            "status": case.get("status"),
            "durationMs": case.get("durationMs"),
            "promptTokens": case.get("promptTokens"),
            "completionTokens": case.get("completionTokens"),
            "totalTokens": case.get("totalTokens"),
            "tracePath": case.get("tracePath"),
        }
        for case in enriched_cases
        if isinstance(case, dict)
    ],
    "telemetry": {
        "directory": str(telemetry_dir),
        "snapshotBeforePrepare": str(telemetry_dir / "snapshot_before_prepare.json"),
        "snapshotAfterPrepare": str(telemetry_dir / "snapshot_after_prepare.json"),
        "snapshotAfterRun": str(telemetry_dir / "snapshot_after_run.json"),
        "prepareDiff": str(telemetry_dir / "diff_prepare.json"),
        "runDiff": str(telemetry_dir / "diff_run.json"),
        "narrativeJson": str(narrative_prefix.with_suffix(".json")),
        "narrativeMarkdown": str(narrative_prefix.with_suffix(".md")),
    },
    "timingBreakdown": timings,
}

print(json.dumps(summary, indent=2))
PY
}

# Archive per-case command traces into the (persistent) per-run telemetry dir.
# The case output dir (OUTPUT_ROOT/<prefix>*/<case>/raw/) is OVERWRITTEN by every
# subsequent run, so without this the codex_stdout.jsonl / agent.json command
# traces are lost — making cross-run comparison (e.g. cloud vs local tool calls)
# impossible. Copies are cheap and keyed by RUN_STAMP, so every run is preserved.
archive_traces() {
  local telemetry_dir="$1" target="$2"
  local prefix=""
  case "${target}" in
    codex)            prefix="Codex--" ;;
    semfs-codex)      prefix="SEMFSCodex--" ;;
    claudecode)       prefix="ClaudeCode--" ;;
    semfs-claudecode) prefix="SEMFSClaudeCode--" ;;
  esac
  [[ -n "${prefix}" ]] || return 0
  local arch="${telemetry_dir}/traces"
  mkdir -p "${arch}"
  while IFS= read -r aj; do
    [[ -n "${aj}" ]] || continue
    local casedir label
    casedir="$(dirname "${aj}")"
    label="$(basename "$(dirname "${casedir}")")__$(basename "${casedir}")"
    mkdir -p "${arch}/${label}"
    cp -f "${aj}" "${arch}/${label}/agent.json" 2>/dev/null || true
    local f
    for f in codex_stdout.jsonl chat_adapter_log.jsonl codex_invocation.json last_message.txt; do
      [[ -f "${casedir}/raw/${f}" ]] && cp -f "${casedir}/raw/${f}" "${arch}/${label}/${f}" 2>/dev/null || true
    done
  done < <(find "${OUTPUT_ROOT}/${prefix}"*/ -name agent.json 2>/dev/null)
  log "archived traces → ${arch}"
}

main() {
  if [[ $# -ne 1 ]]; then
    usage
    exit 1
  fi
  require_env
  mkdir -p "${OUTPUT_ROOT}"
  cd "${EVAL_ROOT}"
  cleanup_stale_mounts
  fresh_clear "$1"
  local telemetry_dir="${TELEMETRY_ROOT}/${RUN_STAMP}-$1-${DATASET}"
  local narrative_prefix="${telemetry_dir}/run_narrative"
  local timing_json="${telemetry_dir}/timing_breakdown.json"
  mkdir -p "${telemetry_dir}"
  snapshot_workspace before_prepare "${telemetry_dir}/snapshot_before_prepare.json"

  local run_config=""
  case "$1" in
    codex)
      run_config="$(build_config Codex gpt-5.4 openai/gpt-5.4 GPT-5.4 GPT54 openai)"
      ;;
    semfs-codex)
      if [[ -z "${SUPERMEMORY_API_KEY:-}" ]]; then
        printf 'SUPERMEMORY_API_KEY is required for semfs runs\n' >&2
        exit 1
      fi
      run_config="$(prepare_semfs_config codex gpt-5.4 "${DATASET}" openai openai/gpt-5.4 GPT-5.4 GPT54)"
      ;;
    claudecode)
      run_config="$(build_config ClaudeCode claude-sonnet-4.6 anthropic/claude-sonnet-4.6 Claude-Sonnet-4.6 SONNET46 anthropic)"
      ;;
    semfs-claudecode)
      if [[ -z "${SUPERMEMORY_API_KEY:-}" ]]; then
        printf 'SUPERMEMORY_API_KEY is required for semfs runs\n' >&2
        exit 1
      fi
      run_config="$(prepare_semfs_config claudecode claude-sonnet-4.6 "${DATASET}" anthropic anthropic/claude-sonnet-4.6 Claude-Sonnet-4.6 SONNET46)"
      ;;
    *)
      usage
      exit 1
      ;;
  esac

  local prepare_started
  local prepare_finished
  prepare_started="$(date +%s)"
  if [[ "${SKIP_PREPARE}" == "1" ]]; then
    log "skipping workdir prepare step"
    snapshot_workspace after_prepare "${telemetry_dir}/snapshot_after_prepare.json"
  else
    python3 "${EVAL_ROOT}/scripts/prepare_workdirs_for_run.py" --run-config "${run_config}"
    snapshot_workspace after_prepare "${telemetry_dir}/snapshot_after_prepare.json"
  fi
  prepare_finished="$(date +%s)"
  diff_workspace \
    "${telemetry_dir}/snapshot_before_prepare.json" \
    "${telemetry_dir}/snapshot_after_prepare.json" \
    "${telemetry_dir}/diff_prepare.json"

  local run_started
  run_started="$(date +%s)"
  python3 -u "${EVAL_ROOT}/src/agent_runner.py" --run-config "${run_config}"
  local run_finished
  run_finished="$(date +%s)"
  snapshot_workspace after_run "${telemetry_dir}/snapshot_after_run.json"
  diff_workspace \
    "${telemetry_dir}/snapshot_after_prepare.json" \
    "${telemetry_dir}/snapshot_after_run.json" \
    "${telemetry_dir}/diff_run.json"
  python3 - "${timing_json}" "${prepare_started}" "${prepare_finished}" "${run_started}" "${run_finished}" "${SKIP_PREPARE}" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
prepare_started = int(sys.argv[2])
prepare_finished = int(sys.argv[3])
run_started = int(sys.argv[4])
run_finished = int(sys.argv[5])
skip_prepare = sys.argv[6] == "1"
payload = {
    "prepareStartedEpochSec": prepare_started,
    "prepareFinishedEpochSec": prepare_finished,
    "prepareDurationSec": prepare_finished - prepare_started,
    "runStartedEpochSec": run_started,
    "runFinishedEpochSec": run_finished,
    "runDurationSec": run_finished - run_started,
    "skipPrepare": skip_prepare,
}
path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
PY
  narrative_workspace "${telemetry_dir}" "${narrative_prefix}"
  archive_traces "${telemetry_dir}" "$1"

  log "run complete"
  emit_summary "${telemetry_dir}" "${narrative_prefix}" "${timing_json}"
}

main "$@"
