"""Phase 1: author GOLD few-shot compression examples with gpt-5.4.

Compression is driven by a LEVER taxonomy in 2 TIERS, plus an incompressible-floor rule:

  Tier 1 (LIGHT, keep ~80-100%): GROUP A levers only (filler/disfluency/redundancy
    deletion). Output stays natural prose.
  Tier 2 (HEAVY, keep ~60-80%): GROUP A + GROUP B (re-representation: structural
    rewrite, reference dedup, notation). Output may become structured.
  FLOOR: fact-saturated passages cannot be compressed without dropping facts; the
    correct output is the text ~unchanged. We deliberately generate these "do NOT
    compress" examples so the student learns to recognize the floor.

Per domain we pick 3 passages by fact-density and run:
  low-density  -> Tier 2  (compressible -> good heavy example)
  mid-density  -> Tier 1  (light example)
  high-density -> Tier 2  (re-representable ones compress; truly saturated ones
                           stay ~identity -> floor example). The data self-sorts.

  modal run benchmarks/modal/author_fewshots_gpt54.py

Writes benchmarks/modal/compress_fewshots.json + prints a tier/ratio/category table.
"""
from __future__ import annotations

import asyncio
import json
import os

import modal

app = modal.App("semfs-author-fewshots")
image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets==2.21.0", "openai>=1.40", "tiktoken", "huggingface_hub")
)

SOURCE_REPO = "pmarmik/semfs-compress-sources-phase1"   # the original 21K, all 7 domains
DOMAINS = ["legal", "medical", "financial", "meetings", "calls", "web", "chat"]
TOK_LO, TOK_HI = 400, 700   # moderate-length passages -> good, compact few-shots
N_CANDIDATES = 80           # in-range candidates scanned per domain (for percentile picks)

# (tier, label, keep_lo%, keep_hi%, density-percentile-to-pick)
PLAN = [
    (2, "heavy", 60, 80, 0.15),   # low density  -> compressible -> heavy example
    (1, "light", 80, 100, 0.50),  # mid density  -> light example
    (2, "floor", 60, 80, 0.90),   # high density -> usually hits the floor (incompressible)
]

# ---------- the compression prompt: levers + 2 tiers + floor ----------
CORE = (
    "You compress text while preserving EVERY FACT exactly. FACTS = numbers, money, dates, "
    "percentages, durations, quantities, names (people/orgs/products), identifiers, decisions, "
    "action items, code, inline code, URLs, file paths, citations/references, speaker labels. "
    "If removing or changing something would lose or alter a fact, KEEP it. When unsure, keep it. "
    "Output ONLY the compressed text — no preamble, notes, or quotes."
)

LEVERS = """COMPRESSION LEVERS — the techniques you may use, each labeled.

GROUP A — DELETION (remove non-fact tokens; output stays natural prose):
  A1 FILLER:     drop hedges/intensifiers/empty phrases ("very","really","basically",
                 "in order to"->"to","the fact that"->removed).
  A2 DISFLUENCY: drop "uh"/"um", false starts, repeated words, backchannels ("yeah","mm"),
                 and transcription markers like {disfmarker}/{vocalsound}.
  A3 REDUNDANCY: state each fact once; cut restatements and empty connectors.

GROUP B — RE-REPRESENTATION (re-encode the SAME facts more densely; output may be structured):
  B1 STRUCTURE:  turn repetitive prose into a compact list / table / "key: value" form.
  B2 DEDUP-REF:  name a repeated entity once, then use a short reference ("the Loan").
  B3 NOTATION:   use $ for dollars, % for percent, standard abbreviations, or a one-line
                 legend for a repeated phrase."""

FLOOR = (
    "THE FLOOR — some passages are fact-saturated: nearly every token is a distinct fact, with no "
    "filler, no repetition, and no scaffolding to factor out (e.g. a dense line of statistics, a "
    "list of unique identifiers, code). For these, the CORRECT output is the text essentially "
    "UNCHANGED. Do NOT manufacture compression by dropping, merging, or approximating facts. "
    "\"I cannot compress this further without losing a fact\" is a correct outcome, not a failure."
)


def tier_block(tier: int) -> str:
    if tier == 1:
        return ("TIER 1 — LIGHT (target keep ~80-100%). Use GROUP A ONLY (A1-A3). Keep natural, "
                "readable prose; do NOT restructure or re-notate. Fact-dense text will barely shrink "
                "— that is correct.")
    return ("TIER 2 — HEAVY (target keep ~60-80%). Use GROUP A AND GROUP B (A1-A3 + B1-B3). Maximize "
            "density; the output MAY become a list/table/notation. Still never drop a fact.")


def system_for(tier: int) -> str:
    return f"{CORE}\n\n{LEVERS}\n\n{tier_block(tier)}\n\n{FLOOR}"


def user_for(text: str, n_in: int, tier: int, lo: int, hi: int) -> str:
    lo_t, hi_t = int(n_in * lo / 100), int(n_in * hi / 100)
    how = ("Apply TIER 1 (deletion only)." if tier == 1
           else "Apply TIER 2 (deletion + re-representation).")
    return (f"Compress this passage.\n\n{text}\n\n"
            f"[{how} Aim for {lo}-{hi}% of the {n_in}-token original (~{lo_t}-{hi_t} tokens) IF it can "
            f"be reached without losing a fact. If the passage is fact-saturated, keep it nearly "
            f"unchanged — do not force the ratio.]")


