"""LoRA fine-tune a small text model as the compressor, with Unsloth on Modal.

- Data: pmarmik/semfs-compress-sft — the FINAL clean SFT dataset (fact-preserved only,
  already in `messages` format). train split -> training; validation -> eval/early-stop.
- Method: 16-bit LoRA (r=32), train_on_responses_only (loss on the compressed output
  only), early-stop on eval_loss, save the merged model to a Volume.

  modal run benchmarks/modal/train_unsloth_compressor.py::smoke   # tiny, validate pipeline
  modal run benchmarks/modal/train_unsloth_compressor.py::run     # full run
"""

from __future__ import annotations

import json
import os

import modal

app = modal.App("semfs-train-compressor")
model_vol = modal.Volume.from_name("semfs-compressor-model", create_if_missing=True)
OUT_DIR = "/out"

# Stack: torch 2.8.0 + transformers 5.5.0 (the MAX unsloth 2026.6.9 supports; 5.12 won't resolve) +
# the consistent unsloth/unsloth_zoo PyPI release pair + cut_cross_entropy.
# The CCE-not-engaging blocker was NEVER a version issue — it was `trust_remote_code=True` in the
# from_pretrained() call making Unsloth's compiler bail (_utils.py:2751), skipping the CCE rewrite.
# Fixed at the call site below. (Codex source diagnostic 2026-06-27, see SEM-41.)
image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install(
        "torch==2.8.0", "transformers==5.5.0",
        "unsloth==2026.6.9", "unsloth_zoo==2026.6.7",   # consistent PyPI release pair
        "bitsandbytes",   # unsloth.kernels imports it at module load
        "datasets", "huggingface_hub", "hf-transfer",
    )
    .env({"HF_HUB_ENABLE_HF_TRANSFER": "1", "PYTORCH_CUDA_ALLOC_CONF": "expandable_segments:True"})
)

MODEL = "unsloth/Qwen3-1.7B"   # text-only, Unsloth-supported fast path (CCE engages, unlike multimodal Qwen3.5)
SFT_REPO = "pmarmik/semfs-compress-final-sft"   # FINAL: prose + agentic-reasoning (json/code/logs dropped)
MAX_SEQ = 2048   # most passages <=1500 tok; 2048 ~2x faster than 4096 with near-zero truncation


@app.function(image=image, gpu="L4", volumes={OUT_DIR: model_vol},
              secrets=[modal.Secret.from_name("hf-token")], timeout=6 * 3600)
