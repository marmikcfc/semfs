#!/bin/bash
# Block-render fix test: 53,171 × n=2. Tests the CODE fix (newline-preserving block
# format for inline-full hits) rather than the prompt band-aid.
#   plain                  — baseline
#   nokg + fix_v2          — dedup ON + rewrite OFF + block render (no prompt)
#   nokg + fix_v1          — dedup ON + rewrite OFF + transcription prompt (band-aid control)
# Question: does block render alone close the gap, without any prompt coaxing?
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_FORCE_OPENROUTER=1
CASES=53,171
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs
echo "RENDER FIX TEST  cases=$CASES  @ $(date +%H:%M:%S)"

runarm() {
  local arm=$1 knob=$2 rep=$3 kargs=""
  [ "$knob" != "-" ] && kargs="--knobs benchmarks/e2b/knobs/$knob.json"
  echo "=== ARM $arm  KNOB $knob  REP $rep @ $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --arms $arm \
    $kargs --rep "$rep" --parallel 2 2>&1 | tail -10
}
for r in rp1 rp2; do runarm plain  -       $r; done   # baseline
for r in rv1 rv2; do runarm nokg   fix_v2  $r; done   # block-render, no prompt
for r in rb1 rb2; do runarm nokg   fix_v1  $r; done   # prompt band-aid (control)

echo "=== JUDGING @ $(date +%H:%M:%S) ==="
mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null
LBL=""
for r in rp1 rp2; do for c in 53 171; do
  d="$A/pm_codex_${c}_plain_r${r}"; [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_plain_r${r}"
done; done
for r in rv1 rv2 rb1 rb2; do for c in 53 171; do
  d="$A/pm_codex_${c}_nokg_r${r}"; [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_nokg_r${r}"
done; done
python3 benchmarks/e2b/run_judge.py $LBL 2>&1 | grep -E "%"
echo "RENDER_FIX_DONE @ $(date +%H:%M:%S)"
