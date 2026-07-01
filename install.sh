#!/bin/bash
# semfs installer — builds from source.
#
# No prebuilt release binaries required: this clones the repo and compiles
# `semfs` with cargo, then installs it. Requires `git` and the Rust toolchain.
#
#   curl -fsSL https://raw.githubusercontent.com/saralai/semfs/main/install.sh | bash
#   ./install.sh [git-ref]          # ref defaults to "main"
#   SEMFS_INSTALL_DIR=/usr/local/bin ./install.sh
set -euo pipefail

REPO="saralai/semfs"
REPO_URL="https://github.com/${REPO}.git"
REF="${1:-main}"                                   # branch or tag to build
INSTALL_DIR="${SEMFS_INSTALL_DIR:-$HOME/.local/bin}"

command -v git >/dev/null 2>&1   || { echo "error: git is required" >&2; exit 1; }
command -v cargo >/dev/null 2>&1 || {
    echo "error: the Rust toolchain (cargo) is required — install it from https://rustup.rs" >&2
    exit 1
}

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

echo "Fetching ${REPO} (${REF})..."
git clone --depth 1 --branch "$REF" "$REPO_URL" "$workdir/semfs" 2>/dev/null \
    || git clone --depth 1 "$REPO_URL" "$workdir/semfs"   # fallback if REF is a commit / default branch differs

echo "Building semfs (release) — this can take a few minutes..."
( cd "$workdir/semfs" && cargo build --release --bin semfs )

mkdir -p "$INSTALL_DIR"
install -m 0755 "$workdir/semfs/target/release/semfs" "$INSTALL_DIR/semfs"

echo ""
echo "✅ semfs installed to $INSTALL_DIR/semfs"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "⚠  $INSTALL_DIR is not on your PATH. Add it with:" >&2
       echo "     export PATH=\"$INSTALL_DIR:\$PATH\"" >&2 ;;
esac
"$INSTALL_DIR/semfs" --version 2>/dev/null || true
