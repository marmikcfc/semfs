"""Assemble a v2 SOURCE corpus to close the EDA gaps: MULTI-LANGUAGE code (whitespace/indentation
lever), more CHAT, and explicitly LONG (>1500-token) passages. This is the *base* dataset the
compress_loop generation will run over next.

Push -> pmarmik/semfs-compress-sources-v2-additions (private).
Run  -> modal run benchmarks/modal/assemble_code_chat_long.py::main
"""
import hashlib
import json
import os

import modal

app = modal.App("assemble-sources-v2")
image = (
    modal.Image.debian_slim(python_version="3.11")
    .pip_install("datasets", "huggingface_hub[hf_transfer]", "tiktoken")
    .env({"HF_HUB_ENABLE_HF_TRANSFER": "1"})
)

# TOP ~12 most-used languages worldwide (incl. spelling variants across rosetta-code / github-code-clean
# so the same language matches in both sources). C# = "C sharp" in rosetta, "C#" in github.
COMMON_LANGS = {"Python", "JavaScript", "TypeScript", "Java", "C", "C++", "C#", "C sharp",
                "Go", "Rust", "PHP", "Ruby", "Swift"}


@app.function(image=image, secrets=[modal.Secret.from_name("hf-token")],
              timeout=3600, cpu=4.0, memory=16384)
def build(n_code_per_lang: int = 80, n_chat: int = 700, n_long_per_dom: int = 200):
    import tiktoken
    from collections import Counter
    from datasets import load_dataset, Dataset
    from huggingface_hub import HfApi

    token = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
             or os.environ.get("HUGGINGFACE_TOKEN"))
    enc = tiktoken.get_encoding("cl100k_base")

    def ntok(s):
        return len(enc.encode(s, disallowed_special=()))

    rows, seen = [], set()

    def add(domain, lang, text, lc):
        text = (text or "").strip()
        if len(text) < 80:
            return False
        h = hashlib.md5(text[:300].encode()).hexdigest()[:16]
        if h in seen:
            return False
        seen.add(h)
        body = text[:24000]
        rows.append({"domain": domain, "lang": lang, "doc_id": f"{domain}-{lang or 'x'}-{len(rows)}",
                     "uid": h, "original": body, "n_tokens": ntok(body), "length_class": lc})
        return True

    report = {"code_rosetta": {}, "code_github": {}, "chat": 0, "long": {}}

    # ---- CODE A: rosetta-code (huge language breadth, task-context = prose+code) ----
    try:
        rc = load_dataset("christopher/rosetta-code", split="train", token=token)
        per_lang = Counter()
        for ex in rc:
            lang = (ex.get("language_name") or "").strip()
            if lang not in COMMON_LANGS or per_lang[lang] >= n_code_per_lang:
                continue
            code = ex.get("code") or ""
            n = ntok(code)
            if 80 <= n <= 4000 and add("code", lang, code, "long" if n > 1500 else "mid"):
                per_lang[lang] += 1
        report["code_rosetta"] = {k: v for k, v in per_lang.items() if v}
    except Exception as e:  # noqa: BLE001
        report["code_rosetta"] = f"skip:{type(e).__name__}"

    # ---- CODE B: github-code-clean (real files, whitespace/indentation-rich) ----
    try:
        gh = load_dataset("codeparrot/github-code-clean", "all-all", split="train", streaming=True,
                          token=token, trust_remote_code=True)
        per_lang2 = Counter()
        it = 0
        for ex in gh:
            it += 1
            if it > 60000:
                break
            lang = ex.get("language") or ""
            if lang not in COMMON_LANGS or per_lang2[lang] >= n_code_per_lang:
                continue
            code = ex.get("code") or ""
            n = ntok(code)
            if 120 <= n <= 4000 and add("code", lang, code, "long" if n > 1500 else "mid"):
                per_lang2[lang] += 1
        report["code_github"] = {k: v for k, v in per_lang2.items() if v}
    except Exception as e:  # noqa: BLE001
        report["code_github"] = f"skip:{type(e).__name__}"

    # ---- CHAT: ultrachat, longer multi-turn ----
    try:
        chat = load_dataset("stingning/ultrachat", split="train", streaming=True, token=token)
        c = 0
        for ex in chat:
            data = ex.get("data")
            text = "\n".join(data) if isinstance(data, list) else str(data or "")
            n = ntok(text)
            if 400 <= n <= 4000 and add("chat", "", text, "long" if n > 1500 else "mid"):
                c += 1
            if c >= n_chat:
                break
        report["chat"] = c
    except Exception as e:  # noqa: BLE001
        report["chat"] = f"skip:{type(e).__name__}"

    # ---- LONG: explicitly >1500-token legal + medical ----
    for dom, repo, field, trc in [("legal", "billsum", "text", False),
                                  ("medical", "ccdv/pubmed-summarization", "article", True)]:
        try:
            ds = load_dataset(repo, split="train", streaming=True, token=token, trust_remote_code=trc)
        except Exception as e:  # noqa: BLE001
            report["long"][dom] = f"skip:{type(e).__name__}"
            continue
        c = 0
        for ex in ds:
            text = ex.get(field) or ""
            if ntok(text) >= 1800 and add(dom, "", text, "long"):
                c += 1
            if c >= n_long_per_dom:
                break
        report["long"][dom] = c

    api = HfApi(token=token)
    repo_id = f"{api.whoami()['name']}/semfs-compress-sources-v2-additions"
    Dataset.from_list(rows).push_to_hub(repo_id, private=True, token=token)
    toks = sorted(r["n_tokens"] for r in rows)
    return {"repo": repo_id, "n": len(rows),
            "by_domain": dict(Counter(r["domain"] for r in rows)),
            "code_by_lang": dict(Counter(r["lang"] for r in rows if r["domain"] == "code")),
            "long_count": sum(1 for r in rows if r["length_class"] == "long"),
            "tok_median": toks[len(toks) // 2] if toks else 0, "tok_max": toks[-1] if toks else 0,
            "sources": report}


@app.local_entrypoint()
def main(n_code_per_lang: int = 80, n_chat: int = 700, n_long_per_dom: int = 200):
    print(json.dumps(build.remote(n_code_per_lang, n_chat, n_long_per_dom), indent=2))


@app.function(image=image, secrets=[modal.Secret.from_name("hf-token")], timeout=600)
def inspect():
    from collections import Counter
    from datasets import load_dataset
    from huggingface_hub import HfApi
    token = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
             or os.environ.get("HUGGINGFACE_TOKEN"))
    me = HfApi(token=token).whoami()["name"]
    ds = list(load_dataset(f"{me}/semfs-compress-sources-v2-additions", split="train", token=token))
    code = [r for r in ds if r["domain"] == "code"]
    toks = sorted(r["n_tokens"] for r in ds)
    return {"n": len(ds), "by_domain": dict(Counter(r["domain"] for r in ds)),
            "code_langs_total": len(set(r["lang"] for r in code)),
            "code_top_langs": dict(Counter(r["lang"] for r in code).most_common(20)),
            "long_gt1500": sum(1 for r in ds if r["n_tokens"] > 1500),
            "tok_p50": toks[len(toks)//2], "tok_p90": toks[int(len(toks)*0.9)], "tok_max": toks[-1]}


@app.local_entrypoint()
def show():
    print(json.dumps(inspect.remote(), indent=2))
