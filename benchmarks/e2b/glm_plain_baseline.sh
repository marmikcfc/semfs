#!/bin/bash
# PLAIN baseline on glm-5.1, cases 53+171, n=3 — establishes the evo objective references:
#   token CEILING (gate: experiment mean tokens must be <= plain mean tokens)
#   accuracy FLOOR (target: experiment mean accuracy must exceed plain mean accuracy)
# Plain is measured ONCE here (fixed reference); evo experiments run only the semfs arm.
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_FORCE_OPENROUTER=1 WB_OR_MODEL=z-ai/glm-5.1
CASES=53,171
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs
echo "PLAIN BASELINE glm-5.1 cases=$CASES n=3 model=$WB_OR_MODEL @ $(date +%H:%M:%S)"
for r in 1 2 3; do
  echo "=== PLAIN rep $r @ $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --arms plain \
    --rep "glmplain$r" --parallel 2 --force 2>&1 | tail -8
done
echo "=== JUDGING @ $(date +%H:%M:%S) ==="
mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null
LBL=""
for r in 1 2 3; do for c in 53 171; do
  d="$A/pm_codex_${c}_plain_rglmplain${r}"
  [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_plain_rglmplain${r}"
done; done
python3 benchmarks/e2b/run_judge.py $LBL 2>&1 | tail -20
echo "PLAIN_BASELINE_DONE @ $(date +%H:%M:%S)"
