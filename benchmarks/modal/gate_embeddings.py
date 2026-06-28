"""Post-generation embedding gate (decoupled from GLM, CPU-only).

Run this AFTER compression is fully done and GLM is shut down. Loads the generated
dataset, computes voyage-4-nano-ONNX (Q4) cosine(original, compressed) per row,
prints the distribution so the threshold can be calibrated, and pushes an enriched
dataset with `gate_embed_cos` + `passed` columns.

  modal run benchmarks/modal/gate_embeddings.py::gate                  # all splits
  EMBED_THRESHOLD=0.83 modal run benchmarks/modal/gate_embeddings.py::gate
"""

from __future__ import annotations

import json
import os

import modal

app = modal.App("semfs-compress-embed-gate")
image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets==2.21.0", "onnxruntime", "transformers", "tokenizers",
                 "huggingface_hub", "numpy")
)

GEN_REPO = "pmarmik/semfs-compress-generated-phase1"
VOYAGE_ONNX = "onnx-community/voyage-4-nano-ONNX"
DOC_PROMPT = "Represent the document for retrieval: "
THRESHOLD = float(os.environ.get("EMBED_THRESHOLD", "0.83"))
SPLITS = ("train", "validation", "test")


@app.function(image=image, secrets=[modal.Secret.from_name("hf-token")], cpu=8.0, memory=16384, timeout=4 * 3600)
def gate() -> dict:
    import glob
    import numpy as np
    import onnxruntime as ort
    from datasets import Dataset, load_dataset
    from huggingface_hub import snapshot_download
    from transformers import AutoTokenizer

    token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
    folder = snapshot_download(VOYAGE_ONNX)
    q4 = os.path.join(folder, "onnx", "model_q4.onnx")
    if not os.path.exists(q4):
        cands = glob.glob(os.path.join(folder, "onnx", "*q4*.onnx")) or glob.glob(os.path.join(folder, "onnx", "*.onnx"))
        q4 = cands[0]
    print(f"voyage ONNX: {os.path.basename(q4)}")
    vtok = AutoTokenizer.from_pretrained(VOYAGE_ONNX)
    sess = ort.InferenceSession(q4, providers=["CPUExecutionProvider"])
    want = {i.name for i in sess.get_inputs()}

    def embed(text):
        inp = vtok(DOC_PROMPT + (text or " "), return_tensors="np", truncation=True, max_length=32000).data
        inp = {k: v.astype(np.int64) for k, v in inp.items() if k in want}
        outs = sess.run(None, inp)
        pooled = next((o[0] for o in outs if o.ndim == 2 and o.shape[0] == 1), outs[-1][0])
        return pooled / (np.linalg.norm(pooled) + 1e-9)

    summary = {}
    for split in SPLITS:
        try:
            ds = load_dataset(GEN_REPO, split=split)
        except Exception as e:  # noqa: BLE001
            summary[split] = f"SKIP: {type(e).__name__}: {str(e)[:80]}"
            continue
        cos, passed = [], []
        for r in ds:
            if r["status"] != "ok" or not r["compressed"].strip():
                cos.append(0.0); passed.append(False); continue
            c = float(np.dot(embed(r["original"]), embed(r["compressed"])))
            cos.append(round(c, 4))
            passed.append(bool(r.get("gate_preserve", False) and c >= THRESHOLD))
        ds = ds.add_column("gate_embed_cos", cos).add_column("passed", passed)
        ds.push_to_hub(GEN_REPO, split=split, private=True, token=token)
        cv = sorted(c for c in cos if c > 0)
        summary[split] = {
            "rows": len(ds), "passed": sum(passed), "pass_rate": round(sum(passed) / max(1, len(ds)), 3),
            "cos_min": cv[0] if cv else 0, "cos_p10": cv[len(cv) // 10] if cv else 0,
            "cos_median": cv[len(cv) // 2] if cv else 0, "threshold": THRESHOLD,
        }
        print(f"[{split}] {summary[split]}")

    print(json.dumps(summary, indent=2))
    return summary


@app.local_entrypoint()
def main():
    print(json.dumps(gate.remote(), indent=2))
