"""Download HF corpora to the Modal volume (corpus layout only — no indexing).

Pipeline:
  phase0_explore  — inspect WB-Workspaces HF repo + current Modal volume state
  phase1_xafs     — snapshot_download xAFS → lay out /data/corpus/xafs/ + tasks.json
  phase2_wb       — extract WB persona from ZIP bundle → /data/corpus/{persona}_standard/

After each download completes, run the indexer from semfs_modal.py:
  SEMFS_SEED_ONLY=1 modal run benchmarks/modal/semfs_modal.py::index_corpus \\
    --corpus-name <name> --out-name <name>-gemma-q4.db

xAFS repo layout: dp_001/question.json + dp_001/data/**  (file-based HF repo, NOT Parquet)
WB-Workspaces: single ZIP — selected_workdirs_bundle_v2.zip — cached after first download
"""
import modal, json, os

app = modal.App("semfs-xafs-wb-embed")
vol = modal.Volume.from_name("semfs-bench-data")
VOL = "/data"

img = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("libssl3", "libgomp1", "ca-certificates", "curl")
    .pip_install("huggingface_hub>=0.23", "hf_transfer>=0.1.8")
)


def _count_files(path):
    import subprocess
    out = subprocess.run(f"find '{path}' -type f 2>/dev/null | wc -l", shell=True,
                         capture_output=True, text=True).stdout.strip()
    return int(out or "0")


# ── Phase 0: explore ──────────────────────────────────────────────────────────

@app.function(image=img, volumes={VOL: vol}, timeout=300)
def explore() -> dict:
    """List WB-Workspaces HF repo files + current Modal volume state."""
    from huggingface_hub import list_repo_files, HfApi
    import sqlite3

    # 1. WB Workspaces repo structure
    print("=== Workspace-Bench/Workspace-Bench-Workspaces (first 80 paths) ===", flush=True)
    try:
        wb_ws_files = list(list_repo_files(
            "Workspace-Bench/Workspace-Bench-Workspaces", repo_type="dataset"
        ))
        print(json.dumps(wb_ws_files[:80], indent=2))
    except Exception as e:
        wb_ws_files = [f"ERROR: {e}"]
        print(f"ERROR: {e}")

    # 2. xAFS repo structure (sanity check — we know it's dp_00X/)
    print("\n=== supermemory/xAFS (first 40 paths) ===", flush=True)
    try:
        xafs_files = list(list_repo_files("supermemory/xAFS", repo_type="dataset"))
        print(json.dumps(xafs_files[:40], indent=2))
    except Exception as e:
        xafs_files = [f"ERROR: {e}"]
        print(f"ERROR: {e}")

    # 3. Modal volume state
    def dir_fc(p):
        return int(os.popen(f"find '{p}' -type f 2>/dev/null | wc -l").read().strip() or "0")

    seeds = {}
    seeds_root = f"{VOL}/seeds"
    if os.path.isdir(seeds_root):
        for f in sorted(os.listdir(seeds_root)):
            if f.endswith(".db"):
                p = os.path.join(seeds_root, f)
                try:
                    conn = sqlite3.connect(p)
                    c = conn.execute("SELECT COUNT(DISTINCT filepath) FROM chunks").fetchone()[0]
                    conn.close()
                except Exception as e:
                    c = f"err:{e}"
                seeds[f] = {"size_mb": round(os.path.getsize(p)/1e6,1), "indexed_files": c}

    corpus_dirs = {}
    corpus_root = f"{VOL}/corpus"
    if os.path.isdir(corpus_root):
        for d in sorted(os.listdir(corpus_root)):
            corpus_dirs[d] = dir_fc(os.path.join(corpus_root, d))

    wb_filesys = {}
    for d in sorted(os.listdir(f"{VOL}/wb/evaluation/filesys") if os.path.isdir(f"{VOL}/wb/evaluation/filesys") else []):
        wb_filesys[d] = dir_fc(f"{VOL}/wb/evaluation/filesys/{d}")

    result = {
        "wb_workspaces_paths_sample": wb_ws_files[:80],
        "xafs_paths_sample": xafs_files[:40],
        "seeds_on_volume": seeds,
        "corpus_dirs_on_volume": corpus_dirs,
        "wb_filesys_persona_dirs": wb_filesys,
    }
    print("\n=== SUMMARY ===")
    print(json.dumps(result, indent=2, ensure_ascii=False))
    return result


