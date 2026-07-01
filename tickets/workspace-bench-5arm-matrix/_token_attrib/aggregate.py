#!/usr/bin/env python3
"""Aggregate every in-scope NVFP4 cell of the 9-arm WB token-attribution matrix."""
import json, os, re, sys
from collections import defaultdict

RUNS = "/Users/marmikpandya/semantic-filesystem/tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs"
CASES = ["15", "44", "53", "95", "175"]

# Logical arm -> (arm_dirname_token, set of allowed rep-prefixes)
# rep is the suffix after the arm token. We match by prefix.
SCOPE = {
    "1_plain":               ("plain",                 ["ra1p", "rrb1p", "rrf1c"]),
    "2_compress":            ("nokg",                  ["ra2c", "rrb1c"]),
    "3_comp_dedup":          ("nokg",                  ["ra3d", "rrb1d"]),
    "4_best":                ("best",                  ["ra47", "rrb1m", "rrf2a"]),
    "5_hkg_edges":           ("hiddenkg_edges",        ["ra47", "rrb1m", "rrf1a", "rrf2a"]),
    "6_hkg_rerank":          ("hiddenkg",              ["ra47", "rrb1m"]),
    "7_hkg_l7":              ("hiddenkg_l7",           ["ra47", "rrb1m"]),
    "8_hkg_retrieval":       ("hiddenkg_retrieval",    ["ra89"]),
    "9_hkg_retrieval_l7":    ("hiddenkg_retrieval_l7", ["ra89"]),
}

def parse_cell(dirname):
    """Return (case, arm_token, rep) from pm_codex_<case>_<armtoken>_<rep>."""
    m = re.match(r"pm_codex_(\d+)_(.+)_([a-zA-Z0-9]+)$", dirname)
    if not m:
        return None
    return m.group(1), m.group(2), m.group(3)

def classify(dirname):
    """Map a directory to a logical arm, respecting exact arm-token + rep-prefix."""
    parsed = parse_cell(dirname)
    if not parsed:
        return None
    case, armtok, rep = parsed
    if case not in CASES:
        return None
    # We must disambiguate arm tokens that are prefixes of each other
    # (e.g. hiddenkg vs hiddenkg_edges vs hiddenkg_retrieval_l7).
    # parse_cell greedily took everything up to the last underscore as armtok,
    # so armtok already excludes the rep. Good.
    for logical, (tok, prefixes) in SCOPE.items():
        if armtok != tok:
            continue
        for p in prefixes:
            if rep.startswith(p):
                return logical, case, rep
    return None

def grep_call_class(cmd):
    """Is this tool command a semfs grep, a file read, or other?"""
    c = cmd.lower()
    if "semfs grep" in c:
        return "semfs_grep"
    if re.search(r"\b(cat|sed|head|tail|less|nl|awk)\b", c) and ("cat >" not in c and "cat <<" not in c and "<<" not in c):
        # a read-ish command but not a heredoc write
        return "read"
    if "grep" in c or "rg " in c or "find " in c or "ls " in c:
        return "search"
    if "cat >" in c or "<<" in c or "tee " in c or "python" in c and ">" in c:
        return "write"
    return "other"

