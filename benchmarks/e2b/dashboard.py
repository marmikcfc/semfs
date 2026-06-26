#!/usr/bin/env python3
"""Self-contained live web dashboard for an in-progress E2B benchmark run.

STDLIB ONLY. Serves a dark-themed auto-refreshing page that monitors three
files written live by the run: manifest.json, results.jsonl, judged.jsonl.

Usage:
    python3 dashboard.py --out <DIR> [--port 8765]
"""

import argparse
import glob
import json
import os
import re
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

# ---------------------------------------------------------------------------
# File reading helpers (re-read every /status call; files are small + grow)
# ---------------------------------------------------------------------------

OUT_DIR = None  # set in main()
PLAIN_OUT_DIR = None  # set in main()

_USAGE_CACHE = {}  # label -> (prompt_tokens, completion_tokens); results.jsonl has only the total


def _cell_io(label):
    """Per-cell input/output token split from result.json usage, cached by label (read once)."""
    if label in _USAGE_CACHE:
        return _USAGE_CACHE[label]
    pin = pout = None
    try:
        with open(os.path.join(OUT_DIR, label, "result.json"), "r", encoding="utf-8") as fh:
            u = (json.load(fh).get("usage") or {})
        if isinstance(u.get("prompt_tokens"), (int, float)):
            pin = float(u["prompt_tokens"])
        if isinstance(u.get("completion_tokens"), (int, float)):
            pout = float(u["completion_tokens"])
    except Exception:
        pass
    _USAGE_CACHE[label] = (pin, pout)
    return _USAGE_CACHE[label]
PLAIN_CASES = {}  # {case: {...}} loaded ONCE at startup, cached in memory
PLAIN_CELLS = []  # flat per-cell plain records (clean r1/r2/r3) for persona/rep breakdowns
PLAIN_LOADED = False  # True if PLAIN_OUT_DIR existed and was scanned


def _read_manifest():
    """Return the manifest dict, or None if absent / unparseable."""
    p = Path(OUT_DIR) / "manifest.json"
    try:
        with open(p, "r", encoding="utf-8") as fh:
            return json.load(fh)
    except Exception:
        return None


