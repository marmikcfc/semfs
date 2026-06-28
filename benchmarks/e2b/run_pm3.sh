#!/bin/bash
# PM workspace (chanpin WB-Lite, all 10 cases) — 3 arms: plain, hkg-edges, hkg-retrieval.
# ONE rep per invocation — launch 3× as separate bg tasks for n=3.
#   Usage:  bash benchmarks/e2b/run_pm3.sh <repnum>
# v3 template (office libs). hkg-edges = semfs-fixed + co-mention; hkg-retrieval = semfs-fixed-retrieval + KG injection.
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
export WB_CASES="${WB_CASES:-15,44,45,53,55,95,171,175,386,388}"
R="$1"
echo "PM3 rep P$R START $(date +%H:%M:%S) cases=$WB_CASES"
# plain (no knob, no mount)
bash benchmarks/e2b/run_bf_group.sh "P${R}p" 4 "plain"
# hkg-edges (semfs-fixed binary, co-mention on)
bash benchmarks/e2b/run_bf_group.sh "P${R}e" 6 "hiddenkg_edges" best_exp0002.json
# hkg-retrieval (semfs-fixed-retrieval binary, KG injection on)
WB_FIXED_BIN=benchmarks/e2b/assets/semfs-fixed-retrieval \
  bash benchmarks/e2b/run_bf_group.sh "P${R}r" 6 "hiddenkg_retrieval" best_exp0002.json
echo "PM3 rep P$R DONE $(date +%H:%M:%S)"
