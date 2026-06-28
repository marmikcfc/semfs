#!/usr/bin/env python3
"""Standalone Modal app to bake per-persona E2B templates (Option B).

Decoupled from ``semfs_modal.py`` ON PURPOSE: that module's agent-runner
functions reference Modal secrets (``claude``, ``codex-auth``) that aren't
provisioned, and ``modal run`` hydrates EVERY function's secrets at app load —
so running the bake through that module fails before it starts. This slim app
needs nothing but the volume + the ``e2b`` secret.

What it does (all Modal-side, zero bytes through the local machine):
  1. tar  /data/corpus/{persona}_standard            -> ctx/corpus.tgz
  2. copy /data/seeds/{persona}-gemma-q4.db + WAL-checkpoint it to one clean file
  3. E2B Template.build: from_template(base) + .copy(corpus.tgz, seed)  -> semfs-mount-{persona}

Cases are ~10 KB so they are NOT baked — the runner pushes them at boot.
research stays runtime-pull (19 GB corpus); this is for the small personas.

Run:  modal run benchmarks/modal/bake_e2b_persona.py::bake --persona kaifa
"""
import json
import shutil
import sqlite3
import tarfile
import tempfile
from pathlib import Path

import modal

app = modal.App("semfs-bake")
image = modal.Image.debian_slim(python_version="3.11").pip_install("e2b")
data_volume = modal.Volume.from_name("semfs-bench-data")
VOL = "/data"


@app.function(
    image=image,
    volumes={VOL: data_volume},
    secrets=[modal.Secret.from_name("e2b")],
    timeout=7200,
    cpu=4,
    memory=8192,
)
def bake(
    persona: str = "kaifa",
    base_template: str = "semfs-baked",
    template_name: str = "",
) -> dict:
    """Bake one persona's E2B template from Modal volume assets.

    Inherits a persona-AGNOSTIC base (``semfs-baked``: WB harness + gemma-q4
    embedder + FUSE deps + adaptive-K binary) and bakes IN the persona's two
    heavy assets so the sandbox boots with ZERO runtime upload:
      - /opt/corpus.tgz            (arcname {persona}_standard for predictable extract)
      - /opt/{persona}-gemma-q4.db (WAL-checkpointed to a single self-contained file)
    """
    from e2b import Template

    tmpl = template_name or f"semfs-mount-{persona}"
    corpus_dir = Path(f"{VOL}/corpus/{persona}_standard")
    seed_src = Path(f"{VOL}/seeds/{persona}-gemma-q4.db")
    seed_name = f"{persona}-gemma-q4.db"

    missing = [str(p) for p in (corpus_dir, seed_src) if not p.exists()]
    if missing:
        raise RuntimeError(f"missing volume assets for {persona}: {missing}")

    print(f"[bake:{persona}] base={base_template} -> {tmpl}", flush=True)
    print(f"  corpus_dir={corpus_dir}", flush=True)
    print(f"  seed={seed_src} size={seed_src.stat().st_size}", flush=True)

    with tempfile.TemporaryDirectory(prefix=f"e2b_{persona}_") as td:
        ctx = Path(td)

        # 1) tar the corpus volume-side (arcname = {persona}_standard).
        corpus_tgz = ctx / "corpus.tgz"
        print(f"[bake:{persona}] tarring corpus -> corpus.tgz", flush=True)
        with tarfile.open(corpus_tgz, "w:gz") as tar:
            tar.add(corpus_dir, arcname=f"{persona}_standard")
        print(f"  corpus.tgz size={corpus_tgz.stat().st_size}", flush=True)

        # 2) copy seed (+ any hot wal/shm), then fold the WAL into the main file.
        # Why: baking a live .db + hot -wal and reopening it later re-creates the
        # main/WAL desync that corrupted houqin (rcas/2026-06-20-sqlite-corruption).
        seed_dst = ctx / seed_name
        shutil.copy2(seed_src, seed_dst)
        for sfx in ("-wal", "-shm"):
            sp = Path(str(seed_src) + sfx)
            if sp.exists():
                shutil.copy2(sp, str(seed_dst) + sfx)
                print(f"  copied hot {seed_name}{sfx}", flush=True)
        con = sqlite3.connect(str(seed_dst))
        con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        con.execute("PRAGMA journal_mode=DELETE")  # single-file DB, no wal sidecar
        try:
            qc = con.execute("PRAGMA quick_check").fetchone()[0]
        except Exception as ex:  # vec0 module absent in plain sqlite3 -> advisory only
            qc = f"skipped ({repr(ex)[:60]})"
        con.close()
        for sfx in ("-wal", "-shm"):
            p = Path(str(seed_dst) + sfx)
            if p.exists():
                p.unlink()
        print(f"  seed checkpointed size={seed_dst.stat().st_size} quick_check={qc}", flush=True)

        # 3) build the E2B template from the base, adding the two persona assets.
        t = Template(file_context_path=str(ctx))
        b = (
            t.from_template(base_template)
             .copy("corpus.tgz", "/opt/corpus.tgz", user="root")
             .copy(seed_name, f"/opt/{seed_name}", user="root")
        )
        print(f"[bake:{persona}] calling E2B Template.build({tmpl})", flush=True)
        info = Template.build(
            b,
            name=tmpl,
            cpu_count=4,
            memory_mb=8192,
            request_timeout=1800.0,
            on_build_logs=lambda e: print("  >", getattr(e, "message", str(e))[:400], flush=True),
        )
        out = {
            "persona": persona,
            "template": tmpl,
            "base": base_template,
            "corpus_bytes": corpus_tgz.stat().st_size,
            "seed_bytes": seed_dst.stat().st_size,
            "seed_quick_check": qc,
            "build_info": str(info),
        }
        print(f"[bake:{persona}] build finished", flush=True)
        print(json.dumps(out, default=str), flush=True)
        return out


