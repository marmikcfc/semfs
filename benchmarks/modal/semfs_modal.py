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
  modal run benchmarks/modal/semfs_modal.py::run_batch --case 289 --reps 4
"""

import json
import os
import subprocess
import time

import modal

# Pin the exact code under test. Bump deliberately; the image rebuilds on change.
SEMFS_GIT_URL = "https://github.com/marmikcfc/semfs.git"
SEMFS_GIT_REF = "feat/backend-agnostic-store"

BOX = "ubuntu@13.201.35.159"  # the EC2 benchmark box (data source for pull_from_box)

app = modal.App("semfs-bench")

data_volume = modal.Volume.from_name("semfs-bench-data", create_if_missing=True)
VOL = "/data"  # volume mountpoint: /data/{seeds,corpus,models,wb,codex}

# Ubuntu 24.04 (glibc 2.39): the prebuilt ONNX-runtime static lib linked by
# fastembed/ort needs glibc >= 2.38 (__isoc23_* symbols) — debian bullseye's
# 2.31 fails at link time.
image = (
    modal.Image.from_registry("ubuntu:24.04", add_python="3.11")
    .apt_install("git", "curl", "build-essential", "pkg-config", "libssl-dev",
                 "rsync", "openssh-client", "ca-certificates", "sqlite3")
    # Rust toolchain + semfs build from the pinned ref. Cached until ref/code changes.
    .run_commands(
        "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal",
        f"git clone --depth 1 --branch {SEMFS_GIT_REF} {SEMFS_GIT_URL} /opt/semfs-src",
        ". $HOME/.cargo/env && cd /opt/semfs-src && cargo build --release --bin semfs",
        "cp /opt/semfs-src/target/release/semfs /usr/local/bin/semfs",
        "cd /opt/semfs-src && git rev-parse HEAD > /usr/local/share/semfs-git-sha",
    )
    # Node 20 + codex CLI (the agent under test).
    .run_commands(
        "curl -fsSL https://deb.nodesource.com/setup_20.x | bash -",
        "apt-get install -y nodejs",
        "npm install -g @openai/codex",
    )
    .pip_install("pyyaml", "tqdm", "requests")
)


def _sh(cmd: str, env: dict | None = None, timeout: int = 1800) -> subprocess.CompletedProcess:
    e = os.environ.copy()
    if env:
        e.update(env)
    return subprocess.run(["bash", "-lc", cmd], capture_output=True, text=True,
                          env=e, timeout=timeout)


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

    Pulls: seeds (clean + leanhint3), gemma-q4 ONNX dir, the chanpin corpus,
    the Workspace-Bench evaluation harness, and the box's codex config.
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
        ("~/.semfs/chanpin-clean.db", f"{VOL}/seeds/"),
        ("~/.semfs/chanpin-leanhint3.db", f"{VOL}/seeds/"),
        ("~/gemma_q4/", f"{VOL}/models/gemma_q4/"),
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


def _prep_workdir(case_corpus: str, seed: str, hint_name: str = "AGENTS.md") -> str:
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
    _sh(f"cp {seed} ~/.semfs/chanpin-modal.db")
    # hint: extract the seed's baked AGENTS.md (authoritative for the arm under test)
    extract = _sh(
        "python3 - <<'PY'\n"
        "import sqlite3\n"
        "db = sqlite3.connect('/root/.semfs/chanpin-modal.db')\n"
        "row = db.execute(\"SELECT d.ino FROM fs_dentry d WHERE d.name='AGENTS.md'\").fetchone()\n"
        "data = b''.join(r[0] for r in db.execute(\n"
        "    'SELECT data FROM fs_data WHERE ino=? ORDER BY chunk_index', (row[0],)))\n"
        "open('/tmp/workdir/AGENTS.md','wb').write(data)\n"
        "print('hint bytes:', len(data))\n"
        "PY"
    )
    print(extract.stdout, extract.stderr[-200:] if extract.returncode else "")
    return wd


SEMFS_ENV = {
    "SEMFS_EMBED_MODEL": "gemma-q4",
    "SEMFS_EMBED_ONNX_DIR": f"{VOL}/models/gemma_q4",
    "SEMFS_NO_PUSH": "1",
    "SEMFS_NO_SYNC": "1",
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
    confirms the daemonless direct-open path serves the index without FUSE."""
    seed = f"{VOL}/seeds/chanpin-leanhint3.db"
    assert os.path.exists(seed), "volume not seeded — run pull_from_box first"
    wd = _prep_workdir(f"{VOL}/corpus/chanpin_standard", seed)

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
        print(f"[{mode}] rc={r.returncode} bytes={len(out)} hits={'best_selling' in out}")
    return results


