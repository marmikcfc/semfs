#!/bin/bash
# Test: compress + prompt, NO dedup (drops the dedup call-inflater). cases 53+171, n=3, glm-5.1.
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_FORCE_OPENROUTER=1 WB_OR_MODEL=z-ai/glm-5.1
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs
echo "COMPRESS+PROMPT (no dedup) cases=53,171 n=3 @ $(date +%H:%M:%S)"
for r in cnd1 cnd2 cnd3; do
  echo "=== rep $r @ $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases 53,171 --agents codex --arms nokg \
    --knobs benchmarks/e2b/knobs/compress_no_dedup.json --rep "$r" --parallel 2 --force 2>&1 | tail -5
done
echo "=== JUDGING @ $(date +%H:%M:%S) ==="
mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null
LBL=""
for r in cnd1 cnd2 cnd3; do for c in 53 171; do
  d="$A/pm_codex_${c}_nokg_r${r}"; [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_nokg_r${r}"
done; done
python3 benchmarks/e2b/run_judge.py $LBL 2>&1 | grep -E "rubrics=|%"
echo "=== calls+tokens per cell ==="
for r in cnd1 cnd2 cnd3; do for c in 53 171; do
  f="$A/pm_codex_${c}_nokg_r${r}/result.json"
  [ -f "$f" ] && python3 -c "import json;d=json.load(open('$f'));print(f'  ${c} ${r}: calls={d.get(\"calls\")} tok={d.get(\"tokens\")} status={d.get(\"status\")}')"
done; done
echo "CND_DONE @ $(date +%H:%M:%S)"