def train(smoke: bool = False) -> dict:
    from unsloth import FastLanguageModel              # MUST import unsloth BEFORE transformers/trl
    from unsloth.chat_templates import train_on_responses_only
    import torch
    import time
    from datasets import load_dataset
    from transformers import EarlyStoppingCallback, TrainerCallback
    from trl import SFTConfig, SFTTrainer
    import glob

    class _VolCommit(TrainerCallback):   # commit each checkpoint to the Volume -> resumable across restarts
        def on_save(self, args, state, control, **kw):
            model_vol.commit()

    class _TPS(TrainerCallback):   # measure REAL tokens/sec (replaces the ~4k L4 estimate)
        def on_step_end(self, args, state, control, **kw):
            now = time.time()
            if not hasattr(self, "_t0"):
                self._t0, self._s0 = now, state.global_step
            elif state.global_step % 10 == 0:
                eff_bs = args.per_device_train_batch_size * args.gradient_accumulation_steps
                toks = (state.global_step - self._s0) * eff_bs * MAX_SEQ   # packed → ~full seq/sample
                dt = now - self._t0
                if dt > 0:
                    print(f"[tps] step={state.global_step} tokens/sec≈{int(toks/dt)} "
                          f"({(now-self._t0)/(state.global_step-self._s0):.1f}s/step)", flush=True)

    model_vol.reload()
    train_ds = load_dataset(SFT_REPO, split="train")        # already fact-preserved + messages format
    val_ds = load_dataset(SFT_REPO, split="validation")
    if smoke:
        train_ds, val_ds = train_ds.select(range(200)), val_ds.select(range(50))
    print(f"train={len(train_ds)} val={len(val_ds)} (fact_preserved only)")

    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name=MODEL, max_seq_length=MAX_SEQ,
        load_in_4bit=False, full_finetuning=False, dtype=None,   # 16-bit LoRA: bf16 base + adapters (no QLoRA quant loss)
        # NO trust_remote_code: it makes Unsloth's compiler bail (_utils.py:2751) -> skips the CCE
        # rewrite -> dense fp32 logits materialize -> 45-56 s/step. Qwen3 has native Unsloth support.
    )
    model = FastLanguageModel.get_peft_model(   # LoRA r=32 -> fits L4 easily, no gradient offloading
        model, r=32, lora_alpha=64, lora_dropout=0.0, bias="none",   # alpha=2*r speeds up training (Unsloth)
        target_modules=["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"],
        use_gradient_checkpointing="unsloth", random_state=3407,   # match the official Qwen3.5 notebook (fast path)
    )

    # render messages -> a single "text" field via the chat template (TRL needs text or formatting_func)
    def to_text(batch):   # enable_thinking=False -> direct compressor, no Qwen3 reasoning trace
        return {"text": [tokenizer.apply_chat_template(m, tokenize=False, enable_thinking=False)
                         for m in batch["messages"]]}
    train_ds = train_ds.map(to_text, batched=True, remove_columns=train_ds.column_names)   # -> only "text"
    val_ds = val_ds.map(to_text, batched=True, remove_columns=val_ds.column_names)

    args = SFTConfig(
        output_dir=f"{OUT_DIR}/ckpt", max_seq_length=MAX_SEQ, dataset_text_field="text",
        per_device_train_batch_size=2, gradient_accumulation_steps=8,   # CCE engages on Qwen3 -> no logit blowup, batch 2 fits
        num_train_epochs=(1 if smoke else 2),
        max_steps=(20 if smoke else -1),
        learning_rate=2e-4, lr_scheduler_type="cosine", warmup_ratio=0.03,   # LoRA uses higher LR than full FT
        weight_decay=0.01, optim="adamw_8bit", bf16=True, packing=True,   # Unsloth packing: 3-5x faster, less padding waste
        logging_steps=5, eval_strategy="steps", eval_steps=(10 if smoke else 100),
        save_strategy="steps", save_steps=(10 if smoke else 100),
        load_best_model_at_end=True, metric_for_best_model="eval_loss", greater_is_better=False,
        report_to="none", dataset_num_proc=4,
    )
    trainer = SFTTrainer(
        model=model, tokenizer=tokenizer, args=args,
        train_dataset=train_ds, eval_dataset=val_ds,
        callbacks=[EarlyStoppingCallback(early_stopping_patience=3), _VolCommit(), _TPS()],
    )
    # loss only on the assistant (compressed) tokens, not the original
    trainer = train_on_responses_only(
        trainer, instruction_part="<|im_start|>user\n", response_part="<|im_start|>assistant\n")

    resume = (not smoke) and bool(glob.glob(f"{OUT_DIR}/ckpt/checkpoint-*"))   # resume if a committed ckpt exists
    print(f"resume_from_checkpoint={resume}")
    res = trainer.train(resume_from_checkpoint=resume)
    metrics = {k: float(v) for k, v in res.metrics.items() if isinstance(v, (int, float))}

    if not smoke:
        save_dir = f"{OUT_DIR}/{MODEL.split('/')[-1].lower()}-compressor"   # model-derived name (was hardcoded stale)
        model.save_pretrained_merged(save_dir, tokenizer, save_method="merged_16bit")   # merge LoRA -> standalone model
        model_vol.commit()
        metrics["saved_to"] = save_dir

    print({"train": len(train_ds), "val": len(val_ds), **metrics})
    return {"n_train": len(train_ds), "n_val": len(val_ds), **metrics}


