# RCA: houqin seed SQLite corruption — incremental commit × Modal volume × spot preemption

- **Date:** 2026-06-20
- **Component:** benchmarks/modal — `build_kg` per-doc incremental commit writing to a seed DB on a Modal Volume, under spot preemption
- **Status:** RESOLVED (houqin re-embedded clean + KG rebuilt; mitigations identified)
- **Severity:** Data loss — one seed's DB (embeddings + KG) corrupted; recovered by re-embed

## Summary

During the all-6 KG run, `houqin-gemma-q4.db` became corrupted: its `build_kg` writer failed with
`database disk image is malformed (Error code 11)`, and `PRAGMA quick_check` showed extensive B-tree
damage (`rowid out of order`, `2nd reference to page`, `child page depth differs`) across many pages.
The other 5 seeds were fine. Cause = the combination of (1) `build_kg`'s **per-doc incremental
commit** (many small SQLite write transactions), (2) **Modal Volume** semantics (the main DB file and
the `-wal` file sync to/from the volume *independently*, with lag), and (3) a **spot preemption** of
houqin's writer mid-run. The preemption + restart, against a volume where main/WAL were out of sync,
let a checkpoint damage the B-tree. WAL-delete did not recover it (the corruption was in the main
file), so houqin was re-embedded from the corpus.

## Symptom

- `build_kg` for houqin: `Error: database disk image is malformed` → `build_kg failed rc=1` → the
  small-4 `parallel_kg` job failed.
- Cross-container monitor reads of houqin returned "malformed" *consistently* (other 5 seeds read
  fine in the same query).
- `quick_check` (after dropping the WAL, writer dead): still corrupt in `database main` (152 MB file),
  pages 21/23/28 and many 36xxx — i.e. real on-disk B-tree corruption, not just a torn read.

## Root cause

`build_kg` was changed this run to commit **each doc's extraction immediately** under a shared
`Mutex<Connection>` (so a preempted worker leaves resumable progress — see the resume work). That
means a steady stream of small WAL writes + checkpoints to a SQLite DB **on a Modal Volume**.

Modal Volumes are not POSIX-concurrent and do not sync a DB+WAL pair atomically: the main file and
the `-wal` can be persisted/restored at different points. Normally fine for a single writer that opens,
writes, and closes cleanly. But under a **spot preemption mid-write**:
1. the writer is killed with WAL frames not yet checkpointed into main;
2. the volume captures main and WAL at inconsistent sync points;
3. the **restarted** writer (resume) opens the DB and its checkpoint/recovery applies a WAL that
   doesn't match the main file → **B-tree corruption**.

The embed phase (also incremental, also on the volume) survived because its writes happened to
checkpoint cleanly and it wasn't preempted at the wrong instant; the KG phase's far higher commit
frequency + a real preemption hit the window. Empirically: the 5 non-preempted seeds were fine; the
one seed that took a spot preemption (houqin) corrupted.

## Why the cheap recovery failed

Deleting `-wal`/`-shm` and reopening from the last checkpoint did NOT fix it — `quick_check` still
reported corruption in the main DB. So the damage was already written into the 152 MB main file, not
confined to the hot WAL. (Salvage via SQLite `.recover` was an option but is unreliable for the vec0
`vchunks` virtual table; for a small seed, re-embed is more reliable.)

## Resolution

- Backed up the corrupt DB (`_houqin_corrupt_bak.db`).
- Deleted the corrupt `houqin-gemma-q4.db` and **re-embedded from `/data/corpus/houqin_standard`**
  via the 12-shard fan-out (CPU, no GPU). Result: clean DB (`quick_check: ok`), 2,313 files / 60.3%
  (slightly *better* coverage than the original 2,090 / 54.5%).
- Rebuilt the KG with Gemma-4-31B → 10,377 entities, 14,027 relations, 584 communities.
- The other 5 seeds were unaffected.

## Mitigations (for SQLite-writing jobs on Modal)

1. **Don't run SQLite writers on preemptible (spot) workers** when the DB lives on a Modal Volume —
   or accept that a preemption can corrupt and design recovery around it.
2. **Write to local container disk, copy to the Volume once at the end** (atomic-ish single file
   move) instead of streaming many WAL commits to the volume-backed file.
3. If incremental commit on the volume is required, **checkpoint + `vol.commit()` less often** and in
   a way that snapshots a consistent main+WAL together; avoid leaving a hot WAL across a preemption.
4. **Verify integrity after the fact** (`PRAGMA quick_check`) before trusting a volume-backed DB that
   experienced a preemption; keep a backup before destructive recovery.
5. Re-embed-from-corpus is the reliable recovery for a corrupt vec0 seed (the corpus is the source of
   truth and is cheap to re-run, especially sharded).

## Trade-off note

The per-doc incremental commit was added precisely to make `build_kg` **resumable** under preemption
(the prior bulk-commit-at-end lost everything on any preemption). That goal is right; the bug is that
incremental commit to a *Modal-volume-backed* SQLite DB trades "lose all progress on preempt" for
"small chance of corruption on preempt." Mitigation #2 (local disk + copy at end) keeps resumability
without the volume-WAL hazard and is the recommended direction.
