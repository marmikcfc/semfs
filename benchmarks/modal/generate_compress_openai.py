"""Production compression generation on OpenAI gpt-4.1-mini (abstractive, mixed
buckets: strict-extractive on light buckets, allow-rephrasing on aggressive ones).

DURABILITY (per user: "if we run out of credits, dataset must be on HuggingFace"):
  - every batch is checkpointed to a Modal Volume (resume on restart);
  - the dataset is PUSHED TO HF every PUSH_EVERY batches, on completion, AND
    immediately if OpenAI returns insufficient_quota (out of credits) — so whatever
    is done is always safe on HF.

Inline gates are cheap (preserve-exact numbers/code/URLs + subseq). The voyage
embedding fidelity gate runs separately afterward (gate_embeddings.py / eval).

  modal run benchmarks/modal/generate_compress_openai.py::smoke
  modal run benchmarks/modal/generate_compress_openai.py::run_all
"""

from __future__ import annotations

import asyncio
import json
import os
import re

import modal

app = modal.App("semfs-compress-openai")
image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets==2.21.0", "huggingface_hub", "httpx", "tiktoken")
)
ckpt_vol = modal.Volume.from_name("semfs-compress-ckpt-openai", create_if_missing=True)

SOURCE_REPO = "pmarmik/semfs-compress-sources-phase2"   # LIGHT buckets (keep_pct 80/90)
GEN_REPO = "pmarmik/semfs-compress-generated-phase2"
OPENAI_URL = "https://api.openai.com/v1/chat/completions"
MODEL = "gpt-4.1-mini"

BATCH_SIZE = int(os.environ.get("BATCH_SIZE", "40"))
PUSH_EVERY = int(os.environ.get("PUSH_EVERY", "20"))   # push to HF every N batches
TEMPERATURE = 0.2

EX_MILD = """Original: The board of directors met on Tuesday to discuss the proposed acquisition of Meridian Systems, which had been under review since the beginning of the quarter, valued at approximately $340 million.
Compressed: The board met on Tuesday to discuss the acquisition of Meridian Systems, valued at $340 million."""

# LIGHT example (~85% kept) — for keep_pct >= 80, so the model isn't anchored to the ~50% EX_MILD.
EX_LIGHT = """Original: The board of directors met on Tuesday to discuss the proposed acquisition of Meridian Systems, which had been under review since the beginning of the quarter, valued at approximately $340 million.
Compressed: The board of directors met Tuesday to discuss the proposed acquisition of Meridian Systems, under review since the start of the quarter, valued at $340 million."""


def system_for(keep_pct: int) -> str:
    base = ("You compress text while preserving EVERY fact exactly (numbers, money, dates, percentages, "
            "durations, names, decisions, action items, code, inline code, URLs, file paths, speaker "
            "labels). Output ONLY the compressed text, no preamble or quotes.")
    if keep_pct >= 80:
        mode = ("MODE: MINIMAL EDIT. Remove ONLY filler/redundant words ('very','really','that','in order to'->'to'). "
                "Keep ALL content words, clauses, and sentences. The output MUST be nearly as long as the input "
                f"(about {keep_pct}% of its length) — barely shorter. Do NOT summarize, paraphrase, or drop sentences.")
        ex = EX_LIGHT
    elif keep_pct >= 65:
        mode = "MODE: STRICT EXTRACTIVE. Delete redundant words ONLY; keep remaining words VERBATIM and grammatical."
        ex = EX_MILD
    elif keep_pct >= 45:
        mode = "MODE: AGGRESSIVE EXTRACTIVE. Delete as much redundancy as possible, keep remaining words VERBATIM."
        ex = EX_MILD
    else:
        mode = ("MODE: COMPRESS HARD. Prefer deletion but you MAY lightly rephrase ('in order to'->'to', "
                "merge/drop subordinate clauses, telegraphic phrasing) to maximize reduction, preserving every fact.")
        ex = EX_MILD
    return f"{base}\n\n{mode}\n\nTarget: keep about {keep_pct}% of the length.\n\nEXAMPLE:\n{ex}"


_NUM = re.compile(r"\d[\d,]*\.?\d*%?")
_URL = re.compile(r"https?://\S+")
_CODE = re.compile(r"`([^`]+)`")
_WORD = re.compile(r"\w+")


