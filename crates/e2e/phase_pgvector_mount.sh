#!/usr/bin/env bash
# Holistic mount e2e for the POSTGRES/pgvector storage backend.
#   embed  = local fastembed Snowflake/arctic-embed-s (384d)   [SEMFS_EMBED_BACKEND=local]
#   rerank = local fastembed jina-reranker-v2 int8             [SEMFS_RERANK_BACKEND=local]
#   store  = Postgres + pgvector                               [SEMFS_STORAGE_BACKEND=pgvector]
#
# Proves the previously-CLI-unreachable `local + pgvector` row works end-to-end:
# the daemon (writer) indexes file writes INTO Postgres, and `grep` (a separate
# process / reader) finds them via pgvector — through a real mount, with POSIX ops.
#
# Requires:
#   - a running Postgres with the `vector` extension and SEMFS_PG_URL reachable,
#   - the binary built WITH the `pg` feature,
#   - SUPERMEMORY_API_KEY (mount key / cloud fallback).
set -uo pipefail

: "${SEMFS_PG_URL:?set SEMFS_PG_URL (e.g. postgres://postgres@127.0.0.1:5433/semfs)}"
# `--ephemeral` makes key validation best-effort + uses an in-memory metadata
# cache, so NO real Supermemory key is needed — the vectors still go to Postgres.
# (Ephemeral mounts write no `.semfs` marker, so grep below uses explicit --tag.)
MOUNT_KEY="${SUPERMEMORY_API_KEY:-sm_placeholder_pgvector_test}"

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$REPO/target/debug/semfs"

export SEMFS_STORAGE_BACKEND=pgvector
export SEMFS_EMBED_BACKEND=local
export SEMFS_RERANK_BACKEND=local
unset OPENROUTER_API_KEY OPENAI_API_KEY SEMFS_EMBED_MODEL_DIR 2>/dev/null || true

TAG="pgvector-$(date +%s)"
MNT="$(mktemp -d)/$TAG"
mkdir -p "$MNT"
echo "mount: $MNT  (tag: $TAG)  store: $SEMFS_PG_URL"

cleanup() { "$BIN" unmount "$TAG" --force >/dev/null 2>&1 || true; kill "$DPID" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "building semfs --features pg..."
(cd "$REPO" && cargo build -p semfs --features pg >/dev/null 2>&1) || { echo "build FAIL"; exit 1; }

echo "== mount (pgvector storage) =="
"$BIN" mount "$TAG" --path "$MNT" --key "$MOUNT_KEY" --ephemeral --no-sync --foreground >/tmp/semfs_pg.log 2>&1 &
DPID=$!
echo "waiting for mount to become ready..."
ready=0
for i in $(seq 1 60); do
  kill -0 "$DPID" 2>/dev/null || { echo "FAIL: daemon exited"; tail -20 /tmp/semfs_pg.log; exit 1; }
  if mount | grep -q "$TAG"; then ready=1; break; fi
  sleep 5
done
[ "$ready" = 1 ] || { echo "FAIL: mount never ready"; tail -20 /tmp/semfs_pg.log; exit 1; }
echo "mount ready after ~$((i*5))s"
grep -q "storage backend: pgvector" /tmp/semfs_pg.log && echo "daemon confirms: pgvector storage" || echo "WARN: pgvector backend line not seen in daemon log"

echo "== POSIX ops through the mount =="
printf '%s\n' "the access token is refreshed by the middleware before each request" > "$MNT/auth.md"
printf '%s\n' "fold the egg whites gently into the batter and bake until golden"     > "$MNT/cooking.md"
printf '%s\n' "scratch to be removed"                                                 > "$MNT/scratch.md"
mkdir -p "$MNT/notes"
printf '%s\n' "rebase your branch onto main and force-push to update the pull request" > "$MNT/notes/git.md"
echo "-- ls --"; ls -la "$MNT" | awk '{print $1, $NF}'
echo "-- cat auth.md --"; cat "$MNT/auth.md"
mv "$MNT/cooking.md" "$MNT/recipe.md"
[ -f "$MNT/recipe.md" ] && [ ! -f "$MNT/cooking.md" ] && echo "mv ok" || { echo "FAIL: mv"; exit 1; }
rm "$MNT/scratch.md"
[ ! -f "$MNT/scratch.md" ] && echo "rm ok" || { echo "FAIL: rm"; exit 1; }
sleep 6  # flush -> embed -> write into Postgres

echo "== grep (separate process -> reads pgvector) =="
OUT=""
for i in $(seq 1 20); do
  OUT="$( "$BIN" grep --tag "$TAG" "how does login credential renewal work" 2>/dev/null || true )"
  echo "$OUT" | grep -q "auth.md" && break
  echo "  attempt $i: not yet"; sleep 4
done
echo "-- grep output --"; echo "$OUT"
echo "-- rows in Postgres chunks table --"
/opt/homebrew/opt/postgresql@17/bin/psql "$SEMFS_PG_URL" -tAc "SELECT count(*), count(DISTINCT filepath) FROM chunks;" 2>/dev/null || true
echo "-- daemon log tail --"; tail -6 /tmp/semfs_pg.log

if echo "$OUT" | grep -q "auth.md"; then
  echo "PASS: local-embed + PGVECTOR works end-to-end through a real mount (POSIX + grep)"
  exit 0
else
  echo "FAIL: auth.md not found via pgvector-backed grep"
  exit 1
fi