# ── Phase 1: xAFS ─────────────────────────────────────────────────────────────

@app.function(
    image=img,
    volumes={VOL: vol},
    timeout=7200,
    cpu=2,
)
def download_xafs() -> dict:
    """Download supermemory/xAFS → lay out /data/corpus/xafs/ + tasks.json.

    Does NOT index — run semfs_modal.py::index_corpus after this completes.

    xAFS repo layout (file-based HF repo, NOT Parquet):
      dp_001/question.json   ← task metadata
      dp_001/data/**         ← workspace files (the corpus to index)
      dp_002/ … dp_013/
    """
    from huggingface_hub import snapshot_download
    import pathlib, shutil

    HF_CACHE   = f"{VOL}/_hf_xafs_cache"
    CORPUS_ROOT = f"{VOL}/corpus/xafs"

    # Enable rust-based fast downloader + longer timeout
    os.environ["HF_HUB_ENABLE_HF_TRANSFER"] = "1"
    os.environ["HF_HUB_HTTP_TIMEOUT"] = "600"

    print("[xAFS] snapshot_download supermemory/xAFS (with hf_transfer) ...", flush=True)
    local_dir = None
    for attempt in range(1, 4):
        try:
            local_dir = snapshot_download(
                repo_id="supermemory/xAFS",
                repo_type="dataset",
                local_dir=HF_CACHE,
                ignore_patterns=["*.git", ".gitattributes", "README.md"],
            )
            print(f"  Done (attempt {attempt}): {local_dir}", flush=True)
            break
        except Exception as e:
            print(f"  Attempt {attempt} failed: {e}", flush=True)
            if attempt == 3:
                raise
            import time as _t; _t.sleep(15)

    # Commit so cache survives crashes; next run skips re-download
    vol.commit()
    print("  [HF cache committed to volume]", flush=True)

    # Lay out corpus: dp_XXX/data/ → /data/corpus/xafs/dp_XXX/
    top = sorted([p for p in pathlib.Path(local_dir).iterdir()
                  if p.is_dir() and p.name.startswith("dp_")])
    print(f"  Case dirs found: {[d.name for d in top]}", flush=True)

    os.makedirs(CORPUS_ROOT, exist_ok=True)
    tasks_meta = []
    total_files = 0

    for case_dir in top:
        case_id = case_dir.name
        q_file = case_dir / "question.json"
        q_data = {}
        if q_file.exists():
            try:
                with open(q_file) as f:
                    raw = json.load(f)
                # some cases have [{"question":...}] instead of {"question":...}
                if isinstance(raw, list):
                    q_data = raw[0] if raw and isinstance(raw[0], dict) else {}
                    if len(raw) > 1:
                        print(f"  WARNING: {case_id} question.json is list of {len(raw)} items, using first", flush=True)
                else:
                    q_data = raw if isinstance(raw, dict) else {}
            except Exception as e:
                q_data = {"error": str(e)}
        else:
            print(f"  WARNING: no question.json in {case_id}", flush=True)

        dest = pathlib.Path(CORPUS_ROOT) / case_id
        data_src = case_dir / "data"
        src = data_src if data_src.is_dir() else case_dir
        if dest.exists():
            shutil.rmtree(str(dest))
        shutil.copytree(str(src), str(dest),
                        ignore=shutil.ignore_patterns("question.json") if src == case_dir else None)
        n = _count_files(str(dest))
        total_files += n
        print(f"  {case_id}: {n} files", flush=True)

        tasks_meta.append({
            "case_id": case_id,
            "question": q_data.get("question") or q_data.get("query") or "",
            "answer":   q_data.get("answer") or q_data.get("expected_output") or "",
            "n_files":  n,
        })

    tasks_json = os.path.join(CORPUS_ROOT, "tasks.json")
    with open(tasks_json, "w") as f:
        json.dump(tasks_meta, f, indent=2, ensure_ascii=False)

    vol.commit()
    print(f"\n[xAFS] Corpus committed: {len(top)} cases, {total_files} files", flush=True)
    print(f"  tasks.json → {tasks_json}", flush=True)
    print(f"  Next: SEMFS_SEED_ONLY=1 modal run benchmarks/modal/semfs_modal.py::index_corpus --corpus-name xafs --out-name xafs-gemma-q4.db", flush=True)

    return {
        "corpus_root": CORPUS_ROOT,
        "xafs_cases": len(top),
        "total_files": total_files,
        "tasks_meta_sample": tasks_meta[:3],
    }


