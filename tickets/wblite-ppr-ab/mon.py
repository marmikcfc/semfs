"""Detailed PPR A/B monitoring snapshot — one view of everything.
Run: python3 tickets/wblite-ppr-ab/mon.py        (add `loop` to refresh every 60s)
"""
import json, time, sys, re, subprocess, collections, pathlib

REPO = pathlib.Path(__file__).resolve().parents[2]
O = REPO / "tickets/wblite-ppr-ab/artifacts/e2b_runs"
PLAINDIR = REPO / "tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs"
RUNLOG = pathlib.Path("/tmp/bake_logs/ppr_run.log")
GPULOG = pathlib.Path("/tmp/bake_logs/gpu_usage.log")
CAP = 20            # E2B concurrent-sandbox cap (observed)
GPU_RATE = 25.0     # $/hr, 4xB200


def jl(p):
    if not p.exists():
        return []
    out = []
    for ln in p.read_text().splitlines():
        ln = ln.strip()
        if ln:
            try: out.append(json.loads(ln))
            except Exception: pass
    return out


def tail(p, n):
    if not p.exists():
        return []
    return p.read_text(errors="ignore").splitlines()[-n:]


def acc(rows):
    p = t = 0
    for r in rows:
        a, b = r.get("passed"), r.get("total")
        if isinstance(a, (int, float)) and isinstance(b, (int, float)) and b:
            p += a; t += b
    return (100.0 * p / t, p, t) if t else (None, 0, 0)


def snap():
    man = {}
    if (O / "manifest.json").exists():
        man = json.loads((O / "manifest.json").read_text())
    total = man.get("total_cells", 0)
    res, jud = jl(O / "results.jsonl"), jl(O / "judged.jsonl")
    done = len(res)
    now = time.time()
    print("=" * 64)
    print(f"PPR A/B MONITOR  {time.strftime('%H:%M:%S')}   done {done}/{total} "
          f"({(done/total*100 if total else 0):.0f}%)")
    print("=" * 64)

    # ---- progress by persona × arm ----
    st = collections.Counter(r.get("status") for r in res)
    print(f"status: {dict(st)}")
    by = collections.Counter((r.get("persona"), r.get("arm")) for r in res)
    line = "  ".join(f"{p}/{a}:{n}" for (p, a), n in sorted(by.items(), key=lambda x: str(x[0])))
    print(f"cells: {line}")

    # ---- throughput / ETA ----
    started = man.get("started_at")
    # use file mtime of results as a recent-rate proxy
    if started and done:
        el = now - started
        rate = done / (el / 3600) if el else 0
        eta = (total - done) / rate if rate else 0
        print(f"throughput: {rate:.1f} cells/hr  |  elapsed {el/3600:.1f}h  |  ETA ~{eta:.1f}h  "
              f"|  GPU cost so far ~${el/3600*GPU_RATE:.0f}")

    # ---- tokens / calls (ok cells) ----
    oks = [r for r in res if r.get("status") == "ok"]
    tk = [r["tokens"] for r in oks if isinstance(r.get("tokens"), (int, float)) and r["tokens"]]
    cl = [r["calls"] for r in oks if isinstance(r.get("calls"), (int, float))]
    if tk:
        print(f"tokens: mean {int(sum(tk)/len(tk)):,}  median {sorted(tk)[len(tk)//2]:,}  "
              f"calls mean {sum(cl)/len(cl):.0f}" if cl else "")

    # ---- live A/B accuracy (3-way on matched cases) ----
    by_arm = collections.defaultdict(list)
    for j in jud:
        by_arm[j.get("arm")].append(j)
    nod = done - len(set(j["label"] for j in jud))
    print(f"\njudged: {len(jud)}  (no-deliverable/unjudged: {nod})")
    for arm in ("ppr_off", "ppr_on"):
        a, p, t = acc(by_arm.get(arm, []))
        print(f"  {arm:8} acc {a:.0f}% ({p}/{t})" if a is not None else f"  {arm:8} acc n/a")

    # ---- GPU (latest poller sample) ----
    g = [l for l in tail(GPULOG, 8) if "gen_tok" in l]
    print(f"\nGPU: {g[-1].split('GPU ')[-1] if g else 'no sample'}")

    # ---- E2B live sandboxes vs cap ----
    try:
        from e2b import Sandbox
        r = Sandbox.list(); ids = []
        b = r.next_items()
        while b:
            ids += [1 for _ in b]
            b = r.next_items() if getattr(r, "has_next", False) else None
        n = len(ids)
        flag = "  ⚠ NEAR CAP" if n >= CAP - 2 else ""
        print(f"E2B sandboxes: {n}/{CAP}{flag}")
    except Exception as e:
        print(f"E2B sandboxes: list err {repr(e)[:50]}")

    # ---- errors (recent run-log window) ----
    rl = tail(RUNLOG, 120)
    e429 = sum("E2B 429" in l for l in rl)
    econn = sum(("ConnectError" in l or "nodename" in l) for l in rl)
    inflight = sum(l.strip().startswith("▶ pm_codex") for l in rl)
    cur = [l for l in rl if l.startswith("=== [")]
    print(f"errors(last120): 429={e429} connect={econn}  |  in-flight ▶≈{inflight}  "
          f"|  current: {cur[-1].strip() if cur else '?'}")
    print()


if __name__ == "__main__":
    if "loop" in sys.argv:
        while True:
            snap(); time.sleep(60)
    else:
        snap()
