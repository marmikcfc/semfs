"""Minimal smoke: does FastVisionModel (the CORRECT loader for the multimodal Qwen3.5-0.8B)
engage the fused-CE fast path? Mirrors Unsloth's official Qwen3.5-0.8B notebook pattern
(FastVisionModel + UnslothVisionDataCollator) but feeds our TEXT compression data.

We only care about the STEP RATE here. The official notebook gets ~3.3 s/step on a T4;
our FastLanguageModel path got 56 s/step. If FastVisionModel engages CCE -> fast.

  modal run benchmarks/modal/smoke_vision_qwen35.py
"""

from __future__ import annotations

import modal

app = modal.App("semfs-vision-smoke")

# SAME image as the (now-cached) training image — notebook stack + torchvision.
image = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("git")
    .pip_install(
        "torch==2.8.0", "transformers==5.5.0",   # max unsloth 2026.6.9 supports (5.12 won't resolve)
        "unsloth==2026.6.9", "unsloth_zoo==2026.6.7",   # consistent PyPI release pair (git HEADs conflicted)
        "bitsandbytes",
        "datasets", "huggingface_hub", "hf-transfer",
    )
    .pip_install("torchvision==0.23.0")
    .env({"HF_HUB_ENABLE_HF_TRANSFER": "1", "PYTORCH_CUDA_ALLOC_CONF": "expandable_segments:True"})
)

MODEL = "unsloth/Qwen3.5-0.8B"   # 0.8B fits T4's 16GB; matches the official notebook's model
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


@app.function(image=image, gpu="T4",
              secrets=[modal.Secret.from_name("hf-token")], timeout=1800)
def smoke() -> dict:
    from unsloth import FastVisionModel
    from unsloth.trainer import UnslothVisionDataCollator
    import torch
    from datasets import load_dataset
    from trl import SFTConfig, SFTTrainer

    # 1) load the MULTIMODAL model with its proper loader (this is the hypothesis under test)
    model, tokenizer = FastVisionModel.from_pretrained(
        MODEL, load_in_4bit=False, use_gradient_checkpointing="unsloth",
    )
    model = FastVisionModel.get_peft_model(
        model,
        finetune_vision_layers=False,      # text-only task -> only language side
        finetune_language_layers=True,
        finetune_attention_modules=True,
        finetune_mlp_modules=True,
        r=16, lora_alpha=16, lora_dropout=0.0, bias="none", random_state=3407,
    )
    FastVisionModel.for_training(model)

    # 2) a few of OUR text examples, in the vision message format (content = typed parts, no image)
    gen = load_dataset(GEN_REPO, split="train")
    ver = load_dataset(VERDICT_REPO, split="train")
    keep = {v["uid"] for v in ver if v["fact_preserved"]}
    convos = []
    for r in gen:
        if r["uid"] not in keep or not r.get("compressed", "").strip():
            continue
        kp = int(float(r["ratio_bucket"]) * 100)
        convos.append({"messages": [
            {"role": "system", "content": [{"type": "text", "text": student_system(kp)}]},
            {"role": "user", "content": [{"type": "text", "text": "Compress:\n" + r["original"]}]},
            {"role": "assistant", "content": [{"type": "text", "text": r["compressed"]}]},
        ]})
        if len(convos) >= 60:
            break
    print(f"smoke convos: {len(convos)}")

    # 3) train a handful of steps with the VISION collator (the path the notebook uses)
    trainer = SFTTrainer(
        model=model, tokenizer=tokenizer,
        data_collator=UnslothVisionDataCollator(model, tokenizer),
        train_dataset=convos,
        args=SFTConfig(
            per_device_train_batch_size=2, gradient_accumulation_steps=4,
            max_steps=10, learning_rate=2e-4, max_length=2048,
            packing=True,   # TEST: does packing speed up the vision/CCE-off path?
            warmup_steps=2, logging_steps=1, optim="adamw_8bit", fp16=True,   # T4 (Turing) has no bf16
            output_dir="/tmp/vout", report_to="none",
            remove_unused_columns=False, dataset_kwargs={"skip_prepare_dataset": True},
        ),
    )
    res = trainer.train()
    metrics = {k: float(v) for k, v in res.metrics.items() if isinstance(v, (int, float))}
    print({"n": len(convos), **metrics})
    return metrics


@app.local_entrypoint()
def main():
    print(smoke.remote())
