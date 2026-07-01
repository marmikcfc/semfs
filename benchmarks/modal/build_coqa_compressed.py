"""Push our compressor to HF (private) + build the CoQA compression dataset (Phase 1).

push_model : load the merged model from the Volume → push to pmarmik/qwen3-1.7b-compressor (private).
compress   : L4 + vLLM, compress all N CoQA-val passages (one row = one unique passage, NO dedup),
             save original|compressed + the QA pairs → push to pmarmik/coqa-compressed-qwen3-1.7b (private).

Cold-start optimizations: model loaded from the Volume (no download), vLLM compile/CUDA-graph cache
persisted to a Volume (VLLM_CACHE_ROOT) so graphs compile ONCE and are reused, Modal memory snapshot,
and CUDA graphs kept ON (enforce_eager=False — eager collapses throughput per our vLLM RCA).

  modal run benchmarks/modal/build_coqa_compressed.py::push_model
  modal run benchmarks/modal/build_coqa_compressed.py::compress --n 500 --keep-pct 60
"""
from __future__ import annotations

import modal

app = modal.App("semfs-coqa-compress")
model_vol = modal.Volume.from_name("semfs-compressor-model")
vllm_cache = modal.Volume.from_name("semfs-vllm-cache", create_if_missing=True)
OUT = "/out"
MODEL_PATH = f"{OUT}/qwen3.5-0.8b-compressor"   # merged Qwen3-1.7B (dir name stale; weights are 1.7B)
HF_MODEL_REPO = "pmarmik/qwen3-1.7b-compressor"
HF_DATA_REPO = "pmarmik/coqa-compressed-qwen3-1.7b"

push_image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("huggingface_hub[hf_transfer]")
    .env({"HF_HUB_ENABLE_HF_TRANSFER": "1"})
)

# vLLM image + the typing_extensions re-pin (Modal client downgrades it below pydantic_core's need)
vllm_image = (
    modal.Image.from_registry("vllm/vllm-openai:latest",
        setup_dockerfile_commands=["RUN ln -sf $(which python3) /usr/local/bin/python"])
    .env({"HF_HUB_ENABLE_HF_TRANSFER": "1", "VLLM_CACHE_ROOT": "/vllm-cache"})
    .pip_install("datasets", "tiktoken", "huggingface_hub[hf_transfer]")
    .run_commands("python -m pip install --no-deps --force-reinstall typing_extensions==4.15.0")
    .entrypoint([])
)


def compress_system(keep_pct: int) -> str:
    mode = ("Delete redundant words only; keep the rest grammatical." if keep_pct >= 65
            else "Delete redundancy aggressively." if keep_pct >= 45
            else "Compress hard; light rephrasing allowed.")
    return (f"Compress the text to about {keep_pct}% of its length, preserving EVERY fact "
            f"(numbers, money, dates, names, code, claims). {mode} Output only the compressed text.")


@app.function(image=push_image, volumes={OUT: model_vol},
              secrets=[modal.Secret.from_name("hf-token")], cpu=4.0, timeout=1800)
def push_model() -> str:
    import os

    from huggingface_hub import HfApi
    token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
    api = HfApi(token=token)
    api.create_repo(HF_MODEL_REPO, repo_type="model", private=True, exist_ok=True)
    api.upload_folder(folder_path=MODEL_PATH, repo_id=HF_MODEL_REPO, repo_type="model",
                      commit_message="Qwen3-1.7B LoRA compressor (merged 16-bit)")
    print(f"pushed -> https://huggingface.co/{HF_MODEL_REPO} (private)")
    return HF_MODEL_REPO


@app.function(image=vllm_image, gpu="L4", volumes={OUT: model_vol, "/vllm-cache": vllm_cache},
              secrets=[modal.Secret.from_name("hf-token")], enable_memory_snapshot=True, timeout=3600)
