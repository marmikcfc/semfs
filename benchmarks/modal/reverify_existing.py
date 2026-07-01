"""Re-verify EXISTING (original -> compressed) pairs with nano@high (Prompt B) — no generation.

Salvages free SFT rows (PASS) + DPO negatives (FAIL) from datasets we already have:
  - pmarmik/semfs-compress-generated-openai   (~24.5K old gpt-4.1-mini pairs; aggressive -> mostly DPO)
  - Sudhendra/semantic-compression-sft         (~25K, CC-BY, dense-notation ~= our Tier-2; SFT + code)

  modal run benchmarks/modal/reverify_existing.py::sample   # measure yield on a sample first
"""
from __future__ import annotations

import ast
import json
import os
import re

import modal

app = modal.App("semfs-reverify")
image = modal.Image.debian_slim(python_version="3.11").pip_install(
    "datasets==2.21.0", "openai>=1.40", "huggingface_hub", "tiktoken")

OLD_REPO = "pmarmik/semfs-compress-generated-openai"
SUDHENDRA = "Sudhendra/semantic-compression-sft"
CODE_RE = re.compile(r"```|def \w+\(|function \w+\(|#include|console\.log\(|public static|class \w+\s*[:{(]", re.I)

# Prompt B — verbatim from compress_loop.py (the locked verifier).
VERIFIER_SYS = """You are a strict FACT-PRESERVATION verifier for a text compressor. Given an \
ORIGINAL passage and its COMPRESSED version, decide whether the compression is LOSSLESS.

FACTS = numbers, money, dates, percentages, durations, quantities, names (people/orgs/products), \
identifiers, decisions, action items, code, inline code, URLs, file paths, citations/references, \
speaker attributions.

RULES:
- Format, wording, and structure changes are FINE. Prose rewritten as a table, list, or key:value \
is LOSSLESS as long as the facts are identical. Judge FACTS, not style.
- Removing filler, hedges, opinions, disfluencies ("uh"), and repetition is LOSSLESS.
- A fact in the ORIGINAL but MISSING from the COMPRESSED => LOSSY (record it in "dropped").
- A fact in the COMPRESSED not supported by the ORIGINAL => LOSSY (record it in "added").
- Paraphrase that CHANGES a fact's value, scope, or specificity => LOSSY (dropped original + added wrong).

NOT FACTS — ignore these; removing or fixing them is LOSSLESS:
- Incomplete sentence fragments at the very START or END of the ORIGINAL (chunk-boundary artifacts).
- OCR/corruption noise and stray tokens (e.g. "n0", "_V_I_"), and transcription markers ({disfmarker}, {vocalsound}).
- Pure backchannels / contentless fillers ("yeah","mm","okay","I think","uh") carrying no decision, answer, or information.
- A count or total DERIVABLE from an explicit enumeration is NOT an added fact (three named systems -> "3 systems" is faithful).
STILL STRICT on numeric values, money, dates, names, quotes and who said them, decisions, action items, identifiers, and
references. NEVER allow inventing an attribution, completing an unfinished statement into a definite claim, or changing who said what.

Output ONLY JSON: {"verdict":"LOSSLESS"|"LOSSY","dropped":[...],"added":[...],"reason":"..."}

EXAMPLE 1
ORIGINAL: Our team, which is honestly amazing, shipped v2.3 to 1,200 users on Jan 5.
COMPRESSED: Team shipped v2.3 to 1,200 users on Jan 5.
VERDICT: {"verdict":"LOSSLESS","dropped":[],"added":[],"reason":"Removed opinion 'which is honestly amazing' (not a fact); v2.3, 1,200 users, Jan 5 all preserved."}

EXAMPLE 2
ORIGINAL: Alice is the CEO. Bob is the CTO. Carol is the CFO.
COMPRESSED: CEO: Alice; CTO: Bob; CFO: Carol.
VERDICT: {"verdict":"LOSSLESS","dropped":[],"added":[],"reason":"Reformatted prose to key:value; all three role-name facts identical."}

EXAMPLE 3
ORIGINAL: Revenue was $5M in Q1 and $6M in Q2.
COMPRESSED: Revenue was $5M in Q1.
VERDICT: {"verdict":"LOSSY","dropped":["Q2 revenue $6M"],"added":[],"reason":"The Q2 figure $6M is missing."}

EXAMPLE 4
ORIGINAL: The drug reduced symptoms in patients.
COMPRESSED: The drug reduced symptoms in 80% of patients within 2 weeks.
VERDICT: {"verdict":"LOSSY","dropped":[],"added":["80% of patients","within 2 weeks"],"reason":"Added a percentage and a timeframe not present in the original."}

EXAMPLE 5
ORIGINAL: were n0 . The study enrolled 240 patients across 3 sites.
COMPRESSED: The study enrolled 240 patients across 3 sites.
VERDICT: {"verdict":"LOSSLESS","dropped":[],"added":[],"reason":"Dropped the OCR fragment 'were n0 .' (not a fact); facts preserved."}

EXAMPLE 6
ORIGINAL: PM: But if you say is it one-way or— UI: Consensus on what we do.
COMPRESSED: PM: We need consensus; it is one-way or multi-purpose.
VERDICT: {"verdict":"LOSSY","dropped":[],"added":["attributes 'consensus' to PM, but UI said it","completes PM's unfinished 'one-way or—' into a definite claim"],"reason":"Invented an attribution and completed an unfinished utterance."}"""


