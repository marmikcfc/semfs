"""Fact-verification judge: Qwen3.6-27B-NVFP4 (MTP) via vLLM offline on Modal.

Two-phase to avoid paying for GPU idle during the weight pull:
  1. download_weights()  — CPU container, snapshot_download the NVFP4 checkpoint to a Volume.
  2. verify()            — RTX-PRO-6000 (96GB Blackwell), loads from the Volume (no download),
                           MTP self-speculative decoding for speed.

Shows the judge each (original, compressed) pair, asks whether ANY fact changed/was
removed, parses UNCHANGED/CHANGED, reports percent UNCHANGED per domain + overall.

  modal run benchmarks/modal/verify_facts_qwen.py::main --split train --limit 200 --no-push   # smoke
  modal run benchmarks/modal/verify_facts_qwen.py::main --split train                         # full + push
"""

from __future__ import annotations

import json
import os

import modal

app = modal.App("semfs-verify-qwen")
weights_vol = modal.Volume.from_name("qwen36-weights", create_if_missing=True)
vckpt_vol = modal.Volume.from_name("semfs-verdict-ckpt", create_if_missing=True)
MODELS_DIR = "/models"
VCKPT_DIR = "/vckpt"
MODEL_HF = "unsloth/Qwen3.6-27B-NVFP4"
MODEL_PATH = f"{MODELS_DIR}/qwen36-27b-nvfp4"
GEN_REPO = "pmarmik/semfs-compress-generated-openai"
VERDICT_REPO = "pmarmik/semfs-compress-verdicts"   # separate so all splits share one schema

base_image = (
    modal.Image.from_registry("vllm/vllm-openai:latest",
                              setup_dockerfile_commands=["RUN ln -sf $(which python3) /usr/local/bin/python"])
    .env({"HF_HOME": MODELS_DIR, "HF_HUB_ENABLE_HF_TRANSFER": "1"})
    .pip_install("datasets==2.21.0", "huggingface_hub[hf_transfer]")
    # Modal's injected client deps downgrade typing_extensions below pydantic_core's
    # need (Sentinel) -> vLLM import crashes. Re-pin last (proven fix from glm51_nvfp4_vllm.py).
    .run_commands("python -m pip install --no-deps --force-reinstall typing_extensions==4.15.0")
    .entrypoint([])
)

JUDGE_SYS = ("You are a strict fact-checker comparing an ORIGINAL text with a COMPRESSED version. "
             "The compression may delete redundant words and rephrase. Judge ONLY whether the FACTS are "
             "preserved — numbers, money, dates, percentages, durations, names, places, codes/identifiers, "
             "and factual claims/relationships.\n\n"
             "On the FIRST line respond with EXACTLY one word:\n"
             "UNCHANGED  — all facts present and worded essentially the same\n"
             "EQUIVALENT — all facts preserved but some reworded/reformatted with IDENTICAL meaning "
             "(e.g. '$340 million'->'$340M'; '6,000,000'->'6 million'; 'attained 16 but not 20'->'16-19')\n"
             "CHANGED    — at least one fact was altered, dropped, added, or contradicted "
             "(e.g. a different number/name/date, a removed claim, a flipped relationship)\n"
             "If CHANGED, list each changed/removed fact on the next lines in <=12 words.")


@app.function(image=base_image, volumes={MODELS_DIR: weights_vol},
              secrets=[modal.Secret.from_name("hf-token")], cpu=8.0, memory=32768, timeout=3600)
def download_weights() -> str:
    from huggingface_hub import snapshot_download
    if os.path.exists(os.path.join(MODEL_PATH, "config.json")):
        print(f"weights already present at {MODEL_PATH}")
        return MODEL_PATH
    print(f"downloading {MODEL_HF} -> {MODEL_PATH} ...")
    snapshot_download(MODEL_HF, local_dir=MODEL_PATH)
    weights_vol.commit()
    print("download complete + committed")
    return MODEL_PATH


MAX_LEN = 16384


@app.function(image=base_image, gpu="RTX-PRO-6000", volumes={MODELS_DIR: weights_vol, VCKPT_DIR: vckpt_vol},
              secrets=[modal.Secret.from_name("hf-token")], timeout=6 * 3600)
def verify(splits: str = "train", limit: int | None = None, push: bool = True) -> dict:
    """Load Qwen3.6 ONCE, then judge each split in `splits` (comma-separated) — one model load."""
    from datasets import Dataset, load_dataset
    from vllm import LLM, SamplingParams

    hf_token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
    llm = LLM(model=MODEL_PATH, tensor_parallel_size=1, max_model_len=MAX_LEN,
              gpu_memory_utilization=0.90, dtype="bfloat16", trust_remote_code=True,
              max_num_seqs=256,   # Qwen3.6 hybrid (Mamba): fewer cache blocks at larger ctx
              speculative_config={"method": "mtp", "num_speculative_tokens": 3})   # MTP self-speculation
    tok = llm.get_tokenizer()
    all_results = {}
    for split in [s.strip() for s in splits.split(",") if s.strip()]:
        all_results[split] = _judge_split(split, llm, tok, hf_token, limit, push, load_dataset, Dataset, SamplingParams)
    print(json.dumps(all_results, indent=2))
    return all_results


def _parse_verdict(txt):
    first = (txt.strip().splitlines()[0].upper() if txt.strip() else "")
    if first.startswith("UNCHANGED"):
        return "UNCHANGED"
    if first.startswith("EQUIVALENT"):
        return "EQUIVALENT"
    return "CHANGED"   # CHANGED or unparseable -> conservative


