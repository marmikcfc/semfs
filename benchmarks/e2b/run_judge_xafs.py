"""xAFS answer-judge — Supermemory-faithful semantic grading.

xAFS cases are QA with a single `gold_answer` (NOT WB-Lite multi-rubric), so this
is a separate judge from `run_judge.py`. It reproduces Supermemory's published method:

  LLM-as-judge, semantic match (paraphrase- & format-tolerant), scoring
  (prompt, gold_answer, candidate_answer) triples. Supermemory used
  gemini-3.1-pro-preview @ temp 0; we reach it via OpenRouter (google/...).
  Headline metric: tokens spent per correct answer.

Pure logic (build_judge_prompt / parse_verdict / aggregate) is unit-tested in
tests/test_run_judge_xafs.py. The live grade_one() call is validated by a gated
smoke (`--smoke`), not the unit suite.

Input cells JSONL (one per agent run): {dp, qid, arm, rep, candidate_answer, agent_tokens}
Gold answers are loaded from <cases-dir>/<dp>/question.json by qid.
"""
import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

JUDGE_MODEL = os.environ.get("XAFS_JUDGE_MODEL", "google/gemini-3.1-pro-preview")
OPENROUTER_BASE = os.environ.get("XAFS_JUDGE_BASE_URL", "https://openrouter.ai/api/v1")


# ── pure logic (unit-tested) ──────────────────────────────────────────────────

def build_judge_prompt(prompt: str, gold_answer: str, candidate_answer: str) -> str:
    """Semantic-match grading prompt over the (question, gold, candidate) triple."""
    return (
        "You are grading an answer to a question about a personal file system.\n"
        "Judge by SEMANTIC equivalence to the gold answer: be paraphrase-tolerant and "
        "format-tolerant (different wording, units, or formatting that conveys the same "
        "fact is CORRECT). A missing, hedged, or factually different answer is INCORRECT.\n\n"
        f"QUESTION:\n{prompt}\n\n"
        f"GOLD ANSWER:\n{gold_answer}\n\n"
        f"CANDIDATE ANSWER:\n{candidate_answer}\n\n"
        'Respond with ONLY a JSON object: {"correct": true|false, "reason": "<one sentence>"}'
    )


def parse_verdict(text: str) -> dict:
    """Extract {correct: bool|None, reason: str} from a judge response.

    correct=None means the verdict was unparseable → an infra parse-fail the caller
    should RETRY, never a real `incorrect` (mirrors run_judge.py discipline)."""
    if not text:
        return {"correct": None, "reason": "empty response"}
    stripped = re.sub(r"^```(?:json)?|```$", "", text.strip(), flags=re.MULTILINE).strip()
    obj = None
    for candidate in (stripped, _first_json_object(stripped)):
        if not candidate:
            continue
        try:
            o = json.loads(candidate)
            if isinstance(o, dict) and "correct" in o:
                obj = o
                break
        except (ValueError, TypeError):
            continue
    if obj is not None and isinstance(obj.get("correct"), bool):
        return {"correct": obj["correct"], "reason": str(obj.get("reason", ""))}
    # last-ditch field regex
    m = re.search(r'"correct"\s*:\s*(true|false)', stripped, re.IGNORECASE)
    if m:
        r = re.search(r'"reason"\s*:\s*"([^"]*)"', stripped)
        return {"correct": m.group(1).lower() == "true", "reason": r.group(1) if r else ""}
    return {"correct": None, "reason": "unparseable verdict"}


def _first_json_object(text: str):
    m = re.search(r"\{.*\}", text, re.S)
    return m.group(0) if m else None


def aggregate(cells: list) -> dict:
    """Accuracy + tokens-per-correct (Supermemory headline). Unjudged cells
    (correct is None) are excluded from BOTH numerator and denominator."""
    judged = [c for c in cells if c.get("correct") is not None]
    n_correct = sum(1 for c in judged if c.get("correct") is True)
    judged_tokens = sum(int(c.get("tokens", 0)) for c in judged)
    return {
        "n_total": len(cells),
        "n_judged": len(judged),
        "n_correct": n_correct,
        "accuracy": (n_correct / len(judged)) if judged else 0.0,
        "tokens_per_correct": (judged_tokens / n_correct) if n_correct else None,
    }


