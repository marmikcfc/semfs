#!/usr/bin/env bash
# Phase "Local L1–L5" end-to-end: mount a container, write markdown THROUGH the
# mount, then `grep --offline` it — exercising the full LOCAL pipeline:
#   L1 chunk → L2 embed (local fastembed) → L3 index (vec0+fts5)
#            → search (KNN ∪ BM25 → RRF) → L5 rerank (cloud Cohere via OpenRouter).
#
# Mounting is privileged. Run with sudo AND preserve env + HOME so the daemon and
# grep resolve the SAME cache dir / model dir / keys:
#
#   sudo -E HOME="$HOME" bash crates/e2e/phase_local_l1_l5.sh
#
# Required env (from bash/.env): SUPERMEMORY_API_KEY (org id + cache path),
# OPENROUTER_API_KEY (cloud reranker). The local embedder model is auto-located
# in the TS transformers.js cache unless SEMFS_EMBED_MODEL_DIR is set.
set -euo pipefail

: "${SUPERMEMORY_API_KEY:?set SUPERMEMORY_API_KEY (needed for org id + cache path)}"
: "${OPENROUTER_API_KEY:?set OPENROUTER_API_KEY (cloud reranker)}"

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
# Capability resolver picks: local embedder (this dir present) + Cohere reranker
# (OPENROUTER_API_KEY present). No --offline flag — it's all data-driven.
export SEMFS_EMBED_MODEL_DIR="${SEMFS_EMBED_MODEL_DIR:-$REPO/bash/node_modules/@huggingface/transformers/.cache/Xenova/all-MiniLM-L6-v2}"

if [ ! -f "$SEMFS_EMBED_MODEL_DIR/onnx/model.onnx" ]; then
  echo "FAIL: local embedder model not found at $SEMFS_EMBED_MODEL_DIR" >&2
  exit 1
fi

BIN="$REPO/target/debug/semfs"
TAG="local-l1l5-$(date +%s)"
MNT="$(mktemp -d)/$TAG"
mkdir -p "$MNT"

echo "building semfs..."
(cd "$REPO/crates" && cargo build -p semfs >/dev/null 2>&1)

echo "mounting '$TAG' at $MNT (local index auto-enabled via the resolver)..."
"$BIN" mount "$TAG" --path "$MNT" --key "$SUPERMEMORY_API_KEY" --no-sync --foreground &
DPID=$!
trap '"$BIN" unmount "$TAG" --force 2>/dev/null || true; kill "$DPID" 2>/dev/null || true' EXIT
sleep 6   # let mount + initial warmup settle

echo "writing markdown files through the mount..."
printf '%s\n' "the access token is refreshed by the middleware before each request" > "$MNT/auth.md"
printf '%s\n' "fold the egg whites gently into the batter and bake until golden"     > "$MNT/cooking.md"
sleep 4   # let flush → local embed → index land

echo "--- semfs grep (config-driven → local index via the .semfs marker, no flag) ---"
OUT="$("$BIN" grep "how does login credential renewal work" "$MNT/" 2>&1 || true)"
echo "$OUT"
if echo "$OUT" | grep -q "auth.md"; then
  echo "PASS: local L1–L5 pipeline found auth.md through the mount (no cloud search)"
else
  echo "FAIL: expected auth.md in the offline results"
  exit 1
fi
