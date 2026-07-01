"""Assemble the Phase-1 SOURCE corpus for compression-dataset generation, and
push it to HuggingFace (private). This is the INPUT to GLM generation — the
original long docs/chunks that GLM will compress. No GLM calls here.

Each row = one unit to compress, tagged with domain / length_class / ratio_bucket.
Generation (generate_compress_glm.py) reads this dataset and adds the compressed
+ reasoning fields.

Run:
  modal run benchmarks/modal/assemble_compress_sources.py::assemble
"""

from __future__ import annotations

import hashlib
import json
import os
from itertools import islice

import modal

app = modal.App("semfs-compress-assemble")

image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets==2.21.0", "tiktoken", "huggingface_hub")
)

REPO = "semfs-compress-sources-phase1"   # pushed under the token owner's namespace
VAL_N = 250                               # per-domain validation rows (test = same)
TEST_N = 250
CHUNK_TOK = 1500
WHOLEDOC_LO, WHOLEDOC_HI = 1800, 4000    # a doc in this band -> one whole_doc example
MIN_CHUNK_TOK = 300                       # skip fragments too short to be worth compressing
MAX_CHUNKS_PER_DOC = 6                     # diversity: don't let one 10-K dominate
RATIOS = ["0.7", "0.5", "0.35"]

# (domain, hf_id, config, split, kind, target_n, is_long)
DOMAINS = [
    ("legal",     "FiscalNote/billsum",                 None,          "train", "text",      3000, True),
    ("medical",   "ccdv/pubmed-summarization",          "document",    "train", "article",   3000, True),
    ("financial", "eloukas/edgar-corpus",               "year_2020",   "train", "edgar",     3000, True),
    ("meetings",  "pszemraj/qmsum-cleaned",             None,          "train", "qmsum",     3000, True),
    ("calls",     "ccdv/mediasum",                      "roberta",     "train", "document",  3000, True),
    ("web",       "HuggingFaceFW/fineweb",              "sample-10BT", "train", "text",      3000, False),
    ("chat",      "stingning/ultrachat",                None,          "train", "ultrachat", 3000, False),
    # claude_compact: DEFERRED — needs your own agent/session transcripts (you own them).
]


def _extract(kind, ex):
    if kind in ("text", "article", "document"):
        return ex.get(kind) or ex.get("text") or ""
    if kind == "edgar":
        return "\n".join(str(ex[k]) for k in ex if k.startswith("section_") and ex.get(k))
    if kind == "ultrachat":
        data = ex.get("data") or []
        return "\n".join(data) if isinstance(data, list) else str(data)
    if kind == "qmsum":
        for k in ("transcript", "input", "text", "meeting_transcript", "src"):
            if ex.get(k):
                return str(ex[k])
        return " ".join(str(v) for v in ex.values() if isinstance(v, str))
    return ""


def _chunk(text, tok, enc):
    """Greedy paragraph-packing into ~CHUNK_TOK pieces."""
    paras = [p.strip() for p in text.split("\n\n") if p.strip()]
    chunks, cur, cur_t = [], [], 0
    for p in paras:
        pt = tok(p)
        if cur_t + pt > CHUNK_TOK and cur:
            chunks.append("\n\n".join(cur))
            cur, cur_t = [], 0
        cur.append(p)
        cur_t += pt
        if pt > CHUNK_TOK:  # a single huge paragraph -> hard split by tokens
            ids = enc.encode(p, disallowed_special=())
            for i in range(0, len(ids), CHUNK_TOK):
                chunks.append(enc.decode(ids[i:i + CHUNK_TOK]))
            cur, cur_t = [], 0
    if cur:
        chunks.append("\n\n".join(cur))
    return chunks


