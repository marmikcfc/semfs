#!/usr/bin/env bash
# Holistic local E2E with fastembed-rs REGISTRY models (auto-downloaded):
#   embed = Snowflake/snowflake-arctic-embed-s (384d), rerank = jina-reranker-v2-base-multilingual.
# Mounts a container, seeds files, exercises basic POSIX ops THROUGH the mount,
# then greps semantically — proving L1 chunk -> L2 embed -> L3 index (vec0+fts5)
# -> search (KNN u BM25 -> RRF) -> L5 rerank, fully local (no cloud embed/rerank).
#
# Usage:  SUPERMEMORY_API_KEY=... bash crates/e2e/phase_local_l1_l5.sh
# First run downloads the models into the fastembed cache (can take a few minutes);
# subsequent runs reuse the cache.
set -uo pipefail

: "${SUPERMEMORY_API_KEY:?set SUPERMEMORY_API_KEY (mount key + cache path)}"

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$REPO/target/debug/semfs"

# Force the local fastembed registry for BOTH embed and rerank (the defaults, set
# explicitly so a stray cloud key in the env can't redirect the test).
export SEMFS_EMBED_BACKEND=local
export SEMFS_RERANK_BACKEND=local
# Keep an LLM key out of the picture so L7 graph extraction doesn't add latency.
unset OPENROUTER_API_KEY OPENAI_API_KEY SEMFS_EMBED_MODEL_DIR 2>/dev/null || true

TAG="local-registry-$(date +%s)"
MNT="$(mktemp -d)/$TAG"
mkdir -p "$MNT"
echo "mount: $MNT  (tag: $TAG)"

cleanup() { "$BIN" unmount "$TAG" --force >/dev/null 2>&1 || true; kill "$DPID" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "building semfs..."; (cd "$REPO/crates" && cargo build -p semfs >/dev/null 2>&1)

echo "== mount (registry local embed+rerank) =="
"$BIN" mount "$TAG" --path "$MNT" --key "$SUPERMEMORY_API_KEY" --no-sync --foreground >/tmp/semfs_holistic.log 2>&1 &
DPID=$!
sleep 6
kill -0 "$DPID" 2>/dev/null || { echo "FAIL: daemon exited"; cat /tmp/semfs_holistic.log; exit 1; }

echo "== POSIX ops through the mount =="
# create
printf '%s\n' "the access token is refreshed by the middleware before each request" > "$MNT/auth.md"
printf '%s\n' "fold the egg whites gently into the batter and bake until golden"     > "$MNT/cooking.md"
printf '%s\n' "scratch file to be removed"                                            > "$MNT/scratch.md"
# mkdir + nested write
mkdir -p "$MNT/notes"
printf '%s\n' "rebase your branch onto main and force-push to update the pull request" > "$MNT/notes/git.md"
# list
echo "-- ls --"; ls -la "$MNT" | awk '{print $1, $NF}'
echo "-- ls notes --"; ls "$MNT/notes"
# read
echo "-- cat auth.md --"; cat "$MNT/auth.md"
# rename (mv)
mv "$MNT/cooking.md" "$MNT/recipe.md"
[ -f "$MNT/recipe.md" ] && echo "mv ok: recipe.md present" || { echo "FAIL: mv"; exit 1; }
[ -f "$MNT/cooking.md" ] && { echo "FAIL: old name still present after mv"; exit 1; } || echo "mv ok: cooking.md gone"
# delete (rm)
rm "$MNT/scratch.md"
[ -f "$MNT/scratch.md" ] && { echo "FAIL: rm"; exit 1; } || echo "rm ok: scratch.md gone"
# stat
stat -f '%N %z bytes' "$MNT/auth.md" 2>/dev/null || stat "$MNT/auth.md"

echo "== poll grep until the local index is searchable (downloads models on first run) =="
OUT=""
for i in $(seq 1 40); do
  sleep 8
  OUT="$( "$BIN" grep "how does login credential renewal work" "$MNT/" 2>/dev/null || true )"
  if echo "$OUT" | grep -q "auth.md"; then break; fi
  echo "  attempt $i: not searchable yet (model download / index lag)"
done
echo "-- grep output --"; echo "$OUT"
echo "-- daemon log tail --"; tail -8 /tmp/semfs_holistic.log

if echo "$OUT" | grep -q "auth.md"; then
  echo "PASS: holistic local registry pipeline (POSIX + semantic grep) works"
  exit 0
else
  echo "FAIL: auth.md not found via local semantic search"
  exit 1
fi
