#!/bin/sh
# semfs-np — a `semfs`-shaped wrapper whose `grep` backend is colgrep (next-plaid late
# interaction). The real semfs grep shim invokes `$SEMFS_BIN grep <pattern> <path>`; we
# set SEMFS_BIN=semfs-np so that call routes to colgrep instead of the semfs daemon. The
# agent's affordance is unchanged (`semfs grep`, exactly like ppr_on) — only the engine
# differs → a clean A/B. cwd = corpus so colgrep finds the baked index by path-hash;
# XDG_DATA_HOME (the baked _xdg) + WB_NP_MODEL come from the cell env.
# Output = colgrep's ranked file/excerpt format on stdout (≈ semfs grep).
if [ "$1" != grep ]; then
    exec /usr/local/bin/semfs "$@"   # any non-grep semfs call → real semfs (none expected)
fi
shift
pattern="$1"
cd "${WB_NP_CORPUS:-/srv/np/corpus}" 2>/dev/null || true
if [ -n "$WB_NP_MERGE" ]; then       # Config-C: two indices → RRF merge
    exec python3 /opt/np/rrf_merge.py "$pattern"
fi
exec colgrep --model "$WB_NP_MODEL" "$pattern"
