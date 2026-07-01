"""Bake a next_plaid E2B template for one cell.

Inherits the persona's existing base template (`semfs-mount-{persona}` — WB harness +
cases + judge + seed) and bakes IN the next_plaid assets so the cell boots ready:
  - /opt/np/{cell}.tgz   the baked colgrep index + corpus (relocates to /srv/np/{cell})
  - /opt/np/colgrep      the colgrep x86_64-linux binary
  - /opt/np/lfm2.tgz     the LFM2 ONNX model dir (relocates to /lfm2/...) [LFM2 cells]
  - /opt/np/rrf_merge.py the dual-index merge shim [Config-C cells]
Cell boot (arm wiring) extracts these to their stable paths. Relocation verified:
phase2_reloc_test.py (index found by path-hash, no rebuild).

Run: modal run benchmarks/modal/bake_nextplaid.py --cell houqin_all --persona houqin --needs-lfm2
"""
import modal, pathlib

app = modal.App("bake-nextplaid")
idx = modal.Volume.from_name("np-indexes")
lfm2 = modal.Volume.from_name("np-lfm2-onnx")

image = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("curl", "ca-certificates")
    .pip_install("e2b")
    .run_commands(
        "curl --proto '=https' --tlsv1.2 -LsSf "
        "https://github.com/lightonai/next-plaid/releases/latest/download/colgrep-installer.sh | sh || true"
    )
)


@app.function(image=image, volumes={"/idx": idx, "/lfm2": lfm2},
              timeout=2400, cpu=4, memory=8192,
              secrets=[modal.Secret.from_name("e2b")])
def bake(cell: str, persona: str, needs_lfm2: bool = False, needs_merge: bool = False,
         base_template: str = "", template_name: str = "", rrf_content: str = "",
         lfm2_in_image: bool = False):
    import tarfile, shutil, os, tempfile, glob
    from e2b import Template

    base = base_template or f"semfs-mount-{persona}"
    cells = [x.strip() for x in cell.split(",") if x.strip()]   # comma-sep → dual-index (kaifa-C)
    tmpl = template_name or f"np-{cells[0].replace('_', '-')}"
    for cl in cells:
        if not os.path.isdir(f"/idx/baked/{cl}"):
            raise RuntimeError(f"missing baked artifact /idx/baked/{cl}")
    colgrep = shutil.which("colgrep") or next(
        (p for p in ["/root/.cargo/bin/colgrep", "/root/.local/bin/colgrep", "/usr/local/bin/colgrep"]
         if os.path.exists(p)), None)
    if not colgrep:  # last resort: search incl. hidden dirs (.cargo)
        cands = glob.glob("/root/**/colgrep", recursive=True, include_hidden=True)
        colgrep = next((c for c in cands if os.path.isfile(c) and os.access(c, os.X_OK)), None)
    if not colgrep:
        raise RuntimeError("colgrep binary not found in bake image")
    print(f"[bake-np:{cell}] base={base} -> {tmpl}  colgrep={colgrep}", flush=True)

    with tempfile.TemporaryDirectory() as td:
        ctx = pathlib.Path(td)
        b = Template(file_context_path=str(ctx)).from_template(base)
        for cl in cells:   # one index tar per lane (extracts to /srv/np/{cl})
            with tarfile.open(ctx / f"{cl}.tgz", "w:gz") as t:
                t.add(f"/idx/baked/{cl}", arcname=cl)
            b = b.copy(f"{cl}.tgz", f"/opt/np/{cl}.tgz", user="root")
        shutil.copy2(colgrep, ctx / "colgrep")
        b = b.copy("colgrep", "/opt/np/colgrep", user="root")
        if needs_lfm2 and lfm2_in_image:
            # Copy the model DIR straight into the READ-ONLY image at /lfm2 — no .tgz, no
            # build-time tar (which peaks at 3GB: .tgz + extracted) and no 1.5GB writable
            # extract at runtime. Drop model_int8.onnx (the fp32 index doesn't use it).
            shutil.copytree("/lfm2/lfm2-colbert-350m-onnx", ctx / "lfm2-colbert-350m-onnx",
                            ignore=shutil.ignore_patterns("model_int8.onnx"))
            b = b.copy("lfm2-colbert-350m-onnx", "/lfm2/lfm2-colbert-350m-onnx", user="root")
        elif needs_lfm2:
            with tarfile.open(ctx / "lfm2.tgz", "w:gz") as t:
                t.add("/lfm2/lfm2-colbert-350m-onnx", arcname="lfm2-colbert-350m-onnx")
            b = b.copy("lfm2.tgz", "/opt/np/lfm2.tgz", user="root")
        if needs_merge and rrf_content:
            (ctx / "rrf_merge.py").write_text(rrf_content)
            b = b.copy("rrf_merge.py", "/opt/np/rrf_merge.py", user="root")
        sizes = {p.name: p.stat().st_size for p in ctx.iterdir()}
        print(f"[bake-np:{cell}] context: {sizes}", flush=True)
        info = Template.build(b, name=tmpl, cpu_count=4, memory_mb=8192, request_timeout=2400.0,
                              on_build_logs=lambda e: print("  >", getattr(e, "message", str(e))[:300], flush=True))
        print(f"[bake-np:{cell}] built {tmpl}", flush=True)
        return {"ok": True, "cell": cell, "template": tmpl, "base": base, "context_sizes": sizes}


@app.local_entrypoint()
def main(cell: str = "houqin_all", persona: str = "houqin",
         needs_lfm2: bool = True, needs_merge: bool = False, template_name: str = "",
         lfm2_in_image: bool = False):
    rrf = ""
    if needs_merge:
        rrf = (pathlib.Path(__file__).resolve().parents[2]
               / "tickets/next-plaid-late-interaction/rrf_merge.py").read_text()
    print("BAKE:", bake.remote(cell, persona, needs_lfm2, needs_merge,
                               template_name=template_name, rrf_content=rrf,
                               lfm2_in_image=lfm2_in_image))