# ── Phase 2: WB persona ────────────────────────────────────────────────────────

# Mapping from WB benchmark persona names → ZIP subdir names.
# ZIP dirs use English role names; WB benchmark uses Chinese persona codes.
WB_PERSONA_TO_ZIP = {
    "chanpin":  "ProductManager_Workdir",
    "kaifa":    "BackendDeveloper_Workdir",
    "houqin":   "LogisticsManager_Workdir",
    "yunying":  "OperationsManager_Workdir",
    "research": "Research_Workdir",
}


@app.function(
    image=img,
    volumes={VOL: vol},
    secrets=[modal.Secret.from_name("openrouter")],
    timeout=14400,
    cpu=4,
)
def build_wb_persona_seed(persona_name: str, zip_subdir: str = "") -> dict:
    """Download WB Workspaces bundle from HF, extract persona dir, build seed.

    The WB-Workspaces HF repo ships as a single ZIP (18.7 GB):
      selected_workdirs_bundle_v2.zip
    Inside: BackendDeveloper_Workdir  LogisticsManager_Workdir
            OperationsManager_Workdir ProductManager_Workdir  Research_Workdir

    persona_name: WB persona code ("houqin", "yunying", "research", "chanpin", "kaifa")
    zip_subdir:   override ZIP dir name (auto-resolved from WB_PERSONA_TO_ZIP if blank)
    """
    from huggingface_hub import hf_hub_download
    import pathlib, shutil, zipfile

    ZIP_CACHE   = f"{VOL}/_hf_wb_workspaces_zip"
    EXTRACT_DIR = f"{VOL}/_hf_wb_workspaces_extracted"
    CORPUS_ROOT = f"{VOL}/corpus/{persona_name}_standard"
    SEED_PATH   = f"{VOL}/seeds/{persona_name}-gemma-q4.db"
    ZIP_NAME    = "selected_workdirs_bundle_v2.zip"

    # Resolve ZIP subdir
    if not zip_subdir:
        zip_subdir = WB_PERSONA_TO_ZIP.get(persona_name, "")
    if not zip_subdir:
        raise ValueError(
            f"Unknown persona '{persona_name}'. Known: {list(WB_PERSONA_TO_ZIP.keys())}. "
            f"Or pass --zip-subdir explicitly."
        )
    print(f"[WB:{persona_name}] ZIP subdir: {zip_subdir}", flush=True)

    # 1. Download the ZIP (cache-friendly: only re-downloads if changed)
    zip_path = os.path.join(ZIP_CACHE, ZIP_NAME)
    if not os.path.exists(zip_path):
        print(f"[WB:{persona_name}] Downloading {ZIP_NAME} from HF ...", flush=True)
        os.makedirs(ZIP_CACHE, exist_ok=True)
        downloaded = hf_hub_download(
            repo_id="Workspace-Bench/Workspace-Bench-Workspaces",
            filename=ZIP_NAME,
            repo_type="dataset",
            local_dir=ZIP_CACHE,
        )
        print(f"  Downloaded to: {downloaded}", flush=True)
        zip_path = downloaded
        # Commit after download so the cache survives crashes
        vol.commit()
        print("  [ZIP committed to volume]", flush=True)
    else:
        print(f"[WB:{persona_name}] Using cached ZIP at {zip_path}", flush=True)

    zip_size_mb = round(os.path.getsize(zip_path) / 1e6, 1)
    print(f"  ZIP size: {zip_size_mb} MB", flush=True)

    # 2. Inspect ZIP top-level names (fast — no full extract)
    with zipfile.ZipFile(zip_path) as zf:
        all_names = zf.namelist()
    top_level = sorted({n.split("/")[0] for n in all_names if n and not n.startswith(".")})
    print(f"  ZIP top-level dirs/files: {top_level[:30]}", flush=True)

    persona_dir_in_zip = zip_subdir
    if persona_dir_in_zip not in top_level:
        raise RuntimeError(
            f"'{persona_dir_in_zip}' not in ZIP top-level: {top_level}"
        )
    print(f"  Matched persona dir in ZIP: {persona_dir_in_zip}", flush=True)

    # 3. Extract only the matched persona subdir
    extract_base = pathlib.Path(EXTRACT_DIR)
    persona_extract = extract_base / persona_dir_in_zip
    if persona_extract.exists():
        shutil.rmtree(str(persona_extract))
    os.makedirs(str(extract_base), exist_ok=True)

    prefix = persona_dir_in_zip + "/"
    n_extracted = 0
    with zipfile.ZipFile(zip_path) as zf:
        members = [m for m in zf.infolist() if m.filename.startswith(prefix) and not m.filename.endswith("/")]
        print(f"  Extracting {len(members)} files for {persona_dir_in_zip} ...", flush=True)
        for m in members:
            zf.extract(m, str(extract_base))
            n_extracted += 1
            if n_extracted % 500 == 0:
                print(f"  ... extracted {n_extracted}/{len(members)}", flush=True)
    print(f"  Extracted {n_extracted} files", flush=True)

    # 4. Copy to corpus dir (does NOT index — use semfs_modal.py::index_corpus next)
    if os.path.exists(CORPUS_ROOT):
        shutil.rmtree(CORPUS_ROOT)
    shutil.copytree(str(persona_extract), CORPUS_ROOT)
    total_files = _count_files(CORPUS_ROOT)
    print(f"  Corpus at {CORPUS_ROOT}: {total_files} files", flush=True)

    if total_files == 0:
        raise RuntimeError(f"Empty corpus after extraction: {CORPUS_ROOT}")

    vol.commit()
    print(f"[WB:{persona_name}] Corpus committed to volume", flush=True)
    print(f"  Next: SEMFS_SEED_ONLY=1 modal run benchmarks/modal/semfs_modal.py::index_corpus "
          f"--corpus-name {persona_name}_standard --out-name {persona_name}-gemma-q4.db", flush=True)

    return {
        "corpus_root": CORPUS_ROOT,
        "persona_dir_in_zip": persona_dir_in_zip,
        "zip_size_mb": zip_size_mb,
        "total_files": total_files,
    }


