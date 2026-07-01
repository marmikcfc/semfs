"""TDD for run_judge_xafs.py — the Supermemory-faithful xAFS answer-judge.

Pure logic only (no network): prompt building, verdict parsing, and the
tokens-per-correct aggregation (Supermemory's headline metric). The live
Gemini call is validated separately by a gated smoke, not here.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import run_judge_xafs as J


# ── build_judge_prompt ────────────────────────────────────────────────────────

def test_judge_prompt_includes_the_triple_and_instructions():
    p = J.build_judge_prompt(
        prompt="What was the invoice amount?",
        gold_answer="$2,034",
        candidate_answer="The invoice was 2034 dollars.",
    )
    # the (question, gold, candidate) triple must all be present
    assert "What was the invoice amount?" in p
    assert "$2,034" in p
    assert "The invoice was 2034 dollars." in p
    # semantic-match (paraphrase/format tolerant) + strict JSON output contract
    assert "semantic" in p.lower()
    assert "correct" in p.lower()
    assert "json" in p.lower()


# ── parse_verdict ─────────────────────────────────────────────────────────────

def test_parse_plain_json_correct_true():
    v = J.parse_verdict('{"correct": true, "reason": "matches gold"}')
    assert v["correct"] is True
    assert v["reason"] == "matches gold"

def test_parse_fenced_json_correct_false():
    v = J.parse_verdict('```json\n{"correct": false, "reason": "wrong amount"}\n```')
    assert v["correct"] is False

def test_parse_json_embedded_in_prose():
    v = J.parse_verdict('Here is my verdict.\n{"correct": true, "reason": "ok"}\nThanks.')
    assert v["correct"] is True

def test_parse_unparseable_returns_none_for_retry():
    # No JSON / no verdict → correct=None so the caller treats it as an infra
    # parse-fail to retry, NOT a real incorrect (mirrors run_judge.py discipline).
    v = J.parse_verdict("the model rambled with no structured verdict")
    assert v["correct"] is None


# ── aggregate (tokens-per-correct = Supermemory headline metric) ──────────────

def test_aggregate_accuracy_and_tokens_per_correct():
    cells = [
        {"correct": True,  "tokens": 100},
        {"correct": False, "tokens": 200},
        {"correct": True,  "tokens": 50},
    ]
    a = J.aggregate(cells)
    assert a["n_total"] == 3
    assert a["n_judged"] == 3
    assert a["n_correct"] == 2
    assert a["accuracy"] == 2 / 3
    # total judged tokens (350) / correct answers (2)
    assert a["tokens_per_correct"] == 175.0

def test_aggregate_excludes_unjudged_from_denominator():
    cells = [
        {"correct": None, "tokens": 100},   # judge failed → unjudged, excluded
        {"correct": True, "tokens": 50},
    ]
    a = J.aggregate(cells)
    assert a["n_judged"] == 1
    assert a["n_correct"] == 1
    assert a["accuracy"] == 1.0
    assert a["tokens_per_correct"] == 50.0

def test_aggregate_no_correct_guards_divzero():
    a = J.aggregate([{"correct": False, "tokens": 100}])
    assert a["n_correct"] == 0
    assert a["accuracy"] == 0.0
    assert a["tokens_per_correct"] is None

def test_aggregate_empty():
    a = J.aggregate([])
    assert a["n_total"] == 0
    assert a["accuracy"] == 0.0
    assert a["tokens_per_correct"] is None
