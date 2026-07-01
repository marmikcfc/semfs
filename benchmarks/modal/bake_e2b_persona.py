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
    corpus_path: str = "",
    corpus_arcname: str = "",
    exclude_names: str = "",
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
    # corpus_path/corpus_arcname let non-WB personas (e.g. xafs at /data/corpus/xafs
    # with dp_XXX/data/** structure) override the WB `{persona}_standard` convention.
    corpus_dir = Path(corpus_path) if corpus_path else Path(f"{VOL}/corpus/{persona}_standard")
    arcname = corpus_arcname or f"{persona}_standard"
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
        # exclude_names drops answer-key files (e.g. question.json,tasks.json for xafs)
        # so the agent's baked workdir never contains the gold answers (leak guard).
        excl = {x for x in exclude_names.split(",") if x}
        def _drop(ti):
            return None if ti.name.rsplit("/", 1)[-1] in excl else ti
        print(f"[bake:{persona}] tarring corpus -> corpus.tgz (exclude={sorted(excl)})", flush=True)
        with tarfile.open(corpus_tgz, "w:gz") as tar:
            tar.add(corpus_dir, arcname=arcname, filter=(_drop if excl else None))
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
def bake_perdp(dps: str = "dp_001,dp_002,dp_003,dp_004,dp_005,dp_006,dp_007,dp_008,dp_009",
               base_template: str = "semfs-baked",
               template_name: str = "semfs-perdp-xafs") -> dict:
    """Bake per-dp xAFS SEARCH seeds (each → /opt/<dp>-gemma-q4.db) into one template, for
    scoped-search runs (mount THIS question's dp seed). Seeds are tiny (~MB each) → fast bake."""
    from e2b import Template

    dp_list = [d.strip() for d in dps.split(",") if d.strip()]
    with tempfile.TemporaryDirectory(prefix="e2b_perdp_") as td:
        ctx = Path(td)
        staged = []
        for dp in dp_list:
            src = Path(f"{VOL}/seeds/{dp}-gemma-q4.db")
            if not src.exists():
                print(f"  SKIP {dp}: seed missing", flush=True)
                continue
            dst = ctx / f"{dp}-gemma-q4.db"
            qc = _checkpoint_seed(src, dst)  # WAL→single file (houqin-corruption guard)
            staged.append(dp)
            print(f"  staged {dp}: {dst.stat().st_size} bytes (quick_check={qc})", flush=True)
        b = Template(file_context_path=str(ctx)).from_template(base_template)
        for dp in staged:
            b = b.copy(f"{dp}-gemma-q4.db", f"/opt/{dp}-gemma-q4.db", user="root")
        print(f"[bake_perdp] building {template_name} with {len(staged)} seeds", flush=True)
        info = Template.build(b, name=template_name, cpu_count=4, memory_mb=8192,
                              request_timeout=1800.0,
                              on_build_logs=lambda e: print("  >", getattr(e, "message", str(e))[:300], flush=True))
        out = {"template": template_name, "base": base_template, "seeds": staged, "build_info": str(info)}
        print("[bake_perdp] build finished", flush=True)
        print(json.dumps(out, default=str), flush=True)
        return out


@app.local_entrypoint()
def bake_perdp_main(dps: str = "dp_001,dp_002,dp_003,dp_004,dp_005,dp_006,dp_007,dp_008,dp_009",
                    template_name: str = "semfs-perdp-xafs"):
    print(json.dumps(bake_perdp.remote(dps, "semfs-baked", template_name), default=str, indent=2))


@app.function(
    image=image,
    volumes={VOL: data_volume},
    secrets=[modal.Secret.from_name("e2b")],
    timeout=7200,
    cpu=4,
    memory=8192,
)
def bake_perdp_plain(dps: str = "dp_001,dp_002,dp_003,dp_004,dp_005,dp_006,dp_007,dp_008,dp_009,dp_010,dp_011,dp_012,dp_013",
                     base_template: str = "semfs-baked") -> dict:
    """Bake a corpus-ONLY template PER xAFS persona (plain-xafs-<dp>) for the plain/FS arm:
    real grep/find/cat over that persona's raw files, NO semfs seed. One separate template each."""
    from e2b import Template

    dp_list = [d.strip() for d in dps.split(",") if d.strip()]
    out = []
    for dp in dp_list:
        corpus_dir = Path(f"{VOL}/corpus/xafs/{dp}")
        if not corpus_dir.exists():
            print(f"  SKIP {dp}: no corpus at {corpus_dir}", flush=True)
            continue
        with tempfile.TemporaryDirectory(prefix=f"e2b_plain_{dp}_") as td:
            ctx = Path(td)
            corpus_tgz = ctx / "corpus.tgz"
            with tarfile.open(corpus_tgz, "w:gz") as tar:
                tar.add(corpus_dir, arcname=f"{dp}_standard")
            tmpl = f"plain-xafs-{dp}"
            b = (Template(file_context_path=str(ctx)).from_template(base_template)
                 .copy("corpus.tgz", "/opt/corpus.tgz", user="root"))
            print(f"[bake_plain] building {tmpl} (corpus {corpus_tgz.stat().st_size} bytes)", flush=True)
            info = Template.build(b, name=tmpl, cpu_count=4, memory_mb=8192, request_timeout=1800.0,
                                  on_build_logs=lambda e: print("  >", getattr(e, "message", str(e))[:200], flush=True))
            out.append({"dp": dp, "template": tmpl, "corpus_bytes": corpus_tgz.stat().st_size})
            print(f"[bake_plain] {dp} → {tmpl} build finished", flush=True)
    print(json.dumps({"plain_templates": out}, default=str), flush=True)
    return {"plain_templates": out}


@app.local_entrypoint()
def bake_perdp_plain_main(dps: str = "dp_001,dp_002,dp_003,dp_004,dp_005,dp_006,dp_007,dp_008,dp_009,dp_010,dp_011,dp_012,dp_013"):
    print(json.dumps(bake_perdp_plain.remote(dps, "semfs-baked"), default=str, indent=2))


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
