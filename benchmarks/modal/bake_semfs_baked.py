#!/usr/bin/env python3
"""Bake the ROOT base E2B template ``semfs-baked`` from scratch (new E2B account).

The original base was built 2026-06-13 by ``/tmp/e2b_matrix/e2b_build_baked.py`` —
ephemeral, never committed (E2B_RUNBOOK.md §2). Every child template
(``semfs-baked-v2/v3``, ``semfs-mount-{persona}``) does ``from_template("semfs-baked")``,
so a fresh account needs this root rebuilt FIRST or nothing else can bake.

Reconstructed per E2B_RUNBOOK.md §2, fully Modal-side from the ``semfs-bench-data``
volume (harness + shims pre-staged under /_basebuild by `modal volume put`):
  - volume _basebuild/evaluation   -> /opt/wb/evaluation (+ npm install -> claude-agent-sdk + linux ripgrep)
  - volume _basebuild/semfs-shims  -> /opt/semfs-shims
  - volume bin/semfs-fixed         -> /usr/local/bin/semfs
  - volume models/gemma_q4         -> /opt/gemma_q4
  - build-time: ubuntu:24.04 + fuse3/python3/ripgrep/sudo + node20 + @openai/codex@0.133.0
  - /opt/cases (empty dir; .task files are baked per-persona / pushed at boot)

NOT baked: the 723 MB chanpin seed (per-persona templates bake their own seed; the
plain arm uses no seed) and ANY credentials (RUNBOOK §0.3 — auth.json/config.toml are
runtime-only, pushed at boot by run_matrix).

Auths to E2B via the ``e2b`` secret — point it at the NEW account first:
  modal secret create e2b E2B_API_KEY=<newkey> --force

Run:  modal run benchmarks/modal/bake_semfs_baked.py
"""
import json
import shutil
import tempfile
from pathlib import Path

import modal