def compress(n: int = 500, keep_pct: int = 60) -> dict:
    import os

    import tiktoken
    from datasets import Dataset, load_dataset
    from vllm import LLM, SamplingParams

    token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")

    # one row = one unique CoQA passage (NO dedup — rows are already distinct)
    ds = load_dataset("stanfordnlp/coqa", split="validation")
    rows = [ds[i] for i in range(min(n, len(ds)))]
    print(f"{len(rows)} unique passages to compress")

    # CUDA graphs ON (enforce_eager=False); compile cache persists on /vllm-cache Volume
    llm = LLM(model=MODEL_PATH, dtype="bfloat16", gpu_memory_utilization=0.85,
              max_model_len=4096, enforce_eager=False, trust_remote_code=True)
    tok = llm.get_tokenizer()

    prompts = []
    for r in rows:
        msgs = [{"role": "system", "content": compress_system(keep_pct)},
                {"role": "user", "content": "Compress:\n" + r["story"]}]
        prompts.append(tok.apply_chat_template(msgs, tokenize=False, add_generation_prompt=True,
                                               enable_thinking=False))
    outs = llm.generate(prompts, SamplingParams(temperature=0.0, max_tokens=1024))

    enc = tiktoken.get_encoding("o200k_base")
    data, ratios = [], []
    for i, (r, o) in enumerate(zip(rows, outs)):
        comp = o.outputs[0].text.strip()
        ot, ct = len(enc.encode(r["story"])), len(enc.encode(comp))
        ratios.append(ct / ot if ot else 1.0)
        data.append({"sid": i, "original": r["story"], "compressed": comp,
                     "orig_tokens": ot, "comp_tokens": ct, "ratio": round(ct / ot, 3) if ot else 1.0,
                     "keep_pct": keep_pct, "questions": r["questions"], "answers": r["answers"]["input_text"]})

    Dataset.from_list(data).push_to_hub(HF_DATA_REPO, private=True, token=token)
    vllm_cache.commit()   # persist the compiled CUDA-graph cache for fast future cold starts

    import statistics
    mean_ratio = round(statistics.mean(ratios), 3)
    stats = {"n_passages": len(data), "keep_pct": keep_pct, "mean_ratio": mean_ratio,
             "mean_tokens_saved_pct": round(100 * (1 - mean_ratio), 1),
             "pushed": f"https://huggingface.co/datasets/{HF_DATA_REPO}"}
    print("=== 2 samples ===")
    for d in data[:2]:
        print(f"[orig {d['orig_tokens']}t] {d['original'][:160]}...")
        print(f"[comp {d['comp_tokens']}t] {d['compressed'][:160]}...\n")
    import json
    print(json.dumps(stats, indent=2))
    return stats


stats_image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets", "tiktoken", "huggingface_hub[hf_transfer]")
    .env({"HF_HUB_ENABLE_HF_TRANSFER": "1"})
)


@app.function(image=stats_image, secrets=[modal.Secret.from_name("hf-token")], cpu=4.0, timeout=900)
def token_stats() -> dict:
    import json
    import os

    import tiktoken
    from datasets import load_dataset
    token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
    ds = load_dataset(HF_DATA_REPO, split="train", token=token)
    enc = tiktoken.get_encoding("o200k_base")

    orig = sum(r["orig_tokens"] for r in ds)
    comp = sum(r["comp_tokens"] for r in ds)
    nq = sum(len(r["questions"]) for r in ds)
    qtok = sum(len(enc.encode(q)) for r in ds for q in r["questions"])
    atok = sum(len(enc.encode(a)) for r in ds for a in r["answers"])

    out = {
        "n_passages": len(ds), "n_questions": nq,
        "passages_original_tokens": orig, "passages_compressed_tokens": comp,
        "passages_tokens_saved": orig - comp, "passages_saved_pct": round(100 * (orig - comp) / orig, 1),
        "question_tokens_total": qtok, "answer_tokens_total": atok,
        "mean_q_tokens": round(qtok / nq, 1), "mean_a_tokens": round(atok / nq, 1),
        "mean_orig_passage_tokens": round(orig / len(ds)), "mean_comp_passage_tokens": round(comp / len(ds)),
    }
    print(json.dumps(out, indent=2))
    return out


DOWNSTREAM = "gpt-4.1-mini"   # fixed answerer for BOTH arms — only the passage differs
JUDGE = "gpt-4.1-mini"

eval_image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets", "tiktoken", "openai", "huggingface_hub[hf_transfer]")
    .env({"HF_HUB_ENABLE_HF_TRANSFER": "1"})
)


@app.function(image=eval_image, secrets=[modal.Secret.from_name("openai-key"),
              modal.Secret.from_name("hf-token")], cpu=8.0, timeout=2 * 3600)
