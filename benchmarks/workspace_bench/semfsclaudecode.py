from __future__ import annotations

import ast
import importlib.util
import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

Json = Any


def _truthy(value: Optional[str], default: bool = False) -> bool:
    if value is None:
        return default
    return value.strip().lower() in {"1", "true", "yes", "y", "on"}


def _load_claudecode_run():
    harness_path = Path(__file__).with_name("claudecode.py")
    spec = importlib.util.spec_from_file_location("workspace_bench_claudecode", harness_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load claudecode harness from {harness_path}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    run_fn = getattr(mod, "run", None)
    if not callable(run_fn):
        raise RuntimeError("claudecode harness missing run()")
    return run_fn


def _semfs_bin() -> str:
    explicit = os.environ.get("SEMFS_BIN")
    if explicit:
        return explicit
    found = shutil.which("semfs")
    if found:
        return found
    raise FileNotFoundError("semfs binary not found; set SEMFS_BIN or add semfs to PATH")


def _safe_tag(value: str) -> str:
    out = []
    for ch in value:
        if ch.isalnum() or ch in {"-", "_"}:
            out.append(ch.lower())
        else:
            out.append("-")
    tag = "".join(out).strip("-")
    while "--" in tag:
        tag = tag.replace("--", "-")
    return tag[:100] or "workspace-bench"


# This adapter is the ClaudeCode harness (semfscodex.py uses "codex").
_AGENT_KEY = "claudecode"


def _model_slug(api_provider: Dict[str, Json]) -> str:
    if isinstance(api_provider, dict):
        for k in ("model_name", "model"):
            v = api_provider.get(k)
            if isinstance(v, str) and v.strip():
                return v
    return "model"


def _container_tag(api_provider: Dict[str, Json]) -> str:
    """Per-agent, persistent container: one space per (harness, model), stable
    across cases so fs_remote accumulates and each file embeds once. Override with
    SEMFS_CONTAINER_TAG."""
    explicit = os.environ.get("SEMFS_CONTAINER_TAG")
    if explicit:
        return _safe_tag(explicit)
    prefix = os.environ.get("SEMFS_CONTAINER_PREFIX", "workspace-bench")
    return _safe_tag(f"{prefix}-{_AGENT_KEY}-{_model_slug(api_provider)}")


def _case_memory_paths(work_dir: str, sandbox_dir: str) -> Optional[str]:
    """Scope semfs memory/embedding to only the files this case needs (from the
    case metadata's data_manifest / file_dep_graph), matched by basename in the
    workdir. Outputs (model_output/) are never inputs, so they're excluded.
    SEMFS_MEMORY_PATHS env overrides; no manifest -> None (no scoping)."""
    env_mp = os.environ.get("SEMFS_MEMORY_PATHS")
    if env_mp is not None:
        return env_mp
    try:
        meta = json.loads(Path(os.path.join(sandbox_dir, "metadata.json")).read_text(encoding="utf-8"))
    except Exception:
        return None
    needed: set[str] = set()
    for m in meta.get("data_manifest") or []:
        if isinstance(m, dict) and isinstance(m.get("filename"), str):
            needed.add(os.path.basename(m["filename"]))
    for e in meta.get("file_dep_graph") or []:
        if isinstance(e, dict) and isinstance(e.get("from"), str):
            needed.add(os.path.basename(e["from"]))
    needed.discard("")
    if not needed:
        return None
    work_abs = os.path.abspath(work_dir)
    paths: set[str] = set()
    for root, _dirs, files in os.walk(work_abs):
        for fname in files:
            if fname in needed:
                rel = os.path.relpath(os.path.join(root, fname), work_abs).replace("\\", "/")
                paths.add("/" + rel)
    return ",".join(sorted(paths)) if paths else None


def _run_cmd(cmd: list[str], *, cwd: str, timeout_s: float) -> Dict[str, Json]:
    started = time.time()
    try:
        proc = subprocess.run(
            cmd,
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=timeout_s if timeout_s > 0 else None,
            check=False,
        )
        return {
            "cmd": cmd,
            "exitCode": int(proc.returncode),
            "stdout": proc.stdout,
            "stderr": proc.stderr,
            "durationMs": int((time.time() - started) * 1000),
        }
    except subprocess.TimeoutExpired as exc:
        return {
            "cmd": cmd,
            "exitCode": 124,
            "stdout": exc.stdout or "",
            "stderr": exc.stderr or "",
            "durationMs": int((time.time() - started) * 1000),
            "timedOut": True,
        }
    except Exception as exc:
        return {
            "cmd": cmd,
            "exitCode": 1,
            "stdout": "",
            "stderr": str(exc),
            "durationMs": int((time.time() - started) * 1000),
        }


def _daemon_log_path(stderr_text: Optional[str]) -> Optional[str]:
    if not isinstance(stderr_text, str):
        return None
    match = re.search(r"Log:\s+(\S+)", stderr_text)
    if not match:
        return None
    return match.group(1)


def _tail_text_file(path: str, *, max_chars: int = 8000) -> Optional[str]:
    try:
        text = Path(path).read_text(encoding="utf-8")
    except Exception:
        return None
    if len(text) <= max_chars:
        return text
    return text[-max_chars:]


def _semfs_failure_diagnostics(*, work_dir: str, mount_result: Dict[str, Json]) -> Dict[str, Json]:
    api_key = os.environ.get("SUPERMEMORY_API_KEY") or ""
    diagnostics: Dict[str, Json] = {
        "semfsBin": _semfs_bin(),
        "env": {
            "supermemoryApiKeyPresent": bool(api_key),
            "supermemoryApiKeyLength": len(api_key) if api_key else 0,
            "supermemoryApiUrl": os.environ.get("SUPERMEMORY_API_URL"),
        },
        "whoami": _run_cmd([_semfs_bin(), "whoami", "--json"], cwd=os.path.abspath(work_dir), timeout_s=15),
    }
    log_path = _daemon_log_path(mount_result.get("stderr") if isinstance(mount_result, dict) else None)
    if log_path:
        diagnostics["daemonLogPath"] = log_path
        diagnostics["daemonLogTail"] = _tail_text_file(log_path)
    return diagnostics


def _mount_semfs(*, work_dir: str, container_tag: str, memory_paths: Optional[str] = None) -> Dict[str, Json]:
    mount_timeout = float(os.environ.get("SEMFS_MOUNT_TIMEOUT_SEC", "120"))
    cmd = [_semfs_bin(), "mount", container_tag, "--path", os.path.abspath(work_dir)]
    if _truthy(os.environ.get("SEMFS_NO_SYNC"), default=False):
        cmd.append("--no-sync")
    if _truthy(os.environ.get("SEMFS_NO_PUSH"), default=False):
        cmd.append("--no-push")
    if _truthy(os.environ.get("SEMFS_CLEAN"), default=False):
        cmd.append("--clean")
    # `semfs mount` aborts if the daemon makes no startup progress for
    # --startup-timeout seconds (default 30). Local-model indexing embeds files
    # on import in a silent, CPU-bound phase that easily exceeds 30s, so raise it
    # for local runs via SEMFS_STARTUP_TIMEOUT_SEC. Keep it <= SEMFS_MOUNT_TIMEOUT_SEC
    # (the subprocess timeout) or the subprocess kills mount first.
    startup_to = os.environ.get("SEMFS_STARTUP_TIMEOUT_SEC")
    if startup_to:
        cmd.extend(["--startup-timeout", startup_to])
    if memory_paths is not None:
        cmd.extend(["--memory-paths", memory_paths])
    return _run_cmd(cmd, cwd=os.path.abspath(work_dir), timeout_s=mount_timeout)


def _unmount_semfs(*, work_dir: str, container_tag: str) -> Dict[str, Json]:
    unmount_timeout = float(os.environ.get("SEMFS_UNMOUNT_TIMEOUT_SEC", "60"))
    cmd = [_semfs_bin(), "unmount", container_tag]
    return _run_cmd(cmd, cwd=os.path.abspath(work_dir), timeout_s=unmount_timeout)


def _mount_with_retry(*, work_dir: str, container_tag: str, memory_paths: Optional[str] = None) -> Dict[str, Json]:
    first = _mount_semfs(work_dir=work_dir, container_tag=container_tag, memory_paths=memory_paths)
    if _exit_code(first) == 0:
        return first
    stderr = first.get("stderr") if isinstance(first.get("stderr"), str) else ""
    if "already mounted" not in stderr:
        return first
    cleanup = _unmount_semfs(work_dir=work_dir, container_tag=container_tag)
    second = _mount_semfs(work_dir=work_dir, container_tag=container_tag, memory_paths=memory_paths)
    second["retryAfterUnmount"] = {
        "firstMount": first,
        "cleanupUnmount": cleanup,
    }
    return second


def _attach_semfs_trace(result: Dict[str, Json], semfs_trace: Dict[str, Json]) -> Dict[str, Json]:
    trace = result.get("trace")
    if not isinstance(trace, dict):
        trace = {}
        result["trace"] = trace
    trace["semfs"] = semfs_trace
    return result


def _exit_code(result: Dict[str, Json], default: int = 1) -> int:
    value = result.get("exitCode")
    if isinstance(value, int):
        return value
    return default


# Bounds so staging never walks the whole memory mount (~thousands of files) or
# copies an unbounded payload off it.
_MAX_STAGED_FILES = 200
_MAX_STAGED_BYTES = 200 * 1024 * 1024


def _extract_returned_paths(text: str) -> List[str]:
    """Pull the agent's final path list out of free text (last bracketed list)."""
    s = str(text or "").strip()
    if not s:
        return []
    candidates = re.findall(r"\[[\s\S]*?\]", s)
    for cand in reversed(candidates or [s]):
        try:
            obj = ast.literal_eval(cand)
        except Exception:
            continue
        if isinstance(obj, list):
            out = [str(x) for x in obj if isinstance(x, str) and str(x).strip()]
            if out:
                return out
    return []


def _rel_under(work_dir: str, p: str) -> Optional[str]:
    """Normalize ``p`` to a work_dir-relative POSIX path, or None if it escapes."""
    rel = str(p or "").strip().replace("\\", "/")
    while rel.startswith("/"):
        rel = rel[1:]
    if not rel:
        return None
    root_abs = os.path.abspath(work_dir)
    abs_p = os.path.abspath(os.path.join(root_abs, rel))
    if abs_p != root_abs and not abs_p.startswith(root_abs + os.sep):
        return None
    return os.path.relpath(abs_p, root_abs).replace("\\", "/")


def _stage_outputs_from_mount(
    *,
    work_dir: str,
    sandbox_dir: str,
    result: Dict[str, Json],
    api_provider: Dict[str, Json],
) -> List[Tuple[str, str]]:
    """While the semfs mount is live, copy the run's deliverables OUT of the mount
    into ``sandbox_dir/semfs_staged``. Discovery mirrors agent_runner: union of
    paths from trace.lastText, result["paths"], the implied output subtree(s), and
    expected filenames. Only the implied subtrees are walked (never the whole mount)."""
    work_abs = os.path.abspath(work_dir)
    trace = result.get("trace") if isinstance(result.get("trace"), dict) else {}
    last_text = str(trace.get("lastText") or "")

    rel_files: List[str] = []
    subtree_dirs: set[str] = set()

    def _note_rel(rel: Optional[str]) -> None:
        if not rel:
            return
        rel_files.append(rel)
        head = rel.split("/", 1)[0]
        if head and head != rel:
            subtree_dirs.add(head)

    for rp in _extract_returned_paths(last_text):
        _note_rel(_rel_under(work_abs, rp))
    for rp in result.get("paths") if isinstance(result.get("paths"), list) else []:
        if isinstance(rp, str):
            _note_rel(_rel_under(work_abs, rp))
    expected = api_provider.get("__expected_output_files__")
    if isinstance(expected, list):
        for ef in expected:
            if isinstance(ef, str) and "/" in ef.strip("/"):
                _note_rel(_rel_under(work_abs, ef))

    found: List[str] = []
    seen: set[str] = set()

    def _add(rel: str) -> None:
        if rel in seen:
            return
        if os.path.isfile(os.path.join(work_abs, rel)):
            seen.add(rel)
            found.append(rel)

    for rel in rel_files:
        _add(rel)
    for sub in sorted(subtree_dirs):
        sub_abs = os.path.join(work_abs, sub)
        if not os.path.isdir(sub_abs):
            continue
        for root, _dirs, files in os.walk(sub_abs):
            for fname in files:
                rel = os.path.relpath(os.path.join(root, fname), work_abs).replace("\\", "/")
                _add(rel)
                if len(found) >= _MAX_STAGED_FILES:
                    break
            if len(found) >= _MAX_STAGED_FILES:
                break

    staged: List[Tuple[str, str]] = []
    total_bytes = 0
    stage_root = os.path.join(os.path.abspath(sandbox_dir), "semfs_staged")
    for rel in found[:_MAX_STAGED_FILES]:
        src = os.path.join(work_abs, rel)
        try:
            size = os.path.getsize(src)
        except OSError:
            continue
        if total_bytes + size > _MAX_STAGED_BYTES:
            break
        dst = os.path.join(stage_root, rel)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copy2(src, dst)
        total_bytes += size
        staged.append((rel, dst))
    return staged


def _path_is_dead_or_mounted(path: str) -> bool:
    """True if ``path`` is still a mountpoint or an orphaned FUSE entry (ENOTCONN)."""
    try:
        if os.path.ismount(path):
            return True
        os.listdir(path)
        return False
    except OSError as exc:
        return exc.errno in (107,)


def _force_clear_mount(work_dir: str) -> bool:
    """semfs unmount tears the daemon down asynchronously, briefly leaving an orphaned
    FUSE entry (ENOTCONN). Retry fusermount3 -u in a short loop to wait out the teardown
    instead of racing it with a single shot. Returns True if cleared."""
    if not _path_is_dead_or_mounted(work_dir):
        return False
    for _attempt in range(20):
        if not _path_is_dead_or_mounted(work_dir):
            return True
        for cmd in (["fusermount3", "-u", work_dir], ["fusermount", "-u", work_dir], ["umount", work_dir]):
            try:
                subprocess.run(cmd, capture_output=True, text=True, timeout=30, check=False)
            except Exception:
                continue
            if not _path_is_dead_or_mounted(work_dir):
                return True
        time.sleep(1)
    return not _path_is_dead_or_mounted(work_dir)


def _restore_outputs_to_workdir(
    *, work_dir: str, staged: List[Tuple[str, str]]
) -> List[str]:
    """After unmount, copy staged files back into the now-bare work_dir."""
    work_abs = os.path.abspath(work_dir)
    restored: List[str] = []
    for rel, staged_abs in staged:
        safe_rel = _rel_under(work_abs, rel)
        if not safe_rel:
            continue
        dst = os.path.join(work_abs, safe_rel)
        try:
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(staged_abs, dst)
        except OSError:
            continue
        restored.append(safe_rel)
    return restored


def run(
    *,
    prompt: str,
    work_dir: str,
    sandbox_dir: str,
    timeout_s: float,
    api_provider: Dict[str, Json],
    agent_id: Optional[str] = None,
) -> Dict[str, Json]:
    claudecode_run = _load_claudecode_run()
    container_tag = _container_tag(api_provider)
    memory_paths = _case_memory_paths(work_dir, sandbox_dir)

    semfs_trace: Dict[str, Json] = {
        "enabled": True,
        "containerTag": container_tag,
        "workDir": os.path.abspath(work_dir),
        "memoryPaths": memory_paths,
    }

    mount_result = _mount_with_retry(work_dir=work_dir, container_tag=container_tag, memory_paths=memory_paths)
    semfs_trace["mount"] = mount_result
    semfs_trace["mountDurationMs"] = mount_result.get("durationMs")

    if _exit_code(mount_result) != 0:
        semfs_trace["failureDiagnostics"] = _semfs_failure_diagnostics(work_dir=work_dir, mount_result=mount_result)
        return {
            "status": "error",
            "paths": [],
            "errorMessage": f"semfs mount failed for {container_tag}",
            "trace": {
                "runner": "SEMFSClaudeCode",
                "rawDir": os.path.join(sandbox_dir, "raw"),
                "semfs": semfs_trace,
            },
            "metrics": {"turns": None, "promptTokens": None, "completionTokens": None, "totalTokens": None},
            "durationMs": int(mount_result.get("durationMs") or 0),
        }

    result: Dict[str, Json]
    staged: List[Tuple[str, str]] = []
    try:
        result = claudecode_run(
            prompt=prompt,
            work_dir=work_dir,
            sandbox_dir=sandbox_dir,
            timeout_s=timeout_s,
            api_provider=api_provider,
            agent_id=agent_id,
        )
        # Mount still live: rescue deliverables before the finally-unmount tears
        # them down. Fail-open so a staging error can't crash the run.
        try:
            staged = _stage_outputs_from_mount(
                work_dir=work_dir,
                sandbox_dir=sandbox_dir,
                result=result,
                api_provider=api_provider,
            )
            semfs_trace["stagedOutputs"] = [rel for rel, _ in staged]
        except Exception as exc:
            semfs_trace["stageError"] = str(exc)
    finally:
        unmount_result = _unmount_semfs(work_dir=work_dir, container_tag=container_tag)
        semfs_trace["unmount"] = unmount_result
        semfs_trace["unmountDurationMs"] = unmount_result.get("durationMs")

    # Mount gone — clear any orphaned FUSE entry, then restore deliverables into
    # the bare work_dir so the grader's os.path.isfile() checks pass.
    try:
        semfs_trace["forcedUnmount"] = _force_clear_mount(work_dir)
        restored = _restore_outputs_to_workdir(work_dir=work_dir, staged=staged)
        semfs_trace["restoredOutputs"] = restored
        if restored:
            work_abs = os.path.abspath(work_dir)
            result["paths"] = [os.path.join(work_abs, rel) for rel in restored]
    except Exception as exc:
        semfs_trace["restoreError"] = str(exc)

    return _attach_semfs_trace(result, semfs_trace)