@app.function(image=image, secrets=[modal.Secret.from_name("hf-token")], timeout=3600)
def assemble() -> dict:
    import tiktoken
    from datasets import Dataset, DatasetDict, load_dataset
    from huggingface_hub import HfApi

    enc = tiktoken.get_encoding("o200k_base")
    tok = lambda s: len(enc.encode(s, disallowed_special=()))

    token = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
             or os.environ.get("HUGGINGFACE_TOKEN"))
    api = HfApi(token=token)
    user = api.whoami()["name"]
    repo_id = f"{user}/{REPO}"
    # The first failed run created this repo as PRIVATE; push_to_hub won't flip an
    # existing repo's visibility, so a stale private repo keeps hitting the quota.
    # Delete + recreate public guarantees a clean public destination.
    try:
        api.delete_repo(repo_id=repo_id, repo_type="dataset")
    except Exception:  # noqa: BLE001
        pass
    api.create_repo(repo_id=repo_id, repo_type="dataset", private=False)

    rows = {"train": [], "validation": [], "test": []}
    seen = set()
    summary = {}

    for domain, hf_id, config, split, kind, target_n, is_long in DOMAINS:
        targets = {"train": target_n, "validation": VAL_N, "test": TEST_N}
        kept = {"train": 0, "validation": 0, "test": 0}
        whole_kept, ratio_i = 0, 0
        wholedoc_cap = int(target_n * 0.15) if is_long else 0
        try:
            args = (hf_id, config) if config else (hf_id,)
            try:
                ds = load_dataset(*args, split=split, streaming=True, trust_remote_code=True)
            except TypeError:
                ds = load_dataset(*args, split=split, streaming=True)
        except Exception as e:  # noqa: BLE001
            summary[domain] = {"hf_id": hf_id, "status": f"LOAD_FAILED: {type(e).__name__}: {str(e)[:120]}", "kept": 0}
            continue

        for doc_i, ex in enumerate(islice(ds, 200000)):
            if all(kept[s] >= targets[s] for s in targets):
                break
            text = _extract(kind, ex)
            if not text or not text.strip():
                continue

            # Assign the WHOLE doc to one split (interleaved so val/test are representative,
            # not just the head of the stream). Doc-level => no chunk leakage across splits.
            if doc_i % 12 == 0 and kept["test"] < targets["test"]:
                dst = "test"
            elif doc_i % 12 == 1 and kept["validation"] < targets["validation"]:
                dst = "validation"
            elif kept["train"] < targets["train"]:
                dst = "train"
            else:
                continue

            total_t = tok(text)
            units = []  # (length_class, chunk_idx, chunk_text); whole_doc only in train
            if dst == "train" and is_long and WHOLEDOC_LO <= total_t <= WHOLEDOC_HI and whole_kept < wholedoc_cap:
                units.append(("whole_doc", 0, text))
                whole_kept += 1
            else:
                for ci, ch in enumerate(_chunk(text, tok, enc)[:MAX_CHUNKS_PER_DOC]):
                    if tok(ch) >= MIN_CHUNK_TOK:
                        units.append(("chunked", ci, ch))

            for length_class, ci, ch in units:
                if kept[dst] >= targets[dst]:
                    break
                h = hashlib.md5(ch.encode("utf-8")).hexdigest()
                if h in seen:
                    continue
                seen.add(h)
                rows[dst].append({
                    "domain": domain,
                    "source_hf": hf_id,
                    "doc_id": f"{domain}-{doc_i}",
                    "chunk_idx": ci,
                    "length_class": length_class,
                    "ratio_bucket": RATIOS[ratio_i % 3],
                    "n_tokens": tok(ch),
                    "text": ch,
                })
                ratio_i += 1
                kept[dst] += 1

        summary[domain] = {"hf_id": hf_id, "status": "ok", "train": kept["train"],
                           "validation": kept["validation"], "test": kept["test"],
                           "train_whole_doc": whole_kept}
        print(f"[{domain}] train={kept['train']} val={kept['validation']} test={kept['test']} (whole={whole_kept})")

    dd = DatasetDict({s: Dataset.from_list(rows[s]) for s in ("train", "validation", "test")})
    dd.push_to_hub(repo_id, private=False, token=token)

    result = {
        "repo_id": repo_id,
        "split_sizes": {s: len(rows[s]) for s in rows},
        "by_domain": summary,
        "ratio_dist": {
            s: {r: sum(x["ratio_bucket"] == r for x in rows[s]) for r in RATIOS}
            for s in rows
        },
    }
    print(json.dumps(result, indent=2))
    return result


@app.local_entrypoint()
def assemble_entry():
    print(json.dumps(assemble.remote(), indent=2))