def evaluate(n_passages: int = 100) -> dict:
    """Phase 2: downstream QA on compressed vs uncompressed passages + LLM-judge vs gold."""
    import json
    import os
    from concurrent.futures import ThreadPoolExecutor

    import tiktoken
    from datasets import load_dataset
    from openai import OpenAI

    hf = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
    rows = list(load_dataset(HF_DATA_REPO, split="train", token=hf))[:n_passages]

    inst = []
    for r in rows:
        qs, ans = r["questions"], r["answers"]
        for i in range(min(len(qs), len(ans))):
            hist = "".join(f"Q: {qs[j]}\nA: {ans[j]}\n" for j in range(i))   # prior turns (uncompressed)
            inst.append({"orig": r["original"], "comp": r["compressed"], "hist": hist, "q": qs[i], "gold": ans[i]})
    print(f"{len(inst)} questions across {len(rows)} passages")

    client = OpenAI(api_key=os.environ["OPENAI_API_KEY"], max_retries=6)   # auto-backoff on 429
    enc = tiktoken.get_encoding("o200k_base")

    def answer(passage, hist, q):
        sys = "Answer the question using ONLY the passage and prior turns. Give a SHORT answer (a few words)."
        r = client.chat.completions.create(model=DOWNSTREAM, temperature=0, max_tokens=40,
              messages=[{"role": "system", "content": sys},
                        {"role": "user", "content": f"Passage:\n{passage}\n\n{hist}Q: {q}\nA:"}])
        return r.choices[0].message.content.strip()

    def judge(q, gold, a):
        r = client.chat.completions.create(model=JUDGE, temperature=0, max_tokens=3,
              messages=[{"role": "user", "content":
                f"Question: {q}\nGold answer: {gold}\nModel answer: {a}\n"
                "Is the model answer correct (same meaning as gold)? Reply YES or NO."}])
        return r.choices[0].message.content.strip().upper().startswith("YES")

    def run_one(x):
        ca, pa = answer(x["orig"], x["hist"], x["q"]), answer(x["comp"], x["hist"], x["q"])
        return {"ctrl_ok": judge(x["q"], x["gold"], ca), "comp_ok": judge(x["q"], x["gold"], pa),
                "ctrl_tok": len(enc.encode(x["orig"] + x["hist"] + x["q"])),
                "comp_tok": len(enc.encode(x["comp"] + x["hist"] + x["q"]))}

    with ThreadPoolExecutor(max_workers=16) as ex:
        res = list(ex.map(run_one, inst))

    nq = len(res)
    ctrl_acc = round(100 * sum(r["ctrl_ok"] for r in res) / nq, 1)
    comp_acc = round(100 * sum(r["comp_ok"] for r in res) / nq, 1)
    ct, pt = sum(r["ctrl_tok"] for r in res), sum(r["comp_tok"] for r in res)
    out = {"n_questions": nq, "n_passages": len(rows),
           "control_acc": ctrl_acc, "compressed_acc": comp_acc, "acc_delta": round(comp_acc - ctrl_acc, 1),
           "control_prompt_tokens": ct, "compressed_prompt_tokens": pt,
           "tokens_saved_pct": round(100 * (ct - pt) / ct, 1)}
    print(json.dumps(out, indent=2))
    return out


@app.local_entrypoint()
def main(n: int = 500, keep_pct: int = 60):
    print(compress.remote(n=n, keep_pct=keep_pct))


@app.function(image=stats_image, secrets=[modal.Secret.from_name("hf-token")], cpu=2.0, timeout=900)
def bucket_check() -> dict:
    """Verify: what keep_pct values did the model actually train on?"""
    import json
    import os
    from collections import Counter

    from datasets import load_dataset
    token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
    ds = load_dataset("pmarmik/semfs-compress-sft", split="train", token=token)
    kp = Counter(int(round(float(r["ratio_bucket"]) * 100)) for r in ds)
    out = {"n_train": len(ds), "distinct_keep_pct": sorted(kp), "counts": dict(sorted(kp.items()))}
    print(json.dumps(out, indent=2))
    return out


@app.local_entrypoint()
def stats():
    print(token_stats.remote())


@app.local_entrypoint()
def buckets():
    print(bucket_check.remote())


@app.function(image=eval_image, secrets=[modal.Secret.from_name("openai-key"),
              modal.Secret.from_name("hf-token")], cpu=8.0, timeout=2 * 3600)
