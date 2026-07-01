#!/bin/sh
# next_plaid grep shim — routes the agent's `grep` to colgrep (late interaction) against
# the baked, relocated index. Installed at /opt/semfs-shims/grep (first on PATH).
#
# The agent often calls `grep -r "terms" <path> --include=*.md -l`; colgrep takes a bare
# query, so we DROP flags and path-like args and keep the search terms. Multi-word queries
# arrive as one quoted arg, so the last non-flag/non-path arg is the query.
#
# Env: WB_NP_CORPUS (cwd → colgrep finds the index by path-hash), WB_NP_MODEL (id/dir),
#      WB_NP_MERGE (Config-C → rrf_merge), XDG_DATA_HOME (→ baked _xdg).
q=""
for a in "$@"; do
    case "$a" in
        -*) ;;            # flag (-r, -l, -n, --include=…)
        /*|./*|*/*) ;;    # a path (search root / file)
        "") ;;
        *) q="$a" ;;      # search term (keep the last quoted non-flag, non-path arg)
    esac
done
[ -z "$q" ] && q="$*"     # fallback: whatever was passed
if [ -n "$WB_NP_MERGE" ]; then
    exec python3 /opt/np/rrf_merge.py "$q"
fi
cd "${WB_NP_CORPUS:-/srv/np/corpus}" 2>/dev/null || true
exec colgrep --model "$WB_NP_MODEL" "$q"
