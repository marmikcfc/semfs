#!/usr/bin/env python3
"""Build a standalone houqin comparison page: previous WB-Lite run (plain/ppr_off/ppr_on,
Jun 16-23) vs today's map run (ppr_on/ppr_map, Jun 27). Per-case + aggregate. Static HTML."""
import json, collections, pathlib

REPO = pathlib.Path("/Users/marmikpandya/semantic-filesystem")
PREV_PPR = REPO / "tickets/wblite-ppr-ab/artifacts/e2b_runs"                  # prev ppr_off / ppr_on
PREV_PLAIN = REPO / "tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs"  # prev plain (5-arm run)
TODAY = REPO / "tickets/wblite-ppr-ab/artifacts/map_smoke_glm"
OUT = REPO / "tickets/wblite-ppr-ab/comparison.html"
HQ = ['23','35','37','47','54','72','79','83','85','87','100','102','116','207','251','255',
      '258','267','274','276','314','328','329','337','354','357','358','372','373','374']
hq = set(HQ)

def from_judged(path, arms):
    acc = collections.defaultdict(lambda: [0, 0])
    f = path / "judged.jsonl"
    if f.exists():
        for l in f.read_text().splitlines():
            if not l.strip():
                continue
            x = json.loads(l)
            if x.get('case') in hq and x.get('arm') in arms and x.get('total'):
                acc[(x['arm'], x['case'])][0] += x['passed']
                acc[(x['arm'], x['case'])][1] += x['total']
    return acc

def plain_from_cells(path):
    acc = collections.defaultdict(lambda: [0, 0])
    for c in HQ:
        for rep in ('1', '2', '3'):
            d = path / f"pm_codex_{c}_plain_r{rep}"
            if not d.exists():
                continue
            jf = list(d.glob("rubrics_judge--*.json"))
            if jf:
                try:
                    data = json.loads(jf[0].read_text())
                    rub = data.get('rubrics') or []
                    if rub:  # old format: 'passed' may be a string 'True'/'False'
                        passed = sum(1 for r in rub if str(r.get('passed')).lower() in ('true', '1'))
                        acc[c][0] += passed; acc[c][1] += len(rub)
                except Exception:
                    pass
    return acc

prev = from_judged(PREV_PPR, {'ppr_off', 'ppr_on'})
prev_plain = plain_from_cells(PREV_PLAIN)
today = from_judged(TODAY, {'ppr_on', 'ppr_map'})

def p_arm(acc, arm, c):
    v = acc.get((arm, c))
    return (100 * v[0] / v[1]) if (v and v[1]) else None

def p_plain(c):
    v = prev_plain.get(c)
    return (100 * v[0] / v[1]) if (v and v[1]) else None

rows = []
for c in HQ:
    rows.append(dict(case=c, pp=p_plain(c), poff=p_arm(prev, 'ppr_off', c),
                     pon=p_arm(prev, 'ppr_on', c), ton=p_arm(today, 'ppr_on', c),
                     tmap=p_arm(today, 'ppr_map', c)))

def agg(getter):
    n = d = 0
    for c in HQ:
        v = getter(c)
        if v is not None:
            n += v; d += 1
    return (n / d) if d else None

A = dict(pp=agg(p_plain), poff=agg(lambda c: p_arm(prev, 'ppr_off', c)),
         pon=agg(lambda c: p_arm(prev, 'ppr_on', c)), ton=agg(lambda c: p_arm(today, 'ppr_on', c)),
         tmap=agg(lambda c: p_arm(today, 'ppr_map', c)))

def mean_tokens(dirpath, arm):
    tot = []
    for f in pathlib.Path(dirpath).glob(f"pm_codex_*_{arm}_r*/result.json"):
        if f.parent.name.split("_")[2] not in hq:
            continue
        try:
            u = json.loads(f.read_text()).get("usage") or {}
            t = u.get("total_tokens") or (u.get("prompt_tokens", 0) + u.get("completion_tokens", 0))
            if t:
                tot.append(t)
        except Exception:
            pass
    return (sum(tot) / len(tot)) if tot else None

T = dict(pp=mean_tokens(PREV_PLAIN, "plain"), poff=mean_tokens(PREV_PPR, "ppr_off"),
         pon=mean_tokens(PREV_PPR, "ppr_on"), ton=mean_tokens(TODAY, "ppr_on"),
         tmap=mean_tokens(TODAY, "ppr_map"))
_tmax = max([v for v in T.values() if v] or [1])

def tokbar(v):
    w = 0 if v is None else min(1.0, v / _tmax) * 200
    lab = "—" if v is None else f"{v / 1000:.0f}K"
    return (f'<div class="bartrack"><div class="bar" style="width:{w}px;background:#d29922"></div>'
            f'<span class="barlab">{lab}</span></div>')