@app.function(image=image, gpu="L4", volumes={OUT_DIR: model_vol},
              secrets=[modal.Secret.from_name("hf-token")], timeout=1800)
def infer(n: int = 5) -> dict:
    """Ground-truth check: load the SAVED merged model + compress held-out test passages.
    The arbiter of whether the run actually trained (vs degenerate masking)."""
    from unsloth import FastLanguageModel
    from datasets import load_dataset
    model_vol.reload()
    save_dir = f"{OUT_DIR}/{MODEL.split('/')[-1].lower()}-compressor"
    model, tokenizer = FastLanguageModel.from_pretrained(model_name=save_dir, max_seq_length=MAX_SEQ,
                                                         load_in_4bit=False, dtype=None)
    FastLanguageModel.for_inference(model)
    test = load_dataset(SFT_REPO, split="test").select(range(n))
    out = []
    for r in test:
        msgs = r["messages"][:2]   # system + user (the passage); drop the gold assistant
        prompt = tokenizer.apply_chat_template(msgs, tokenize=False, add_generation_prompt=True,
                                               enable_thinking=False)
        ids = tokenizer(prompt, return_tensors="pt").to("cuda")
        gen = model.generate(**ids, max_new_tokens=1024, temperature=0.1, do_sample=False)
        comp = tokenizer.decode(gen[0][ids.input_ids.shape[1]:], skip_special_tokens=True)
        orig = r["messages"][1]["content"]
        out.append({"domain": r.get("domain", ""), "orig_chars": len(orig), "comp_chars": len(comp),
                    "ratio": round(len(comp) / max(1, len(orig)), 2),
                    "orig_head": orig[:200], "comp_head": comp[:200]})
    return {"model": save_dir, "samples": out}


@app.local_entrypoint()
def test_model(n: int = 5):
    print(json.dumps(infer.remote(n), indent=2, ensure_ascii=False))


@app.function(image=image, gpu="L4", volumes={OUT_DIR: model_vol},
              secrets=[modal.Secret.from_name("hf-token")], timeout=2400)
def ratio_by_length(per_band: int = 12) -> dict:
    """Compress test passages stratified by TOKEN length → is aggressive ratio only on big passages?"""
    import statistics as st
    from collections import defaultdict
    from unsloth import FastLanguageModel
    from datasets import load_dataset
    model_vol.reload()
    save_dir = f"{OUT_DIR}/{MODEL.split('/')[-1].lower()}-compressor"
    model, tokenizer = FastLanguageModel.from_pretrained(model_name=save_dir, max_seq_length=MAX_SEQ,
                                                         load_in_4bit=False, dtype=None)
    FastLanguageModel.for_inference(model)
    bands = [(0, 300), (300, 600), (600, 1000), (1000, 1500), (1500, 99999)]
    buckets = {b: [] for b in bands}
    for r in load_dataset(SFT_REPO, split="test").shuffle(seed=0):
        orig = r["messages"][1]["content"]
        nt = len(tokenizer(orig).input_ids)
        for b in bands:
            if b[0] <= nt < b[1] and len(buckets[b]) < per_band:
                buckets[b].append((r, nt, orig)); break
        if all(len(v) >= per_band for v in buckets.values()):
            break
    rows, by_band, by_dom = [], defaultdict(list), defaultdict(list)
    for b, items in buckets.items():
        for r, nt, orig in items:
            prompt = tokenizer.apply_chat_template(r["messages"][:2], tokenize=False,
                                                   add_generation_prompt=True, enable_thinking=False)
            ids = tokenizer(prompt, return_tensors="pt").to("cuda")
            gen = model.generate(**ids, max_new_tokens=1200, do_sample=False)
            comp = tokenizer.decode(gen[0][ids.input_ids.shape[1]:], skip_special_tokens=True)
            ct = len(tokenizer(comp).input_ids)
            ratio = round(ct / max(1, nt), 3)
            rows.append({"band": f"{b[0]}-{b[1]}", "n_tok": nt, "domain": r.get("domain", ""), "ratio": ratio})
            by_band[f"{b[0]}-{b[1]}"].append(ratio); by_dom[r.get("domain", "")].append(ratio)
    return {"by_band": {k: {"n": len(v), "mean_ratio": round(st.mean(v), 3),
                            "min": min(v), "max": max(v)} for k, v in by_band.items()},
            "by_domain": {k: round(st.mean(v), 3) for k, v in sorted(by_dom.items())},
            "rows": rows}


