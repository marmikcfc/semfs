# RCA: `--enforce-eager` collapses vLLM decode throughput on the KG-extraction serve

- **Date:** 2026-06-19
- **Component:** benchmarks/modal — Gemma-4-31B-IT-NVFP4 vLLM serve (`gemma4_31b_nvfp4_vllm.py`), used as the KG entity-extraction LLM for `build_kg`
- **Status:** RESOLVED (flag removed; serve redeployed 4×B200 with CUDA graphs)
- **Severity:** Performance (≈2–3× slower than necessary on an already-expensive B200; turned a ~1.5h job into a ~5h job)

## Summary

The KG-generation run over the 6 seeds was crawling: ~1,400 tok/s generation on a B200,
~26% done in 1h45m, ~5h ETA. Root cause was **`--enforce-eager` in the vLLM serve command**,
which disables CUDA graphs. For LLM **decode** (1 token/step), the per-step work is tiny but
the CPU must dispatch hundreds of GPU kernels individually — so the GPU idles between launches
and decode throughput collapses. The flag had been copied from the GLM NVFP4 serve as a
"faster cold-start" habit, never re-evaluated for a multi-hour throughput batch.

## Symptom

- 1×B200 serving Gemma-4-31B-IT-NVFP4 produced only **~1,400 tokens/s** generation.
- KG run: ~26% of 35,079 files in ~105 min → ~5h projected to finish.
- Adding client workers did not help (already saturating the single instance).

## Investigation (data flow → hypothesis → evidence)

Pulled the live vLLM engine logs from the serve:

```
Running: 59 reqs, Waiting: 0 reqs
GPU KV cache usage: 17.7%                 ← memory 82% FREE
Avg generation throughput: ~1,400 tokens/s
```

Two facts ruled out the obvious culprits and pointed at launch overhead:

1. **Not memory-bound.** KV-cache usage was only **17.7%** at 59 concurrent requests — by
   memory alone the GPU could hold ~300+ concurrent. So "add more instances / raise
   max_num_seqs / use the spare VRAM" would NOT help (more concurrency just splits the same
   compute; extra instances also duplicate the 21 GB weights for zero gain).
2. **Throughput too low for the hardware.** ~1,400 tok/s is far below what a B200 (8 TB/s
   HBM3e) should do on a 31B NVFP4 model — implying the GPU was **stalling between kernels**,
   i.e. launch/dispatch-bound, not compute- or bandwidth-bound.

External corroboration: aphrodite-engine (shares vLLM's CUDA-graph mechanism) issue #1114
reports the same `enforce_eager` → throughput-drop relationship.

## Root cause

`--enforce-eager` forces PyTorch eager execution and **disables CUDA graphs**.

- Generating one token through a 31B model fires **hundreds of GPU kernels** (per-layer
  matmuls + attention × ~48 layers). In decode the batch advances 1 token/step, so each
  kernel's math is small.
- In **eager mode** the CPU launches each kernel one-by-one every step; the GPU waits on the
  CPU between launches. With small per-kernel work, this fixed dispatch overhead dominates →
  the GPU is half-idle → decode throughput collapses.
- **CUDA graphs** capture the whole decode step into one replayable graph; each step is a
  single replay with no per-kernel CPU chatter → typically **~2–3× decode throughput**.

Why the flag was there: it was copied verbatim from `glm51_nvfp4_vllm.py` (comment: "skip
torch.compile … same reasoning as FP8 path"), where it was a deliberate **startup-time /
NVFP4-capture-safety** choice. It was never re-justified for the KG **batch-throughput**
workload, where steady-state speed matters far more than a few minutes of cold start.

## Why `--enforce-eager` should be OFF (for this workload)

- The job is **decode-throughput-bound over many hours**; CUDA graphs are exactly the
  mechanism that removes the decode bottleneck.
- The one-time graph-capture cost (~minutes at boot) is negligible against a multi-hour run.
- The GPU is expensive (B200 ~$6.25/hr); running it at ~⅓ its decode rate wastes both time
  and money — total cost scales with wall-clock.

## When `--enforce-eager` is legitimately ON

Keep it (or accept it) when:
- **Short / smoke runs** where cold-start time dominates total time.
- **Quant/kernel incompatibility** — some quantization kernels can't be captured into CUDA
  graphs (this is the real risk for **NVFP4**; it's plausibly why the GLM path used eager).
  If graph capture fails at boot, eager is the safe fallback.
- Highly **dynamic shapes** that defeat graph capture.

→ Treat removal as **try-and-verify**: drop the flag, confirm graph capture succeeds at boot
(watch for "capturing cuda graph" → "Application startup complete" with no capture errors),
and revert only if NVFP4 + graphs throws.

## Resolution

- Removed `--enforce-eager` from `serve()` in `benchmarks/modal/gemma4_31b_nvfp4_vllm.py`.
- Simultaneously scaled the serve to **4×B200 data-parallel replicas** (`N_REPLICAS=4`,
  `min/max_containers=4`; Modal load-balances across them) since the GLM 4×B200 quota was
  free (that app was stopped). The model is only ~21 GB, so 1 replica per B200 (TP=1) is
  optimal — packing multiple instances per GPU would duplicate weights and split compute.
- Expected combined speedup: CUDA graphs (~2–3×/GPU) × 4 replicas ≈ **~8–12×**.
- The KG build is resume-safe (`SEMFS_KG_RESUME=1` + per-doc incremental commit, see the
  separate build_kg robustness work), so the ~26% already committed is preserved across the
  serve redeploy + worker relaunch.

## Measured outcome (2026-06-20) — the hypothesis did NOT hold

After redeploying WITHOUT `--enforce-eager` (CUDA graphs captured cleanly — NVFP4+graphs worked,
~9s capture, 0.25 GiB) and scaling to 4×B200, the engine logs showed:

```
eager (before):   ~1,400 tok/s per engine @ 59 concurrent
graphs (after):   ~1,300 tok/s per engine @ ~56 concurrent   ← essentially FLAT
```

**CUDA graphs gave ~no per-GPU speedup here.** At batch ~56 the GPU is doing enough work per decode
step that the kernel-launch overhead is already amortized — so it was **compute-bound, not
launch-bound**, and graphs (which mainly help at *small* batch) added nothing. The aphrodite #1114
report is real but applies to the low-batch/low-concurrency regime, not our saturated batch.

The actual ~3.3× speedup of the KG run came **entirely from 4× data-parallel B200 replicas**
(Modal load-balanced). So: removing `--enforce-eager` was harmless/correct (it can't hurt and helps
at low batch), but it was NOT the lever — **GPU count was.** Lesson: validate the "graphs will fix
it" hypothesis with the engine's own throughput metric at the *actual* batch size before attributing
a win to it.

## Lessons / preventics

1. **Don't copy serve flags across workloads without re-justifying them.** `--enforce-eager`
   was right for GLM's interactive/smoke use and wrong for a KG throughput batch.
2. **Diagnose with the engine's own metrics.** KV-cache-usage % instantly distinguished
   memory-bound (false) from launch/compute-bound (true) and prevented the wrong fix
   (more VRAM / more instances per GPU).
3. **Memory headroom ≠ throughput.** When compute/launch-bound, free VRAM buys nothing;
   scale across GPUs (data-parallel), not within one.
4. For future self-hosted serves powering batch jobs: default to **CUDA graphs ON**, only
   reaching for `--enforce-eager` as a capture-failure fallback.
