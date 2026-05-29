#!/usr/bin/env bash
# Phase-1 E2E: mount a container, write a file, grep for it semantically, unmount.
# Proves the SemanticIndex refactor (grep -> Arc<dyn SemanticIndex> -> CloudIndex)
# works end-to-end against the real Supermemory service.
set -euo pipefail

# Requires SUPERMEMORY_API_KEY exported in the environment (a credited account).
REPO="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
: "${SUPERMEMORY_API_KEY:?set SUPERMEMORY_API_KEY in the environment}"

TAG="e2e-phase1-$(date +%s)"
MNT="$(mktemp -d)/$TAG"
BIN="$REPO/target/debug/semfs"

cleanup() { "$BIN" unmount "$TAG" --force >/dev/null 2>&1 || true; }
trap cleanup EXIT

"$BIN" mount "$TAG" --path "$MNT" --key "$SUPERMEMORY_API_KEY"  # returns when mounted
echo "the access token is refreshed by the auth middleware before each request" \
  > "$MNT/auth-notes.md"

# Server-side has TWO async stages: document processing (status -> done) AND
# search-index propagation. `done` fires first; the index lags it. So the only
# trustworthy readiness signal for a search assertion is the search returning
# the row. Poll grep itself (the thing under test) until it finds the file,
# up to ~5 min (free-plan processing + index lag can stack).
# Run grep the way an agent does: from INSIDE the mount, so the container tag
# resolves from the .semfs marker in cwd. (Running `grep <query> "$MNT/"` from
# outside the mount fails with "No container tag found" — the path arg does NOT
# carry the tag.)
OUT=""
for i in $(seq 1 30); do
  sleep 10
  OUT="$( cd "$MNT" && "$BIN" grep "how does login credential renewal work" 2>/dev/null || true )"
  if echo "$OUT" | grep -q "auth-notes.md"; then break; fi
  echo "attempt $i: not searchable yet (free-plan processing + index lag)"
done

echo "--- grep output ---"; echo "$OUT"
if echo "$OUT" | grep -q "auth-notes.md"; then
  echo "PASS: found via semantic search"
  exit 0
else
  echo "INCONCLUSIVE: not found within ~120s."
  echo "  - HTTP 402 on push => account out of SuperRAG credits (use a credited key)."
  echo "  - otherwise => indexing still in flight; increase the poll window."
  exit 1
fi
