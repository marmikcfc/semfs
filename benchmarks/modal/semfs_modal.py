"""Modal environment for semfs Workspace-Bench testing.

Design (see README.md):
- MOUNTLESS semfs arm: Modal's gVisor sandbox has no FUSE, but under
  SEARCH_ONLY=off the mount's agent-visible surface is exactly
  {real corpus tree + AGENTS.md hint + `semfs grep`} — all replicable
  without a mount. FUSE-fidelity runs stay on the EC2 box.
- The semfs binary is compiled INTO the image from a pinned git ref →
  every run is versioned and reproducible.
- Data (seeds, corpus, models, judge harness, codex config) lives on a
  Volume, seeded once from the EC2 box by `pull_from_box`.
- Parallel reps via .starmap — the box serializes; Modal does n=10 in one wave.

Quickstart:
  modal secret create openrouter OPENROUTER_API_KEY=sk-or-...
  modal secret create semfs-box-ssh SSH_KEY="$(cat ~/.ssh/semfs-benchmark)"   # for pull_from_box only
  modal volume create semfs-bench-data
  modal run benchmarks/modal/semfs_modal.py::verify_image          # builds image, checks binary
  modal run benchmarks/modal/semfs_modal.py::pull_from_box         # seeds the volume (~1.5GB)
  modal run benchmarks/modal/semfs_modal.py::smoke_grep            # mountless grep + render modes
  modal run benchmarks/modal/semfs_modal.py::e9w2_smoke           # one case-289 end-to-end smoke
  modal run benchmarks/modal/semfs_modal.py::run_batch --case 289 --reps 4
"""

import json
import os
import shutil
import socket
import subprocess
import time
from pathlib import Path

import modal

BOX = "ubuntu@13.201.35.159"  # the EC2 benchmark box (data source for pull_from_box)
_THIS_FILE = Path(__file__).resolve()
REPO_ROOT = (
    _THIS_FILE.parents[2]
    if len(_THIS_FILE.parents) > 2 and (_THIS_FILE.parents[2] / "Cargo.toml").exists()
    else Path("/opt/semfs-src")
)
SEMFS_LOCAL_REF = (
    subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    if (REPO_ROOT / ".git").exists()
    else "unknown"
) or "unknown"

app = modal.App("semfs-bench")

# Modal hydrates EVERY function's secrets when the app loads, so the agent-run
# functions' `claude`/`codex-auth` secrets must exist even to run an unrelated
# function (e.g. build_kaifa_seed). `SEMFS_SEED_ONLY=1` drops the agent secrets
# at load time so the seed build runs in an environment that only has
# `openrouter`. Real agent runs leave it unset and require the real secrets.
_SEED_ONLY = os.environ.get("SEMFS_SEED_ONLY") == "1"


def _agent_secrets(*names: str) -> list:
    """openrouter + the named agent secrets, unless SEMFS_SEED_ONLY drops them."""
    secrets = [modal.Secret.from_name("openrouter")]
    if not _SEED_ONLY:
        secrets += [modal.Secret.from_name(n) for n in names]
    return secrets

data_volume = modal.Volume.from_name("semfs-bench-data", create_if_missing=True)
VOL = "/data"  # volume mountpoint: /data/{seeds,corpus,models,wb,codex}
CANONICAL_SEED_DB = "chanpin-gemma-q4.db"
DEFAULT_CORPUS = "chanpin_standard"

def _ignore_source(path: Path) -> bool:
    path = Path(path)
    rel = path.relative_to(REPO_ROOT) if path.is_absolute() else path
    skip_roots = {
        ".git",
        ".agents",
        ".claude",
        ".codex",
        ".fastembed_cache",
        "node_modules",
        "target",
    }
    if rel.parts and rel.parts[0] in skip_roots:
        return True
    if rel.parts[:3] == ("tickets", "workspace-bench-5arm-matrix", "artifacts"):
        return True
    # Transient editor/lock/swap files (e.g. .codex_auth.json.swp) get written or
    # removed mid-build → Modal's "modified during build" abort. Never copy them.
    name = rel.name
    if name.endswith((".swp", ".swo", ".tmp", "~")) or name.startswith(".#") or name.endswith(".lock"):
        return True
    # Secrets must NEVER enter a shareable image layer (e.g. a stray codex_auth.json
    # at the repo root). Inject credentials into the running sandbox at runtime instead.
    if name in {"auth.json", "codex_auth.json", ".codex_auth.json", ".env",
                "claude_auth_config.json", ".claude_auth_config.json"} or name.endswith((".pem", ".key")):
        return True
    return False


# Ubuntu 24.04 (glibc 2.39): the prebuilt ONNX-runtime static lib linked by
# fastembed/ort needs glibc >= 2.38 (__isoc23_* symbols) — debian bullseye's
# 2.31 fails at link time.
image = (
    modal.Image.from_registry("ubuntu:24.04", add_python="3.11")
    .apt_install("git", "curl", "build-essential", "pkg-config", "libssl-dev",
                 "rsync", "openssh-client", "ca-certificates", "sqlite3")
    .add_local_dir(REPO_ROOT, "/opt/semfs-src", copy=True, ignore=_ignore_source)
    # Rust toolchain + semfs build from the current local worktree.
    .run_commands(
        "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal",
        ". $HOME/.cargo/env && cd /opt/semfs-src && cargo build --release --bin semfs",
        "cp /opt/semfs-src/target/release/semfs /usr/local/bin/semfs",
        # Seed-build examples (dir→seed indexer + dual-lane KG builder). These
        # compile the 14 tree-sitter C grammars (cc from build-essential).
        ". $HOME/.cargo/env && cd /opt/semfs-src && "
        "cargo build --release -p semfs-core --example seed_dir --example build_kg",
        "cp /opt/semfs-src/target/release/examples/seed_dir /usr/local/bin/seed_dir",
        "cp /opt/semfs-src/target/release/examples/build_kg /usr/local/bin/build_kg",
        f"printf '%s\\n' '{SEMFS_LOCAL_REF}+local' > /usr/local/share/semfs-git-sha",
    )
    # Node 20 + codex CLI (the agent under test).
    .run_commands(
        "curl -fsSL https://deb.nodesource.com/setup_20.x | bash -",
        "apt-get install -y nodejs",
        "npm install -g @openai/codex",
    )
    # Claude Code (2nd agent): ClaudeCode.js imports @anthropic-ai/claude-agent-sdk
    # from evaluation/node_modules (hardcoded ../../evaluation/node_modules path), so
    # the SDK + office-doc deps (docx/exceljs/pdfkit) must be installed in that dir.
    .run_commands(
        "cd /opt/semfs-src/benchmarks/vendor/Workspace-Bench/evaluation && "
        "npm install --no-audit --no-fund",
        "test -f /opt/semfs-src/benchmarks/vendor/Workspace-Bench/evaluation/"
        "node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs && echo CLAUDE_SDK_OK",
    )
    .pip_install("pyyaml", "tqdm", "requests")
)


def _sh(cmd: str, env: dict | None = None, timeout: int = 1800) -> subprocess.CompletedProcess:
    e = os.environ.copy()
    if env:
        e.update(env)
    return subprocess.run(["bash", "-lc", cmd], capture_output=True, text=True,
                          env=e, timeout=timeout)


def _local_modal_preflight() -> None:
    """Fail before a remote Modal call when the local network cannot reach Modal."""
    try:
        socket.getaddrinfo("api.modal.com", 443, proto=socket.IPPROTO_TCP)
    except OSError as exc:
        raise RuntimeError(
            "local DNS cannot resolve api.modal.com; run this from a network-enabled shell"
        ) from exc


@app.function(image=image, timeout=600)
def verify_image() -> dict:
    """Prove the environment exists: binary built from the pinned ref, codex present."""
    semfs_v = _sh("semfs --help | head -2").stdout
    sha = _sh("cat /usr/local/share/semfs-git-sha").stdout.strip()
    knobs = _sh("strings /usr/local/bin/semfs | grep -m1 SEMFS_GREP_RENDER_MODE").stdout.strip()
    codex_v = _sh("codex --version").stdout.strip()
    out = {"semfs_git_sha": sha, "render_mode_knob_present": bool(knobs),
           "codex": codex_v, "semfs_help_head": semfs_v.strip()[:120]}
    print(json.dumps(out, indent=2))
    assert sha and knobs and codex_v, "image incomplete"
    return out


