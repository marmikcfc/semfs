"""Judge A/B: does Qwen + Prompt B verify as well as gpt-5.4 + Prompt B?

Holds the PROMPT constant (Prompt B, copied from compress_loop.py) and swaps the judge
model: gpt-5.4 (ground truth, from compress_fewshots_verdicts.json) vs qwen3.7-max via
OpenRouter. Runs both on the SAME 21 labeled (original, compressed) pairs and reports:
  - overall agreement
  - LOSSY recall   (of the gpt-LOSSY cases, how many Qwen also catches)  <- the metric that matters
  - LOSSLESS agreement (of the gpt-LOSSLESS cases, how many Qwen passes; over-flagging = waste)

qwen3.7-max is a STRONGER/larger Qwen than the self-hosted Qwen3.6-27B we'd actually run, so
it's an UPPER BOUND for the family: if it can't catch the fabrication, the 27B won't either.

  modal run benchmarks/modal/verify_qwen_openrouter_test.py
"""
from __future__ import annotations

import asyncio
import json
import os
import re

import modal

app = modal.App("semfs-verify-qwen-ab")
image = modal.Image.debian_slim(python_version="3.11").pip_install("openai>=1.40")

# Prompt B — verbatim from compress_loop.py (held constant; that file is canonical).
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
VERDICT: {"verdict":"LOSSLESS","dropped":[],"added":[],"reason":"Reformatted prose to key:value; all three role-name facts identical. Format change is not fact change."}

EXAMPLE 3
ORIGINAL: Revenue was $5M in Q1 and $6M in Q2.
COMPRESSED: Revenue was $5M in Q1.
VERDICT: {"verdict":"LOSSY","dropped":["Q2 revenue $6M"],"added":[],"reason":"The Q2 figure $6M is missing from the compressed text."}

EXAMPLE 4
ORIGINAL: The drug reduced symptoms in patients.
COMPRESSED: The drug reduced symptoms in 80% of patients within 2 weeks.
VERDICT: {"verdict":"LOSSY","dropped":[],"added":["80% of patients","within 2 weeks"],"reason":"Added a percentage and a timeframe not present in the original."}

EXAMPLE 5
ORIGINAL: were n0 . The study enrolled 240 patients across 3 sites.
COMPRESSED: The study enrolled 240 patients across 3 sites.
VERDICT: {"verdict":"LOSSLESS","dropped":[],"added":[],"reason":"Dropped the OCR fragment 'were n0 .' (not a fact); 240 patients and 3 sites preserved."}

