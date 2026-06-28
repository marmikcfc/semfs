#!/usr/bin/env bash
# Sync remaining semfs artifacts to Google Drive (semfs/ folder).
# Prereq: an rclone remote named "gdrive" pointing at your Google Drive.
#   rclone config create gdrive drive   # (opens a browser to authorize)
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

REMOTE="${RCLONE_REMOTE:-gdrive}"

echo "==> Architecture HTML  -> semfs/docs"
rclone copy   docs                                  "$REMOTE:semfs/docs"        --include '*.html' -P

echo "==> Research HTML      -> semfs/research"
rclone copy   research/ai-prediction-market-trading "$REMOTE:semfs/research"    --include '*.html' -P

echo "==> OpenRouter logs    -> semfs/logs"
rclone copyto openrouter_logs.csv                   "$REMOTE:semfs/logs/openrouter_logs.csv" -P

echo "==> Ticket HTML reports -> semfs/experiments/ticket-html"
rclone copy   tickets                               "$REMOTE:semfs/experiments/ticket-html" --include '*.html' -P

echo "==> 70 MB matrix bundle -> semfs/experiments"
rclone copyto tickets/workspace-bench-5arm-matrix/artifacts/matrix_artifacts_FULL.tgz \
              "$REMOTE:semfs/experiments/matrix_artifacts_FULL.tgz" -P

echo "==> DONE. Tree:"
rclone tree "$REMOTE:semfs" --dirsfirst 2>/dev/null || rclone lsd "$REMOTE:semfs"