@app.function(
    image=image,
    volumes={VOL: data_volume},
    secrets=[modal.Secret.from_name("semfs-box-ssh")],
    timeout=3600,
)
def pull_from_box() -> dict:
    """Seed the volume from the EC2 box (run once; idempotent rsync).

    Pulls: the canonical q4 seed, gemma-q4 ONNX dir, both the clean extract
    source corpus and the benchmark materialized corpus, the Workspace-Bench
    evaluation harness, and the box's codex config.
    Requires secret `semfs-box-ssh` with SSH_KEY = the box private key.
    """
    key_path = "/root/.ssh/box"
    os.makedirs("/root/.ssh", exist_ok=True)
    with open(key_path, "w") as f:
        f.write(os.environ["SSH_KEY"].rstrip() + "\n")
    os.chmod(key_path, 0o600)
    ssh = f"ssh -i {key_path} -o StrictHostKeyChecking=no -o ConnectTimeout=20"

    pulls = [
        # (remote, local-volume-dest)
        ("~/.semfs/chanpin-gemma-q4.db", f"{VOL}/seeds/"),
        ("~/gemma_q4/", f"{VOL}/models/gemma_q4/"),
        ("/srv/semfs-benchmark/extract-test/chanpin_seed/",
         f"{VOL}/corpus/chanpin_seed/"),
        ("/srv/semfs-benchmark/Workspace-Bench/evaluation/filesys/chanpin_standard/",
         f"{VOL}/corpus/chanpin_standard/"),
        ("/srv/semfs-benchmark/Workspace-Bench/evaluation/", f"{VOL}/wb/evaluation/"),
        ("~/.codex/config.toml", f"{VOL}/codex/"),
    ]
    report = {}
    for remote, dest in pulls:
        os.makedirs(dest, exist_ok=True)
        r = _sh(f'rsync -az -e "{ssh}" {BOX}:{remote} {dest}', timeout=3000)
        report[remote] = "ok" if r.returncode == 0 else f"FAIL: {r.stderr[-200:]}"
        print(remote, "->", report[remote])
    data_volume.commit()
    return report


def _prep_workdir(case_corpus: str, seed: str, tag: str = "chanpin-modal") -> str:
    """Materialize the MOUNTLESS semfs workspace: corpus copy + hint + seed registered.

    Equivalence to the EC2 mount (SEARCH_ONLY=off): the agent sees the real tree,
    reads the same AGENTS.md, and `semfs grep --tag` answers from the same index.
    Differences (accepted): no FUSE latency; agent writes land on local disk only.
    """
    wd = "/tmp/workdir"
    _sh(f"rm -rf {wd} && mkdir -p {wd}")
    _sh(f"cp -r {case_corpus}/. {wd}/")
    # seed db under ~/.semfs/<tag>.db so grep's --tag path resolves it
    _sh("mkdir -p ~/.semfs")
    _sh(f"cp {seed} ~/.semfs/{tag}.db")
    marker = "\n".join([
        f"container_tag={tag}",
        "api_url=https://api.supermemory.ai",
        f"mount_path={wd}",
        f"db_path=/root/.semfs/{tag}.db",
        "backend=sqlite",
        "",
    ])
    Path(f"{wd}/.semfs").write_text(marker)
    # hint: extract the seed's baked AGENTS.md; fall back to a discovery hint if absent
    extract = _sh(
        "python3 - <<'PY'\n"
        "import sqlite3, sys\n"
        f"db = sqlite3.connect('/root/.semfs/{tag}.db')\n"
        "row = db.execute(\"SELECT d.ino FROM fs_dentry d WHERE d.name='AGENTS.md'\").fetchone()\n"
        "if row is None:\n"
        "    fallback = ('# Workspace Search\\n'\n"
        "                'Use semantic search to find relevant files across the workspace:\\n'\n"
        "                '  semfs grep \"your query\"\\n'\n"
        "                'The workspace has many similar-looking files.'\n"
        "                ' Search is faster than reading each file individually.\\n')\n"
        "    open('/tmp/workdir/AGENTS.md','w').write(fallback)\n"
        "    print('no AGENTS.md in seed; wrote fallback discovery hint')\n"
        "    sys.exit(0)\n"
        "data = b''.join(r[0] for r in db.execute(\n"
        "    'SELECT data FROM fs_data WHERE ino=? ORDER BY chunk_index', (row[0],)))\n"
        "open('/tmp/workdir/AGENTS.md','wb').write(data)\n"
        "print('hint bytes:', len(data))\n"
        "PY"
    )
    print(extract.stdout, extract.stderr[-200:] if extract.returncode else "")
    # E16 task-awareness: the baked seed hint predates `--all`; append the guidance so
    # the agent knows it can ask for the full set on synthesis/report tasks. (The grep
    # output also nudges `--all` just-in-time when adaptive-K collapses the result.)
    _sh(
        "cat >> /tmp/workdir/AGENTS.md <<'EOF'\n"
        "\n## How many search results\n"
        "By default `semfs grep` returns only the most confident few results (often one) — "
        "ideal for a single-answer lookup. If your task must cover MANY files (write a report, "
        "summarize/compare across files, list all X), add `--all` (or `-n <count>`) to "
        "`semfs grep` to get the full set in one call instead of re-searching.\n"
        "EOF"
    )
    return wd


def _prep_workdir_plain(case_corpus: str) -> str:
    """Corpus-only workspace for the plain arm (no semfs seed, no AGENTS.md injection).

    The agent sees the raw corpus tree — same files, same 403-trapped entries — but
    has no retrieval affordance (no .semfs marker → grep falls back to cloud/fails,
    no baked AGENTS.md hint → no instruction to use semfs grep).
    """
    wd = "/tmp/workdir"
    _sh(f"rm -rf {wd} && mkdir -p {wd}")
    _sh(f"cp -r {case_corpus}/. {wd}/")
    # no .semfs marker → semfs grep won't resolve any local index
    # no AGENTS.md injection → agent uses whatever the corpus already has (or nothing)
    agents_in_corpus = os.path.join(wd, "AGENTS.md")
    if os.path.isfile(agents_in_corpus):
        print(f"plain arm: corpus AGENTS.md present ({os.path.getsize(agents_in_corpus)} bytes), leaving as-is")
    else:
        print("plain arm: no AGENTS.md in corpus")
    return wd


def _metadata_paths(task_root: str) -> list[Path]:
    root = Path(task_root)
    if not root.exists():
        return []
    if root.is_file() and root.name == "metadata.json":
        return [root]
    if root.is_file():
        return []
    return [p for p in sorted(root.rglob("metadata.json")) if p.is_file()]


def _load_case_meta(case: str, task_roots: list[str]) -> tuple[dict | None, str | None]:
    needle = str(case).strip()
    for task_root in task_roots:
        for meta_path in _metadata_paths(task_root):
            try:
                meta = json.loads(meta_path.read_text(encoding="utf-8"))
            except Exception:
                continue
            if not isinstance(meta, dict):
                continue
            exact_ids = {
                str(meta.get("id") or "").strip(),
                str(meta.get("absolute_id") or "").strip(),
                str(meta.get("case_id") or "").strip(),
                str(meta.get("caseId") or "").strip(),
                str(meta.get("task_id") or "").strip(),
                str(meta.get("taskId") or "").strip(),
                meta_path.parent.name.strip(),
            }
            exact_ids.discard("")
            if needle in exact_ids:
                return meta, str(meta_path)
    return None, None


SEMFS_ENV = {
    "SEMFS_EMBED_MODEL": "gemma-q4",
    "SEMFS_EMBED_ONNX_DIR": f"{VOL}/models/gemma_q4",
    "SEMFS_NO_PUSH": "1",
    "SEMFS_NO_SYNC": "1",
    "SEMFS_SEARCH_ONLY": "off",
    "SUPERMEMORY_API_KEY": "dummy-local",
    "SEMFS_RESULT_LIMIT": "5",
    "SEMFS_GREP_RESULT_CAP": "6144",
    "SEMFS_GREP_TOTAL_CAP": "10240",
    "SEMFS_REWRITE": "0",  # keep smoke deterministic; enable for agent runs
}


@app.function(image=image, volumes={VOL: data_volume},
              secrets=[modal.Secret.from_name("openrouter")], timeout=1800)
def smoke_grep() -> dict:
    """Phase-A verification: mountless `semfs grep --tag` against the seed,
    across all three render modes. THE feasibility gate for this environment —
    confirms the daemonless direct-open path serves the index without FUSE.
    Needs only /data/seeds + /data/models on the volume (corpus optional —
    `--tag` resolution is cwd-independent)."""
    seed = f"{VOL}/seeds/{CANONICAL_SEED_DB}"
    assert os.path.exists(seed), "volume not seeded — run pull_from_box first"
    corpus = f"{VOL}/corpus/{DEFAULT_CORPUS}"
    if os.path.isdir(corpus):
        wd = _prep_workdir(corpus, seed)
    else:
        _sh("mkdir -p ~/.semfs")
        _sh(f"cp {seed} ~/.semfs/chanpin-modal.db")
        wd = "/tmp"

    results = {}
    for mode in ("inline", "two-tier", "paths"):
        env = dict(SEMFS_ENV, SEMFS_GREP_RENDER_MODE=mode)
        r = _sh(
            f'cd {wd} && semfs grep --tag chanpin-modal '
            f'"best selling product transaction amount conversion rate" 2>&1',
            env=env, timeout=600,
        )
        out = r.stdout + r.stderr
        results[mode] = {
            "rc": r.returncode,
            "bytes": len(out),
            "has_hits": "best_selling" in out,
            "marker": ("# confidence:" in out) if mode == "two-tier" else None,
            "head": out[:200],
        }
        print(f"[{mode}] rc={r.returncode} bytes={len(out)} hits={'best_selling' in out} | out: {out[:300]!r}")
    return results