EXAMPLE 6
ORIGINAL: PM: But if you say is it one-way or— UI: Consensus on what we do.
COMPRESSED: PM: We need consensus; it is one-way or multi-purpose.
VERDICT: {"verdict":"LOSSY","dropped":[],"added":["attributes 'consensus' to PM, but UI said it","completes PM's unfinished 'one-way or—' into a definite claim"],"reason":"Invented an attribution and turned an unfinished utterance into a definite statement."}"""


VERIFIER_SYS_COT = VERIFIER_SYS + (
    "\n\nPROCEDURE — do this IN ORDER before deciding:\n"
    "STEP 1 (ADDED): scan the COMPRESSED for any fact, attribution, number, name, or COMPLETED statement that "
    "is NOT supported by the ORIGINAL. Invented speaker attributions and unfinished sentences turned into "
    "definite claims are the most common fabrications — look hard. Put every one in \"added\".\n"
    "STEP 2 (DROPPED): scan the ORIGINAL for any fact MISSING from the COMPRESSED. IGNORE the NOT-FACTS "
    "(chunk fragments, OCR noise, backchannels, derivable counts, format/wording changes). Put real misses in \"dropped\".\n"
    "STEP 3 (VERDICT): LOSSY if \"added\" or \"dropped\" is non-empty, else LOSSLESS.\n"
    "Be strict about fabrication and dropped facts; do NOT flag NOT-FACTS or pure reformatting.")


def parse_verdict(text: str) -> dict:
    try:
        return json.loads(text)
    except Exception:  # noqa: BLE001
        m = re.search(r"\{.*\}", text, re.DOTALL)
        if m:
            try:
                return json.loads(m.group(0))
            except Exception:  # noqa: BLE001
                pass
    return {"verdict": "PARSE_ERROR", "dropped": [], "added": [], "reason": text[:160]}


@app.function(image=image, timeout=1800,
              secrets=[modal.Secret.from_name("openrouter"), modal.Secret.from_name("openai-key")])
def run_test(pairs: list, judge: str = "qwen", reason: str = "", prompt: str = "base") -> dict:
    from openai import AsyncOpenAI

    sys = VERIFIER_SYS_COT if prompt == "cot" else VERIFIER_SYS
    via_or = "/" in judge or judge.startswith("qwen")   # OpenRouter (slug has "/", e.g. z-ai/glm-5.2)
    if via_or:
        model = judge if "/" in judge else os.environ.get("QWEN_MODEL", "qwen/qwen3.7-max")
        key = (os.environ.get("OPENROUTER_API_KEY") or os.environ.get("OPENROUTER_KEY")
               or os.environ.get("OPENROUTER"))
        client = AsyncOpenAI(api_key=key, base_url="https://openrouter.ai/api/v1")
    else:                          # OpenAI; judge IS the model id (gpt-4.1-mini, gpt-5.4-mini, gpt-5.4-nano, ...)
        model = {"mini": "gpt-4.1-mini"}.get(judge, judge)
        client = AsyncOpenAI(api_key=os.environ["OPENAI_API_KEY"])
    sem = asyncio.Semaphore(3)   # OpenRouter rate-limits low tiers; keep concurrency modest

    async def one(p):
        user = f"ORIGINAL:\n{p['original']}\n\nCOMPRESSED:\n{p['compressed']}\n\nReturn the JSON verdict."
        async with sem:
            v = {"verdict": "ERROR:none", "dropped": [], "added": [], "reason": ""}
            usage = {}
            for attempt in range(4):
                try:
                    kwargs = {"model": model,
                              "messages": [{"role": "system", "content": sys},
                                           {"role": "user", "content": user}]}
                    if str(model).startswith("gpt-5"):
                        kwargs["max_completion_tokens"] = 16000 if reason else 8000
                        if reason:
                            kwargs["reasoning_effort"] = reason
                    elif via_or:
                        kwargs["max_tokens"] = 16000 if reason else (8000 if "glm" in str(model).lower() else 600)
                        if reason:
                            kwargs["extra_body"] = {"reasoning": {"effort": reason}}   # force thinking ON
                    else:
                        kwargs["max_tokens"] = 600               # tiny verdict; avoids 65536 default credit-estimate
                    r = await client.chat.completions.create(**kwargs)
                    v = parse_verdict((r.choices[0].message.content or "").strip())
                    u = r.usage
                    rt = getattr(getattr(u, "completion_tokens_details", None), "reasoning_tokens", 0) or 0
                    usage = {"in": u.prompt_tokens, "out": u.completion_tokens, "reasoning": rt}
                    break
                except Exception as e:  # noqa: BLE001
                    v = {"verdict": f"ERROR:{type(e).__name__}", "dropped": [], "added": [], "reason": str(e)[:160]}
                    if attempt < 3:
                        await asyncio.sleep(5 * (attempt + 1))
        return {"domain": p["domain"], "label": p["label"], "gpt": p["gpt_verdict"],
                "qwen": v.get("verdict"), "qwen_dropped": v.get("dropped", []),
                "qwen_added": v.get("added", []), "qwen_reason": v.get("reason", ""), "usage": usage}

    async def gather_all():
        return await asyncio.gather(*[one(p) for p in pairs])

    res = asyncio.run(gather_all())
    return {"model": model, "results": res}


@app.function(image=image, secrets=[modal.Secret.from_name("openai-key")])
def list_openai_models() -> list:
    from openai import OpenAI
    c = OpenAI(api_key=os.environ["OPENAI_API_KEY"])
    return sorted(m.id for m in c.models.list() if "gpt-5" in m.id)


@app.local_entrypoint()
def models():
    print("OpenAI gpt-5* models available:\n  " + "\n  ".join(list_openai_models.remote()))


@app.function(image=image, secrets=[modal.Secret.from_name("openrouter")])
def list_or_models(substr: str = "glm") -> list:
    from openai import OpenAI
    key = (os.environ.get("OPENROUTER_API_KEY") or os.environ.get("OPENROUTER_KEY") or os.environ.get("OPENROUTER"))
    c = OpenAI(api_key=key, base_url="https://openrouter.ai/api/v1")
    return sorted(m.id for m in c.models.list() if substr.lower() in m.id.lower())


@app.local_entrypoint()
def or_models(substr: str = "glm"):
    print(f"OpenRouter models matching '{substr}':\n  " + "\n  ".join(list_or_models.remote(substr)))


@app.local_entrypoint()
def main(judge: str = "qwen", reason: str = "", prompt: str = "base"):   # --reason high ; --prompt cot
    here = os.path.dirname(__file__)
    fewshots = json.load(open(os.path.join(here, "compress_fewshots.json")))["examples"]
    verdicts = json.load(open(os.path.join(here, "compress_fewshots_verdicts.json")))["results"]
    gpt_by_key = {(v["domain"], v["label"]): v["verdict"] for v in verdicts}
    pairs = [{"domain": e["domain"], "label": e["label"], "original": e["original"],
              "compressed": e["compressed"], "gpt_verdict": gpt_by_key.get((e["domain"], e["label"]), "?")}
             for e in fewshots]

    res = run_test.remote(pairs, judge=judge, reason=reason, prompt=prompt)
    rows = res["results"]
    rows.sort(key=lambda r: (r["gpt"] != "LOSSY", r["domain"]))

    agree = sum(r["gpt"] == r["qwen"] for r in rows)
    gpt_lossy = [r for r in rows if r["gpt"] == "LOSSY"]
    gpt_clean = [r for r in rows if r["gpt"] == "LOSSLESS"]
    recall = sum(r["qwen"] == "LOSSY" for r in gpt_lossy)
    clean_agree = sum(r["qwen"] == "LOSSLESS" for r in gpt_clean)

    print(f"\nmodel={res['model']}")
    print(f"overall agreement     {agree}/{len(rows)}")
    print(f"LOSSY recall (CRITICAL) {recall}/{len(gpt_lossy)}  (gpt-LOSSY that Qwen also caught)")
    print(f"LOSSLESS agreement     {clean_agree}/{len(gpt_clean)}  (gpt-clean that Qwen also passed)\n")
    print(f"{'domain':<10} {'label':<6} {'gpt':<9} {'qwen':<12} {'match':<6}")
    for r in rows:
        print(f"{r['domain']:<10} {r['label']:<6} {r['gpt']:<9} {str(r['qwen']):<12} "
              f"{'OK' if r['gpt']==r['qwen'] else 'DIFF'}")
    us = [r.get("usage") for r in rows if r.get("usage")]
    if us:
        n = len(us); ti = sum(u["in"] for u in us); to = sum(u["out"] for u in us); tr = sum(u.get("reasoning", 0) for u in us)
        print(f"\nTOKENS over {n} calls: in={ti} out={to} (reasoning={tr}) | per-call avg: "
              f"in={ti//n} out={to//n} reasoning={tr//n}")
    tag = judge.replace('/', '_') + (f"_think-{reason}" if reason else "") + (f"_{prompt}" if prompt != "base" else "")
    json.dump(res, open(os.path.join(here, f"verify_{tag}_ab.json"), "w"), indent=2, ensure_ascii=False)
