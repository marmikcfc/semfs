"""Generate the compression SFT pairs (Bear-style EXTRACTIVE deletion).

GLM-5.1 deletes redundant words/phrases while keeping every kept word VERBATIM and
the text grammatical — the output is a subsequence of the input, so it cannot
hallucinate or corrupt a fact. Few-shot examples anchor the deletion behavior.
Thinking is ON (bigger token budget so the reasoning trace finishes and content
isn't truncated into cut-off statements); the trace is stored in `reasoning`.

TIGHT LEASH on the (spot, preemption-prone) B200: rows are processed in BATCHES of
BATCH_SIZE. Between every batch we (1) confirm GLM /health is 200 and read the
preemption counter, pausing if it's down; (2) checkpoint the batch to a Modal
Volume and commit — so a preemption costs ONE batch, not the run, and is caught
within one batch. Re-running resumes from the checkpoint (skips done uids).

Inline gates are CHEAP only (no GPU/embedder): preserve-exact + subsequence ratio.
The voyage-4-nano embedding gate runs SEPARATELY (gate_embeddings.py) after GLM is
shut down.

  modal run benchmarks/modal/generate_compress_glm.py::smoke
  modal run benchmarks/modal/generate_compress_glm.py::run_full
"""

from __future__ import annotations

import asyncio
import json
import os
import re

import modal

app = modal.App("semfs-compress-generate")

image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets==2.21.0", "huggingface_hub", "httpx", "tiktoken")
)
ckpt_vol = modal.Volume.from_name("semfs-compress-ckpt", create_if_missing=True)

SOURCE_REPO = "pmarmik/semfs-compress-sources-phase1"
GEN_REPO = "pmarmik/semfs-compress-generated-phase1"
GLM_BASE = "https://ada-diffusion-llm--glm51-nvfp4-vllm-serve.modal.run"
GLM_MODEL = "glm-5.1-nvfp4"

BATCH_SIZE = int(os.environ.get("BATCH_SIZE", "40"))     # rows per batch == concurrency (max_inputs=64)
POLL_S = float(os.environ.get("POLL_S", "1.5"))          # health poll cadence
TEMPERATURE = float(os.environ.get("TEMPERATURE", "0.3"))

EXTRACTIVE_SYSTEM = """You compress text by DELETING redundant words and phrases. You NEVER reword, paraphrase, abbreviate, reorder, or add anything: every word you keep must appear EXACTLY as in the original, in the same order. The output is the original text with deletions only, and it must still read as grammatical English.

DELETE: articles (a/an/the) where the sentence still reads; qualifiers and hedges (proposed, approximate/approximately, detailed, essentially, broadly, fairly, somewhat); filler (just, really, basically, actually, simply, generally); pleasantries; redundant back-references and clauses that merely restate known context; procedural filler; repeated information.

KEEP EXACTLY (never delete or alter): every numeric value, money amount, date, percentage, duration; all proper nouns (people, companies, products, places); decisions, outcomes, and action items; speaker labels; code, inline code, URLs, file paths, commands; technical terms.

RULES:
- Output ONLY the compressed text (the original minus deletions). No preamble, no commentary, no quotes.
- Do NOT change spelling, tense, capitalization, word order, or punctuation of the words you keep.
- Stay grammatical: do not delete a word if it makes the sentence ungrammatical or ambiguous.
- Preserve EVERY fact. When unsure whether something is redundant, keep it.
- Target: keep about {keep_pct}% of the original length; delete the rest.

EXAMPLES (deletion only; every kept word is verbatim from the original):

Original: The board of directors met on Tuesday to discuss the proposed acquisition of Meridian Systems, which had been under review since the beginning of the quarter. CFO Laura Chen presented a detailed financial analysis showing that the deal, valued at approximately $340 million, would be accretive to earnings within 18 months.
Compressed: The board met on Tuesday to discuss the acquisition of Meridian Systems. CFO Laura Chen presented a financial analysis showing the deal, valued at $340 million, would be accretive within 18 months.

Original: The company, which was founded in 1998, announced on Monday that it would be laying off approximately 500 employees as part of a broader restructuring effort.
Compressed: The company, founded in 1998, announced Monday it would be laying off 500 employees as part of a restructuring effort.

Original: The researchers, who had spent nearly a decade working on the project, finally published their groundbreaking findings in the journal Nature last week.
Compressed: The researchers published their findings in the journal Nature last week.

Original: To install the package, you should simply run the command `npm install left-pad`, and then you will need to make sure to restart the development server.
Compressed: To install the package, run the command `npm install left-pad`, then restart the development server.

Original: Project Manager: We should, you know, probably just finalize the budget of $50,000 by Friday or so.
Compressed: Project Manager: We should finalize the budget of $50,000 by Friday."""

# ---- cheap inline gates ----------------------------------------------------
_NUM = re.compile(r"\d[\d,]*\.?\d*%?")
_URL = re.compile(r"https?://\S+")
_CODE = re.compile(r"`([^`]+)`")
_WORD = re.compile(r"\w+")
_P = re.compile(r"vllm:num_preemptions_total\{[^}]*\}\s+([0-9.eE+-]+)")