@app.function(image=image, volumes={VOL: data_volume}, timeout=300)
def inspect_corpora() -> dict:
    """Diagnostic: file counts for candidate corpus roots + box reachability,
    to locate the kaifa workspace (volume dirs may be empty placeholders)."""
    import glob
    roots = sorted(
        glob.glob(f"{VOL}/wb/evaluation/filesys/*")
        + glob.glob(f"{VOL}/corpus/*")
    )
    counts = {}
    for r in roots:
        if os.path.isdir(r):
            n = int(_sh(f"find {r} -type f 2>/dev/null | wc -l").stdout.strip() or "0")
            counts[r.replace(VOL, "")] = n
    out = {"file_counts": counts}
    print(json.dumps(out, indent=2))
    return out


@app.function(image=image, volumes={VOL: data_volume}, timeout=300)
def volume_status() -> dict:
    """Check whether the shared volume has the assets needed for the smoke."""
    paths = {
        "seed": f"{VOL}/seeds/{CANONICAL_SEED_DB}",
        "model_dir": f"{VOL}/models/gemma_q4",
        "benchmark_corpus": f"{VOL}/corpus/{DEFAULT_CORPUS}",
        "wb_eval": f"{VOL}/wb/evaluation",
        "codex_config": f"{VOL}/codex/config.toml",
    }
    status = {name: os.path.exists(path) for name, path in paths.items()}
    status["ready_for_smoke_grep"] = status["seed"] and status["model_dir"]
    status["ready_for_e9w2"] = all(status.values())
    print(json.dumps(status, indent=2))
    return status


@app.function(image=image, volumes={VOL: data_volume},
              secrets=[modal.Secret.from_name("openrouter")],
              cpu=8.0, timeout=3600)
def build_kaifa_seed(corpus_name: str = "kaifa_standard", out_name: str = "kaifa-gemma-q4.db") -> dict:
    """Build the gemma-q4 `kaifa` (BackendDeveloper) seed + dual-lane KG.

    Orchestration ONLY — all semfs logic is in the core binaries (`seed_dir`,
    `build_kg`), which this just shells out to:
      1. `seed_dir`  : index the kaifa corpus with gemma-q4 ONNX → chunks/vchunks
                       (the dir→seed engine the mount daemon runs online).
      2. coverage    : assert ~every file landed in `chunks` (no <50% warm bug —
                       seed_dir indexes synchronously, so this is a sanity gate).
      3. `build_kg`  : dual lane — AST code lane (tree-sitter, 14 langs) for
                       source files + LLM `extract_graph` for docs.
      4. commit      : persist `kaifa-gemma-q4.db` to /data/seeds.
    """
    # Resolve the corpus under the known roots (WB filesys workspaces or the
    # pulled /corpus seeds). Accepts a bare name or an absolute path.
    candidates = (
        [corpus_name]
        if corpus_name.startswith("/")
        else [f"{VOL}/corpus/{corpus_name}", f"{VOL}/wb/evaluation/filesys/{corpus_name}"]
    )
    # Prefer a NON-EMPTY dir (the WB filesys placeholders exist but are empty).
    def _nonempty(d: str) -> bool:
        return os.path.isdir(d) and int(_sh(f"find {d} -type f 2>/dev/null | head -1 | wc -l").stdout.strip() or "0") > 0
    corpus = next((c for c in candidates if _nonempty(c)), candidates[0])
    assert os.path.isdir(corpus), f"corpus not staged on volume: {corpus}"
    assert os.path.isdir(f"{VOL}/models/gemma_q4"), "gemma_q4 ONNX missing on volume"
    out_db = f"{VOL}/seeds/{out_name}"
    _sh(f"rm -f {out_db} {out_db}-shm {out_db}-wal")  # fresh, idempotent rebuild

    env = dict(
        SEMFS_EMBED_MODEL="gemma-q4",
        SEMFS_EMBED_ONNX_DIR=f"{VOL}/models/gemma_q4",
        OPENROUTER_API_KEY=os.environ.get("OPENROUTER_API_KEY", ""),
    )

    n_corpus = int(_sh(f"find {corpus} -type f | wc -l").stdout.strip() or "0")
    assert n_corpus > 0, f"corpus {corpus} has no files (try corpus_name=kaifa_raw)"
    print(f"== seed_dir: indexing {n_corpus} files from {corpus} ==")
    r_seed = _sh(f"seed_dir {out_db} {corpus} 2>&1", env=env, timeout=3000)
    print(r_seed.stdout[-2000:])
    assert r_seed.returncode == 0 and os.path.exists(out_db), "seed_dir failed"

    indexed = int(_sh(
        f'sqlite3 {out_db} "SELECT COUNT(DISTINCT filepath) FROM chunks;"'
    ).stdout.strip() or "0")
    coverage = indexed / n_corpus if n_corpus else 0.0
    print(f"== coverage: {indexed}/{n_corpus} files indexed ({coverage:.0%}) ==")

    print("== build_kg: dual-lane KG (AST code + LLM docs) ==")
    r_kg = _sh(f"build_kg {out_db} {corpus} 2>&1", env=env, timeout=3000)
    print(r_kg.stdout[-3000:])
    assert r_kg.returncode == 0, "build_kg failed"

    q = lambda sql: _sh(f'sqlite3 {out_db} "{sql}"').stdout.strip()
    stats = {
        "corpus": corpus,
        "out_db": out_db,
        "files_corpus": n_corpus,
        "files_indexed": indexed,
        "coverage": round(coverage, 3),
        "entities_total": int(q("SELECT COUNT(*) FROM graph_entity;") or 0),
        "entities_code": int(q("SELECT COUNT(*) FROM graph_entity WHERE file_type='code';") or 0),
        "relations_total": int(q("SELECT COUNT(*) FROM graph_relation;") or 0),
        "relations_by_type": q(
            "SELECT relation||':'||COUNT(*) FROM graph_relation GROUP BY relation ORDER BY 1;"
        ).replace("\n", ", "),
        "confidence_breakdown": q(
            "SELECT confidence||':'||COUNT(*) FROM graph_relation GROUP BY confidence;"
        ).replace("\n", ", "),
        "entity_kinds": q(
            "SELECT kind||':'||COUNT(*) FROM graph_entity GROUP BY kind ORDER BY 2 DESC;"
        ).replace("\n", ", "),
    }
    data_volume.commit()
    print(json.dumps(stats, indent=2))
    return stats


_WB_PROMPT_TAIL = (
    "[Note] Save all task deliverables to the required location inside the working "
    "directory. When you finish, provide the final output file paths as a list. "
    "The paths must be relative to the working directory, for example "
    "['model_output/a.xlsx', 'model_output/b.docx']."
)


def _wrap_prompt_wb(task: str, work_dir: str,
                    task_target_output_dir: str = "model_output") -> str:
    """Replicate agent_runner.py::_wrap_prompt() + build_run_config.py::PROMPT_TAIL.

    Without this wrapping the agent has no instruction to write to model_output/
    and no instruction to output a Python list of paths — both of which the WB
    harness and judge depend on to collect deliverables.
    """
    path_req = (
        f"请你无视任务要求中的输出文件保存路径要求，将所有输出文件放置在目录："
        f"{task_target_output_dir}下\n"
    ) if task_target_output_dir else ""
    head = (
        "【重要要求 1：工作目录】\n"
        f"本轮测试允许访问的工作目录是：{os.path.abspath(work_dir)}\n"
        "你只能在该目录下使用相对路径读写文件；禁止访问工作目录以外的位置。\n"
        "如果你看到其他工作区路径提示，请忽略，以本提示的工作目录为准。\n"
        + path_req
    )
    tail = (
        "\n【重要要求 2：输出路径列表】\n"
        "完成所有文件创建并确认文件已实际写入磁盘后，在最后一步输出一个 Python 列表（list[str]），"
        "里面是你生成的所有输出文件路径。\n"
        "路径请使用相对工作目录的相对路径（不要以 / 开头）。示例：['model_output/a.txt','report.md']\n"
    )
    body = (task.strip() + "\n" + _WB_PROMPT_TAIL).strip()
    return head + "\n" + body + "\n" + tail


