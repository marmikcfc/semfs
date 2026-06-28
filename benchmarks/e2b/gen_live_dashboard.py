#!/usr/bin/env python3
"""Live HTML dashboard for the in-flight PM + kaifa 3-arm runs (GLM-5.1-NVFP4).

Headline metric = TOKEN REDUCTION of each semfs arm vs `plain`, computed per-case
(matched) so missing cells don't bias it. Also shows live progress (cells done /
expected per arm) + recent completions, so you can see what's running right now.

Scans per-cell result.json under e2b_runs/. Regenerate on a loop; the page
auto-refreshes in the browser:
    while true; do python3 benchmarks/e2b/gen_live_dashboard.py; sleep 20; done

Usage: python3 benchmarks/e2b/gen_live_dashboard.py [out.html]
"""
import json, os, glob, pathlib, re, subprocess, sys
from datetime import datetime
from statistics import mean

REPO = pathlib.Path(__file__).resolve().parents[2]
OUT_DIR = REPO / "tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs"
DASH = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else (
    # Under artifacts/ which the Modal bake's _ignore_source excludes — so the 20s regen
    # loop can keep writing during a `modal run` build without tripping "modified during build".
    REPO / "tickets/workspace-bench-5arm-matrix/artifacts/LIVE_DASHBOARD.html")

ALABEL = {"plain": "plain", "hiddenkg_edges": "hkg-edges", "hiddenkg_retrieval": "hkg-retrieval",
          "cloud": "cloud (supermem)"}
WS = {
    "pm":    {"name": "PM (chanpin)",     "cases": ["15","44","45","53","55","95","171","175","386","388"], "reps": 3,
              "arms": ["plain", "hiddenkg_edges", "hiddenkg_retrieval", "cloud"]},
    "kaifa": {"name": "Backend (kaifa)",  "cases": ["3","7","91","92","94","226","242","266","286","300","311"], "reps": 3,
              "arms": ["plain", "hiddenkg_edges", "hiddenkg_retrieval"]},
}
LABEL_RE = re.compile(r"_r([PK])([123])[per]$")   # current-run reps only (excludes rKsmoke*)


def load_cells():
    rows = []
    for rj in glob.glob(str(OUT_DIR / "*" / "result.json")):
        try:
            d = json.load(open(rj))
        except Exception:
            continue
        label = d.get("label", "")
        m = LABEL_RE.search(label)
        if not m:
            continue
        ws = "kaifa" if m.group(1) == "K" else "pm"
        rows.append({
            "ws": ws, "rep": m.group(2), "case": str(d.get("case")), "arm": d.get("arm"),
            "status": d.get("status"), "tokens": d.get("tokens"),
            "acc": d.get("accuracy"), "grep": d.get("used_semfs_grep"),
            "calls": d.get("calls"),
        })
    return rows


def okt(rows):  # ok cells with a real token count
    return [r for r in rows if r["status"] == "ok" and isinstance(r["tokens"], (int, float)) and r["tokens"] > 0]


def arm_mean_tokens(rows, ws, arm):
    v = [r["tokens"] for r in okt(rows) if r["ws"] == ws and r["arm"] == arm]
    return mean(v) if v else None


def matched_reduction(rows, ws, arm):
    """Mean per-case token reduction (%) of `arm` vs plain, over cases where BOTH
    have >=1 ok cell. Positive = arm uses FEWER tokens than plain."""
    cells = okt(rows)
    per = []
    for case in WS[ws]["cases"]:
        p = [r["tokens"] for r in cells if r["ws"] == ws and r["arm"] == "plain" and r["case"] == case]
        a = [r["tokens"] for r in cells if r["ws"] == ws and r["arm"] == arm and r["case"] == case]
        if p and a:
            per.append((mean(p) - mean(a)) / mean(p) * 100.0)
    return (mean(per), len(per)) if per else (None, 0)


def proc_count(pat):
    try:
        out = subprocess.run(["pgrep", "-f", pat], capture_output=True, text=True, timeout=5).stdout
        return len([x for x in out.splitlines() if x.strip()])
    except Exception:
        return 0


def fnum(x, suf=""):
    return "—" if x is None else f"{x:,.0f}{suf}"


