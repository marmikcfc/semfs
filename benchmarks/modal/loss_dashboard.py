"""Accumulating loss dashboard for a Modal training run.

`modal app logs` only returns a recent window, so run this in a loop: each pass scrapes the
current window, MERGES new (epoch,loss) points into a persistent JSONL (dedup), and regenerates
the HTML from the full accumulated history — so the curve survives even as logs scroll off.

  while true; do python benchmarks/modal/loss_dashboard.py <app-id>; sleep 20; done   # live
"""
import json
import os
import re
import subprocess
import sys

AID = sys.argv[1] if len(sys.argv) > 1 else "ap-wgAYkY7XXZuTLKpGlnqFcN"
TOTAL = 1318
DATA = "/tmp/loss_data.json"
OUT = "/tmp/loss_dashboard.html"

raw = subprocess.run(["modal", "app", "logs", AID], capture_output=True, text=True, timeout=90)
log = re.sub(r"[│╭╰─╮╯]", "", (raw.stdout or "") + (raw.stderr or ""))

# accumulate, keyed by rounded epoch so re-scrapes dedup
store = json.load(open(DATA)) if os.path.exists(DATA) else {"train": {}, "eval": {}}
for m in re.finditer(r"\{'loss': '?([\d.]+)'?.*?'epoch': '?([\d.]+)'?\}", log):
    store["train"][f"{float(m.group(2)):.4f}"] = float(m.group(1))
for m in re.finditer(r"\{'eval_loss': '?([\d.]+)'?.*?'epoch': '?([\d.]+)'?\}", log):
    store["eval"][f"{float(m.group(2)):.4f}"] = float(m.group(1))
json.dump(store, open(DATA, "w"))

train = sorted(({"x": float(k), "y": v} for k, v in store["train"].items()), key=lambda p: p["x"])
evals = sorted(({"x": float(k), "y": v} for k, v in store["eval"].items()), key=lambda p: p["x"])

prog = re.findall(r"(\d+)/" + str(TOTAL) + r" \[([\d:]+)<([\d:]+),\s*([\d.]+)s/it\]", log)
step, elapsed, eta, rate = (prog[-1] if prog else ("0", "-", "-", "-"))
done = re.search(r"'train_runtime': '?([\d.]+)'?", log)
status = "✅ COMPLETE" if done else ("🟢 TRAINING" if prog else "⏳ starting")
cur_loss = train[-1]["y"] if train else "-"
cur_eval = evals[-1]["y"] if evals else "-"
pct = round(100 * int(step) / TOTAL, 1) if step.isdigit() else 0

html = f"""<!doctype html><html><head><meta charset=utf8><meta http-equiv=refresh content=25>
<title>Compressor LoRA — loss</title><script src="https://cdn.jsdelivr.net/npm/chart.js@4"></script>
<style>
 body{{font:14px -apple-system,system-ui,sans-serif;background:#0d1117;color:#e6edf3;margin:0;padding:28px}}
 h1{{font-size:18px;margin:0 0 4px}} .sub{{color:#8b949e;margin-bottom:20px}}
 .cards{{display:flex;gap:14px;flex-wrap:wrap;margin-bottom:24px}}
 .card{{background:#161b22;border:1px solid #30363d;border-radius:10px;padding:14px 18px;min-width:118px}}
 .card .k{{color:#8b949e;font-size:11px;text-transform:uppercase;letter-spacing:.05em}}
 .card .v{{font-size:22px;font-weight:600;margin-top:4px}} .green{{color:#3fb950}} .blue{{color:#58a6ff}}
 .wrap{{background:#161b22;border:1px solid #30363d;border-radius:10px;padding:18px}}
 .bar{{height:6px;background:#30363d;border-radius:3px;overflow:hidden;margin-top:10px}} .bar>div{{height:100%;background:#3fb950;width:{pct}%}}
</style></head><body>
<h1>Qwen3-1.7B compressor — LoRA training</h1>
<div class=sub>app {AID} · {status} · auto-refreshes every 25s · {len(train)} train / {len(evals)} eval points</div>
<div class=cards>
 <div class=card><div class=k>Step</div><div class=v>{step}/{TOTAL}</div><div class=bar><div></div></div></div>
 <div class=card><div class=k>Rate</div><div class=v>{rate} s/it</div></div>
 <div class=card><div class=k>Elapsed</div><div class=v>{elapsed}</div></div>
 <div class=card><div class=k>ETA</div><div class=v>{eta}</div></div>
 <div class=card><div class=k>Train loss</div><div class="v green">{cur_loss}</div></div>
 <div class=card><div class=k>Eval loss</div><div class="v blue">{cur_eval}</div></div>
</div>
<div class=wrap><canvas id=c height=108></canvas></div>
<script>
const train={json.dumps(train)}, evals={json.dumps(evals)};
new Chart(document.getElementById('c'),{{type:'line',data:{{datasets:[
  {{label:'train loss',data:train,borderColor:'#3fb950',backgroundColor:'#3fb95022',tension:.25,pointRadius:0,borderWidth:2}},
  {{label:'eval loss',data:evals,borderColor:'#58a6ff',tension:.1,pointRadius:4,borderWidth:2}}]}},
 options:{{parsing:false,scales:{{
   x:{{type:'linear',title:{{display:true,text:'epoch',color:'#8b949e'}},grid:{{color:'#21262d'}},ticks:{{color:'#8b949e'}}}},
   y:{{title:{{display:true,text:'loss',color:'#8b949e'}},grid:{{color:'#21262d'}},ticks:{{color:'#8b949e'}}}}}},
  plugins:{{legend:{{labels:{{color:'#e6edf3'}}}}}}}}}});
</script></body></html>"""
open(OUT, "w").write(html)
print(f"train {len(train)} | eval {len(evals)} | step {step}/{TOTAL} | loss {cur_loss} eval {cur_eval} -> {OUT}")
