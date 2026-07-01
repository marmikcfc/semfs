"""Measure-only Modal job: project the GLM token cost of generating the
Phase-1 compression dataset. NO GLM calls, NO generation run.

It streams a small sample from each finalized source dataset, counts tokens
with a real BPE tokenizer (o200k_base as a GLM proxy, +/- ~10%), measures the
Caveman system-prompt overhead, then projects input/output tokens and $ cost
for the full ~12K generated examples.

Run:
  modal run benchmarks/modal/measure_compress_dataset_cost.py::measure
"""

from __future__ import annotations

import json
import statistics
from itertools import islice

import modal

app = modal.App("semfs-compress-cost")

image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets==2.21.0", "tiktoken", "huggingface_hub")
)

# ----- Phase-1 design parameters (from the finalized spec) -------------------
PRICE_IN = 0.95 / 1_000_000   # $/token
PRICE_OUT = 3.00 / 1_000_000  # $/token

N_GEN_NET = 12_000            # net generated examples we want to keep
DROP_RATE = 0.25             # fraction dropped by gates -> over-generate
PCT_CHUNKED = 0.85
PCT_WHOLEDOC = 0.15
CHUNK_TOK = 1_500            # we chunk long docs to ~this size
WHOLEDOC_CAP = 4_000         # whole-doc examples selected/capped to <=4K
# ratio buckets 0.7 / 0.5 / 0.35 in equal thirds:
RATIO_MEAN = (0.70 + 0.50 + 0.35) / 3

SAMPLE_N = 250               # docs streamed per source for length measurement

# The actual per-call system prompt (Caveman rules + a domain-preserve line).
# This is what gets re-sent on EVERY generation call -> measured for overhead.
CAVEMAN_SYSTEM = """You compress natural-language text into caveman-speak to reduce input tokens while preserving every fact. Output ONLY the compressed text.

REMOVE: articles (a/an/the); filler (just/really/basically/actually/simply/essentially/generally); pleasantries (sure/certainly/of course/happy to/I'd recommend); hedging (it might be worth/you could consider/it would be good to); redundant phrasing (in order to->to, make sure to->ensure, the reason is because->because); connective fluff (however/furthermore/additionally/in addition).

PRESERVE EXACTLY (never modify): code blocks (fenced ``` and indented); inline code (backtick content); URLs and links; file paths; commands (npm install, git commit, docker build); technical terms (library/API/protocol/algorithm names); proper nouns (projects/people/companies); dates, version numbers, numeric values; environment variables ($HOME, NODE_ENV).

PRESERVE STRUCTURE: all markdown headings (keep heading text, compress body); bullet hierarchy and nesting; numbered lists; tables (compress cell text, keep structure); frontmatter/YAML headers.

COMPRESS: short synonyms (big not extensive, fix not implement a solution for, use not utilize); fragments OK (Run tests before commit); drop you should / make sure to / remember to and just state the action; merge redundant bullets; keep one example where multiple show the same pattern.

CRITICAL: anything inside code fences/backticks must be copied EXACTLY -- do not remove comments, spacing, reorder lines, shorten commands, or simplify. If unsure whether something is code or prose, leave it unchanged.

DOMAIN PRESERVE (in addition to the above): keep all citations, section numbers, party names, drug names and dosages, lab values, ICD/CPT codes, monetary figures, fiscal periods, line items, speaker labels, decisions, and action items verbatim.

TARGET COMPRESSION RATIO: {ratio}. Output the compressed text only."""

# ----- Sources: (domain, hf_id, config, split, field-extractor-key) ----------
# Some sources are streamed directly; harder ones use a length-proxy (noted).
SOURCES = [
    # domain,            hf_id,                          config,         split,   kind
    ("legal_bills",      "FiscalNote/billsum",           None,           "train", "text"),
    ("medical",          "ccdv/pubmed-summarization",    "document",     "train", "article"),  # proxy for PMC commercial subset
    ("financial",        "eloukas/edgar-corpus",         "year_2020",    "train", "edgar"),
    ("meetings",         "pszemraj/qmsum-cleaned",       None,           "train", "qmsum"),  # ungated meeting-transcript proxy (AMI is gated)
    ("web",              "HuggingFaceFW/fineweb",        "sample-10BT",  "train", "text"),
    ("chat",             "stingning/ultrachat",          None,           "train", "ultrachat"),
    ("sudhendra_user",   "Sudhendra/semantic-compression-sft", None,     "train", "sft_user"),
]


def _extract(kind, ex):
    if kind in ("text", "article", "dialogue"):
        return ex.get(kind) or ""
    if kind == "edgar":
        return "\n".join(
            str(ex[k]) for k in ex
            if k.startswith("section_") and ex.get(k)
        )
    if kind == "ultrachat":
        data = ex.get("data") or []
        return "\n".join(data) if isinstance(data, list) else str(data)
    if kind == "qmsum":
        for k in ("transcript", "input", "text", "meeting_transcript", "src"):
            if ex.get(k):
                return str(ex[k])
        return " ".join(str(v) for v in ex.values() if isinstance(v, str))
    if kind == "sft_user":
        msgs = ex.get("messages") or []
        for m in msgs:
            if m.get("role") == "user":
                return m.get("content") or ""
        return ""
    return ""


