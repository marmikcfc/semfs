#!/usr/bin/env bash
# Complete, contamination-free seed of the ENTIRE chanpin workspace that WAITS for
# the warm/index to finish before unmounting — and does it SAFELY (the hard lessons
# from rcas/2026-06-08-partial-seed-indexing.md + the daemon-race corruption):
#
#   1. `semfs mount` returns "ready" when the daemon answers Ping, NOT when indexing
#      finishes; the warm is NOT resumable (import returns early on AlreadyExists).
#      → poll the chunk count until stable, THEN unmount.
#   2. The real daemon is `semfs daemon-inner` (NOT `semfs mount`, the parent). Killing
#      the parent orphans the daemon; relaunching puts a SECOND writer on the same db →
#      CORRUPTION ("file is not a database"). → GUARD against a pre-existing daemon and
#      NEVER kill/relaunch mid-run.
#   3. A `mode=ro` connection can't read the live WAL db (returns 0/errors). → poll with a
#      normal connection + timeout.
#
# Seed from the CANONICAL workspace `<role>_standard` (what the runner restores the
# agent workdir from), NOT `<role>_seed` (subset, missing .extracted.md sidecars) or
# `<role>_raw` (= standard + node_modules cruft).
#
# Usage: TAG=chanpin-gemma-q4 EMBED=gemma-q4 [SEMFS_EMBED_ONNX_DIR=$HOME/gemma_q4] \
#        SRC_CORPUS=/srv/semfs-benchmark/Workspace-Bench/evaluation/filesys/chanpin_standard \
#        bash seed_complete.sh
set -u
TAG="${TAG:-chanpin-gemma-q4}"
EMBED="${EMBED:-gemma-q4}"
SRC_CORPUS="${SRC_CORPUS:-/srv/semfs-benchmark/Workspace-Bench/evaluation/filesys/chanpin_standard}"
SRC="${SRC:-/srv/semfs-benchmark/${TAG}-src}"
DB="$HOME/.semfs/$TAG.db"; LOG="${LOG:-/tmp/${TAG}.log}"
source "$HOME/.semfs_seed_env" 2>/dev/null || true
export SEMFS_EMBED_MODEL="$EMBED" SEMFS_KG=on SEMFS_NO_PUSH=1 SEMFS_NO_SYNC=1
# gemma-q4 BYO-ONNX needs the model dir (default $HOME/gemma_q4); harmless otherwise.
export SEMFS_EMBED_ONNX_DIR="${SEMFS_EMBED_ONNX_DIR:-$HOME/gemma_q4}"

# GUARD: never start a second daemon for this tag (the corruption cause).
if pgrep -f "bin/semfs daemon-inner --container-tag $TAG" >/dev/null; then
  echo "ABORT: a daemon for $TAG is already running — refusing to start a second writer."; exit 1
fi

rsync -a --delete --exclude node_modules --exclude .git --exclude __pycache__ \
  --exclude '*.semfs-error*' --exclude .venv --exclude model_output "$SRC_CORPUS/" "$SRC/"
N=$(find "$SRC" -type f | wc -l); echo "workspace(copy)=$N embedder=$EMBED START=$(date +%s)"

"$HOME/.local/bin/semfs" unmount "$TAG" 2>/dev/null; sleep 2; rm -f "$DB" "$DB"-wal "$DB"-shm
nohup "$HOME/.local/bin/semfs" mount "$TAG" --path "$SRC" --startup-timeout 7200 > "$LOG" 2>&1 &

# WAL-aware poll (normal connection, NOT mode=ro).
cnt(){ python3 -c "import sqlite3,os
try:
 c=sqlite3.connect(os.path.expanduser('~/.semfs/$TAG.db'),timeout=5)
 print(c.execute('SELECT COUNT(DISTINCT filepath) FROM chunks').fetchone()[0])
except Exception: print(-1)" 2>/dev/null; }
t0=$(date +%s); prev=-2; stable=0; echo "polling for index completion..."
while true; do
  sleep 30; cur=$(cnt); el=$(( $(date +%s)-t0 )); echo "t=${el}s indexed=$cur /$N"
  if [ "$cur" = "$prev" ] && [ "$cur" -gt 100 ]; then stable=$((stable+1)); else stable=0; fi; prev=$cur
  if [ "$stable" -ge 5 ]; then echo "STABLE indexed=$cur"; break; fi   # 150s no growth
  [ "$el" -gt 18000 ] && { echo "CEILING_HIT ${el}s indexed=$cur"; break; }
done
"$HOME/.local/bin/semfs" unmount "$TAG" 2>/dev/null
echo "SEED_DONE tag=$TAG indexed=$cur workspace=$N END=$(date +%s)"
[ "$cur" -lt $(( N*7/10 )) ] && echo "WARN: indexed($cur) < 70% of workspace($N) — still partial"
# Next: rebuild KG (examples/build_kg.rs) over the complete db, then mount once to materialize the projection.