def preserve_check(orig: str, comp: str):
    nums = set(_NUM.findall(orig))
    num_ratio = (sum(n in comp for n in nums) / len(nums)) if nums else 1.0
    urls_ok = all(u in comp for u in set(_URL.findall(orig)))
    code_ok = all(c in comp for c in set(_CODE.findall(orig)))
    return (num_ratio >= 0.9 and urls_ok and code_ok), round(num_ratio, 3)


def subseq_ratio(orig: str, comp: str):
    """Fraction of compressed words that appear, in order, in the original.
    ~1.0 => purely extractive (deletion only); low => model paraphrased."""
    o = _WORD.findall(orig.lower())
    c = _WORD.findall(comp.lower())
    if not c:
        return 0.0
    i = matched = 0
    for w in c:
        while i < len(o) and o[i] != w:
            i += 1
        if i < len(o):
            matched += 1
            i += 1
    return round(matched / len(c), 3)


async def call_glm(client, key, system, user, max_out, enable_thinking):
    payload = {
        "model": GLM_MODEL,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
        "temperature": TEMPERATURE, "top_p": 0.95, "max_tokens": max_out,
        "chat_template_kwargs": {"enable_thinking": enable_thinking},
    }
    for attempt in range(4):
        try:
            r = await client.post(f"{GLM_BASE}/v1/chat/completions",
                                  headers={"Authorization": f"Bearer {key}"}, json=payload)
            r.raise_for_status()
            ch = r.json()["choices"][0]
            msg = ch["message"]
            reasoning = msg.get("reasoning") or msg.get("reasoning_content") or ""
            return (msg.get("content") or ""), reasoning, ch.get("finish_reason")
        except Exception as e:  # noqa: BLE001
            if attempt == 3:
                return None, f"ERROR: {type(e).__name__}: {str(e)[:120]}", None
            await asyncio.sleep(2 * (attempt + 1))   # ride out cold-start / 303 re-route windows


@app.function(image=image,
              secrets=[modal.Secret.from_name("glm-vllm-key"), modal.Secret.from_name("hf-token")],
              volumes={"/ckpt": ckpt_vol}, cpu=4.0, memory=8192, timeout=12 * 3600)
