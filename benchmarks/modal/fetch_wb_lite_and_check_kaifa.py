"""Two data-prep jobs on Modal (network + the volume live here):

1. fetch_rubrics  — download the FULL Workspace-Bench-Lite (all personas incl.
   Backend Engineer) from HuggingFace and write one metadata.json (= the rubric)
   per task to the volume under wb/lite_all/task_lite_clean_en/<id>/. Emits a
   persona index so we can see which case ids belong to which persona.
2. check_kaifa    — completeness check of the backend-dev seed
   seeds/kaifa-gemma-q4.db: tries our own `semfs seed-verify`, plus a pure-SQL
   coverage backup (chunks coverage / unindexed / error stubs).

Usage:
  modal run benchmarks/modal/fetch_wb_lite_and_check_kaifa.py
  modal volume get semfs-bench-data /wb/lite_all benchmarks/e2b/assets/wb_lite_all --force
"""
import modal

app = modal.App("wb-lite-fetch")
vol = modal.Volume.from_name("semfs-bench-data")
VOL = "/data"

img = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("libssl3", "libgomp1", "ca-certificates")
    .pip_install("datasets>=2.0", "huggingface_hub")
)


@app.function(image=img, volumes={VOL: vol}, timeout=1800)
def fetch_rubrics():
    import json, os, collections
    from datasets import load_dataset
    ds = load_dataset("Workspace-Bench/Workspace-Bench-Lite", split="lite")
    base = f"{VOL}/wb/lite_all/task_lite_clean_en"
    os.makedirs(base, exist_ok=True)
    persona = collections.defaultdict(list)
    cols = list(ds.features.keys())
    sample = [{"id": ds[i].get("id"), "absolute_id": ds[i].get("absolute_id"),
               "persona": ds[i].get("persona")} for i in range(min(3, len(ds)))]
    n = 0
    for row in ds:
        d = {k: row[k] for k in cols}
        cid = d.get("id")
        if cid is None:
            cid = d.get("absolute_id")
        cid = str(cid)
        os.makedirs(f"{base}/{cid}", exist_ok=True)
        with open(f"{base}/{cid}/metadata.json", "w") as f:
            json.dump(d, f, ensure_ascii=False, indent=2)
        persona[str(d.get("persona", "?"))].append(cid)
        n += 1
    with open(f"{VOL}/wb/lite_all/persona_index.json", "w") as f:
        json.dump({k: sorted(v, key=lambda x: int(x) if x.isdigit() else 0)
                   for k, v in persona.items()}, f, ensure_ascii=False, indent=2)
    vol.commit()
    return {"columns": cols, "sample": sample, "tasks": n,
            "by_persona": {k: len(v) for k, v in sorted(persona.items())},
            "personas_ids": {k: sorted(v, key=lambda x: int(x) if x.isdigit() else 0)
                             for k, v in sorted(persona.items())}}


@app.function(image=img, volumes={VOL: vol}, timeout=300)
def tar_rubrics():
    """Tar the rubric tree into one file (single-file `modal volume get` is
    reliable; directory get is not in this CLI version)."""
    import subprocess, os
    subprocess.run("cd /data/wb && tar czf lite_all.tgz lite_all", shell=True, check=True)
    vol.commit()
    return {"tgz_bytes": os.path.getsize("/data/wb/lite_all.tgz")}


@app.function(image=img, volumes={VOL: vol}, timeout=600)
def check_kaifa():
    import subprocess, sqlite3, os
    db = f"{VOL}/seeds/kaifa-gemma-q4.db"
    out = {"db_exists": os.path.exists(db), "db_size_mb": round(os.path.getsize(db) / 1e6, 1) if os.path.exists(db) else None}
    # 1) our own gate (best-effort — may fail if the binary needs ONNX .so at load)
    try:
        r = subprocess.run([f"{VOL}/bin/semfs-fixed", "seed-verify", db],
                           capture_output=True, text=True, timeout=300)
        out["seed_verify"] = {"rc": r.returncode, "out": (r.stdout + r.stderr)[-1200:]}
    except Exception as e:
        out["seed_verify"] = {"error": str(e)[:200]}
    # 2) pure-SQL coverage backup (no binary needed)
    try:
        con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        def q(sql):
            try: return con.execute(sql).fetchone()[0]
            except Exception as e: return f"err:{e}"
        out["fs_layer"] = {
            "fs_inode_total": q("SELECT count(*) FROM fs_inode"),
            "fs_dentry_total": q("SELECT count(*) FROM fs_dentry"),
            "fs_data_rows": q("SELECT count(*) FROM fs_data"),
            "sample_modes": [r[0] for r in (con.execute("SELECT DISTINCT mode FROM fs_inode LIMIT 6").fetchall() or [])],
            "graph_relations": q("SELECT count(*) FROM graph_relation"),
        }
        reg = q("SELECT count(*) FROM fs_inode WHERE (mode & 61440)=32768")
        stubs = q("SELECT count(*) FROM fs_dentry WHERE name LIKE '%.semfs-error.txt'")
        withc = q("SELECT count(DISTINCT filepath) FROM chunks")
        cov = (withc / (reg - stubs) * 100) if isinstance(reg, int) and isinstance(withc, int) and (reg - stubs) > 0 else None
        out["sql"] = {
            "regular_files": reg,
            "error_stubs": stubs,
            "files_with_chunks": withc,
            "total_chunks": q("SELECT count(*) FROM chunks"),
            "unindexed": q("SELECT count(*) FROM fs_unindexed"),
            "graph_entities": q("SELECT count(*) FROM graph_entity"),
            "coverage_pct": round(cov, 1) if cov is not None else None,
        }
    except Exception as e:
        out["sql"] = {"error": str(e)[:200]}
    return out


@app.local_entrypoint()
def main():
    import json
    try:
        print("== fetch_rubrics ==")
        print(json.dumps(fetch_rubrics.remote(), ensure_ascii=False, indent=2))
    except Exception as e:
        print("fetch_rubrics FAILED:", repr(e)[:300])
    print("== check_kaifa ==")
    print(json.dumps(check_kaifa.remote(), ensure_ascii=False, indent=2))
