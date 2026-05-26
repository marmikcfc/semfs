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

# Event types in Codex's JSONL output stream that represent agent actions.
_TOOL_EVENT_TYPES = {
    "local_shell_call",
    "local_shell_call_output",
    "function_call",
    "function_call_output",
    "code_cell_execution",
    "code_cell_output",
}


def _truthy(value: Optional[str], default: bool = False) -> bool:
    if value is None:
        return default
    return value.strip().lower() in {"1", "true", "yes", "y", "on"}


def _load_codex_run():
    codex_path = Path(__file__).with_name("codex.py")
    spec = importlib.util.spec_from_file_location("workspace_bench_codex", codex_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load codex harness from {codex_path}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    run_fn = getattr(mod, "run", None)
    if not callable(run_fn):
        raise RuntimeError("codex harness missing run()")
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


# This adapter is the Codex harness (semfsclaudecode.py uses "claudecode").
_AGENT_KEY = "codex"


def _model_slug(api_provider: Dict[str, Json]) -> str:
    if isinstance(api_provider, dict):
        for k in ("model_name", "model"):
            v = api_provider.get(k)
            if isinstance(v, str) and v.strip():
                return v
    return "model"


def _container_tag(api_provider: Dict[str, Json]) -> str:
    """Per-agent, persistent container: one space per (harness, model), stable
    across cases/personas so the cache's fs_remote accumulates and each file
    embeds once instead of re-embedding the workspace every run.

    Override with SEMFS_CONTAINER_TAG for full manual control (e.g. per-case).
    """
    explicit = os.environ.get("SEMFS_CONTAINER_TAG")
    if explicit:
        return _safe_tag(explicit)
    prefix = os.environ.get("SEMFS_CONTAINER_PREFIX", "workspace-bench")
    return _safe_tag(f"{prefix}-{_AGENT_KEY}-{_model_slug(api_provider)}")


def _case_memory_paths(work_dir: str, sandbox_dir: str) -> Optional[str]:
    """Scope semfs memory/embedding to ONLY the files this case needs, so we don't
    embed the entire ~3,800-file workspace.

    The case's `metadata.json` (written to sandbox_dir by the runner) declares the
    needed inputs in `data_manifest[*].filename` and `file_dep_graph[*].from`. We
    match those by basename against the (pre-mount) workdir and return a
    comma-separated list of workdir-relative paths for `--memory-paths`. Output
    files (model_output/) are never in the inputs, so they are excluded.

    Precedence: explicit SEMFS_MEMORY_PATHS env wins; then the case manifest; else
    None (no scoping -> semfs default).
    """
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


def _read_raw_file(raw_dir: str, candidates: List[str]) -> str:
    """Read the first existing file from raw_dir, return empty string if none found."""
    if not raw_dir:
        return ""
    for name in candidates:
        try:
            return Path(os.path.join(raw_dir, name)).read_text(encoding="utf-8")
        except Exception:
            pass
    return ""


def _parse_codex_events(stdout_text: str) -> List[Dict[str, Json]]:
    """Parse the JSONL event stream emitted by the Codex runner."""
    events = []
    for raw_line in stdout_text.splitlines():
        line = raw_line.strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            events.append({"type": "_unparsed", "raw": line})
    return events


def _summarize_codex_events(
    events: List[Dict[str, Json]],
    execution_trace: Optional[List[Dict[str, Json]]] = None,
) -> Dict[str, Json]:
    """
    Produce a structured summary of what the Codex agent did:
    - which tool calls (shell/function/code) were invoked and what they returned
    - assistant text messages
    - any errors
    Covers both the raw JSONL event stream and the executionTrace (structured
    conversation log with per-step LLM metadata).
    """
    tool_calls: List[Dict[str, Json]] = []
    turns: List[Dict[str, Json]] = []
    agent_messages: List[str] = []
    errors: List[Dict[str, Json]] = []
    items_by_id: Dict[str, Dict[str, Json]] = {}
    current_turn: Optional[Dict[str, Json]] = None
    usage: Optional[Dict[str, Json]] = None

    for event in events:
        etype = event.get("type", "")

        if etype == "turn.started":
            current_turn = {"toolCalls": [], "errors": []}

        elif etype == "turn.completed":
            usage = event.get("usage")
            if current_turn is not None:
                current_turn["usage"] = usage
                turns.append(current_turn)
            current_turn = None

        elif etype == "item.started":
            item = event.get("item") or {}
            iid = item.get("id")
            if iid:
                items_by_id[iid] = item

        elif etype == "item.completed":
            item = event.get("item") or {}
            iid = item.get("id")
            itype = item.get("type", "")

            if itype == "agent_message":
                agent_messages.append(item.get("text", ""))

            elif itype in _TOOL_EVENT_TYPES or "shell" in itype or "call" in itype:
                # Capture shell/function/code tool calls
                entry: Dict[str, Json] = {
                    "id": iid,
                    "type": itype,
                    "call": item.get("call") or item.get("action") or item.get("command"),
                    "output": item.get("output"),
                    "exitCode": item.get("exit_code"),
                    "error": item.get("error"),
                }
                tool_calls.append(entry)
                if current_turn is not None:
                    current_turn["toolCalls"].append(entry)

            elif itype == "error" or item.get("error"):
                err = {"id": iid, "type": itype, "detail": item.get("error") or item}
                errors.append(err)
                if current_turn is not None:
                    current_turn["errors"].append(err)

        elif etype == "error":
            err = {"type": "stream_error", "detail": event}
            errors.append(err)

    # Build the summary
    item_types_seen = sorted({
        (e.get("item") or {}).get("type", "")
        for e in events
        if e.get("type") in ("item.started", "item.completed")
        and (e.get("item") or {}).get("type")
    })

    # Parse executionTrace for tool-use steps and LLM call metadata.
    # Each entry has: type ("text"/"tool_use"/"function_call"/etc),
    # role ("user"/"assistant"), content, turn, llm (provider/usage/stopReason).
    exec_tool_calls: List[Dict[str, Json]] = []
    exec_assistant_messages: List[Dict[str, Json]] = []
    for step in (execution_trace or []):
        stype = step.get("type", "")
        role = step.get("role", "")
        if stype in ("tool_use", "function_call", "local_shell_call", "code_cell_execution"):
            exec_tool_calls.append({
                "turn": step.get("turn"),
                "type": stype,
                "name": step.get("name") or step.get("function", {}).get("name"),
                "input": step.get("input") or step.get("arguments") or step.get("action"),
                "output": step.get("output"),
                "exitCode": step.get("exit_code"),
                "error": step.get("error"),
                "llm": step.get("llm"),
            })
        elif role == "assistant" and stype == "text":
            llm = step.get("llm") or {}
            exec_assistant_messages.append({
                "turn": step.get("turn"),
                "content": step.get("content", ""),
                "stopReason": llm.get("stopReason"),
                "errorMessage": llm.get("errorMessage"),
                "usage": (llm.get("usage") or {}),
            })

    all_tool_calls = tool_calls + exec_tool_calls
    no_tools = not all_tool_calls
    note_parts = []
    if no_tools:
        note_parts.append("No tool calls were made — model produced text/plan only")
    else:
        note_parts.append(f"{len(all_tool_calls)} tool call(s) recorded")
    if exec_assistant_messages:
        last = exec_assistant_messages[-1]
        if last.get("stopReason") not in ("stop", "end_turn", None):
            note_parts.append(f"last stopReason={last['stopReason']!r}")
        if last.get("errorMessage"):
            note_parts.append(f"LLM error={last['errorMessage']!r}")

    return {
        "totalTurns": len(turns),
        "totalToolCalls": len(all_tool_calls),
        "toolCallsMade": not no_tools,
        "agentMessages": agent_messages,
        "itemTypesSeen": item_types_seen,
        "toolCalls": all_tool_calls,
        "errors": errors,
        "usagePerTurn": [t.get("usage") for t in turns],
        "rawEventCount": len(events),
        "executionSteps": {
            "toolCalls": exec_tool_calls,
            "assistantMessages": exec_assistant_messages,
        },
        "note": " | ".join(note_parts) if note_parts else "ok",
    }


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
    """Pull the agent's final path list out of free text.

    Mirrors Workspace-Bench's deepagent: take the LAST bracketed Python list
    (models often wrap it in prose) and literal-eval it to a list[str].
    """
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
    """While the semfs mount is still live, copy the run's deliverables OUT of the
    mount into a staging dir under ``sandbox_dir`` (which is *not* under the mount).

    Returns ``[(rel_path, staged_abs_path), ...]``. The grader inspects ``work_dir``
    after unmount; without this the files vanish with the mount.

    Discovery mirrors agent_runner._collect_output_paths: the union of
      (1) paths the agent listed in its final message (trace.lastText),
      (2) any result["paths"] entries, and
      (3) the output subtree(s) implied by (1) (e.g. ``model_output/``),
      (4) expected output filenames.
    We only walk the implied subtrees — never the whole mount.
    """
    work_abs = os.path.abspath(work_dir)
    trace = result.get("trace") if isinstance(result.get("trace"), dict) else {}
    last_text = str(trace.get("lastText") or "")

    rel_files: List[str] = []  # explicit file candidates, work_dir-relative
    subtree_dirs: set[str] = set()

    def _note_rel(rel: Optional[str]) -> None:
        if not rel:
            return
        rel_files.append(rel)
        head = rel.split("/", 1)[0]
        if head and head != rel:  # rel had a directory component
            subtree_dirs.add(head)

    for rp in _extract_returned_paths(last_text):
        _note_rel(_rel_under(work_abs, rp))
    for rp in result.get("paths") if isinstance(result.get("paths"), list) else []:
        if isinstance(rp, str):
            _note_rel(_rel_under(work_abs, rp))
    expected = api_provider.get("__expected_output_files__")
    expected_basenames = set()
    if isinstance(expected, list):
        for ef in expected:
            if not isinstance(ef, str) or not ef:
                continue
            if "/" in ef.strip("/"):
                _note_rel(_rel_under(work_abs, ef))
            else:
                expected_basenames.add(ef)

    # Build the ordered, de-duplicated set of relative files that exist in the mount.
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
    # Walk only the implied output subtrees (bounded), capturing files the agent
    # wrote but did not list, including any expected-basename matches.
    for sub in sorted(subtree_dirs):
        sub_abs = os.path.join(work_abs, sub)
        if not os.path.isdir(sub_abs):
            continue
        for root, _dirs, files in os.walk(sub_abs):
            for fname in files:
                ap = os.path.join(root, fname)
                rel = os.path.relpath(ap, work_abs).replace("\\", "/")
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
        # ENOTCONN (107) = "Transport endpoint is not connected" = orphaned FUSE mount.
        return exc.errno in (107,)


def _force_clear_mount(work_dir: str) -> bool:
    """semfs unmount can leave an orphaned kernel FUSE entry (daemon gone, mount
    still registered → ENOTCONN). Restore would write into a dead mount, so clear
    it first. Returns True if a clear was attempted and the path is now usable."""
    if not _path_is_dead_or_mounted(work_dir):
        return False
    for cmd in (["fusermount3", "-u", work_dir], ["fusermount", "-u", work_dir], ["umount", work_dir]):
        try:
            subprocess.run(cmd, capture_output=True, text=True, timeout=30, check=False)
        except Exception:
            continue
        if not _path_is_dead_or_mounted(work_dir):
            return True
    return not _path_is_dead_or_mounted(work_dir)


def _restore_outputs_to_workdir(
    *, work_dir: str, staged: List[Tuple[str, str]]
) -> List[str]:
    """After unmount, copy staged files back into the (now bare) work_dir at their
    original relative paths. Returns the rel paths actually restored."""
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
            # e.g. work_dir is a dead FUSE mountpoint (ENOTCONN) — skip, don't crash.
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
    codex_run = _load_codex_run()
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
            "trace": {"runner": "SEMFSCodex", "rawDir": os.path.join(sandbox_dir, "raw"), "semfs": semfs_trace},
            "metrics": {"turns": None, "promptTokens": None, "completionTokens": None, "totalTokens": None},
            "durationMs": int(mount_result.get("durationMs") or 0),
        }

    result: Dict[str, Json]
    staged: List[Tuple[str, str]] = []
    try:
        result = codex_run(
            prompt=prompt,
            work_dir=work_dir,
            sandbox_dir=sandbox_dir,
            timeout_s=timeout_s,
            api_provider=api_provider,
            agent_id=agent_id,
        )
        # Mount is still live here: rescue the deliverables out of it before the
        # finally-unmount tears them down. Fail-open — a staging error must not
        # crash the run (it degrades to the pre-fix behavior).
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

    # Mount is gone — work_dir should now be the bare underlying dir the grader
    # inspects. But semfs unmount can leave an orphaned kernel FUSE entry (ENOTCONN);
    # clear it before restoring, or every copy fails into a dead mount.
    try:
        semfs_trace["forcedUnmount"] = _force_clear_mount(work_dir)
        restored = _restore_outputs_to_workdir(work_dir=work_dir, staged=staged)
        semfs_trace["restoredOutputs"] = restored
        if restored:
            work_abs = os.path.abspath(work_dir)
            result["paths"] = [os.path.join(work_abs, rel) for rel in restored]
    except Exception as exc:
        semfs_trace["restoreError"] = str(exc)

    # Parse the Codex JSONL event stream so tool calls and errors are visible
    # in the trace without digging into the raw escaped stdout string.
    # codex.py writes stdout to raw_dir files rather than returning it in the
    # result dict, so we read from raw_dir/codex_stdout.jsonl.
    try:
        runner_trace = result.get("trace") or {}
        raw_dir = runner_trace.get("rawDir") or ""
        stdout_text = _read_raw_file(raw_dir, ["codex_stdout.jsonl", "runner_stdout.txt", "stdout.txt"])
        stderr_text = _read_raw_file(raw_dir, ["runner_stderr.txt", "stderr.txt"])
        events = _parse_codex_events(stdout_text)
        execution_trace = runner_trace.get("executionTrace") or []
        semfs_trace["codexEventSummary"] = _summarize_codex_events(
            events, execution_trace=execution_trace
        )
        if stderr_text.strip():
            semfs_trace["codexStderr"] = stderr_text
    except Exception as exc:
        semfs_trace["codexEventSummaryError"] = str(exc)

    return _attach_semfs_trace(result, semfs_trace)
