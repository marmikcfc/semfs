#!/bin/bash
# Dedup A/B (SEM-19 v1): SEMFS_DEDUP_WINDOW=5 (on) vs 0 (off), codex nokg.
# BOTH arms run the SAME freshly-built binary (assets/semfs-fixed) — only the
# window differs, so this isolates the cross-turn dedup effect. Re-grep-heavy
# case 53 is the primary signal; the rest give the aggregate token picture.
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
CASES=${CASES:-15,44,45,53,55}        # override: CASES=45,53 for the lean first pass
export WB_FORCE_OPENROUTER=${WB_FORCE_OPENROUTER:-1}   # this A/B runs on OpenRouter
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs
echo "DEDUP A/B  cases=$CASES  openrouter=$WB_FORCE_OPENROUTER"

run() { # cfg_file  rep
  echo "=== CONFIG $1  REP $2  @ $(date +%H:%M:%S) ==="
  python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --arms nokg \
    --knobs benchmarks/e2b/knobs/$1.json --rep "$2" --parallel 3 2>&1 | tail -12
}
run dedup_off dOFFa
run dedup_off dOFFb
run dedup_w5  dW5a
run dedup_w5  dW5b

echo "=== JUDGING dedup A/B @ $(date +%H:%M:%S) ==="
mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null
LBL=""
for r in dOFFa dOFFb dW5a dW5b; do for c in ${CASES//,/ }; do
  d="$A/pm_codex_${c}_nokg_r${r}"
  [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_nokg_r${r}"
done; done
python3 benchmarks/e2b/run_judge.py $LBL 2>&1 | grep -E "%"
echo "DEDUP_AB_DONE @ $(date +%H:%M:%S)"
