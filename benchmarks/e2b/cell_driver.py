#!/usr/bin/env python3
"""Per-cell runner for the E2B WB-PM matrix. Uploaded to ~/cell_driver.py at runtime
(overrides the older baked /opt/cell_driver.py).

Patched vs the baked driver (see tickets/workspace-bench-5arm-matrix/E2B_RUNBOOK.md):
  1. Claude ALWAYS goes via OpenRouter (user requirement) with the Anthropic-shaped
     endpoint  https://openrouter.ai/api  (NOT /api/v1 — runbook §6); no native attempt.
  2. Codex stays native ChatGPT-subscription (CODEX_USE_CHATGPT=1, bare model id),
     OpenRouter fallback on auth/limit failure.
  3. Claude semfs arms get the parity env (SEMFS_MOUNT_PATH / WB_READ_PATHS / shim vars)
     so the patched ClaudeCode.js re-enables mount access + the grep shim even though
     cwd is OUTSIDE the mount (write-outside design). RCA 2026-06-13 (parity).

Usage:  python3 cell_driver.py --label L --agent {claude|codex} --case N --arm {plain|nokg|nokgAK|best|hiddenkg|hiddenkg_edges}
Emits one line:  RESULT=<json>
"""
import json, os, time, importlib.util, argparse

ap = argparse.ArgumentParser()
for k in ("label", "agent", "case", "arm"):
    ap.add_argument("--" + k, required=True)
a = ap.parse_args()

HARN = {"claude": "/home/user/wb/evaluation/src/agents/claudecode.py",
        "codex":  "/home/user/wb/evaluation/src/agents/codex.py"}[a.agent]
spec = importlib.util.spec_from_file_location("h", HARN)
h = importlib.util.module_from_spec(spec); spec.loader.exec_module(h)

ORKEY = os.environ.get("OPENROUTER_API_KEY", "")
WS = "/home/user/ws/mnt"          # semfs mount (semfs arms)
PLAIN = "/home/user/ws/plain"     # raw corpus tree (plain arm)

SEMFS_HINT = (
    f"The workspace is the directory {WS}/. To find anything, search by MEANING — a plain\n"
    f'    grep "<2-4 key terms>"\n'
    "returns ranked excerpts naming WHICH file + its content (top = best match). `# ^ COMPLETE FILE` = that "
    "file's entire content; `# ^ TRUNCATED` = open that file with cat/sed for the rest. "
    "COST: a broad crawl (find/os.walk over the tree) or opening many files costs far more context than a focused "
    "search plus the few reads you actually need. Do NOT repeat a search you already ran — its results are already in your context."
)
# KG-on arm only: point the agent at the knowledge-graph overlay (built from the
# code AST graph's Louvain communities). The `nokg` arm omits this line entirely.
SEMFS_KG_LINE = (
    f"A KNOWLEDGE-GRAPH overlay is mounted at {WS}/kg/ — it groups the workspace into topic "
    "communities (each dir = a community of related files, named by its hub/'god-node' file). "
    f"For whole-workspace orientation (e.g. 'what areas exist', 'which files relate to X'), browse {WS}/kg/ "
    "and read its KNOWLEDGE_GRAPH.md; for a specific lookup go straight to semfs grep."
)

WD = f"/home/user/run/{a.label}"
os.makedirs(WD, exist_ok=True)
os.makedirs(f"{WD}/model_output", exist_ok=True)

if a.arm == "plain":
    note = (f"Your working directory is {WD}. The workspace to analyze is the directory tree at {PLAIN} "
            "(explore it with `find`, `grep -r`, `ls`, `cat`).")
    # Claude parity: widen read access to the plain tree (writes stay in cwd).
    if a.agent == "claude":
        os.environ["WB_READ_PATHS"] = PLAIN
        os.environ.pop("SEMFS_MOUNT_PATH", None)
elif a.arm == "cloud":
    # Supermemory cloud backend — server-side search against the live container.
    # No local mount/seed; the real SUPERMEMORY_API_KEY (+ optional _URL) is injected
    # by the orchestrator into the cell env (NOT the dummy-local the semfs arms use).
    CLOUD_TAG = "workspace-bench-chanpin"
    note = (f"Your working directory is {WD}. There is NO local file tree. To FIND content, use the "
            f"Supermemory cloud semantic index:\n"
            f'    semfs grep "<2-4 key terms>" --tag {CLOUD_TAG}\n'
            "It returns ranked excerpts naming WHICH file + its content (top = best match). Rely on those "
            "results — there are no local files to open. Do NOT repeat a search you already ran; its "
            "results are already in your context.")
    os.environ.update({
        "SEMFS_STORAGE_BACKEND": "cloud", "SEMFS_RESULT_LIMIT": "5",
        "SEMFS_GREP_RESULT_CAP": "6144", "SEMFS_GREP_TOTAL_CAP": "10240", "SEMFS_REWRITE": "1",
    })
    if a.agent == "claude":
        os.environ["SEMFS_BIN"] = "/usr/local/bin/semfs"
        os.environ["SEMFS_REAL_HOME"] = "/home/user"
        os.environ["SEMFS_SHIM_DIR"] = "/home/user/semfs-shims"
        os.environ.setdefault("SEMFS_REAL_RG", os.environ.get("WB_REAL_RG", "rg"))