@app.function(image=image, timeout=1800)
def measure() -> dict:
    import tiktoken
    from datasets import load_dataset

    enc = tiktoken.get_encoding("o200k_base")  # GLM-proxy tokenizer (+/- ~10%)
    tok = lambda s: len(enc.encode(s, disallowed_special=()))

    sys_tokens = tok(CAVEMAN_SYSTEM.replace("{ratio}", "0.50"))

    per_domain = {}
    for domain, hf_id, config, split, kind in SOURCES:
        rec = {"hf_id": hf_id, "status": "ok", "n": 0, "lengths": []}
        try:
            args = (hf_id, config) if config else (hf_id,)
            try:
                ds = load_dataset(*args, split=split, streaming=True,
                                  trust_remote_code=True)
            except TypeError:
                ds = load_dataset(*args, split=split, streaming=True)
            for ex in islice(ds, SAMPLE_N):
                txt = _extract(kind, ex)
                if txt and txt.strip():
                    rec["lengths"].append(tok(txt))
            rec["n"] = len(rec["lengths"])
        except Exception as e:  # noqa: BLE001
            rec["status"] = f"FAILED: {type(e).__name__}: {str(e)[:160]}"
        if rec["lengths"]:
            L = sorted(rec["lengths"])
            rec["mean"] = round(statistics.mean(L))
            rec["median"] = L[len(L) // 2]
            rec["p90"] = L[int(len(L) * 0.9)]
            rec["max"] = L[-1]
            rec["pct_ge_1500"] = round(sum(x >= 1500 for x in L) / len(L), 3)
            rec["pct_ge_4000"] = round(sum(x >= 4000 for x in L) / len(L), 3)
        rec.pop("lengths")
        per_domain[domain] = rec

    # ----- cost projection -------------------------------------------------
    avg_content_in = PCT_CHUNKED * CHUNK_TOK + PCT_WHOLEDOC * min(WHOLEDOC_CAP, 3000)
    in_per_call = sys_tokens + avg_content_in
    out_per_call = avg_content_in * RATIO_MEAN

    n_gross = round(N_GEN_NET / (1 - DROP_RATE))  # over-generate for drops

    def cost(n_in_tok, n_out_tok):
        return n_in_tok * PRICE_IN + n_out_tok * PRICE_OUT

    # Scenario A: compression only (embedding/NLI gate -> 0 extra GLM tokens)
    A_in = n_gross * in_per_call
    A_out = n_gross * out_per_call
    A = {
        "calls": n_gross,
        "input_tokens": round(A_in),
        "output_tokens": round(A_out),
        "input_cost_usd": round(A_in * PRICE_IN, 2),
        "output_cost_usd": round(A_out * PRICE_OUT, 2),
        "total_cost_usd": round(cost(A_in, A_out), 2),
    }

    # Scenario B: + LLM QA-retention gate (qgen + answer-on-orig + answer-on-compressed)
    gate_in = (avg_content_in + 120) + (avg_content_in + 200) + (avg_content_in * RATIO_MEAN + 200)
    gate_out = 80 + 60 + 60
    B_in = A_in + n_gross * gate_in
    B_out = A_out + n_gross * gate_out
    B = {
        "calls": n_gross * 4,  # compress + 3 gate calls
        "input_tokens": round(B_in),
        "output_tokens": round(B_out),
        "total_cost_usd": round(cost(B_in, B_out), 2),
    }

    result = {
        "tokenizer": "o200k_base (GLM proxy, +/-~10%)",
        "system_prompt_tokens": sys_tokens,
        "assumptions": {
            "n_gen_net": N_GEN_NET, "drop_rate": DROP_RATE, "n_gross": n_gross,
            "pct_chunked": PCT_CHUNKED, "chunk_tok": CHUNK_TOK,
            "pct_wholedoc": PCT_WHOLEDOC, "wholedoc_cap": WHOLEDOC_CAP,
            "ratio_mean": round(RATIO_MEAN, 4),
            "avg_content_in_tok": round(avg_content_in),
            "in_per_call_tok": round(in_per_call),
            "out_per_call_tok": round(out_per_call),
            "price_in_per_Mtok": 0.95, "price_out_per_Mtok": 3.00,
        },
        "per_domain_source_lengths": per_domain,
        "scenario_A_compression_only": A,
        "scenario_B_with_llm_qa_gate": B,
    }
    print(json.dumps(result, indent=2))
    return result


@app.local_entrypoint()
def measure_entry():
    print(json.dumps(measure.remote(), indent=2))
