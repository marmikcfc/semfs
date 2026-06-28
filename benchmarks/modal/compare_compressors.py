"""Compare GLM-5.1 vs gpt-4.1-mini (OpenRouter) on the SAME source rows, with
MIXED-bucket prompts (strict-extractive on light buckets, allow light rephrasing
on aggressive buckets). GLM rows are read from the checkpoint Volume (already
generated); gpt-4.1-mini is called fresh on the same originals.

Reports per-model: achieved compression ratio, subsequence ratio (extractiveness),
preserve-exact pass rate, and gpt-4.1-mini token cost. Plus side-by-side samples.

  modal run benchmarks/modal/compare_compressors.py::compare
"""

from __future__ import annotations

import asyncio
import json
import os
import re

import modal

app = modal.App("semfs-compress-compare")
image = modal.Image.debian_slim(python_version="3.11").pip_install("httpx", "tiktoken")
ckpt_vol = modal.Volume.from_name("semfs-compress-ckpt", create_if_missing=True)

OR_BASE = "https://openrouter.ai/api/v1/chat/completions"
OR_MODEL = "openai/gpt-5.4-nano"
PRICE_IN, PRICE_OUT = 0.20 / 1e6, 1.25 / 1e6   # gpt-5.4-nano; reasoning tokens billed at output rate
REASONING_EFFORT = "medium"

_NUM = re.compile(r"\d[\d,]*\.?\d*%?")
_URL = re.compile(r"https?://\S+")
_CODE = re.compile(r"`([^`]+)`")
_WORD = re.compile(r"\w+")

EX_MILD = """Original: The board of directors met on Tuesday to discuss the proposed acquisition of Meridian Systems, which had been under review since the beginning of the quarter. CFO Laura Chen presented a detailed financial analysis showing that the deal, valued at approximately $340 million, would be accretive to earnings within 18 months.
Compressed: The board met on Tuesday to discuss the acquisition of Meridian Systems. CFO Laura Chen presented a financial analysis showing the deal, valued at $340 million, would be accretive within 18 months."""

EX_HARD = """Original: In order to install the package, you should simply run the command `npm install left-pad`, and then you will need to make sure to restart the development server before you continue.
Compressed: To install the package, run `npm install left-pad`, then restart the dev server."""


def system_for(keep_pct: int) -> str:
    base = ("You compress text while preserving EVERY fact exactly: numbers, money amounts, dates, "
            "percentages, durations, names, decisions, action items, code, inline code, URLs, file "
            "paths, speaker labels. Output ONLY the compressed text, no preamble or quotes.")
    if keep_pct >= 65:
        mode = ("MODE: STRICT EXTRACTIVE. Delete redundant words/phrases ONLY (articles, qualifiers, "
                "hedges, filler, back-references). Keep every remaining word VERBATIM and in order; do "
                "not reword. Output must read as grammatical English.")
        ex = EX_MILD
    elif keep_pct >= 45:
        mode = ("MODE: AGGRESSIVE EXTRACTIVE. Delete as much redundancy as possible (filler, qualifiers, "
                "redundant clauses, procedural fluff, repeated info) while keeping remaining words VERBATIM "
                "and grammatical.")
        ex = EX_MILD
    else:
        mode = ("MODE: COMPRESS HARD. Prefer deletion, but you MAY lightly rephrase to shorten ('in order "
                "to'->'to', 'make sure to'->'ensure', merge/drop subordinate clauses, telegraphic phrasing) "
                "as long as EVERY fact is preserved exactly. Maximize token reduction.")
        ex = EX_HARD
    return f"{base}\n\n{mode}\n\nTarget: keep about {keep_pct}% of the original length.\n\nEXAMPLE:\n{ex}"


def preserve_ok(orig, comp):
    nums = set(_NUM.findall(orig))
    nr = (sum(n in comp for n in nums) / len(nums)) if nums else 1.0
    return nr >= 0.9 and all(u in comp for u in set(_URL.findall(orig))) and all(c in comp for c in set(_CODE.findall(orig)))


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
    return m / len(c)


@app.function(image=image, secrets=[modal.Secret.from_name("openrouter")],
              volumes={"/ckpt": ckpt_vol}, timeout=1800)