# ── Local entrypoints ─────────────────────────────────────────────────────────

@app.local_entrypoint()
def phase0_explore():
    """Explore WB-Workspaces HF structure + current Modal volume state."""
    print("\n=== EXPLORING ===")
    result = explore.remote()
    print("\n=== FINAL RESULT ===")
    print(json.dumps(result, indent=2, ensure_ascii=False))


@app.local_entrypoint()
def phase1_xafs():
    """Download xAFS from HF → lay out /data/corpus/xafs/ (no indexing).

    After this completes, run:
      SEMFS_SEED_ONLY=1 modal run benchmarks/modal/semfs_modal.py::index_corpus \\
        --corpus-name xafs --out-name xafs-gemma-q4.db
    """
    print("\n=== DOWNLOAD xAFS CORPUS ===")
    result = download_xafs.remote()
    print("\n=== xAFS DOWNLOAD RESULT ===")
    print(json.dumps(result, indent=2, ensure_ascii=False))


@app.local_entrypoint()
def phase2_wb(persona_name: str = "houqin"):
    """Download WB persona workspace from ZIP bundle → /data/corpus/{persona}_standard/ (no indexing).

    ZIP cached after first run. Defaults to houqin.
    After this completes, run:
      SEMFS_SEED_ONLY=1 modal run benchmarks/modal/semfs_modal.py::index_corpus \\
        --corpus-name houqin_standard --out-name houqin-gemma-q4.db
    """
    print(f"\n=== DOWNLOAD WB PERSONA CORPUS: {persona_name} ===")
    result = build_wb_persona_seed.remote(persona_name)
    print("\n=== WB PERSONA RESULT ===")
    print(json.dumps(result, indent=2, ensure_ascii=False))
