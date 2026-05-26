#!/usr/bin/env python3
from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

import yaml


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Install SEMFS-backed agent adapters into a local Workspace-Bench checkout.",
    )
    parser.add_argument("--workspace-bench-root", required=True, help="Path to a local Workspace-Bench checkout")
    parser.add_argument(
        "--harness",
        default="codex",
        choices=["codex", "claudecode"],
        help="Official Workspace-Bench harness to target",
    )
    parser.add_argument("--model", required=True, help="Official Workspace-Bench model alias, e.g. kimi-k2.5")
    parser.add_argument("--dataset", default="lite", choices=["smoke", "lite", "full"])
    parser.add_argument("--provider-type", default="openai", choices=["openai", "anthropic"])
    parser.add_argument("--model-id")
    parser.add_argument("--model-name")
    parser.add_argument("--env-prefix")
    parser.add_argument("--run-name")
    parser.add_argument("--task-limit", type=int)
    parser.add_argument("--timeout-sec", type=float, default=2000.0)
    parser.add_argument("--eval-yaml", default="runs/judge.yaml")
    return parser.parse_args()


def copy_adapter(repo_root: Path, wb_root: Path, harness: str) -> Path:
    adapter_name = f"semfs{harness}"
    src = repo_root / "benchmarks" / "workspace_bench" / f"{adapter_name}.py"
    dst = wb_root / "evaluation" / "src" / "agents" / f"{adapter_name}.py"
    if not src.is_file():
        raise SystemExit(f"missing adapter source: {src}")
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dst)
    return dst


def build_base_run_config(wb_root: Path, args: argparse.Namespace) -> Path:
    eval_root = wb_root / "evaluation"
    script = eval_root / "scripts" / "build_run_config.py"
    cmd = [
        sys.executable,
        str(script),
        "--harness",
        args.harness,
        "--model",
        args.model,
        "--dataset",
        args.dataset,
        "--eval-root",
        str(eval_root),
        "--provider-type",
        args.provider_type,
        "--timeout-sec",
        str(args.timeout_sec),
        "--eval-yaml",
        args.eval_yaml,
    ]
    if args.model_id:
        cmd.extend(["--model-id", args.model_id])
    if args.model_name:
        cmd.extend(["--model-name", args.model_name])
    if args.env_prefix:
        cmd.extend(["--env-prefix", args.env_prefix])
    if args.run_name:
        cmd.extend(["--run-name", args.run_name])
    if args.task_limit is not None:
        cmd.extend(["--task-limit", str(args.task_limit)])

    result = subprocess.run(cmd, check=True, capture_output=True, text=True)
    config_path = Path(result.stdout.strip())
    if not config_path.is_absolute():
        config_path = (eval_root / config_path).resolve()
    return config_path


def rewrite_run_config(base_config: Path, harness: str) -> Path:
    config = yaml.safe_load(base_config.read_text(encoding="utf-8"))
    if not isinstance(config, dict):
        raise SystemExit(f"run config is not a mapping: {base_config}")

    semfs_agent_name = f"SEMFS{'ClaudeCode' if harness == 'claudecode' else 'Codex'}"
    config["agent_name"] = semfs_agent_name
    run_name = str(config.get("run_name") or "").strip()
    if run_name and "SEMFS" not in run_name:
        config["run_name"] = f"{run_name}-SEMFS"

    target = base_config.with_name(base_config.stem.replace(harness, f"semfs{harness}") + ".yaml")
    target.write_text(yaml.safe_dump(config, allow_unicode=True, sort_keys=False), encoding="utf-8")
    return target


def main() -> None:
    args = parse_args()
    repo_root = Path(__file__).resolve().parents[2]
    wb_root = Path(args.workspace_bench_root).resolve()
    eval_root = wb_root / "evaluation"

    if not eval_root.is_dir():
        raise SystemExit(f"Workspace-Bench evaluation dir not found: {eval_root}")

    adapter_path = copy_adapter(repo_root, wb_root, args.harness)
    base_config = build_base_run_config(wb_root, args)
    semfs_config = rewrite_run_config(base_config, args.harness)

    print(f"[ok] installed adapter: {adapter_path}")
    print(f"[ok] base config: {base_config}")
    print(f"[ok] semfs config: {semfs_config}")
    print()
    print("Next steps:")
    print(f"  cd {eval_root}")
    print("  python3 scripts/download_hf_assets.py --lite --workspaces")
    print(f"  python3 scripts/prepare_workdirs_for_run.py --run-config {semfs_config}")
    print(f"  python3 -u src/agent_runner.py --run-config {semfs_config}")


if __name__ == "__main__":
    main()