@app.function(image=image, volumes={VOL: data_volume},
              secrets=[modal.Secret.from_name("openrouter")],
              timeout=3600, cpu=4, memory=8192)
def run_case(case: str = "289", label: str = "modal1",
             render_mode: str = "inline", extra_env: str = "") -> dict:
    """One full mountless agent run: codex in the materialized workspace, then
    judge via the WB harness. extra_env: 'K=V,K=V' overrides."""
    seed = f"{VOL}/seeds/chanpin-leanhint3.db"
    wd = _prep_workdir(f"{VOL}/corpus/chanpin_standard", seed)
    _sh(f"mkdir -p /root/.codex && cp {VOL}/codex/config.toml /root/.codex/ 2>/dev/null || true")

    # task prompt from the WB harness metadata
    meta_glob = _sh(
        f"python3 -c \"import glob,json,sys;"
        f"fs=glob.glob('{VOL}/wb/evaluation/**/tasks*full*/**/*{case}*/metadata.json',recursive=True) or "
        f"glob.glob('{VOL}/wb/evaluation/**/*{case}*/metadata.json',recursive=True);"
        f"print(fs[0] if fs else '')\""
    ).stdout.strip()
    if not meta_glob:
        return {"label": label, "error": f"no metadata.json found for case {case} on volume"}
    task = json.loads(open(meta_glob).read())["task"]

    env = dict(SEMFS_ENV, SEMFS_REWRITE="1", SEMFS_GREP_RENDER_MODE=render_mode,
               OPENROUTER_API_KEY=os.environ.get("OPENROUTER_API_KEY", ""))
    for kv in filter(None, extra_env.split(",")):
        k, v = kv.split("=", 1)
        env[k] = v

    t0 = time.time()
    r = _sh(
        f"cd {wd} && timeout 2400 codex exec --skip-git-repo-check "
        f"--sandbox danger-full-access --json {json.dumps(task)} > /tmp/trace.jsonl 2>/tmp/err.log",
        env=env, timeout=2700,
    )
    wall = int(time.time() - t0)

    usage, calls = {}, 0
    for line in open("/tmp/trace.jsonl"):
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            continue
        if e.get("type") == "turn.completed":
            usage = e.get("usage", {})
        it = e.get("item", {}) or {}
        if it.get("type") == "command_execution" and it.get("aggregated_output"):
            calls += 1

    out = {
        "label": label, "case": case, "render_mode": render_mode, "wall_s": wall,
        "rc": r.returncode, "calls": calls,
        "tokens": (usage.get("input_tokens", 0) or 0) + (usage.get("output_tokens", 0) or 0),
        "usage": usage,
        "deliverables": _sh(f"ls {wd}/model_output 2>/dev/null").stdout.split(),
        "semfs_sha": _sh("cat /usr/local/share/semfs-git-sha").stdout.strip()[:12],
    }
    print(json.dumps(out, indent=2))
    return out


@app.local_entrypoint()
def run_batch(case: str = "289", reps: int = 4, render_mode: str = "inline"):
    """Parallel reps — the thing the EC2 box cannot do."""
    args = [(case, f"m{i+1}", render_mode, "") for i in range(reps)]
    for res in run_case.starmap(args):
        print(json.dumps(res))
