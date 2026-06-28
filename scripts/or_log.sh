#!/usr/bin/env bash
# Fetch an OpenRouter generation's input messages + completion from the dashboard
# /logs server-action, self-refreshing the short-lived Clerk __session each call.
#
# Usage:   scripts/or_log.sh <generationId>            # parsed view
#          scripts/or_log.sh <generationId> --raw      # raw RSC payload
#
# Auth:    needs the long-lived Clerk client cookie in scripts/.or_client_cookie
#          (a line like:  __client=<jwt>; __client_uat=<ts> ).
#          When that expires, re-copy the `clerk.openrouter.ai/.../tokens` request
#          from DevTools and replace the file's contents.
set -euo pipefail

GEN="${1:?usage: or_log.sh <generationId> [--raw]}"
RAW="${2:-}"
DIR="$(cd "$(dirname "$0")" && pwd)"
COOKIE_FILE="${OR_COOKIE_FILE:-$DIR/.or_client_cookie}"
SID="${OR_SID:-sess_3F7ch5Uie5VIOAbUdGGgqw0kHxD}"
NEXT_ACTION="${OR_NEXT_ACTION:-404609ded39b9e390538fbe335e8970f2f8bed9b4e}"
ROUTER_TREE='%5B%22%22%2C%7B%22children%22%3A%5B%22(user)%22%2C%7B%22children%22%3A%5B%22(dashboard)%22%2C%7B%22children%22%3A%5B%22logs%22%2C%7B%22children%22%3A%5B%22__PAGE__%22%2C%7B%7D%2Cnull%2Cnull%2C0%5D%7D%2Cnull%2Cnull%2C0%5D%7D%2Cnull%2Cnull%2C0%5D%7D%2Cnull%2Cnull%2C4%5D%7D%2Cnull%2Cnull%2C28%5D'

[ -f "$COOKIE_FILE" ] || { echo "missing $COOKIE_FILE — paste the __client cookie line into it" >&2; exit 1; }
CLIENT="$(cat "$COOKIE_FILE")"

# 1) mint a fresh __session via the long-lived client cookie
JWT="$(curl -s "https://clerk.openrouter.ai/v1/client/sessions/$SID/tokens?__clerk_api_version=2025-11-10&_clerk_js_version=5.125.13" \
  -H 'content-type: application/x-www-form-urlencoded' -b "$CLIENT" \
  -H 'origin: https://openrouter.ai' -H 'referer: https://openrouter.ai/' \
  --data-raw 'organization_id=' | python3 -c "import sys,json;print(json.load(sys.stdin).get('jwt',''))")"
[ -n "$JWT" ] || { echo "refresh failed — client cookie likely expired; re-copy the clerk .../tokens request" >&2; exit 2; }

# 2) call /logs with the full cookie jar (bare __session is rejected)
JAR="__client_uat=1781429663; clerk_active_context=$SID:; __session=$JWT"
RESP="$(curl -s "https://openrouter.ai/logs?transaction=$GEN" \
  -H 'accept: text/x-component' -H 'content-type: text/plain;charset=UTF-8' -b "$JAR" \
  -H "next-action: $NEXT_ACTION" -H "next-router-state-tree: $ROUTER_TREE" \
  -H 'origin: https://openrouter.ai' -H "referer: https://openrouter.ai/logs?transaction=$GEN" \
  -H 'user-agent: Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36' \
  --data-raw "[{\"generationId\":\"$GEN\"}]")"

if [ "$RAW" = "--raw" ]; then
  printf '%s\n' "$RESP"
else
  printf '%s\n' "$RESP" | python3 "$DIR/_or_parse.py"
fi