app = modal.App("semfs-bake-base")
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
def bake_base(template_name: str = "semfs-baked") -> dict:
    from e2b import Template

    semfs_bin = Path(f"{VOL}/bin/semfs-fixed")
    gemma_dir = Path(f"{VOL}/models/gemma_q4")
    harness = Path(f"{VOL}/_basebuild/evaluation")
    shims = Path(f"{VOL}/_basebuild/semfs-shims")
    meta_root = Path(f"{VOL}/wb/lite_all/task_lite_clean_en")  # case metadata → /opt/cases/*.task
    missing = [str(p) for p in (semfs_bin, gemma_dir, harness, shims, meta_root) if not p.exists()]
    if missing:
        raise RuntimeError(f"missing inputs for semfs-baked: {missing}")

    print(f"[base] building {template_name} from ubuntu:24.04", flush=True)
    with tempfile.TemporaryDirectory(prefix="e2b_base_") as td:
        ctx = Path(td)
        # Stage the E2B build context (everything .copy() references must live here).
        shutil.copytree(harness, ctx / "evaluation")
        shutil.copytree(shims, ctx / "semfs-shims")
        shutil.copytree(gemma_dir, ctx / "gemma_q4")
        shutil.copy2(semfs_bin, ctx / "semfs")
        # Case .task files (the task instruction cell_driver.py:134 reads from
        # /opt/cases/<id>.task). The original semfs-baked baked these (RUNBOOK §2);
        # run_matrix does NOT push them at boot. = metadata.json["task"] per case.
        cases_ctx = ctx / "cases"
        cases_ctx.mkdir()
        n_cases = 0
        for cd in sorted(meta_root.iterdir()):
            mp = cd / "metadata.json"
            if not mp.exists():
                continue
            try:
                task = json.loads(mp.read_text())["task"].strip()
            except Exception as ex:
                print(f"  skip case {cd.name}: {repr(ex)[:60]}", flush=True)
                continue
            (cases_ctx / f"{cd.name}.task").write_text(task + "\n")
            n_cases += 1
        print(f"  baked {n_cases} case .task files into /opt/cases", flush=True)
        if n_cases == 0:
            raise RuntimeError("no case .task files generated — /opt/cases would be empty")
        for sub in ("evaluation", "semfs-shims", "gemma_q4", "semfs", "cases"):
            p = ctx / sub
            sz = sum(f.stat().st_size for f in p.rglob("*") if f.is_file()) if p.is_dir() else p.stat().st_size
            print(f"  ctx/{sub}: {sz / 1024 / 1024:.1f} MB", flush=True)

        t = Template(file_context_path=str(ctx))
        b = (
            t.from_ubuntu_image("24.04")
            .apt_install([
                "fuse3", "python3", "python-is-python3", "ripgrep",
                "ca-certificates", "curl", "gnupg", "git", "sudo", "unzip",
            ])
            # node20 (RUNBOOK §2) + codex CLI pinned to the known-good harness version.
            .run_cmd(
                "curl -fsSL https://deb.nodesource.com/setup_20.x | bash - "
                "&& apt-get install -y nodejs",
                user="root",
            )
            .run_cmd("npm install -g @openai/codex@0.133.0", user="root")
            # semfs release binary (boot_prep pushes the FIXED one over this anyway).
            .copy("semfs", "/usr/local/bin/semfs", user="root")
            .run_cmd("chmod 755 /usr/local/bin/semfs", user="root")
            # WB harness + its node deps (claude-agent-sdk ships the linux ripgrep
            # that boot_prep globs at *ripgrep*linux*/rg; also needed by claude arms).
            .copy("evaluation", "/opt/wb/evaluation", user="root")
            .run_cmd(
                "cd /opt/wb/evaluation && npm install --no-audit --no-fund",
                user="root",
            )
            # embedder + grep/format shims + the cases mount-point dir.
            .copy("gemma_q4", "/opt/gemma_q4", user="root")
            .copy("semfs-shims", "/opt/semfs-shims", user="root")
            .copy("cases", "/opt/cases", user="root")
            .run_cmd(
                "chmod -R a+rX /opt/semfs-shims /opt/wb /opt/gemma_q4 /opt/cases",
                user="root",
            )
            # a+rX only adds +x to dirs/already-exec files — the shim SCRIPTS ship
            # mode 644, so without this they're not executable and PATH-prepend is a
            # no-op (which grep -> /usr/bin/grep). Force +x on the shim executables.
            .run_cmd("chmod +x /opt/semfs-shims/grep /opt/semfs-shims/rg", user="root")
            # Build-time sanity: every contract boot_prep/cell_driver depends on.
            .run_cmd(
                "set -e; "
                "echo '--- semfs ---'; /usr/local/bin/semfs --version || /usr/local/bin/semfs --help | head -1; "
                "echo '--- codex ---'; codex --version; "
                "echo '--- node ---'; node --version; "
                "echo '--- python ---'; python3 --version; "
                "echo '--- codex.py harness ---'; test -f /opt/wb/evaluation/src/agents/codex.py && echo OK; "
                "echo '--- claude-agent-sdk ---'; test -d /opt/wb/evaluation/node_modules/@anthropic-ai && echo OK; "
                "echo '--- linux ripgrep glob ---'; find /opt/wb -path '*ripgrep*linux*/rg' | head -1 || true; "
                "echo '--- system rg fallback ---'; command -v rg; "
                "echo '--- shims ---'; ls /opt/semfs-shims; "
                "echo '--- embedder ---'; ls /opt/gemma_q4/model_q4.onnx; "
                "echo '--- cases ---'; echo \"count=$(ls /opt/cases | wc -l)\"; "
                "test -f /opt/cases/15.task && echo 'case 15.task OK' || (echo 'MISSING 15.task' && exit 1)",
                user="root",
            )
        )
        print(f"[base] calling E2B Template.build({template_name})", flush=True)
        info = Template.build(
            b,
            name=template_name,
            cpu_count=4,
            memory_mb=8192,
            request_timeout=1800.0,
            on_build_logs=lambda e: print("  >", getattr(e, "message", str(e))[:400], flush=True),
        )
        out = {"template": template_name, "base": "ubuntu:24.04", "build_info": str(info)}
        print(f"[base] build finished: {json.dumps(out, default=str)}", flush=True)
        return out


@app.local_entrypoint()
def main(template_name: str = "semfs-baked"):
    print(json.dumps(bake_base.remote(template_name), default=str, indent=2))