def preserve_check(orig, comp):
    nums = set(_NUM.findall(orig))
    nr = (sum(n in comp for n in nums) / len(nums)) if nums else 1.0
    return (nr >= 0.9 and all(u in comp for u in set(_URL.findall(orig)))
            and all(c in comp for c in set(_CODE.findall(orig)))), round(nr, 3)


def subseq_ratio(orig, comp):
    o, c = _WORD.findall(orig.lower()), _WORD.findall(comp.lower())
    if not c:
        return 0.0
    i = m = 0
    for w in c:
        while i < len(o) and o[i] != w:
            i += 1
        if i < len(o):
            m += 1
            i += 1
    return round(m / len(c), 3)


class CreditExhausted(Exception):
    pass


async def call_llm(client, cfg, system, user, max_out):
    body = {"model": cfg["model"], "temperature": TEMPERATURE, "max_tokens": max_out,
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}]}
    for attempt in range(6):
        try:
            r = await client.post(cfg["url"], headers={"Authorization": f"Bearer {cfg['key']}"}, json=body)
            if r.status_code == 200:
                ch = r.json()["choices"][0]
                return ch["message"].get("content") or "", ch.get("finish_reason")
            if r.status_code == 402:                     # OpenRouter: out of credits
                raise CreditExhausted()
            if r.status_code == 429:
                err = r.json().get("error") or {}
                msg = f"{err.get('code')} {err.get('message')}".lower()
                if "insufficient_quota" in msg or "credit" in msg or "quota" in msg:
                    raise CreditExhausted()              # OpenAI no-credits (vs rate limit)
                await asyncio.sleep(min(30, 3 * (attempt + 1)))   # rate limit -> backoff
                continue
            if r.status_code >= 500:
                await asyncio.sleep(2 * (attempt + 1))
                continue
            return None, f"HTTP{r.status_code}:{r.text[:80]}"
        except CreditExhausted:
            raise
        except Exception as e:  # noqa: BLE001
            if attempt == 5:
                return None, f"ERR:{type(e).__name__}"
            await asyncio.sleep(2 * (attempt + 1))
    return None, "ERR:retries"


@app.function(image=image,
              secrets=[modal.Secret.from_name("openai-key"), modal.Secret.from_name("openrouter"),
                       modal.Secret.from_name("hf-token")],
              volumes={"/ckpt": ckpt_vol}, cpu=4.0, memory=8192, timeout=8 * 3600)
