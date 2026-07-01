# Recovering `cached_tokens` (KV / prefix-cache accounting) — investigation + fix

**Status:** vLLM-side fix MADE + CONFIRMED end-to-end (incl. real codex binary). GLM/litellm-bridge link still UNVERIFIED. Not deployed persistently (probe ran ephemerally; live deploy was stopped to save GPU). · **Dates:** 2026-06-28/29 · **Backend:** Gemma-4-31B-NVFP4 on Modal vLLM (test vehicle; quality irrelevant — only cache surfacing).

---

## TL;DR
- Our runs showed `cache_read=0` even though vLLM prefix caching was ON (~96.8% engine hit rate). Cause: vLLM was never told to **report** cache hits, and the count has to survive a 3-link chain.
- **Fix (Gemma path):** add `--enable-prompt-tokens-details` to the vLLM serve cmd. One flag. Confirmed it surfaces `cached_tokens`.
- **Confirmed end-to-end with the real `codex` binary:** run 1 (cold) `cached_input_tokens=0`; run 2 (warm) `cached_input_tokens=8256/8278` (99.7%).
- **Key nuance:** caching does NOT reduce the token COUNT. vLLM always reports full `prompt_tokens`; `cached_tokens` is an informational sub-field. So on a **token metric** nothing changes (all prior results stand); it only matters on a **compute/$ metric**.
- **Past runs are NOT exactly recoverable** (never emitted; per-turn usage not stored; engine counters reset). But reconstructable as an estimate (~96–97%), validated against the 96.8% probe and the 98–99.7% live measurements.

---

## Root cause — the 3-link chain
```
Codex ──/v1/responses──▶ LiteLLM proxy ──/v1/chat/completions──▶ vLLM (APC on)
```
1. **vLLM** — APC on by default (V1), but without `--enable-prompt-tokens-details` it never *reports* `prompt_tokens_details.cached_tokens`. ← primary drop point (now fixed for Gemma).
2. **LiteLLM bridge** (GLM path only) — responses↔chat translation must map `prompt_tokens_details.cached_tokens` → `input_tokens_details.cached_tokens` (open gap: litellm#22984). UNVERIFIED.
3. **Harness** — `openclaw.py:329` reads `prompt_tokens_details.cached_tokens`; the WB codex harness reads codex's `cached_input_tokens` (per its test fixture). Correct; just never received a non-zero value.

Grounding (official docs):
- vLLM V1 enables APC by default; disable with `--no-enable-prefix-caching`.
- `--enable-prompt-tokens-details` makes the OpenAI server report `usage.prompt_tokens_details.cached_tokens`.
- Known bugs: vllm#16162 (`--enable-prompt-tokens-details` flaky in V1), litellm#22984 (cached_tokens not carried from vLLM).

## The change made
`benchmarks/modal/gemma4_31b_nvfp4_vllm.py:124` — added `--enable-prompt-tokens-details` to the `serve()` vLLM command. GPU-agnostic (applies on B200 or RTX-PRO-6000; `GEMMA_GPU` env already supported, default B200). NOT yet added to the GLM serve (`glm51_nvfp4_vllm.py`).

## Confirmations (all on the real Gemma-4-31B-NVFP4 + the flag)
| Surface | Result |
|---|---|
| raw `/v1/chat/completions` | `prompt_tokens_details.cached_tokens = 1792 / 1827` (98%) |
| `/v1/responses` (codex's transport) | `input_tokens_details.cached_tokens = 1792 / 1818` |
| **real `codex` CLI binary (v0.141)** | run1 cold `cached_input_tokens=0`; run2 warm **`cached_input_tokens=8256 / 8278`** (99.7%) |

Gemma's NATIVE `/v1/responses` works (no litellm needed); GLM's crashes on tool calls → that's why GLM uses the litellm bridge.
Probe scripts: `scratchpad/{gemma_cache_probe,responses_cache_probe,codex_cache_test}.py` (key handled server-side inside Modal; never materialized locally — auto-mode classifier enforced this).

## Estimate for PAST runs (not exact — reconstruction)
Recorded value is gone (never emitted; only aggregate usage stored; vLLM `/metrics` reset). Reconstructed from the **append-only** structure: on turn N the whole turn-(N-1) prompt is an exact prefix ⇒ `cached = P − p_T`; with only `(P=Σprompt, T=calls)` and ~linear growth ⇒ **`cached ≈ P·(T−1)/(T+1)`**, `uncached ≈ 2P/(T+1)`.

Result: **~96–97% cached across every arm/persona/rep** (per-persona and per-rep tables computed in this session). Corroborated by the independent 96.8% engine probe and the 98–99.7% live measurements. → reported "total tokens" overstate true GPU cost ~30×.

ppr_map exact component: the 5,415-tok map is a fixed prefix → guaranteed hit every turn after #1 = **14.07M cached tokens (~32% of ppr_map's prompt budget)** ⇒ ppr_map's token penalty is ~entirely cache-served (a billing artifact, as concluded). On a token metric ppr_map still costs more; on a compute metric ppr_map ≈ ppr_on.

## Remaining work
- [ ] **GLM path:** verify the litellm responses↔chat bridge carries `cached_tokens` (litellm#22984); add `--enable-prompt-tokens-details` to `glm51_nvfp4_vllm.py`.
- [ ] **Harness:** ensure the responses path reads `input_tokens_details.cached_tokens` (codex records `cached_input_tokens`; openclaw proxy reads chat-shape `prompt_tokens_details`).
- [ ] **Deploy persistence:** the Gemma flag change is in source but the live deploy was STOPPED (GPU off). Redeploy when a real run needs it.
- [ ] Optional: add vLLM `/metrics` prefix-cache gauges (`gpu_prefix_cache_hits/queries`) to the run scrape for an engine-level hit rate per run.

## Cost / ops notes
- All probe apps ephemeral + stopped. Live Gemma deploy STOPPED (`modal app stop gemma4-31b-nvfp4-vllm`). `serve()` defaults to `min_containers=4` (no scale-to-zero) → always pass `GEMMA_REPLICAS=1` for tests.
- Metric directive stands: track TOKEN usage, not $ (so this recovery is a *second* axis, never a reduction of the token number).
