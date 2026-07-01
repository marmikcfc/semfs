# Compressor-model fine-tune (Bear-style fact-preserving LoRA)

**Linear:** [SEM-41](https://linear.app/semfs/issue/SEM-41/compressor-model-fine-tune-bear-style-fact-preserving-lora-pipeline) · **Started** ~2026-06-25 · **Status:** blocked on CCE

## Goal
Train a small LoRA "compressor": shrink long LLM input text while preserving **every fact**
(Bear-style, thetokencompany.com/blog/coqa). Distill from a teacher (gpt-4.1-mini), fact-verify,
fine-tune a small student on Modal with Unsloth. Use case: "replace Claude compact / shrink long
inputs", 7 domains: legal, medical, financial, meetings, calls, web, chat.

## Pipeline & datasets (HF, public)
| Stage | Dataset | What |
| -- | -- | -- |
| 1 sources | `pmarmik/semfs-compress-sources-phase1` | ~24.5K source texts, 7 domains |
| 2 teacher pairs | `pmarmik/semfs-compress-generated-openai` | gpt-4.1-mini compressions, mixed ratio buckets |
| 3 verdicts | `pmarmik/semfs-compress-verdicts` | Qwen3.6-27B fact judge (UNCHANGED/EQUIVALENT/CHANGED) |
| 4 **final SFT** | **`pmarmik/semfs-compress-sft`** | **fact-preserved only, `messages` schema, Unsloth-ready** |

Final SFT: train **19,254** (dropped 1,746) · val **1,660** (90) · test **1,665** (85). ~91.7% keep.
Per-domain balanced (~2.5–3K each in train; medical/meetings smallest = hardest).

## Codebase — `benchmarks/modal/`
- `assemble_compress_sources.py` — assemble the 7-domain source corpus → HF.
- `generate_compress_openai.py` — teacher generation (gpt-4.1-mini), per-batch checkpoint + HF push.
- `verify_facts_qwen.py` — Qwen3.6-27B-NVFP4 judge (vLLM/MTP on RTX-PRO-6000), chunked checkpoint.
- `build_sft_dataset.py` — join generated ⋈ verdicts, keep `fact_preserved==True` → `…-compress-sft`.
- `train_unsloth_compressor.py` — **the LoRA trainer** (`::smoke` / `::run`). 16-bit LoRA r=32/α=64,
  `train_on_responses_only`, early-stop, saves merged model → Volume `semfs-compressor-model`.
- `smoke_vision_qwen35.py` — FastVisionModel smoke (multimodal Qwen3.5 variants).
- `token_stats_compressor.py` — token accounting (37.5M tokens, mean 1,947, 2.7% > 4096).

## Key decisions
- Teacher = gpt-4.1-mini (GLM-5.1 thinking-ON abandoned: runaway reasoning).
- Method = **16-bit LoRA** (not QLoRA — base ≤1.7B fits bf16; no quant-loss/dequant overhead).
- Judge = Qwen3.6-27B-NVFP4; ~92% of teacher compressions preserve facts.

## ⛔ BLOCKER — Unsloth CCE will not engage → ~45–56 s/step
The full `[seq × vocab]` fp32 logit tensor materializes (Qwen3.5 248K vocab → 3.78 GiB/microstep)
instead of CCE's online softmax. Log always: `Unsloth: Will smartly offload gradients to save VRAM!`.
Seq 4096 → ~37 hr/2 epochs; the 2-hr target needs CCE (~11,736 tok/s → ~1.5 hr).

Ruled out (data): model (Qwen3.5-0.8B / Qwen3-1.7B / Qwen3.5-2B), loader (FastLanguageModel /
FastVisionModel), GPU (L4 62 s/step ≈ A100 55 s/step), torch (2.8 ≈ 2.10), transformers (~5.5 & 5.2
both slow; 5.12 won't build — unsloth 2026.6.9 caps `transformers<=5.5.0`). → CCE-off is a
**code/config issue in the loss-invocation path**, not versions/model/GPU.

Gotchas: Qwen3.5 series is multimodal (needs FastVisionModel+torchvision); git unsloth HEAD needs git
unsloth_zoo (`device_type`) which conflicts with stable torch; the "5× faster" FastVisionModel result
was a config artifact (max_len 2048 + batch 2), not the loader.

## Current state / next
- `pmarmik/semfs-compress-sft` DONE; trainer simplified to one-line `load_dataset`.
- **Codex source-level CCE diagnostic running** (no GPU) — find the exact CCE gate + minimal fix.
- After fix: smoke (confirm ~2–3 s/step, no offload) → full ~1.5 hr run on L4 (~$1.2).
- Student model TBD once CCE engages: Qwen3-1.7B (closest gen, text) vs Qwen2.5-1.5B (mature).
