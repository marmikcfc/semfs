#!/bin/bash
# Controlled test (SEM-35): the 2 plain-wins cases (53, 171) with the infra confound removed
# (mount-health gate now active in run_matrix) and the query-rewriter isolated.
# Question: with a guaranteed-live mount, does semfs close the gap to plain — and how much
# of the gap is the rewriter (PO_4 → "phosphate")?
#   plain  | nokg + SEMFS_REWRITE=0 (the fix)  | nokg + SEMFS_REWRITE=1 (default, measures rewriter harm)
# parallel=2 (only 2 cases; also lowers daemon RAM pressure that likely killed the fd2 mount).
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_FORCE_OPENROUTER=1
CASES=53,171
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs
echo "CONTROLLED  cases=$CASES  openrouter=1  mount-gate=on  @ $(date +%H:%M:%S)"

runarm() { # arm  knob(or -)  rep
  local arm=$1 knob=$2 rep=$3 kargs=""
  [ "$knob" != "-" ] && kargs="--knobs benchmarks/e2b/knobs/$knob.json"
  echo "=== ARM $arm  KNOB $knob  REP $rep @ $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --arms $arm \
    $kargs --rep "$rep" --parallel 2 2>&1 | tail -10
}
for r in cp1 cp2 cp3; do runarm plain -   $r; done   # plain baseline
for r in c0a c0b c0c; do runarm nokg  rw0 $r; done   # semfs, rewrite OFF (fix)
for r in c1a c1b c1c; do runarm nokg  rw1 $r; done   # semfs, rewrite ON (default)

echo "=== JUDGING @ $(date +%H:%M:%S) ==="
mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null
LBL=""
for r in cp1 cp2 cp3; do for c in 53 171; do
  d="$A/pm_codex_${c}_plain_r${r}"; [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_plain_r${r}"
done; done
for r in c0a c0b c0c c1a c1b c1c; do for c in 53 171; do
  d="$A/pm_codex_${c}_nokg_r${r}"; [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_nokg_r${r}"
done; done
python3 benchmarks/e2b/run_judge.py $LBL 2>&1 | grep -E "%"
echo "CONTROLLED_DONE @ $(date +%H:%M:%S)"
