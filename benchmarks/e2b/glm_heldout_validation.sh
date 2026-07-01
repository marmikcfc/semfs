#!/bin/bash
# Held-out validation: does the converged winner (exp_0002 config) generalize beyond 53+171?
# Best-config nokg vs plain on 3 unseen PM cases, n=2, glm-5.1.
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_FORCE_OPENROUTER=1 WB_OR_MODEL=z-ai/glm-5.1
CASES=95,386,175
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs
echo "HELDOUT glm-5.1 cases=$CASES n=2 @ $(date +%H:%M:%S)"
for r in h1 h2; do
  echo "=== plain rep $r @ $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --arms plain --rep "$r" --parallel 2 2>&1 | tail -4
  echo "=== nokg(best) rep $r @ $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --arms nokg --knobs benchmarks/e2b/knobs/best_exp0002.json --rep "$r" --parallel 2 2>&1 | tail -4
done
echo "=== JUDGING @ $(date +%H:%M:%S) ==="
mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null
LBL=""
for r in h1 h2; do for c in 95 386 175; do for arm in plain nokg; do
  d="$A/pm_codex_${c}_${arm}_r${r}"; [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_${arm}_r${r}"
done; done; done
python3 benchmarks/e2b/run_judge.py $LBL 2>&1 | tail -20
echo "HELDOUT_DONE @ $(date +%H:%M:%S)"
