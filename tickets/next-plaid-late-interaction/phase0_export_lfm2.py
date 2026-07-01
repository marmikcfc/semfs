"""Phase 0 gate — export LiquidAI/LFM2-ColBERT-350M to ONNX via pylate-onnx-export.

LFM2 ships only safetensors (no ONNX); LateOn / LateOn-Code already ship ONNX.
This spike proves the LFM2 export works (its hybrid conv+attn backbone is the risk),
persists model.onnx to the `np-lfm2-onnx` Modal volume, and verifies it loads in
onnxruntime and emits a [.., .., 128] per-token tensor.

Run:  modal run tickets/next-plaid-late-interaction/phase0_export_lfm2.py
Gate: returns ok=True with dim128=True  →  kaifa-C + houqin-A doc lanes unblocked.
Fail: → fallback = a baked PyLate (PyTorch) encoder sidecar for the LFM2 lanes.
"""
import modal

app = modal.App("phase0-lfm2-onnx-export")
vol = modal.Volume.from_name("np-lfm2-onnx", create_if_missing=True)
image = modal.Image.debian_slim(python_version="3.11").pip_install("pylate-onnx-export")


@app.function(image=image, volumes={"/out": vol}, timeout=3600, cpu=4.0, memory=16384)
def export():
    import subprocess, glob, os
    out = "/out/lfm2-colbert-350m-onnx"
    print(">>> pylate-onnx-export LiquidAI/LFM2-ColBERT-350M", flush=True)
    r = subprocess.run(
        ["pylate-onnx-export", "LiquidAI/LFM2-ColBERT-350M", "-o", out, "--force"],
        capture_output=True, text=True,
    )
    print("STDOUT:\n" + r.stdout[-6000:], flush=True)
    print("STDERR:\n" + r.stderr[-6000:], flush=True)
    print("exit code:", r.returncode, flush=True)

    files = [f for f in glob.glob(out + "/**/*", recursive=True) if os.path.isfile(f)]
    onnx_files = [f for f in files if f.endswith(".onnx")]
    print("produced:", [(os.path.relpath(f, out), os.path.getsize(f)) for f in files], flush=True)
    if not onnx_files:
        return {"ok": False, "reason": "no .onnx produced", "code": r.returncode}

    try:
        import onnxruntime as ort, numpy as np
        m = next(f for f in onnx_files if "int8" not in f)
        sess = ort.InferenceSession(m, providers=["CPUExecutionProvider"])
        print("inputs:", [(i.name, i.shape) for i in sess.get_inputs()], flush=True)
        print("outputs:", [(o.name, o.shape) for o in sess.get_outputs()], flush=True)
        seq = 16
        feed = {}
        for i in sess.get_inputs():
            n = i.name.lower()
            feed[i.name] = (np.zeros if "type" in n else np.ones)((1, seq), dtype=np.int64)
        outs = sess.run(None, feed)
        shapes = [list(o.shape) for o in outs]
        dim128 = any(o.shape[-1] == 128 for o in outs)
        print("output shapes:", shapes, "dim128:", dim128, flush=True)
        vol.commit()
        return {"ok": dim128, "onnx": [os.path.basename(f) for f in onnx_files], "shapes": shapes, "dim128": dim128}
    except Exception as e:
        vol.commit()
        return {"ok": False, "reason": f"verify:{e!r}", "onnx": [os.path.basename(f) for f in onnx_files]}


@app.local_entrypoint()
def main():
    print("RESULT:", export.remote())
