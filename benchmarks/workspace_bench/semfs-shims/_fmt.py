#!/usr/bin/env python3
"""Format `semfs grep` stdout into the ripgrep output shape the caller asked for.

semfs grep stdout is `<filepath>:<rest>` lines (rest may be `L1-L2:chunk` or a
chunk), blank-line separated; `#` header lines go to stderr so stdin here is
clean. We map container-relative paths to absolute mount paths and emit:

  --mode files   -> one path per matching file (rg -l / --files-with-matches)
  --mode count   -> path:count                (rg -c / --count)
  --json 1       -> rg --json events (begin/match/end + summary)
  else (content) -> path:text                 (rg default content mode)
"""
import sys, json, argparse

ap = argparse.ArgumentParser()
ap.add_argument("--mode", default="content")   # content | files | count
ap.add_argument("--json", default="0")
ap.add_argument("--mount", default="")
ap.add_argument("--pattern", default="")
a = ap.parse_args()

mount = a.mount.rstrip("/")

def to_abs(fp):
    if not mount or fp.startswith(mount):
        return fp
    return mount + fp if fp.startswith("/") else mount + "/" + fp

results = []  # (abspath, text)
for raw in sys.stdin:
    line = raw.rstrip("\n")
    if not line or line.startswith("#"):
        continue
    fp, _, text = line.partition(":")
    fp = fp.strip()
    if not fp or fp == "(unknown)":
        continue
    # If `rest` was `L1-L2:chunk`, drop the leading line-range for display text.
    head, sep, tail = text.partition(":")
    if sep and "-" in head and head.replace("-", "").isdigit():
        text = tail
    results.append((to_abs(fp), text))

if a.json == "1":
    for fp, text in results:
        print(json.dumps({"type": "begin", "data": {"path": {"text": fp}}}))
        print(json.dumps({"type": "match", "data": {
            "path": {"text": fp},
            "lines": {"text": text + "\n"},
            "line_number": 1, "absolute_offset": 0,
            "submatches": [{"match": {"text": a.pattern}, "start": 0, "end": len(a.pattern)}],
        }}))
        print(json.dumps({"type": "end", "data": {
            "path": {"text": fp}, "binary_offset": None,
            "stats": {"elapsed": {"secs": 0, "nanos": 0, "human": "0.0s"},
                      "searches": 1, "searches_with_match": 1, "bytes_searched": 0,
                      "bytes_printed": 0, "matched_lines": 1, "matches": 1},
        }}))
    n = len(results)
    print(json.dumps({"type": "summary", "data": {
        "elapsed_total": {"secs": 0, "nanos": 0, "human": "0.0s"},
        "stats": {"searches": n, "searches_with_match": n, "matched_lines": n, "matches": n}}}))
elif a.mode == "files":
    seen = set()
    for fp, _ in results:
        if fp not in seen:
            seen.add(fp); print(fp)
elif a.mode == "count":
    from collections import Counter
    for fp, c in Counter(fp for fp, _ in results).items():
        print(f"{fp}:{c}")
else:  # content
    for fp, text in results:
        print(f"{fp}:{text}")