else:
    # semfs arms:
    #   kg        = surfaced KG
    #   nokg      = no KG surface, no co-mention
    #   nokgAK    = nokg + adaptive-K
    #   best           = best_exp0002 knobs + no KG surface + no hidden KG
    #   hiddenkg       = best_exp0002 knobs + hidden internal KG on, no surface
    #   hiddenkg_edges = best_exp0002 knobs + old co-mention-only proxy, no surface
    note = f"Your working directory is {WD}.\n{SEMFS_HINT}"
    if a.arm == "kg":
        note += "\n" + SEMFS_KG_LINE
    os.environ.update({
        "SEMFS_EMBED_MODEL": "gemma-q4", "SEMFS_EMBED_ONNX_DIR": "/home/user/gemma_q4",
        "SUPERMEMORY_API_KEY": "dummy-local", "SEMFS_NO_PUSH": "1", "SEMFS_NO_SYNC": "1",
        # Keep this aligned with the mount-side env; do not re-force search-only.
        "SEMFS_SEARCH_ONLY": "off",
    })
    if a.arm in {"nokg", "nokgAK", "best"}:
        os.environ["SEMFS_KG"] = "off"
        os.environ["SEMFS_COMENTION"] = "off"
        os.environ["SEMFS_HIDDEN_KG"] = "off"
        os.environ["SEMFS_GRAPH_FS"] = "off"
    elif a.arm == "hiddenkg":
        os.environ["SEMFS_KG"] = "off"
        os.environ["SEMFS_COMENTION"] = "off"
        os.environ["SEMFS_HIDDEN_KG"] = "on"
        os.environ["SEMFS_GRAPH_FS"] = "off"
    elif a.arm == "hiddenkg_edges":
        os.environ["SEMFS_KG"] = "off"
        os.environ["SEMFS_COMENTION"] = "on"
        os.environ["SEMFS_HIDDEN_KG"] = "off"
        os.environ["SEMFS_GRAPH_FS"] = "off"
    elif a.arm == "kg":
        os.environ["SEMFS_KG"] = "on"
        os.environ["SEMFS_COMENTION"] = "on"
        os.environ["SEMFS_HIDDEN_KG"] = "off"
    # Tunable knobs — respect an external override (run_matrix --knobs sweep) over the
    # default, so optimization can vary them without editing this file.
    for _k, _v in (("SEMFS_RESULT_LIMIT", "5"), ("SEMFS_GREP_RESULT_CAP", "6144"),
                   ("SEMFS_GREP_TOTAL_CAP", "10240"), ("SEMFS_REWRITE", "1")):
        os.environ.setdefault(_k, _v)
    if a.arm == "nokgAK":
        os.environ["SEMFS_ADAPTIVE_K"] = "on"
    # semfs kit + mount read access — for BOTH agents now (Claude got this via the
    # RCA 2026-06-13 parity fix; codex now gets it too). With the shim dir first on
    # PATH, codex's NATURAL `grep "terms"` routes to semantic: SEMFS_MOUNT_PATH lets
    # a path-less grep scope to the mount even though cwd stays writable (outside it).
    os.environ["SEMFS_MOUNT_PATH"] = WS
    os.environ["WB_READ_PATHS"] = WS
    os.environ["SEMFS_BIN"] = "/usr/local/bin/semfs"
    os.environ["SEMFS_REAL_HOME"] = "/home/user"
    os.environ["SEMFS_REAL_GREP"] = "/usr/bin/grep"
    os.environ["SEMFS_SHIM_DIR"] = "/home/user/semfs-shims"
    os.environ.setdefault("SEMFS_REAL_RG", os.environ.get("WB_REAL_RG", "rg"))
    # Put the shim dir FIRST on PATH so a bare `grep`/`rg` resolves to the shim.
    # (codex runs its Bash tool non-interactively and inherits this process env.)
    os.environ["PATH"] = "/opt/semfs-shims:" + os.environ.get(
        "PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin")

