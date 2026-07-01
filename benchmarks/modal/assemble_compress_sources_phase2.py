"""Assemble the Phase-2 SOURCE corpus for the LIGHT compaction buckets (keep_pct 80 & 90).

Mirrors assemble_compress_sources.py exactly, with two changes:
  - RATIOS = ["0.8", "0.9"]  (the gentle end the model never trained on)
  - each domain SKIPs past phase-1's consumed docs (ds.skip) so passages are DISJOINT
    from phase-1 — a genuinely separate base, not the same text recompressed.
Pushed PRIVATE to <user>/semfs-compress-sources-phase2. No teacher calls here.

  modal run benchmarks/modal/assemble_compress_sources_phase2.py::assemble
"""
from __future__ import annotations

import hashlib
import json
import os
from itertools import islice

import modal

app = modal.App("semfs-compress-assemble-p2")
image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets==2.21.0", "tiktoken", "huggingface_hub")
)

REPO = "semfs-compress-sources-phase2"
VAL_N = 250
TEST_N = 250
CHUNK_TOK = 1500
WHOLEDOC_LO, WHOLEDOC_HI = 1800, 4000
MIN_CHUNK_TOK = 300
MAX_CHUNKS_PER_DOC = 6
RATIOS = ["0.8", "0.9"]   # the new LIGHT buckets

# (domain, hf_id, config, split, kind, target_n, is_long, skip_docs)
# skip_docs pushes past phase-1's range (phase-1 took 3000/domain from the stream head).
# meetings (qmsum) is small -> skip 0 (content reuse at new ratios is fine for ratio-learning).
DOMAINS = [
    ("legal",     "FiscalNote/billsum",        None,          "train", "text",      2500, True,   8000),
    ("medical",   "ccdv/pubmed-summarization", "document",    "train", "article",   2500, True,  15000),
    ("financial", "eloukas/edgar-corpus",      "year_2019",   "train", "edgar",     2500, True,      0),
    ("meetings",  "pszemraj/qmsum-cleaned",    None,          "train", "qmsum",     2500, True,      0),
    ("calls",     "ccdv/mediasum",             "roberta",     "train", "document",  2500, True,  20000),
    ("web",       "HuggingFaceFW/fineweb",     "sample-10BT", "train", "text",      2500, False, 60000),
    ("chat",      "stingning/ultrachat",       None,          "train", "ultrachat", 2500, False, 40000),
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
    paras = [p.strip() for p in text.split("\n\n") if p.strip()]
    chunks, cur, cur_t = [], [], 0
    for p in paras:
        pt = tok(p)
        if cur_t + pt > CHUNK_TOK and cur:
            chunks.append("\n\n".join(cur))
            cur, cur_t = [], 0
        cur.append(p)
        cur_t += pt
        if pt > CHUNK_TOK:
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
    try:
        api.delete_repo(repo_id=repo_id, repo_type="dataset")
    except Exception:  # noqa: BLE001
        pass
    api.create_repo(repo_id=repo_id, repo_type="dataset", private=True)

    rows = {"train": [], "validation": [], "test": []}
    seen, summary = set(), {}

    for domain, hf_id, config, split, kind, target_n, is_long, skip_docs in DOMAINS:
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
            if skip_docs:
                ds = ds.skip(skip_docs)   # DISJOINT from phase-1
        except Exception as e:  # noqa: BLE001
            summary[domain] = {"hf_id": hf_id, "status": f"LOAD_FAILED: {type(e).__name__}: {str(e)[:120]}", "kept": 0}
            print(f"[{domain}] LOAD_FAILED: {e}")
            continue

        for doc_i, ex in enumerate(islice(ds, 200000)):
            if all(kept[s] >= targets[s] for s in targets):
                break
            text = _extract(kind, ex)
            if not text or not text.strip():
                continue
            if doc_i % 12 == 0 and kept["test"] < targets["test"]:
                dst = "test"
            elif doc_i % 12 == 1 and kept["validation"] < targets["validation"]:
                dst = "validation"
            elif kept["train"] < targets["train"]:
                dst = "train"
            else:
                continue

            total_t = tok(text)
            units = []
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
                    "domain": domain, "source_hf": hf_id, "doc_id": f"{domain}-p2-{doc_i}",
                    "chunk_idx": ci, "length_class": length_class,
                    "ratio_bucket": RATIOS[ratio_i % 2], "n_tokens": tok(ch), "text": ch,
                })
                ratio_i += 1
                kept[dst] += 1

        summary[domain] = {"hf_id": hf_id, "status": "ok", "train": kept["train"],
                           "validation": kept["validation"], "test": kept["test"], "train_whole_doc": whole_kept}
        print(f"[{domain}] train={kept['train']} val={kept['validation']} test={kept['test']} (whole={whole_kept})")

    dd = DatasetDict({s: Dataset.from_list(rows[s]) for s in ("train", "validation", "test")})
    dd.push_to_hub(repo_id, private=True, token=token)
    result = {"repo_id": repo_id, "split_sizes": {s: len(rows[s]) for s in rows},
              "ratio_dist": {s: {r: sum(x["ratio_bucket"] == r for x in rows[s]) for r in RATIOS} for s in rows},
              "by_domain": summary}
    print(json.dumps(result, indent=2))
    return result


@app.local_entrypoint()
def main():
    print(assemble.remote())
