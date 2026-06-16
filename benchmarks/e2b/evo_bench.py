#!/usr/bin/env python3
"""evo benchmark harness — semfs grep-delivery optimization on glm-5.1 (E2B real-FUSE).

Contract (evo instrumentation):
  * reads experiment edits from the WORKTREE (crates/ Rust source + knobs/evo_active.json
    config + benchmarks/e2b/{cell_driver,run_matrix}.py),
  * runs cases 53+171, arm=nokg, n=3 reps on glm-5.1 (codex-cli via OpenRouter) on E2B,
  * judges with Seed-2.0-Lite,
  * writes {"score": mean_accuracy, "tasks": {...}} to $EVO_RESULT_PATH (maximize),
  * drops a fixed sidecar .evo_bench_metrics.json (mean tokens) for the paired token gate
    (gates do NOT receive EVO_* env, so the gate reads this fixed worktree-relative path).

Objective (user): beat PLAIN on BOTH axes — higher accuracy AND lower tokens. evo maximizes
accuracy; the token-ceiling gate (evo_token_gate.py) enforces tokens <= plain.

Surface = code+config: if the worktree's Rust source differs from any cached build, Modal-
rebuild the x86_64 binary first (hash-cached, so identical source never rebuilds twice);
pure-config experiments reuse the existing compiled binary (no rebuild).

Big assets (seed/corpus/embedder/wb_lite) are gitignored → absent from worktrees; we symlink
the MAIN repo's assets/ in (read-only shared). The per-experiment binary is passed via
WB_FIXED_BIN so we never write into the shared assets dir.

Invoked by evo as:
  EVO_MAIN_REPO=/abs/main python3 {worktree}/benchmarks/e2b/evo_bench.py --worktree {worktree}
"""
import argparse, hashlib, json, os, pathlib, shutil, subprocess, sys, time

CASES = ["53", "171"]
N_REPS = 3
ARM = "nokg"
OR_MODEL = "z-ai/glm-5.1"
# A cell needs >= this many valid (judged) results out of N_REPS*len(CASES) to be a real
# score; below it we exit non-zero (infra failure, not a low score) so evo retries.
MIN_VALID_FRACTION = 0.5


def log(msg):
    print(f"[evo_bench {time.strftime('%H:%M:%S')}] {msg}", flush=True)


def run(cmd, cwd=None, env=None, timeout=None, retries=1, backoff=20):
    """Run a subprocess with exponential-backoff retry on transient infra failures
    (Internal server error / 5xx / connection resets — per the run's standing goal)."""
    last = None
    for attempt in range(retries):
        try:
            r = subprocess.run(cmd, cwd=cwd, env=env, timeout=timeout,
                               capture_output=True, text=True)
            blob = (r.stdout or "") + (r.stderr or "")
            transient = any(s in blob.lower() for s in (
                "internal server error", "502 bad gateway", "503 service",
                "connection reset", "timed out", "temporarily unavailable"))
            if r.returncode == 0 or not transient or attempt == retries - 1:
                return r
            wait = backoff * (2 ** attempt)
            log(f"  transient failure (attempt {attempt+1}/{retries}); backoff {wait}s")
            time.sleep(wait)
            last = r
        except subprocess.TimeoutExpired as e:
            last = e
            if attempt == retries - 1:
                raise
            wait = backoff * (2 ** attempt)
            log(f"  subprocess timeout (attempt {attempt+1}/{retries}); backoff {wait}s")
            time.sleep(wait)
    return last


def load_env(main_repo):
    """Inject OPENROUTER_API_KEY / E2B_API_KEY from the MAIN repo's .env + pin glm-5.1."""
    env = dict(os.environ)
    envf = main_repo / ".env"
    if envf.exists():
        for line in envf.read_text().splitlines():
            line = line.strip()
            if "=" in line and not line.startswith("#"):
                k, _, v = line.partition("=")
                env[k.strip()] = v.strip()
    env["WB_FORCE_OPENROUTER"] = "1"
    env["WB_OR_MODEL"] = OR_MODEL
    return env


def src_hash(worktree):
    """Stable content hash of the Rust build inputs in the worktree."""
    h = hashlib.sha256()
    roots = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]
    files = []
    for r in roots:
        p = worktree / r
        if p.exists():
            files.append(p)
    crates = worktree / "crates"
    if crates.exists():
        files += sorted(crates.rglob("*.rs"))
        files += sorted(crates.rglob("Cargo.toml"))
    for p in sorted(set(files), key=lambda x: str(x.relative_to(worktree))):
        h.update(str(p.relative_to(worktree)).encode())
        h.update(b"\0")
        h.update(p.read_bytes())
        h.update(b"\0")
    return h.hexdigest()[:16]


