#!/usr/bin/env python3
import argparse
import json
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, Iterable, List, Tuple


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def scan_workspace(workspace_dir: Path) -> Dict[str, object]:
    files: Dict[str, Dict[str, int]] = {}
    total_bytes = 0
    file_count = 0

    for path in sorted(workspace_dir.rglob("*")):
        if not path.is_file():
            continue
        try:
            stat = path.stat()
        except FileNotFoundError:
            continue
        rel_path = path.relative_to(workspace_dir).as_posix()
        size = int(stat.st_size)
        mtime_ns = int(stat.st_mtime_ns)
        files[rel_path] = {"size": size, "mtimeNs": mtime_ns}
        total_bytes += size
        file_count += 1

    return {
        "path": str(workspace_dir),
        "fileCount": file_count,
        "totalBytes": total_bytes,
        "files": files,
    }


def iter_workspaces(filesys_root: Path) -> Iterable[Path]:
    for path in sorted(filesys_root.glob("*_workdir_*")):
        if path.is_dir():
            yield path


def build_snapshot(filesys_root: Path, label: str) -> Dict[str, object]:
    workspaces = {}
    for workspace_dir in iter_workspaces(filesys_root):
        workspaces[workspace_dir.name] = scan_workspace(workspace_dir)
    return {
        "capturedAt": utc_now(),
        "label": label,
        "filesysRoot": str(filesys_root),
        "workspaceCount": len(workspaces),
        "workspaces": workspaces,
    }


def diff_workspace(
    before_files: Dict[str, Dict[str, int]], after_files: Dict[str, Dict[str, int]]
) -> Dict[str, object]:
    before_paths = set(before_files)
    after_paths = set(after_files)
    created = sorted(after_paths - before_paths)
    deleted = sorted(before_paths - after_paths)
    modified: List[str] = []
    unchanged = 0

    for path in sorted(before_paths & after_paths):
        before_meta = before_files[path]
        after_meta = after_files[path]
        if before_meta.get("size") != after_meta.get("size") or before_meta.get("mtimeNs") != after_meta.get("mtimeNs"):
            modified.append(path)
        else:
            unchanged += 1

    return {
        "createdCount": len(created),
        "deletedCount": len(deleted),
        "modifiedCount": len(modified),
        "unchangedCount": unchanged,
        "createdPaths": created[:200],
        "deletedPaths": deleted[:200],
        "modifiedPaths": modified[:200],
    }


def build_diff(before: Dict[str, object], after: Dict[str, object]) -> Dict[str, object]:
    before_workspaces = before.get("workspaces") if isinstance(before.get("workspaces"), dict) else {}
    after_workspaces = after.get("workspaces") if isinstance(after.get("workspaces"), dict) else {}
    workspace_names = sorted(set(before_workspaces) | set(after_workspaces))
    per_workspace: Dict[str, object] = {}

    total_created = 0
    total_deleted = 0
    total_modified = 0

    for name in workspace_names:
        before_ws = before_workspaces.get(name) if isinstance(before_workspaces.get(name), dict) else {}
        after_ws = after_workspaces.get(name) if isinstance(after_workspaces.get(name), dict) else {}
        before_files = before_ws.get("files") if isinstance(before_ws.get("files"), dict) else {}
        after_files = after_ws.get("files") if isinstance(after_ws.get("files"), dict) else {}
        diff = diff_workspace(before_files, after_files)
        diff["beforeFileCount"] = before_ws.get("fileCount")
        diff["afterFileCount"] = after_ws.get("fileCount")
        diff["beforeTotalBytes"] = before_ws.get("totalBytes")
        diff["afterTotalBytes"] = after_ws.get("totalBytes")
        per_workspace[name] = diff
        total_created += int(diff["createdCount"])
        total_deleted += int(diff["deletedCount"])
        total_modified += int(diff["modifiedCount"])

    return {
        "capturedAt": utc_now(),
        "beforeLabel": before.get("label"),
        "afterLabel": after.get("label"),
        "filesysRoot": after.get("filesysRoot") or before.get("filesysRoot"),
        "workspaceCount": len(workspace_names),
        "summary": {
            "createdCount": total_created,
            "deletedCount": total_deleted,
            "modifiedCount": total_modified,
        },
        "workspaces": per_workspace,
    }


def write_json(path: Path, payload: Dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Capture or diff workspace telemetry snapshots.")
    subparsers = parser.add_subparsers(dest="command", required=True)

    snapshot_parser = subparsers.add_parser("snapshot")
    snapshot_parser.add_argument("--filesys-root", required=True)
    snapshot_parser.add_argument("--label", required=True)
    snapshot_parser.add_argument("--output", required=True)

    diff_parser = subparsers.add_parser("diff")
    diff_parser.add_argument("--before", required=True)
    diff_parser.add_argument("--after", required=True)
    diff_parser.add_argument("--output", required=True)

    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.command == "snapshot":
        payload = build_snapshot(Path(args.filesys_root), args.label)
        write_json(Path(args.output), payload)
        return
    if args.command == "diff":
        before = json.loads(Path(args.before).read_text(encoding="utf-8"))
        after = json.loads(Path(args.after).read_text(encoding="utf-8"))
        payload = build_diff(before, after)
        write_json(Path(args.output), payload)
        return
    raise SystemExit(f"unsupported command: {args.command}")


if __name__ == "__main__":
    main()