task = open(f"/home/user/cases/{a.case}.task", encoding="utf-8").read().strip()
# Inject the EXPECTED output filename(s) the judge grades against. Upstream WB never tells
# the agent these (they live judge-side in metadata `output_files`), so a correct deliverable
# under a different name scores 0 — the "filename lottery". Telling the agent the exact name
# (a) removes that noise and (b) turns it into a real instruction-following test (told the
# name → did it comply?). Applied to ALL arms, so the relative comparison stays fair.
EXPECTED_FILES = [f.strip() for f in os.environ.get("WB_OUTPUT_FILES", "").split(",") if f.strip()]
fname_hint = ""
if EXPECTED_FILES:
    names = ", ".join(f"model_output/{f}" for f in EXPECTED_FILES)
    fname_hint = (f"\n[REQUIRED OUTPUT FILENAME] Your graded deliverable MUST be saved with EXACTLY "
                  f"this name (put ALL required content inside it): {names}. Use this exact filename "
                  "(including extension); do not split it or rename it.")
# Optional turn-brake (WB_TURNBRAKE env, set via --knobs): cut re-reads/over-exploration
# WITHOUT cutting retrieval — attacks the cache-read tax from the turn-count side.
turnbrake = ("\n" + os.environ["WB_TURNBRAKE"]) if os.environ.get("WB_TURNBRAKE") else ""
# Optional WORKSPACE MAP (WB_WORKSPACE_MAP=<file>): a cached, always-present index of the
# workspace (dirs + topic clusters + key entities) so the agent navigates to the right place
# itself instead of relying on ranked retrieval. Placed as a STABLE PREFIX (after the system
# note, before the per-case task) so the prefix cache keeps it ~free across a persona's cells.
wsmap = ""
_mapf = os.environ.get("WB_WORKSPACE_MAP", "")
if _mapf and os.path.exists(_mapf):
    _mp = open(_mapf, encoding="utf-8").read().strip()
    if _mp:
        wsmap = ("[WORKSPACE MAP] An index of this workspace is below — directories (file counts, "
                 "types, sample files) and topic clusters (label + key entities). USE IT FIRST to "
                 "decide which directory/files hold the answer, then go straight there (grep scoped "
                 "to that dir, or cat the file) rather than broad searching.\n" + _mp + "\n\n")
wrapped = (f"{note}\n\n{wsmap}{task}\n[Note] Save deliverables under ./model_output/ (relative to your "
           "working directory) and end by printing the file paths as a Python list." + fname_hint + turnbrake)


def run_agent(use_openrouter):
    sd = f"/tmp/sbx_{a.label}_{'or' if use_openrouter else 'nat'}"
    os.makedirs(sd, exist_ok=True)
    if a.agent == "codex":
        # native = ChatGPT subscription; fallback = OpenRouter
        os.environ["CODEX_SANDBOX_MODE"] = "danger-full-access"
        if os.environ.get("WB_MODAL_GLM") == "1":
            # Modal self-hosted GLM-5.1 (vLLM, 8xH200): point codex straight at the vLLM endpoint.
            # The WB codex harness auto-starts its local responses->chat adapter for non-"gpt-"
            # models AND drops the type:"namespace" multi_agent tool — so neither the LiteLLM proxy
            # nor `codex exec --disable multi_agent` is needed (both handled in-harness).
            os.environ.pop("CODEX_USE_CHATGPT", None)
            _mk = os.environ.get("MODAL_VLLM_API_KEY", "")
            os.environ["CODEX_API_KEY"] = _mk
            ap_ = {"baseUrl": (os.environ.get("WB_MODAL_BASE")
                               or "https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run/v1"),
                   "apiKey": _mk,
                   "model": os.environ.get("WB_MODAL_MODEL") or "glm-5.1-nvfp4"}
        else:
            os.environ["CODEX_API_KEY"] = ORKEY
            if not use_openrouter:
                os.environ["CODEX_USE_CHATGPT"] = "1"
                ap_ = {"model": "gpt-5.5"}
            else:
                os.environ.pop("CODEX_USE_CHATGPT", None)
                # OpenRouter model is env-overridable (WB_OR_MODEL) so the optimization campaign
                # can pin a model (e.g. z-ai/glm-5.1) without editing this file. Empty → default.
                ap_ = {"baseUrl": "https://openrouter.ai/api/v1", "apiKey": ORKEY,
                       "model": os.environ.get("WB_OR_MODEL") or "openai/gpt-5.4"}
    else:
        # Claude: native Claude Code subscription FIRST (CLAUDE_CODE_OAUTH_TOKEN injected by
        # the orchestrator); OpenRouter only as fallback. Native = free (subscription) + the
        # reliable path (runbook §6); the harness strips ANTHROPIC_* so the OAuth wins.
        if not use_openrouter:
            os.environ["USE_CLAUDE_LONG_RUNNING_TOKEN"] = "1"
            os.environ["CLAUDE_OAUTH_MODEL"] = "claude-sonnet-4-6"
            ap_ = {"model": "anthropic/claude-sonnet-4.6"}
        else:
            os.environ.pop("USE_CLAUDE_LONG_RUNNING_TOKEN", None)
            ap_ = {"provider_type": "anthropic", "baseUrl": "https://openrouter.ai/api",
                   "apiKey": ORKEY, "model": "anthropic/claude-sonnet-4.6"}
    agent_timeout = int(os.environ.get("WB_AGENT_TIMEOUT") or 1500)  # raise for grep-heavy/compress runs
    return h.run(prompt=wrapped, work_dir=WD, sandbox_dir=sd, timeout_s=agent_timeout, api_provider=ap_)