def analyze_failures(n_passages: int = 40) -> dict:
    """RCA the compression-caused failures: control RIGHT but compressed WRONG -> why?
    Categorize each: DROPPED (fact missing from compressed) | REPHRASED | JUDGE (false failure)."""
    import json
    import os
    from collections import Counter
    from concurrent.futures import ThreadPoolExecutor

    from datasets import load_dataset
    from openai import OpenAI

    hf = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
    rows = list(load_dataset(HF_DATA_REPO, split="train", token=hf))[:n_passages]
    inst = []
    for r in rows:
        qs, ans = r["questions"], r["answers"]
        for i in range(min(len(qs), len(ans))):
            hist = "".join(f"Q: {qs[j]}\nA: {ans[j]}\n" for j in range(i))
            inst.append({"orig": r["original"], "comp": r["compressed"], "hist": hist, "q": qs[i], "gold": ans[i]})
    print(f"{len(inst)} questions across {len(rows)} passages")

    client = OpenAI(api_key=os.environ["OPENAI_API_KEY"], max_retries=6)

    def answer(passage, hist, q):
        r = client.chat.completions.create(model=DOWNSTREAM, temperature=0, max_tokens=40,
              messages=[{"role": "system", "content": "Answer using ONLY the passage and prior turns. SHORT answer."},
                        {"role": "user", "content": f"Passage:\n{passage}\n\n{hist}Q: {q}\nA:"}])
        return r.choices[0].message.content.strip()

    def judge(q, gold, a):
        r = client.chat.completions.create(model=JUDGE, temperature=0, max_tokens=3,
              messages=[{"role": "user", "content": f"Question: {q}\nGold: {gold}\nAnswer: {a}\nCorrect (same meaning)? YES/NO."}])
        return r.choices[0].message.content.strip().upper().startswith("YES")

    def run_one(x):
        ca, pa = answer(x["orig"], x["hist"], x["q"]), answer(x["comp"], x["hist"], x["q"])
        return {**x, "ctrl_ans": ca, "comp_ans": pa,
                "ctrl_ok": judge(x["q"], x["gold"], ca), "comp_ok": judge(x["q"], x["gold"], pa)}

    with ThreadPoolExecutor(max_workers=16) as ex:
        res = list(ex.map(run_one, inst))

    fails = [r for r in res if r["ctrl_ok"] and not r["comp_ok"]]   # compression-CAUSED failures
    print(f"{len(fails)} compression-caused failures (control right, compressed wrong)")

    def categorize(f):
        r = client.chat.completions.create(model=JUDGE, temperature=0, max_tokens=8,
              messages=[{"role": "user", "content":
                f"ORIGINAL passage:\n{f['orig']}\n\nCOMPRESSED passage:\n{f['comp']}\n\n"
                f"Question: {f['q']}\nGold answer: {f['gold']}\nAnswer from compressed: {f['comp_ans']}\n\n"
                "The model answered correctly from ORIGINAL but wrong from COMPRESSED. Why? Reply ONE word:\n"
                "DROPPED (info needed for the answer is missing from COMPRESSED)\n"
                "REPHRASED (info is in COMPRESSED but reworded/ambiguous)\n"
                "JUDGE (the compressed answer is actually correct; the failure is a judging error)"}])
        return r.choices[0].message.content.strip().upper().split()[0]

    with ThreadPoolExecutor(max_workers=16) as ex:
        cats = list(ex.map(categorize, fails))
    breakdown = Counter(c if c in ("DROPPED", "REPHRASED", "JUDGE") else "OTHER" for c in cats)

    samples = [{"q": f["q"], "gold": f["gold"], "comp_ans": f["comp_ans"], "cat": c,
                "compressed": f["comp"][:300]} for f, c in zip(fails, cats)][:6]
    out = {"n_questions": len(res), "n_compression_failures": len(fails),
           "breakdown": dict(breakdown),
           "pct_dropped": round(100 * breakdown["DROPPED"] / max(len(fails), 1), 1),
           "samples": samples}
    print(json.dumps(out, indent=2))
    return out


@app.local_entrypoint()
def eval_phase2(n_passages: int = 100):
    print(evaluate.remote(n_passages=n_passages))


@app.local_entrypoint()
def rca(n_passages: int = 40):
    print(analyze_failures.remote(n_passages=n_passages))
