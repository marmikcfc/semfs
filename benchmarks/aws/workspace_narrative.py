#!/usr/bin/env python3
import argparse
import json
from pathlib import Path
from typing import Any, Dict, List, Optional


Json = Any


def load_json(path: Path) -> Json:
    return json.loads(path.read_text(encoding="utf-8"))


def latest_report(output_root: Path) -> Path:
    reports = sorted(output_root.rglob("agent_runner_report.json"), key=lambda p: p.stat().st_mtime)
    if not reports:
        raise SystemExit(f"no agent_runner_report.json found under {output_root}")
    return reports[-1]


def short_text(text: Optional[str], limit: int = 500) -> Optional[str]:
    if not isinstance(text, str):
        return None
    compact = " ".join(text.strip().split())
    if not compact:
        return None
    if len(compact) <= limit:
        return compact
    return compact[: limit - 3] + "..."


def summarize_execution_trace(trace: List[Json]) -> Dict[str, Any]:
    text_events = 0
    tool_events = 0
    first_user = None
    last_assistant = None
    tools: List[Dict[str, Any]] = []

    for item in trace:
        if not isinstance(item, dict):
            continue
        item_type = item.get("type")
        role = item.get("role")
        content = item.get("content")
        if item_type == "text":
            text_events += 1
            if role == "user" and first_user is None:
                first_user = short_text(content, 700)
            if role == "assistant":
                last_assistant = short_text(content, 700)
        elif item_type == "tool":
            tool_events += 1
            tool_name = item.get("tool_name")
            tools.append(
                {
                    "tool": tool_name,
                    "input": item.get("tool_input"),
                    "output": item.get("tool_output"),
                }
            )

    return {
        "textEventCount": text_events,
        "toolEventCount": tool_events,
        "firstUserMessage": first_user,
        "lastAssistantMessage": last_assistant,
        "tools": tools[:50],
    }


def workspace_changes(diff_payload: Dict[str, Any]) -> List[Dict[str, Any]]:
    workspaces = diff_payload.get("workspaces")
    if not isinstance(workspaces, dict):
        return []
    changes = []
    for name, payload in sorted(workspaces.items()):
        if not isinstance(payload, dict):
            continue
        created = int(payload.get("createdCount") or 0)
        deleted = int(payload.get("deletedCount") or 0)
        modified = int(payload.get("modifiedCount") or 0)
        if created == 0 and deleted == 0 and modified == 0:
            continue
        changes.append(
            {
                "workspace": name,
                "createdCount": created,
                "deletedCount": deleted,
                "modifiedCount": modified,
                "createdPaths": payload.get("createdPaths") or [],
                "deletedPaths": payload.get("deletedPaths") or [],
                "modifiedPaths": payload.get("modifiedPaths") or [],
            }
        )
    return changes


