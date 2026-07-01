# Tech debt: local sqlite mount needs a Supermemory key + network just to scope the cache

- **Type:** Tech debt / backend-agnosticism (local search shouldn't require a cloud round-trip)
- **Status:** **IMPLEMENTED 2026-06-05** (code + unit tests + clippy green; mount-level E2E pending,
  see "Implementation"). Root cause confirmed in code.
- **Created:** 2026-06-04
- **Component:** `semfs::cmd::daemon_runtime` (mount startup), `semfs-core::api::validate_key`,
  `semfs::cmd::auth::resolve_api_key`.
- **Branch context:** `feat/backend-agnostic-store` (this is squarely the branch's goal).

## Problem

A purely **local sqlite** mount (`--no-push --no-sync`, sqlite backend) still **requires a valid
Supermemory API key AND a network round-trip** at startup. Search is 100% local, but the *mount* can't
start offline. This blocks offline local search and forces a cloud dependency on a backend that has
none.

## Why (confirmed in code)

The mount is org-scoped, and the `org_id` is fetched from the Supermemory server:

1. `daemon_runtime.rs:197–199` (non-ephemeral mount): `validate_key(...).context("validating API key
   (required to scope cache by org)")?` — **hard-required**, `?`-propagated → mount fails without it.
2. `daemon_runtime.rs:222–228` (opening the persistent cache): `org_id = session…ok_or_else(|| "server
   did not return org id; cannot open cache")` — the persistent sqlite cache path
   `<cache>/semfs/<org_id>/<tag>.db` **can't be opened without `org_id`.**
3. `api/mod.rs:95–126` (`validate_key`): does `GET {base_url}/v3/session` and reads `org_id` from the
   **server response body** (`body["org"]["id"]`) — i.e. a live network call.

The only offline path today is **ephemeral mode** (`daemon_runtime.rs:220–221` → `Db::open_in_memory()`,
org → `"_ephemeral"`), but that's an in-memory cache — it can't reuse a seeded persistent sqlite cache.

## Key insight: the org_id is redundant — it's the key's own prefix

API keys are shaped `sm_<ORG>_<secret>`. The server's `/v3/session` returns
`org.id == api_key.split('_')[1]`. Example: key `sm_CjeeM2Seni3y9xfovYsfv4_<secret>` → server org
`CjeeM2Seni3y9xfovYsfv4` → cache dir `…/semfs/CjeeM2Seni3y9xfovYsfv4/<tag>.db`. So the value that
scopes the cache is **trivially derivable from the key offline** — the network call obtains something
we already hold. The Supermemory dependency for local scoping is **incidental, not fundamental.**

## Chosen solution (implemented) — fixed org-free cache root `~/.semfs/<tag>.db`

The implemented fix removes the org from the cache path entirely rather than re-deriving it. The local
SQLite store never sees `org_id` (`SqliteVecStore::new(db, embedder)` takes none) — org was *only* a
directory-name namespace. So the cache now lives at a fixed, key-independent location and the whole
`org_id`/`validate_key`/network chain drops out for local mounts.

```
   NOW   <cache>/<org_id>/<tag>.db     org_id ← server ← network ← valid key   ❌ offline
NEW      ~/.semfs/<tag>.db             no org, no server, no key                ✅ offline
```

Concretely (see Implementation below): `cache_db_path` is org-free; `validate_key` is skipped for
`--no-push --no-sync`; the key requirement itself is relaxed for local-only so a fully **keyless** mount
works; and a guard rejects the one new name clash (mounting inside `$HOME`, where the per-mount `.semfs`
marker file would collide with the `~/.semfs` cache dir).

### Why NOT the original "derive org from the key prefix" plan (superseded)
The first proposal was to parse `sm_<org>_<secret>` → `<org>` offline. Rejected because:
- **Unverified contract.** Nothing in the repo parses or guarantees that `org.id == key.split('_')[1]`;
  it rested on a single observed example of Supermemory's key format.
- **Fails its own acceptance.** Acceptance requires working with a *"keyless or any-string key."* A
  garbage key can't encode the real org, so prefix-parsing can't open the right cache — self-contradiction.
- A fixed root needs **no** assumption about the key at all, and is strictly simpler.

> ~~Original proposal: derive `org_id` from the `sm_<org>_<secret>` key prefix, guard with
> `is_safe_path_component`, gate `validate_key` for `sqlite + --no-push --no-sync`. "Cache-path
> compatible by construction" since the parsed prefix equals the server's org.~~ **Superseded** — the
> prefix-equals-org claim is unverified and incompatible with the "any-string key" acceptance criterion.

### Trade-off accepted: existing seeded caches must move once
Because the root changed, caches seeded under the old layout
(`…/Library/Caches/ai.supermemory.semfs/<org>/<tag>.db`) are no longer found. They need a **one-time
move** to `~/.semfs/<tag>.db` (e.g. `mv …/CjeeM2Seni3y9xfovYsfv4/chanpin.db ~/.semfs/chanpin.db`). For
the benchmark this is a single `mv`. Cross-org tag collisions (the reason the org dir existed) are
re-introduced but irrelevant for a local-first single-user machine; the tag still separates containers.

## Considerations / risks
- `validate_key` also returns user/plan info and is the auth gate for **push/sync** — only skip it when
  those are off; never skip it for cloud writes. **Honored:** skipped only for `no_push && no_sync`.
- ~~Key-prefix parsing must be defensive…~~ **N/A** — the fixed-root solution parses no key, so there is
  no `sm_<org>_<rest>` shape to validate. (`container_tag` is still validated as a safe path component
  at CLI parse time, which is what now namespaces the file.)
- **No key at all:** handled — `mount` falls back to an empty key for local-only, and `daemon_runtime`
  skips validation, so a fully keyless offline sqlite mount works (the backend-agnostic end state).
- **New clash introduced + guarded:** `~/.semfs` (cache dir) vs the `.semfs` per-mount marker *file*.
  Only collides when mounting directly in `$HOME`; rejected up front with a clear error.

## How it surfaced
Trying to run the codex E2E (`run_workspace_bench.sh semfs-codex`) on the local sqlite config #8 cache:
the harness hard-requires `SUPERMEMORY_API_KEY`, and even bypassing that, the mount itself calls
`validate_key`. Investigating "is the org hardcoded?" revealed org_id = the key prefix, fetched
redundantly from the server.

## Acceptance
- ✅ A sqlite `--no-push --no-sync` mount starts with **no network call and no valid SM key** (keyless or
  any-string key). Cache is the fixed `~/.semfs/<tag>.db` (no longer "the existing org-scoped cache" —
  superseded by the fixed root; pre-existing caches are relocated once, see trade-off above).
- ✅ Cloud paths (push/pull/cloud backends) still validate the key as today (`validate_key` retained for
  any mount where push or sync is on).
- ⏳ The Workspace-Bench `semfs-codex` harness can run sqlite configs without a live Supermemory key —
  unblocked by the keyless/offline mount; **end-to-end harness run still pending** (needs macFUSE +
  the relocated seeded cache).

## Implementation (2026-06-05)

- `semfs-core::config` — added `semfs_home()` → `~/.semfs`; `cache_db_path(container_tag)` is now
  **org-free** (`~/.semfs/<tag>.db`). Tests updated: dropped the per-org cases, added
  `cache_db_path_is_under_semfs_home_and_tagged` / `cache_db_path_is_org_independent`.
- `semfs::cmd::daemon_runtime` — (1) `validate_key` **skipped entirely** for `local_only = no_push &&
  no_sync` (`session = None`; no network); retained + required when push/sync is on. (2) Cache open no
  longer requires `org_id` (removed the `ok_or_else` "server did not return org id" bail); opens
  `cache_db_path(tag)`. (3) **Marker/cache-home clash guard**: `bail!` if the per-mount `.semfs` marker
  path equals `semfs_home()` (i.e. mounting directly in `$HOME`). pglite's `org_scope` keeps its
  `_ephemeral` sentinel fallback, so a missing session is safe there too.
- `semfs::cmd::mount` — key requirement **relaxed for local-only**: if `resolve_api_key` finds none and
  `no_push && no_sync`, fall back to an empty key instead of bailing → fully keyless offline mount.
- Comments referencing the old `<org>/<tag>` layout corrected in `daemon_runtime` and `resolve` (pglite
  doc).
- **Verified:** `cargo test -p semfs-core --lib` (282 passed) + `-p semfs` (45 passed); `cargo build`
  + `cargo clippy` clean on `default` and `pg`. **Not yet:** a real macFUSE mount + the `semfs-codex`
  benchmark E2E (requires the seeded cache moved to `~/.semfs/` and a FUSE-capable host).

## Related
- `tickets/decouple-backends-from-supermemory/` — parent backend-agnostic effort.
- `rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md` — the ranking work whose E2E this blocks.
