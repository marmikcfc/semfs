#!/usr/bin/env python3
"""Path 3 — DeepSeek Pro judges each answer 0/1 (rubric + reference). OpenRouter.

  python3 judge_deepseek.py <out_dir> [--limit N]
reads <out_dir>/results.jsonl + tasks_prompt.jsonl -> results_judged.jsonl (resumable)
"""
import json, re, sys, time, urllib.error
from _common import read_env, http_json, JUDGE_MODEL

KEY = read_env("OPENROUTER_API_KEY", "OPENROUTER_KEY")
RUBRIC = ("You are grading whether an AI assistant's ANSWER correctly and completely addresses a "
          "developer TASK. Use the REFERENCE (if provided) as ground truth. Reply with brief reasoning, "
          "then the final line EXACTLY `VERDICT: 1` (fully correct) or `VERDICT: 0` (wrong/incomplete).")


def parse_verdict(text):
    text = text or ""
    m = re.search(r"VERDICT:\s*([01])", text)
    if m:
        return int(m.group(1))
    m = re.search(r'"?score"?\s*[:=]\s*([01])', text)
    return int(m.group(1)) if m else None


def _call(messages):
    body = {"model": JUDGE_MODEL, "messages": messages, "temperature": 0, "max_tokens": 400}
    hdr = {"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"}
    for attempt in range(5):
        try:
            r = http_json("https://openrouter.ai/api/v1/chat/completions", body, hdr, timeout=120)
            return r["choices"][0]["message"]["content"]
        except urllib.error.HTTPError as e:
            if e.code == 429 or e.code >= 500:
                time.sleep(2 ** attempt); continue
            raise
    return ""


def judge_one(task, answer, reference, n):
    user = (f"TASK:\n{task[:4000]}\n\n" + (f"REFERENCE:\n{reference[:3000]}\n\n" if reference else "") +
            f"ANSWER:\n{answer[:4000]}")
    votes = []
    for _ in range(n):
        v = parse_verdict(_call([{"role": "system", "content": RUBRIC}, {"role": "user", "content": user}]))
        if v is not None:
            votes.append(v)
        time.sleep(0.3)
    if not votes:
        return None, 0
    score = 1 if sum(votes) * 2 >= len(votes) else 0   # majority (ties -> 1)
    conf = sum(1 for v in votes if v == score) / len(votes)
    return score, len(votes)


def main():
    out = sys.argv[1]
    limit = int(sys.argv[sys.argv.index("--limit") + 1]) if "--limit" in sys.argv else None
    tasks = {t["task_id"]: t for t in (json.loads(l) for l in open(f"{out}/tasks_prompt.jsonl"))}
    done = set()
    jpath = f"{out}/results_judged.jsonl"
    try:
        for line in open(jpath):
            r = json.loads(line); done.add((r["task_id"], r["model"]))
    except FileNotFoundError:
        pass
    rows = [json.loads(l) for l in open(f"{out}/results.jsonl")]
    if limit:
        keep = list(dict.fromkeys(r["task_id"] for r in rows))[:limit]
        rows = [r for r in rows if r["task_id"] in set(keep)]
    with open(jpath, "a") as f:
        for r in rows:
            if (r["task_id"], r["model"]) in done or r.get("error"):
                continue
            t = tasks.get(r["task_id"], {})
            n = 2 if t.get("split") in ("val", "test") else 1
            score, nj = judge_one(t.get("prompt", ""), r.get("answer", ""), t.get("reference", ""), n)
            r["score"], r["n_judges"] = score, nj
            f.write(json.dumps(r) + "\n"); f.flush()
    print("judged ->", jpath)


if __name__ == "__main__":
    main()