def load_cell(dirname):
    d = os.path.join(RUNS, dirname)
    cls = classify(dirname)
    if not cls:
        return None
    logical, case, rep = cls
    out = {"dir": dirname, "arm": logical, "case": case, "rep": rep}
    try:
        r = json.load(open(os.path.join(d, "result.json")))
    except Exception as e:
        out["error"] = f"result.json: {e}"
        return out
    u = r.get("usage", {}) or {}
    out["status"] = r.get("status")
    out["prompt_tokens"] = u.get("prompt_tokens", 0)
    out["completion_tokens"] = u.get("completion_tokens", 0)
    out["total_tokens"] = u.get("total_tokens", r.get("tokens", 0))
    out["cache_read"] = u.get("cache_read", None)
    out["used_semfs_grep"] = r.get("used_semfs_grep")
    out["n_deliverables"] = len(r.get("deliverables", []) or [])
    out["followed_filename"] = r.get("followed_filename")
    out["deliverable_content_len"] = len(r.get("deliverable_content", "") or "")

    # real turns from agent.json
    turns = None
    tool_outputs = []  # list of (idx, cmd, output_len) for completed tools, in order
    try:
        a = json.load(open(os.path.join(d, "agent.json")))
        et = a["trace"]["executionTrace"]
        comp = [e for e in et if e.get("type") == "tool" and e.get("status") == "completed"]
        turns = len(comp)
        for e in comp:
            cmd = (e.get("input") or {}).get("command", "")
            ol = len(e.get("output") or "")
            tool_outputs.append((cmd, ol))
        out["assistant_text_entries"] = sum(1 for e in et if e.get("type") == "text" and e.get("role") == "assistant")
    except Exception as e:
        out["agent_error"] = str(e)
    out["turns"] = turns if turns is not None else (r.get("calls", 0) // 2)
    out["tool_outputs"] = tool_outputs

    # classify tool calls
    n_grep = n_read = n_search = n_write = n_other = 0
    grep_output_chars = 0
    max_tool_output = 0
    max_tool_cmd = ""
    for cmd, ol in tool_outputs:
        k = grep_call_class(cmd)
        if k == "semfs_grep":
            n_grep += 1; grep_output_chars += ol
        elif k == "read":
            n_read += 1
        elif k == "search":
            n_search += 1
        elif k == "write":
            n_write += 1
        else:
            n_other += 1
        if ol > max_tool_output:
            max_tool_output = ol; max_tool_cmd = cmd[:90]
    out["n_semfs_grep"] = n_grep
    out["n_read"] = n_read
    out["n_search"] = n_search
    out["n_write"] = n_write
    out["n_other"] = n_other
    out["grep_output_chars"] = grep_output_chars
    out["max_tool_output_chars"] = max_tool_output
    out["max_tool_cmd"] = max_tool_cmd
    out["total_tool_output_chars"] = sum(ol for _, ol in tool_outputs)

    # re-prefill weighted cost: each output at position s re-paid (T-s) more times => weight = (T - s)
    # approximate prompt-token contribution ~ sum_s output_len[s] * (T - s) / 4 chars-per-token
    T = len(tool_outputs)
    weighted = 0
    for s, (_, ol) in enumerate(tool_outputs):
        weighted += ol * (T - 1 - s)  # number of later turns that re-prefill it
    out["reprefill_weighted_chars"] = weighted

    # accuracy
    acc_passed = acc_total = None
    rb = os.path.join(d, "rubrics_judge--seed-2.0-lite-judge.json")
    if os.path.exists(rb):
        try:
            j = json.load(open(rb))
            s = j.get("summary", {}) or {}
            acc_passed = s.get("passed")
            acc_total = s.get("total")
        except Exception as e:
            out["judge_error"] = str(e)
    # accuracy = 0 if no deliverable
    if out["n_deliverables"] == 0:
        out["accuracy"] = 0.0
        out["acc_note"] = "no_deliverable"
    elif acc_passed is not None and acc_total:
        out["accuracy"] = acc_passed / acc_total
        out["acc_passed"] = acc_passed
        out["acc_total"] = acc_total
    else:
        out["accuracy"] = None
        out["acc_note"] = "no_judge"
    return out

def main():
    cells = []
    for dirname in sorted(os.listdir(RUNS)):
        if not dirname.startswith("pm_codex_"):
            continue
        c = load_cell(dirname)
        if c:
            cells.append(c)
    json.dump(cells, open("/Users/marmikpandya/semantic-filesystem/tickets/workspace-bench-5arm-matrix/_token_attrib/cells.json", "w"), indent=1)
    print(f"IN-SCOPE CELLS: {len(cells)}")
    # quick per-arm count
    byarm = defaultdict(list)
    for c in cells:
        byarm[c["arm"]].append(c)
    for arm in sorted(byarm):
        print(f"  {arm:28s} n={len(byarm[arm])}")
    return cells

if __name__ == "__main__":
    main()