def _parse(text):
    try:
        return json.loads(text)
    except Exception:  # noqa: BLE001
        m = re.search(r"\{.*\}", text, re.DOTALL)
        if m:
            try:
                return json.loads(m.group(0))
            except Exception:  # noqa: BLE001
                pass
    return {"verdict": "PARSE_ERROR"}


@app.function(image=image, timeout=3600,
              secrets=[modal.Secret.from_name("openai-key"), modal.Secret.from_name("hf-token")])
def sample(n_old: int = 600, n_sud: int = 600) -> dict:
    import asyncio
    import random
    from datasets import disable_progress_bars, load_dataset
    from openai import AsyncOpenAI
    disable_progress_bars()

    hf = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
          or os.environ.get("HUGGINGFACE_TOKEN"))
    client = AsyncOpenAI(api_key=os.environ["OPENAI_API_KEY"])
    random.seed(0)

    items = []
    old = load_dataset(OLD_REPO, split="train", token=hf)
    oi = list(range(len(old))); random.shuffle(oi)
    for i in oi[:n_old]:
        r = old[i]
        items.append({"src": "old", "domain": r["domain"], "original": r["original"],
                      "compressed": r["compressed"], "achieved": float(r.get("achieved_ratio") or 0)})
    sud = load_dataset(SUDHENDRA, split="train", token=hf)
    si = list(range(len(sud))); random.shuffle(si)
    for i in si[:n_sud]:
        msgs = sud[i]["messages"]
        if isinstance(msgs, str):
            msgs = ast.literal_eval(msgs)
        u = next((m["content"] for m in msgs if m.get("role") == "user"), "")
        a = next((m["content"] for m in msgs if m.get("role") == "assistant"), "")
        items.append({"src": "sud", "original": u, "compressed": a, "is_code": bool(CODE_RE.search(u))})

    sem = asyncio.Semaphore(24)

    async def one(it):
        async with sem:
            for attempt in range(4):
                try:
                    r = await client.chat.completions.create(
                        model="gpt-5.4-nano", max_completion_tokens=16000, reasoning_effort="high",
                        messages=[{"role": "system", "content": VERIFIER_SYS},
                                  {"role": "user", "content": f"ORIGINAL:\n{it['original'][:6000]}\n\nCOMPRESSED:\n{it['compressed'][:6000]}\n\nReturn the JSON verdict."}])
                    it["verdict"] = _parse((r.choices[0].message.content or "").strip()).get("verdict")
                    return it
                except Exception:  # noqa: BLE001
                    if attempt == 3:
                        it["verdict"] = "ERROR"
                    else:
                        await asyncio.sleep(3 * (attempt + 1))
        return it

    async def go():
        return await asyncio.gather(*[one(it) for it in items])

    res = asyncio.run(go())

    def rate(sub):
        n = len(sub); p = sum(x["verdict"] == "LOSSLESS" for x in sub)
        return {"n": n, "pass": p, "pass_pct": round(100 * p / n, 1) if n else 0}
    old_r = [x for x in res if x["src"] == "old"]
    sud_r = [x for x in res if x["src"] == "sud"]
    # old pass-rate by achieved-ratio band
    bands = {}
    for x in old_r:
        b = "<0.3" if x["achieved"] < 0.3 else ("0.3-0.6" if x["achieved"] < 0.6 else ">=0.6")
        bands.setdefault(b, []).append(x)
    code_sud = [x for x in sud_r if x.get("is_code")]
    return {
        "old": rate(old_r),
        "old_by_band": {b: rate(v) for b, v in bands.items()},
        "sudhendra": rate(sud_r),
        "sudhendra_code": {**rate(code_sud), "frac_code": round(len(code_sud) / max(1, len(sud_r)), 2)},
        "sud_content_samples": [{"is_code": x.get("is_code"), "orig": x["original"][:300],
                                 "comp": x["compressed"][:200], "verdict": x["verdict"]}
                                for x in sud_r[:4]],
    }


