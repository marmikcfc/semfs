#!/usr/bin/env bash
# run_case_fixed.sh <CASE> <ARM> <STAMP> [SKIPPREP]
# ARM ladder (each adds exactly one capability over the previous):
#   plain   : baseline codex, real files on disk (no semfs)
#   nokg    : semfs LOCAL sqlite search + grep-inline. NO KG, NO by-topic.
#   gfs_off : nokg + knowledge graph (/kg/ artifacts)
#   gfs_on  : gfs_off + /by-topic/ filesystem overlay
#   cloud   : Supermemory CLOUD search (tag workspace-bench-chanpin). NO KG, NO by-topic.
# Telemetry: every (case,arm) archives traces+snapshots+diffs+timing+judge+deliverable
#            to $MATRIX_ART/<case>_<arm>/ (nothing is lost to clobbering).
set -u
CASE="$1"; ARM="$2"; STAMP="$3"; SKIPPREP="${4:-0}"
SEMFS=/home/ubuntu/.local/bin/semfs
HARNESS=/srv/semfs-benchmark/semantic-filesystem/benchmarks/aws/run_workspace_bench.sh
EVAL=/srv/semfs-benchmark/Workspace-Bench/evaluation
HF=$EVAL/tasks_lite.full
RESULTS="${MATRIX_RESULTS:-/tmp/pm_results_5.jsonl}"
ARTROOT="${MATRIX_ART:-/srv/semfs-benchmark/matrix_artifacts/run}"
CANON=/home/ubuntu/.semfs/chanpin-gemma-q4.db     # clean local seed (never written)
MATRIX_TAG=chanpin-matrix                          # local working-copy tag
CLOUD_TAG=workspace-bench-chanpin                  # Supermemory cloud container (verified searchable)
WORKDIR=$EVAL/filesys/chanpin_workdir_Codex_GPT-5.4
set -a; . /home/ubuntu/.semfs_seed_env 2>/dev/null; set +a
export SUPERMEMORY_API_KEY="${SUPERMEMORY_API_KEY:-harness-guard}"
export SEMFS_BIN="$SEMFS"; export PATH="/home/ubuntu/.local/bin:$PATH"
export DATASET=smoke RUN_STAMP="$STAMP"
# straggler cleanup — SERIAL ONLY. Remove these broad pkills before parallelizing.
pkill -f run_workspace_bench 2>/dev/null || true
pkill -f agent_runner.py 2>/dev/null || true
pkill -f "codex exec" 2>/dev/null || true
for t in chanpin-matrix workspace-bench-chanpin chanpin-gemma-q4-sib chanpin-gemma-q4 chanpin-e5-nosum; do "$SEMFS" unmount "$t" >/dev/null 2>&1 || true; done
sleep 2
rm -rf "$EVAL/tasks_lite"/* ; cp -r "$HF/$CASE" "$EVAL/tasks_lite/$CASE"

MOUNT_TAG=""
case "$ARM" in
  plain)
    TARGET=codex; LABEL="Codex--GPT-5.4--Smoke" ;;
  nokg|gfs_off|gfs_on)
    TARGET=semfs-codex; LABEL="SEMFSCodex--GPT-5.4--Smoke-SEMFS"; MOUNT_TAG=$MATRIX_TAG
    cp -f "$CANON" "/home/ubuntu/.semfs/$MATRIX_TAG.db"   # fresh clean copy -> contamination impossible
    export SEMFS_CONTAINER_TAG=$MATRIX_TAG SEMFS_EMBED_MODEL=gemma-q4 SEMFS_EMBED_ONNX_DIR=/home/ubuntu/gemma_q4
    export XDG_CACHE_HOME=/srv/semfs-benchmark/rewrite-test/cache
    export SEMFS_REWRITE=1 SEMFS_RETURN_MODE=snippet SEMFS_RESULT_LIMIT=8 SEMFS_SEARCH_ONLY=on SEMFS_GREP_INLINE=on
    export SEMFS_MOUNT_TIMEOUT_SEC=1800 SEMFS_STARTUP_TIMEOUT_SEC=900 SEMFS_NO_PUSH=1 SEMFS_NO_SYNC=1
    case "$ARM" in
      gfs_on)  export SEMFS_KG=on  SEMFS_GRAPH_FS=on ;;
      gfs_off) export SEMFS_KG=on  SEMFS_GRAPH_FS=off ;;
      nokg)    export SEMFS_KG=off SEMFS_GRAPH_FS=off ;;
    esac ;;
  cloud)
    TARGET=semfs-codex; LABEL="SEMFSCodex--GPT-5.4--Smoke-SEMFS"; MOUNT_TAG=$CLOUD_TAG
    # Supermemory cloud: no local index copy; search runs server-side.
    export SEMFS_CONTAINER_TAG=$CLOUD_TAG SEMFS_STORAGE_BACKEND=cloud
    export XDG_CACHE_HOME=/srv/semfs-benchmark/rewrite-test/cache
    export SEMFS_REWRITE=1 SEMFS_RETURN_MODE=snippet SEMFS_RESULT_LIMIT=8 SEMFS_SEARCH_ONLY=on SEMFS_GREP_INLINE=on
    export SEMFS_MOUNT_TIMEOUT_SEC=1800 SEMFS_STARTUP_TIMEOUT_SEC=900 SEMFS_NO_PUSH=1 SEMFS_NO_SYNC=1 SEMFS_KG=off SEMFS_GRAPH_FS=off ;;
  *) echo "unknown arm: $ARM" >&2; exit 2 ;;
esac

OUT=$EVAL/output/$LABEL/$CASE
rm -rf "$OUT"
if [ "$ARM" = plain ]; then
  rm -rf "$WORKDIR/model_output" 2>/dev/null || true
fi
cd "$EVAL"
LOG=/tmp/run_${STAMP}.log
echo "### START $ARM/$CASE target=$TARGET tag=${MOUNT_TAG:-none} kg=${SEMFS_KG:-n/a} gfs=${SEMFS_GRAPH_FS:-n/a} storage=${SEMFS_STORAGE_BACKEND:-sqlite}" | tee "$LOG"
T0=$(date +%s)
[ "$SKIPPREP" = 1 ] && export SKIP_PREPARE=1
timeout 2700 "$HARNESS" "$TARGET" >>"$LOG" 2>&1 </dev/null
WALL=$(( $(date +%s) - T0 ))

# --- capture the agent deliverable from the (still-mounted) workdir BEFORE unmount ---
# (semfs arms write model_output INTO the mount/DB, which is gone after unmount + next fresh copy)
ART="$ARTROOT/${CASE}_${ARM}"; mkdir -p "$ART"
[ -d "$WORKDIR/model_output" ] && cp -r "$WORKDIR/model_output" "$ART/model_output" 2>/dev/null || true

[ -n "$MOUNT_TAG" ] && "$SEMFS" unmount "$MOUNT_TAG" >/dev/null 2>&1 || true

timeout 300 python3 src/agent_eval.py --task-dir "$OUT" --eval-yaml /tmp/judge_seed.yaml --overwrite >/dev/null 2>&1

# --- result line ---
python3 - "$CASE" "$ARM" "$OUT" "$WALL" >> "$RESULTS" << "PYEOF"
import json,sys,os,glob
case,arm,out=sys.argv[1],sys.argv[2],sys.argv[3]
r={"case":case,"arm":arm,"wall_sec":int(sys.argv[4]) if len(sys.argv)>4 else None}
try:
    a=json.load(open(os.path.join(out,"agent.json")))
    r["tokens"]=a.get("totalTokens"); r["turns"]=a.get("turns"); r["status"]=a.get("status")
except Exception as e: r["agent_err"]=str(e)[:80]
try:
    tc=0; cached=inp=outp=None
    jl=sorted(glob.glob(os.path.join(out,"raw","*.jsonl")))
    pick=[p for p in jl if "codex_stdout" in p] or jl
    if pick:
        for line in open(pick[0]):
            line=line.strip()
            if not line: continue
            try: ev=json.loads(line)
            except: continue
            it=(ev.get("item") or {}).get("type")
            if ev.get("type")=="item.completed" and it=="command_execution": tc+=1
            if ev.get("type")=="turn.completed":
                u=ev.get("usage") or {}; cached=u.get("cached_input_tokens"); inp=u.get("input_tokens"); outp=u.get("output_tokens")
    r["tool_calls"]=tc; r["cached_input"]=cached; r["input_tokens"]=inp; r["output_tokens"]=outp
except Exception as e: r["tc_err"]=str(e)[:60]
try:
    j=json.load(open(os.path.join(out,"rubrics_judge--seed-2.0-lite-judge.json")))
    s=j.get("summary",{}); r["passed"]=s.get("passed"); r["total"]=s.get("total"); r["judge_err"]=(j.get("judge",{}) or {}).get("error")
except Exception as e: r["rubric_err"]=str(e)[:80]
print(json.dumps(r))
PYEOF

# --- archive ALL telemetry artifacts (traces, snapshots, diffs, timing, narrative, judge) ---
[ -d "$OUT" ] && cp -r "$OUT" "$ART/output" 2>/dev/null || true
TELDIR="$EVAL/output/_telemetry/${STAMP}-${TARGET}-${DATASET}"
[ -d "$TELDIR" ] && cp -r "$TELDIR" "$ART/telemetry" 2>/dev/null || true
cp "$LOG" "$ART/run.log" 2>/dev/null || true
echo "### DONE $ARM/$CASE wall=${WALL}s -> $ART"
