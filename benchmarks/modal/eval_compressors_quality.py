"""3-way compressor quality eval on the SAME rows: GLM-5.1 (from checkpoint),
gpt-4.1-mini, gpt-5.4-nano. Fidelity = voyage-4-nano-ONNX cosine(original,
compressed); plus achieved compression ratio. Best = highest cosine at the
lowest ratio (most meaning kept per token saved).

  modal run benchmarks/modal/eval_compressors_quality.py::evaluate
"""

from __future__ import annotations

import asyncio
import json
import os
import re

import modal

app = modal.App("semfs-compress-eval")
image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("httpx", "tiktoken", "onnxruntime", "transformers", "tokenizers",
                 "huggingface_hub", "numpy")
)
ckpt_vol = modal.Volume.from_name("semfs-compress-ckpt", create_if_missing=True)

OR_BASE = "https://openrouter.ai/api/v1/chat/completions"
VOYAGE_ONNX = "onnx-community/voyage-4-nano-ONNX"
DOC_PROMPT = "Represent the document for retrieval: "
_WORD = re.compile(r"\w+")

EX_MILD = """Original: The board of directors met on Tuesday to discuss the proposed acquisition of Meridian Systems, which had been under review since the beginning of the quarter, valued at approximately $340 million.
Compressed: The board met on Tuesday to discuss the acquisition of Meridian Systems, valued at $340 million."""


def system_for(keep_pct: int) -> str:
    base = ("You compress text while preserving EVERY fact exactly (numbers, money, dates, percentages, "
            "durations, names, decisions, code, URLs, file paths, speaker labels). Output ONLY the "
            "compressed text.")
    if keep_pct >= 65:
        mode = "MODE: STRICT EXTRACTIVE. Delete redundant words ONLY; keep remaining words VERBATIM and grammatical."
    elif keep_pct >= 45:
        mode = "MODE: AGGRESSIVE EXTRACTIVE. Delete as much redundancy as possible, keep remaining words VERBATIM."
    else:
        mode = ("MODE: COMPRESS HARD. Prefer deletion but you MAY lightly rephrase ('in order to'->'to', "
                "merge/drop subordinate clauses) to maximize reduction, preserving every fact.")
    return f"{base}\n\n{mode}\n\nTarget: keep about {keep_pct}% of the length.\n\nEXAMPLE:\n{EX_MILD}"


@app.function(image=image,
              secrets=[modal.Secret.from_name("openrouter"), modal.Secret.from_name("hf-token")],
              volumes={"/ckpt": ckpt_vol}, cpu=8.0, memory=16384, timeout=2400)
