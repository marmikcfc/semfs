#!/bin/bash
# Phase 1 of the knob sweep: 3 hypotheses × n=2 × 5 cases, codex nokg (hint + fixed binary).
# Rep labels namespace each config so they don't collide with the baseline (r1/r2) or each other.
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
CASES=15,44,45,53,55
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs

run() { # cfg_file  rep_short
  echo "=== CONFIG $1  REP $2  @ $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --arms nokg \
    --knobs benchmarks/e2b/knobs/$1.json --rep "$2" --parallel 3 2>&1 | tail -14
}

run h1_tight_caps  h1a
run h1_tight_caps  h1b
run h2_fewer_hits  h2a
run h2_fewer_hits  h2b
run h3_no_rewrite  h3a
run h3_no_rewrite  h3b

echo "=== JUDGING all phase-1 cells @ $(date +%H:%M:%S) ==="
mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null
LBL=""
for r in h1a h1b h2a h2b h3a h3b; do for c in 15 44 45 53 55; do
  d="$A/pm_codex_${c}_nokg_r${r}"
  [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_nokg_r${r}"
done; done
python3 benchmarks/e2b/run_judge.py $LBL 2>&1 | grep -E "%"
echo "PHASE1_DONE @ $(date +%H:%M:%S)"
