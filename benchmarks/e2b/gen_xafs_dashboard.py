#!/usr/bin/env python3
"""Generate the xAFS 13-persona x 4-arm live dashboard (HTML).

Reads per-arm result JSONs in benchmarks/e2b/ and renders a matrix:
  rows = personas dp_001..dp_013 (with file counts)
  cols = plain · ppr_on · ppr_map · ppr_off
  cell = ✓/✗ + tokens (color-coded), or "·" if not run yet.
Plus per-arm accuracy + tokens-per-correct, and seed/template build status.

Re-run anytime to refresh:  python3 benchmarks/e2b/gen_xafs_dashboard.py
"""
import json, os, pathlib, glob

REPO = pathlib.Path(__file__).resolve().parents[2]
E2B = REPO / "benchmarks/e2b"
OUT = E2B / "xafs_dashboard.html"

PERSONAS = [f"dp_{i:03d}" for i in range(1, 14)]
NFILES = {"dp_001":5,"dp_002":10,"dp_003":20,"dp_004":30,"dp_005":50,"dp_006":100,"dp_007":200,
          "dp_008":299,"dp_009":480,"dp_010":991,"dp_011":1998,"dp_012":4998,"dp_013":9988}
# arm -> candidate result files (first that exists wins)
ARM_FILES = {
    "plain":   ["_xafs_perdp_plain.json", "_xafs_slice_plain.json"],
    "ppr_on":  ["_xafs_perdp_ppr_on.json"],
    "ppr_map": ["_xafs_perdp_ppr_map.json"],
    "ppr_off": ["_xafs_perdp_ppr_off.json"],
}
ARMS = list(ARM_FILES)

def is_real(r):
    """A real, counted cell: not an error and the agent actually ran (tokens>0)."""
    st = str(r.get("status") or "").lower()
    return st != "error" and (r.get("tokens") or 0) > 0

def load(arm):
    for f in ARM_FILES[arm]:
        p = E2B / f
        if p.exists():
            try:
                d = json.loads(p.read_text())
                # keep only real cells (drop 0-token error records → shown as pending, re-runnable)
                return {r["dp"]: r for r in d.get("results", []) if is_real(r)}, f
            except Exception:
                pass
    return {}, None

def seeds_status():
    # which per-dp KG seeds + plain corpus look ready (best-effort, from filenames only here)
    return {}

data = {a: load(a) for a in ARMS}
fmt = lambda n: f"{n/1000:.0f}K" if n and n >= 1000 else (str(n) if n else "·")

# build matrix rows
rows = ""
for dp in PERSONAS:
    cells = ""
    for a in ARMS:
        r = data[a][0].get(dp)
        if not r:
            cells += '<td class="c pend">·</td>'
        else:
            ok = r.get("correct"); tok = r.get("tokens") or 0
            cls = "ok" if ok else "no"
            mark = "✓" if ok else "✗"
            cells += f'<td class="c {cls}"><div class="m">{mark}</div><div class="tk">{fmt(tok)}</div></td>'
    rows += f'<tr><td class="p">{dp}<span class="nf">{NFILES[dp]:,}f</span></td>{cells}</tr>'

# per-arm summaries
sumcells = ""
for a in ARMS:
    res = data[a][0]
    done = [r for r in res.values() if r.get("status") not in (None,"ERROR")]
    n = len(done); cor = sum(1 for r in done if r.get("correct"))
    tot = sum(r.get("tokens") or 0 for r in done)
    tpc = f"{tot/cor/1000:.0f}K" if cor else "—"
    acc = f"{cor}/{n}" if n else "0/0"
    pct = f"{100*cor/n:.0f}%" if n else "—"
    src = data[a][1] or "—"
    sumcells += (f'<td class="s"><div class="big">{acc}</div>'
                 f'<div class="sl">correct / tested &middot; {pct}</div>'
                 f'<div class="sl2">{n} of 110 q tested &middot; {tot/1e6:.2f}M tok &middot; {tpc}/correct</div>'
                 f'<div class="src">{src}</div></td>')

n_done = sum(1 for a in ARMS for r in data[a][0].values() if r.get("status") not in (None,"ERROR"))

