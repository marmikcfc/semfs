//! The gateway's built-in config page. Served at `GET /config`; it reads the
//! live state from `/health` and current values from `/config.json`, and saves
//! back via `POST /config` (JSON). Self-contained — inline CSS/JS, no external
//! requests — since it's served off loopback with a strict-ish origin.

pub const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>tokopt · backend config</title>
<style>
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body { font: 15px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
         max-width: 640px; margin: 40px auto; padding: 0 20px; }
  h1 { font-size: 20px; margin: 0 0 4px; }
  .sub { opacity: .65; margin: 0 0 24px; }
  .card { border: 1px solid rgba(128,128,128,.3); border-radius: 10px; padding: 18px 20px; margin: 16px 0; }
  label { display: block; font-weight: 600; margin: 14px 0 4px; }
  .hint { font-weight: 400; opacity: .6; font-size: 13px; }
  input { width: 100%; padding: 9px 11px; border: 1px solid rgba(128,128,128,.4);
          border-radius: 7px; background: transparent; color: inherit; font: inherit; }
  button { margin-top: 18px; padding: 10px 18px; border: 0; border-radius: 8px;
           background: #4f46e5; color: #fff; font: inherit; font-weight: 600; cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  .row { display: flex; gap: 8px; align-items: baseline; }
  .pill { display: inline-block; font-size: 12px; padding: 1px 8px; border-radius: 999px;
          border: 1px solid rgba(128,128,128,.4); }
  .ok { color: #16a34a; } .bad { color: #dc2626; }
  code { background: rgba(128,128,128,.15); padding: 1px 5px; border-radius: 4px; font-size: 13px; }
  #msg { margin-top: 12px; min-height: 20px; font-size: 14px; }
  table { width: 100%; border-collapse: collapse; font-size: 14px; }
  td { padding: 4px 0; }
  td:first-child { opacity: .6; width: 110px; }
</style>
</head>
<body>
  <h1>tokopt backend config</h1>
  <p class="sub">Where the proxy sends <code>/optimize</code> (compress + route) calls.</p>

  <div class="card">
    <div class="row" style="justify-content:space-between">
      <strong>Live status</strong>
      <span id="mode" class="pill">…</span>
    </div>
    <table id="status"><tbody>
      <tr><td>router</td><td id="s-router">…</td></tr>
      <tr><td>compressor</td><td id="s-compressor">…</td></tr>
    </tbody></table>
    <p class="hint" style="margin:10px 0 0">
      Precedence: <code>ROUTER_*</code>/<code>COMPRESSOR_*</code> env (BYO) &gt;
      this page &gt; <code>TOKOPT_ENV=dev</code>→localhost &gt; our hosted endpoint.
      BYO env, if set, wins over anything saved here.
    </p>
  </div>

  <div class="card">
    <strong>Config</strong>
    <label>Backend URL
      <span class="hint">— your <code>/optimize</code> endpoint. Leave blank to use env/localhost/hosted defaults.</span></label>
    <input id="backend_url" placeholder="https://your-backend.example.com" />

    <label>Backend API key <span class="hint">— optional; stored 0600 in ~/.tokopt/config.json</span></label>
    <input id="backend_api_key" type="password" placeholder="(unchanged)" />

    <label>Router model <span class="hint">— tier-picker model</span></label>
    <input id="router_model" placeholder="gpt-4.1-nano" />

    <label>Compressor model</label>
    <input id="compressor_model" placeholder="chopratejas/kompress-v2-base" />

    <button id="save">Save</button>
    <div id="msg"></div>
    <p class="hint" style="margin-top:6px">Saved values apply immediately — no gateway restart needed.</p>
  </div>

<script>
function reachable(x){ return x ? '<span class="ok">reachable</span>' : '<span class="bad">unreachable</span>'; }
async function refresh(){
  try {
    const h = await (await fetch('/health')).json();
    document.getElementById('mode').textContent = h.mode || 'gateway';
    const r = h.router||{}, c = h.compressor||{};
    document.getElementById('s-router').innerHTML =
      `<code>${r.model}</code> · ${r.source} · ${reachable(r.reachable)}`;
    document.getElementById('s-compressor').innerHTML =
      `<code>${c.model}</code> · ${c.source} · ${reachable(c.reachable)}`;
  } catch(e){}
  try {
    const cfg = await (await fetch('/config.json')).json();
    if (cfg.backend_url) document.getElementById('backend_url').value = cfg.backend_url;
    if (cfg.router_model) document.getElementById('router_model').value = cfg.router_model;
    if (cfg.compressor_model) document.getElementById('compressor_model').value = cfg.compressor_model;
    if (cfg.backend_api_key) document.getElementById('backend_api_key').placeholder = '•••••••• (set — leave blank to keep)';
  } catch(e){}
}
document.getElementById('save').addEventListener('click', async () => {
  const btn = document.getElementById('save'), msg = document.getElementById('msg');
  btn.disabled = true; msg.textContent = 'saving…';
  const body = {
    backend_url: document.getElementById('backend_url').value.trim() || null,
    router_model: document.getElementById('router_model').value.trim() || null,
    compressor_model: document.getElementById('compressor_model').value.trim() || null,
  };
  const key = document.getElementById('backend_api_key').value;
  if (key) body.backend_api_key = key;           // blank = leave existing key untouched
  try {
    const res = await fetch('/config', {
      method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify(body),
    });
    if (!res.ok) throw new Error(await res.text());
    msg.innerHTML = '<span class="ok">saved ✓</span>';
    document.getElementById('backend_api_key').value = '';
    await refresh();
  } catch(e){ msg.innerHTML = '<span class="bad">error: '+e.message+'</span>'; }
  btn.disabled = false;
});
refresh();
</script>
</body>
</html>
"##;
