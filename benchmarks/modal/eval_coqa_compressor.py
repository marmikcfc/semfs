"""CoQA downstream-QA eval for our Qwen3-1.7B compressor (The Token Company / Bear methodology).

For N conversational-QA instances: compress the PASSAGE with our model (keep questions +
conversation history uncompressed), have a fixed downstream model answer, and LLM-judge the
answer vs gold. Compare COMPRESSED vs an UNCOMPRESSED control — accuracy retained + tokens saved.

  modal run benchmarks/modal/eval_coqa_compressor.py --n 500 --keep-pct 60

Reports: control_acc, compressed_acc, accuracy delta, mean tokens (control vs compressed), tokens saved %.
"""
from __future__ import annotations

import modal

app = modal.App("semfs-eval-coqa")
model_vol = modal.Volume.from_name("semfs-compressor-model")
OUT = "/out"
MODEL_PATH = f"{OUT}/qwen3.5-0.8b-compressor"   # merged Qwen3-1.7B (dir name is stale, weights are 1.7B)

image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("torch==2.8.0", "transformers==5.5.0", "accelerate",
                 "datasets", "openai", "tiktoken", "huggingface_hub", "hf-transfer")
    .env({"HF_HUB_ENABLE_HF_TRANSFER": "1"})
)

DOWNSTREAM = "gpt-4.1-mini"   # fixed answerer for BOTH arms (only the passage differs)
JUDGE = "gpt-4.1-mini"


def compress_system(keep_pct: int) -> str:
    mode = ("Delete redundant words only; keep the rest grammatical." if keep_pct >= 65
            else "Delete redundancy aggressively." if keep_pct >= 45
            else "Compress hard; light rephrasing allowed.")
    return (f"Compress the text to about {keep_pct}% of its length, preserving EVERY fact "
            f"(numbers, money, dates, names, code, claims). {mode} Output only the compressed text.")


@app.function(image=image, gpu="L4", volumes={OUT: model_vol},
              secrets=[modal.Secret.from_name("openai-key")], timeout=3600)
def eval_coqa(n: int = 500, keep_pct: int = 60) -> dict:
    import os
    from concurrent.futures import ThreadPoolExecutor

    import tiktoken
    import torch
    from datasets import load_dataset
    from openai import OpenAI
    from transformers import AutoModelForCausalLM, AutoTokenizer

    # 1) flatten CoQA val -> N conversational-QA instances (with prior turns as history)
    ds = load_dataset("stanfordnlp/coqa", split="validation")
    inst = []
    for ri, row in enumerate(ds):
        qs, gold = row["questions"], row["answers"]["input_text"]
        for i in range(min(len(qs), len(gold))):
            hist = "".join(f"Q: {qs[j]}\nA: {gold[j]}\n" for j in range(i))
            inst.append({"sid": ri, "story": row["story"], "hist": hist, "q": qs[i], "gold": gold[i]})
            if len(inst) >= n:
                break
        if len(inst) >= n:
            break
    stories = {x["sid"]: x["story"] for x in inst}
    print(f"{len(inst)} QA instances across {len(stories)} passages")

    # 2) compress each unique passage with OUR model
    tok = AutoTokenizer.from_pretrained(MODEL_PATH)
    model = AutoModelForCausalLM.from_pretrained(MODEL_PATH, torch_dtype=torch.bfloat16, device_map="cuda")
    model.eval()

    def compress(text: str) -> str:
        msgs = [{"role": "system", "content": compress_system(keep_pct)},
                {"role": "user", "content": "Compress:\n" + text}]
        p = tok.apply_chat_template(msgs, tokenize=False, add_generation_prompt=True, enable_thinking=False)
        ids = tok(p, return_tensors="pt").to("cuda")
        with torch.no_grad():
            out = model.generate(**ids, max_new_tokens=1024, do_sample=False, pad_token_id=tok.eos_token_id)
        return tok.decode(out[0][ids.input_ids.shape[1]:], skip_special_tokens=True).strip()

    comp = {sid: compress(s) for sid, s in stories.items()}
    print("compressed all passages")

    # 3) downstream answer (control + compressed) + judge, concurrently via API
    client = OpenAI(api_key=os.environ["OPENAI_API_KEY"])
    enc = tiktoken.get_encoding("o200k_base")

    def answer(passage: str, hist: str, q: str) -> str:
        sys = "Answer the question using ONLY the passage and prior turns. Give a SHORT answer (a few words)."
        user = f"Passage:\n{passage}\n\n{hist}Q: {q}\nA:"
        r = client.chat.completions.create(model=DOWNSTREAM, temperature=0,
              messages=[{"role": "system", "content": sys}, {"role": "user", "content": user}], max_tokens=40)
        return r.choices[0].message.content.strip()

    def judge(q: str, gold: str, ans: str) -> bool:
        r = client.chat.completions.create(model=JUDGE, temperature=0, max_tokens=3,
              messages=[{"role": "user", "content":
                f"Question: {q}\nGold answer: {gold}\nModel answer: {ans}\n"
                "Is the model answer correct (same meaning as gold)? Reply YES or NO."}])
        return r.choices[0].message.content.strip().upper().startswith("YES")

    def run_one(x):
        ctrl_ans = answer(x["story"], x["hist"], x["q"])
        comp_ans = answer(comp[x["sid"]], x["hist"], x["q"])
        return {
            "ctrl_ok": judge(x["q"], x["gold"], ctrl_ans),
            "comp_ok": judge(x["q"], x["gold"], comp_ans),
            "ctrl_tok": len(enc.encode(x["story"])),
            "comp_tok": len(enc.encode(comp[x["sid"]])),
        }

    with ThreadPoolExecutor(max_workers=24) as ex:
        res = list(ex.map(run_one, inst))

    nq = len(res)
    ctrl_acc = round(100 * sum(r["ctrl_ok"] for r in res) / nq, 1)
    comp_acc = round(100 * sum(r["comp_ok"] for r in res) / nq, 1)
    ctrl_tok = sum(r["ctrl_tok"] for r in res)
    comp_tok = sum(r["comp_tok"] for r in res)
    saved = round(100 * (ctrl_tok - comp_tok) / ctrl_tok, 1)
    out = {"n_questions": nq, "n_passages": len(stories), "keep_pct": keep_pct,
           "control_acc": ctrl_acc, "compressed_acc": comp_acc, "acc_delta": round(comp_acc - ctrl_acc, 1),
           "mean_passage_tokens_control": round(ctrl_tok / nq), "mean_passage_tokens_compressed": round(comp_tok / nq),
           "tokens_saved_pct": saved}
    import json
    print(json.dumps(out, indent=2))
    return out


@app.local_entrypoint()
def main(n: int = 500, keep_pct: int = 60):
    print(eval_coqa.remote(n=n, keep_pct=keep_pct))
