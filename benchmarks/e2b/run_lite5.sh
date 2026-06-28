#!/bin/bash
# WB-Lite REMAINING 5 cases (45,55,171,386,388) × 4 arms (plain, compress, hkg-edges, hkg-rerank).
# ONE rep per invocation — launch 3× as separate bg tasks for n=3.
#   Usage:  bash benchmarks/e2b/run_lite5.sh <repnum>
# Reuses run_bf_group.sh (litellm endpoint, semfs-fixed binary, chanpin-4arm.db seed).
# 3 knob-groups run sequentially → peak footprint = max group PAR (6).
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
export WB_CASES="${WB_CASES:-45,55,171,386,388}"
R="$1"
echo "LITE5 rep L$R START $(date +%H:%M:%S) cases=$WB_CASES"
# edges + rerank share best_exp0002 → one call (2 arms × 5 cases = 10 cells)
bash benchmarks/e2b/run_bf_group.sh "L${R}e" 6 "hiddenkg_edges,hiddenkg" best_exp0002.json
# plain (no knob, no mount)
bash benchmarks/e2b/run_bf_group.sh "L${R}p" 3 "plain"
# compress (nokg + compress-only)
bash benchmarks/e2b/run_bf_group.sh "L${R}c" 3 "nokg" compress_only_clean.json
echo "LITE5 rep L$R DONE $(date +%H:%M:%S)"
