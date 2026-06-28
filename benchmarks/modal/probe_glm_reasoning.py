"""One-shot probe: how does GLM-5.1 emit reasoning on this vLLM? Runs inside Modal
(has the glm-vllm-key secret). Dumps finish_reason / message keys / content /
reasoning_content / usage for (A) our terse compression prompt and (B) the same
with chat_template_kwargs enable_thinking=true.

  modal run benchmarks/modal/probe_glm_reasoning.py::probe
"""
from __future__ import annotations
import json
import os
import modal

app = modal.App("semfs-glm-probe")
image = modal.Image.debian_slim(python_version="3.11").pip_install("httpx")
GLM_BASE = "https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run"

SYS = ("You compress text into caveman-speak preserving all facts. Output ONLY the "
       "compressed text. TARGET COMPRESSION RATIO: 0.5.")
USER = ("Compress:\nThe board met Tuesday to discuss the $340 million acquisition of "
        "Meridian Systems, accretive within 18 months.")


@app.function(image=image, secrets=[modal.Secret.from_name("glm-vllm-key")], timeout=600)
def probe():
    import httpx
    key = os.environ["MODAL_VLLM_API_KEY"]
    h = {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}

    def call(extra, max_tokens=2048):
        body = {"model": "glm-5.1-nvfp4",
                "messages": [{"role": "system", "content": SYS}, {"role": "user", "content": USER}],
                "temperature": 0.4, "top_p": 0.95, "max_tokens": max_tokens, **extra}
        r = httpx.post(f"{GLM_BASE}/v1/chat/completions", headers=h, json=body, timeout=300)
        d = r.json()
        ch = d["choices"][0]
        msg = ch["message"]
        reasoning = msg.get("reasoning") or msg.get("reasoning_content") or ""
        return {
            "http": r.status_code, "finish_reason": ch.get("finish_reason"),
            "content": (msg.get("content") or "")[:300],
            "reasoning_len": len(reasoning), "reasoning_head": reasoning[:300], "reasoning_tail": reasoning[-200:],
            "usage": d.get("usage"),
        }

    out = {
        "A_default_2k": call({}, 2048),
        "B_default_8k": call({}, 8192),                                  # does it finish if given room?
        "C_disable_thinking": call({"chat_template_kwargs": {"enable_thinking": False}}, 1024),
    }
    print(json.dumps(out, indent=2))
    return out


@app.local_entrypoint()
def main():
    print(json.dumps(probe.remote(), indent=2))