def ensure_assets(worktree, main_repo):
    """Symlink the MAIN repo's (gitignored, big) assets/ into the worktree — read-only shared."""
    wt_assets = worktree / "benchmarks/e2b/assets"
    main_assets = main_repo / "benchmarks/e2b/assets"
    if wt_assets.is_symlink() or wt_assets.exists():
        return
    wt_assets.parent.mkdir(parents=True, exist_ok=True)
    wt_assets.symlink_to(main_assets)
    log(f"  symlinked assets → {main_assets}")


def ensure_binary(worktree, main_repo, env):
    """Return the path to the x86_64 binary for this experiment's source.
    Hash-cached: identical source never rebuilds. Seeds the cache from the existing
    assets/semfs-fixed for the baseline source hash."""
    sh = src_hash(worktree)
    cache_dir = main_repo / "benchmarks/e2b/assets/.evo_bin_cache" / sh
    cached = cache_dir / "semfs-fixed"
    existing = main_repo / "benchmarks/e2b/assets/semfs-fixed"
    if cached.exists():
        log(f"  binary cache HIT for src {sh}")
        return cached
    # Seed: if no cache yet and this hash matches the recorded baseline, reuse existing binary.
    baseline_sha_f = main_repo / "benchmarks/e2b/.evo_baseline_src_hash"
    if baseline_sha_f.exists() and baseline_sha_f.read_text().strip() == sh and existing.exists():
        cache_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy2(existing, cached)
        log(f"  binary seeded from baseline assets for src {sh}")
        return cached
    # Rebuild on Modal from the worktree source.
    log(f"  binary cache MISS for src {sh} — Modal x86_64 rebuild")
    cache_dir.mkdir(parents=True, exist_ok=True)
    tgz = f"/tmp/evo_src_{sh}.tgz"
    tar_inputs = [p for p in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "crates")
                  if (worktree / p).exists()]
    run(["tar", "czf", tgz, "-C", str(worktree), *tar_inputs], timeout=300)
    run(["modal", "volume", "put", "semfs-bench-data", tgz, "/_build/semfs_src.tgz", "--force"],
        cwd=str(main_repo), env=env, timeout=600, retries=3)
    br = run(["modal", "run", "benchmarks/modal/build_semfs.py"],
             cwd=str(main_repo), env=env, timeout=2700, retries=3)
    if br is None or br.returncode != 0:
        raise RuntimeError(f"Modal build failed: {(br.stderr or '')[-800:] if br else 'no result'}")
    gr = run(["modal", "volume", "get", "semfs-bench-data", "/bin/semfs-fixed",
              str(cached), "--force"], cwd=str(main_repo), env=env, timeout=600, retries=3)
    if gr is None or gr.returncode != 0 or not cached.exists():
        raise RuntimeError(f"Modal binary fetch failed: {(gr.stderr or '')[-400:] if gr else 'no result'}")
    os.chmod(cached, 0o755)
    log(f"  rebuilt binary cached at {cached}")
    return cached


def run_reps(worktree, knobs, binary, env, exp_tag):
    """Run cases × N_REPS for the nokg arm via the worktree's run_matrix.py; return labels."""
    runmtx = worktree / "benchmarks/e2b/run_matrix.py"
    labels = []
    cell_env = dict(env)
    cell_env["WB_FIXED_BIN"] = str(binary)
    for r in range(1, N_REPS + 1):
        rep = f"{exp_tag}r{r}"
        cmd = ["python3", str(runmtx), "--cases", ",".join(CASES), "--agents", "codex",
               "--arms", ARM, "--rep", rep, "--parallel", "2", "--force"]
        if knobs.exists():
            cmd += ["--knobs", str(knobs)]
        log(f"  run_matrix rep {r}/{N_REPS} (tag {rep})")
        run(cmd, cwd=str(worktree), env=cell_env, timeout=4200, retries=2)
        for c in CASES:
            labels.append(f"pm_codex_{c}_{ARM}_r{rep}")
    return labels


def judge(worktree, labels, env):
    runjudge = worktree / "benchmarks/e2b/run_judge.py"
    # stage rubrics for the judge (it reads /tmp/wb_lite)
    run(["bash", "-c",
         "mkdir -p /tmp/wb_lite && cp -a benchmarks/e2b/assets/wb_lite/task_lite_clean_en /tmp/wb_lite/ 2>/dev/null || true"],
        cwd=str(worktree), env=env, timeout=120)
    run(["python3", str(runjudge), *labels], cwd=str(worktree), env=env, timeout=3000, retries=2)


