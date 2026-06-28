# RCA: Modal case-289 metadata lookup matched the wrong task

Date: 2026-06-11

## Symptom

The Modal `run_case(case="289", render_mode="two-tier")` smoke reached the WB
metadata lookup, printed:

```text
case metadata: /data/wb/evaluation/.generated/hf_downloads/full/task_clean_en/25/metadata.json
```

and then returned a failure payload with `rc=1`, `calls=0`, `tokens=0`.

## Root Cause

`benchmarks/modal/semfs_modal.py::_load_case_meta()` uses broad substring
matching across multiple metadata fields and paths. That was sufficient to find
some metadata file on the shared volume, but it was not strict enough to
guarantee the requested case id.

For case `289`, the matcher selected an unrelated task directory whose text
contained a matching substring, so the smoke never executed the intended
benchmark case.

## Next Fix

Tighten case selection to require an exact case id match first, and only fall
back to weaker matching if the volume layout genuinely lacks an explicit id
field.
