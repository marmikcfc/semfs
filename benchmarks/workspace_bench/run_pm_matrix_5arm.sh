#!/usr/bin/env bash
# 5-arm x 5-case matrix (serial). Per case: plain preps the workdir (skip=0),
# the 4 semfs arms reuse it (skip=1). Cloud last (shadows the workdir anyway).
set -u
CASES="15 44 95 175 289"
export MATRIX_RESULTS=/tmp/pm_results_5arm.jsonl
export MATRIX_ART=/srv/semfs-benchmark/matrix_artifacts/run5arm
mkdir -p "$MATRIX_ART"
echo "=== MATRIX-5ARM START $(date -u +%H:%M:%S) ==="
for c in $CASES; do
  for spec in "plain 0" "nokg 1" "gfs_off 1" "gfs_on 1" "cloud 1"; do
    set -- $spec; arm=$1; skip=$2
    echo "--- START $c/$arm $(date -u +%H:%M:%S) ---"
    /tmp/run_case_fixed.sh "$c" "$arm" "m5a_${c}_${arm}" "$skip"
    echo "--- DONE $c/$arm $(date -u +%H:%M:%S) :: $(tail -1 $MATRIX_RESULTS 2>/dev/null) ---"
  done
done
echo "=== MATRIX-5ARM COMPLETE $(date -u +%H:%M:%S) ==="
