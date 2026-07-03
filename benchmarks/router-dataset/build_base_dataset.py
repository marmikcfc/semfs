#!/usr/bin/env python3
"""
Base dataset (Layer-2 skeleton) for the model-router benchmark (SEM-52).

Builds ONE pool at fine-tune scale, then emits TWO versions:
  base_finetune.jsonl  — large (SFT of a router model; tolerates weaker labels at volume)
  base_prompt.jsonl    — small balanced subsample of the pool (DSPy prompt tuning; GOLD labels)
v1 ⊂ v2, so hold the prompt-set's eval slice OUT of SFT training to avoid leakage.

Each row: identity/context filled now; `runs` matrix + `oracle_*` empty (fill by running
opus/sonnet/fable/haiku × reasoning + judge). Sources are relabelled by us — generator ≠ label.

Run: python3 build_base_dataset.py <out_dir> [--per-prompt 50]
"""
import itertools, json, os, sys, urllib.request
from collections import Counter, defaultdict

OUT = sys.argv[1] if len(sys.argv) > 1 else "./base_out"
PER_PROMPT = int(sys.argv[sys.argv.index("--per-prompt") + 1]) if "--per-prompt" in sys.argv else 50
os.makedirs(OUT, exist_ok=True)

PARTITIONS = ["qa_explain", "small_edit", "single_file_feature", "multi_file_refactor",
              "root_cause_debug", "test_or_review", "code_gen"]

# per-source caps for the fine-tune-scale pool (prompt version is subsampled from it)
FT = {"ourtestset": 300, "synth": 360, "codegen": 350, "hermes": 300, "openswe": 150}


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": "curl/8"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def first_split(ds):
    sp = get(f"https://datasets-server.huggingface.co/splits?dataset={ds}").get("splits", [])
    return (sp[0]["config"], sp[0]["split"]) if sp else (None, None)


def paged(ds, cfg, split, cap, chunk=30):
    """Paginated pull up to `cap` rows via the datasets-server rows API."""
    out, off = [], 0
    while len(out) < cap:
        u = (f"https://datasets-server.huggingface.co/rows?dataset={ds}"
             f"&config={cfg}&split={split}&offset={off}&length={min(chunk, cap - len(out))}")
        try:
            r = [x["row"] for x in get(u).get("rows", [])]
        except Exception:
            break
        if not r:
            break
        out.extend(r); off += len(r)
        if len(r) < chunk:
            break
    return out


def transcript(turns, max_turns=8, max_chars=4000):
    out = []
    for t in turns[-max_turns:]:
        role = t.get("role") or t.get("from") or "?"
        c = t.get("content") if t.get("content") is not None else t.get("value", "")
        if isinstance(c, list):
            c = " ".join(b.get("text", "") for b in c if isinstance(b, dict) and b.get("type") == "text") or json.dumps(c)
        out.append(f"[{role}] {(c or '')[:800]}")
    return "\n".join(out)[:max_chars]


def depth_of(n):
    return "single" if n == 0 else ("short" if n <= 6 else "deep")


def mk(source, partition, difficulty, prompt, prior_turns, extra=None):
    prior = transcript(prior_turns) if prior_turns else ""
    ctx = depth_of(len(prior_turns) if prior_turns else 0)
    tid = f"{source}__{partition}__{abs(hash((source, (prompt or '')[:120], ctx))) % 10**8}"
    return {"task_id": tid, "partition": partition, "difficulty_prior": difficulty,
            "context_depth": ctx, "source_dataset": source,
            "prompt": (prompt or "")[:6000], "prior_context": prior, "meta": extra or {},
            "runs": [], "oracle_route": None, "oracle_effort": None, "label_confidence": None}


# ------------------------------- adapters ------------------------------------
def adapt_openswe(cap):
    """Streaming (parquet) — the rows API truncates the huge `trajectory` cells."""
    got = []
    try:
        from datasets import load_dataset
        it = load_dataset("nvidia/Open-SWE-Traces", "sweagent", split="qwen35_122b", streaming=True)
        for row in it:
            traj = row.get("trajectory") or []
            cat = (row.get("metadata") or {}).get("category", "")
            resolved = row.get("resolved")
            users = [i for i, t in enumerate(traj) if t.get("role") == "user"]
            if not users:
                continue
            issue = traj[users[0]].get("content", "") or ""
            mp = (row.get("metadata") or {}).get("model_patch") or {}
            nf = mp.get("num_modified_files", 0) if isinstance(mp, dict) else 0
            part = ("root_cause_debug" if "bug" in str(cat).lower() else
                    "multi_file_refactor" if (nf or 0) > 1 else "single_file_feature")
            diff = "hard" if (nf or 0) > 2 else ("easy" if resolved == 1 and (nf or 0) <= 1 else "medium")
            got.append(mk("Open-SWE-Traces", part, diff, issue, traj[users[0] + 1: users[0] + 11],
                          {"repo": row.get("repo"), "language": row.get("language"),
                           "category": cat, "resolved": resolved, "n_files": nf}))
            if len(got) >= cap:
                break
    except Exception as e:
        print("  [openswe skip]", str(e)[:120])
    return got


