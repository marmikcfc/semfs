#!/bin/bash
# Full PM matrix on OpenRouter: 3 arms × n=3 × 10 PM cases (289 EXCLUDED — seed/corpus leak).
# Arms (all on the SAME new binary, dedup compiled in):
#   plain                         — ripgrep baseline (no semfs)
#   nokg + dedup_w5               — semfs, cross-turn dedup ON (W=5)
#   nokg + dedup_w5_p2b           — dedup ON + turn-brake prompt hint (p2b)
# Distinct rep labels per arm so the two nokg configs don't collide.
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_FORCE_OPENROUTER=1
CASES=15,44,45,53,55,95,171,175,386,388
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs
echo "PM MATRIX  cases=$CASES  openrouter=$WB_FORCE_OPENROUTER  @ $(date +%H:%M:%S)"

runarm() { # arm  knob(or -)  rep
  local arm=$1 knob=$2 rep=$3 kargs=""
  [ "$knob" != "-" ] && kargs="--knobs benchmarks/e2b/knobs/$knob.json"
  echo "=== ARM $arm  KNOB $knob  REP $rep @ $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --arms $arm \
    $kargs --rep "$rep" --parallel 3 2>&1 | tail -8
}

for r in fp1 fp2 fp3; do runarm plain -            $r; done   # plain
for r in fd1 fd2 fd3; do runarm nokg  dedup_w5     $r; done   # dedup-on
for r in ft1 ft2 ft3; do runarm nokg  dedup_w5_p2b $r; done   # dedup-on + turn-brake

echo "=== JUDGING @ $(date +%H:%M:%S) ==="
mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null
LBL=""
for r in fp1 fp2 fp3; do for c in ${CASES//,/ }; do
  d="$A/pm_codex_${c}_plain_r${r}"; [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_plain_r${r}"
done; done
for r in fd1 fd2 fd3 ft1 ft2 ft3; do for c in ${CASES//,/ }; do
  d="$A/pm_codex_${c}_nokg_r${r}"; [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_nokg_r${r}"
done; done
python3 benchmarks/e2b/run_judge.py $LBL 2>&1 | grep -E "%"
echo "PM_MATRIX_DONE @ $(date +%H:%M:%S)"