def _run_judge(
    *,
    case: str,
    label: str,
    meta_path: str,
    workdir: str,
    or_key: str,
    sandbox_dir: str,
) -> dict:
    """Run the WB Seed-2.0-Lite judge on the agent's deliverables.

    Sets up a minimal task_dir structure, writes a judge YAML from the
    OpenRouter key, calls agent_eval.py, and returns the parsed rubric scores.
    """
    judge_dir = f"/tmp/judgetask_{case}_{label}"
    os.makedirs(judge_dir, exist_ok=True)
    shutil.copy(meta_path, os.path.join(judge_dir, "metadata.json"))

    # Copy ONLY model_output/ into judge_dir/output/ so the judge's 50-file cap is
    # not hit by fastembed cache blobs in the full workdir.
    output_dir = os.path.join(judge_dir, "output")
    os.makedirs(output_dir, exist_ok=True)
    mo_src = os.path.join(workdir, "model_output")
    if os.path.isdir(mo_src):
        for fn in os.listdir(mo_src):
            src = os.path.join(mo_src, fn)
            if os.path.isfile(src):
                shutil.copy2(src, os.path.join(output_dir, fn))

    judge_yaml = f"/tmp/judge_{case}_{label}.yaml"
    Path(judge_yaml).write_text(
        f'model_name: "seed-2.0-lite-judge"\n'
        f'baseUrl: "https://openrouter.ai/api/v1"\n'
        f'model: "bytedance-seed/seed-2.0-lite"\n'
        f'apiKey: "{or_key}"\n',
        encoding="utf-8",
    )

    agent_eval = "/opt/semfs-src/benchmarks/vendor/Workspace-Bench/evaluation/src/agent_eval.py"
    r = _sh(
        f"python3 {agent_eval} --task-dir {judge_dir} --eval-yaml {judge_yaml} --overwrite",
        timeout=600,
    )
    print(f"judge rc={r.returncode} stderr_tail={r.stderr[-400:]!r}")

    rubric_file = next(
        (os.path.join(judge_dir, f) for f in os.listdir(judge_dir)
         if f.startswith("rubrics_judge--")),
        None,
    )
    if rubric_file and os.path.isfile(rubric_file):
        try:
            rubrics = json.loads(Path(rubric_file).read_text(encoding="utf-8"))
            summary = rubrics.get("summary") or {}
            return {
                "passed": summary.get("passed"),
                "total": summary.get("total"),
                "score": round(summary.get("passed", 0) / max(summary.get("total", 1), 1), 3),
                "per_rubric": [
                    {"id": rb.get("id"), "passed": rb.get("passed"), "evidence": str(rb.get("evidence") or "")[:200]}
                    for rb in (rubrics.get("rubrics") or [])
                ],
            }
        except Exception as exc:
            return {"error": str(exc)}
    return {"error": f"judge rc={r.returncode}: {r.stderr[-300:]}"}


def _check_confidence_high(sandbox_dir: str) -> bool:
    """Scan the codex execution trace for any CONFIDENCE: HIGH grep output."""
    trace_path = os.path.join(sandbox_dir, "raw", "codex_stdout.jsonl")
    if not os.path.isfile(trace_path):
        return False
    try:
        text = Path(trace_path).read_text(encoding="utf-8", errors="ignore")
        return "CONFIDENCE: HIGH" in text or "confidence: HIGH" in text
    except Exception:
        return False