def summarize_case(case: Dict[str, Any], report_path: Path) -> Dict[str, Any]:
    output_dir = case.get("outputDir")
    agent_json = {}
    if isinstance(output_dir, str) and output_dir:
        agent_json_path = Path(output_dir) / "agent.json"
        if agent_json_path.exists():
            agent_json = load_json(agent_json_path)
    trace = agent_json.get("trace") if isinstance(agent_json.get("trace"), dict) else {}
    outputs = trace.get("outputs") if isinstance(trace.get("outputs"), dict) else {}
    checks = agent_json.get("checks") if isinstance(agent_json.get("checks"), list) else []
    runner_trace = trace.get("raw", {}).get("runner") if isinstance(trace.get("raw"), dict) else {}
    semfs_trace = runner_trace.get("semfs") if isinstance(runner_trace, dict) and isinstance(runner_trace.get("semfs"), dict) else None
    execution_trace = trace.get("executionTrace") if isinstance(trace.get("executionTrace"), list) else []
    llm = trace.get("llm") if isinstance(trace.get("llm"), dict) else {}
    exec_summary = summarize_execution_trace(execution_trace)

    return {
        "caseId": case.get("caseId"),
        "status": agent_json.get("status") or case.get("status"),
        "durationMs": agent_json.get("durationMs") or case.get("durationMs"),
        "turns": agent_json.get("turns"),
        "promptTokens": agent_json.get("promptTokens"),
        "completionTokens": agent_json.get("completionTokens"),
        "totalTokens": agent_json.get("totalTokens"),
        "workDir": agent_json.get("workDir"),
        "returnedPaths": outputs.get("returnedPaths") if isinstance(outputs.get("returnedPaths"), list) else [],
        "outputManifest": outputs.get("outputManifest") if isinstance(outputs.get("outputManifest"), list) else [],
        "retrievalMethod": outputs.get("retrievalMethod") if isinstance(outputs.get("retrievalMethod"), list) else [],
        "checks": checks,
        "llm": {
            "provider": llm.get("provider"),
            "baseUrl": llm.get("baseUrl"),
            "model": llm.get("model"),
            "usageTotal": llm.get("usageTotal") if isinstance(llm.get("usageTotal"), dict) else {},
        },
        "executionSummary": exec_summary,
        "semfs": {
            "enabled": bool(semfs_trace),
            "mountDurationMs": semfs_trace.get("mountDurationMs") if isinstance(semfs_trace, dict) else None,
            "unmountDurationMs": semfs_trace.get("unmountDurationMs") if isinstance(semfs_trace, dict) else None,
            "containerTag": semfs_trace.get("containerTag") if isinstance(semfs_trace, dict) else None,
            "mountExitCode": ((semfs_trace.get("mount") or {}).get("exitCode") if isinstance(semfs_trace, dict) and isinstance(semfs_trace.get("mount"), dict) else None),
            "unmountExitCode": ((semfs_trace.get("unmount") or {}).get("exitCode") if isinstance(semfs_trace, dict) and isinstance(semfs_trace.get("unmount"), dict) else None),
        },
        "paths": {
            "reportPath": str(report_path),
            "outputDir": output_dir,
            "agentJson": str(Path(output_dir) / "agent.json") if isinstance(output_dir, str) and output_dir else None,
        },
    }


def build_narrative(report_path: Path, telemetry_dir: Path) -> Dict[str, Any]:
    report = load_json(report_path)
    cases = report.get("cases") if isinstance(report.get("cases"), list) else []
    config = report.get("config") if isinstance(report.get("config"), dict) else {}
    api_provider = config.get("api_provider") if isinstance(config.get("api_provider"), dict) else {}

    prepare_diff_path = telemetry_dir / "diff_prepare.json"
    run_diff_path = telemetry_dir / "diff_run.json"
    prepare_diff = load_json(prepare_diff_path) if prepare_diff_path.exists() else {}
    run_diff = load_json(run_diff_path) if run_diff_path.exists() else {}

    summarized_cases = [summarize_case(case, report_path) for case in cases if isinstance(case, dict)]

    return {
        "agentId": report.get("agentId"),
        "model": {
            "modelName": config.get("model_name"),
            "providerType": api_provider.get("provider_type"),
            "modelId": api_provider.get("model"),
            "baseUrl": api_provider.get("baseUrl"),
        },
        "run": {
            "startedAt": report.get("startedAt"),
            "finishedAt": report.get("finishedAt"),
            "totalDurationMs": report.get("totalDurationMs"),
            "runsRoot": report.get("runsRoot"),
            "reportPath": str(report_path),
            "telemetryDir": str(telemetry_dir),
        },
        "summary": report.get("summary") if isinstance(report.get("summary"), dict) else {},
        "workspaceTelemetry": {
            "prepare": {
                "path": str(prepare_diff_path),
                "summary": prepare_diff.get("summary") if isinstance(prepare_diff, dict) else {},
                "changedWorkspaces": workspace_changes(prepare_diff if isinstance(prepare_diff, dict) else {}),
            },
            "run": {
                "path": str(run_diff_path),
                "summary": run_diff.get("summary") if isinstance(run_diff, dict) else {},
                "changedWorkspaces": workspace_changes(run_diff if isinstance(run_diff, dict) else {}),
            },
        },
        "cases": summarized_cases,
    }


