"""Materialize the FINAL, Unsloth-friendly SFT dataset -> pmarmik/semfs-compress-sft.

Joins generated compressions (GEN_REPO) with the fact-verdicts (VERDICT_REPO) by uid,
keeps ONLY fact_preserved == True (drops the teacher's fact-LOSING compressions — we must
not train the student to drop facts), and emits the standard conversational format Unsloth
expects for text SFT:

    {"messages": [
        {"role": "system",    "content": <compression instruction w/ target ratio>},
        {"role": "user",      "content": "Compress:\n<original>"},
        {"role": "assistant", "content": <compressed>},
    ], "domain": ..., "ratio_bucket": ..., "uid": ..., "fact_verdict": ...}

`messages` with role/content (plain-string content) is model-agnostic: SFTTrainer +
tokenizer.apply_chat_template consume it directly for Qwen3 / Qwen2.5 / Llama, etc.

  modal run benchmarks/modal/build_sft_dataset.py
"""

from __future__ import annotations

import modal

app = modal.App("semfs-build-sft")
image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets", "huggingface_hub", "hf-transfer")
    .env({"HF_HUB_ENABLE_HF_TRANSFER": "1"})
)

GEN_REPO = "pmarmik/semfs-compress-generated-openai"
VERDICT_REPO = "pmarmik/semfs-compress-verdicts"
OUT_REPO = "pmarmik/semfs-compress-sft"


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
def build() -> dict:
    import json
    import os

    from datasets import Dataset, DatasetDict, load_dataset

    token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
    splits_out, stats = {}, {}

    for split in ["train", "validation", "test"]:
        gen = load_dataset(GEN_REPO, split=split)
        ver = load_dataset(VERDICT_REPO, split=split)
        vmap = {v["uid"]: v for v in ver}

        rows, dropped_changed, dropped_empty, per_domain = [], 0, 0, {}
        for r in gen:
            v = vmap.get(r["uid"])
            if v is None:
                continue
            if not v["fact_preserved"]:          # drop CHANGED (fact-losing) compressions
                dropped_changed += 1
                continue
            if not r.get("compressed", "").strip():
                dropped_empty += 1
                continue
            kp = int(float(r["ratio_bucket"]) * 100)
            rows.append({
                "messages": [
                    {"role": "system", "content": student_system(kp)},
                    {"role": "user", "content": "Compress:\n" + r["original"]},
                    {"role": "assistant", "content": r["compressed"]},
                ],
                "domain": r["domain"],
                "ratio_bucket": float(r["ratio_bucket"]),
                "uid": r["uid"],
                "fact_verdict": v["fact_verdict"],
            })
            per_domain[r["domain"]] = per_domain.get(r["domain"], 0) + 1

        splits_out[split] = Dataset.from_list(rows)
        stats[split] = {"kept": len(rows), "dropped_changed": dropped_changed,
                        "dropped_empty": dropped_empty, "per_domain": per_domain}
        print(f"[{split}] kept={len(rows)} dropped_changed={dropped_changed} dropped_empty={dropped_empty}")

    dd = DatasetDict(splits_out)
    dd.push_to_hub(OUT_REPO, private=False, token=token)
    print(f"pushed -> https://huggingface.co/datasets/{OUT_REPO}")
    print(json.dumps(stats, indent=2))
    return stats


@app.local_entrypoint()
def main():
    print(build.remote())
