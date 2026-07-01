"""Prompt B: the fact-preservation VERIFIER (gpt, few-shot).

Given (original, compressed), judges whether the compression is LOSSLESS:
  - preserves EVERY fact from the original (nothing dropped), AND
  - adds NO fact not supported by the original (no fabrication), AND
  - format/wording/structure changes are explicitly OK (prose -> table is fine).

Verification is input-heavy + tiny-output, so it's cheap on the API (the reason we run
the GENERATOR on gemma and the VERIFIER on gpt). gpt-5.4 by default for the 21-example
few-shot certification (best judge, tiny volume); switch VERIFIER_MODEL=gpt-4.1-mini for
the bulk later.

  modal run benchmarks/modal/verify_facts_gpt.py        # certify the few-shot library
"""
from __future__ import annotations

import asyncio
import json
import os

import modal

app = modal.App("semfs-verify-facts-gpt")
image = modal.Image.debian_slim(python_version="3.11").pip_install("openai>=1.40")

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


@app.function(image=image, timeout=2400, secrets=[modal.Secret.from_name("openai-key")])
def verify(examples: list) -> dict:
    from openai import AsyncOpenAI

    model = os.environ.get("VERIFIER_MODEL", "gpt-5.4")
    client = AsyncOpenAI(api_key=os.environ["OPENAI_API_KEY"])
    sem = asyncio.Semaphore(8)

    async def one(e):
        user = (f"ORIGINAL:\n{e['original']}\n\nCOMPRESSED:\n{e['compressed']}\n\n"
                "Return the JSON verdict.")
        async with sem:
            r = await client.chat.completions.create(
                model=model, response_format={"type": "json_object"},
                messages=[{"role": "system", "content": VERIFIER_SYS},
                          {"role": "user", "content": user}])
        try:
            v = json.loads(r.choices[0].message.content)
        except Exception:  # noqa: BLE001
            v = {"verdict": "PARSE_ERROR", "dropped": [], "added": [], "reason": r.choices[0].message.content[:200]}
        return {"domain": e["domain"], "label": e["label"], "tier": e["tier"],
                "achieved_ratio": e["achieved_ratio"], "category": e["category"],
                "verdict": v.get("verdict"), "dropped": v.get("dropped", []),
                "added": v.get("added", []), "reason": v.get("reason", "")}

    async def gather_all():
        return await asyncio.gather(*[one(e) for e in examples])

    results = asyncio.run(gather_all())
    results.sort(key=lambda x: (x["verdict"] != "LOSSLESS", x["domain"]))
    npass = sum(r["verdict"] == "LOSSLESS" for r in results)
    print(f"\n{npass}/{len(results)} LOSSLESS")
    return {"model": model, "n_pass": npass, "n_total": len(results), "results": results}


@app.local_entrypoint()
def main():
    path = os.path.join(os.path.dirname(__file__), "compress_fewshots.json")
    examples = json.load(open(path))["examples"]
    res = verify.remote(examples)
    out = os.path.join(os.path.dirname(__file__), "compress_fewshots_verdicts.json")
    with open(out, "w") as f:
        json.dump(res, f, indent=2, ensure_ascii=False)
    print(f"wrote {out}\n")
    print(f"{'domain':<10} {'label':<6} {'ratio':>6} {'verdict':<10} issues")
    for r in res["results"]:
        issue = ""
        if r["dropped"]:
            issue += f"DROPPED {r['dropped']} "
        if r["added"]:
            issue += f"ADDED {r['added']}"
        print(f"{r['domain']:<10} {r['label']:<6} {r['achieved_ratio']*100:>5.0f}% {r['verdict']:<10} {issue[:70]}")