def refine_for(prev: str, n_out: int, n_in: int, lo: int, hi: int) -> str:
    lo_t, hi_t = int(n_in * lo / 100), int(n_in * hi / 100)
    pct = round(100 * n_out / n_in)
    return (f"Your output is {n_out} tokens ({pct}% of the {n_in}-token original); the target is "
            f"{lo}-{hi}% (~{lo_t}-{hi_t} tokens). If MORE can be removed or re-encoded WITHOUT losing "
            f"any fact, do it now and output the shorter version. If it is already at the floor (every "
            f"token a distinct fact), output the SAME text unchanged.\n\n"
            f"Previous:\n{prev}\n\nOutput ONLY the compressed text.")


@app.function(image=image, timeout=2400,
              secrets=[modal.Secret.from_name("openai-key"), modal.Secret.from_name("hf-token")])
def author() -> dict:
    import tiktoken
    from datasets import load_dataset
    from openai import AsyncOpenAI

    model = os.environ.get("AUTHOR_MODEL", "gpt-5.4")
    enc = tiktoken.get_encoding("o200k_base")
    ntok = lambda s: len(enc.encode(s, disallowed_special=()))

    # 1) gather in-range candidates per domain
    ds = load_dataset(SOURCE_REPO, split="train")
    cand = {d: [] for d in DOMAINS}
    for r in ds:
        d = r["domain"]
        if d in cand and len(cand[d]) < N_CANDIDATES:
            n = ntok(r["text"])
            if TOK_LO <= n <= TOK_HI:
                cand[d].append((sum(c.isdigit() for c in r["text"]), n, r["doc_id"], r["text"]))
        if all(len(cand[d]) >= N_CANDIDATES for d in DOMAINS):
            break

    # 2) build the work plan: per domain, pick passages at density percentiles
    jobs = []
    for d in DOMAINS:
        cs = sorted(cand[d], key=lambda x: x[0])   # ascending fact-density
        if not cs:
            print(f"[{d}] NO candidates in [{TOK_LO},{TOK_HI}] tok"); continue
        print(f"[{d}] {len(cs)} cands, density {cs[0][0]}..{cs[-1][0]} digit-chars")
        for tier, label, lo, hi, pct in PLAN:
            digits, n, doc_id, text = cs[min(int(len(cs) * pct), len(cs) - 1)]
            jobs.append({"domain": d, "tier": tier, "label": label, "lo": lo, "hi": hi,
                         "doc_id": doc_id, "n_in": n, "digits": digits, "text": text})

    # 3) gpt-5.4 compresses each, with a light non-forcing refinement loop
    client = AsyncOpenAI(api_key=os.environ["OPENAI_API_KEY"])
    sem = asyncio.Semaphore(8)

    async def run_job(j):
        n_in, tier, lo, hi = j["n_in"], j["tier"], j["lo"], j["hi"]
        msgs = [{"role": "system", "content": system_for(tier)},
                {"role": "user", "content": user_for(j["text"], n_in, tier, lo, hi)}]
        comp, n_out, rounds = "", n_in, 0
        async with sem:
            for rnd in range(2):   # generate -> "cut more IF lossless, else keep same" -> regenerate
                r = await client.chat.completions.create(model=model, messages=msgs)
                comp = (r.choices[0].message.content or "").strip()
                n_out, rounds = ntok(comp), rnd + 1
                if n_out <= hi / 100 * n_in:   # reached the tier ceiling -> done
                    break
                msgs += [{"role": "assistant", "content": comp},
                         {"role": "user", "content": refine_for(comp, n_out, n_in, lo, hi)}]
        ratio = n_out / n_in
        cat = "floor" if ratio >= 0.92 else ("heavy" if tier == 2 else "light")
        return {"domain": j["domain"], "tier": tier, "label": j["label"], "lo": lo, "hi": hi,
                "doc_id": j["doc_id"], "digits": j["digits"], "original": j["text"], "compressed": comp,
                "n_in": n_in, "n_out": n_out, "achieved_ratio": round(ratio, 3),
                "rounds": rounds, "category": cat}

    async def gather_all():
        return await asyncio.gather(*[run_job(j) for j in jobs])

    examples = asyncio.run(gather_all())
    examples.sort(key=lambda e: (DOMAINS.index(e["domain"]), e["tier"], e["label"]))

    cats = {c: sum(e["category"] == c for e in examples) for c in ("light", "heavy", "floor")}
    print(f"\ncategories: {cats}")
    return {"model": model, "source_repo": SOURCE_REPO,
            "tiers": {"1": "LIGHT keep 80-100 (group A)", "2": "HEAVY keep 60-80 (group A+B)"},
            "examples": examples}


@app.local_entrypoint()
def main():
    res = author.remote()
    path = os.path.join(os.path.dirname(__file__), "compress_fewshots.json")
    with open(path, "w") as f:
        json.dump(res, f, indent=2, ensure_ascii=False)
    print(f"\nwrote {path}")
    print(f"{'domain':<10} {'tier':<5} {'label':<6} {'achieved':>9} {'category':>9} {'digits':>7}")
    for e in res["examples"]:
        print(f"{e['domain']:<10} T{e['tier']:<4} {e['label']:<6} "
              f"{e['achieved_ratio']*100:>7.1f}% {e['category']:>9} {e['digits']:>7}")