def generate(split: str = "train", per_domain: int = 4, push: bool = False, thinking: bool = True) -> dict:
    import httpx
    import tiktoken
    from datasets import Dataset, load_dataset

    enc = tiktoken.get_encoding("o200k_base")
    ntok = lambda s: len(enc.encode(s, disallowed_special=()))
    glm_key = os.environ["MODAL_VLLM_API_KEY"]
    hf_token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
    ckpt_path = f"/ckpt/{split}.jsonl"

    ds = load_dataset(SOURCE_REPO, split=split)
    by_dom = {}
    for r in ds:
        by_dom.setdefault(r["domain"], []).append(r)
    rows = []
    for items in by_dom.values():
        rows.extend(items[:per_domain] if per_domain else items)
    for r in rows:
        r["uid"] = f'{r["doc_id"]}#{r["chunk_idx"]}'

    done = {}
    if os.path.exists(ckpt_path):
        with open(ckpt_path) as fh:
            for line in fh:
                try:
                    rec = json.loads(line)
                    done[rec["uid"]] = rec
                except Exception:  # noqa: BLE001
                    pass
    todo = [r for r in rows if r["uid"] not in done]
    n_batches = (len(todo) + BATCH_SIZE - 1) // BATCH_SIZE
    print(f"split={split}: {len(rows)} rows, resumed {len(done)} from ckpt, {len(todo)} to do "
          f"in {n_batches} batches of {BATCH_SIZE} (thinking={'ON' if thinking else 'OFF'}, extractive)")

    async def wait_healthy(client, max_wait=900):
        waited = 0.0
        while waited < max_wait:
            try:
                h = await client.get(f"{GLM_BASE}/health",
                                     headers={"Authorization": f"Bearer {glm_key}"}, timeout=15)
                if h.status_code == 200:
                    m = await client.get(f"{GLM_BASE}/metrics",
                                         headers={"Authorization": f"Bearer {glm_key}"}, timeout=15)
                    return float(_P.search(m.text).group(1)) if _P.search(m.text) else -1.0
            except Exception:  # noqa: BLE001
                pass
            print(f"  [leash] GLM unhealthy, waiting... ({waited:.0f}s)")
            await asyncio.sleep(POLL_S * 2)
            waited += POLL_S * 2
        return None

    async def process_row(row, client):
        keep_pct = int(float(row["ratio_bucket"]) * 100)
        system = EXTRACTIVE_SYSTEM.format(keep_pct=keep_pct)
        n_in = row["n_tokens"]
        max_out = min(16384, int(n_in * 1.3) + 8192)
        comp, reasoning, finish = await call_glm(client, glm_key, system, "Compress:\n" + row["text"], max_out, thinking)
        status = (reasoning if comp is None else ("empty_content" if not comp.strip() else "ok"))
        rec = {"uid": row["uid"], **{k: row[k] for k in ("domain", "doc_id", "length_class", "ratio_bucket")},
               "original": row["text"], "n_tokens_in": n_in, "compressed": comp or "",
               "reasoning": reasoning if comp is not None else "", "finish_reason": finish, "status": status}
        if status == "ok":
            pres, num_ratio = preserve_check(row["text"], comp)
            rec["n_tokens_out"] = ntok(comp)
            rec["achieved_ratio"] = round(rec["n_tokens_out"] / max(1, n_in), 3)
            rec["gate_preserve"] = pres
            rec["num_ratio"] = num_ratio
            rec["subseq_ratio"] = subseq_ratio(row["text"], comp)
        else:
            rec.update({"n_tokens_out": 0, "achieved_ratio": 0.0, "gate_preserve": False,
                        "num_ratio": 0.0, "subseq_ratio": 0.0})
        return rec

    async def driver():
        produced, prev_p = [], None
        async with httpx.AsyncClient(timeout=300, follow_redirects=True) as client:
            with open(ckpt_path, "a") as ckpt:
                for bi in range(n_batches):
                    batch = todo[bi * BATCH_SIZE:(bi + 1) * BATCH_SIZE]
                    preempt = await wait_healthy(client)                 # leash: gate before batch
                    if preempt is None:
                        print("  [leash] ABORT: GLM unhealthy > max_wait; progress checkpointed")
                        break
                    if prev_p is not None and preempt > prev_p:
                        print(f"  [leash] preemptions rose {prev_p}->{preempt}")
                    prev_p = preempt
                    recs = await asyncio.gather(*[process_row(r, client) for r in batch])
                    for rec in recs:
                        ckpt.write(json.dumps(rec) + "\n")
                    ckpt.flush()
                    ckpt_vol.commit()
                    produced.extend(recs)
                    nok = sum(r["status"] == "ok" for r in recs)
                    nerr = sum(r["status"] not in ("ok", "empty_content") for r in recs)
                    print(f"  [batch {bi + 1}/{n_batches}] ok={nok} err={nerr} empty={len(recs)-nok-nerr} "
                          f"preempt={preempt} done={len(done)+len(produced)}/{len(rows)}")
                    if nerr > len(recs) * 0.3:
                        print("  [leash] high batch error rate -> rechecking GLM health")
                        await wait_healthy(client)
        return produced

    produced = asyncio.run(driver())
    out = list(done.values()) + produced

    ok = [r for r in out if r["status"] == "ok"]
    subs = sorted(r["subseq_ratio"] for r in ok)
    ratios = sorted(r["achieved_ratio"] for r in ok) or [0]
    rlens = sorted(ntok(r["reasoning"]) for r in ok) or [0]
    stats = {
        "n": len(out), "glm_ok": len(ok),
        "empty_content": sum(r["status"] == "empty_content" for r in out),
        "glm_errors": sum(r["status"] not in ("ok", "empty_content") for r in out),
        "finish_length": sum(r.get("finish_reason") == "length" for r in out),
        "fail_preserve": sum(not r["gate_preserve"] for r in ok),
        "subseq_min": subs[0] if subs else 0, "subseq_median": subs[len(subs) // 2] if subs else 0,
        "subseq_p10": subs[len(subs) // 10] if subs else 0,
        "fully_extractive_pct": round(sum(s >= 0.98 for s in subs) / max(1, len(subs)), 3),
        "achieved_ratio_median": ratios[len(ratios) // 2],
        "reasoning_tok_median": rlens[len(rlens) // 2], "reasoning_tok_max": rlens[-1],
        "batch_size": BATCH_SIZE, "to_do": len(todo), "resumed_from_ckpt": len(done),
    }
    samples = [{"domain": r["domain"], "ratio_bucket": r["ratio_bucket"], "status": r["status"],
                "finish": r.get("finish_reason"), "subseq": r.get("subseq_ratio"),
                "achieved": r.get("achieved_ratio"), "orig": r["original"][:280],
                "reasoning": (r.get("reasoning") or "")[:200], "compressed": r["compressed"][:280]}
               for r in out[:4]]

    if push:
        Dataset.from_list(out).push_to_hub(GEN_REPO, split=split, private=True, token=hf_token)
        stats["pushed_to"] = f"{GEN_REPO}:{split}"

    print(json.dumps({"stats": stats, "samples": samples}, indent=2))
    return {"stats": stats, "samples": samples}


@app.local_entrypoint()
def smoke(split: str = "train", per_domain: int = 3, thinking: bool = True):
    print(json.dumps(generate.remote(split=split, per_domain=per_domain, push=False, thinking=thinking), indent=2))


@app.local_entrypoint()
def run_full(split: str = "train", thinking: bool = False):   # production: thinking OFF (decided 2026-06-25)
    print(json.dumps(generate.remote(split=split, per_domain=0, push=True, thinking=thinking), indent=2))
