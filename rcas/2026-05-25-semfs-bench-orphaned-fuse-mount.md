# RCA: semfs-codex benchmark error — orphaned FUSE mount

**Date:** 2026-05-25  
**Host:** ubuntu@REDACTED_BENCH_HOST (EC2 ip-172-31-46-24)  
**Benchmark:** Workspace-Bench smoke, SEMFSCodex--GPT-5.4  
**Symptom:** status=error, 0 tokens, 1.3s fail. Also plain codex run status=failed with rollback ENOTCONN.

## Root Cause

A **stale orphaned FUSE mount** at `evaluation/filesys/houqin_workdir_Codex_GPT-5.4` caused every subsequent benchmark run to fail immediately.

### Why the mount got orphaned

The semfs daemon's push queue never drains on unmount because:
1. The Supermemory API returns `auth failed (401)` on the org-scoped push endpoint (`validating API key required to scope cache by org`). `semfs whoami` succeeds (plan: free, org: saral) but the write/push path is rejected.
2. ~3,300 docs are queued for push after each mount. At ~25 clears/30s, the queue needs ~3,900s to drain. The drain timeout fires (WARN in daemon log) and the daemon exits.
3. When the daemon exits after a drain timeout, the **kernel FUSE entry is not removed** — `mount` still shows the entry but `ls` on the path returns `Transport endpoint is not connected` (ENOTCONN, os error 107).

### How it manifested

- **semfs-codex runner:** Next `semfs mount` call tried to mount over the occupied kernel entry → `Error: File exists (os error 17)` → daemon exits before ready → adapter returns `status:error`, `errorMessage:"semfs mount failed"`, all token metrics null.
- **Plain codex runner:** Workdir path `filesys/houqin_workdir_Codex_GPT-5.4` is the same dead FUSE mountpoint. `codex_core::agents_md` trying to read AGENTS.md → `Socket not connected (os error 107)`. Rollback also hit ENOTCONN.
- The `_mount_with_retry` in `semfscodex.py:162` only retries on the string `"already mounted"` — the daemon's error message was `"daemon exited before becoming ready"` so no retry fired.

## Fix Applied

```bash
fusermount3 -u /srv/semfs-benchmark/Workspace-Bench/evaluation/filesys/houqin_workdir_Codex_GPT-5.4
```

Then re-ran without `SKIP_PREPARE=1`. Result: `status=failed` (model ran, 9,179 tokens, 61s) — infra unblocked.

## Recurring Risk

Every run still ends with a drain timeout → orphaned mount. Any subsequent rerun requires the `fusermount3 -u` cleanup first.

## Needed Fixes

1. **Investigate the 401 on org-scoped push** — likely a free-plan restriction. Consider running benchmarks with `SEMFS_NO_SYNC=1` / `--no-sync` to skip push during bench runs.
2. **Patch `semfscodex.py` `_mount_with_retry`** to detect `"File exists"` / ENOTCONN as a stale-mount signal and auto-run `fusermount3 -u` before retrying, making reruns self-healing.
3. **Patch unmount to force-remove kernel FUSE entry** even if drain times out, so the mount is never left orphaned.
