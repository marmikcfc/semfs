# RCA: Modal mountless grep fell back to cloud and returned 401

Date: 2026-06-11

## Symptom

`modal run benchmarks/modal/semfs_modal.py::e9w2_smoke` reached the seeded Modal
volume, copied the corpus, extracted the seed `AGENTS.md`, then `smoke_grep`
failed in every render mode:

```text
Error: auth failed (401)
```

## Root Cause

The Modal mountless workdir copied the SQLite seed DB to
`/root/.semfs/chanpin-modal.db`, but did not write a `.semfs` marker into
`/tmp/workdir`.

`semfs grep --tag chanpin-modal ...` resolved the explicit tag, but had no
tag-matched marker metadata, so `db_path` was absent. With no daemon and no
direct local index path, `grep` correctly fell back to the Supermemory cloud
backend. The smoke environment sets `SUPERMEMORY_API_KEY=dummy-local`, so cloud
search returned 401.

## Fix

`benchmarks/modal/semfs_modal.py::_prep_workdir` now writes a `.semfs` marker
with:

- `container_tag=chanpin-modal`
- `mount_path=/tmp/workdir`
- `db_path=/root/.semfs/chanpin-modal.db`
- `backend=sqlite`

This makes mountless Modal workdirs preserve the same local-index metadata that
`semfs grep` gets from a real mount.

## Prevention

Any future mountless benchmark environment must materialize both sides of the
contract:

- the visible corpus tree and `AGENTS.md`
- the `.semfs` marker pointing at the local seed DB
