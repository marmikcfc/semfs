"""Phase 2 de-risk — does a persisted colgrep index RELOCATE to a fresh container?

Simulates a fresh E2B cell: copy baked/houqin_all → the stable path /srv/np/houqin_all,
point XDG at its _xdg, and query. PASS = returns results AND does NOT print "Building
index" (i.e. it FOUND the prebuilt index by path-hash, didn't re-encode). This is the
core assumption the bake relies on. No E2B, no LLM.

Run: modal run tickets/next-plaid-late-interaction/phase2_reloc_test.py
"""
import modal

app = modal.App("phase2-reloc-test")
idx = modal.Volume.from_name("np-indexes")
lfm2 = modal.Volume.from_name("np-lfm2-onnx")
image = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("curl", "ca-certificates")
    .run_commands(
        "curl --proto '=https' --tlsv1.2 -LsSf "
        "https://github.com/lightonai/next-plaid/releases/latest/download/colgrep-installer.sh | sh || true"
    )
)


@app.function(image=image, volumes={"/idx": idx, "/lfm2": lfm2}, timeout=900, cpu=4.0)
def reloc():
    import shutil, os, subprocess, json, glob
    colgrep = shutil.which("colgrep") or next(
        (p for p in ["/root/.local/bin/colgrep", "/root/.cargo/bin/colgrep",
                     "/usr/local/bin/colgrep"] if os.path.exists(p)), None)
    if not colgrep:
        cands = glob.glob("/root/**/colgrep", recursive=True) + glob.glob("/usr/**/colgrep", recursive=True)
        colgrep = next((c for c in cands if os.path.isfile(c) and os.access(c, os.X_OK)), None)
    print("colgrep located at:", colgrep, flush=True)
    # fresh cell: place the baked artifact at the SAME stable path it was built at
    shutil.rmtree("/srv/np/houqin_all", ignore_errors=True)
    os.makedirs("/srv/np", exist_ok=True)
    shutil.copytree("/idx/baked/houqin_all", "/srv/np/houqin_all")
    corpus = "/srv/np/houqin_all/corpus"
    xdg = "/srv/np/houqin_all/_xdg"
    env = {**os.environ, "HOME": "/root", "XDG_DATA_HOME": xdg, "XDG_CONFIG_HOME": xdg}
    q = subprocess.run([colgrep, "--model", "/lfm2/lfm2-colbert-350m-onnx", "--json",
                        "staffing summary report"], cwd=corpus, capture_output=True, text=True, env=env)
    rebuilt = "Building index" in q.stderr
    try:
        n = len(json.loads(q.stdout))
    except Exception:
        n = -1
    print("STDERR:", q.stderr[:700], flush=True)
    print("results:", n, "| rebuilt:", rebuilt, flush=True)
    if n > 0:
        print("top file:", json.loads(q.stdout)[0].get("unit", {}).get("file"), flush=True)
    return {"ok": n > 0 and not rebuilt, "results": n, "rebuilt_in_cell": rebuilt}


@app.local_entrypoint()
def main():
    print("RELOC:", reloc.remote())
