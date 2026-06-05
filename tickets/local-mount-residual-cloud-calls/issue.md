# Bug/Tech-debt: a local-only sqlite mount still makes cloud calls (`--memory-paths ""` doesn't disable; blocks offline E2E)

- **Type:** Bug + tech-debt (backend-agnosticism) — local sqlite mount/E2E can't run without a live cloud container
- **Status:** **IMPLEMENTED 2026-06-05** — both the minimal fix AND the backend-agnostic "zero cloud
  calls" version shipped (code + clippy + tests green; mount-level E2E pending macFUSE). See
  "Implementation".
- **Created:** 2026-06-04
- **Component:** `semfs::cmd::daemon_runtime` (mount startup: `update_memory_paths`, sync, validate_key);
  `semfs::cmd::mount` (`--memory-paths` doc); WB harness `benchmarks/workspace_bench/semfscodex.py`.
- **Branch context:** `feat/backend-agnostic-store`

## Summary

Mounting a **local-only sqlite** tag (a cache seeded locally, with **no corresponding Supermemory
cloud container**) fails because the mount still makes cloud API calls that 404. Surfaced trying to run
the codex E2E on config #8 (`chanpin-e5-nosum`, per-sheet + max/best-rank RRF). The daemon log is just:

```
collected 1367 file(s) for import
Error: not found (404)
```

## The specific bug: `--memory-paths ""` does NOT disable the cloud call

`mount.rs:54–55` documents: `--memory-paths "" → disable memory generation`. But the daemon
(`daemon_runtime.rs:277–286`) calls the cloud API **even for empty paths**:

```rust
if let Some(raw) = &cfg.memory_paths {
    let paths = if raw.is_empty() { Vec::new() } else { raw.split(',')…collect() };
    api.update_memory_paths(paths).await?;   // ← called for empty paths too → cloud /v… → 404 (no container)
}
```

So passing `--memory-paths ""` still hits `update_memory_paths([])`, which 404s for a local-only tag —
contradicting the documented "disable" behaviour.

### Minimal fix
Skip the call when there are no paths (make the doc true):
```rust
if !paths.is_empty() {
    api.update_memory_paths(paths).await?;
}
```

## The broader issue: a chain of cloud-coupling assumptions blocks offline local E2E

Running the WB `semfs-codex` harness on a local-only sqlite tag hit **five** cloud/env couplings in a
row, each masking the next:
1. **Grader stale-resume** reused a 12-h-old result (not a bug — set `SEMFS_FRESH`/clear case output).
2. **`SEMFS_BIN`** not on the harness's PATH → "semfs binary not found" (harness env gap).
3. **`--no-sync`** not passed (only `--no-push`) → initial sync of a non-existent container → 404
   (`SEMFS_NO_SYNC=1` fixes; harness gates it on that env).
4. **`--memory-paths`** always passed by the harness (auto-derived from the case manifest) →
   `update_memory_paths` cloud call → 404 (this ticket's bug; empty doesn't disable it).
5. (Latent) `validate_key`/org-scoping still requires a live key — see
   `tickets/decouple-sqlite-cache-scoping-from-supermemory/`.

Net: there is **no config-only way** to mount a local-only tag through the harness today — the daemon
calls a cloud API whenever `--memory-paths` is present, and the harness always presents it.

## Proposed fix (scoped) — ✅ BOTH DONE (see Implementation)
- **Now (unblocks E2E):** ✅ skip `update_memory_paths` for empty paths — and additionally for any
  local-only mount, so the harness needn't even pass `--memory-paths ""`.
- **Right (backend-agnostic):** ✅ gated **all** cloud calls on the local-only backend, so a sqlite
  `--no-sync --no-push` mount makes **zero** network calls (`validate_key` via the decouple ticket;
  `update_memory_paths`, `warm_profile`, `initial_pull`, `unmount_scan` here; background sync already
  gated). A local sqlite tag now mounts fully offline.

## Acceptance
- ✅ `semfs mount <local-tag> --no-sync --no-push` makes **no cloud call** and does not 404 (mounts the
  warm local cache, serves search). `--memory-paths ""` no longer needed to avoid the 404 — a local-only
  mount skips the call regardless — though empty is now honored for any mount too.
- ⏳ WB `semfs-codex` runs end-to-end on a local-only sqlite tag — **code-unblocked; harness run pending**
  (needs macFUSE + the seeded cache at `~/.semfs/<tag>.db`, and the harness env gaps #1/#2 below).

## Implementation (2026-06-05)

The `local_only = no_push && no_sync` flag (introduced by the decouple ticket) now gates **every**
residual cloud call in the mount lifecycle — not just `update_memory_paths`. While fixing the reported
bug I found the chain was longer than the ticket listed: **four** pre-/post-mount calls fire on a
local-only mount, three of them soft-failing (so they hid behind the one loud `?` crash):

| Call | Site | Before | Now (local-only) |
|---|---|---|---|
| `validate_key` | startup | required (`?`) | **skipped** — done by `decouple-sqlite-cache-scoping` (#5) |
| `update_memory_paths` | `daemon_runtime` ~300 | `.await?` → **fatal 404** | skipped (`!local_only && !paths.is_empty()`) |
| `warm_profile`→`get_profile` | ~408 | soft 404, ungated | skipped (`profile.md` empty, correct offline) |
| `initial_pull` | ~415 | soft 404, ungated | skipped; `pull_succeeded = true` so auto-import still runs |
| `unmount_scan`→`deletion_scan` | ~697 | soft 404 at shutdown | skipped (no remote to reconcile) |
| background `SyncEngine` | ~516 | already gated | `pull_enabled/push_enabled = !no_sync/!no_push` → idle |

- The reported bug (`update_memory_paths` ignoring `--memory-paths ""`) is fixed at its root: the call is
  skipped for empty paths (honoring the doc) **and** for any local-only mount.
- **Behavior change (intentional):** on a local-only mount, auto-import now **proceeds** instead of being
  skipped behind a failed pull — `pull_succeeded` is forced `true` because local dedup against the warm
  cache needs no remote reconciliation. This is what makes a local seed actually import.
- **Verified:** `cargo build`/`clippy`/`test -p semfs` (45 passed) green; file re-formatted with rustfmt.
  **Not yet:** real macFUSE mount + `semfs-codex` E2E.

### Out of scope (unchanged)
- A `--no-sync`-only mount (push still on) keeps its pre-existing initial-pull/deletion-scan behavior;
  only the local-only (`no_push && no_sync`) combination is gated here, matching the acceptance.
- Harness env gaps #1 (grader stale-resume) and #2 (`SEMFS_BIN` PATH) are environmental, not daemon code.

## How it surfaced
Measuring config #8's codex token/search count (the payoff of the RRF chunk-mass fix) on a local seed.
The ranking results in `rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md` already stand
(RRF #14→#7); this ticket is about being able to run the **agent E2E** on a purely local backend.

## Related
- `tickets/decouple-sqlite-cache-scoping-from-supermemory/` — sibling cloud-coupling (validate_key/org).
- `tickets/decouple-backends-from-supermemory/` — parent backend-agnostic effort.
- `tickets/rrf-chunk-mass-and-lane-fusion/` — the fix whose E2E this blocks.
