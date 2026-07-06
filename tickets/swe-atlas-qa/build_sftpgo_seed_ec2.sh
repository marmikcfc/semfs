#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Build the sftpgo gliner-KG seed on an EC2 box, from the REAL SWE-Atlas image.
#
#   pull public image → extract /app (byte-identical to the plain run's workspace)
#   → seed_dir (embed, fastembed)          [CPU]
#   → build_kg --features gliner-kg        [CPU]   AST(Go tree-sitter) + gliner doc lane
#   → materialize_kg (Leiden communities)  [CPU]   → sftpgo-gliner.db + workspace map
#
# GPU-FREE end to end (gliner is a Candle CPU encoder). No GHCR token — the
# SWE-Atlas images are PUBLIC (anonymous pull). Verified locally on the same
# commit: dual-lane KG (1241 code entities / 6140 calls via AST + gliner doc
# entities) + 7 Leiden communities → ready for plain / ppr_off / ppr_on / ppr_map.
#
# Prereqs on the box: docker, a Rust toolchain (rustup), python3, ~4 GB free.
# Usage:  SEMFS_REPO=~/semantic-filesystem bash build_sftpgo_seed_ec2.sh
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

IMG="ghcr.io/scaleapi/swe-atlas:swe_atlas_QnA_drakkan_sftpgo_1.0"   # public, no token
BASE_COMMIT="44634210287cb192f2a53147eafb84a33a96826b"              # for the record
REPO="${SEMFS_REPO:-$HOME/semantic-filesystem}"
WORK="${WORK:-$HOME/sftpgo-seed}"
DB="$WORK/sftpgo-gliner.db"
APP="$WORK/app"
export HF_HOME="${HF_HOME:-$WORK/hf_cache}"    # persist model downloads across reruns
mkdir -p "$WORK" "$HF_HOME"

echo "== 1/6 pull the REAL sftpgo image (public, anonymous) =="
docker pull "$IMG"

echo "== 2/6 extract /app (the exact tree the plain agent explored) =="
CID=$(docker create "$IMG")
rm -rf "$APP"; mkdir -p "$APP"
docker cp "$CID:/app/." "$APP/"
docker rm "$CID" >/dev/null
echo "   /app: $(find "$APP" -type f | wc -l) files ( $(find "$APP" -name '*.go' | wc -l) Go )"

echo "== 3/6 build semfs examples with gliner-kg (one build; non-KG examples ignore the feature) =="
cd "$REPO"
cargo build --release -p semfs-core --features gliner-kg \
  --example seed_dir --example build_kg --example materialize_kg --example materialize_fs
SD="$REPO/target/release/examples/seed_dir"
BK="$REPO/target/release/examples/build_kg"
MK="$REPO/target/release/examples/materialize_kg"
MF="$REPO/target/release/examples/materialize_fs"

echo "== 4/6 PHASE 1 embed (seed_dir → chunks + vectors, fastembed CPU) =="
rm -f "$DB" "$DB"-wal "$DB"-shm
"$SD" "$DB" "$APP"

echo "== 4b materialize FS tree (materialize_fs) — REQUIRED, else the seed is search-only =="
# Without this, fs_inode/fs_dentry/fs_data are empty → the mount serves NO files
# (empty ls, no cat). seed_dir only writes chunks/vchunks. Run it after seed_dir.
# (Order note: retrieval is path-based — grep searches chunks by filepath, the mount
#  serves path→ino→fs_data — so the seed is functional regardless of ino allocation.
#  seed-verify's ino-join may under-report coverage; it does NOT reflect functional
#  reachability, which we confirmed: every chunked file is both served and searchable.)
"$MF" "$DB" "$APP"

echo "== 5/6 PHASE 2 KG (AST Go tree-sitter + gliner doc lane; GPU-free) + PHASE 3 finalize =="
# gliner-kg is the default doc-lane when compiled. SEMFS_KG_EXTRACTOR=llm forces the LLM path.
# The gliner model (fastino/gliner2-large-v1) downloads once into $HF_HOME.
# NOTE: doc lane currently also runs gliner on css/svg/sql (harmless no-ops); scope to
# prose later via a corpus prefilter if build time matters on larger repos.
"$BK" "$DB" "$APP"
"$MK" "$DB"

echo "== 6/6 inspect seed (must have vectors + dual-lane KG + communities for all arms) =="
python3 - "$DB" <<'PY'
import sqlite3, sys, os
db = sys.argv[1]
print(f"seed: {db}  ({os.path.getsize(db)/1e6:.1f} MB)")
con = sqlite3.connect(db); cur = con.cursor()
tabs = {r[0] for r in cur.execute("SELECT name FROM sqlite_master WHERE type='table'")}
def n(t):
    try: return cur.execute(f"SELECT COUNT(*) FROM {t}").fetchone()[0]
    except Exception as e: return f"err({e})"
for t in ["chunks","graph_entity","graph_relation","edges","graph_community"]:
    if t in tabs: print(f"  {t:16}: {n(t)}")
ok = all(t in tabs for t in ["chunks","graph_entity","edges","graph_community"])
print("  READY for plain/ppr_off/ppr_on/ppr_map:", ok)
con.close()
PY
echo "SEED READY → $DB   (commit $BASE_COMMIT)"
