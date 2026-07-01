#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_ROOT="/Users/marmikpandya/skills"
DEST_ROOT="$ROOT_DIR/codex-home/skills"

if [[ ! -d "$SOURCE_ROOT" ]]; then
  echo "Missing source skill repo: $SOURCE_ROOT" >&2
  exit 1
fi

rm -rf "$DEST_ROOT"
mkdir -p "$DEST_ROOT"

while IFS= read -r src; do
  name="$(basename "$src")"
  cp -R "$src" "$DEST_ROOT/$name"
done < <(
  find "$SOURCE_ROOT" \
    \( -path '*/.git' -o -path '*/.git/*' \) -prune -o \
    -mindepth 3 -maxdepth 3 -type d -path '*/skills/*' -print | sort
)

echo "Synced $(find "$DEST_ROOT" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ') skills into $DEST_ROOT"