def bad(r):
    ut = (r.get("trace", {}) or {}).get("usageTotal", {}) or {}
    tk = ut.get("total_tokens") or ut.get("prompt_tokens") or 0
    e = str(r.get("errorMessage") or "").lower()
    return (r.get("status") != "ok" or tk == 0 or
            any(s in e for s in ("401", "403", "429", "rate", "limit", "unauthorized",
                                 "invalid_grant", "overloaded", "quota")))


t0 = time.time()
# Both agents: NATIVE subscription first (codex=ChatGPT, claude=Claude OAuth) → OpenRouter
# fallback only if native fails. Keeps the big agent spend on subscriptions, not OpenRouter.
# WB_FORCE_OPENROUTER=1 skips the native attempt and runs OpenRouter directly
# (explicit OpenRouter A/B; avoids the native→fallback two-attempt path).
if os.environ.get("WB_MODAL_GLM") == "1":
    res = run_agent(use_openrouter=False); auth = "modal-glm51"   # codex branch routes to the vLLM endpoint
elif os.environ.get("WB_FORCE_OPENROUTER") == "1" and ORKEY:
    res = run_agent(use_openrouter=True); auth = "openrouter(forced)"
else:
    res = run_agent(use_openrouter=False)
    auth = "native(chatgpt)" if a.agent == "codex" else "native(claude-oauth)"
    if bad(res) and ORKEY:
        res = run_agent(use_openrouter=True); auth = "openrouter(fallback)"

wall = int(time.time() - t0)
tr = res.get("trace", {}) or {}
ut = tr.get("usageTotal", {}) or {}
et = tr.get("executionTrace", []) or []
calls = sum(1 for e in et if e.get("type") == "tool")
cmds = [str((e.get("input") or {}).get("command") or (e.get("input") or {}).get("cmd") or "")[:200]
        for e in et if e.get("type") == "tool"]
used = any("semfs grep" in c for c in cmds)
deliv = {}
mo = f"{WD}/model_output"
if os.path.isdir(mo):
    for f in os.listdir(mo):
        p = os.path.join(mo, f)
        if os.path.isfile(p):
            deliv[f] = open(p, encoding="utf-8", errors="replace").read()[:2500]

# Write agent.json — the WB trace format `agent_eval.py` reads NATIVELY (trace.executionTrace
# WITH tool outputs = the file-read evidence the source-grounded rubrics need). This is the
# fix for the judge-starvation bug: past runs saved only command strings.
try:
    open(f"{WD}/agent.json", "w", encoding="utf-8").write(
        json.dumps({"trace": {"executionTrace": et, "usageTotal": ut}}, ensure_ascii=False))
except Exception:
    pass

# Instruction-following signal: did the agent save the EXACT expected filename(s) it was told?
followed = (all(f in deliv for f in EXPECTED_FILES) if EXPECTED_FILES else None)

print("RESULT=" + json.dumps({
    "label": a.label, "agent": a.agent, "case": a.case, "arm": a.arm, "work_dir": WD,
    "auth_used": auth, "status": res.get("status"), "wall_s": wall, "calls": calls,
    "used_semfs_grep": used,
    "tokens": ut.get("total_tokens") or ((ut.get("prompt_tokens") or 0) + (ut.get("completion_tokens") or 0)),
    "usage": ut, "deliverables": list(deliv.keys()), "deliverable_content": deliv,
    "expected_files": EXPECTED_FILES, "followed_filename": followed,
    "tool_commands": [c for c in cmds if c][:40],
    "err": str(res.get("errorMessage") or "")[:600],
}, ensure_ascii=False))