def evaluate(per_bucket: int = 6) -> dict:
    import glob
    import httpx
    import numpy as np
    import onnxruntime as ort
    import tiktoken
    from huggingface_hub import snapshot_download
    from transformers import AutoTokenizer

    enc = tiktoken.get_encoding("o200k_base")
    ntok = lambda s: len(enc.encode(s, disallowed_special=()))
    key = os.environ["OPENROUTER_API_KEY"]

    # voyage embedder
    folder = snapshot_download(VOYAGE_ONNX)
    q4 = os.path.join(folder, "onnx", "model_q4.onnx")
    if not os.path.exists(q4):
        q4 = (glob.glob(os.path.join(folder, "onnx", "*q4*.onnx")) or glob.glob(os.path.join(folder, "onnx", "*.onnx")))[0]
    vtok = AutoTokenizer.from_pretrained(VOYAGE_ONNX)
    sess = ort.InferenceSession(q4, providers=["CPUExecutionProvider"])
    want = {i.name for i in sess.get_inputs()}

    def embed(text):
        inp = vtok(DOC_PROMPT + (text or " "), return_tensors="np", truncation=True, max_length=32000).data
        inp = {k: v.astype(np.int64) for k, v in inp.items() if k in want}
        outs = sess.run(None, inp)
        p = next((o[0] for o in outs if o.ndim == 2 and o.shape[0] == 1), outs[-1][0])
        return p / (np.linalg.norm(p) + 1e-9)

    cosine = lambda a, b: float(np.dot(a, b))

    # sample GLM checkpoint rows, balanced by bucket
    glm = [json.loads(l) for l in open("/ckpt/train.jsonl") if l.strip()]
    glm = [r for r in glm if r.get("status") == "ok"]
    by_b, sample = {}, []
    for r in glm:
        by_b.setdefault(r["ratio_bucket"], []).append(r)
    for items in by_b.values():
        sample.extend(items[:per_bucket])
    print(f"eval {len(sample)} rows (domains: {set(r['domain'] for r in sample)})")

    sem = asyncio.Semaphore(8)

    async def call(model, row, client):
        kp = int(float(row["ratio_bucket"]) * 100)
        body = {"model": model, "messages": [
            {"role": "system", "content": system_for(kp)},
            {"role": "user", "content": "Compress:\n" + row["original"]}]}
        if "5.4-nano" in model:
            body["reasoning"] = {"effort": "medium"}
        async with sem:
            for a in range(3):
                try:
                    r = await client.post(OR_BASE, headers={"Authorization": f"Bearer {key}"}, json=body)
                    r.raise_for_status()
                    return r.json()["choices"][0]["message"].get("content") or ""
                except Exception:  # noqa: BLE001
                    await asyncio.sleep(2 * (a + 1))
            return ""

    async def run():
        async with httpx.AsyncClient(timeout=180) as client:
            g41 = asyncio.gather(*[call("openai/gpt-4.1-mini", r, client) for r in sample])
            g54 = asyncio.gather(*[call("openai/gpt-5.4-nano", r, client) for r in sample])
            return await g41, await g54

    c41, c54 = asyncio.run(run())

    # embed + score
    models = {"GLM": [r["compressed"] for r in sample], "gpt-4.1-mini": c41, "gpt-5.4-nano": c54}
    rows_out = []
    for i, row in enumerate(sample):
        eo = embed(row["original"])
        rec = {"bucket": row["ratio_bucket"]}
        for m, comps in models.items():
            comp = comps[i]
            rec[m] = {"ratio": round(ntok(comp) / max(1, row["n_tokens_in"]), 3) if comp.strip() else 0,
                      "cos": round(cosine(eo, embed(comp)), 4) if comp.strip() else 0.0}
        rows_out.append(rec)

    def summ(m):
        cs = [r[m]["cos"] for r in rows_out if r[m]["cos"] > 0]
        rs = [r[m]["ratio"] for r in rows_out if r[m]["cos"] > 0]
        mc = sum(cs) / len(cs) if cs else 0
        mr = sum(rs) / len(rs) if rs else 0
        # fidelity-per-saving: meaning kept per token removed (higher = better compressor)
        score = round(mc * (1 - mr), 4) if rs else 0
        return {"mean_cos": round(mc, 4), "mean_ratio": round(mr, 3),
                "saved_pct": round((1 - mr) * 100, 1), "quality_score": score}

    ranking = {m: summ(m) for m in models}
    best = max(ranking, key=lambda m: ranking[m]["quality_score"])
    samples = [{"bucket": r["bucket"], "orig": s["original"][:180],
                "GLM": models["GLM"][i][:160], "gpt41": c41[i][:160], "gpt54": c54[i][:160],
                "cos": {m: r[m]["cos"] for m in models}, "ratio": {m: r[m]["ratio"] for m in models}}
               for i, (r, s) in enumerate(zip(rows_out, sample))][:4]

    out = {"n": len(sample), "ranking": ranking, "best_by_quality_score": best,
           "note": "quality_score = mean_cos * (1 - mean_ratio) = meaning retained x fraction saved",
           "samples": samples}
    print(json.dumps(out, indent=2))
    return out


@app.local_entrypoint()
def main():
    print(json.dumps(evaluate.remote(), indent=2))
