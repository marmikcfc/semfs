#!/usr/bin/env python3
"""Path 1 — assemble the `tasks` table per DATASET_SPEC.md. Runs FREE (no model calls).

  python3 gen_tasks.py <out_dir> [--n 512]

Difficulty comes from the real source signal; context_bucket is assigned INDEPENDENTLY
(hash of the prompt) so context ⟂ difficulty. prior_context is built from real trace text
to hit the bucket's token budget. v1 uses frugal budgets (cap 96k) to conserve OAuth quota;
scale toward cc-traces' 256k+ tail later.
"""
import itertools, json, os, sys
from collections import Counter, defaultdict
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _common import http_json, count_tokens

DIFF = ["easy", "medium", "hard"]; DIFF_W = [35, 35, 30]
CTX = ["fresh", "small", "medium", "large", "xlarge"]; CTX_W = [25, 25, 20, 20, 10]
BUCKET_TOKENS = {"fresh": 0, "small": 4000, "medium": 16000, "large": 48000, "xlarge": 96000}
FILLER = []


def _pick(pairs, x):
    acc = 0
    for name, w in pairs:
        acc += w
        if x < acc:
            return name
    return pairs[-1][0]


def assign_axes(seed, i):
    """Independent deterministic (difficulty, context_bucket) from different bit-slices."""
    h = (seed * 2654435761 + i * 40503 + 12345) & 0xffffffff
    return _pick(list(zip(DIFF, DIFF_W)), h % 100), _pick(list(zip(CTX, CTX_W)), (h // 101) % 100)


def build_prior_context(turns, target_tokens):
    if target_tokens <= 0:
        return "", 0
    parts, tok = [], 0
    for t in turns:
        c = t.get("content") if isinstance(t, dict) else str(t)
        if isinstance(c, list):
            c = " ".join(b.get("text", "") for b in c if isinstance(b, dict))
        if not c:
            continue
        parts.append(c); tok += count_tokens(c)
        if tok >= target_tokens:
            break
    s = "\n\n".join(parts)
    return s, count_tokens(s)


def _rows(ds, cfg, split, n):
    out, off = [], 0
    while len(out) < n:
        u = (f"https://datasets-server.huggingface.co/rows?dataset={ds}"
             f"&config={cfg}&split={split}&offset={off}&length={min(30, n - len(out))}")
        try:
            r = [x["row"] for x in http_json(u).get("rows", [])]
        except Exception:
            break
        if not r:
            break
        out += r; off += len(r)
        if len(r) < 30:
            break
    return out


def _first_split(ds):
    sp = http_json(f"https://datasets-server.huggingface.co/splits?dataset={ds}").get("splits", [])
    return (sp[0]["config"], sp[0]["split"]) if sp else (None, None)


def src_codegen(cap):
    got = []
    for ds, pf, rf in [("openai/openai_humaneval", "prompt", "canonical_solution"),
                       ("google-research-datasets/mbpp", "text", "code"),
                       ("bigcode/bigcodebench", "instruct_prompt", "canonical_solution")]:
        try:
            cfg, split = _first_split(ds)
            for row in _rows(ds, cfg, split, cap - len(got)):
                p = row.get(pf) or row.get("prompt") or ""
                if p.strip():
                    got.append({"prompt": p, "dimension": "code_gen", "difficulty": "medium",
                                "reference": str(row.get(rf) or "")[:3000], "source": ds.split("/")[-1]})
                if len(got) >= cap:
                    return got
        except Exception as e:
            print("  [codegen", ds, "skip]", str(e)[:60])
    return got


def src_ourtestset(cap):
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
            dim = "small_edit" if any(w in pr.lower() for w in ("rename", "fix", "edit", "add", "refactor", "implement")) else "qa_explain"
            got.append({"prompt": pr, "dimension": dim, "difficulty": "easy", "reference": "", "source": "cc-testset"})
            if len(got) >= cap:
                break
    except Exception as e:
        print("  [ourtestset skip]", str(e)[:80])
    return got


SYN_T = ["the function `parse_config`", "utils/date.py", "the variable `res`", "the `auth` module", "config.py",
         "strings.py", "the class `UserSession`", "handlers/api.py", "the retry logic", "the cache layer",
         "the CLI entrypoint", "db/models.py", "the rate limiter", "tests/test_api.py", "the logging setup",
         "the `format_date` helper", "README.md", "the settings loader", "the error handler", "the webhook receiver"]
# per-dimension synth templates (difficulty, prompt) — used to TOP UP thin real-source dims
SYN_BY_DIM = {
    "qa_explain": [("easy", "What does {t} do?"), ("easy", "Which file defines {t}?"),
                   ("easy", "Summarize the responsibility of {t}."), ("easy", "Where is {t} used?")],
    "small_edit": [("easy", "Rename {t} to something clearer."), ("easy", "Add a docstring to {t}."),
                   ("easy", "Add type hints to {t}."), ("easy", "Format {t} with black.")],
    "single_file_feature": [("medium", "Add input validation to {t}."), ("medium", "Implement pagination for {t}."),
                            ("medium", "Wrap {t} with retry-and-backoff."), ("medium", "Add structured logging to {t}.")],
    "test_or_review": [("medium", "Write pytest unit tests for {t}."), ("medium", "Review {t} for bugs and edge cases."),
                       ("medium", "Add a test covering the error path in {t}."), ("medium", "Write a property-based test for {t}.")],
    "multi_file_refactor": [("hard", "Extract {t} into a shared module used across the codebase."),
                            ("hard", "Refactor {t} to dependency-inject its collaborators."),
                            ("hard", "Split {t} into interface and implementation across modules.")],
    "root_cause_debug": [("hard", "Users report {t} intermittently returns stale results — find the root cause."),
                         ("hard", "{t} throws under concurrent load; diagnose why."),
                         ("hard", "A regression made {t} 5x slower since last release; find the cause.")],
    "perf_optimization": [("hard", "{t} is a hotspot under load — profile and optimize it."),
                          ("hard", "Reduce memory allocations in {t}."), ("hard", "Halve the p99 latency of {t}.")],
}


def src_synth_dim(dim, cap):
    out = []
    for (diff, tmpl), t in itertools.product(SYN_BY_DIM.get(dim, []), SYN_T):
        out.append({"prompt": tmpl.format(t=t), "dimension": dim, "difficulty": diff, "reference": "", "source": "synth"})
        if len(out) >= cap:
            return out
    return out


def src_openswe(cap):
    got = []
    try:
        from datasets import load_dataset
        it = load_dataset("nvidia/Open-SWE-Traces", "sweagent", split="qwen35_122b", streaming=True)
        for row in it:
            traj = row.get("trajectory") or []
            users = [i for i, t in enumerate(traj) if t.get("role") == "user"]
            if not users:
                continue
            issue = traj[users[0]].get("content", "") or ""
            md = row.get("metadata") or {}; cat = md.get("category", ""); mp = md.get("model_patch") or {}
            nf = mp.get("num_modified_files", 0) if isinstance(mp, dict) else 0
            dim = ("root_cause_debug" if "bug" in str(cat).lower() else
                   "multi_file_refactor" if (nf or 0) > 1 else "single_file_feature")
            diff = "hard" if (nf or 0) > 2 else ("easy" if row.get("resolved") == 1 and (nf or 0) <= 1 else "medium")
            ref = md.get("reference_patch") or {}
            ref = ref.get("patch", "") if isinstance(ref, dict) else str(ref)
            got.append({"prompt": issue, "dimension": dim, "difficulty": diff, "reference": str(ref)[:3000], "source": "Open-SWE"})
            for t in traj[users[0] + 1: users[0] + 9]:
                c = t.get("content") or ""
                if c:
                    FILLER.append(c[:2000])
            if len(got) >= cap:
                break
    except Exception as e:
        print("  [openswe skip]", str(e)[:80])
    return got


def src_hermes(cap):
    got = []
    try:
        from datasets import load_dataset
        it = load_dataset("lambda/hermes-agent-reasoning-traces", "kimi", split="train", streaming=True)
        canned = "maximum number of tool-calling iterations"
        for row in it:
            conv = row.get("conversations") or []
            sub = (row.get("subcategory") or ""); cat = (row.get("category") or "").lower()
            humans = [i for i, t in enumerate(conv) if t.get("from") == "human"
                      and len(t.get("value", "")) > 40 and canned not in t.get("value", "")]
            if not humans:
                continue
            idx = humans[-1]
            dim = ("test_or_review" if any(w in sub.lower() for w in ("test", "review", "debug")) else
                   "single_file_feature" if ("cod" in cat or "terminal" in cat) else "qa_explain")
            got.append({"prompt": conv[idx].get("value", ""), "dimension": dim, "difficulty": "medium", "reference": "", "source": "hermes"})
            for t in conv[max(0, idx - 6): idx]:
                v = t.get("value") or ""
                if v:
                    FILLER.append(v[:2000])
            if len(got) >= cap:
                break
    except Exception as e:
        print("  [hermes skip]", str(e)[:80])
    return got


def main():
    out = sys.argv[1]; os.makedirs(out, exist_ok=True)
    n = int(sys.argv[sys.argv.index("--n") + 1]) if "--n" in sys.argv else 512
    per = max(1, n // 8)
    pool = []
    for fn, cap in [(src_codegen, per), (src_ourtestset, per),
                    (src_openswe, per * 2), (src_hermes, per * 2)]:
        g = fn(cap); print(f"  {fn.__name__}: {len(g)}"); pool += g
    seen, dd = set(), []
    for r in pool:
        if r["prompt"] and r["prompt"] not in seen:
            seen.add(r["prompt"]); dd.append(r)
    bydim = defaultdict(list)
    for r in dd:
        bydim[r["dimension"]].append(r)
    DIMS = ["qa_explain", "small_edit", "code_gen", "single_file_feature",
            "test_or_review", "multi_file_refactor", "root_cause_debug", "perf_optimization"]
    balanced, topped = [], 0
    for dim in DIMS:
        rs = bydim.get(dim, [])[:per]
        if len(rs) < per:                       # top up thin real-source dims with synth
            have = {r["prompt"] for r in rs}
            add = [r for r in src_synth_dim(dim, per - len(rs)) if r["prompt"] not in have]
            topped += len(add); rs += add
        balanced += rs[:per]
    print(f"  synth top-up: +{topped} rows to reach ~{per}/dim across {len(DIMS)} dims")
    filler = ([{"content": x} for x in FILLER] * 20) or [{"content": "Prior step output.\n" + "context tokens " * 40}] * 400
    tasks = []
    for i, r in enumerate(balanced):
        _, ctx = assign_axes(0, abs(hash(r["prompt"])) % 10 ** 6)
        prior, ctok = build_prior_context(filler, BUCKET_TOKENS[ctx])
        split = "train" if i % 5 < 3 else ("val" if i % 5 == 3 else "test")
        tid = f"{r['source']}__{r['dimension']}__{abs(hash(r['prompt'])) % 10 ** 8}"
        tasks.append({"task_id": tid, "dimension": r["dimension"], "difficulty": r["difficulty"],
                      "context_bucket": ctx, "context_tokens": ctok, "split": split,
                      "source_dataset": r["source"], "prompt": r["prompt"][:6000],
                      "prior_context": prior, "reference": r["reference"], "meta": {}})
    with open(f"{out}/tasks_prompt.jsonl", "w") as f:
        for t in tasks:
            f.write(json.dumps(t) + "\n")
    print(f"\n== tasks_prompt.jsonl: {len(tasks)} ==")
    print("  dimension: ", dict(Counter(t["dimension"] for t in tasks)))
    print("  difficulty:", dict(Counter(t["difficulty"] for t in tasks)))
    print("  context:   ", dict(Counter(t["context_bucket"] for t in tasks)))
    print("  split:     ", dict(Counter(t["split"] for t in tasks)))
    dc = defaultdict(Counter)
    for t in tasks:
        dc[t["difficulty"]][t["context_bucket"]] += 1
    print("  difficulty x context (decoupling check):")
    for d in DIFF:
        if dc[d]:
            print(f"     {d:6s}: {dict(dc[d])}")


if __name__ == "__main__":
    main()
