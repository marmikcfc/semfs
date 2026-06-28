#!/usr/bin/env python3
"""Generate a self-contained HTML dashboard over ALL e2b matrix runs.
One row per cell (run): tokens + what happened (status) + accuracy (if judged),
grouped/filterable by rep. Reads result.json (status/tokens/etc.) and the
rubrics_judge--*.json (accuracy) from each cell dir.

Run:  python3 benchmarks/e2b/gen_run_dashboard.py
Out:  tickets/workspace-bench-5arm-matrix/artifacts/RUN_DASHBOARD.html
"""
import json, re, pathlib, html, time

REPO = pathlib.Path(__file__).resolve().parents[2]
RUNS = REPO / "tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs"
OUT  = REPO / "tickets/workspace-bench-5arm-matrix/artifacts/RUN_DASHBOARD.html"


def rep_of(label, agent, case, arm):
    """rep = whatever trails pm_<agent>_<case>_<arm>_ in the label."""
    pfx = f"pm_{agent}_{case}_{arm}_"
    return label[len(pfx):] if label.startswith(pfx) else label


def fam_of(rep):
    """rep family = rep with the trailing run-index stripped (ra1p1->ra1p, rfp3->rfp)."""
    return re.sub(r'[0-9]+[a-zA-Z]?$', '', rep) or rep


def accuracy(cell):
    jf = list(cell.glob("rubrics_judge--*.json"))
    if not jf:
        return None
    try:
        s = json.load(open(jf[0])).get("summary", {})
        if s.get("total"):
            return {"passed": s["passed"], "total": s["total"]}
    except Exception:
        pass
    return None


rows = []
for rj in sorted(RUNS.glob("pm_*/result.json")):
    cell = rj.parent
    try:
        d = json.load(open(rj))
    except Exception:
        continue
    label = d.get("label", cell.name)
    agent = d.get("agent", "codex")
    case  = str(d.get("case", "?"))
    arm   = d.get("arm", "?")
    rep   = rep_of(label, agent, case, arm)
    acc   = accuracy(cell)
    ndeliv = len(d.get("deliverables") or [])
    status = d.get("status", "?")
    # richer outcome: distinguish ok-but-empty
    outcome = status
    if status == "ok" and ndeliv == 0:
        outcome = "ok-empty"
    rows.append({
        "label": label, "case": case, "arm": arm, "rep": rep, "fam": fam_of(rep),
        "agent": agent, "status": status, "outcome": outcome,
        "tokens": int(d.get("tokens") or 0),
        "wall_s": int(d.get("wall_s") or 0),
        "calls": int(d.get("calls") or 0),
        "semfs_grep": bool(d.get("used_semfs_grep")),
        "filename_ok": bool(d.get("followed_filename")),
        "ndeliv": ndeliv,
        "acc_p": acc["passed"] if acc else None,
        "acc_t": acc["total"] if acc else None,
        "err": (d.get("err") or "")[:300],
    })

# ---- aggregates ----
total = len(rows)
ok = sum(1 for r in rows if r["status"] == "ok")
to = sum(1 for r in rows if r["status"] == "timeout")
empty = sum(1 for r in rows if r["outcome"] == "ok-empty")
ok_toks = [r["tokens"] for r in rows if r["status"] == "ok" and r["tokens"] > 0]
mean_tok = int(sum(ok_toks) / len(ok_toks)) if ok_toks else 0
sum_tok = sum(ok_toks)
judged = sum(1 for r in rows if r["acc_t"])

# per-rep-family pivot
fams = {}
for r in rows:
    f = fams.setdefault(r["fam"], {"n": 0, "ok": 0, "to": 0, "toks": [], "acc_p": 0, "acc_t": 0})
    f["n"] += 1
    if r["status"] == "ok": f["ok"] += 1
    if r["status"] == "timeout": f["to"] += 1
    if r["status"] == "ok" and r["tokens"] > 0: f["toks"].append(r["tokens"])
    if r["acc_t"]: f["acc_p"] += r["acc_p"]; f["acc_t"] += r["acc_t"]

fam_rows = []
for f, v in sorted(fams.items()):
    mt = int(sum(v["toks"]) / len(v["toks"])) if v["toks"] else 0
    accpct = (100 * v["acc_p"] / v["acc_t"]) if v["acc_t"] else None
    fam_rows.append({"fam": f, "n": v["n"], "ok": v["ok"], "to": v["to"],
                     "mean_tok": mt, "accpct": accpct, "acc_t": v["acc_t"]})

DATA = json.dumps(rows)
FAMS = json.dumps(fam_rows)
ts = time.strftime("%Y-%m-%d %H:%M:%S")