@app.local_entrypoint()
def ratio_len(per_band: int = 12):
    print(json.dumps(ratio_by_length.remote(per_band), indent=2, ensure_ascii=False))


@app.function(image=image, gpu="L4", volumes={OUT_DIR: model_vol},
              secrets=[modal.Secret.from_name("hf-token")], timeout=1800)
def inspect_short(n: int = 6, max_tok: int = 300) -> dict:
    """Pull FULL outputs for SHORT passages to see WHAT the blowup is (repeat? ramble? no-stop?)."""
    from unsloth import FastLanguageModel
    from datasets import load_dataset
    model_vol.reload()
    save_dir = f"{OUT_DIR}/{MODEL.split('/')[-1].lower()}-compressor"
    model, tokenizer = FastLanguageModel.from_pretrained(model_name=save_dir, max_seq_length=MAX_SEQ,
                                                         load_in_4bit=False, dtype=None)
    FastLanguageModel.for_inference(model)
    out = []
    for r in load_dataset(SFT_REPO, split="test").shuffle(seed=1):
        orig = r["messages"][1]["content"]
        nt = len(tokenizer(orig).input_ids)
        if nt >= max_tok:
            continue
        prompt = tokenizer.apply_chat_template(r["messages"][:2], tokenize=False,
                                               add_generation_prompt=True, enable_thinking=False)
        ids = tokenizer(prompt, return_tensors="pt").to("cuda")
        gen = model.generate(**ids, max_new_tokens=1200, do_sample=False)
        comp = tokenizer.decode(gen[0][ids.input_ids.shape[1]:], skip_special_tokens=True)
        ct = len(tokenizer(comp).input_ids)
        gold = r["messages"][2]["content"] if len(r["messages"]) > 2 else ""
        out.append({"domain": r.get("domain", ""), "n_tok": nt, "out_tok": ct,
                    "ratio": round(ct / max(1, nt), 2), "original": orig,
                    "gold_compressed": gold, "model_output": comp})
        if len(out) >= n:
            break
    return {"samples": out}


@app.local_entrypoint()
def short(n: int = 6):
    print(json.dumps(inspect_short.remote(n), indent=2, ensure_ascii=False))


@app.local_entrypoint()
def smoke():
    print(train.remote(smoke=True))


@app.local_entrypoint()
def run():
    print(train.remote(smoke=False))


@app.function(image=image, volumes={OUT_DIR: model_vol})
def loss_curve() -> dict:
    import glob
    model_vol.reload()
    cks = sorted(glob.glob(f"{OUT_DIR}/ckpt/checkpoint-*"), key=lambda p: int(p.split("-")[-1]))
    if not cks:
        return {"err": "no checkpoint"}
    st = json.load(open(f"{cks[-1]}/trainer_state.json"))
    hist = st.get("log_history", [])
    train = [(round(h["epoch"], 2), h["step"], round(h["loss"], 4)) for h in hist if "loss" in h]
    evals = [(round(h["epoch"], 2), h["step"], round(h["eval_loss"], 4)) for h in hist if "eval_loss" in h]
    return {"checkpoint": cks[-1], "n_train_logs": len(train), "n_evals": len(evals),
            "train_loss": train, "eval_loss": evals,
            "best_eval": min((e[2] for e in evals), default=None)}


@app.local_entrypoint()
def curve():
    print(json.dumps(loss_curve.remote(), indent=2))