def _checkpoint_seed(src: Path, dst: Path) -> str:
    """Copy a seed (+ any hot wal/shm) and fold the WAL into a single clean file.

    Same houqin-corruption guard as ``bake``: never bake a live .db + hot -wal.
    Returns the PRAGMA quick_check result ('ok', or 'skipped(...)' if the vec0
    module isn't loadable in plain sqlite3)."""
    shutil.copy2(src, dst)
    for sfx in ("-wal", "-shm"):
        sp = Path(str(src) + sfx)
        if sp.exists():
            shutil.copy2(sp, str(dst) + sfx)
    con = sqlite3.connect(str(dst))
    con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    con.execute("PRAGMA journal_mode=DELETE")
    try:
        qc = con.execute("PRAGMA quick_check").fetchone()[0]
    except Exception as ex:
        qc = f"skipped ({repr(ex)[:60]})"
    con.close()
    for sfx in ("-wal", "-shm"):
        p = Path(str(dst) + sfx)
        if p.exists():
            p.unlink()
    return qc


@app.function(
    image=image,
    volumes={VOL: data_volume},
    secrets=[modal.Secret.from_name("e2b")],
    timeout=7200,
    cpu=4,
    memory=8192,
)
def bake_chanpin_v3(template_name: str = "semfs-baked-v3-gemma") -> dict:
    """chanpin's v3 template, refreshed with the NEW gemma-KG seed.

    "v3 version with gemma embeddings": the v3 lineage = chanpin corpus + the 4
    matrix seeds + office WRITER libs (python-docx/pptx/openpyxl, needed by WB
    synthesis cases). This rebuilds it from the persona-agnostic ``semfs-baked``
    base, swapping in the current gemma-q4-embedded + Gemma-4-31B-KG
    ``chanpin-gemma-q4.db`` (the prior v3 carried the OLD-KG one), keeping the
    clean/leanhint3/4arm arm seeds. Built under a NEW alias so the in-use
    ``semfs-baked-v3`` is not clobbered until verified.
    """
    from e2b import Template

    corpus_dir = Path(f"{VOL}/corpus/chanpin_standard")
    seed_names = [
        "chanpin-gemma-q4.db",   # PRIMARY — the new gemma-q4 + Gemma-4-31B KG seed
        "chanpin-clean.db",
        "chanpin-leanhint3.db",
        "chanpin-4arm.db",
    ]
    seed_srcs = {n: Path(f"{VOL}/seeds/{n}") for n in seed_names}

    missing = [str(p) for p in (corpus_dir, *seed_srcs.values()) if not p.exists()]
    if missing:
        raise RuntimeError(f"missing chanpin v3 assets: {missing}")

    print(f"[chanpin-v3] -> {template_name}", flush=True)
    with tempfile.TemporaryDirectory(prefix="e2b_chanpin_v3_") as td:
        ctx = Path(td)

        corpus_tgz = ctx / "corpus.tgz"
        print("[chanpin-v3] tarring corpus -> corpus.tgz", flush=True)
        with tarfile.open(corpus_tgz, "w:gz") as tar:
            tar.add(corpus_dir, arcname="chanpin_standard")
        print(f"  corpus.tgz size={corpus_tgz.stat().st_size}", flush=True)

        checks = {}
        for n, src in seed_srcs.items():
            qc = _checkpoint_seed(src, ctx / n)
            checks[n] = qc
            print(f"  seed {n} size={(ctx / n).stat().st_size} quick_check={qc}", flush=True)

        t = Template(file_context_path=str(ctx))
        b = t.from_template("semfs-baked").copy("corpus.tgz", "/opt/corpus.tgz", user="root")
        for n in seed_names:
            b = b.copy(n, f"/opt/{n}", user="root")
        # v3 feature: office WRITER libs (build-time network; agent runtime has none).
        b = (
            b.set_user("root")
             .run_cmd("apt-get update -qq && apt-get install -y -qq python3-pip")
             .run_cmd("python3 -m pip install --break-system-packages --no-cache-dir "
                      "python-docx python-pptx openpyxl")
             .run_cmd("python3 -c 'import docx, pptx, openpyxl; print(chr(111)+chr(107))'")
             .set_user("user")
        )
        print(f"[chanpin-v3] calling E2B Template.build({template_name})", flush=True)
        info = Template.build(
            b,
            name=template_name,
            cpu_count=4,
            memory_mb=8192,
            request_timeout=1800.0,
            on_build_logs=lambda e: print("  >", getattr(e, "message", str(e))[:400], flush=True),
        )
        out = {
            "template": template_name,
            "base": "semfs-baked",
            "corpus_bytes": corpus_tgz.stat().st_size,
            "seeds": list(seed_names),
            "seed_quick_checks": checks,
            "writer_libs": ["python-docx", "python-pptx", "openpyxl"],
            "build_info": str(info),
        }
        print("[chanpin-v3] build finished", flush=True)
        print(json.dumps(out, default=str), flush=True)
        return out
