#!/bin/bash
# kaifa workspace (Backend Developer, 11 WB-Lite cases) — 3 arms: plain, hkg-edges, hkg-retrieval.
# Mirror of run_pm3.sh but for the kaifa persona (SEM-37 kaifa-first). ONE rep per invocation —
# launch 3x as separate bg tasks for n=3.   Usage:  bash benchmarks/e2b/run_kaifa3.sh <repnum>
#
# Persona wiring (the ONLY deltas vs run_pm3.sh — all via env knobs run_matrix.py already honors):
#   WB_E2B_TEMPLATE      semfs-baked-kaifa  (kaifa corpus + seed + 11 .task files baked; v3 office libs)
#   WB_E2B_SEED_DEFAULT  /opt/kaifa-gemma-q4.db  (both semfs arms mount the kaifa seed; KG-complete)
#   WB_BOOT_SEED         /opt/kaifa-gemma-q4.db  (boot-prep copies the kaifa seed, not chanpin's)
#   WB_LITE_DIR          all-persona metadata dir (so the output-file hint resolves for kaifa cases)
#   WB_CASES             the 11 Backend-Developer case ids
# Mount label stays "chanpin" inside run_matrix (cosmetic daemon label); the DB content is kaifa.
set -uo pipefail
cd /Users/marmikpandya/semantic-filesystem
export WB_E2B_TEMPLATE=semfs-baked-kaifa
export WB_E2B_SEED_DEFAULT=/opt/kaifa-gemma-q4.db
export WB_BOOT_SEED=/opt/kaifa-gemma-q4.db
# kaifa seed is seed_dir-built (search index only, no materialized fs_* tree) → grep-only mount.
export WB_SEARCH_ONLY=on
export WB_LITE_DIR=/Users/marmikpandya/semantic-filesystem/benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en
export WB_CASES="${WB_CASES:-3,7,91,92,94,226,242,266,286,300,311}"
R="$1"
echo "KAIFA3 rep K$R START $(date +%H:%M:%S) cases=$WB_CASES"
# plain (no knob, no mount)
bash benchmarks/e2b/run_bf_group.sh "K${R}p" 4 "plain"
# hkg-edges (semfs-fixed binary, co-mention on) — same best knob as the PM run for comparability
bash benchmarks/e2b/run_bf_group.sh "K${R}e" 6 "hiddenkg_edges" best_exp0002.json
# hkg-retrieval (semfs-fixed-retrieval binary, KG injection on)
WB_FIXED_BIN=benchmarks/e2b/assets/semfs-fixed-retrieval \
  bash benchmarks/e2b/run_bf_group.sh "K${R}r" 6 "hiddenkg_retrieval" best_exp0002.json
echo "KAIFA3 rep K$R DONE $(date +%H:%M:%S)"