@app.function(image=image, timeout=3600,
              secrets=[modal.Secret.from_name("openai-key"), modal.Secret.from_name("hf-token")])
def diagnose(n: int = 150) -> dict:
    """Re-verify WITHOUT truncation, capture verdict distribution + reasons, and split pass-rate
    by whether the pair WOULD have been truncated at 6000 chars (to isolate the truncation bug)."""
    import asyncio
    import random
    from collections import Counter
    from datasets import disable_progress_bars, load_dataset
    from openai import AsyncOpenAI
    disable_progress_bars()

    hf = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
          or os.environ.get("HUGGINGFACE_TOKEN"))
    client = AsyncOpenAI(api_key=os.environ["OPENAI_API_KEY"])
    random.seed(1)

    items = []
    old = load_dataset(OLD_REPO, split="train", token=hf)
    oi = list(range(len(old))); random.shuffle(oi)
    for i in oi[:n]:
        r = old[i]
        items.append({"src": "old", "original": r["original"], "compressed": r["compressed"],
                      "achieved": float(r.get("achieved_ratio") or 0)})
    sud = load_dataset(SUDHENDRA, split="train", token=hf)
    si = list(range(len(sud))); random.shuffle(si)
    for i in si[:n]:
        msgs = sud[i]["messages"]
        if isinstance(msgs, str):
            msgs = ast.literal_eval(msgs)
        u = next((m["content"] for m in msgs if m.get("role") == "user"), "")
        a = next((m["content"] for m in msgs if m.get("role") == "assistant"), "")
        items.append({"src": "sud", "original": u, "compressed": a, "is_code": bool(CODE_RE.search(u))})

    sem = asyncio.Semaphore(24)

    async def one(it):
        it["orig_chars"], it["comp_chars"] = len(it["original"]), len(it["compressed"])
        it["would_truncate"] = it["orig_chars"] > 6000 or it["comp_chars"] > 6000
        async with sem:
            for attempt in range(4):
                try:
                    r = await client.chat.completions.create(   # NO truncation this time
                        model="gpt-5.4-nano", max_completion_tokens=20000, reasoning_effort="high",
                        messages=[{"role": "system", "content": VERIFIER_SYS},
                                  {"role": "user", "content": f"ORIGINAL:\n{it['original']}\n\nCOMPRESSED:\n{it['compressed']}\n\nReturn the JSON verdict."}])
                    v = _parse((r.choices[0].message.content or "").strip())
                    it["verdict"] = v.get("verdict")
                    it["reason"] = (v.get("reason") or "")[:160]
                    it["dropped"] = v.get("dropped", [])[:3]
                    return it
                except Exception as e:  # noqa: BLE001
                    if attempt == 3:
                        it["verdict"], it["reason"] = "ERROR", str(e)[:120]
                    else:
                        await asyncio.sleep(3 * (attempt + 1))
        return it

    async def go():
        return await asyncio.gather(*[one(it) for it in items])

    res = asyncio.run(go())

    def pct(sub):
        sub = list(sub)
        n_ = len(sub)
        return {"n": n_, "pass_pct": round(100 * sum(x["verdict"] == "LOSSLESS" for x in sub) / n_, 1) if n_ else 0}

    out = {}
    for src in ("old", "sud"):
        s = [x for x in res if x["src"] == src]
        out[src] = {
            "verdicts": dict(Counter(x["verdict"] for x in s)),
            "pass_overall": pct(s),
            "pass_short(<=6000)": pct(x for x in s if not x["would_truncate"]),
            "pass_long(>6000, was truncated before)": pct(x for x in s if x["would_truncate"]),
            "n_long": sum(x["would_truncate"] for x in s),
            "fails_annotated": [{"chars": x["orig_chars"], "long": x["would_truncate"], "verdict": x["verdict"],
                                 "dropped": x.get("dropped"), "reason": x.get("reason")}
                                for x in s if x["verdict"] != "LOSSLESS"][:4],
        }
    return out


@app.local_entrypoint()
def main(n_old: int = 600, n_sud: int = 600):
    res = sample.remote(n_old, n_sud)
    p = os.path.join(os.path.dirname(__file__), "_reverify_sample.json")
    with open(p, "w") as f:
        json.dump(res, f, indent=2, ensure_ascii=False)
    print("WROTE", p)


@app.local_entrypoint()
def diag(n: int = 150):
    res = diagnose.remote(n)
    p = os.path.join(os.path.dirname(__file__), "_reverify_diag.json")
    with open(p, "w") as f:
        json.dump(res, f, indent=2, ensure_ascii=False)
    print("WROTE", p)
