"""Tests for the semfs-codex Layer-2 staging shim.

Layer 2 fix: semfs unmounts the agent's deliverables out of existence before the
Workspace-Bench grader inspects work_dir. _stage_outputs_from_mount copies them
out while the mount is live; _restore_outputs_to_workdir puts them back after
unmount so os.path.isfile() checks pass.

Run: python3 benchmarks/workspace_bench/test_semfscodex_staging.py
"""
from __future__ import annotations

import importlib.util
import os
import shutil
import tempfile

_spec = importlib.util.spec_from_file_location(
    "semfscodex", os.path.join(os.path.dirname(__file__), "semfscodex.py")
)
semfscodex = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(semfscodex)


def test_extract_returned_paths():
    assert semfscodex._extract_returned_paths(
        "blah ['model_output/onsite_hosting_execution_manual.doc'] done"
    ) == ["model_output/onsite_hosting_execution_manual.doc"]
    assert semfscodex._extract_returned_paths("['a.txt','b.md']") == ["a.txt", "b.md"]
    assert semfscodex._extract_returned_paths("no list here") == []
    # last list wins when the model emits several
    assert semfscodex._extract_returned_paths("['x']\nfinal: ['y']") == ["y"]


def test_rel_under_rejects_escape():
    assert semfscodex._rel_under("/work", "model_output/x") == "model_output/x"
    assert semfscodex._rel_under("/work", "/model_output/x") == "model_output/x"
    assert semfscodex._rel_under("/work", "../../etc/passwd") is None


def test_stage_restore_roundtrip():
    tmp = tempfile.mkdtemp()
    try:
        work = os.path.join(tmp, "work")
        sandbox = os.path.join(tmp, "sandbox")
        os.makedirs(os.path.join(work, "model_output"))
        open(os.path.join(work, "model_output", "onsite_hosting_execution_manual.doc"), "w").write("MANUAL")
        open(os.path.join(work, "model_output", "extra_unlisted.txt"), "w").write("EXTRA")
        # top-level memory-mount noise that must NOT be staged
        open(os.path.join(work, "some_memory_doc.md"), "w").write("NOISE")

        result = {"paths": [], "trace": {"lastText": "Done. ['model_output/onsite_hosting_execution_manual.doc']"}}
        api = {"__expected_output_files__": ["onsite_hosting_execution_manual.doc"]}

        staged = semfscodex._stage_outputs_from_mount(
            work_dir=work, sandbox_dir=sandbox, result=result, api_provider=api
        )
        staged_rels = sorted(r for r, _ in staged)
        assert "model_output/onsite_hosting_execution_manual.doc" in staged_rels
        assert "model_output/extra_unlisted.txt" in staged_rels, "subtree walk must catch unlisted file"
        assert "some_memory_doc.md" not in staged_rels, "must not stage top-level mount noise"

        # simulate unmount: work_dir reverts to bare underlying state
        shutil.rmtree(work)
        os.makedirs(work)
        open(os.path.join(work, "some_memory_doc.md"), "w").write("NOISE")

        restored = semfscodex._restore_outputs_to_workdir(work_dir=work, staged=staged)
        assert os.path.isfile(os.path.join(work, "model_output", "onsite_hosting_execution_manual.doc"))
        assert os.path.isfile(os.path.join(work, "model_output", "extra_unlisted.txt"))
        assert sorted(restored) == staged_rels
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_force_clear_mount_noop_on_normal_dir():
    tmp = tempfile.mkdtemp()
    try:
        # A normal directory is neither mounted nor dead → no clear attempted.
        assert semfscodex._path_is_dead_or_mounted(tmp) is False
        assert semfscodex._force_clear_mount(tmp) is False
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_stage_is_noop_when_no_outputs():
    tmp = tempfile.mkdtemp()
    try:
        work = os.path.join(tmp, "work")
        os.makedirs(work)
        open(os.path.join(work, "noise.md"), "w").write("x")
        result = {"paths": [], "trace": {"lastText": "I could not complete the task."}}
        staged = semfscodex._stage_outputs_from_mount(
            work_dir=work, sandbox_dir=os.path.join(tmp, "sb"), result=result, api_provider={}
        )
        assert staged == []
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_container_tag_is_per_agent_model():
    # Per-agent persistent: tag keys on (harness, model), not the per-case workdir.
    assert semfscodex._container_tag({"model_name": "GPT-5.4"}) == "workspace-bench-codex-gpt-5-4"
    os.environ["SEMFS_CONTAINER_TAG"] = "explicit-tag"
    try:
        assert semfscodex._container_tag({"model_name": "GPT-5.4"}) == "explicit-tag"
    finally:
        del os.environ["SEMFS_CONTAINER_TAG"]


def test_case_memory_paths_scopes_to_needed_files():
    import json
    tmp = tempfile.mkdtemp()
    try:
        work = os.path.join(tmp, "work")
        sb = os.path.join(tmp, "sb")
        os.makedirs(os.path.join(work, "Desktop", "Downloads"))
        os.makedirs(sb)
        for n in ("host_script_1.docx", "host_script_2.docx"):
            open(os.path.join(work, "Desktop", "Downloads", n), "w").write("x")
        open(os.path.join(work, "unrelated.txt"), "w").write("noise")
        os.makedirs(os.path.join(work, "model_output"))
        open(os.path.join(work, "model_output", "out.doc"), "w").write("out")
        json.dump(
            {
                "data_manifest": [{"filename": "host_script_1.docx"}, {"filename": "host_script_2.docx"}],
                "file_dep_graph": [{"from": "host_script_1.docx", "to": "out.doc"}],
                "output_files": ["out.doc"],
            },
            open(os.path.join(sb, "metadata.json"), "w"),
        )
        mp = semfscodex._case_memory_paths(work, sb)
        assert mp == "/Desktop/Downloads/host_script_1.docx,/Desktop/Downloads/host_script_2.docx", mp
        # outputs and unrelated files must NOT be in scope
        assert "out.doc" not in mp and "unrelated" not in mp
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_case_memory_paths_env_override_wins():
    os.environ["SEMFS_MEMORY_PATHS"] = "/explicit/path"
    try:
        assert semfscodex._case_memory_paths("/nonexistent", "/nonexistent") == "/explicit/path"
    finally:
        del os.environ["SEMFS_MEMORY_PATHS"]


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"PASS {name}")
    print("ALL PASS")
