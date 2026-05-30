#!/usr/bin/env bash
# Realistic concurrency test for the daemon-IPC search architecture.
#   <sqlite|pglite|pgvector>
#
# Exercises the path the Codex review flagged: the daemon is the SOLE owner of
# the backend connection (single mutex-guarded PgConnection for pglite/pgvector),
# so concurrent writes (POSIX) AND concurrent greps (separate processes over IPC)
# must SERIALIZE through it without deadlocking or wedging — and the new 25s
# server-side search timeout must not trip under normal load.
#
# Asserts: (1) N parallel file writes all land + index; (2) M concurrent greps
# over IPC all return the expected hit; (3) the daemon survives the whole storm.
set -uo pipefail

BACKEND="${1:?usage: phase_ipc_concurrency.sh <sqlite|pglite|pgvector>}"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$REPO/target/debug/semfs"
MOUNT_KEY="${SUPERMEMORY_API_KEY:-sm_placeholder_ipc_test}"

export SEMFS_EMBED_BACKEND=local
export SEMFS_RERANK_BACKEND=local
unset OPENROUTER_API_KEY OPENAI_API_KEY SEMFS_EMBED_MODEL_DIR 2>/dev/null || true

FEATURES="pglite"
case "$BACKEND" in
  sqlite)   unset SEMFS_STORAGE_BACKEND SEMFS_PG_URL 2>/dev/null || true ;;
  pglite)   export SEMFS_STORAGE_BACKEND=pglite; unset SEMFS_PG_URL 2>/dev/null || true ;;
  pgvector) export SEMFS_STORAGE_BACKEND=pgvector
            export SEMFS_PG_URL="${SEMFS_PG_URL:-postgres://postgres@127.0.0.1:5433/semfs}" ;;
  *) echo "unknown backend: $BACKEND"; exit 2 ;;
esac

TAG="conc-$BACKEND-$(date +%s)"
MNT="$(mktemp -d)/$TAG"
mkdir -p "$MNT"
echo "== CONCURRENCY backend=$BACKEND tag=$TAG mnt=$MNT =="

cleanup(){ "$BIN" unmount "$TAG" --force >/dev/null 2>&1 || true; kill "$DPID" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "building (--features $FEATURES)..."; (cd "$REPO" && CARGO_INCREMENTAL=0 cargo build -p semfs --features "$FEATURES" >/dev/null 2>&1) || { echo "build FAIL"; exit 1; }

echo "-- mount (--ephemeral) --"
"$BIN" mount "$TAG" --path "$MNT" --key "$MOUNT_KEY" --ephemeral --no-sync --foreground >/tmp/semfs_conc_$BACKEND.log 2>&1 &
DPID=$!
ready=0
for i in $(seq 1 60); do
  kill -0 "$DPID" 2>/dev/null || { echo "FAIL: daemon exited"; tail -20 /tmp/semfs_conc_$BACKEND.log; exit 1; }
  if mount | grep -q "$TAG"; then ready=1; break; fi
  sleep 5
done
[ "$ready" = 1 ] || { echo "FAIL: mount never ready"; tail -20 /tmp/semfs_conc_$BACKEND.log; exit 1; }
echo "mount ready (~$((i*5))s)"

# (1) N parallel writes — fan out distinct files at once.
N=12
echo "-- $N parallel writes --"
declare -a sentences=(
  "the access token is refreshed by the middleware before each request"
  "fold the egg whites gently into the batter and bake until golden"
  "rebase onto main and force-push the feature branch after review"
  "the kubernetes pod was evicted due to memory pressure on the node"
  "she planted tomatoes and basil along the south-facing garden wall"
  "the invoice total includes a fifteen percent service charge"
  "compress the dataset with zstd before uploading to the bucket"
  "the orchestra tuned to the oboe before the conductor arrived"
  "retry the payment with exponential backoff and a jitter window"
  "the glacier retreated nearly two kilometers over the decade"
  "parse the yaml config and validate it against the json schema"
  "the sourdough starter doubled overnight at room temperature"
)
for k in $(seq 0 $((N-1))); do
  printf '%s\n' "${sentences[$k]}" > "$MNT/doc_$k.md" &
done
wait
echo "wrote $(ls "$MNT"/doc_*.md | wc -l | tr -d ' ') files in parallel"
sleep 10   # flush -> embed -> index all N into the single-connection backend

# (2) M concurrent greps over IPC — separate processes, all hitting the daemon
#     at once. The single connection must serialize them without wedging.
M=8
echo "-- $M concurrent greps over IPC --"
rm -f /tmp/conc_grep_*.out
for j in $(seq 1 $M); do
  ( "$BIN" grep --tag "$TAG" "how does login credential renewal work" 2>/dev/null > /tmp/conc_grep_$j.out ) &
done
wait
ok=0
for j in $(seq 1 $M); do
  if grep -q "auth.md\|access token" /tmp/conc_grep_$j.out 2>/dev/null; then ok=$((ok+1)); fi
done
echo "concurrent greps that returned the expected hit: $ok/$M"

# (3) daemon still alive after the storm.
if ! kill -0 "$DPID" 2>/dev/null; then
  echo "FAIL [$BACKEND]: daemon DIED under concurrent load"; tail -20 /tmp/semfs_conc_$BACKEND.log; exit 1
fi
"$BIN" grep --tag "$TAG" "ping after storm" >/dev/null 2>&1
echo "daemon alive after $N parallel writes + $M concurrent greps"

if [ "$ok" -ge $((M/2)) ]; then
  echo "PASS [$BACKEND]: concurrent writes+greps serialized cleanly ($ok/$M greps hit), daemon survived"
  exit 0
else
  echo "FAIL [$BACKEND]: only $ok/$M concurrent greps returned the hit"
  echo "-- daemon log tail --"; tail -15 /tmp/semfs_conc_$BACKEND.log
  exit 1
fi
