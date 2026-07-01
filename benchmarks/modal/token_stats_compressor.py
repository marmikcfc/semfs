"""Actual token accounting of the compressor training set with the REAL Qwen3.5 tokenizer,
to predict epoch time on a GPU.

Tokenizes every fact_preserved training example (system+user+assistant, via the chat template)
exactly as training will see it, then reports the token distribution and predicts epoch wall-time
at several throughputs (the slow path we observed vs Unsloth's normal vs packing-corrected).

  modal run benchmarks/modal/token_stats_compressor.py
"""

from __future__ import annotations

import modal

app = modal.App("semfs-token-stats")
image = modal.Image.debian_slim(python_version="3.11").pip_install(
    "transformers", "datasets", "huggingface_hub", "hf-transfer", "jinja2", "numpy"
).env({"HF_HUB_ENABLE_HF_TRANSFER": "1"})

MODEL = "unsloth/Qwen3.5-0.8B"
GEN_REPO = "pmarmik/semfs-compress-generated-openai"
VERDICT_REPO = "pmarmik/semfs-compress-verdicts"


def student_system(keep_pct: int) -> str:
    if keep_pct >= 65:
        mode = "Delete redundant words only; keep the rest grammatical."
    elif keep_pct >= 45:
        mode = "Delete redundancy aggressively."
    else:
        mode = "Compress hard; light rephrasing allowed."
    return (f"Compress the text to about {keep_pct}% of its length, preserving EVERY fact "
            f"(numbers, money, dates, names, code, claims). {mode} Output only the compressed text.")


@app.function(image=image, secrets=[modal.Secret.from_name("hf-token")], cpu=8.0, timeout=1800)
def stats() -> dict:
    import numpy as np
    from datasets import load_dataset
    from transformers import AutoTokenizer

    tok = AutoTokenizer.from_pretrained(MODEL, trust_remote_code=True)
    print(f"tokenizer: {MODEL} | vocab_size={tok.vocab_size}")

    gen = load_dataset(GEN_REPO, split="train")
    ver = load_dataset(VERDICT_REPO, split="train")
    keep = {v["uid"] for v in ver if v["fact_preserved"]}

    full_toks, resp_toks = [], []
    for r in gen:
        if r["uid"] not in keep or not r.get("compressed", "").strip():
            continue
        keep_pct = int(float(r["ratio_bucket"]) * 100)
        msgs = [
            {"role": "system", "content": student_system(keep_pct)},
            {"role": "user", "content": "Compress:\n" + r["original"]},
            {"role": "assistant", "content": r["compressed"]},
        ]
        text = tok.apply_chat_template(msgs, tokenize=False)
        n_full = len(tok.encode(text, add_special_tokens=False))
        n_resp = len(tok.encode(r["compressed"], add_special_tokens=False))
        full_toks.append(n_full)
        resp_toks.append(n_resp)

    full = np.array(full_toks)
    resp = np.array(resp_toks)
    n = len(full)
    total = int(full.sum())

    def pct_over(arr, cap):
        return round(100 * float((arr > cap).mean()), 1)

    dist = {
        "n_examples": n,
        "total_full_tokens": total,
        "mean_full": round(float(full.mean()), 1),
        "p50_full": int(np.percentile(full, 50)),
        "p90_full": int(np.percentile(full, 90)),
        "p99_full": int(np.percentile(full, 99)),
        "max_full": int(full.max()),
        "mean_resp": round(float(resp.mean()), 1),
        "pct_over_4096": pct_over(full, 4096),
        "pct_over_2048": pct_over(full, 2048),
        "pct_over_1024": pct_over(full, 1024),
        # effective tokens actually trained if we truncate at a given max_seq (sum of min(len, cap))
        "eff_tokens_at_4096": int(np.minimum(full, 4096).sum()),
        "eff_tokens_at_2048": int(np.minimum(full, 2048).sum()),
    }

    EPOCHS = 2
    # throughputs (tokens/sec): observed slow path, Unsloth-normal benchmark, and an optimistic packing+CCE target
    THROUGHPUTS = {"observed_slow_436": 436, "unsloth_normal_11736": 11736, "packing_cce_target_20000": 20000}
    eff_per_epoch = dist["eff_tokens_at_4096"]
    pred = {}
    for name, tps in THROUGHPUTS.items():
        sec_epoch = eff_per_epoch / tps
        pred[name] = {
            "tok_per_sec": tps,
            "min_per_epoch": round(sec_epoch / 60, 1),
            "hr_total_2epochs": round(sec_epoch * EPOCHS / 3600, 2),
        }

    out = {"distribution": dist, "epochs": EPOCHS, "predictions": pred}
    import json
    print(json.dumps(out, indent=2))
    return out


@app.local_entrypoint()
def main():
    print(stats.remote())