def _judge_split(split, llm, tok, hf_token, limit, push, load_dataset, Dataset, SamplingParams):
    ckpt_path = f"{VCKPT_DIR}/{split}.jsonl"
    done = {}   # uid -> {verdict, change}  (resume: never re-judge a checkpointed row)
    if os.path.exists(ckpt_path):
        with open(ckpt_path) as fh:
            for line in fh:
                try:
                    r = json.loads(line)
                    done[r["uid"]] = r
                except Exception:  # noqa: BLE001
                    pass

    ds = load_dataset(GEN_REPO, split=split)
    rows = [r for r in ds if r.get("status") == "ok" and r.get("compressed", "").strip()]
    if limit:
        rows = rows[:limit]
    by_uid = {r["uid"]: r for r in rows}

    cap = MAX_LEN - 200
    todo_rows, todo_prompts, skipped = [], [], 0
    for r in rows:
        if r["uid"] in done:
            continue
        msgs = [{"role": "system", "content": JUDGE_SYS},
                {"role": "user", "content": f"ORIGINAL:\n{r['original']}\n\nCOMPRESSED:\n{r['compressed']}\n\nDid the compression change or remove any fact?"}]
        p = tok.apply_chat_template(msgs, tokenize=False, add_generation_prompt=True, enable_thinking=False)
        if len(tok.encode(p)) <= cap:
            todo_rows.append(r); todo_prompts.append(p)
        else:
            skipped += 1
    print(f"[{split}] {len(rows)} rows | {len(done)} resumed from ckpt | {len(todo_rows)} to judge | {skipped} skipped(too long)")

    # CHUNKED generate + per-chunk checkpoint+commit -> a kill loses at most one chunk
    CHUNK = 2000
    with open(ckpt_path, "a") as ck:
        for i in range(0, len(todo_rows), CHUNK):
            cr, cp = todo_rows[i:i + CHUNK], todo_prompts[i:i + CHUNK]
            outs = llm.generate(cp, SamplingParams(temperature=0.0, max_tokens=160))
            for r, o in zip(cr, outs):
                txt = o.outputs[0].text
                v = _parse_verdict(txt)
                rec = {"uid": r["uid"], "verdict": v, "change": "" if v != "CHANGED" else txt.strip()[:300]}
                done[r["uid"]] = rec
                ck.write(json.dumps(rec) + "\n")
            ck.flush()
            vckpt_vol.commit()
            print(f"[{split}] checkpointed {min(i + CHUNK, len(todo_rows))}/{len(todo_rows)}")

    judged = [(by_uid[u], rec["verdict"], rec.get("change", "")) for u, rec in done.items() if u in by_uid]
    by_dom = {}
    for r, v, _ in judged:
        c = by_dom.setdefault(r["domain"], {"UNCHANGED": 0, "EQUIVALENT": 0, "CHANGED": 0})
        c[v] += 1
    per_domain = {}
    for dom, c in by_dom.items():
        n = sum(c.values())
        per_domain[dom] = {"n": n, "pct_fact_preserved": round(100 * (c["UNCHANGED"] + c["EQUIVALENT"]) / n, 1),
                           "pct_changed": round(100 * c["CHANGED"] / n, 1), **c}
    total = len(judged)
    n_pres = sum(v != "CHANGED" for _, v, _ in judged)
    overall = {
        "pct_fact_preserved": round(100 * n_pres / total, 1) if total else 0,
        "pct_strict_unchanged": round(100 * sum(v == "UNCHANGED" for _, v, _ in judged) / total, 1) if total else 0,
        "pct_real_changed": round(100 * (total - n_pres) / total, 1) if total else 0,
    }
    changed_examples = [{"domain": r["domain"], "bucket": r["ratio_bucket"], "orig": r["original"][:180],
                         "comp": r["compressed"][:180], "judge": ch[:200]}
                        for r, v, ch in judged if v == "CHANGED"][:8]

    if push:
        # push to a SEPARATE verdicts dataset (all splits share one schema -> no HF mismatch),
        # and never let a push error crash the judging (verdicts are durable on the Volume).
        try:
            vrows = [{"uid": r["uid"], "domain": r["domain"], "ratio_bucket": r["ratio_bucket"],
                      "fact_verdict": done[r["uid"]]["verdict"],
                      "fact_preserved": done[r["uid"]]["verdict"] != "CHANGED",
                      "fact_changes": done[r["uid"]].get("change", "")}
                     for r in rows if r["uid"] in done]
            Dataset.from_list(vrows).push_to_hub(VERDICT_REPO, split=split, private=False, token=hf_token)
            print(f"[{split}] pushed {len(vrows)} verdicts to {VERDICT_REPO}")
        except Exception as e:  # noqa: BLE001
            print(f"[{split}] verdict push skipped (non-fatal): {type(e).__name__}: {str(e)[:120]}")

    result = {"split": split, "n": total, "skipped_too_long": skipped, "overall": overall,
              "per_domain": per_domain, "changed_examples": changed_examples}
    print(json.dumps(result, indent=2))
    return result


@app.local_entrypoint()
def main(splits: str = "train", limit: int = 0, push: bool = True):
    download_weights.remote()                       # phase 1: CPU pull -> Volume (idempotent)
    verify.remote(splits=splits, limit=(limit or None), push=push)   # phase 2: GPU, ONE load for all splits