def adapt_hermes(cap):
    """Streaming (parquet) — the rows API truncates long `conversations` cells."""
    got = []
    try:
        from datasets import load_dataset
        it = load_dataset("lambda/hermes-agent-reasoning-traces", "kimi", split="train", streaming=True)
        for row in it:
            conv = row.get("conversations") or []
            cat = (row.get("category") or "").lower(); sub = (row.get("subcategory") or "")
            canned = "maximum number of tool-calling iterations"
            humans = [i for i, t in enumerate(conv) if t.get("from") == "human"
                      and len(t.get("value", "")) > 40 and canned not in t.get("value", "")]
            if not humans:
                continue
            idx = humans[-1]   # last real user ask -> maximal prior context
            part = ("test_or_review" if any(w in sub.lower() for w in ("test", "review", "debug")) else
                    "small_edit" if "file" in cat else
                    "single_file_feature" if ("cod" in cat or "terminal" in cat) else "qa_explain")
            got.append(mk("hermes", part, "medium", conv[idx].get("value", ""), conv[max(0, idx - 8): idx],
                          {"category": row.get("category"), "subcategory": sub}))
            if len(got) >= cap:
                break
    except Exception as e:
        print("  [hermes skip]", str(e)[:120])
    return got


def adapt_codegen(cap):
    got = []
    for ds, field in [("openai/openai_humaneval", "prompt"),
                      ("google-research-datasets/mbpp", "text"),
                      ("bigcode/bigcodebench", "instruct_prompt")]:
        try:
            cfg, split = first_split(ds)
            for row in paged(ds, cfg, split, cap - len(got)):
                p = row.get(field) or row.get("prompt") or ""
                if p.strip():
                    got.append(mk("codegen:" + ds.split("/")[-1], "code_gen", "mixed", p, None, {}))
                if len(got) >= cap:
                    return got
        except Exception as e:
            print(f"  [{ds} skip]", str(e)[:70])
    return got


def adapt_ourtestset(cap):
    got = []
    try:
        from huggingface_hub import hf_hub_download
        p = hf_hub_download("pmarmik/claude-code-router-compress-testset", "fixtures.jsonl", repo_type="dataset")
        for line in open(p):
            row = json.loads(line)
            if row.get("door") != "UserPromptSubmit":
                continue
            pr = row.get("payload") or ""
            if not pr.strip():
                continue
            part = "small_edit" if any(w in pr.lower() for w in ("rename", "fix", "edit", "add", "update", "implement", "refactor")) else "qa_explain"
            got.append(mk("cc-testset", part, "easy", pr, None, {"model": row.get("model")}))
            if len(got) >= cap:
                break
    except Exception as e:
        print("  [ourtestset skip]", str(e)[:100])
    return got


SYN_TARGETS = ["the function `parse_config`", "utils/date.py", "the variable `res`", "the `auth` module",
               "config.py", "strings.py", "the class `UserSession`", "handlers/api.py", "the retry logic",
               "the cache layer", "the CLI entrypoint", "db/models.py", "the rate limiter", "tests/test_api.py",
               "the logging setup", "the `format_date` helper", "README.md", "the settings loader",
               "the error handler", "the webhook receiver"]
SYN_TEMPLATES = [("small_edit", "Rename {t} to something clearer."), ("small_edit", "Add a docstring to {t}."),
                 ("small_edit", "Add type hints to {t}."), ("small_edit", "Format {t} with black."),
                 ("small_edit", "Remove the trailing whitespace in {t}."), ("small_edit", "Extract a constant from {t}."),
                 ("qa_explain", "What does {t} do?"), ("qa_explain", "Which file defines {t}?"),
                 ("qa_explain", "Summarize the responsibility of {t}."), ("qa_explain", "List the public functions in {t}."),
                 ("qa_explain", "Where is {t} used?"), ("qa_explain", "Explain the control flow through {t}.")]


def adapt_synth(cap):
    out = []
    for (part, tmpl), t in itertools.product(SYN_TEMPLATES, SYN_TARGETS):
        out.append(mk("synth", part, "easy", tmpl.format(t=t), None, {}))
        if len(out) >= cap:
            return out
    return out


def dump(rows_, name):
    with open(f"{OUT}/{name}", "w") as f:
        for r in rows_:
            f.write(json.dumps(r) + "\n")
    print(f"\n== {name}: {len(rows_)} rows ==")
    print("  partition:  ", dict(Counter(r["partition"] for r in rows_)))
    print("  difficulty: ", dict(Counter(r["difficulty_prior"] for r in rows_)))
    print("  context:    ", dict(Counter(r["context_depth"] for r in rows_)))
    print("  w/ context: ", sum(1 for r in rows_ if r["prior_context"]))


def main():
    pool = []
    for fn, cap in ((adapt_ourtestset, FT["ourtestset"]), (adapt_synth, FT["synth"]),
                    (adapt_codegen, FT["codegen"]), (adapt_hermes, FT["hermes"]),
                    (adapt_openswe, FT["openswe"])):
        got = fn(cap); print(f"  {fn.__name__}: {len(got)}"); pool.extend(got)
    # dedupe by prompt text
    seen, dedup = set(), []
    for r in pool:
        k = r["prompt"]  # full prompt — Open-SWE issues share a fixed 160-char prefix
        if k and k not in seen:
            seen.add(k); dedup.append(r)
    pool = dedup

    dump(pool, "base_finetune.jsonl")

    # prompt version = balanced subsample (<= PER_PROMPT per partition) from the same pool
    by = defaultdict(list)
    for r in pool:
        by[r["partition"]].append(r)
    prompt = []
    for part in PARTITIONS:
        prompt.extend(by.get(part, [])[:PER_PROMPT])
    dump(prompt, "base_prompt.jsonl")
    print(f"\nNOTE: base_prompt ⊂ base_finetune — hold the prompt-set eval slice OUT of SFT.")


if __name__ == "__main__":
    main()
