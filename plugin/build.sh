#!/usr/bin/env bash
# Build the tokopt gateway and bundle it into plugin/bin/ — the only path that
# survives a marketplace install (installed plugins can't reach the workspace
# target/). The binary itself is gitignored (a rebuildable artifact); run this
# after a fresh clone, or before packing a release tarball.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
cargo build --release -p tokopt-gateway --manifest-path "$repo/Cargo.toml"
mkdir -p "$here/bin"
cp -f "$repo/target/release/tokopt-gateway" "$here/bin/tokopt-gateway"
echo "bundled: $here/bin/tokopt-gateway"
