#!/usr/bin/env bash
# Realistic restart-lifecycle test for the daemon-IPC architecture.
#   <pglite|pgvector>
#
# Mount → write+index → UNMOUNT (kill daemon) → REMOUNT same tag → grep over IPC,
# asserting the CORRECT lifecycle per backend (both run with --ephemeral here):
#
#   - pgvector: data lives in the EXTERNAL Postgres server, independent of the
#               mount/--ephemeral lifecycle. Expectation: the hit SURVIVES the
#               restart (served from the durable server, not re-indexed).
#   - pglite:   with --ephemeral the store is a THROWAWAY temp dir wiped at unmount
#               (Codex fix: ephemeral must not persist, and must not leak across
#               same-tag mounts). Expectation: the hit is GONE after restart —
#               proving the cleanup guard ran and no stale data leaked.
#
# SQLite is intentionally NOT covered: --ephemeral SQLite is in-memory (gone on
# unmount), and PERSISTENT pglite/SQLite need a real org-scoped key (validate_key
# scopes the cache by org) — out of scope for a key-less test.
set -uo pipefail

BACKEND="${1:?usage: phase_ipc_persistence.sh <pglite|pgvector>}"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$REPO/target/debug/semfs"
MOUNT_KEY="${SUPERMEMORY_API_KEY:-sm_placeholder_ipc_test}"

export SEMFS_EMBED_BACKEND=local
export SEMFS_RERANK_BACKEND=local
unset OPENROUTER_API_KEY OPENAI_API_KEY SEMFS_EMBED_MODEL_DIR 2>/dev/null || true

FEATURES="pglite"
case "$BACKEND" in
  pglite)   export SEMFS_STORAGE_BACKEND=pglite; unset SEMFS_PG_URL 2>/dev/null || true ;;
  pgvector) export SEMFS_STORAGE_BACKEND=pgvector
            export SEMFS_PG_URL="${SEMFS_PG_URL:-postgres://postgres@127.0.0.1:5433/semfs}" ;;
  *) echo "persistence test only applies to pglite|pgvector (got: $BACKEND)"; exit 2 ;;
esac

# STABLE tag (no timestamp) so both mount cycles address the same backend state.
TAG="persist-$BACKEND-fixed"
SENTINEL="the access token is refreshed by the middleware before each request"
QUERY="how does login credential renewal work"

# Per-backend expectation after a daemon restart.
case "$BACKEND" in
  pgvector) EXPECT="survive" ;;   # external server is durable across the restart
  pglite)   EXPECT="gone" ;;      # --ephemeral pglite is a throwaway temp dir
esac

mount_cycle() {  # $1 = label ; sets MNT, DPID ; mounts and waits for ready
  local label="$1"
  MNT="$(mktemp -d)/$TAG"
  mkdir -p "$MNT"
  echo "  [$label] mount tag=$TAG mnt=$MNT"
  "$BIN" mount "$TAG" --path "$MNT" --key "$MOUNT_KEY" --ephemeral --no-sync --foreground \
    >/tmp/semfs_persist_${BACKEND}_${label}.log 2>&1 &
  DPID=$!
  local ready=0 i
  for i in $(seq 1 60); do
    kill -0 "$DPID" 2>/dev/null || { echo "  [$label] FAIL: daemon exited"; tail -20 /tmp/semfs_persist_${BACKEND}_${label}.log; exit 1; }
    if mount | grep -q "$TAG"; then ready=1; break; fi
    sleep 5
  done
  [ "$ready" = 1 ] || { echo "  [$label] FAIL: mount never ready"; tail -20 /tmp/semfs_persist_${BACKEND}_${label}.log; exit 1; }
  echo "  [$label] ready (~$((i*5))s)"
}

unmount_cycle() {
  "$BIN" unmount "$TAG" --force >/dev/null 2>&1 || true
  for i in $(seq 1 20); do mount | grep -q "$TAG" || break; sleep 1; done
  kill "$DPID" >/dev/null 2>&1 || true
  wait "$DPID" 2>/dev/null || true
}

final_cleanup(){ "$BIN" unmount "$TAG" --force >/dev/null 2>&1 || true; kill "$DPID" >/dev/null 2>&1 || true; }
trap final_cleanup EXIT

echo "== PERSISTENCE backend=$BACKEND tag=$TAG =="
echo "building (--features $FEATURES)..."; (cd "$REPO" && CARGO_INCREMENTAL=0 cargo build -p semfs --features "$FEATURES" >/dev/null 2>&1) || { echo "build FAIL"; exit 1; }

# ---- CYCLE 1: write + index, then unmount (daemon dies) ----
echo "-- cycle 1: write + index --"
mount_cycle "c1"
printf '%s\n' "$SENTINEL" > "$MNT/auth.md"
sleep 10  # flush -> embed -> index into durable storage
# Confirm it's findable BEFORE the restart (sanity: indexing worked at all).
pre=""
for i in $(seq 1 15); do
  pre="$("$BIN" grep --tag "$TAG" "$QUERY" 2>/dev/null || true)"
  echo "$pre" | grep -q "auth.md\|access token" && break
  sleep 4
done
echo "$pre" | grep -q "auth.md\|access token" || { echo "FAIL: not indexed even before restart"; tail -20 /tmp/semfs_persist_${BACKEND}_c1.log; exit 1; }
echo "  indexed OK before restart"
echo "-- unmount (kill daemon) --"
unmount_cycle
mount | grep -q "$TAG" && { echo "FAIL: still mounted after unmount"; exit 1; }
echo "  daemon down, mount gone"

# ---- CYCLE 2: remount SAME tag, do NOT rewrite, grep over IPC ----
echo "-- cycle 2: remount + grep WITHOUT rewriting (expect: $EXPECT) --"
mount_cycle "c2"
# NOTE: no file is written here. A hit can only come from storage the new daemon
# reopened. For pgvector that's the durable external server (expect survive); for
# ephemeral pglite the temp dir was wiped at unmount (expect gone).
post=""
for i in $(seq 1 20); do
  post="$("$BIN" grep --tag "$TAG" "$QUERY" 2>/dev/null || true)"
  echo "$post" | grep -q "auth.md\|access token" && break
  sleep 4
done
echo "$post"
found=no; echo "$post" | grep -q "auth.md\|access token" && found=yes

if [ "$EXPECT" = survive ]; then
  if [ "$found" = yes ]; then
    echo "PASS [$BACKEND]: index SURVIVED daemon restart (served from durable external store, not re-indexed)"
    exit 0
  fi
  echo "FAIL [$BACKEND]: durable hit did NOT survive restart — persistence broken"
  echo "-- cycle2 daemon log tail --"; tail -20 /tmp/semfs_persist_${BACKEND}_c2.log
  exit 1
else  # EXPECT=gone
  if [ "$found" = no ]; then
    echo "PASS [$BACKEND]: ephemeral data was GONE after restart (throwaway temp dir wiped; no cross-mount leak)"
    exit 0
  fi
  echo "FAIL [$BACKEND]: stale ephemeral data LEAKED across the restart — cleanup guard didn't run"
  echo "-- cycle2 daemon log tail --"; tail -20 /tmp/semfs_persist_${BACKEND}_c2.log
  exit 1
fi
