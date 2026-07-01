#!/bin/bash
# Fix test (rcas/2026-06-16-...transcribe): cases 53,171 × n=2.
#   plain                         — baseline
#   nokg + fix_v1 (dedup ON + rewrite OFF + turn-based TRANSCRIPTION prompt)
# The fix targets the verified failure: agent has the grep content but writes empty/generic
# instead of transcribing. Mount-gate is active (run_matrix) so dead mounts can't confound.
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_FORCE_OPENROUTER=1
CASES=53,171
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs
echo "FIX TEST  cases=$CASES  openrouter=1  mount-gate=on  @ $(date +%H:%M:%S)"

runarm() { # arm  knob(or -)  rep
  local arm=$1 knob=$2 rep=$3 kargs=""
  [ "$knob" != "-" ] && kargs="--knobs benchmarks/e2b/knobs/$knob.json"
  echo "=== ARM $arm  KNOB $knob  REP $rep @ $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --arms $arm \
    $kargs --rep "$rep" --parallel 2 2>&1 | tail -10
}
for r in xp1 xp2; do runarm plain -       $r; done   # baseline
for r in xf1 xf2; do runarm nokg  fix_v1  $r; done    # the fix

echo "=== JUDGING @ $(date +%H:%M:%S) ==="
mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null
LBL=""
for r in xp1 xp2; do for c in 53 171; do
  d="$A/pm_codex_${c}_plain_r${r}"; [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_plain_r${r}"
done; done
for r in xf1 xf2; do for c in 53 171; do
  d="$A/pm_codex_${c}_nokg_r${r}"; [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_nokg_r${r}"
done; done
python3 benchmarks/e2b/run_judge.py $LBL 2>&1 | grep -E "%"
echo "FIX_TEST_DONE @ $(date +%H:%M:%S)"
