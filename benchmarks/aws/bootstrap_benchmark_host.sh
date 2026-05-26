#!/usr/bin/env bash
set -euo pipefail

USER_NAME="${SUDO_USER:-${USER}}"
USER_HOME="$(eval echo "~${USER_NAME}")"
BENCH_ROOT="${BENCH_ROOT:-/srv/semfs-benchmark}"
WB_ROOT="${WB_ROOT:-${BENCH_ROOT}/Workspace-Bench}"
REPO_ROOT="${REPO_ROOT:-${BENCH_ROOT}/semantic-filesystem}"
EVAL_ROOT="${WB_ROOT}/evaluation"
ENV_FILE="${ENV_FILE:-${BENCH_ROOT}/benchmark.env}"

log() {
  printf '[bootstrap] %s\n' "$*"
}

run_as_user() {
  sudo -u "${USER_NAME}" -H bash -lc "$1"
}

install_system_packages() {
  log "installing system packages"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends \
    bash \
    build-essential \
    ca-certificates \
    curl \
    fuse3 \
    git \
    jq \
    pkg-config \
    python3 \
    python3-pip \
    python3-venv \
    python-is-python3 \
    unzip \
    wget
}

install_node() {
  if command -v node >/dev/null 2>&1 && command -v npm >/dev/null 2>&1; then
    log "node already installed"
    return
  fi
  log "installing node 24"
  curl -fsSL https://deb.nodesource.com/setup_24.x | bash -
  apt-get install -y nodejs
}

install_codex() {
  if command -v codex >/dev/null 2>&1; then
    log "codex already installed"
    return
  fi
  log "installing codex cli"
  npm install -g @openai/codex@0.133.0
}

install_semfs() {
  if command -v semfs >/dev/null 2>&1; then
    log "semfs already installed"
    return
  fi
  log "installing semfs"
  run_as_user 'curl -fsSL https://semfs.ai/install | bash'
}

prepare_dirs() {
  log "preparing directories under ${BENCH_ROOT}"
  mkdir -p "${BENCH_ROOT}"
  chown -R "${USER_NAME}:${USER_NAME}" "${BENCH_ROOT}"
}

sync_workspace_bench_source() {
  local vendored_root="${REPO_ROOT}/benchmarks/vendor/Workspace-Bench"
  if [[ ! -d "${vendored_root}/evaluation" ]]; then
    printf 'vendored Workspace-Bench not found: %s\n' "${vendored_root}" >&2
    exit 1
  fi
  log "syncing vendored workspace-bench source"
  run_as_user "mkdir -p '${WB_ROOT}'"
  run_as_user "cd '${vendored_root}' && tar cf - . | (cd '${WB_ROOT}' && tar xf -)"
}

install_workspace_bench_deps() {
  log "installing workspace-bench dependencies"
  run_as_user "python3 -m pip install --break-system-packages -e '${WB_ROOT}/deepagents/libs/deepagents'"
  run_as_user "python3 -m pip install --break-system-packages -e '${WB_ROOT}/deepagents/libs/cli'"
  run_as_user "python3 -m pip install --break-system-packages pyyaml huggingface_hub tqdm uv"
  run_as_user "npm install --prefix '${EVAL_ROOT}'"
  run_as_user "npm install --prefix '${EVAL_ROOT}/baselines'"
}

install_semfs_adapters() {
  log "installing local semfs benchmark adapters"
  run_as_user "python3 '${REPO_ROOT}/benchmarks/workspace_bench/setup_workspace_bench_semfs.py' --workspace-bench-root '${WB_ROOT}' --harness codex --model gpt-5.4 --dataset smoke --model-id openai/gpt-5.4 --model-name GPT-5.4 --env-prefix GPT54 --provider-type openai >/tmp/setup-codex.log"
  run_as_user "python3 '${REPO_ROOT}/benchmarks/workspace_bench/setup_workspace_bench_semfs.py' --workspace-bench-root '${WB_ROOT}' --harness claudecode --model claude-sonnet-4.6 --dataset smoke --model-id anthropic/claude-sonnet-4.6 --model-name Claude-Sonnet-4.6 --env-prefix SONNET46 --provider-type anthropic >/tmp/setup-claude.log"
}

download_datasets() {
  log "downloading shared workspace-bench datasets"
  run_as_user "set -a; [[ -f '${ENV_FILE}' ]] && source '${ENV_FILE}'; set +a; cd '${EVAL_ROOT}' && python3 scripts/download_hf_assets.py --lite --full --workspaces"
}

write_env_template() {
  if [[ -f "${ENV_FILE}" ]]; then
    log "env file already exists at ${ENV_FILE}"
    return
  fi
  log "writing benchmark env template"
  cat > "${ENV_FILE}" <<'EOF'
OPENROUTER_API_KEY=
SUPERMEMORY_API_KEY=
HF_TOKEN=
GPT54_BASE_URL=https://openrouter.ai/api/v1
GPT54_API_KEY=${OPENROUTER_API_KEY}
SONNET46_BASE_URL=https://openrouter.ai/api/v1
SONNET46_API_KEY=${OPENROUTER_API_KEY}
SONNET46_ANTHROPIC_BASE_URL=https://openrouter.ai/api
SONNET46_ANTHROPIC_MODEL=anthropic/claude-sonnet-4.6
SEMFS_CONTAINER_PREFIX=workspace-bench
SEMFS_MOUNT_TIMEOUT_SEC=120
SEMFS_UNMOUNT_TIMEOUT_SEC=60
CODEX_SANDBOX_MODE=danger-full-access
EOF
  chown "${USER_NAME}:${USER_NAME}" "${ENV_FILE}"
  chmod 600 "${ENV_FILE}"
}

main() {
  prepare_dirs
  install_system_packages
  install_node
  install_codex
  install_semfs
  sync_workspace_bench_source
  install_workspace_bench_deps
  install_semfs_adapters
  write_env_template
  download_datasets
  log "bootstrap complete"
}

main "$@"
