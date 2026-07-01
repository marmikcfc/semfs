"""Phase 1 — build a next-plaid index for one (persona, lane, model) via colgrep.

Materializes the corpus from the seed (chunks → files, routed: code at raw ext for
tree-sitter, everything else as .extracted.md for the text path), installs colgrep,
runs `colgrep init <dir> --model <id|local-dir>`, and smoke-queries.

Shakedown order: houqin-A (small, single LFM2-ONNX) → then kaifa-C (2 models) → xAFS-A (19K, LateOn).
Run: modal run tickets/next-plaid-late-interaction/phase1_build.py
"""
import modal

app = modal.App("phase1-build-index")
seeds = modal.Volume.from_name("semfs-bench-data")
lfm2 = modal.Volume.from_name("np-lfm2-onnx")
out = modal.Volume.from_name("np-indexes", create_if_missing=True)

image = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("curl", "ca-certificates")
    .run_commands(
        "curl --proto '=https' --tlsv1.2 -LsSf "
        "https://github.com/lightonai/next-plaid/releases/latest/download/colgrep-installer.sh "
        "| sh || echo 'INSTALLER_FAILED'"
    )
)

CODE_INNER = {".go", ".py", ".ts", ".tsx", ".js", ".jsx", ".java", ".rs", ".c", ".h",
              ".cpp", ".cc", ".cs", ".rb", ".php", ".kt", ".swift", ".scala", ".lua",
              ".sh", ".bash", ".sql", ".r", ".jl", ".tf", ".vue", ".svelte"}
WRAP = {".md", ".txt", ".extracted"}


def inner_ext(p):
    import os
    q = p.lower()
    while True:
        root, ext = os.path.splitext(q)
        if ext in WRAP and root:
            q = root
        else:
            break
    return os.path.splitext(q)[1]


@app.function(image=image, volumes={"/seeds": seeds, "/lfm2": lfm2, "/out": out},
              timeout=7200, cpu=8.0, memory=32768)
def build(persona: str, model: str, lane: str = "all"):
    import sqlite3, os, subprocess, shutil
    from collections import defaultdict

    colgrep = shutil.which("colgrep") or next(
        (p for p in ["/root/.local/bin/colgrep", "/root/.cargo/bin/colgrep", "/usr/local/bin/colgrep"] if os.path.exists(p)), None)
    print("colgrep binary:", colgrep, flush=True)
    if not colgrep:
        return {"ok": False, "reason": "colgrep not installed", "PATH_probe": os.listdir("/root/.local/bin") if os.path.isdir("/root/.local/bin") else "no ~/.local/bin"}

    if model.startswith("local:"):
        model = model[len("local:"):]
        print("model dir contents:", os.listdir(model) if os.path.isdir(model) else "MISSING", flush=True)
        # PyPI pylate-onnx-export (older) saves config_sentence_transformers.json;
        # colgrep (latest) requires onnx_config.json (same content). Bridge it.
        ocfg, scfg = os.path.join(model, "onnx_config.json"), os.path.join(model, "config_sentence_transformers.json")
        if not os.path.exists(ocfg) and os.path.exists(scfg):
            shutil.copy(scfg, ocfg)
            lfm2.commit()
            print("fixup: copied config_sentence_transformers.json → onnx_config.json", flush=True)

    seed = f"/seeds/seeds/{persona}-gemma-q4.db"
    # Cell-stable absolute path (valid in both Modal build + E2B cell), so colgrep's
    # path-hash index lookup + absolute result paths resolve identically after bake.
    stable = f"/srv/np/{persona}_{lane}"
    corpus = f"{stable}/corpus"
    xdg = f"{stable}/_xdg"
    shutil.rmtree(stable, ignore_errors=True)
    os.makedirs(xdg, exist_ok=True)
    con = sqlite3.connect(f"file:{seed}?mode=ro", uri=True)
    byf = defaultdict(list)
    for fp, ordd, t in con.execute("SELECT filepath, ord, text FROM chunks WHERE text IS NOT NULL"):
        if fp:
            byf[fp].append((ordd or 0, t))
    con.close()
    written = code_n = doc_n = 0
    for fp, parts in byf.items():
        parts.sort()
        content = "\n".join(t for _, t in parts)
        ie = inner_ext(fp)
        rel = fp.lstrip("/")
        if ie in CODE_INNER:
            if lane == "doc":
                continue
            dest = os.path.join(corpus, rel); code_n += 1
        else:
            if lane == "code":
                continue
            dest = os.path.join(corpus, rel + ".extracted.md"); doc_n += 1
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        with open(dest, "w") as f:
            f.write(content)
        written += 1
    print(f"materialized {written} files (code={code_n} doc={doc_n}) → {corpus}", flush=True)

    env = {**os.environ, "HOME": "/root", "XDG_DATA_HOME": xdg, "XDG_CONFIG_HOME": xdg}
    r = subprocess.run([colgrep, "init", corpus, "--model", model, "-y"],
                       capture_output=True, text=True, env=env)
    print("INIT stdout:\n" + r.stdout[-5000:], flush=True)
    print("INIT stderr:\n" + r.stderr[-5000:], flush=True)
    q = subprocess.run([colgrep, "--model", model, "--json", "staffing summary report"], cwd=corpus,
                       capture_output=True, text=True, env=env)
    print("QUERY stdout:\n" + q.stdout[:1500], flush=True)
    print("QUERY stderr:\n" + q.stderr[:800], flush=True)
    # Persist corpus + XDG index together (keyed to the stable path) for the bake.
    dest = f"/out/baked/{persona}_{lane}"
    shutil.rmtree(dest, ignore_errors=True)
    shutil.copytree(stable, dest)
    out.commit()
    xdg_files = sum(len(f) for _, _, f in os.walk(xdg))
    print(f"persisted → {dest} (xdg index files: {xdg_files})", flush=True)
    return {"ok": r.returncode == 0, "persona": persona, "lane": lane, "stable_path": stable,
            "written": written, "code_n": code_n, "doc_n": doc_n, "init_code": r.returncode,
            "query_hit": bool(q.stdout.strip().startswith("[")), "xdg_files": xdg_files}


@app.local_entrypoint()
def main(persona: str = "houqin", model: str = "local:/lfm2/lfm2-colbert-350m-onnx", lane: str = "all"):
    print("RESULT:", build.remote(persona, model, lane))