def narrative_markdown(narrative: Dict[str, Any]) -> str:
    lines: List[str] = []
    agent_id = narrative.get("agentId")
    model = narrative.get("model") if isinstance(narrative.get("model"), dict) else {}
    summary = narrative.get("summary") if isinstance(narrative.get("summary"), dict) else {}
    telemetry = narrative.get("workspaceTelemetry") if isinstance(narrative.get("workspaceTelemetry"), dict) else {}

    lines.append(f"# {agent_id} Run Narrative")
    lines.append("")
    lines.append(f"- Model: `{model.get('modelId')}`")
    lines.append(f"- Provider: `{model.get('providerType')}`")
    lines.append(f"- Started: `{narrative.get('run', {}).get('startedAt')}`")
    lines.append(f"- Finished: `{narrative.get('run', {}).get('finishedAt')}`")
    lines.append(f"- Status summary: `{summary}`")
    lines.append("")

    for phase_name in ("prepare", "run"):
        phase = telemetry.get(phase_name) if isinstance(telemetry.get(phase_name), dict) else {}
        lines.append(f"## Workspace {phase_name.capitalize()} Phase")
        lines.append("")
        lines.append(f"- Summary: `{phase.get('summary')}`")
        changed = phase.get("changedWorkspaces") if isinstance(phase.get("changedWorkspaces"), list) else []
        if not changed:
            lines.append("- Changed workspaces: none")
        else:
            lines.append("- Changed workspaces:")
            for item in changed[:10]:
                lines.append(
                    f"  - `{item.get('workspace')}` created={item.get('createdCount')} modified={item.get('modifiedCount')} deleted={item.get('deletedCount')}"
                )
        lines.append("")

    cases = narrative.get("cases") if isinstance(narrative.get("cases"), list) else []
    for case in cases:
        if not isinstance(case, dict):
            continue
        lines.append(f"## Case {case.get('caseId')}")
        lines.append("")
        lines.append(f"- Status: `{case.get('status')}`")
        lines.append(f"- Duration: `{case.get('durationMs')} ms`")
        lines.append(
            f"- Tokens: `prompt={case.get('promptTokens')} completion={case.get('completionTokens')} total={case.get('totalTokens')}`"
        )
        lines.append(f"- Workdir: `{case.get('workDir')}`")
        returned_paths = case.get("returnedPaths") if isinstance(case.get("returnedPaths"), list) else []
        lines.append(f"- Returned paths: `{returned_paths}`")
        checks = case.get("checks") if isinstance(case.get("checks"), list) else []
        if checks:
            lines.append("- Checks:")
            for check in checks:
                if not isinstance(check, dict):
                    continue
                lines.append(f"  - `{check.get('type')}` passed={check.get('passed')} detail={short_text(check.get('detail'), 200)}")
        exec_summary = case.get("executionSummary") if isinstance(case.get("executionSummary"), dict) else {}
        lines.append(
            f"- Execution trace: textEvents={exec_summary.get('textEventCount')} toolEvents={exec_summary.get('toolEventCount')}"
        )
        if exec_summary.get("lastAssistantMessage"):
            lines.append(f"- Last assistant message: {exec_summary.get('lastAssistantMessage')}")
        semfs = case.get("semfs") if isinstance(case.get("semfs"), dict) else {}
        if semfs.get("enabled"):
            lines.append(
                f"- SEMFS: mount={semfs.get('mountDurationMs')}ms unmount={semfs.get('unmountDurationMs')}ms container={semfs.get('containerTag')}"
            )
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def write_outputs(output_prefix: Path, narrative: Dict[str, Any]) -> None:
    output_prefix.parent.mkdir(parents=True, exist_ok=True)
    output_prefix.with_suffix(".json").write_text(json.dumps(narrative, indent=2) + "\n", encoding="utf-8")
    output_prefix.with_suffix(".md").write_text(narrative_markdown(narrative), encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate a readable run narrative from Workspace-Bench outputs.")
    parser.add_argument("--output-root", required=True)
    parser.add_argument("--telemetry-dir", required=True)
    parser.add_argument("--output-prefix", required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    report_path = latest_report(Path(args.output_root))
    telemetry_dir = Path(args.telemetry_dir)
    narrative = build_narrative(report_path, telemetry_dir)
    write_outputs(Path(args.output_prefix), narrative)


if __name__ == "__main__":
    main()
