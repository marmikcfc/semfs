#!/bin/bash
# Phase 2: turn-brake variants × n=2 × 5 cases, codex nokg (hint + fixed binary).
# Phase 1 showed cap-tightening starves accuracy; this cuts turns/re-reads instead.
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
CASES=15,44,45,53,55
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs

run() { # cfg_file  rep
  echo "=== CONFIG $1  REP $2  @ $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --arms nokg \
    --knobs benchmarks/e2b/knobs/$1.json --rep "$2" --parallel 3 2>&1 | tail -12
}
run p2a_turnbrake_mild   p2aA
run p2a_turnbrake_mild   p2aB
run p2b_turnbrake_strong p2bA
run p2b_turnbrake_strong p2bB

echo "=== JUDGING phase-2 @ $(date +%H:%M:%S) ==="
mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null
LBL=""
for r in p2aA p2aB p2bA p2bB; do for c in 15 44 45 53 55; do
  d="$A/pm_codex_${c}_nokg_r${r}"
  [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_nokg_r${r}"
done; done
python3 benchmarks/e2b/run_judge.py $LBL 2>&1 | grep -E "%"
echo "PHASE2_DONE @ $(date +%H:%M:%S)"