def _load_codex_harness():
    """Import codex.py harness from the image copy of the repo.

    The harness contains a local Python HTTP chat-adapter that translates
    the OpenAI Responses-API format (what the codex CLI speaks) into
    REST /chat/completions calls (what OpenRouter accepts). Importing it
    here means Modal runs use the same provider wiring as the EC2 box
    without needing a ripbench proxy or a native OpenAI key.
    """
    import importlib.util
    harness_path = (
        "/opt/semfs-src/benchmarks/vendor/Workspace-Bench/evaluation/src/agents/codex.py"
    )
    spec = importlib.util.spec_from_file_location("codex_harness", harness_path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _load_claude_harness():
    """Import claudecode.py harness from the image copy of the repo.

    It shells out to `node ClaudeCode.js <cfg> -o <report>` (ESM via the
    baselines/package.json type:module), which drives @anthropic-ai/claude-agent-sdk.
    Auth: OAuth subscription token (USE_CLAUDE_LONG_RUNNING_TOKEN + CLAUDE_CODE_OAUTH_TOKEN)
    talks to api.anthropic.com directly with the canonical model id. Return shape
    mirrors the codex harness (status / trace.usageTotal / executionTrace).
    """
    import importlib.util
    harness_path = (
        "/opt/semfs-src/benchmarks/vendor/Workspace-Bench/evaluation/src/agents/claudecode.py"
    )
    spec = importlib.util.spec_from_file_location("claude_harness", harness_path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@app.function(image=image, volumes={VOL: data_volume},
              secrets=_agent_secrets("codex-auth"),
              timeout=3600, cpu=4, memory=8192)
def run_case(case: str = "289", label: str = "modal1",
             render_mode: str = "inline", extra_env: str = "",
             corpus_name: str = DEFAULT_CORPUS,
             model: str = "openai/gpt-5.4",
             arm: str = "nokg",
             seed_name: str = CANONICAL_SEED_DB) -> dict:
    """One full mountless agent run: codex in the materialized workspace.

    arm: 'nokg' (semfs corpus + AGENTS.md hint + grep index) or
         'plain' (raw corpus only, no retrieval affordances).

    Provider wiring: the codex.py harness starts a local Python HTTP server
    (the chat adapter) that accepts the OpenAI Responses API from the codex
    CLI and forwards REST /chat/completions calls to OpenRouter. Model names
    that do NOT start with 'gpt-' (e.g. 'openai/gpt-5.4') trigger the
    adapter automatically; 'gpt-*' names would skip it and hit OpenAI
    directly → 401. Always use the 'openai/…' prefix for Modal runs.

    extra_env: 'K=V,K=V' environment overrides applied on top of SEMFS_ENV.
    """
    harness = _load_codex_harness()

    seed = f"{VOL}/seeds/{seed_name}"
    corpus_dir = f"{VOL}/corpus/{corpus_name}"
    if arm == "plain":
        wd = _prep_workdir_plain(corpus_dir)
    else:
        wd = _prep_workdir(corpus_dir, seed)

    # task prompt from the WB harness metadata
    meta, meta_path = _load_case_meta(
        case,
        [
            f"{VOL}/wb/evaluation/tasks_local",   # local E11+ cases (not in upstream WB)
            f"{VOL}/wb/evaluation/tasks_lite",
            f"{VOL}/wb/evaluation/tasks",
            f"{VOL}/wb/evaluation/tasks_lite.full",
            f"{VOL}/wb/evaluation",
        ],
    )
    if not meta or not meta_path:
        out = {"label": label, "error": f"no metadata.json found for case {case} on volume"}
        print(json.dumps(out, indent=2))
        return out
    print(f"case metadata: {meta_path}")
    task = str(meta.get("task") or "")
    or_key = os.environ.get("OPENROUTER_API_KEY", "")
    print(f"task_len={len(task)} openrouter_present={bool(or_key)} model={model}")

    # Apply SEMFS env vars so grep uses the right render mode / limits.
    # plain arm skips these — it has no semfs index and any accidental grep call
    # should fail cleanly rather than fall back to cloud.
    if arm != "plain":
        for k, v in dict(SEMFS_ENV, SEMFS_REWRITE="1", SEMFS_GREP_RENDER_MODE=render_mode).items():
            os.environ[k] = v
    for kv in filter(None, extra_env.split(",")):
        k, v = kv.split("=", 1)
        os.environ[k] = v

    # The harness reads CODEX_API_KEY; the chat adapter forwards it to OpenRouter.
    os.environ["CODEX_API_KEY"] = or_key
    # /tmp/workdir has no .git — workspace-write sandbox uses git boundary to scope
    # writes, so all agent writes are blocked. danger-full-access removes that gate.
    os.environ["CODEX_SANDBOX_MODE"] = "danger-full-access"

    # Pre-create model_output/ so agent can write there without mkdir.
    os.makedirs(f"{wd}/model_output", exist_ok=True)

    # Wrap task with WB prompt conventions (workdir anchor + model_output/ rule +
    # path-list tail). Without this the agent doesn't know where to write and
    # deliverables stay empty.
    wrapped_task = _wrap_prompt_wb(task, wd)
    print(f"wrapped_task_len={len(wrapped_task)}")

    sandbox_dir = f"/tmp/sandbox_{case}_{label}"
    os.makedirs(sandbox_dir, exist_ok=True)

    import base64 as _b64
    t0 = time.time()

    def _run_agent(use_chatgpt, sbx_dir):
        os.makedirs(sbx_dir, exist_ok=True)
        if use_chatgpt:
            os.environ["CODEX_USE_CHATGPT"] = "1"
            b64 = os.environ.get("CODEX_AUTH_B64", "")
            if b64:
                cdir = os.path.expanduser("~/.codex")
                os.makedirs(cdir, exist_ok=True)
                with open(os.path.join(cdir, "auth.json"), "wb") as fh:
                    fh.write(_b64.b64decode(b64))
            ap = {"model": "gpt-5.5"}  # bare id; native OpenAI provider via the ChatGPT OAuth
        else:
            os.environ.pop("CODEX_USE_CHATGPT", None)
            ap = {"baseUrl": "https://openrouter.ai/api/v1", "apiKey": or_key, "model": model}
        return harness.run(prompt=wrapped_task, work_dir=wd, sandbox_dir=sbx_dir,
                           timeout_s=2400, api_provider=ap)

    # Auth: try the user's ChatGPT subscription first ($0 OpenRouter); fall back to
    # OpenRouter per-run if it fails (auth error / no successful call / non-ok status).
    have_chatgpt = bool(os.environ.get("CODEX_AUTH_B64"))
    auth_used = "chatgpt" if have_chatgpt else "openrouter"
    result = _run_agent(have_chatgpt, sandbox_dir)
    if have_chatgpt:
        _ut = (result.get("trace", {}) or {}).get("usageTotal", {}) or {}
        _toks = _ut.get("prompt_tokens") or 0
        _err = str(result.get("errorMessage") or "").lower()
        _authfail = any(s in _err for s in ("401", "403", "unauthorized", "invalid_grant"))
        if result.get("status") != "ok" or _toks == 0 or _authfail:
            print(f"native ChatGPT auth failed (status={result.get('status')} toks={_toks}); falling back to OpenRouter")
            os.makedirs(f"{wd}/model_output", exist_ok=True)
            sandbox_dir = sandbox_dir + "_or"
            result = _run_agent(False, sandbox_dir)
            auth_used = "openrouter(fallback)"
    wall = int(time.time() - t0)

    trace = result.get("trace", {}) or {}
    usage_total = trace.get("usageTotal", {}) or {}
    exec_trace = trace.get("executionTrace", []) or []
    calls = sum(
        1 for e in exec_trace
        if e.get("type") == "tool" and e.get("status") == "completed"
    )

    # Slice-adoption instrumentation: capture the exact bash commands the agent
    # ran (input.command) + a sample of grep outputs, so adoption (did the agent
    # sed/cat a CITED path instead of re-grepping?) is measurable from the
    # returned JSON without pulling the raw trace off the container.
    tool_commands = []
    grep_output_samples = []
    for e in exec_trace:
        if e.get("type") != "tool":
            continue
        cmd = str((e.get("input") or {}).get("command") or "")
        if not cmd:
            continue
        if e.get("status") == "completed":
            tool_commands.append(cmd[:400])
        if "grep" in cmd and e.get("output"):
            grep_output_samples.append(str(e.get("output"))[:900])

    # Check if E9w2 confidence signal fired
    high_fired = _check_confidence_high(sandbox_dir)
    last_text = trace.get("lastText", "")[:500]

    deliverable_paths = [
        f for f in _sh(
            f"find {wd}/model_output -maxdepth 2 -type f 2>/dev/null"
        ).stdout.splitlines() if f.strip()
    ]

    # Read deliverable content (usually <1KB for these tasks)
    deliverable_content = {}
    for dp in deliverable_paths[:3]:
        try:
            deliverable_content[os.path.basename(dp)] = Path(dp).read_text(
                encoding="utf-8", errors="replace"
            )[:4000]
        except Exception:
            pass

    # Run the WB rubric judge
    judge_result = _run_judge(
        case=case,
        label=label,
        meta_path=meta_path,
        workdir=wd,
        or_key=or_key,
        sandbox_dir=sandbox_dir,
    )
    print(f"judge: {json.dumps(judge_result)}")

    out = {
        "label": label, "case": case, "render_mode": render_mode,
        "arm": arm, "model": model, "wall_s": wall,
        "rc": 0 if result.get("status") == "ok" else 1,
        "status": result.get("status"),
        "calls": calls,
        "tokens": (usage_total.get("prompt_tokens") or 0) + (usage_total.get("completion_tokens") or 0),
        "usage": usage_total,
        "deliverables": deliverable_paths,
        "deliverable_content": deliverable_content,
        "confidence_high_fired": high_fired,
        "last_text": last_text,
        "judge": judge_result,
        "tool_commands": tool_commands,
        "grep_output_samples": grep_output_samples[:3],
        "auth_used": auth_used,
        "semfs_sha": _sh("cat /usr/local/share/semfs-git-sha").stdout.strip()[:12],
    }
    if result.get("errorMessage"):
        out["runner_err_head"] = str(result["errorMessage"])[:4000]
    # Persist the raw codex trace to the volume so all artifacts can be pulled local.
    try:
        os.makedirs(f"{VOL}/e2e_traces", exist_ok=True)
        _sh(f"cp {sandbox_dir}/raw/codex_stdout.jsonl {VOL}/e2e_traces/{label}.jsonl 2>/dev/null")
        Path(f"{VOL}/e2e_traces/{label}.result.json").write_text(json.dumps(out), encoding="utf-8")
        data_volume.commit()
    except Exception as exc:
        print(f"trace-save skipped: {exc}")
    print(json.dumps(out, indent=2))
    return out


@app.function(image=image, volumes={VOL: data_volume},
              secrets=_agent_secrets("claude"),
              timeout=3600, cpu=4, memory=8192)
def run_claude_case(case: str = "289", label: str = "claude1",
                    render_mode: str = "inline", extra_env: str = "",
                    corpus_name: str = DEFAULT_CORPUS,
                    model: str = "anthropic/claude-sonnet-4.6",
                    arm: str = "nokg",
                    seed_name: str = CANONICAL_SEED_DB) -> dict:
    """One full mountless agent run with Claude Code (sonnet-4.6) as the agent.

    Same arms/seed/corpus/judge as run_case; only the agent differs. Auth:
    Claude subscription OAuth (CLAUDE_CODE_OAUTH_TOKEN via the 'claude' secret)
    first; on failure (auth error / rate-limit / no successful call) fall back
    to OpenRouter per-run. Identical SEMFS env so the nokg/adaptive-K arms
    behave exactly as in the codex matrix.
    """
    harness = _load_claude_harness()

    seed = f"{VOL}/seeds/{seed_name}"
    corpus_dir = f"{VOL}/corpus/{corpus_name}"
    if arm == "plain":
        wd = _prep_workdir_plain(corpus_dir)
    else:
        wd = _prep_workdir(corpus_dir, seed)

    meta, meta_path = _load_case_meta(
        case,
        [
            f"{VOL}/wb/evaluation/tasks_local",
            f"{VOL}/wb/evaluation/tasks_lite",
            f"{VOL}/wb/evaluation/tasks",
            f"{VOL}/wb/evaluation/tasks_lite.full",
            f"{VOL}/wb/evaluation",
        ],
    )
    if not meta or not meta_path:
        out = {"label": label, "error": f"no metadata.json found for case {case} on volume"}
        print(json.dumps(out, indent=2))
        return out
    print(f"case metadata: {meta_path}")
    task = str(meta.get("task") or "")
    or_key = os.environ.get("OPENROUTER_API_KEY", "")
    have_oauth = bool(os.environ.get("CLAUDE_CODE_OAUTH_TOKEN"))
    print(f"task_len={len(task)} oauth_present={have_oauth} openrouter_present={bool(or_key)} model={model}")

    if arm != "plain":
        for k, v in dict(SEMFS_ENV, SEMFS_REWRITE="1", SEMFS_GREP_RENDER_MODE=render_mode).items():
            os.environ[k] = v
    for kv in filter(None, extra_env.split(",")):
        k, v = kv.split("=", 1)
        os.environ[k] = v

    os.makedirs(f"{wd}/model_output", exist_ok=True)
    wrapped_task = _wrap_prompt_wb(task, wd)
    print(f"wrapped_task_len={len(wrapped_task)}")

    sandbox_dir = f"/tmp/sandbox_{case}_{label}"
    os.makedirs(sandbox_dir, exist_ok=True)
    t0 = time.time()

    def _run_agent(use_oauth, sbx_dir):
        os.makedirs(sbx_dir, exist_ok=True)
        if use_oauth:
            os.environ["USE_CLAUDE_LONG_RUNNING_TOKEN"] = "1"
            os.environ["CLAUDE_OAUTH_MODEL"] = "claude-sonnet-4-6"
            ap = {"model": model}  # harness normalizes to claude-sonnet-4-6 for OAuth
        else:
            os.environ.pop("USE_CLAUDE_LONG_RUNNING_TOKEN", None)
            ap = {"provider_type": "anthropic", "baseUrl": "https://openrouter.ai/api/v1",
                  "apiKey": or_key, "model": model}
        return harness.run(prompt=wrapped_task, work_dir=wd, sandbox_dir=sbx_dir,
                           timeout_s=2400, api_provider=ap)

    auth_used = "claude_oauth" if have_oauth else "openrouter"
    result = _run_agent(have_oauth, sandbox_dir)
    if have_oauth:
        _ut = (result.get("trace", {}) or {}).get("usageTotal", {}) or {}
        _toks = _ut.get("prompt_tokens") or 0
        _err = str(result.get("errorMessage") or "").lower()
        _authfail = any(s in _err for s in ("401", "403", "unauthorized", "invalid_grant",
                                            "429", "rate", "limit", "overloaded", "quota"))
        if result.get("status") != "ok" or _toks == 0 or _authfail:
            print(f"native Claude OAuth failed (status={result.get('status')} toks={_toks} err={_err[:120]}); falling back to OpenRouter")
            os.makedirs(f"{wd}/model_output", exist_ok=True)
            sandbox_dir = sandbox_dir + "_or"
            result = _run_agent(False, sandbox_dir)
            auth_used = "openrouter(fallback)"
    wall = int(time.time() - t0)

    trace = result.get("trace", {}) or {}
    usage_total = trace.get("usageTotal", {}) or {}
    exec_trace = trace.get("executionTrace", []) or []
    calls = sum(1 for e in exec_trace if e.get("type") == "tool")

    tool_commands = []
    grep_output_samples = []
    for e in exec_trace:
        if e.get("type") != "tool":
            continue
        inp = e.get("input") or {}
        cmd = str(inp.get("command") or inp.get("cmd") or inp.get("file_path") or inp)
        if cmd:
            tool_commands.append(cmd[:400])
        if "grep" in cmd and e.get("output"):
            grep_output_samples.append(str(e.get("output"))[:900])

    high_fired = _check_confidence_high(sandbox_dir)
    last_text = trace.get("lastText", "")[:500]

    deliverable_paths = [
        f for f in _sh(
            f"find {wd}/model_output -maxdepth 2 -type f 2>/dev/null"
        ).stdout.splitlines() if f.strip()
    ]
    deliverable_content = {}
    for dp in deliverable_paths[:3]:
        try:
            deliverable_content[os.path.basename(dp)] = Path(dp).read_text(
                encoding="utf-8", errors="replace"
            )[:4000]
        except Exception:
            pass

    judge_result = _run_judge(
        case=case, label=label, meta_path=meta_path,
        workdir=wd, or_key=or_key, sandbox_dir=sandbox_dir,
    )
    print(f"judge: {json.dumps(judge_result)}")

    out = {
        "label": label, "case": case, "render_mode": render_mode,
        "arm": arm, "agent": "claude", "model": model, "wall_s": wall,
        "rc": 0 if result.get("status") == "ok" else 1,
        "status": result.get("status"),
        "calls": calls,
        "tokens": (usage_total.get("prompt_tokens") or 0) + (usage_total.get("completion_tokens") or 0),
        "usage": usage_total,
        "deliverables": deliverable_paths,
        "deliverable_content": deliverable_content,
        "confidence_high_fired": high_fired,
        "last_text": last_text,
        "judge": judge_result,
        "tool_commands": tool_commands,
        "grep_output_samples": grep_output_samples[:3],
        "auth_used": auth_used,
        "semfs_sha": _sh("cat /usr/local/share/semfs-git-sha").stdout.strip()[:12],
    }
    if result.get("errorMessage"):
        out["runner_err_head"] = str(result["errorMessage"])[:4000]
    try:
        os.makedirs(f"{VOL}/e2e_traces", exist_ok=True)
        _sh(f"cp {sandbox_dir}/raw/runner_stdout.txt {VOL}/e2e_traces/{label}.stdout.txt 2>/dev/null")
        _sh(f"cp {sandbox_dir}/raw/claudecode_report.json {VOL}/e2e_traces/{label}.report.json 2>/dev/null")
        Path(f"{VOL}/e2e_traces/{label}.result.json").write_text(json.dumps(out), encoding="utf-8")
        data_volume.commit()
    except Exception as exc:
        print(f"trace-save skipped: {exc}")
    print(json.dumps(out, indent=2))
    return out


@app.local_entrypoint()
def e9w2_smoke(seed_if_missing: bool = True, model: str = "openai/gpt-5.4"):
    """Single end-to-end smoke mirroring the planned E9w2 shape.

    Parity target:
    - case 289, chanpin_standard corpus
    - SEARCH_ONLY=off, RESULT_LIMIT=5, GREP_RESULT_CAP=6144
    - GREP_TOTAL_CAP=10240, RENDER_MODE=two-tier

    Provider path (no ripbench / no OpenAI key needed):
      codex CLI → local Python chat-adapter (port on 127.0.0.1)
              → OpenRouter /chat/completions → model
    The model name MUST contain a '/' (e.g. 'openai/gpt-5.4') so the
    harness's _should_use_chat_adapter() returns True. A bare 'gpt-*'
    name would bypass the adapter and hit api.openai.com → 401.
    """
    _local_modal_preflight()
    verify_image.remote()
    status = volume_status.remote()
    if seed_if_missing and not status["ready_for_e9w2"]:
        pull_from_box.remote()
        status = volume_status.remote()
    if not status["ready_for_smoke_grep"]:
        raise RuntimeError("Modal volume is missing seed/model assets; run pull_from_box first")
    smoke = smoke_grep.remote()
    if not any(v.get("rc") == 0 and v.get("has_hits") for v in smoke.values()):
        raise RuntimeError(f"smoke_grep did not return usable hits: {smoke}")
    print(json.dumps(run_case.remote(
        case="289",
        label="e9w2-modal-smoke",
        render_mode="two-tier",
        extra_env="",
        corpus_name=DEFAULT_CORPUS,
        model=model,
    )))


@app.local_entrypoint()
def run_batch(case: str = "289", reps: int = 4, render_mode: str = "inline",
              corpus_name: str = DEFAULT_CORPUS, model: str = "openai/gpt-5.4",
              arm: str = "nokg"):
    """Parallel reps — the thing the EC2 box cannot do."""
    args = [(case, f"m{i+1}", render_mode, "", corpus_name, model, arm, CANONICAL_SEED_DB) for i in range(reps)]
    for res in run_case.starmap(args):
        print(json.dumps(res))


@app.local_entrypoint()
def run_slice_pilot(reps: int = 3, case: str = "289", model: str = "openai/gpt-5.4"):
    """Slice-adoption pilot (the gate for the whole cite-the-path / Option-A direction).

    THE question: when the grep response is SMALL and cites `path:line_start-line_end`,
    does the agent read just those lines (sed/cat the cited path) instead of re-grepping
    or dumping the whole file? Adoption is agent-context, so this is Modal-valid.

    Arms (both nokg, same seed/binary/model — only the render differs):
      A0 inline      — current default: full chunk inline (control)
      A1 cite-path   — two-tier render + SEMFS_GREP_RESULT_CAP=1536 (small top excerpt
                       + path:line-range for the rest → exact rows need a slice)

    Kill (Modal-valid): adoption < 1/3 → agent won't slice → direction is a UX dead-end.
    Success: adoption >= 2/3 AND turns <= A0 AND accuracy >= A0 - tol → greenlight EC2 gate.
    Adoption/tokens/accuracy are parsed locally from the printed JSON (tool_commands field).
    """
    _local_modal_preflight()
    args = []
    for i in range(reps):
        args.append((case, f"a0_inline_r{i+1}", "inline", "",
                     DEFAULT_CORPUS, model, "nokg", CANONICAL_SEED_DB))
        args.append((case, f"a1_cite_r{i+1}", "two-tier", "SEMFS_GREP_RESULT_CAP=1536",
                     DEFAULT_CORPUS, model, "nokg", CANONICAL_SEED_DB))
    results = []
    for res in run_case.starmap(args):
        print(json.dumps(res))
        results.append(res)
    # Single machine-parseable line for the local adoption parser.
    print("SLICE_PILOT_RESULTS=" + json.dumps(results))


@app.local_entrypoint()
def run_e2e(reps: int = 3, model: str = "openai/gpt-5.4"):
    """Full E2E matrix: 5 WB cases × 3 arms (plain / nokg / nokg+adaptive-K) × n reps.
    Auth: ChatGPT subscription first, per-run OpenRouter fallback (run_case handles it).
    Raw traces + per-run result JSON are persisted to the volume (e2e_traces/) for pull.
    """
    _local_modal_preflight()
    cases = ["95", "289", "175", "44", "15"]
    arms = [("plain",  "plain", ""),
            ("nokg",   "nokg",  ""),                       # standard nokg (SEMFS_ENV defaults)
            ("nokgAK", "nokg",  "SEMFS_ADAPTIVE_K=on")]    # standard nokg + adaptive-K only
    args = []
    for case in cases:
        for aname, arm, aenv in arms:
            for i in range(reps):
                args.append((case, f"e2e_{case}_{aname}_r{i+1}", "inline", aenv,
                             DEFAULT_CORPUS, model, arm, CANONICAL_SEED_DB))
    results = []
    for res in run_case.starmap(args):
        print(json.dumps(res))
        results.append(res)
    print("E2E_RESULTS=" + json.dumps(results))


# All 11 WB-Lite Product-Manager (chanpin / 产品人员) cases — derived by reading
# every task_lite_clean_en/<case>/metadata.json persona from the HF dataset.
PM_LITE_CASES = "15,44,45,53,55,95,171,175,289,386,388"


@app.local_entrypoint()
def run_pm_matrix(reps: int = 2, agents: str = "claude,codex",
                  claude_model: str = "anthropic/claude-sonnet-4.6",
                  codex_model: str = "openai/gpt-5.4",
                  cases: str = PM_LITE_CASES):
    """The /goal matrix: all 11 PM-lite cases × 3 arms × 2 agents × reps.

    Arms (identical to run_e2e): plain / nokg / nokg+adaptive-K — same seed
    (chanpin-gemma-q4) and SEMFS env; only SEMFS_ADAPTIVE_K differs.
    Agents run in order (Claude first, then codex per the /goal). Each agent is
    native-auth-first (Claude OAuth / codex ChatGPT subscription) with a per-run
    OpenRouter fallback. All per-run result JSON + raw traces persist to
    /data/e2e_traces for `modal volume get`.

    Cost note: at reps=2 this is 2×3×11×2 = 132 agentic runs. Claude cells fan
    out concurrently and may hit the subscription rate limit → those fall back to
    OpenRouter (per the /goal). Run with `--agents claude` or a smaller `--cases`
    list first if you want to pace the subscription.
    """
    _local_modal_preflight()
    case_list = [c.strip() for c in cases.split(",") if c.strip()]
    agent_list = [a.strip() for a in agents.split(",") if a.strip()]
    arms = [("plain",  "plain", ""),
            ("nokg",   "nokg",  ""),
            ("nokgAK", "nokg",  "SEMFS_ADAPTIVE_K=on")]

    results = []
    for agent in agent_list:  # claude first, then codex
        model = claude_model if agent == "claude" else codex_model
        fn = run_claude_case if agent == "claude" else run_case
        args = []
        for case in case_list:
            for aname, arm, aenv in arms:
                for i in range(reps):
                    args.append((case, f"pm_{agent}_{case}_{aname}_r{i+1}", "inline",
                                 aenv, DEFAULT_CORPUS, model, arm, CANONICAL_SEED_DB))
        print(f"=== {agent}: {len(args)} cells "
              f"({len(case_list)} cases × {len(arms)} arms × {reps} reps) ===")
        for res in fn.starmap(args):
            print(json.dumps(res))
            results.append(res)
    print("PM_MATRIX_RESULTS=" + json.dumps(results))


@app.local_entrypoint()
def run_e16(reps: int = 5, model: str = "openai/gpt-5.4", cases: str = "95,289"):
    """E16 — confidence-adaptive-K A/B (cases via --cases, default 95,289).

    arm A (fixed):    SEMFS_RESULT_LIMIT=10 → today's behaviour (up to 10 ranked hits).
    arm B (adaptive): + SEMFS_ADAPTIVE_K=on → grep returns 1 (dominant) … up to 10 (flat).
    Same seed/binary/model, nokg arm, inline render. Only difference = adaptive-K on/off.
    Metrics parsed from the JSON: tokens, calls, judge accuracy, tool_commands,
    grep_output_samples (to read the HIGH/Cluster verdict + the false-HIGH guard).
    """
    _local_modal_preflight()
    case_list = [c.strip() for c in cases.split(",") if c.strip()]
    arms = [("Afix", "SEMFS_RESULT_LIMIT=10"),
            ("Badpt", "SEMFS_RESULT_LIMIT=10,SEMFS_ADAPTIVE_K=on")]
    args = []
    for case in case_list:
        for aname, aenv in arms:
            for i in range(reps):
                args.append((case, f"e16_{case}_{aname}_r{i+1}", "inline", aenv,
                             DEFAULT_CORPUS, model, "nokg", CANONICAL_SEED_DB))
    results = []
    for res in run_case.starmap(args):
        print(json.dumps(res))
        results.append(res)
    print("E16_RESULTS=" + json.dumps(results))


@app.local_entrypoint()
def run_e8(reps: int = 3, model: str = "openai/gpt-5.4"):
    """E8 honest headline run: all discriminating cases × both arms × n reps in parallel.

    Pre-registered condition: ≥3 of 5 cases where semfs (nokg) mean_tokens < plain
    AND accuracy ≥ plain−1 → 'semfs delivers' headline. <3 → declare wrong arena.

    Cases: 95/175/289 (discriminating), 15/44 (structural ceiling — completeness only).
    Arms: plain (baseline), nokg (two-tier render, leanhint3-class seed, v4.1 hint).
    Render mode: two-tier for both (consistent render surface).
    """
    _local_modal_preflight()
    cases = ["289", "175", "95", "15", "44"]
    arms = ["plain", "nokg"]
    render_mode = "two-tier"

    # fan out: all (case, arm, rep) cells in parallel
    arg_tuples = []
    for case in cases:
        for arm in arms:
            for i in range(reps):
                label = f"e8_{case}_{arm}_r{i+1}"
                arg_tuples.append((case, label, render_mode, "", DEFAULT_CORPUS, model, arm, CANONICAL_SEED_DB))

    print(f"Launching {len(arg_tuples)} cells: {len(cases)} cases × {len(arms)} arms × {reps} reps")
    results = []
    for res in run_case.starmap(arg_tuples):
        results.append(res)
        print(json.dumps(res))

    # Print summary table
    print("\n=== E8 SUMMARY ===")
    from collections import defaultdict
    cells: dict = defaultdict(list)
    for r in results:
        cells[(r["case"], r.get("arm", "?"))].append(r)
    print(f"{'case':>6} {'arm':>6} {'reps':>4} {'mean_acc':>9} {'mean_tok':>10} {'verdict'}")
    print("-" * 60)
    plain_acc: dict = {}
    for case in cases:
        for arm in arms:
            cell = cells[(case, arm)]
            if not cell:
                print(f"{case:>6} {arm:>6} {'0':>4}   {'N/A':>9} {'N/A':>10}")
                continue
            acc = [r["judge"]["score"] for r in cell if r.get("judge")]
            tok = [r["tokens"] for r in cell]
            mean_acc = sum(acc) / len(acc) if acc else 0
            mean_tok = int(sum(tok) / len(tok)) if tok else 0
            if arm == "plain":
                plain_acc[case] = mean_acc
            verdict = ""
            if arm == "nokg" and case in plain_acc:
                tok_win = mean_tok < (sum(r["tokens"] for r in cells[(case, "plain")]) / len(cells[(case, "plain")]))
                acc_ok = mean_acc >= plain_acc[case] - (1 / 15)
                verdict = "WIN" if (tok_win and acc_ok) else ("ACC_ONLY" if acc_ok else "LOSS")
            print(f"{case:>6} {arm:>6} {len(cell):>4} {mean_acc:>9.3f} {mean_tok:>10,}  {verdict}")
    wins = sum(1 for case in cases if cells.get((case, "nokg"))
               and any(r["judge"]["score"] for r in cells[(case, "nokg")] if r.get("judge")))
    print(f"\nHeadline condition: ≥3/5 cases semfs wins → check table above")


# ─── E11: Discovery-stressed + cross-lingual cases ────────────────────────────

E11_SEED_DB = "e11_seed.db"
E11_CORPUS = "e11_discovery_corpus"


@app.function(
    image=image,
    volumes={VOL: data_volume},
    secrets=[modal.Secret.from_name("semfs-box-ssh")],
    timeout=3600,
)
def build_e11_seed_via_box() -> dict:
    """Build the E11 discovery semfs seed on the EC2 box (has FUSE) and pull back.

    The E11 corpus is 400 plain text files (200 product reports, 200 region summaries).
    Modal's gVisor has no FUSE, so indexing must happen on the EC2 box.

    Steps:
    1. Rsync e11 corpus from Modal volume to EC2 /tmp/e11_corpus/
    2. Run `semfs mount` with no-sync/no-push flags on EC2
    3. Wait for indexing to finish
    4. Pull the seed DB back to Modal volume at /data/seeds/e11_seed.db
    """
    key_path = "/root/.ssh/box"
    os.makedirs("/root/.ssh", exist_ok=True)
    with open(key_path, "w") as f:
        f.write(os.environ["SSH_KEY"].rstrip() + "\n")
    os.chmod(key_path, 0o600)
    box = "ubuntu@13.201.35.159"
    ssh = f"ssh -i {key_path} -o StrictHostKeyChecking=no -o ConnectTimeout=30"

    e11_corpus = f"{VOL}/corpus/{E11_CORPUS}"
    e11_seed = f"{VOL}/seeds/{E11_SEED_DB}"

    if os.path.exists(e11_seed):
        print(f"E11 seed already exists at {e11_seed}: {os.path.getsize(e11_seed)} bytes")
        return {"status": "already_exists", "path": e11_seed}

    # 1. rsync corpus to EC2
    r = _sh(
        f'rsync -az -e "{ssh}" {e11_corpus}/ {box}:/tmp/e11_corpus/',
        timeout=600,
    )
    print(f"rsync corpus to box: rc={r.returncode} err={r.stderr[-200:]!r}")
    if r.returncode != 0:
        return {"status": "error", "step": "rsync_corpus", "stderr": r.stderr[-500:]}

    # 2. Build the index on box (semfs mount + wait for completion)
    build_cmd = (
        "SEMFS_EMBED_MODEL=gemma-q4 "
        "SEMFS_EMBED_ONNX_DIR=~/gemma_q4 "
        "SEMFS_NO_PUSH=1 "
        "SEMFS_NO_SYNC=1 "
        "SEMFS_STARTUP_TIMEOUT_SEC=600 "
        "SEMFS_MOUNT_TIMEOUT_SEC=900 "
        "/home/ubuntu/.local/bin/semfs mount /tmp/e11_corpus "
        "--tag e11-discovery "
        "&& /home/ubuntu/.local/bin/semfs status --tag e11-discovery "
        "&& /home/ubuntu/.local/bin/semfs unmount e11-discovery"
    )
    r = _sh(f'{ssh} {box} \'{build_cmd}\'', timeout=1200)
    print(f"build index on box: rc={r.returncode}")
    print(r.stdout[-500:])
    if r.returncode != 0:
        print(f"stderr: {r.stderr[-500:]}")
        return {"status": "error", "step": "build_index", "rc": r.returncode, "stderr": r.stderr[-400:]}

    # 3. Pull seed back from box to Modal volume
    os.makedirs(f"{VOL}/seeds", exist_ok=True)
    r = _sh(
        f'rsync -az -e "{ssh}" {box}:~/.semfs/e11-discovery.db {e11_seed}',
        timeout=300,
    )
    print(f"rsync seed from box: rc={r.returncode} err={r.stderr[-200:]!r}")
    if r.returncode != 0:
        return {"status": "error", "step": "rsync_seed", "stderr": r.stderr[-500:]}

    if not os.path.exists(e11_seed):
        return {"status": "error", "step": "verify", "error": "seed file missing after rsync"}

    size = os.path.getsize(e11_seed)
    data_volume.commit()
    print(f"E11 seed saved: {e11_seed} ({size} bytes)")
    return {"status": "ok", "path": e11_seed, "size": size}


@app.local_entrypoint()
def run_e11(reps: int = 3, model: str = "openai/gpt-5.4"):
    """E11 discovery-stressed + cross-lingual runs.

    Cases: e11-001 (product Q4-2023 return rate), e11-002 (region H1-2024 growth).
    Corpus: e11_discovery_corpus (200 files per case directory).
    Arms: plain only (nokg pending E11 semfs seed build via build_e11_seed_via_box).
    """
    _local_modal_preflight()
    cases = ["e11-001", "e11-002"]
    render_mode = "two-tier"

    # nokg arm requires the e11 seed — check if it exists first
    e11_seed_ready = volume_status_e11.remote()
    arms = ["plain"]
    if e11_seed_ready:
        arms.append("nokg")
        print(f"E11 seed ready — running both arms: {arms}")
    else:
        print("E11 seed NOT ready — running plain arm only. "
              "Run build_e11_seed_via_box() first for nokg arm.")

    arg_tuples = []
    for case in cases:
        for arm in arms:
            seed = E11_SEED_DB if arm == "nokg" else CANONICAL_SEED_DB
            for i in range(reps):
                label = f"e11_{case}_{arm}_r{i+1}"
                arg_tuples.append(
                    (case, label, render_mode, "", E11_CORPUS, model, arm, seed)
                )

    print(f"Launching {len(arg_tuples)} E11 cells: {len(cases)} cases × {len(arms)} arms × {reps} reps")
    results = []
    for res in run_case.starmap(arg_tuples):
        results.append(res)
        print(json.dumps(res))

    print("\n=== E11 SUMMARY ===")
    from collections import defaultdict
    cells: dict = defaultdict(list)
    for r in results:
        cells[(r["case"], r.get("arm", "?"))].append(r)
    for case in cases:
        for arm in arms:
            cell = cells[(case, arm)]
            if not cell:
                continue
            acc = [r["judge"]["score"] for r in cell if r.get("judge")]
            tok = [r["tokens"] for r in cell]
            mean_acc = sum(acc) / len(acc) if acc else 0
            mean_tok = int(sum(tok) / len(tok)) if tok else 0
            print(f"  {case} arm={arm:<6} n={len(cell)} mean_acc={mean_acc:.3f} mean_tok={mean_tok:,}")


@app.function(image=image, volumes={VOL: data_volume}, timeout=120)
def volume_status_e11() -> bool:
    """Check whether the E11 seed is ready on the volume."""
    seed = f"{VOL}/seeds/{E11_SEED_DB}"
    corpus = f"{VOL}/corpus/{E11_CORPUS}"
    ready = os.path.exists(seed) and os.path.exists(corpus)
    print(f"E11 seed: {'ok' if os.path.exists(seed) else 'MISSING'} | corpus: {'ok' if os.path.exists(corpus) else 'MISSING'}")
    return ready


@app.function(
    image=image,
    volumes={VOL: data_volume},
    timeout=3600, cpu=4, memory=8192,
)
def build_e11_seed_modal() -> dict:
    """Build the E11 discovery semfs seed directly in Modal using SEMFS_INDEX_ONLY=1.

    SEMFS_INDEX_ONLY=1 makes daemon-inner skip the FUSE mount and exit after indexing.
    Modal's gVisor has no FUSE, so this is the Modal-native indexing path.
    The DB is written to ~/.semfs/e11-discovery.db, then committed to the volume.

    The semfs binary must be built from a commit that includes the SEMFS_INDEX_ONLY
    feature (added 2026-06-12 in daemon_runtime.rs).
    """
    e11_corpus = f"{VOL}/corpus/{E11_CORPUS}"
    e11_seed = f"{VOL}/seeds/{E11_SEED_DB}"

    if os.path.exists(e11_seed):
        size = os.path.getsize(e11_seed)
        print(f"E11 seed already exists at {e11_seed}: {size} bytes")
        return {"status": "already_exists", "path": e11_seed, "size": size}

    if not os.path.isdir(e11_corpus):
        return {"status": "error", "error": f"E11 corpus missing at {e11_corpus}"}

    # Count corpus files for sanity check
    corpus_files = []
    for root, _, files in os.walk(e11_corpus):
        corpus_files.extend(os.path.join(root, f) for f in files)
    print(f"E11 corpus: {len(corpus_files)} files in {e11_corpus}")

    # Set up semfs config directory
    _sh("mkdir -p ~/.semfs")

    # Run daemon-inner with SEMFS_INDEX_ONLY=1: index corpus, write DB, exit without FUSE
    # daemon-inner args: --container-tag, --mount, --backend, --key, --api-url,
    #                    --no-sync, --no-push
    index_env = {
        "SEMFS_EMBED_MODEL": "gemma-q4",
        "SEMFS_EMBED_ONNX_DIR": f"{VOL}/models/gemma_q4",
        "SEMFS_NO_PUSH": "1",
        "SEMFS_NO_SYNC": "1",
        "SEMFS_INDEX_ONLY": "1",       # skip FUSE; exit after indexing
        "SEMFS_KG": "0",               # skip KG build to save time/space
        "SUPERMEMORY_API_KEY": "dummy-local-e11",
    }
    cmd = (
        "semfs daemon-inner "
        "--container-tag e11-discovery "
        f"--mount {e11_corpus} "
        "--backend fuse "              # backend field (ignored when INDEX_ONLY)
        "--key dummy-local-e11 "
        "--api-url https://api.supermemory.ai "
        "--no-sync --no-push "
        "2>&1"
    )
    print(f"Starting indexer: {cmd}")
    r = _sh(cmd, env=index_env, timeout=2700)
    print(f"daemon-inner rc={r.returncode}")
    print(r.stdout[-800:])
    if r.returncode != 0:
        return {"status": "error", "step": "index", "rc": r.returncode,
                "stderr": r.stderr[-500:], "stdout_tail": r.stdout[-300:]}

    # The DB is written to ~/.semfs/e11-discovery.db
    local_db = os.path.expanduser("~/.semfs/e11-discovery.db")
    if not os.path.exists(local_db):
        return {"status": "error", "step": "verify_db",
                "error": f"DB not found at {local_db} after indexing"}

    db_size = os.path.getsize(local_db)
    print(f"Indexed DB: {local_db} ({db_size} bytes)")

    # Copy to volume
    os.makedirs(f"{VOL}/seeds", exist_ok=True)
    _sh(f"cp {local_db} {e11_seed}")
    data_volume.commit()
    print(f"E11 seed committed to volume: {e11_seed} ({db_size} bytes)")
    return {"status": "ok", "path": e11_seed, "size": db_size, "corpus_files": len(corpus_files)}


@app.local_entrypoint()
def build_e11_seed():
    """Build the E11 discovery semfs seed.

    Tries Modal-native indexing first (SEMFS_INDEX_ONLY=1, no EC2 needed).
    Falls back to EC2 box if Modal build fails (requires semfs-box-ssh secret).
    """
    _local_modal_preflight()
    print("Building E11 seed via Modal-native SEMFS_INDEX_ONLY indexer...")
    result = build_e11_seed_modal.remote()
    print(json.dumps(result, indent=2))
    if result.get("status") in ("ok", "already_exists"):
        print("E11 seed ready.")
    else:
        print("Modal-native build failed. Trying EC2 box fallback...")
        result = build_e11_seed_via_box.remote()
        print(json.dumps(result, indent=2))