def pct(x):
    return "—" if x is None else f"{x:+.1f}%"


def render():
    rows = load_cells()
    now = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    pm_active = proc_count("run_pm3.sh")
    kf_active = proc_count("run_kaifa3.sh")
    rm_active = proc_count("run_matrix.py")

    cards = []
    for wsk, wsmeta in WS.items():
        wr = [r for r in rows if r["ws"] == wsk]
        ncases, reps = len(wsmeta["cases"]), wsmeta["reps"]
        arms = wsmeta["arms"]
        expected = ncases * reps
        # progress per arm
        prog = []
        for arm in arms:
            ar = [r for r in wr if r["arm"] == arm]
            ok = len([r for r in ar if r["status"] == "ok"])
            fail = len([r for r in ar if r["status"] not in ("ok", None)])
            done = ok + fail
            mt = arm_mean_tokens(rows, wsk, arm)
            red, nred = (None, 0) if arm == "plain" else matched_reduction(rows, wsk, arm)
            prog.append((arm, done, expected, ok, fail, mt, red, nred))

        # hero reduction tiles
        tiles = ""
        for arm, done, exp, ok, fail, mt, red, nred in prog:
            if arm == "plain":
                tiles += (f'<div class="tile plain"><div class="t-arm">plain</div>'
                          f'<div class="t-big">{fnum(mt)}</div><div class="t-sub">mean tokens · baseline</div></div>')
            else:
                cls = "good" if (red is not None and red > 0) else ("bad" if red is not None else "na")
                tiles += (f'<div class="tile {cls}"><div class="t-arm">{ALABEL[arm]}</div>'
                          f'<div class="t-big">{pct(red)}</div>'
                          f'<div class="t-sub">tokens vs plain · {fnum(mt)} mean · n={nred} cases</div></div>')

        # progress bars
        bars = ""
        for arm, done, exp, ok, fail, mt, red, nred in prog:
            w = int(100 * done / exp) if exp else 0
            failtxt = f' · <span class="fail">{fail} fail</span>' if fail else ""
            bars += (f'<div class="prow"><div class="plabel">{ALABEL[arm]}</div>'
                     f'<div class="pbar"><div class="pfill" style="width:{w}%"></div>'
                     f'<span class="ptxt">{done}/{exp} cells ({ok} ok{failtxt})</span></div></div>')

        # per-case token table
        nonplain = [a for a in arms if a != "plain"]
        thead = "".join(f"<th>{ALABEL[a]}</th>" for a in arms) + "".join(f"<th>{ALABEL[a]} Δ</th>" for a in nonplain)
        trows = ""
        cells = okt(rows)
        for case in wsmeta["cases"]:
            tds = ""
            means = {}
            for arm in arms:
                v = [r["tokens"] for r in cells if r["ws"] == wsk and r["arm"] == arm and r["case"] == case]
                means[arm] = mean(v) if v else None
                tds += f"<td>{fnum(means[arm])}</td>"
            def dcell(arm):
                p, a = means["plain"], means[arm]
                if p and a:
                    r = (p - a) / p * 100
                    c = "good" if r > 0 else "bad"
                    return f'<td class="{c}">{r:+.0f}%</td>'
                return "<td>—</td>"
            trows += f"<tr><td class='case'>{case}</td>{tds}{''.join(dcell(a) for a in nonplain)}</tr>"

        total_done = sum(p[1] for p in prog)
        cards.append(f"""
        <section class="card">
          <h2>{wsmeta['name']} <span class="muted">— {total_done}/{ncases*reps} cells (n={reps}, {ncases} cases × {len(arms)} arms)</span></h2>
          <div class="tiles">{tiles}</div>
          <div class="bars">{bars}</div>
          <table><thead><tr><th>case</th>{thead}</tr></thead><tbody>{trows}</tbody></table>
        </section>""")

    running = []
    if pm_active: running.append(f'<span class="run">PM running ({pm_active})</span>')
    if kf_active: running.append(f'<span class="run">kaifa running ({kf_active})</span>')
    if not running: running.append('<span class="idle">no run procs active</span>')
    e2b_est = rm_active  # rough: each run_matrix invocation ~PAR sandboxes; show invocation count

    html = f"""<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="refresh" content="15">
<title>semfs live — PM + kaifa token reduction</title>
<style>
  :root {{ --bg:#0d1117; --card:#161b22; --line:#30363d; --fg:#e6edf3; --mut:#8b949e;
          --good:#3fb950; --bad:#f85149; --plain:#58a6ff; }}
  *{{box-sizing:border-box}} body{{margin:0;background:var(--bg);color:var(--fg);
    font:14px/1.5 -apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif}}
  header{{padding:18px 24px;border-bottom:1px solid var(--line);position:sticky;top:0;background:var(--bg);z-index:9}}
  h1{{margin:0;font-size:18px}} .sub{{color:var(--mut);font-size:13px;margin-top:4px}}
  .run{{color:var(--good);font-weight:600;margin-right:14px}} .idle{{color:var(--mut)}}
  main{{padding:20px 24px;display:grid;gap:22px;max-width:1100px;margin:0 auto}}
  .card{{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:18px 20px}}
  h2{{margin:0 0 14px;font-size:16px}} .muted{{color:var(--mut);font-weight:400;font-size:13px}}
  .tiles{{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;margin-bottom:16px}}
  .tile{{border:1px solid var(--line);border-radius:10px;padding:14px;text-align:center;background:#0d1117}}
  .tile.good{{border-color:var(--good)}} .tile.bad{{border-color:var(--bad)}} .tile.plain{{border-color:var(--plain)}}
  .t-arm{{color:var(--mut);font-size:12px;text-transform:uppercase;letter-spacing:.5px}}
  .t-big{{font-size:30px;font-weight:700;margin:6px 0}}
  .tile.good .t-big{{color:var(--good)}} .tile.bad .t-big{{color:var(--bad)}} .tile.plain .t-big{{color:var(--plain)}}
  .t-sub{{color:var(--mut);font-size:12px}}
  .bars{{display:grid;gap:8px;margin-bottom:16px}}
  .prow{{display:flex;align-items:center;gap:12px}} .plabel{{width:110px;color:var(--mut);font-size:13px}}
  .pbar{{position:relative;flex:1;height:22px;background:#0d1117;border:1px solid var(--line);border-radius:6px;overflow:hidden}}
  .pfill{{position:absolute;left:0;top:0;bottom:0;background:linear-gradient(90deg,#1f6feb,#388bfd);opacity:.55}}
  .ptxt{{position:absolute;left:10px;top:0;line-height:22px;font-size:12px}}
  .fail{{color:var(--bad)}}
  table{{width:100%;border-collapse:collapse;font-size:13px}}
  th,td{{padding:5px 8px;border-bottom:1px solid var(--line);text-align:right}}
  th:first-child,td.case{{text-align:left;color:var(--mut)}}
  td.good{{color:var(--good)}} td.bad{{color:var(--bad)}}
  .note{{color:var(--mut);font-size:12px;max-width:1100px;margin:0 auto;padding:0 24px 30px}}
</style></head><body>
<header>
  <h1>semfs live — token reduction (GLM-5.1-NVFP4)</h1>
  <div class="sub">{' '.join(running)} · run_matrix invocations: {rm_active} · updated {now} · auto-refresh 15s</div>
</header>
<main>
{''.join(cards)}
</main>
<p class="note"><b>Metric = TOKEN USAGE only</b> (not $ cost). <b>Reduction</b> = mean per-case token change of an
arm vs <code>plain</code> (green = FEWER tokens = win; red = more). Matched per-case so missing cells don't bias it.
GLM-5.1-NVFP4 on vLLM DOES have prefix caching (~97% engine hit) but the vLLM↔codex usage accounting counts full
prompt_tokens/turn — so these are accounting tokens, counted the same for every arm (relative comparison valid).
<b>cloud</b> = Supermemory server-side search (PM only). Kaifa semfs arms run grep-only (SEARCH_ONLY, no browsable tree).
Accuracy is judged after the run (not shown here).</p>
</body></html>"""
    DASH.write_text(html)
    return len(rows)


if __name__ == "__main__":
    n = render()
    print(f"dashboard → {DASH}  ({n} current-run cells)")
