#!/usr/bin/env bash
# Mount + POSIX + grep(over IPC) for ONE backend. Run with: <backend>
#   sqlite    — default embedded SQLite (vec0+fts5)
#   pglite    — embedded pglite (in-process Postgres+pgvector, shipped in-box)
#   pgvector  — external Postgres (SEMFS_PG_URL)
#
# Exercises the daemon-IPC architecture: the daemon owns the index connection,
# and `semfs grep` (a separate process) searches THROUGH the daemon over IPC —
# the one path that works for all three backends. Uses --ephemeral so no
# Supermemory key is needed (grep no longer reads a persisted file; it asks the
# daemon, which holds the live index even when the metadata cache is in-memory).
set -uo pipefail

BACKEND="${1:?usage: phase_ipc_backends.sh <sqlite|pglite|pgvector>}"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$REPO/target/debug/semfs"
MOUNT_KEY="${SUPERMEMORY_API_KEY:-sm_placeholder_ipc_test}"

export SEMFS_EMBED_BACKEND=local
export SEMFS_RERANK_BACKEND=local
unset OPENROUTER_API_KEY OPENAI_API_KEY SEMFS_EMBED_MODEL_DIR 2>/dev/null || true

FEATURES="pglite"   # one binary covers all three backends (pglite ⊇ pg ⊇ default)
case "$BACKEND" in
  sqlite)   unset SEMFS_STORAGE_BACKEND SEMFS_PG_URL 2>/dev/null || true ;;
  pglite)   export SEMFS_STORAGE_BACKEND=pglite; unset SEMFS_PG_URL 2>/dev/null || true ;;
  pgvector) export SEMFS_STORAGE_BACKEND=pgvector
            export SEMFS_PG_URL="${SEMFS_PG_URL:-postgres://postgres@127.0.0.1:5433/semfs}" ;;
  *) echo "unknown backend: $BACKEND"; exit 2 ;;
esac

TAG="ipc-$BACKEND-$(date +%s)"
MNT="$(mktemp -d)/$TAG"
mkdir -p "$MNT"
echo "== backend=$BACKEND tag=$TAG mnt=$MNT =="

cleanup(){ "$BIN" unmount "$TAG" --force >/dev/null 2>&1 || true; kill "$DPID" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "building (--features $FEATURES)..."; (cd "$REPO" && CARGO_INCREMENTAL=0 cargo build -p semfs --features "$FEATURES" >/dev/null 2>&1) || { echo "build FAIL"; exit 1; }

echo "-- mount (--ephemeral) --"
"$BIN" mount "$TAG" --path "$MNT" --key "$MOUNT_KEY" --ephemeral --no-sync --foreground >/tmp/semfs_ipc_$BACKEND.log 2>&1 &
DPID=$!
ready=0
for i in $(seq 1 60); do
  kill -0 "$DPID" 2>/dev/null || { echo "FAIL: daemon exited"; tail -20 /tmp/semfs_ipc_$BACKEND.log; exit 1; }
  if mount | grep -q "$TAG"; then ready=1; break; fi
  sleep 5
done
[ "$ready" = 1 ] || { echo "FAIL: mount never ready"; tail -20 /tmp/semfs_ipc_$BACKEND.log; exit 1; }
echo "mount ready (~$((i*5))s); daemon backend line:"; grep -E "storage backend|local semantic index" /tmp/semfs_ipc_$BACKEND.log | head -2

echo "-- POSIX ops --"
printf '%s\n' "the access token is refreshed by the middleware before each request" > "$MNT/auth.md"
printf '%s\n' "fold the egg whites gently into the batter and bake until golden"     > "$MNT/cooking.md"
printf '%s\n' "scratch to delete" > "$MNT/scratch.md"
mkdir -p "$MNT/notes"; printf '%s\n' "rebase onto main and force-push the branch" > "$MNT/notes/git.md"
ls -la "$MNT" | awk '{print $1,$NF}'
cat "$MNT/auth.md" >/dev/null && echo "cat ok"
mv "$MNT/cooking.md" "$MNT/recipe.md"; [ -f "$MNT/recipe.md" ] && [ ! -f "$MNT/cooking.md" ] && echo "mv ok" || { echo "FAIL mv"; exit 1; }
rm "$MNT/scratch.md"; [ ! -f "$MNT/scratch.md" ] && echo "rm ok" || { echo "FAIL rm"; exit 1; }
sleep 6   # flush -> embed -> index (into whichever backend the daemon owns)

echo "-- grep (separate process -> daemon IPC -> $BACKEND) --"
OUT=""
for i in $(seq 1 20); do
  OUT="$("$BIN" grep --tag "$TAG" "how does login credential renewal work" 2>/dev/null || true)"
  echo "$OUT" | grep -q "auth.md" && break
  sleep 4
done
echo "$OUT"
if echo "$OUT" | grep -q "auth.md"; then
  echo "PASS [$BACKEND]: mount + POSIX + grep-over-IPC works"
  exit 0
else
  echo "FAIL [$BACKEND]: auth.md not found via grep-over-IPC"
  echo "-- daemon log tail --"; tail -10 /tmp/semfs_ipc_$BACKEND.log
  exit 1
fi