def _read_jsonl(name):
    """Read a .jsonl file fully, tolerating absent files and bad/partial lines.

    Returns a list of dicts (only successfully-parsed object lines).
    """
    p = Path(OUT_DIR) / name
    rows = []
    try:
        with open(p, "r", encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except Exception:
                    # malformed or trailing partial line — skip it
                    continue
                if isinstance(obj, dict):
                    rows.append(obj)
    except FileNotFoundError:
        return []
    except Exception:
        return rows
    return rows


# ---------------------------------------------------------------------------
# PLAIN per-case loader (static — scanned ONCE at startup, cached in memory)
# ---------------------------------------------------------------------------

# cell dirs are named  pm_codex_<case>_plain_r<rep>/
_PLAIN_DIR_RE = re.compile(r"^pm_codex_(.+)_plain_r(.*)$")


def load_plain_cases(plain_dir):
    """Scan <plain_dir> for pm_codex_<case>_plain_r<rep>/ cells and aggregate
    per case. Returns ({case: {...}}, loaded_bool).

    Per cell:
      - accuracy: the single rubrics_judge--*.json -> summary.passed / summary.total
      - tokens / status / calls: result.json
    Per case (across reps):
      plain_passed   = Σ passed
      plain_total    = Σ total
      plain_acc      = plain_passed / plain_total  (None if total == 0)
      plain_tok      = mean(tokens for ok reps with non-null tokens)
      plain_reps     = count of cell dirs
      plain_timeouts = count(status == "timeout")
    Defensive: every file read wrapped; unreadable cells skipped silently.
    """
    if not plain_dir or not os.path.isdir(plain_dir):
        return {}, False

    agg = {}  # case -> mutable accumulator
    try:
        entries = os.listdir(plain_dir)
    except Exception:
        return {}, False

    for name in entries:
        m = _PLAIN_DIR_RE.match(name)
        if not m:
            continue
        case = m.group(1)
        # Keep ONLY the clean n=3 SEM-39 reps (r1/r2/r3). The shared plain dir is polluted
        # with old experimental rep labels (rP1p, ra1p1, rdiag, rfp1, rfrpl1, rrb1p, …) from
        # prior 5-arm-matrix sessions — those must NOT contaminate the plain baseline.
        rep = name.rsplit("_plain_r", 1)[-1]
        if rep not in ("1", "2", "3"):
            continue
        cell_dir = os.path.join(plain_dir, name)
        if not os.path.isdir(cell_dir):
            continue

        a = agg.setdefault(case, {
            "case": case,
            "passed": 0.0,
            "total": 0.0,
            "have_acc": False,
            "tokens": [],
            "in": [],
            "out": [],
            "calls": [],
            "reps": 0,
            "timeouts": 0,
        })
        a["reps"] += 1

        # accuracy from the single rubric file
        try:
            rubric_files = glob.glob(os.path.join(cell_dir, "rubrics_judge--*.json"))
            if rubric_files:
                with open(rubric_files[0], "r", encoding="utf-8") as fh:
                    rub = json.load(fh)
                summary = rub.get("summary") or {}
                tot = summary.get("total")
                pas = summary.get("passed")
                if isinstance(tot, (int, float)) and isinstance(pas, (int, float)):
                    a["passed"] += float(pas)
                    a["total"] += float(tot)
                    a["have_acc"] = True
        except Exception:
            pass

        # tokens / status from result.json
        try:
            with open(os.path.join(cell_dir, "result.json"), "r", encoding="utf-8") as fh:
                res = json.load(fh)
            status = res.get("status")
            if status == "timeout":
                a["timeouts"] += 1
            tok = res.get("tokens")
            if status == "ok" and isinstance(tok, (int, float)):
                a["tokens"].append(float(tok))
                u = res.get("usage") or {}
                if isinstance(u.get("prompt_tokens"), (int, float)):
                    a["in"].append(float(u["prompt_tokens"]))
                if isinstance(u.get("completion_tokens"), (int, float)):
                    a["out"].append(float(u["completion_tokens"]))
                if isinstance(res.get("calls"), (int, float)):
                    a["calls"].append(float(res["calls"]))
        except Exception:
            pass

    out = {}
    for case, a in agg.items():
        total = a["total"]
        passed = a["passed"]
        plain_acc = (passed / total) if (a["have_acc"] and total > 0) else None
        toks = a["tokens"]
        plain_tok = (sum(toks) / len(toks)) if toks else None
        mean1 = lambda xs: (sum(xs) / len(xs)) if xs else None
        plain_in, plain_out, plain_calls = mean1(a["in"]), mean1(a["out"]), mean1(a["calls"])
        out[case] = {
            "case": case,
            "plain_passed": passed,
            "plain_total": total,
            "plain_acc": plain_acc,
            "plain_tok": plain_tok,
            "plain_in": plain_in,
            "plain_out": plain_out,
            "plain_calls": plain_calls,
            "plain_reps": a["reps"],
            "plain_timeouts": a["timeouts"],
        }
    return out, True


def load_plain_cells(plain_dir):
    """Flat per-cell plain records (clean r1/r2/r3 only) so the dashboard can filter plain by
    persona (case set) AND by rep for the breakdown tables. One dict per cell."""
    cells = []
    if not plain_dir or not os.path.isdir(plain_dir):
        return cells
    for name in os.listdir(plain_dir):
        m = _PLAIN_DIR_RE.match(name)
        if not m:
            continue
        rep = name.rsplit("_plain_r", 1)[-1]
        if rep not in ("1", "2", "3"):
            continue
        cd = os.path.join(plain_dir, name)
        if not os.path.isdir(cd):
            continue
        rec = {"case": m.group(1), "rep": rep, "passed": None, "total": None,
               "tok": None, "in": None, "out": None, "calls": None}
        try:
            rf = glob.glob(os.path.join(cd, "rubrics_judge--*.json"))
            if rf:
                s = (json.load(open(rf[0])).get("summary") or {})
                if isinstance(s.get("total"), (int, float)):
                    rec["total"] = float(s["total"]); rec["passed"] = float(s.get("passed", 0))
        except Exception:
            pass
        try:
            r = json.load(open(os.path.join(cd, "result.json")))
            if r.get("status") == "ok":
                u = r.get("usage") or {}
                if isinstance(r.get("tokens"), (int, float)):
                    rec["tok"] = float(r["tokens"])
                if isinstance(u.get("prompt_tokens"), (int, float)):
                    rec["in"] = float(u["prompt_tokens"])
                if isinstance(u.get("completion_tokens"), (int, float)):
                    rec["out"] = float(u["completion_tokens"])
                if isinstance(r.get("calls"), (int, float)):
                    rec["calls"] = float(r["calls"])
        except Exception:
            pass
        cells.append(rec)
    return cells


# ---------------------------------------------------------------------------
# Score parsing
# ---------------------------------------------------------------------------

def parse_score(score):
    """Parse a score into (passed, total).

    - "a/b" string  -> (float(a), float(b))
    - numeric in [0,1] -> (float, 1.0)  (treat as a fraction)
    - numeric > 1   -> (float, None)    (raw count we can't normalize)
    - anything else -> (None, None)
    """
    if score is None:
        return (None, None)
    # "a/b" form preferred
    if isinstance(score, str):
        s = score.strip()
        if "/" in s:
            a, _, b = s.partition("/")
            try:
                pa = float(a.strip())
                tb = float(b.strip())
                return (pa, tb)
            except Exception:
                return (None, None)
        # plain numeric string
        try:
            v = float(s)
        except Exception:
            return (None, None)
        if v <= 1.0:
            return (v, 1.0)
        return (v, None)
    if isinstance(score, bool):
        return (1.0 if score else 0.0, 1.0)
    if isinstance(score, (int, float)):
        v = float(score)
        if v <= 1.0:
            return (v, 1.0)
        return (v, None)
    return (None, None)


def avg_accuracy(judged_rows):
    """Average accuracy across judged rows.

    Prefer sum(pass)/sum(total) when totals are known; else mean of raw passes.
    Returns a float fraction (0..1) or None if nothing usable.
    """
    sum_pass = 0.0
    sum_total = 0.0
    have_total = False
    raw_vals = []
    for r in judged_rows:
        pa, tb = parse_score(r.get("score"))
        if pa is None:
            continue
        if tb is not None and tb > 0:
            sum_pass += pa
            sum_total += tb
            have_total = True
        else:
            raw_vals.append(pa)
    if have_total and sum_total > 0:
        return sum_pass / sum_total
    if raw_vals:
        return sum(raw_vals) / len(raw_vals)
    return None


# ---------------------------------------------------------------------------
# Status computation
# ---------------------------------------------------------------------------

def _arm_block(res_sub, jud_sub, plain_cells_sub):
    """Full per-arm rows [plain, ppr_off, ppr_on] for an arbitrary subset (used for the
    cumulative, per-persona, and per-rep tables — same columns everywhere)."""
    mean1 = lambda xs: (sum(xs) / len(xs)) if xs else None
    rows = []
    if plain_cells_sub:
        pp = pt = 0.0; pin = []; pout = []; ptok = []; pcl = []
        for c in plain_cells_sub:
            if isinstance(c.get("total"), (int, float)) and c["total"]:
                pp += c["passed"]; pt += c["total"]
            for k, acc in (("in", pin), ("out", pout), ("tok", ptok), ("calls", pcl)):
                if isinstance(c.get(k), (int, float)):
                    acc.append(c[k])
        n = len(plain_cells_sub)
        rows.append({"arm": "plain", "done": n, "ok": n, "failed": 0,
                     "accuracy": (pp / pt) if pt else None, "mean_in": mean1(pin),
                     "mean_out": mean1(pout), "mean_tokens": mean1(ptok),
                     "mean_calls": mean1(pcl), "judged_n": n})
    for arm in ("ppr_off", "ppr_on"):
        ar = [r for r in res_sub if r.get("arm") == arm]
        done = len(ar); failed = sum(1 for r in ar if r.get("status") != "ok"); ok = done - failed
        aj = [j for j in jud_sub if j.get("arm") == arm]
        okr = [r for r in ar if r.get("status") == "ok"]
        tok = [r["tokens"] for r in okr if isinstance(r.get("tokens"), (int, float))]
        cl = [r["calls"] for r in okr if isinstance(r.get("calls"), (int, float))]
        ios = [_cell_io(r["label"]) for r in okr if r.get("label")]
        ins = [i for i, o in ios if isinstance(i, (int, float))]
        outs = [o for i, o in ios if isinstance(o, (int, float))]
        rows.append({"arm": arm, "done": done, "ok": ok, "failed": failed,
                     "accuracy": avg_accuracy(aj), "mean_in": mean1(ins), "mean_out": mean1(outs),
                     "mean_tokens": mean1(tok), "mean_calls": mean1(cl), "judged_n": len(aj)})
    return rows


def compute_status():
    now = time.time()
    manifest = _read_manifest()

    if not manifest:
        return {
            "ready": False,
            "now": now,
            "message": "waiting for run to start (manifest.json not found)",
            "out_dir": OUT_DIR,
        }

    results = _read_jsonl("results.jsonl")
    judged = _read_jsonl("judged.jsonl")

    arms = manifest.get("arms") or []
    personas = manifest.get("personas") or []
    reps = manifest.get("reps") or 0
    cases = manifest.get("cases") or {}
    total_cells = manifest.get("total_cells")
    if not isinstance(total_cells, int) or total_cells <= 0:
        # derive a fallback if missing
        derived = 0
        for p in personas:
            derived += len(cases.get(p, [])) * (reps or 1) * (len(arms) or 1)
        total_cells = derived
    started_at = manifest.get("started_at")
    elapsed = None
    if isinstance(started_at, (int, float)):
        elapsed = max(0.0, now - float(started_at))

    done = len(results)
    failed = sum(1 for r in results if r.get("status") != "ok")
    ok = done - failed
    pct = (100.0 * done / total_cells) if total_cells else 0.0

    # ---- per-arm summary ----
    arm_summary = []
    arm_acc = {}  # arm -> mean accuracy fraction (or None)
    arm_tok = {}  # arm -> mean tokens (or None)
    for arm in arms:
        arm_results = [r for r in results if r.get("arm") == arm]
        arm_done = len(arm_results)
        arm_failed = sum(1 for r in arm_results if r.get("status") != "ok")
        arm_ok = arm_done - arm_failed

        arm_judged = [r for r in judged if r.get("arm") == arm]
        acc = avg_accuracy(arm_judged)
        arm_acc[arm] = acc

        ok_results = [r for r in arm_results if r.get("status") == "ok"]
        toks = [r.get("tokens") for r in ok_results if isinstance(r.get("tokens"), (int, float))]
        mean_tok = (sum(toks) / len(toks)) if toks else None
        arm_tok[arm] = mean_tok

        # input/output split from each cell's result.json usage (results.jsonl lacks it)
        ios = [_cell_io(r["label"]) for r in ok_results if r.get("label")]
        ins = [i for i, o in ios if isinstance(i, (int, float))]
        outs = [o for i, o in ios if isinstance(o, (int, float))]
        mean_in = (sum(ins) / len(ins)) if ins else None
        mean_out = (sum(outs) / len(outs)) if outs else None

        calls = [r.get("calls") for r in ok_results if isinstance(r.get("calls"), (int, float))]
        mean_calls = (sum(calls) / len(calls)) if calls else None

        arm_summary.append({
            "arm": arm,
            "done": arm_done,
            "ok": arm_ok,
            "failed": arm_failed,
            "accuracy": acc,
            "mean_tokens": mean_tok,
            "mean_in": mean_in,
            "mean_out": mean_out,
            "mean_calls": mean_calls,
            "judged_n": len(arm_judged),
        })

    # plain (SEM-39) as a first-class per-arm row, on the SAME cases the ppr arms have
    # covered so far → apples-to-apples in the per-arm table (fraction, like the ppr rows).
    if PLAIN_CASES:
        covered = (set(str(r.get("case")) for r in results)
                   | set(str(r.get("case")) for r in judged))
        # count plain CELL-RUNS (reps x cases), not cases — to match the ppr arms' unit.
        pp = pt = 0.0; ptoks = []; pins = []; pouts = []; pcalls = []; pcells = 0
        for cs in covered:
            pc = PLAIN_CASES.get(cs)
            if not pc:
                continue
            pcells += int(pc.get("plain_reps") or 0)
            a, b = pc.get("plain_passed"), pc.get("plain_total")
            if isinstance(a, (int, float)) and isinstance(b, (int, float)) and b:
                pp += a; pt += b
            for key, acc_list in (("plain_tok", ptoks), ("plain_in", pins),
                                  ("plain_out", pouts), ("plain_calls", pcalls)):
                v = pc.get(key)
                if isinstance(v, (int, float)):
                    acc_list.append(v)
        mean1 = lambda xs: (sum(xs) / len(xs)) if xs else None
        arm_summary.insert(0, {
            "arm": "plain", "done": pcells, "ok": pcells, "failed": 0,
            "accuracy": (pp / pt) if pt else None,
            "mean_tokens": mean1(ptoks), "mean_in": mean1(pins), "mean_out": mean1(pouts),
            "mean_calls": mean1(pcalls), "judged_n": pcells,
        })

    # ---- A/B headline (ppr_on - ppr_off) ----
    headline = None
    if "ppr_on" in arms and "ppr_off" in arms:
        on_judged = [r for r in judged if r.get("arm") == "ppr_on"]
        off_judged = [r for r in judged if r.get("arm") == "ppr_off"]
        if on_judged and off_judged:
            acc_on = arm_acc.get("ppr_on")
            acc_off = arm_acc.get("ppr_off")
            tok_on = arm_tok.get("ppr_on")
            tok_off = arm_tok.get("ppr_off")
            acc_delta_pp = None
            if acc_on is not None and acc_off is not None:
                acc_delta_pp = (acc_on - acc_off) * 100.0
            tok_delta = None
            if tok_on is not None and tok_off is not None:
                tok_delta = tok_on - tok_off
            headline = {
                "acc_on": acc_on,
                "acc_off": acc_off,
                "acc_delta_pp": acc_delta_pp,
                "tok_on": tok_on,
                "tok_off": tok_off,
                "tok_delta": tok_delta,
            }

    # ---- 3-way aggregate on the MATCHED case set ----
    # "matched" = cases present in BOTH the ppr run (results/judged) AND plain.
    # apples-to-apples: identical case set across all three columns.
    def _arm_agg_over_cases(arm, case_set):
        """(acc_pct, mean_tok, n_ok_cells) for an arm over a set of cases.

        acc_pct = 100 * Σpassed / Σtotal across judged rows whose case ∈ case_set;
        mean_tok = mean tokens over ok result cells whose case ∈ case_set;
        n = count of ok result cells that contributed a token value.
        """
        sp = 0.0
        st = 0.0
        seen = False
        for r in judged:
            if r.get("arm") != arm:
                continue
            c = r.get("case")
            if c not in case_set and str(c) not in case_set:
                continue
            p = r.get("passed")
            t = r.get("total")
            if isinstance(p, (int, float)) and isinstance(t, (int, float)):
                sp += float(p)
                st += float(t)
                seen = True
            else:
                pa, tb = parse_score(r.get("score"))
                if pa is not None and tb is not None and tb > 0:
                    sp += pa
                    st += tb
                    seen = True
        acc_pct = (100.0 * sp / st) if (seen and st > 0) else None

        toks = []
        for r in results:
            if r.get("arm") != arm or r.get("status") != "ok":
                continue
            c = r.get("case")
            if c not in case_set and str(c) not in case_set:
                continue
            if isinstance(r.get("tokens"), (int, float)):
                toks.append(float(r.get("tokens")))
        mean_tok = (sum(toks) / len(toks)) if toks else None
        return acc_pct, mean_tok, len(toks)

    three_way = None
    if PLAIN_CASES:
        # cases present in the ppr run (any arm) — derive from results/judged directly
        # (case_persona is built later in this function, so don't reference it here).
        ppr_case_strs = (set(str(r.get("case")) for r in results if r.get("case") is not None)
                         | set(str(r.get("case")) for r in judged if r.get("case") is not None))
        plain_case_strs = set(str(c) for c in PLAIN_CASES.keys())
        matched = ppr_case_strs & plain_case_strs
        if matched:
            # plain aggregate over matched cases (sum passed / sum total)
            p_sp = 0.0
            p_st = 0.0
            p_seen = False
            p_toks = []
            for cs in matched:
                pc = PLAIN_CASES.get(cs) or {}
                pp = pc.get("plain_passed")
                pt = pc.get("plain_total")
                if isinstance(pp, (int, float)) and isinstance(pt, (int, float)) and pt > 0:
                    p_sp += float(pp)
                    p_st += float(pt)
                    p_seen = True
                ptok = pc.get("plain_tok")
                if isinstance(ptok, (int, float)):
                    p_toks.append(float(ptok))
            plain_acc = (100.0 * p_sp / p_st) if (p_seen and p_st > 0) else None
            plain_tok = (sum(p_toks) / len(p_toks)) if p_toks else None

            off_acc, off_tok, off_n = _arm_agg_over_cases("ppr_off", matched)
            on_acc, on_tok, on_n = _arm_agg_over_cases("ppr_on", matched)

            def _dpp(a, b):
                if a is None or b is None:
                    return None
                return a - b

            three_way = {
                "matched_cases": len(matched),
                "plain": {"acc": plain_acc, "tok": plain_tok, "n": len(p_toks)},
                "ppr_off": {"acc": off_acc, "tok": off_tok, "n": off_n},
                "ppr_on": {"acc": on_acc, "tok": on_tok, "n": on_n},
                "d_off_plain": _dpp(off_acc, plain_acc),
                "d_on_plain": _dpp(on_acc, plain_acc),
                "d_on_off": _dpp(on_acc, off_acc),
            }

    # ---- persona x arm grid ----
    grid = []
    for persona in personas:
        expected = len(cases.get(persona, [])) * (reps or 0)
        row = {"persona": persona, "expected": expected, "cells": []}
        for arm in arms:
            pa_results = [
                r for r in results
                if r.get("persona") == persona and r.get("arm") == arm
            ]
            cell_done = len(pa_results)
            pa_judged = [
                r for r in judged
                if r.get("persona") == persona and r.get("arm") == arm
            ]
            acc = avg_accuracy(pa_judged)
            complete = (expected > 0 and cell_done >= expected)
            row["cells"].append({
                "arm": arm,
                "done": cell_done,
                "expected": expected,
                "accuracy": acc,
                "complete": complete,
            })
        grid.append(row)

    # ---- per-persona 3-way (plain vs ppr_off vs ppr_on: accuracy% + mean tokens) ----
    persona_3way = []
    for persona in personas:
        pcases = set(str(c) for c in cases.get(persona, []))
        prow = {"persona": persona}
        pp = pt = 0.0; ptoks = []   # plain (clean n=3 from PLAIN_CASES, restricted to persona)
        for cs in pcases:
            pc = PLAIN_CASES.get(cs)
            if not pc:
                continue
            a, b = pc.get("plain_passed"), pc.get("plain_total")
            if isinstance(a, (int, float)) and isinstance(b, (int, float)) and b:
                pp += a; pt += b
            tk = pc.get("plain_tok")
            if isinstance(tk, (int, float)):
                ptoks.append(tk)
        prow["plain"] = {"acc": (100.0 * pp / pt) if pt else None,
                         "tok": (sum(ptoks) / len(ptoks)) if ptoks else None}
        for arm in ("ppr_off", "ppr_on"):
            ap = at = 0.0; atoks = []
            for j in judged:
                if j.get("arm") == arm and str(j.get("case")) in pcases:
                    a, b = j.get("passed"), j.get("total")
                    if not isinstance(b, (int, float)):
                        a, b = parse_score(j.get("score"))
                    if isinstance(a, (int, float)) and isinstance(b, (int, float)) and b:
                        ap += a; at += b
            for r in results:
                if r.get("arm") == arm and str(r.get("case")) in pcases and r.get("status") == "ok":
                    tk = r.get("tokens")
                    if isinstance(tk, (int, float)):
                        atoks.append(tk)
            prow[arm] = {"acc": (100.0 * ap / at) if at else None,
                         "tok": (sum(atoks) / len(atoks)) if atoks else None}
        persona_3way.append(prow)

    # ---- per-persona FULL breakdown (same columns as the cumulative per-arm table) ----
    covered_cases = set(str(r.get("case")) for r in results) | set(str(j.get("case")) for j in judged)
    persona_full = []
    for persona in personas:
        pcases = set(str(c) for c in cases.get(persona, []))
        rs = [r for r in results if r.get("persona") == persona]
        js = [j for j in judged if j.get("persona") == persona]
        pcs = [c for c in PLAIN_CELLS if c["case"] in pcases]
        persona_full.append({"persona": persona, "rows": _arm_block(rs, js, pcs)})

    # ---- per-rep FULL breakdown (rep 1/2/3 x arm) ----
    rep_full = []
    for rep in ("1", "2", "3"):
        rs = [r for r in results if str(r.get("rep")) == rep]
        js = [j for j in judged if str(j.get("rep")) == rep]
        pcs = [c for c in PLAIN_CELLS if c["rep"] == rep and c["case"] in covered_cases]
        rep_full.append({"rep": rep, "rows": _arm_block(rs, js, pcs)})

    # ---- per-case bifurcation (live; ppr_off vs ppr_on vs static plain) ----
    # one row per case that appears in the ppr run, sorted by persona then
    # numeric case. acc from judged.jsonl (sum passed / sum total); tokens from
    # results.jsonl ok cells; timeouts from results.jsonl.
    BIF_ARMS = ("ppr_off", "ppr_on")

    def _case_arm_acc(case, arm):
        """(passed, total) summed over judged rows for case+arm, preferring the
        integer passed/total fields, falling back to parsing `score`."""
        sp = 0.0
        st = 0.0
        seen = False
        for r in judged:
            if r.get("case") != case or r.get("arm") != arm:
                continue
            p = r.get("passed")
            t = r.get("total")
            if isinstance(p, (int, float)) and isinstance(t, (int, float)):
                sp += float(p)
                st += float(t)
                seen = True
            else:
                pa, tb = parse_score(r.get("score"))
                if pa is not None and tb is not None and tb > 0:
                    sp += pa
                    st += tb
                    seen = True
        if not seen or st <= 0:
            return (None, None)
        return (sp, st)

    def _case_arm_tok_to(case, arm):
        """(mean tokens over ok cells, timeout count) for case+arm."""
        toks = []
        timeouts = 0
        for r in results:
            if r.get("case") != case or r.get("arm") != arm:
                continue
            if r.get("status") == "timeout":
                timeouts += 1
            if r.get("status") == "ok" and isinstance(r.get("tokens"), (int, float)):
                toks.append(float(r.get("tokens")))
        mean_tok = (sum(toks) / len(toks)) if toks else None
        return (mean_tok, timeouts)

    # collect cases present in the ppr run (results or judged) + their persona
    case_persona = {}
    for r in results:
        c = r.get("case")
        if c is None:
            continue
        if c not in case_persona and r.get("persona") is not None:
            case_persona[c] = r.get("persona")
        case_persona.setdefault(c, None)
    for r in judged:
        c = r.get("case")
        if c is None:
            continue
        if case_persona.get(c) is None and r.get("persona") is not None:
            case_persona[c] = r.get("persona")
        case_persona.setdefault(c, None)

    def _case_sort_key(c):
        persona = case_persona.get(c) or ""
        try:
            cnum = int(re.sub(r"\D", "", str(c)) or "0")
        except Exception:
            cnum = 0
        return (persona, cnum, str(c))

    def _acc_pct(passed, total):
        if passed is None or total is None or total <= 0:
            return None
        return 100.0 * passed / total

    per_case = []
    for c in sorted(case_persona.keys(), key=_case_sort_key):
        plain = PLAIN_CASES.get(str(c)) or PLAIN_CASES.get(c) or {}
        plain_acc = plain.get("plain_acc")  # fraction or None
        plain_pct = (plain_acc * 100.0) if plain_acc is not None else None
        plain_tok = plain.get("plain_tok")

        off_p, off_t = _case_arm_acc(c, "ppr_off")
        on_p, on_t = _case_arm_acc(c, "ppr_on")
        off_pct = _acc_pct(off_p, off_t)
        on_pct = _acc_pct(on_p, on_t)
        off_tok, off_to = _case_arm_tok_to(c, "ppr_off")
        on_tok, on_to = _case_arm_tok_to(c, "ppr_on")

        d_on_plain = None
        if on_pct is not None and plain_pct is not None:
            d_on_plain = on_pct - plain_pct
        d_on_off = None
        if on_pct is not None and off_pct is not None:
            d_on_off = on_pct - off_pct

        per_case.append({
            "case": c,
            "persona": case_persona.get(c),
            "plain_acc_pct": plain_pct,
            "plain_tok": plain_tok,
            "off_acc_pct": off_pct,
            "off_tok": off_tok,
            "off_timeouts": off_to,
            "on_acc_pct": on_pct,
            "on_tok": on_tok,
            "on_timeouts": on_to,
            "d_on_plain": d_on_plain,
            "d_on_off": d_on_off,
        })

    # ---- failures (most recent first, cap 20) ----
    failures = []
    for r in reversed(results):
        if r.get("status") != "ok":
            failures.append({
                "label": r.get("label"),
                "status": r.get("status"),
                "persona": r.get("persona"),
                "arm": r.get("arm"),
            })
        if len(failures) >= 20:
            break

    # ---- ETA ----
    eta_seconds = None
    if done > 0 and elapsed and elapsed > 0 and total_cells:
        rate = done / elapsed  # cells per second
        if rate > 0:
            eta_seconds = (total_cells - done) / rate
            if eta_seconds < 0:
                eta_seconds = 0

    return {
        "ready": True,
        "now": now,
        "out_dir": OUT_DIR,
        "run_id": manifest.get("run_id"),
        "agent": manifest.get("agent"),
        "arms": arms,
        "personas": personas,
        "reps": reps,
        "started_at": started_at,
        "elapsed": elapsed,
        "total_cells": total_cells,
        "done": done,
        "ok": ok,
        "failed": failed,
        "pct": pct,
        "eta_seconds": eta_seconds,
        "arm_summary": arm_summary,
        "headline": headline,
        "three_way": three_way,
        "grid": grid,
        "persona_3way": persona_3way,
        "persona_full": persona_full,
        "rep_full": rep_full,
        "failures": failures,
        "judged_n": len(judged),
        "per_case": per_case,
        "plain_loaded": PLAIN_LOADED,
        "plain_cases": len(PLAIN_CASES),
        "plain_out_dir": PLAIN_OUT_DIR,
    }


# ---------------------------------------------------------------------------
# HTML page (inline; vanilla JS; fetch /status every 3s)
# ---------------------------------------------------------------------------

PAGE = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>E2B benchmark dashboard</title>
<style>
  :root {
    --bg: #0e1116;
    --panel: #161b22;
    --panel2: #1c2430;
    --border: #2a313c;
    --fg: #e6edf3;
    --muted: #8b949e;
    --accent: #58a6ff;
    --good: #3fb950;
    --bad: #f85149;
    --warn: #d29922;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; background: var(--bg); color: var(--fg);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
    font-size: 14px; line-height: 1.5;
  }
  .num { font-family: "SF Mono", Menlo, Consolas, monospace; }
  .wrap { max-width: 1100px; margin: 0 auto; padding: 20px; }
  h1 { font-size: 20px; margin: 0 0 4px; }
  h2 { font-size: 14px; text-transform: uppercase; letter-spacing: .06em;
       color: var(--muted); margin: 24px 0 8px; }
  .sub { color: var(--muted); font-size: 13px; }
  .panel { background: var(--panel); border: 1px solid var(--border);
           border-radius: 8px; padding: 14px 16px; margin-top: 10px; }
  .row { display: flex; flex-wrap: wrap; gap: 14px; }
  .stat { background: var(--panel2); border: 1px solid var(--border);
          border-radius: 6px; padding: 10px 14px; min-width: 110px; }
  .stat .k { color: var(--muted); font-size: 11px; text-transform: uppercase;
             letter-spacing: .05em; }
  .stat .v { font-size: 22px; font-weight: 600; font-family: "SF Mono", Menlo, monospace; }
  .progress { height: 10px; background: var(--panel2); border-radius: 5px;
              overflow: hidden; margin-top: 10px; border: 1px solid var(--border); }
  .progress > div { height: 100%; background: linear-gradient(90deg,#1f6feb,#58a6ff); }
  table { border-collapse: collapse; width: 100%; }
  th, td { text-align: right; padding: 7px 10px; border-bottom: 1px solid var(--border); }
  th:first-child, td:first-child { text-align: left; }
  th { color: var(--muted); font-size: 11px; text-transform: uppercase;
       letter-spacing: .05em; font-weight: 600; }
  td.num, th.num { font-family: "SF Mono", Menlo, monospace; }
  .headline { background: linear-gradient(135deg,#15233a,#101620);
              border: 1px solid #2d4a78; border-radius: 10px; padding: 18px 20px;
              margin-top: 10px; }
  .headline .big { display: flex; gap: 40px; flex-wrap: wrap; }
  .headline .metric .label { color: var(--muted); font-size: 12px;
              text-transform: uppercase; letter-spacing: .05em; }
  .headline .metric .delta { font-size: 34px; font-weight: 700;
              font-family: "SF Mono", Menlo, monospace; }
  .headline .metric .ctx { color: var(--muted); font-size: 12px; }
  .good { color: var(--good); }
  .bad { color: var(--bad); }
  .warn { color: var(--warn); }
  .threeway { background: linear-gradient(135deg,#15233a,#101620);
              border: 1px solid #2d4a78; border-radius: 10px; padding: 18px 20px;
              margin-top: 10px; }
  .threeway .cols { display: flex; gap: 28px; flex-wrap: wrap; align-items: flex-end; }
  .tw-col { min-width: 130px; }
  .tw-col .name { color: var(--muted); font-size: 12px; text-transform: uppercase;
                  letter-spacing: .05em; margin-bottom: 2px; }
  .tw-col .acc { font-size: 34px; font-weight: 700;
                 font-family: "SF Mono", Menlo, monospace; }
  .tw-col .tok { color: var(--muted); font-size: 12px; }
  .tw-deltas { display: flex; gap: 28px; flex-wrap: wrap; margin-top: 14px;
               padding-top: 12px; border-top: 1px solid #2d4a78; }
  .tw-delta .label { color: var(--muted); font-size: 11px; text-transform: uppercase;
                     letter-spacing: .05em; }
  .tw-delta .val { font-size: 20px; font-weight: 700;
                   font-family: "SF Mono", Menlo, monospace; }
  .grid-cell { padding: 8px 10px; border: 1px solid var(--border);
               border-radius: 6px; background: var(--panel2); min-width: 120px; }
  .grid-cell.complete { border-color: var(--good); background: #11251a; }
  .grid-cell .pct { font-family: "SF Mono", Menlo, monospace; font-size: 16px; }
  .grid-cell .dn { color: var(--muted); font-size: 12px; }
  .gridtable td { vertical-align: top; }
  .fail { font-family: "SF Mono", Menlo, monospace; font-size: 12.5px;
          padding: 4px 0; border-bottom: 1px solid var(--border); }
  .fail .st { color: var(--bad); }
  .footer { color: var(--muted); font-size: 12px; margin-top: 22px; }
  .waiting { text-align: center; padding: 60px 20px; color: var(--muted); font-size: 16px; }
  .pill { display: inline-block; padding: 2px 8px; border-radius: 10px;
          background: var(--panel2); border: 1px solid var(--border);
          font-size: 12px; color: var(--muted); }
</style>
</head>
<body>
<div class="wrap" id="root">
  <div class="waiting">loading…</div>
</div>

<script>
function fmtDur(s) {
  if (s === null || s === undefined) return "—";
  s = Math.floor(s);
  var h = Math.floor(s / 3600);
  var m = Math.floor((s % 3600) / 60);
  var sec = s % 60;
  if (h > 0) return h + ":" + String(m).padStart(2,"0") + ":" + String(sec).padStart(2,"0");
  return m + ":" + String(sec).padStart(2,"0");
}
function fmtEta(s) {
  if (s === null || s === undefined) return "—";
  s = Math.floor(s);
  var h = Math.floor(s / 3600);
  var m = Math.floor((s % 3600) / 60);
  return h + ":" + String(m).padStart(2,"0");
}
function pct(frac) {
  if (frac === null || frac === undefined) return "—";
  return (frac * 100).toFixed(1) + "%";
}
function num(n, d) {
  if (n === null || n === undefined) return "—";
  return Number(n).toLocaleString(undefined, {maximumFractionDigits: d === undefined ? 0 : d});
}
function esc(x) {
  if (x === null || x === undefined) return "";
  return String(x).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;");
}
function signed(n, d, suffix) {
  if (n === null || n === undefined) return "—";
  var s = (n >= 0 ? "+" : "") + Number(n).toFixed(d === undefined ? 1 : d);
  return s + (suffix || "");
}
function deltaClass(n) {
  if (n === null || n === undefined) return "";
  if (n > 0) return "good";
  if (n < 0) return "bad";
  return "";
}
function ipct(p) {
  // p is already a percentage number (0..100) or null
  if (p === null || p === undefined) return "—";
  return Math.round(p) + "%";
}
function tokc(n) {
  // compact token count: 341000 -> "341k", 1200000 -> "1.2m"
  if (n === null || n === undefined) return "—";
  n = Number(n);
  if (n >= 1e6) return (n / 1e6).toFixed(1).replace(/\.0$/, "") + "m";
  if (n >= 1e3) return Math.round(n / 1e3) + "k";
  return String(Math.round(n));
}
function cellAcc(p, tok, to) {
  // "acc% (tok, ⏱n)"  — acc & tok dashed when missing; ⏱n only when to>0
  var s = ipct(p) + " (" + tokc(tok);
  if (to && to > 0) s += ", ⏱" + to;
  s += ")";
  return s;
}
function dpp(n) {
  // signed percentage-point delta, integer
  if (n === null || n === undefined) return "—";
  return (n > 0 ? "+" : "") + Math.round(n);
}

function render(d) {
  var root = document.getElementById("root");
  if (!d.ready) {
    root.innerHTML = '<div class="waiting">' + esc(d.message || "waiting…") +
      '<div class="sub" style="margin-top:8px">out = ' + esc(d.out_dir) + '</div></div>';
    return;
  }

  var html = "";

  // header
  html += '<h1>' + esc(d.run_id || "(run)") + '</h1>';
  html += '<div class="sub">agent <span class="pill">' + esc(d.agent) +
          '</span> &nbsp; elapsed <span class="num">' + fmtDur(d.elapsed) + '</span>' +
          (d.eta_seconds !== null && d.eta_seconds !== undefined ?
            ' &nbsp; eta <span class="num">' + fmtEta(d.eta_seconds) + '</span>' : '') +
          '</div>';

  // top stats
  html += '<div class="row" style="margin-top:12px">';
  html += '<div class="stat"><div class="k">done / total</div><div class="v">' +
          d.done + ' / ' + d.total_cells + '</div></div>';
  html += '<div class="stat"><div class="k">complete</div><div class="v">' +
          d.pct.toFixed(1) + '%</div></div>';
  html += '<div class="stat"><div class="k">ok</div><div class="v good">' + d.ok + '</div></div>';
  html += '<div class="stat"><div class="k">failed</div><div class="v ' +
          (d.failed > 0 ? "bad" : "") + '">' + d.failed + '</div></div>';
  html += '<div class="stat"><div class="k">judged</div><div class="v">' + d.judged_n + '</div></div>';
  html += '</div>';

  html += '<div class="progress"><div style="width:' + d.pct.toFixed(1) + '%"></div></div>';

  // 3-way headline (plain vs ppr_off vs ppr_on) on the matched case set
  if (d.three_way) {
    var t = d.three_way;
    html += '<h2>3-way headline — plain vs ppr_off vs ppr_on ' +
            '&nbsp;<span class="pill">matched ' + t.matched_cases + ' cases</span></h2>';
    html += '<div class="threeway"><div class="cols">';
    function twCol(name, o) {
      return '<div class="tw-col"><div class="name">' + name + '</div>' +
             '<div class="acc">' + ipct(o.acc) + '</div>' +
             '<div class="tok">' + tokc(o.tok) + ' tok &middot; n=' + o.n + '</div></div>';
    }
    html += twCol('plain', t.plain);
    html += twCol('ppr_off', t.ppr_off);
    html += twCol('ppr_on', t.ppr_on);
    html += '</div>';  // cols
    html += '<div class="tw-deltas">';
    function twDelta(label, v) {
      return '<div class="tw-delta"><div class="label">' + label + '</div>' +
             '<div class="val ' + deltaClass(v) + '">' + signed(v, 1, " pp") + '</div></div>';
    }
    html += twDelta('&Delta; ppr_off &minus; plain', t.d_off_plain);
    html += twDelta('&Delta; ppr_on &minus; plain', t.d_on_plain);
    html += twDelta('&Delta; ppr_on &minus; ppr_off', t.d_on_off);
    html += '</div>';  // deltas
    html += '</div>';  // threeway
  }

  // A/B headline
  if (d.headline) {
    var h = d.headline;
    html += '<h2>A/B headline — ppr_on minus ppr_off</h2>';
    html += '<div class="headline"><div class="big">';
    html += '<div class="metric"><div class="label">accuracy delta</div>' +
            '<div class="delta ' + deltaClass(h.acc_delta_pp) + '">' +
            signed(h.acc_delta_pp, 1, " pp") + '</div>' +
            '<div class="ctx">on ' + pct(h.acc_on) + ' &nbsp;vs&nbsp; off ' + pct(h.acc_off) + '</div></div>';
    // for tokens, lower is better -> invert color sense
    var tokCls = (h.tok_delta === null || h.tok_delta === undefined) ? "" :
                 (h.tok_delta < 0 ? "good" : (h.tok_delta > 0 ? "bad" : ""));
    html += '<div class="metric"><div class="label">mean tokens delta</div>' +
            '<div class="delta ' + tokCls + '">' + signed(h.tok_delta, 0) + '</div>' +
            '<div class="ctx">on ' + num(h.tok_on) + ' &nbsp;vs&nbsp; off ' + num(h.tok_off) + '</div></div>';
    html += '</div></div>';
  }

  // per-arm summary
  html += '<h2>Per-arm summary</h2><div class="panel"><table><thead><tr>' +
          '<th>arm</th><th class="num">done</th><th class="num">ok</th>' +
          '<th class="num">failed</th><th class="num">accuracy</th>' +
          '<th class="num">mean in</th><th class="num">mean out</th>' +
          '<th class="num">mean tokens</th><th class="num">mean calls</th>' +
          '<th class="num">judged</th></tr></thead><tbody>';
  d.arm_summary.forEach(function(a){
    html += '<tr><td>' + esc(a.arm) + '</td>' +
            '<td class="num">' + a.done + '</td>' +
            '<td class="num good">' + a.ok + '</td>' +
            '<td class="num ' + (a.failed>0?"bad":"") + '">' + a.failed + '</td>' +
            '<td class="num">' + pct(a.accuracy) + '</td>' +
            '<td class="num">' + num(a.mean_in) + '</td>' +
            '<td class="num">' + num(a.mean_out) + '</td>' +
            '<td class="num">' + num(a.mean_tokens) + '</td>' +
            '<td class="num">' + num(a.mean_calls, 1) + '</td>' +
            '<td class="num">' + a.judged_n + '</td></tr>';
  });
  // (plain now renders as a first-class row via arm_summary above — no separate hack)
  html += '</tbody></table></div>';

  // reusable: full per-arm columns broken down by a label (persona / rep)
  function armBreakdownTable(title, blocks, labelHdr) {
    if (!blocks || !blocks.length) return '';
    var h = '<h2>' + title + '</h2><div class="panel"><table><thead><tr>' +
      '<th>' + labelHdr + '</th><th>arm</th><th class="num">done</th><th class="num">ok</th>' +
      '<th class="num">failed</th><th class="num">accuracy</th><th class="num">mean in</th>' +
      '<th class="num">mean out</th><th class="num">mean tokens</th><th class="num">mean calls</th>' +
      '<th class="num">judged</th></tr></thead><tbody>';
    blocks.forEach(function(b){
      var lbl = (b.persona !== undefined) ? b.persona : ('rep ' + b.rep);
      (b.rows || []).forEach(function(a, i){
        h += '<tr' + (i === 0 ? ' style="border-top:2px solid var(--border)"' : '') + '>' +
          '<td>' + (i === 0 ? esc(lbl) : '') + '</td>' +
          '<td>' + esc(a.arm) + '</td>' +
          '<td class="num">' + a.done + '</td>' +
          '<td class="num good">' + a.ok + '</td>' +
          '<td class="num ' + (a.failed > 0 ? "bad" : "") + '">' + a.failed + '</td>' +
          '<td class="num">' + pct(a.accuracy) + '</td>' +
          '<td class="num">' + num(a.mean_in) + '</td>' +
          '<td class="num">' + num(a.mean_out) + '</td>' +
          '<td class="num">' + num(a.mean_tokens) + '</td>' +
          '<td class="num">' + num(a.mean_calls, 1) + '</td>' +
          '<td class="num">' + a.judged_n + '</td></tr>';
      });
    });
    h += '</tbody></table></div>';
    return h;
  }
  html += armBreakdownTable('Per-persona breakdown (plain / ppr_off / ppr_on)', d.persona_full, 'persona');
  html += armBreakdownTable('Per-rep breakdown (plain / ppr_off / ppr_on)', d.rep_full, 'rep');

  // persona x arm grid
  html += '<h2>Persona &times; arm</h2><div class="panel"><table class="gridtable"><thead><tr><th>persona</th>';
  d.arms.forEach(function(arm){ html += '<th>' + esc(arm) + '</th>'; });
  html += '</tr></thead><tbody>';
  d.grid.forEach(function(row){
    html += '<tr><td>' + esc(row.persona) +
            '<div class="dn" style="color:var(--muted)">exp ' + row.expected + '</div></td>';
    row.cells.forEach(function(c){
      html += '<td><div class="grid-cell' + (c.complete ? " complete" : "") + '">' +
              '<div class="pct">' + pct(c.accuracy) + '</div>' +
              '<div class="dn">' + c.done + ' / ' + c.expected + '</div></div></td>';
    });
    html += '</tr>';
  });
  html += '</tbody></table></div>';

  // per-persona 3-way (plain vs ppr_off vs ppr_on) — accuracy (mean tokens)
  if (d.persona_3way && d.persona_3way.length) {
    html += '<h2>Per-persona: plain vs ppr_off vs ppr_on</h2><div class="panel"><table><thead><tr>' +
            '<th>persona</th><th class="num">plain</th><th class="num">ppr_off</th><th class="num">ppr_on</th>' +
            '<th class="num">&Delta; best ppr &minus; plain</th></tr></thead><tbody>';
    function tw_cell(x){ return (x && x.acc != null) ? ipct(x.acc) + ' <span class="sub">(' + num(x.tok) + ')</span>' : '—'; }
    d.persona_3way.forEach(function(r){
      var pl = r.plain && r.plain.acc, of = r.ppr_off && r.ppr_off.acc, on = r.ppr_on && r.ppr_on.acc;
      var best = (of != null || on != null) ? Math.max(of != null ? of : -1, on != null ? on : -1) : null;
      var dlt = (best != null && pl != null) ? (best - pl) : null;
      var dcol = dlt == null ? '' : (dlt > 0 ? 'good' : (dlt < 0 ? 'bad' : ''));
      html += '<tr><td>' + esc(r.persona) + '</td>' +
              '<td class="num">' + tw_cell(r.plain) + '</td>' +
              '<td class="num">' + tw_cell(r.ppr_off) + '</td>' +
              '<td class="num">' + tw_cell(r.ppr_on) + '</td>' +
              '<td class="num ' + dcol + '">' + (dlt == null ? '—' : (dlt > 0 ? '+' : '') + dlt.toFixed(1) + 'pp') + '</td></tr>';
    });
    html += '</tbody></table></div>';
  }

  // per-case bifurcation
  var pc = d.per_case || [];
  html += '<h2>Per-case bifurcation' +
          (d.plain_loaded ? ' &nbsp;<span class="pill">plain: ' + d.plain_cases + ' cases</span>'
                          : ' &nbsp;<span class="pill">plain: not loaded</span>') +
          '</h2><div class="panel">';
  if (!pc.length) {
    html += '<div class="sub">no ppr cases yet</div>';
  } else {
    html += '<table class="num"><thead><tr>' +
            '<th>persona</th><th>case</th>' +
            '<th class="num">plain acc% (tok)</th>' +
            '<th class="num">ppr_off acc% (tok, &#9201;n)</th>' +
            '<th class="num">ppr_on acc% (tok, &#9201;n)</th>' +
            '<th class="num">&Delta;(on&minus;plain)</th>' +
            '<th class="num">&Delta;(on&minus;off)</th>' +
            '</tr></thead><tbody>';
    pc.forEach(function(r){
      var plainCell = (r.plain_acc_pct === null || r.plain_acc_pct === undefined)
        ? '—' : (ipct(r.plain_acc_pct) + ' (' + tokc(r.plain_tok) + ')');
      html += '<tr>' +
              '<td>' + esc(r.persona || '') + '</td>' +
              '<td class="num">' + esc(r.case) + '</td>' +
              '<td class="num">' + plainCell + '</td>' +
              '<td class="num">' + cellAcc(r.off_acc_pct, r.off_tok, r.off_timeouts) + '</td>' +
              '<td class="num">' + cellAcc(r.on_acc_pct, r.on_tok, r.on_timeouts) + '</td>' +
              '<td class="num ' + deltaClass(r.d_on_plain) + '">' + dpp(r.d_on_plain) + '</td>' +
              '<td class="num ' + deltaClass(r.d_on_off) + '">' + dpp(r.d_on_off) + '</td>' +
              '</tr>';
    });
    html += '</tbody></table>';
  }
  html += '</div>';

  // failures
  html += '<h2>Failures (recent)</h2><div class="panel">';
  if (!d.failures.length) {
    html += '<div class="sub">none</div>';
  } else {
    d.failures.forEach(function(f){
      html += '<div class="fail"><span class="st">' + esc(f.status) + '</span> &nbsp; ' +
              esc(f.label) + ' &nbsp;<span class="sub">[' + esc(f.persona) + '/' + esc(f.arm) + ']</span></div>';
    });
  }
  html += '</div>';

  html += '<div class="footer">out = ' + esc(d.out_dir) +
          ' &nbsp;·&nbsp; last updated ' + new Date(d.now * 1000).toLocaleTimeString() +
          ' &nbsp;·&nbsp; auto-refresh 3s</div>';

  root.innerHTML = html;
}

function tick() {
  fetch("/status", {cache: "no-store"})
    .then(function(r){ return r.json(); })
    .then(render)
    .catch(function(e){
      var root = document.getElementById("root");
      root.innerHTML = '<div class="waiting">status fetch failed: ' + esc(e) + '</div>';
    });
}
tick();
setInterval(tick, 3000);
</script>
</body>
</html>
"""


# ---------------------------------------------------------------------------
# HTTP handler
# ---------------------------------------------------------------------------

class Handler(BaseHTTPRequestHandler):
    def _send(self, code, body, ctype):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path == "/" or path == "/index.html":
            self._send(200, PAGE, "text/html; charset=utf-8")
            return
        if path == "/status":
            try:
                payload = compute_status()
            except Exception as exc:
                payload = {"ready": False, "now": time.time(),
                           "message": "status error: %s" % exc, "out_dir": OUT_DIR}
            self._send(200, json.dumps(payload), "application/json; charset=utf-8")
            return
        self._send(404, "not found", "text/plain; charset=utf-8")

    def log_message(self, *args):
        # quiet — don't spam stdout with every poll
        pass


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    global OUT_DIR, PLAIN_OUT_DIR, PLAIN_CASES, PLAIN_CELLS, PLAIN_LOADED
    ap = argparse.ArgumentParser(description="Live E2B benchmark dashboard (stdlib only).")
    ap.add_argument("--out", required=True, help="directory containing manifest.json / results.jsonl / judged.jsonl")
    ap.add_argument("--port", type=int, default=8765, help="port to bind (default 8765)")
    ap.add_argument(
        "--plain-out",
        default="/Users/marmikpandya/semantic-filesystem/tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs",
        help="dir of SEM-39 plain run (pm_codex_<case>_plain_r<rep>/); skipped if missing",
    )
    args = ap.parse_args()

    OUT_DIR = os.path.abspath(args.out)

    # static plain per-case data — load ONCE here, cache in memory
    PLAIN_OUT_DIR = os.path.abspath(args.plain_out) if args.plain_out else None
    PLAIN_CASES, PLAIN_LOADED = load_plain_cases(PLAIN_OUT_DIR)
    PLAIN_CELLS = load_plain_cells(PLAIN_OUT_DIR)
    if PLAIN_LOADED:
        print("plain: loaded %d cases from %s" % (len(PLAIN_CASES), PLAIN_OUT_DIR))
    else:
        print("plain: not loaded (dir missing or empty): %s" % PLAIN_OUT_DIR)

    server = HTTPServer(("127.0.0.1", args.port), Handler)
    print("dashboard: http://127.0.0.1:%d  (out=%s)" % (args.port, OUT_DIR))
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nshutting down")
        server.shutdown()


if __name__ == "__main__":
    main()