# ── live judge (gated smoke / CLI) ────────────────────────────────────────────

def grade_one(prompt, gold_answer, candidate_answer, model=JUDGE_MODEL,
              api_key=None, base_url=OPENROUTER_BASE, temperature=0.0, max_retries=4) -> dict:
    """One semantic-match grading call. Retries parse-fails / transient HTTP."""
    api_key = api_key or os.environ.get("OPENROUTER_API_KEY", "")
    body = json.dumps({
        "model": model,
        "temperature": temperature,
        "messages": [{"role": "user",
                      "content": build_judge_prompt(prompt, gold_answer, candidate_answer)}],
    }).encode()
    last = {"correct": None, "reason": "no attempt"}
    for attempt in range(max_retries):
        try:
            req = urllib.request.Request(
                f"{base_url}/chat/completions", data=body,
                headers={"Authorization": f"Bearer {api_key}",
                         "Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=120) as resp:
                d = json.load(resp)
            content = d["choices"][0]["message"]["content"]
            tokens = (d.get("usage") or {}).get("total_tokens", 0)
            v = parse_verdict(content)
            v["judge_tokens"] = tokens
            if v["correct"] is not None:
                return v
            last = v  # parse-fail → retry
        except (urllib.error.URLError, KeyError, ValueError, TimeoutError) as e:
            last = {"correct": None, "reason": f"infra: {e}", "judge_tokens": 0}
        time.sleep(1.5 * (attempt + 1))
    return last


def _load_gold(cases_dir, dp, qid):
    qfile = os.path.join(cases_dir, dp, "question.json")
    data = json.load(open(qfile))
    arr = data if isinstance(data, list) else [data]
    for q in arr:
        if q.get("id") == qid:
            return q.get("prompt", ""), q.get("gold_answer", "")
    raise KeyError(f"{dp}/{qid} not found in {qfile}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cells", help="input JSONL: {dp,qid,arm,rep,candidate_answer,agent_tokens}")
    ap.add_argument("--cases-dir", help="dir of <dp>/question.json (gold)")
    ap.add_argument("--out", help="output judged JSONL")
    ap.add_argument("--model", default=JUDGE_MODEL)
    ap.add_argument("--smoke", action="store_true",
                    help="live grade the dp_001/q01 fixture (correct + wrong) and exit")
    a = ap.parse_args()

    if a.smoke:
        prompt = "What was Coppertide's exact Stitch invoice amount for April 2026?"
        gold = "$2,034"
        for cand, label in [("The invoice came to 2034 dollars.", "expect CORRECT"),
                            ("It was $3,500.", "expect INCORRECT")]:
            v = grade_one(prompt, gold, cand, model=a.model)
            print(f"[{label}] correct={v['correct']} tokens={v.get('judge_tokens')} "
                  f"reason={v['reason']!r}")
        return

    judged, seen = [], set()
    if a.out and os.path.exists(a.out):  # resume-safe
        for line in open(a.out):
            c = json.loads(line)
            judged.append(c)
            seen.add((c["dp"], c["qid"], c["arm"], c["rep"]))
    out_fh = open(a.out, "a") if a.out else None
    for line in open(a.cells):
        cell = json.loads(line)
        key = (cell["dp"], cell["qid"], cell["arm"], cell["rep"])
        if key in seen:
            continue
        prompt, gold = _load_gold(a.cases_dir, cell["dp"], cell["qid"])
        v = grade_one(prompt, gold, cell.get("candidate_answer", ""), model=a.model)
        rec = {**cell, "correct": v["correct"], "reason": v["reason"],
               "tokens": cell.get("agent_tokens", 0), "judge_tokens": v.get("judge_tokens", 0)}
        judged.append(rec)
        if out_fh:
            out_fh.write(json.dumps(rec) + "\n")
            out_fh.flush()
    if out_fh:
        out_fh.close()

    overall = aggregate(judged)
    print("OVERALL:", json.dumps(overall, indent=2))
    arms = sorted({c["arm"] for c in judged})
    for arm in arms:
        print(f"  {arm}:", json.dumps(aggregate([c for c in judged if c["arm"] == arm])))


if __name__ == "__main__":
    main()
