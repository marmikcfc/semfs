#!/usr/bin/env python3
"""
Label EVERY Claude-Code generation in openrouter_logs.csv by its own work_dir
(reliable per-run signal), so run-token attribution is exact (not per-device-hash).

Efficient: mints one Clerk __session and reuses it for ~50s of fetches before
re-minting. Resumable: caches gen_id -> label to scripts/.gen_labels.json after
every fetch, so a re-run only fetches what's missing.
"""
import csv, json, os, re, time, subprocess, urllib.request, urllib.parse, sys

ROOT = subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip()
CSV = os.path.join(ROOT, "openrouter_logs.csv")
COOKIE_FILE = os.path.join(ROOT, "scripts/.or_client_cookie")
CACHE = os.path.join(ROOT, "scripts/.gen_labels.json")
SID = "sess_3F7ch5Uie5VIOAbUdGGgqw0kHxD"
NEXT_ACTION = "404609ded39b9e390538fbe335e8970f2f8bed9b4e"
ROUTER_TREE = "%5B%22%22%2C%7B%22children%22%3A%5B%22(user)%22%2C%7B%22children%22%3A%5B%22(dashboard)%22%2C%7B%22children%22%3A%5B%22logs%22%2C%7B%22children%22%3A%5B%22__PAGE__%22%2C%7B%7D%2Cnull%2Cnull%2C0%5D%7D%2Cnull%2Cnull%2C0%5D%7D%2Cnull%2Cnull%2C0%5D%7D%2Cnull%2Cnull%2C4%5D%7D%2Cnull%2Cnull%2C28%5D"
LABEL_RE = re.compile(r"(pm|kaifa)_(claude|codex)_[A-Za-z0-9_]+_(r\d+|\d+_(kg|nokg))|/home/user/run/([A-Za-z0-9_.\-]+)")
WORKDIR_RE = re.compile(r"/home/user/run/([A-Za-z0-9_\-]+)")

CLIENT = open(COOKIE_FILE).read().strip()


UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36"


def mint_session():
    url = f"https://clerk.openrouter.ai/v1/client/sessions/{SID}/tokens?__clerk_api_version=2025-11-10&_clerk_js_version=5.125.13"
    out = subprocess.run(["curl", "-s", url, "-H", "content-type: application/x-www-form-urlencoded",
                          "-b", CLIENT, "-H", "origin: https://openrouter.ai", "-H", "referer: https://openrouter.ai/",
                          "-H", f"user-agent: {UA}", "--data-raw", "organization_id="],
                         capture_output=True, text=True, timeout=30).stdout
    return json.loads(out)["jwt"]


def fetch_workdir(gen_id, jwt):
    url = f"https://openrouter.ai/logs?transaction={urllib.parse.quote(gen_id)}"
    jar = f"__client_uat=1781429663; clerk_active_context={SID}:; __session={jwt}"
    body = json.dumps([{"generationId": gen_id}])
    txt = subprocess.run(["curl", "-s", url, "-H", "accept: text/x-component",
                          "-H", "content-type: text/plain;charset=UTF-8", "-b", jar,
                          "-H", f"next-action: {NEXT_ACTION}", "-H", f"next-router-state-tree: {ROUTER_TREE}",
                          "-H", "origin: https://openrouter.ai",
                          "-H", f"referer: https://openrouter.ai/logs?transaction={gen_id}",
                          "-H", f"user-agent: {UA}", "--data-raw", body],
                         capture_output=True, text=True, timeout=30).stdout
    m = WORKDIR_RE.search(txt)
    return m.group(1) if m else ("401" if '"code":401' in txt else None)


def main():
    rows = list(csv.DictReader(open(CSV)))
    gens = [r["generation_id"] for r in rows if r["app_name"] == "Claude Code"]
    cache = json.load(open(CACHE)) if os.path.exists(CACHE) else {}
    todo = [g for g in gens if g not in cache]
    print(f"{len(gens)} Claude-Code gens, {len(cache)} cached, {len(todo)} to fetch", flush=True)

    jwt = None
    minted = 0.0
    done = 0
    for g in todo:
        if jwt is None or (time.time() - minted) > 45:
            jwt = mint_session(); minted = time.time()
        try:
            wd = fetch_workdir(g, jwt)
        except Exception as e:
            wd = None
            print(f"  {g}: ERR {e}", flush=True)
        if wd == "401":            # session died early; re-mint and retry once
            jwt = mint_session(); minted = time.time()
            try: wd = fetch_workdir(g, jwt)
            except Exception: wd = None
        cache[g] = wd
        done += 1
        if done % 25 == 0:
            json.dump(cache, open(CACHE, "w"))
            print(f"  {done}/{len(todo)} (last {g} -> {wd})", flush=True)
    json.dump(cache, open(CACHE, "w"))
    labeled = sum(1 for v in cache.values() if v)
    print(f"done. {labeled}/{len(cache)} labeled; distinct workdirs: {len(set(v for v in cache.values() if v))}", flush=True)


if __name__ == "__main__":
    main()
