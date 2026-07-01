"""Phase 0 gate (patched) — export LFM2-ColBERT-350M to ONNX.

First attempt failed: pylate-onnx-export assumes non-ModernBERT ⇒ token_type_ids,
but LFM2 (Lfm2Model, Llama-like) doesn't emit them → KeyError 'token_type_ids'.
Fix: derive uses_token_type_ids from tokenizer.model_input_names (monkeypatch).
This also tells us whether the LFM2 backbone itself traces to ONNX (the real risk).

Run: modal run tickets/next-plaid-late-interaction/phase0_export_lfm2_patched.py
"""
import modal

app = modal.App("phase0-lfm2-onnx-export-patched")
vol = modal.Volume.from_name("np-lfm2-onnx")
image = modal.Image.debian_slim(python_version="3.11").pip_install("pylate-onnx-export", "onnxscript")


@app.function(image=image, volumes={"/out": vol}, timeout=3600, cpu=4.0, memory=16384)
def export():
    import colbert_export.export as ce
    _orig = ce.detect_model_architecture

    def patched(pm):
        d = _orig(pm)
        names = list(getattr(pm[0].tokenizer, "model_input_names", []) or [])
        d["uses_token_type_ids"] = "token_type_ids" in names
        print(f">>> patched uses_token_type_ids={d['uses_token_type_ids']} model_input_names={names}", flush=True)
        return d

    ce.detect_model_architecture = patched

    # New torch defaults to the dynamo exporter, which fails on LFM2's KV-cache
    # data-dependent guard (modeling_lfm2.py:508). Force the legacy TorchScript
    # exporter — we export a plain encoder forward (no generation) so the branch
    # is never taken and the concrete path traces cleanly.
    import torch
    _oe = torch.onnx.export
    def _legacy_export(*a, **k):
        k["dynamo"] = False
        return _oe(*a, **k)
    torch.onnx.export = _legacy_export

    out = "/out/lfm2-colbert-350m-onnx"
    try:
        ce.export_model("LiquidAI/LFM2-ColBERT-350M", output_dir=out, quantize=True, force=True)
    except Exception as e:
        import traceback
        traceback.print_exc()
        return {"ok": False, "reason": f"export: {e!r}"}

    import glob, os, numpy as np, onnxruntime as ort
    onnx_files = glob.glob(out + "/**/*.onnx", recursive=True)
    print("onnx files:", [(os.path.basename(f), os.path.getsize(f)) for f in onnx_files], flush=True)
    if not onnx_files:
        return {"ok": False, "reason": "no .onnx"}
    m = next(f for f in onnx_files if "int8" not in f)
    sess = ort.InferenceSession(m, providers=["CPUExecutionProvider"])
    feed = {i.name: np.ones((1, 16), dtype=np.int64) for i in sess.get_inputs()}
    outs = sess.run(None, feed)
    shapes = [list(o.shape) for o in outs]
    dim128 = any(o.shape[-1] == 128 for o in outs)
    print("inputs:", [i.name for i in sess.get_inputs()], "shapes:", shapes, "dim128:", dim128, flush=True)
    vol.commit()
    return {"ok": dim128, "inputs": [i.name for i in sess.get_inputs()], "shapes": shapes, "dim128": dim128}


@app.local_entrypoint()
def main():
    print("RESULT:", export.remote())
