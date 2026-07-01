"""Assemble the AGENTIC v2 base: tool-result outputs, logs, JSON dumps, and agent-trace tool-output
blocks — the high-value compressible content (per the headroom/kompress insight). Permissive sources only.

Push -> pmarmik/semfs-compress-sources-v2-agentic (private).
Probe schemas first:  modal run benchmarks/modal/assemble_agentic.py::probe
Then build:           modal run benchmarks/modal/assemble_agentic.py::main
"""
import hashlib
import json
import os

import modal

app = modal.App("assemble-agentic-v2")
image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets", "huggingface_hub[hf_transfer]", "tiktoken")
    .env({"HF_HUB_ENABLE_HF_TRANSFER": "1"})
)

# (id, optional config, streaming) — permissive licenses only.
SOURCES = [
    ("lambda/hermes-agent-reasoning-traces", "glm-5.1", "train"),       # Apache-2.0 <tool_response>
    ("nvidia/Open-SWE-Traces", "sweagent", "qwen35_122b"),            # CC-BY SWE trajectories
]


@app.function(image=image, secrets=[modal.Secret.from_name("hf-token")], timeout=1800)
def probe():
    """Load 1 row from each source; report keys + a truncated sample of each field."""
    from datasets import load_dataset
    token = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
             or os.environ.get("HUGGINGFACE_TOKEN"))
    out = {}
    for sid, cfg, split in SOURCES:
        try:
            ds = load_dataset(sid, cfg, split=split, streaming=True, token=token)
            row = next(iter(ds))
            fields = {}
            for k, v in row.items():
                s = v if isinstance(v, str) else json.dumps(v, default=str)
                fields[k] = {"type": type(v).__name__, "len": len(s), "head": s[:160]}
            out[sid] = fields
        except Exception as e:  # noqa: BLE001
            out[sid] = f"ERR: {type(e).__name__}: {str(e)[:160]}"
    return out


@app.local_entrypoint()
def probe_main():
    print(json.dumps(probe.remote(), indent=2, ensure_ascii=False))


@app.function(image=image, secrets=[modal.Secret.from_name("hf-token")],
              timeout=3600, cpu=4.0, memory=16384)
def build(n_tool: int = 1000, n_logs: int = 1200, n_json: int = 600,
          n_deepseek: int = 1000, n_openswe: int = 1000):
    import re
    import tiktoken
    from collections import Counter
    from datasets import load_dataset, Dataset
    from huggingface_hub import HfApi
    token = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
             or os.environ.get("HUGGINGFACE_TOKEN"))
    enc = tiktoken.get_encoding("cl100k_base")

    def ntok(s):
        return len(enc.encode(s, disallowed_special=()))

    rows, seen = [], set()

    def add(domain, src, text):
        text = (text or "").strip()
        if len(text) < 80:
            return False
        h = hashlib.md5(text[:300].encode()).hexdigest()[:16]
        if h in seen:
            return False
        body = text[:24000]
        n = ntok(body)
        if not (120 <= n <= 6000):
            return False
        seen.add(h)
        rows.append({"domain": domain, "source": src, "doc_id": f"{domain}-{len(rows)}", "uid": h,
                     "original": body, "n_tokens": n, "length_class": "long" if n > 1500 else "mid"})
        return True

    report = {}

    # 1. glaive — tool_result JSON (FUNCTION RESPONSE blocks)
    try:
        ds = load_dataset("glaiveai/glaive-function-calling-v2", split="train", streaming=True, token=token)
        pat = re.compile(r"FUNCTION RESPONSE:\s*(\{.*?\})\s*(?:\n\n|ASSISTANT:|USER:|$)", re.S)
        c = 0
        for ex in ds:
            for m in pat.findall(ex.get("chat", "")):
                if add("tool_result", "glaive", m):
                    c += 1
            if c >= n_tool:
                break
        report["glaive"] = c
    except Exception as e:  # noqa: BLE001
        report["glaive"] = f"skip:{type(e).__name__}:{str(e)[:80]}"

    # 2. loghub_2 — group consecutive log lines into chunks
    try:
        ds = load_dataset("bolu61/loghub_2", split="train", streaming=True, token=token)
        c, buf = 0, []
        for ex in ds:
            buf.append(ex.get("text", ""))
            if len(buf) >= 70:
                if add("logs", "loghub", "\n".join(buf)):
                    c += 1
                buf = []
                if c >= n_logs:
                    break
        report["logs"] = c
    except Exception as e:  # noqa: BLE001
        report["logs"] = f"skip:{type(e).__name__}:{str(e)[:80]}"

    # 3. paraloq — JSON dumps (schema + instance)
    try:
        ds = load_dataset("paraloq/json_data_extraction", split="train", streaming=True, token=token)
        c = 0
        for ex in ds:
            blob = ex.get("item") or ""
            if ex.get("schema"):
                blob = ex["schema"] + "\n\n" + blob
            if add("json", "paraloq", blob):
                c += 1
            if c >= n_json:
                break
        report["json"] = c
    except Exception as e:  # noqa: BLE001
        report["json"] = f"skip:{type(e).__name__}:{str(e)[:80]}"

    # 4. deepseek-hermes — agent traces (high-token text carries verbose tool output)
    try:
        ds = load_dataset("r0b0tlab/deepseek-hermes-reasoning-traces", split="train", streaming=True, token=token)
        c = 0
        for ex in ds:
            if (ex.get("tokens") or 0) >= 1000 and add("agent_trace", "deepseek-hermes", ex.get("text", "")):
                c += 1
            if c >= n_deepseek:
                break
        report["deepseek"] = c
    except Exception as e:  # noqa: BLE001
        report["deepseek"] = f"skip:{type(e).__name__}:{str(e)[:80]}"

    # 5. Open-SWE — tool-output turns from the trajectory + the diff patch
    try:
        ds = load_dataset("nvidia/Open-SWE-Traces", "sweagent", split="qwen35_122b", streaming=True, token=token)
        c = 0
        for ex in ds:
            for turn in (ex.get("trajectory") or []):
                if not isinstance(turn, dict):
                    continue
                content = turn.get("content") or ""
                role = turn.get("role") or ""
                if len(content) > 400 and role in ("tool", "user", "function", "observation"):
                    if add("agent_trace", "open-swe", content):
                        c += 1
                if c >= n_openswe:
                    break
            if c < n_openswe and ex.get("model_patch"):
                if add("agent_trace", "open-swe-diff", ex["model_patch"]):
                    c += 1
            if c >= n_openswe:
                break
        report["open-swe"] = c
    except Exception as e:  # noqa: BLE001
        report["open-swe"] = f"skip:{type(e).__name__}:{str(e)[:80]}"

    api = HfApi(token=token)
    repo_id = f"{api.whoami()['name']}/semfs-compress-sources-v2-agentic"
    Dataset.from_list(rows).push_to_hub(repo_id, private=True, token=token)
    toks = sorted(r["n_tokens"] for r in rows)
    return {"repo": repo_id, "n": len(rows), "by_domain": dict(Counter(r["domain"] for r in rows)),
            "by_source": dict(Counter(r["source"] for r in rows)),
            "long_count": sum(1 for r in rows if r["length_class"] == "long"),
            "tok_median": toks[len(toks) // 2] if toks else 0, "tok_max": toks[-1] if toks else 0,
            "report": report}


@app.local_entrypoint()
def main(n_tool: int = 1000, n_logs: int = 1200, n_json: int = 600,
         n_deepseek: int = 1000, n_openswe: int = 1000):
    print(json.dumps(build.remote(n_tool, n_logs, n_json, n_deepseek, n_openswe), indent=2))