def compare(n_per_domain: int = 4) -> dict:
    import httpx
    import tiktoken

    enc = tiktoken.get_encoding("o200k_base")
    ntok = lambda s: len(enc.encode(s, disallowed_special=()))
    key = os.environ["OPENROUTER_API_KEY"]

    glm_rows = []
    with open("/ckpt/train.jsonl") as fh:
        for line in fh:
            try:
                glm_rows.append(json.loads(line))
            except Exception:  # noqa: BLE001
                pass
    glm_rows = [r for r in glm_rows if r.get("status") == "ok"]
    by_dom, sample = {}, []
    for r in glm_rows:
        by_dom.setdefault(r["domain"], []).append(r)
    for items in by_dom.values():
        sample.extend(items[:n_per_domain])
    print(f"comparing {len(sample)} rows (gpt-4.1-mini mixed-bucket vs GLM checkpoint)")

    sem = asyncio.Semaphore(8)
    usage = {"in": 0, "out": 0, "reason": 0}

    async def gpt(row, client):
        keep_pct = int(float(row["ratio_bucket"]) * 100)
        body = {"model": OR_MODEL, "reasoning": {"effort": REASONING_EFFORT},
                "messages": [{"role": "system", "content": system_for(keep_pct)},
                             {"role": "user", "content": "Compress:\n" + row["original"]}]}
        async with sem:
            for attempt in range(3):
                try:
                    r = await client.post(OR_BASE, headers={"Authorization": f"Bearer {key}"}, json=body)
                    r.raise_for_status()
                    d = r.json()
                    u = d.get("usage", {})
                    usage["in"] += u.get("prompt_tokens", 0)
                    usage["out"] += u.get("completion_tokens", 0)
                    usage["reason"] += (u.get("completion_tokens_details") or {}).get("reasoning_tokens", 0)
                    msg = d["choices"][0]["message"]
                    reasoning = msg.get("reasoning") or ""
                    if not reasoning and msg.get("reasoning_details"):
                        reasoning = " ".join(str(x.get("text") or x.get("summary") or "")
                                             for x in msg["reasoning_details"])
                    return (msg.get("content") or ""), reasoning
                except Exception as e:  # noqa: BLE001
                    if attempt == 2:
                        return "", f"ERR:{type(e).__name__}"
                    await asyncio.sleep(2 * (attempt + 1))
            return "", ""

    async def run():
        async with httpx.AsyncClient(timeout=180) as client:
            return await asyncio.gather(*[gpt(r, client) for r in sample])

    results = asyncio.run(run())
    gpt_comps = [c for c, _ in results]
    gpt_reason = [rsn for _, rsn in results]

    def agg(rows, comp_of):
        by_bucket = {}
        for r, comp in zip(rows, comp_of):
            b = r["ratio_bucket"]
            nin = r["n_tokens_in"]
            rec = by_bucket.setdefault(b, {"ar": [], "ss": [], "pres": 0, "n": 0})
            rec["n"] += 1
            if comp.strip():
                rec["ar"].append(ntok(comp) / max(1, nin))
                rec["ss"].append(subseq_ratio(r["original"], comp))
                rec["pres"] += int(preserve_ok(r["original"], comp))
        return {b: {"achieved_ratio": round(sum(v["ar"]) / len(v["ar"]), 3) if v["ar"] else 0,
                    "subseq": round(sum(v["ss"]) / len(v["ss"]), 3) if v["ss"] else 0,
                    "preserve_pass": round(v["pres"] / v["n"], 2), "n": v["n"]} for b, v in by_bucket.items()}

    glm_agg = agg(sample, [r["compressed"] for r in sample])
    gpt_agg = agg(sample, gpt_comps)

    cost = usage["in"] * PRICE_IN + usage["out"] * PRICE_OUT
    full_run_cost = cost / max(1, len(sample)) * 24500
    rtr = sorted(len(x) for x in gpt_reason if x and not x.startswith("ERR"))
    samples = [{"domain": r["domain"], "bucket": r["ratio_bucket"], "orig": r["original"][:200],
                "GLM": r["compressed"][:200], "gpt54nano": gc[:200], "reasoning": (rsn or "")[:260]}
               for r, gc, rsn in list(zip(sample, gpt_comps, gpt_reason))[:4]]

    out = {
        "model": OR_MODEL, "reasoning_effort": REASONING_EFFORT, "n": len(sample),
        "GLM_by_bucket": glm_agg, "gpt54nano_by_bucket": gpt_agg,
        "gpt_tokens": usage,
        "reasoning_traces_nonempty": sum(1 for x in gpt_reason if x and not x.startswith("ERR")),
        "reasoning_chars_median": rtr[len(rtr) // 2] if rtr else 0,
        "gpt_cost_sample_usd": round(cost, 4),
        "gpt_full_run_cost_est_usd": round(full_run_cost, 2),
        "samples": samples,
    }
    print(json.dumps(out, indent=2))
    return out


@app.local_entrypoint()
def main():
    print(json.dumps(compare.remote(), indent=2))