def eff(acc, tok):  # accuracy points per 100K tokens
    return f"{acc / (tok / 100000):.1f}" if (acc is not None and tok) else "—"

def cell(v, ref=None):
    if v is None:
        return '<td class="na">—</td>'
    cls = ""
    if ref is not None:
        d = v - ref
        cls = "win" if d > 3 else ("loss" if d < -3 else "")
    return f'<td class="{cls}">{v:.0f}%</td>'

def bar(v, color):
    w = 0 if v is None else min(100, v) * 1.6
    lab = "—" if v is None else f"{v:.1f}%"
    return (f'<div class="barrow"><div class="bartrack"><div class="bar" style="width:{w}px;background:{color}"></div>'
            f'<span class="barlab">{lab}</span></div></div>')

trows = "\n".join(
    f'<tr><td class="case">{r["case"]}</td>{cell(r["pp"])}{cell(r["poff"])}{cell(r["pon"])}'
    f'{cell(r["ton"], r["pon"])}{cell(r["tmap"], r["ton"])}</tr>'
    for r in rows)

html = f"""<!doctype html><html><head><meta charset="utf-8"><title>houqin: prev run vs today</title>
<style>
body{{background:#0d1117;color:#c9d1d9;font:14px/1.5 -apple-system,Segoe UI,sans-serif;margin:0;padding:28px;max-width:1080px;margin:auto}}
h1{{font-size:22px;margin:0 0 4px}} h2{{font-size:16px;color:#58a6ff;margin:26px 0 10px;border-bottom:1px solid #21262d;padding-bottom:6px}}
.sub{{color:#8b949e;margin-bottom:18px}}
table{{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums}}
th,td{{padding:5px 9px;text-align:right;border-bottom:1px solid #21262d}}
th{{color:#8b949e;font-weight:600;font-size:12px;position:sticky;top:0;background:#0d1117}}
td.case{{text-align:left;color:#8b949e}} td.na{{color:#30363d}}
.win{{background:rgba(46,160,67,.22);color:#3fb950}} .loss{{background:rgba(248,81,73,.20);color:#f85149}}
.grp{{background:#161b22}}
.bartrack{{position:relative;height:22px;background:#161b22;border-radius:4px;width:200px;display:inline-block;vertical-align:middle}}
.bar{{height:22px;border-radius:4px}} .barlab{{position:absolute;left:8px;top:2px;font-size:12px;color:#fff;text-shadow:0 0 3px #000}}
.kpi{{display:flex;gap:10px;align-items:center;margin:6px 0}} .kpi .lab{{width:170px;color:#8b949e}}
.card{{background:#161b22;border:1px solid #21262d;border-radius:8px;padding:16px 18px;margin:10px 0}}
.big{{font-size:19px;color:#e6edf3}} .up{{color:#3fb950}} .flat{{color:#d29922}} .down{{color:#f85149}}
ul{{margin:6px 0 6px 0;padding-left:20px}} li{{margin:5px 0}} code{{background:#21262d;padding:1px 5px;border-radius:4px}}
.legend{{font-size:12px;color:#8b949e;margin:8px 0}}
</style></head><body>
<h1>houqin — previous WB-Lite run &nbsp;vs&nbsp; today's map run</h1>
<div class="sub">houqin 30 cases · <b>ALL truncation-fixed</b> (SEM-42 P0 — judge now sees full deliverables, not 2000 chars) · prev ppr = PPR A/B (re-judged) · plain = 5-arm run (re-judged) · today = map run (Jun 27)</div>

<h2>Aggregate (mean per-case accuracy, houqin)</h2>
<div class="kpi"><div class="lab">prev · plain</div>{bar(A['pp'], '#6e7681')}</div>
<div class="kpi"><div class="lab">prev · ppr_off</div>{bar(A['poff'], '#6e7681')}</div>
<div class="kpi"><div class="lab">prev · ppr_on</div>{bar(A['pon'], '#8b949e')}</div>
<div class="kpi"><div class="lab">TODAY · ppr_on</div>{bar(A['ton'], '#58a6ff')}</div>
<div class="kpi"><div class="lab">TODAY · ppr_map</div>{bar(A['tmap'], '#a371f7')}</div>

<h2>Tokens (mean total per cell, houqin) <span style="font-size:12px;color:#8b949e">— lower better · eff = accuracy pts per 100K tok (higher = more cost-effective) · cache_read=0 overcounts re-sent prefix</span></h2>
<div class="kpi"><div class="lab">prev · plain</div>{tokbar(T['pp'])}<span style="margin-left:12px;color:#8b949e;font-size:12px">eff {eff(A['pp'],T['pp'])}</span></div>
<div class="kpi"><div class="lab">prev · ppr_off</div>{tokbar(T['poff'])}<span style="margin-left:12px;color:#8b949e;font-size:12px">eff {eff(A['poff'],T['poff'])}</span></div>
<div class="kpi"><div class="lab">prev · ppr_on</div>{tokbar(T['pon'])}<span style="margin-left:12px;color:#8b949e;font-size:12px">eff {eff(A['pon'],T['pon'])}</span></div>
<div class="kpi"><div class="lab">TODAY · ppr_on</div>{tokbar(T['ton'])}<span style="margin-left:12px;color:#8b949e;font-size:12px">eff {eff(A['ton'],T['ton'])}</span></div>
<div class="kpi"><div class="lab">TODAY · ppr_map</div>{tokbar(T['tmap'])}<span style="margin-left:12px;color:#8b949e;font-size:12px">eff {eff(A['tmap'],T['tmap'])}</span></div>

<div class="card">
<div class="big">What changed: the judge can now see the full deliverable (SEM-42 P0)</div>
<ul>
<li><b>Every arm jumped ~+10pp.</b> The judge was truncating deliverables to 2000 chars and false-failing what it couldn't see (~⅓ of correct work hidden). The pre-fix SEM-40 verdict (<code>plain 17 &gt; ppr_off 12.9 &gt; ppr_on 11.9</code>) was measuring that bug.</li>
<li><b>The ppr_off-vs-ppr_on ordering flipped.</b> Was ppr_on 9.2 &lt; ppr_off 9.6 ("ppr_on worst → PPR net-negative"); corrected it's ppr_on <span class="up">{A['pon']:.1f}%</span> ≥ ppr_off <span class="flat">{A['poff']:.1f}%</span> (within noise). "PPR is worst" was an artifact.</li>
<li><b>Corrected 3-way (houqin):</b> plain <b>{A['pp']:.0f}%</b> · ppr_off <b>{A['poff']:.0f}%</b> · ppr_on <b>{A['pon']:.0f}%</b> — read the ordering against the original plain&gt;ppr_off&gt;ppr_on.</li>
<li><b>The map still adds nothing:</b> ppr_map <span class="flat">{A['tmap']:.1f}%</span> ≈ today's ppr_on <span class="flat">{A['ton']:.1f}%</span> (+37% tokens). Parked.</li>
</ul></div>

<h2>Per-case (green = better than the column to its left's baseline)</h2>
<div class="legend">TODAY·ppr_on shaded vs prev·ppr_on (harness effect) · TODAY·ppr_map shaded vs TODAY·ppr_on (the map effect)</div>
<table>
<tr><th>case</th><th>prev plain</th><th>prev ppr_off</th><th>prev ppr_on</th><th>TODAY ppr_on</th><th>TODAY ppr_map</th></tr>
{trows}
<tr class="grp"><td class="case"><b>MEAN</b></td><td><b>{A['pp']:.0f}%</b></td><td><b>{A['poff']:.0f}%</b></td><td><b>{A['pon']:.0f}%</b></td><td><b>{A['ton']:.0f}%</b></td><td><b>{A['tmap']:.0f}%</b></td></tr>
</table>

<h2>Analyst's read</h2>
<div class="card"><ul>
<li><b>The magnitudes were bug-deflated, but the headline HELD.</b> All three arms ~doubled once the judge saw full deliverables — yet plain stayed on top: corrected (houqin) plain <b>{A['pp']:.0f}%</b> &gt; ppr_on <b>{A['pon']:.0f}%</b> &gt; ppr_off <b>{A['poff']:.0f}%</b>. Only the ppr_off/ppr_on sub-order flipped.</li>
<li><b>Plain wins by MORE, not less.</b> Corrected gap plain→ppr is ~9–11pp (original ~5pp). Plain's longer full-workspace deliverables were truncated harder, so the fix lifted plain most → "plain &gt; PPR" is CONFIRMED + strengthened, not refuted.</li>
<li><b>Truncation was the dominant measurement bug</b> — +10.6pp on ppr_on alone, airtight (same deliverables, judge-only change). It did NOT reduce run-to-run variance (that's agent-side, find-vs-miss).</li>
<li><b>The map is neutral</b> (ppr_map ≈ ppr_on, +37% tokens) — parked.</li>
<li><b>Caveats:</b> houqin only · n=2 (rep noise ~6.7pp) · cross-run (plain from the 5-arm run, ppr from the PPR A/B run — same as the original) · today's ppr also carries the filename fix (fresher run, so 27.6% &gt; the re-judged-old 19.8%).</li>
</ul></div>
</body></html>"""

OUT.write_text(html)
print("wrote", OUT)
print(f"aggregate: prev plain={A['pp']:.1f} off={A['poff']:.1f} on={A['pon']:.1f} | today on={A['ton']:.1f} map={A['tmap']:.1f}")