def prune_heavy(worktree, labels):
    """Disk-bound the autonomous loop: drop the bulky re-pullable raw from THIS experiment's
    own fresh cells (full.tgz / sandbox_raw / semfs_logs) after scoring. Keeps result.json,
    model_output, and the judge output. Never touches other experiments' artifacts."""
    runs = worktree / "tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs"
    for lbl in labels:
        d = runs / lbl
        if not d.exists():
            continue
        try:
            (d / "full.tgz").unlink(missing_ok=True)
            for sub in ("sandbox_raw", "semfs_logs"):
                shutil.rmtree(d / sub, ignore_errors=True)
        except Exception:
            pass


def collect(worktree, labels):
    """Read accuracy (judge summary) + tokens (result.json) per cell."""
    runs = worktree / "tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs"
    cells = []
    for lbl in labels:
        d = runs / lbl
        acc = tok = None
        status = "missing"
        rj = d / "result.json"
        if rj.exists():
            try:
                rd = json.loads(rj.read_text())
                status = rd.get("status")
                tok = rd.get("tokens")
            except Exception:
                pass
        jf = sorted(d.glob("rubrics_judge--*.json"))
        if jf:
            try:
                s = json.loads(jf[0].read_text()).get("summary", {})
                if s.get("total"):
                    acc = s["passed"] / s["total"]
            except Exception:
                pass
        cells.append({"label": lbl, "status": status, "accuracy": acc, "tokens": tok})
    return cells


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--worktree", required=True)
    args = ap.parse_args()
    worktree = pathlib.Path(args.worktree).resolve()
    main_repo = pathlib.Path(os.environ.get("EVO_MAIN_REPO", "")).resolve()
    if not main_repo or not (main_repo / "benchmarks/e2b/assets").exists():
        log(f"FATAL: EVO_MAIN_REPO unset or missing assets ({main_repo})")
        sys.exit(2)
    exp_id = os.environ.get("EVO_EXPERIMENT_ID", "unknown").replace("_", "")
    exp_tag = f"evo{exp_id}"
    log(f"experiment {exp_id}  worktree={worktree}")

    env = load_env(main_repo)
    ensure_assets(worktree, main_repo)
    binary = ensure_binary(worktree, main_repo, env)
    knobs = worktree / "benchmarks/e2b/knobs/evo_active.json"

    labels = run_reps(worktree, knobs, binary, env, exp_tag)
    judge(worktree, labels, env)
    cells = collect(worktree, labels)
    prune_heavy(worktree, labels)   # disk-bound the loop: keep result.json+model_output+rubrics

    valid = [c for c in cells if c["accuracy"] is not None and c["tokens"] is not None]
    log(f"valid cells {len(valid)}/{len(cells)}")
    for c in cells:
        log(f"  {c['label']:42} status={c['status']:18} acc={c['accuracy']} tok={c['tokens']}")
    if len(valid) < MIN_VALID_FRACTION * len(cells):
        log("FATAL: too few valid cells — treating as infra failure (non-zero exit)")
        sys.exit(3)

    mean_acc = sum(c["accuracy"] for c in valid) / len(valid)
    mean_tok = sum(c["tokens"] for c in valid) / len(valid)
    # per-task scores: index by case+rep label suffix
    tasks = {c["label"]: c["accuracy"] for c in valid}

    # token sidecar for the paired gate (gates can't see EVO_* env → fixed relative path)
    metrics = {"mean_accuracy": mean_acc, "mean_tokens": mean_tok,
               "n_valid": len(valid), "n_total": len(cells), "cells": cells}
    (worktree / ".evo_bench_metrics.json").write_text(json.dumps(metrics, indent=2))

    result = {"score": round(mean_acc, 6), "tasks": tasks,
              "mean_tokens": mean_tok, "n_valid": len(valid)}
    payload = json.dumps(result)
    rp = os.environ.get("EVO_RESULT_PATH")
    if rp:
        tmp = rp + ".tmp"
        # atomic claim-then-rename per the instrumentation contract
        fd = os.open(rp, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
        os.close(fd)
        pathlib.Path(tmp).write_text(payload)
        os.replace(tmp, rp)
    else:
        print(payload)
    log(f"DONE score(accuracy)={mean_acc:.4f}  mean_tokens={mean_tok:.0f}")


if __name__ == "__main__":
    main()