html = f"""<!DOCTYPE html><html><head><meta charset="utf-8">
<meta http-equiv="refresh" content="30">
<title>xAFS dashboard</title><style>
:root{{--bg:#0d1117;--pan:#161b22;--ln:#30363d;--ink:#e6edf3;--mut:#8b949e;
--ok:#3fb950;--no:#f85149;--semfs:#a371f7;--plain:#3fb950}}
*{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--ink);
font-family:-apple-system,Segoe UI,Roboto,sans-serif;padding:24px}}
.wrap{{max-width:1000px;margin:0 auto}}
h1{{margin:0 0 2px;font-size:24px}}.sub{{color:var(--mut);font-size:13px;margin-bottom:8px}}
.warn{{background:rgba(240,136,62,.12);border-left:3px solid #f0883e;color:#e6edf3;
font-size:12px;padding:8px 12px;border-radius:0 8px 8px 0;margin-bottom:16px;line-height:1.5}}
table{{width:100%;border-collapse:separate;border-spacing:0;font-size:13px}}
th,td{{border-bottom:1px solid var(--ln);padding:8px 6px;text-align:center}}
thead th{{color:var(--mut);font-weight:600;font-size:12px;text-transform:uppercase;letter-spacing:.05em}}
td.p{{text-align:left;font-family:ui-monospace,monospace;font-weight:600;white-space:nowrap}}
.nf{{color:var(--mut);font-weight:400;font-size:11px;margin-left:6px}}
td.c{{font-family:ui-monospace,monospace}}.c .m{{font-size:15px;font-weight:800}}.c .tk{{font-size:11px;color:var(--mut)}}
.ok{{background:rgba(63,185,80,.10)}}.ok .m{{color:var(--ok)}}
.no{{background:rgba(248,81,73,.10)}}.no .m{{color:var(--no)}}
.pend{{color:#3a3f47}}
td.s{{border-top:2px solid var(--ln);padding-top:12px;vertical-align:top}}
.s .big{{font-size:22px;font-weight:800}}.s .sl{{font-size:11px;color:var(--mut)}}
.s .sl2{{font-size:11px;color:var(--mut);margin-top:4px}}.s .src{{font-size:10px;color:#3a3f47;margin-top:3px;font-family:monospace}}
.legend{{color:var(--mut);font-size:12px;margin-top:14px}}
.col-plain{{color:var(--plain)}}.col-ppr{{color:var(--semfs)}}
.bar{{height:6px;border-radius:3px;background:var(--ln);margin-top:18px;overflow:hidden}}
.bar>i{{display:block;height:100%;background:linear-gradient(90deg,var(--plain),var(--semfs));width:{n_done/52*100:.0f}%}}
</style></head><body><div class="wrap">
<h1>xAFS &mdash; 13 personas &times; 4 arms</h1>
<div class="sub">agent codex/gpt-5.4-mini &middot; judge Gemini 3.1 Pro &middot; auto-refresh 30s &middot; <b>{n_done}/52 cells</b></div>
<div class="warn">⚠️ SLICE: only <b>q01</b> tested per persona &mdash; <b>13 of 110</b> xAFS questions (~12%).
Each ✓/✗ is that persona's <b>single tested question</b>, not its full accuracy. q02&ndash;q09 not yet run.</div>
<div class="bar"><i></i></div>
<table><thead><tr><th style="text-align:left">persona</th>
<th class="col-plain">plain</th><th class="col-ppr">ppr_on</th><th class="col-ppr">ppr_map</th><th class="col-ppr">ppr_off</th></tr></thead>
<tbody>{rows}
<tr><td class="p" style="border-top:2px solid var(--ln)">TOTALS</td>{sumcells}</tr>
</tbody></table>
<div class="legend">✓ correct &middot; ✗ wrong &middot; · not run yet &middot; cell = answer + agent tokens.
plain = raw grep/find &middot; ppr_* = semfs hidden-KG graph prior (on / +map / off-control).</div>
</div></body></html>"""

OUT.write_text(html)
print(f"dashboard → {OUT}  ({n_done}/52 cells)")