HTML = """<!doctype html><html lang=en><head><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1">
<title>E2B Run Dashboard</title>
<style>
:root{--bg:#0d1117;--panel:#161b22;--bd:#30363d;--fg:#e6edf3;--mut:#8b949e;
--ok:#2ea043;--to:#f85149;--empty:#d29922;--accent:#58a6ff}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);
font:13px/1.45 ui-monospace,SFMono-Regular,Menlo,monospace}
h1{font-size:18px;margin:0 0 2px}.sub{color:var(--mut);font-size:12px;margin-bottom:14px}
.wrap{padding:18px 22px;max-width:1500px;margin:0 auto}
.cards{display:flex;flex-wrap:wrap;gap:10px;margin-bottom:16px}
.card{background:var(--panel);border:1px solid var(--bd);border-radius:8px;padding:10px 14px;min-width:120px}
.card .v{font-size:22px;font-weight:600}.card .l{color:var(--mut);font-size:11px;text-transform:uppercase;letter-spacing:.04em}
.card.ok .v{color:var(--ok)}.card.to .v{color:var(--to)}.card.em .v{color:var(--empty)}
table{border-collapse:collapse;width:100%;background:var(--panel);border:1px solid var(--bd);border-radius:8px;overflow:hidden}
th,td{padding:5px 9px;text-align:left;border-bottom:1px solid var(--bd);white-space:nowrap}
th{background:#1c2128;cursor:pointer;user-select:none;position:sticky;top:0;font-size:11px;text-transform:uppercase;letter-spacing:.03em;color:var(--mut)}
th:hover{color:var(--fg)}tr:hover td{background:#1c2128}
td.num{text-align:right;font-variant-numeric:tabular-nums}
.pill{display:inline-block;padding:1px 7px;border-radius:10px;font-size:11px;font-weight:600}
.pill.ok{background:rgba(46,160,67,.18);color:var(--ok)}
.pill.timeout{background:rgba(248,81,73,.18);color:var(--to)}
.pill.ok-empty{background:rgba(210,153,34,.18);color:var(--empty)}
.acc{font-weight:600}
.bar{height:14px;border-radius:3px;background:#21262d;position:relative;width:64px;display:inline-block;vertical-align:middle}
.bar>i{position:absolute;left:0;top:0;bottom:0;border-radius:3px;background:var(--accent)}
.controls{display:flex;gap:8px;align-items:center;margin-bottom:10px;flex-wrap:wrap}
input,select{background:var(--panel);border:1px solid var(--bd);color:var(--fg);border-radius:6px;padding:6px 9px;font:inherit}
input{min-width:240px}
button{background:var(--panel);border:1px solid var(--bd);color:var(--fg);border-radius:6px;padding:6px 11px;font:inherit;cursor:pointer}
button.active{border-color:var(--accent);color:var(--accent)}
.muted{color:var(--mut)}.sec{margin:22px 0 8px;font-size:14px;color:var(--accent)}
.scroll{max-height:62vh;overflow:auto;border-radius:8px}
details>summary{cursor:pointer;color:var(--accent);margin:14px 0 6px}
.t-yes{color:var(--ok)}.t-no{color:var(--mut)}
</style></head><body><div class=wrap>
<h1>E2B Matrix — Run Dashboard</h1>
<div class=sub>__TS__ · one row per run · model under test: <b>GLM-5.1-NVFP4</b> · tokens are FRESH (no prefix caching)</div>
<div class=cards id=cards></div>

<details open><summary>Per-rep-family summary (click to collapse)</summary>
<div class=scroll><table id=famtab><thead><tr>
<th data-k=fam>rep family</th><th data-k=n class=num>runs</th><th data-k=ok class=num>ok</th>
<th data-k=to class=num>timeout</th><th data-k=mean_tok class=num>mean tok (ok)</th>
<th data-k=accpct class=num>accuracy</th></tr></thead><tbody></tbody></table></div></details>

<div class=sec>All runs</div>
<div class=controls>
<input id=q placeholder="filter: label / case / arm / rep …">
<button data-f=all class=active>all</button>
<button data-f=ok>ok</button>
<button data-f=timeout>timeout</button>
<button data-f=ok-empty>ok-empty</button>
<button data-f=judged>judged only</button>
<span class=muted id=count></span>
</div>
<div class=scroll><table id=tab><thead><tr>
<th data-k=case>case</th><th data-k=arm>arm</th><th data-k=rep>rep</th>
<th data-k=outcome>status</th><th data-k=tokens class=num>tokens</th>
<th data-k=wall_s class=num>wall&nbsp;s</th><th data-k=calls class=num>calls</th>
<th data-k=semfs_grep>semfs&nbsp;grep</th><th data-k=filename_ok>file✓</th>
<th data-k=ndeliv class=num>deliv</th><th data-k=accpct class=num>accuracy</th>
</tr></thead><tbody></tbody></table></div>
<div class=sub style="margin-top:18px">accuracy = rubrics passed/total (Seed-2.0-Lite judge). blank = not yet judged.</div>
</div>
<script>
const DATA=__DATA__, FAMS=__FAMS__;
DATA.forEach(r=>r.accpct = r.acc_t ? 100*r.acc_p/r.acc_t : null);
const cards=[
 ['total runs', DATA.length, ''],
 ['ok', DATA.filter(r=>r.status=='ok').length, 'ok'],
 ['timeout', DATA.filter(r=>r.status=='timeout').length, 'to'],
 ['ok-empty', DATA.filter(r=>r.outcome=='ok-empty').length, 'em'],
 ['mean tok (ok)', __MEANTOK__.toLocaleString(), ''],
 ['total tok', __SUMTOK__.toLocaleString(), ''],
 ['judged', DATA.filter(r=>r.acc_t).length, ''],
];
document.getElementById('cards').innerHTML = cards.map(([l,v,c])=>
 `<div class="card ${c}"><div class=v>${v}</div><div class=l>${l}</div></div>`).join('');

function accCell(p,t){ if(!t) return '<span class=muted>—</span>';
 const pct=100*p/t, col = pct>=60?'#2ea043':pct>=30?'#d29922':'#f85149';
 return `<span class=acc style=color:${col}>${p}/${t}</span> <span class=bar><i style="width:${pct}%;background:${col}"></i></span>`; }

let curF='all', curK='case', curDir=1;
const tb=document.querySelector('#tab tbody');
function render(){
 const q=document.getElementById('q').value.toLowerCase();
 let rows=DATA.filter(r=>{
  if(curF=='judged' && !r.acc_t) return false;
  if(curF!='all' && curF!='judged' && r.outcome!=curF && r.status!=curF) return false;
  if(q && !(`${r.label} ${r.case} ${r.arm} ${r.rep}`.toLowerCase().includes(q))) return false;
  return true;});
 rows.sort((a,b)=>{let x=a[curK],y=b[curK];
  if(x==null)x=-1; if(y==null)y=-1;
  if(typeof x=='string'){return curDir*x.localeCompare(y);} return curDir*(x-y);});
 tb.innerHTML=rows.map(r=>`<tr>
  <td>${r.case}</td><td>${r.arm}</td><td>${r.rep}</td>
  <td><span class="pill ${r.outcome}">${r.outcome}</span></td>
  <td class=num>${r.tokens?r.tokens.toLocaleString():'<span class=muted>0</span>'}</td>
  <td class=num>${r.wall_s||''}</td><td class=num>${r.calls||''}</td>
  <td class="${r.semfs_grep?'t-yes':'t-no'}">${r.semfs_grep?'yes':'·'}</td>
  <td class="${r.filename_ok?'t-yes':'t-no'}">${r.filename_ok?'yes':'·'}</td>
  <td class=num>${r.ndeliv||''}</td>
  <td class=num>${accCell(r.acc_p,r.acc_t)}</td></tr>`).join('');
 document.getElementById('count').textContent=`${rows.length} shown`;
}
document.querySelectorAll('#tab th').forEach(th=>th.onclick=()=>{
 const k=th.dataset.k; curDir = (curK==k)?-curDir:1; curK=k; render();});
document.querySelectorAll('[data-f]').forEach(b=>b.onclick=()=>{
 curF=b.dataset.f; document.querySelectorAll('[data-f]').forEach(x=>x.classList.remove('active'));
 b.classList.add('active'); render();});
document.getElementById('q').oninput=render;

// fam table
const ftb=document.querySelector('#famtab tbody');
let fK='fam', fDir=1;
function frender(){
 const rows=[...FAMS].sort((a,b)=>{let x=a[fK],y=b[fK]; if(x==null)x=-1;if(y==null)y=-1;
  if(typeof x=='string')return fDir*x.localeCompare(y); return fDir*(x-y);});
 ftb.innerHTML=rows.map(r=>`<tr>
  <td>${r.fam}</td><td class=num>${r.n}</td>
  <td class=num style=color:var(--ok)>${r.ok}</td>
  <td class=num style=color:var(--to)>${r.to||''}</td>
  <td class=num>${r.mean_tok?r.mean_tok.toLocaleString():''}</td>
  <td class=num>${r.acc_t?accCell(Math.round(r.accpct*r.acc_t/100),r.acc_t):'<span class=muted>—</span>'}</td>
 </tr>`).join('');
}
document.querySelectorAll('#famtab th').forEach(th=>th.onclick=()=>{
 const k=th.dataset.k; fDir=(fK==k)?-fDir:1; fK=k; frender();});
render(); frender();
</script></body></html>"""

HTML = (HTML.replace("__TS__", ts).replace("__DATA__", DATA).replace("__FAMS__", FAMS)
            .replace("__MEANTOK__", str(mean_tok)).replace("__SUMTOK__", str(sum_tok)))
OUT.write_text(HTML)
print(f"wrote {OUT}")
print(f"  {total} runs · ok={ok} timeout={to} ok-empty={empty} · judged={judged}")
print(f"  mean tok (ok)={mean_tok:,} · total tok={sum_tok:,}")