def generate(split: str = "train", per_domain: int = 0, provider: str = "openai") -> dict:
    import httpx
    import tiktoken
    from datasets import Dataset, load_dataset

    enc = tiktoken.get_encoding("o200k_base")
    ntok = lambda s: len(enc.encode(s, disallowed_special=()))
    if provider == "openrouter":
        cfg = {"url": "https://openrouter.ai/api/v1/chat/completions",
               "model": "openai/gpt-4.1-mini", "key": os.environ["OPENROUTER_API_KEY"]}
    else:
        cfg = {"url": OPENAI_URL, "model": MODEL, "key": os.environ["OPENAI_API_KEY"]}
    hf_token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
    ckpt_path = f"/ckpt/{GEN_REPO.split('/')[-1]}-{split}.jsonl"   # repo-specific: phase-1/2 don't share checkpoints

    # Storage cleaned up -> push PRIVATE.
    from huggingface_hub import HfApi
    _api = HfApi(token=hf_token)
    _api.create_repo(GEN_REPO, repo_type="dataset", private=True, exist_ok=True)

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
    print(f"split={split}: {len(rows)} rows, resumed {len(done)}, {len(todo)} to do in {n_batches} batches (gpt-4.1-mini)")

    def push_hf(all_rows, tag):
        try:
            Dataset.from_list(all_rows).push_to_hub(GEN_REPO, split=split, private=True, token=hf_token)
            print(f"  [HF] pushed {len(all_rows)} rows to {GEN_REPO}:{split} ({tag})")
        except Exception as e:  # noqa: BLE001
            print(f"  [HF] push FAILED ({tag}): {type(e).__name__}: {str(e)[:100]}")

    async def process_row(row, client):
        kp = int(float(row["ratio_bucket"]) * 100)
        n_in = row["n_tokens"]
        max_out = min(4096, int(n_in * 1.2) + 128)
        target = int(n_in * kp / 100)
        user = (f"Compress:\n{row['text']}\n\n[OUTPUT LENGTH: ~{target} tokens ({kp}% of the {n_in}-token "
                f"input). Do NOT go below {int(target * 0.85)} tokens — keep it long.]")
        comp, finish = await call_llm(client, cfg, system_for(kp), user, max_out)
        status = (finish if comp is None else ("empty_content" if not comp.strip() else "ok"))
        rec = {"uid": row["uid"], **{k: row[k] for k in ("domain", "doc_id", "length_class", "ratio_bucket")},
               "original": row["text"], "n_tokens_in": n_in, "compressed": comp or "",
               "finish_reason": finish, "status": status}
        if status == "ok":
            pres, nr = preserve_check(row["text"], comp)
            rec.update({"n_tokens_out": ntok(comp), "achieved_ratio": round(ntok(comp) / max(1, n_in), 3),
                        "gate_preserve": pres, "num_ratio": nr, "subseq_ratio": subseq_ratio(row["text"], comp)})
        else:
            rec.update({"n_tokens_out": 0, "achieved_ratio": 0.0, "gate_preserve": False,
                        "num_ratio": 0.0, "subseq_ratio": 0.0})
        return rec

    async def driver():
        produced, credits_out = [], False
        async with httpx.AsyncClient(timeout=120) as client:
            with open(ckpt_path, "a") as ckpt:
                for bi in range(n_batches):
                    batch = todo[bi * BATCH_SIZE:(bi + 1) * BATCH_SIZE]
                    try:
                        recs = await asyncio.gather(*[process_row(r, client) for r in batch])
                    except CreditExhausted:
                        print("  [CREDITS] OpenAI insufficient_quota -> flushing to HF and stopping")
                        credits_out = True
                        break
                    for rec in recs:
                        ckpt.write(json.dumps(rec) + "\n")
                    ckpt.flush()
                    await ckpt_vol.commit.aio()   # async commit: blocking commit() deadlocks the event loop
                    produced.extend(recs)
                    nok = sum(r["status"] == "ok" for r in recs)
                    print(f"  [batch {bi + 1}/{n_batches}] ok={nok}/{len(recs)} done={len(done)+len(produced)}/{len(rows)}")
                    if (bi + 1) % PUSH_EVERY == 0:
                        await asyncio.to_thread(push_hf, list(done.values()) + produced, f"checkpoint b{bi+1}")
        return produced, credits_out

    produced, credits_out = asyncio.run(driver())
    out = list(done.values()) + produced
    push_hf(out, "final (credits_out)" if credits_out else "final")   # always land on HF

    ok = [r for r in out if r["status"] == "ok"]
    subs = sorted(r["subseq_ratio"] for r in ok)
    ratios = sorted(r["achieved_ratio"] for r in ok) or [0]
    stats = {
        "split": split, "n": len(out), "glm_ok": len(ok), "credits_exhausted": credits_out,
        "empty_content": sum(r["status"] == "empty_content" for r in out),
        "errors": sum(r["status"] not in ("ok", "empty_content") for r in out),
        "fail_preserve": sum(not r["gate_preserve"] for r in ok),
        "achieved_ratio_median": ratios[len(ratios) // 2], "saved_pct_median": round((1 - ratios[len(ratios) // 2]) * 100, 1),
        "subseq_median": subs[len(subs) // 2] if subs else 0,
        "pushed_to": f"{GEN_REPO}:{split}",
    }
    print(json.dumps(stats, indent=2))
    return stats


@app.local_entrypoint()
def smoke(split: str = "validation", per_domain: int = 3, provider: str = "openai"):
    print(json.dumps(generate.remote(split=split, per_domain=per_domain, provider=provider), indent=2))


@app.local_entrypoint()
def run_all(provider: str = "openai"):
    for sp in ("train", "validation", "test"):
        print(json.dumps(generate.remote(split=sp, per_domain=0, provider=provider), indent=2))


@app.local_entrypoint()
def run_split(split: str = "test", provider: str = "openrouter"):   # resume one split from checkpoint
    print(json.dumps(generate.remote(split=split, per_domain=0, provider=provider), indent=2))
