"""Unit tests for the pure units of the router-dataset harness."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def test_context_bucket_decoupled_from_difficulty():
    from gen_tasks import assign_axes
    pairs = [assign_axes(0, i) for i in range(400)]
    assert len({d for d, _ in pairs}) == 3 and len({c for _, c in pairs}) == 5
    assert len({(d, c) for d, c in pairs}) >= 12   # not collapsed onto a diagonal


def test_build_prior_context_hits_budget():
    from gen_tasks import build_prior_context
    turns = [{"role": "user", "content": "x" * 4000} for _ in range(50)]
    s, tok = build_prior_context(turns, target_tokens=16000)
    assert 8000 <= tok <= 24000


def test_fresh_bucket_empty_context():
    from gen_tasks import build_prior_context
    s, tok = build_prior_context([{"content": "x" * 4000}], target_tokens=0)
    assert s == "" and tok == 0


def test_already_done_skips(tmp_path):
    from run_matrix import already_done
    p = tmp_path / "results.jsonl"
    p.write_text('{"task_id":"A"}\n{"task_id":"A"}\n{"task_id":"B"}\n')
    assert already_done(str(p)) == {"A", "B"}


def test_select_tasks_per_dim_and_skip():
    from run_matrix import select_tasks
    tasks = ([{"dimension": "qa_explain", "task_id": f"q{i}"} for i in range(15)]
             + [{"dimension": "code_gen", "task_id": f"c{i}"} for i in range(15)]
             + [{"dimension": "perf_optimization", "task_id": f"p{i}"} for i in range(15)])
    sel = select_tasks(tasks, per_dim=10, skip={"perf_optimization"})
    from collections import Counter
    c = Counter(t["dimension"] for t in sel)
    assert c == {"qa_explain": 10, "code_gen": 10}   # 10/dim, perf skipped


def test_parse_verdict():
    from judge_deepseek import parse_verdict
    assert parse_verdict('reasoning... VERDICT: 1') == 1
    assert parse_verdict('final\nVERDICT: 0') == 0
    assert parse_verdict('{"score": 0}') == 0
    assert parse_verdict('no verdict here') is None


def test_oracle_picks_cheapest_passing():
    from derive_oracle import derive_oracle
    from _common import PRICE
    rows = [{"model": "opus", "score": 1, "input_tokens": 100, "output_tokens": 50, "n_judges": 2},
            {"model": "haiku", "score": 1, "input_tokens": 100, "output_tokens": 50, "n_judges": 2},
            {"model": "sonnet", "score": 0, "input_tokens": 100, "output_tokens": 50, "n_judges": 2}]
    assert derive_oracle(rows, PRICE)["oracle_route"] == "haiku"


def test_oracle_none_passed():
    from derive_oracle import derive_oracle
    from _common import PRICE
    rows = [{"model": "haiku", "score": 0, "input_tokens": 1, "output_tokens": 1}]
    assert derive_oracle(rows, PRICE)["oracle_route"] == "none"
