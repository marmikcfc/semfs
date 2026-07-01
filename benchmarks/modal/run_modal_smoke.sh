#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

RUN_BENCHMARK="${RUN_BENCHMARK:-1}"

echo "== Modal CLI =="
modal --version
modal profile current

echo "== Modal API reachability =="
python3 - <<'PY'
import socket
host = "api.modal.com"
addrs = socket.getaddrinfo(host, 443, proto=socket.IPPROTO_TCP)
print(f"{host}: {addrs[0][4][0]}:{addrs[0][4][1]}")
PY

curl -Is --connect-timeout 8 https://api.modal.com >/tmp/modal_api_headers.txt || {
  echo "curl to https://api.modal.com failed"
  cat /tmp/modal_api_headers.txt 2>/dev/null || true
  exit 1
}
sed -n '1,8p' /tmp/modal_api_headers.txt

echo "== Minimal Modal + Volume smoke =="
modal run benchmarks/modal/modal_min_smoke.py::smoke

if [[ "$RUN_BENCHMARK" == "1" ]]; then
  echo "== E9w2-shaped benchmark smoke =="
  modal run benchmarks/modal/semfs_modal.py::e9w2_smoke
else
  echo "Skipping benchmark smoke because RUN_BENCHMARK=$RUN_BENCHMARK"
fi
