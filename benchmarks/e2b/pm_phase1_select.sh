#!/bin/bash
# PHASE 1 — SELECT: 5 arms × 5 tasks × n=2 (glm-5.1). Pick the best arm; Phase 2 validates it
# on the held-out 5 tasks vs plain. Train/validate split → guards against overfitting.
cd /Users/marmikpandya/semantic-filesystem
set -a; . ./.env; set +a
export WB_FORCE_OPENROUTER=1 WB_OR_MODEL=z-ai/glm-5.1
export WB_AGENT_TIMEOUT=2400 WB_CELL_TIMEOUT=2600   # raised so compress isn't zeroed by latency
CASES=15,44,53,95,175
A=tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs
echo "PHASE1 SELECT  5 arms × {$CASES} × n2  @ $(date +%H:%M:%S)"

# arm: "rep_prefix  arm  knob(or -)"
ARMS=(
  "p1pl    plain  -"
  "p1po    nokg   prompt_only.json"
  "p1oc    nokg   output_compression.json"
  "p1cd    nokg   best_exp0002.json"
  "p1cdoc  nokg   compress_dedup_oc.json"
)

clean_heavy(){ # drop re-pullable bulk from THIS run's cells only (keep result.json+model_output+rubrics)
  for d in $A/pm_codex_*_rp1*; do
    rm -f "$d/full.tgz" 2>/dev/null
    rm -rf "$d/sandbox_raw" "$d/semfs_logs" 2>/dev/null
  done
}

for r in 1 2; do
  for spec in "${ARMS[@]}"; do
    set -- $spec; prefix=$1; arm=$2; knob=$3
    kargs=""; [ "$knob" != "-" ] && kargs="--knobs benchmarks/e2b/knobs/$knob"
    echo "=== arm=$prefix ($arm ${knob}) rep $r @ $(date +%H:%M:%S) ==="
    python3 benchmarks/e2b/run_matrix.py --cases $CASES --agents codex --arms $arm $kargs \
      --rep "${prefix}${r}" --parallel 3 2>&1 | tail -4
    clean_heavy
  done
done

echo "=== JUDGING @ $(date +%H:%M:%S) ==="
mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null
LBL=""
for r in 1 2; do for spec in "${ARMS[@]}"; do set -- $spec; prefix=$1; arm=$2
  for c in 15 44 53 95 175; do
    d="$A/pm_codex_${c}_${arm}_r${prefix}${r}"
    [ -f "$d/result.json" ] && rm -f "$d"/rubrics_judge--*.json && LBL="$LBL pm_codex_${c}_${arm}_r${prefix}${r}"
  done
done; done
python3 benchmarks/e2b/run_judge.py $LBL 2>&1 | grep -E "%" | tail -60

python3 - <<'PY'
import json, glob, os
A="tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs"
ARMS=[("p1pl","plain","plain"),("p1po","nokg","prompt"),("p1oc","nokg","prompt+oc"),
      ("p1cd","nokg","compress+dedup+prompt"),("p1cdoc","nokg","compress+dedup+prompt+oc")]
cases=["15","44","53","95","175"]
print("\n=== PHASE 1 per-arm aggregate (5 tasks × n2) ===")
print(f"  {'arm':26} {'mean_acc':>9} {'mean_tok':>10}  n")
for pfx,arm,name in ARMS:
    accs=[]; toks=[]
    for r in (1,2):
        for c in cases:
            d=f"{A}/pm_codex_{c}_{arm}_r{pfx}{r}"
            rj=f"{d}/result.json"
            if os.path.exists(rj):
                try:
                    t=json.load(open(rj)).get("tokens")
                    if t: toks.append(t)
                except: pass
            jf=glob.glob(f"{d}/rubrics_judge--*.json")
            if jf:
                try:
                    s=json.load(open(jf[0])).get("summary",{})
                    if s.get("total"): accs.append(s["passed"]/s["total"])
                except: pass
    ma=round(sum(accs)/len(accs),3) if accs else None
    mt=int(sum(toks)/len(toks)) if toks else None
    print(f"  {name:26} {str(ma):>9} {str(mt):>10}  {len(accs)}")
PY
echo "PHASE1_DONE @ $(date +%H:%M:%S)"
