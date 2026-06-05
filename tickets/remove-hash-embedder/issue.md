# Tech debt: completely remove `HashEmbedder`

- **Type:** Tech debt / cleanup (remove a fake embedder that's used as a routing hack + test crutch)
- **Status:** **IMPLEMENTED + MOUNT-VERIFIED 2026-06-05** — all 5 migration steps landed; build +
  clippy green on `default`/`pg`/`pglite`; full suite (semfs-core 282, semfs 47) green; Codex
  adversarial review looped to 0 medium/high; and a **real macOS NFS mount E2E** confirmed both sides
  of the `is_local()` gate (cloud mount builds no index → grep routes to cloud over IPC; local sqlite
  mount builds an index → semantic grep answers offline). See "Implementation".
- **Created:** 2026-06-05
- **Component:** `semfs-core::embed::hash` (`HashEmbedder`), `embed/mod.rs`; `semfs::cmd::resolve`
  (`EmbedChoice::Hash`, `build_embedder`, `HASH_DIMS`, `local_indexing_enabled`); the backend test
  suites (`sqlite_vec.rs`, `pgvector.rs`).
- **Branch context:** `feat/backend-agnostic-store`

## Goal

`HashEmbedder` produces deterministic feature-hashed vectors with **no semantic meaning** — it is not a
real embedder. It survives only as (1) an offline "floor" when fastembed models aren't downloaded, and
(2) a **routing hack**: choosing it flips `local_indexing_enabled` false so `grep` falls through to the
cloud. Neither justifies a fake embedder in the product. Remove it entirely.

## Where it's used (enumerated)

| Site | Count | Role |
|---|---:|---|
| `embed/hash.rs` | (def) | the `HashEmbedder` struct + `Embedder` impl |
| `embed/mod.rs` | 2 | `pub use hash::HashEmbedder` + doc |
| `cmd/resolve.rs` | 2 | **production**: `EmbedChoice::Hash` → `build_embedder` → `HashEmbedder::new(384)`; `HASH_DIMS`; gates `local_indexing_enabled` |
| `backend/sqlite_vec.rs` | **82** | **tests** — the cheap deterministic embedder for nearly every store test |
| `backend/pgvector.rs` | **12** | **tests** — same |

So it's ~94 **test** usages + 2 **production** usages. Removal is two workstreams.

## The two production responsibilities to re-home (not just delete)

`HashEmbedder` can't be deleted outright — two production roles must be re-homed first. The design below
folds both in (no separate prerequisite ticket needed):

1. **Cloud routing.** `resolve.rs:112` defines `local_indexing_enabled = choose_embed(env) != Hash` —
   `hash` is the *only* way today to say "don't build a local index → search the cloud." Deleting `Hash`
   without a replacement makes `local_indexing_enabled` always true → **cloud search unreachable.**
   → Re-homed onto **`StorageChoice::Cloud`** (design §1, migration step 1): a cloud run needs *no*
   embedder; the cloud embeds query + docs. Routing becomes a property of *storage*, not the embedder.
2. **Offline floor.** The doc calls `HashEmbedder` "the dependency-free embedder that can run offline."
   → **Drop it** (design §4): a "search" with no semantics isn't a feature; offline = the cached local
   fastembed models.

## Ideal design (SOLID / YAGNI / DRY)

The clean fix is **not** "replace `HashEmbedder` with a better component" — it's to see that one class
is doing **three unrelated jobs** and route each to where it belongs (or delete it). The damaging one is
job #2: **"which embedder" is silently steering "where do I search."** Those are independent axes.

### 1. Make the two axes explicit and independent (SRP)
```
SEMFS_EMBED_BACKEND   = local | openai | openrouter         # HOW vectors are made (real ones only)
SEMFS_STORAGE_BACKEND = sqlite | pgvector | pglite | cloud  # WHERE search happens  ← add `cloud`
```
`cloud` becomes a first-class **`StorageChoice` variant** (it *is* a semantic-index backend — Supermemory
stores + searches). Routing keys off **storage**, never the embedder:
```rust
impl StorageChoice { fn needs_local_embedder(&self) -> bool { !matches!(self, Cloud) } }
```
`local_indexing_enabled()` — which exists only to read `Hash` — **disappears**, replaced by
`storage.is_local()`.

### 2. Resolve the embedder only when it's needed (ISP + DIP)
The mount/daemon already depends on the `SemanticIndex` trait (`CloudIndex`/`SqliteVecStore`/
`PgVectorStore` all impl it). Branch the build on storage; the embedder is an **implementation detail of
the local variants** — cloud needs none:
```rust
fn build_index(cfg) -> Arc<dyn SemanticIndex> {
    match cfg.storage {
        Cloud  => CloudIndex::new(api),                 // no embedder, honestly
        local  => { let emb = build_embedder(cfg)?;     // real embedder, required
                    build_local_store(local, emb) }
    }
}
```
Correct dependency direction: mount → `SemanticIndex`; the embedder is hidden inside local impls and not
forced where it doesn't belong. No `Option<Embedder>` leaking everywhere, no fake embedder to satisfy a
type.

### 3. The test fixture is a test fixture, not a product feature (DRY)
The ~94 test sites need *one* deterministic, offline, dims-configurable embedder: a single
`#[cfg(test)]` / `test-util`-feature **`StubEmbedder`** — today's `HashEmbedder` code relocated *out of
the shipped API*. One definition, mechanically swapped into all 94 sites; never appears in
`SEMFS_EMBED_BACKEND`.

### 4. Delete the offline floor (YAGNI)
Semantic-less hash "search" isn't a capability anyone wants — drop it; offline = the local fastembed
models (cached after first download). And **don't** invent a parallel `SearchMode` enum — `StorageChoice`
already models "where," so reusing it is the DRY choice over a second routing concept.

> **Net:** we don't *add* a component — we **dissolve** `HashEmbedder`. Routing moves onto the axis that
> already owns "where" (`StorageChoice::Cloud`); the embedder dependency retreats into the local index
> impls; the test role becomes a test double. Fewer concepts, each with one responsibility.

## Migration (incremental; each step green)
1. Add `StorageChoice::Cloud` + `needs_local_embedder()`; branch `build_index`/`resolve_index` on it
   (cloud → `CloudIndex`, no embedder). Replace `local_indexing_enabled` callers with `storage.is_local()`.
   *(Cloud routing is now honest — `hash` no longer needed for it. This is the prerequisite, folded in.)*
2. Remove `EmbedChoice::Hash`, the `build_embedder` Hash arm, `HASH_DIMS`, the `"hash"` parse, and the
   now-moot `explicit_backend_without_embedder(hash)` cases + their tests.
3. Add `StubEmbedder` (test-support); migrate the ~94 `HashEmbedder::new(N)` test sites.
4. Delete `embed/hash.rs` + the `mod.rs` re-export/doc; drop the offline-floor claim.
5. Build + `clippy` + full test suite green on `default` and `pg` features.

## Acceptance
- No `HashEmbedder` in the shipped crate (production or public API); a test-only deterministic embedder
  replaces it in the suites.
- Cloud search still reachable — via the new cloud-only mode, **not** via a fake embedder.
- `SEMFS_EMBED_BACKEND` options reduce to real ones (`local` | `openai` | `openrouter`); `hash` is gone.
- All tests pass without `HashEmbedder`.

## Why it matters
Removes a fake embedder masquerading as a config option, and forces the cloud-routing path to be
expressed honestly (a cloud-only mode) rather than "pick garbage vectors so indexing silently turns
off." Surfaced while reproducing the Supermemory baseline, where we had to set `SEMFS_EMBED_BACKEND=hash`
purely to route `grep` to the cloud.

## Implementation (2026-06-05)

Landed as designed — routing moved onto `StorageChoice`, the embedder retreated into the local impls,
and the fake embedder became a test double.

**semfs-core**
- `embed/hash.rs` → `embed/stub.rs` (`git mv`): `HashEmbedder` → `StubEmbedder`, identity `hash:N`→`stub:N`,
  docs rewritten to "test double only".
- `embed/mod.rs`: the module + re-export are now `#[cfg(test)]` and `pub(crate)` — `StubEmbedder` is
  **out of the shipped crate / public API** (acceptance #1). Dropped the public `pub use hash::HashEmbedder`.
- `backend/sqlite_vec.rs` (82) + `backend/pgvector.rs` (12): mechanically migrated all 94 sites to
  `StubEmbedder`.

**semfs (`cmd/resolve.rs`)**
- Removed `EmbedChoice::Hash`, `HASH_DIMS`, the `"hash"` parse arm, the `build_embedder` Hash arm, the
  `HashEmbedder` import, and the whole `local_indexing_enabled` fn.
- Removed `explicit_backend_without_embedder` (dead once `hash` is gone — it could only fire on the
  hash+explicit-backend contradiction, which no longer exists).
- Added `StorageChoice::Cloud` + `is_local()` (the routing predicate that replaced `local_indexing_enabled`);
  `as_str()` → `"cloud"`; `storage_choice_from` parses `"cloud"`/`"supermemory"`. `ResolveEnv` docs updated
  (`SEMFS_EMBED_BACKEND` = local|openai|openrouter; `SEMFS_STORAGE_BACKEND` gains `cloud`).
- Tests: dropped the hash/`explicit_backend` cases; added storage-parse, `is_local()`, and a
  marker round-trip (`as_str()`↔`storage_choice_from`) test incl. `Cloud`.

**semfs (`cmd/grep.rs`)** — `resolve_index` now gates on `choice.is_local()` (the marker's storage
backend), not the embedder env; added the `Cloud` match arm (unreachable under the gate) and added
`Cloud` to the daemon-`Failed` `cloud_safe` set. Doc rewritten.

**semfs (`cmd/daemon_runtime.rs`)** — the mount index-build gate is now `if storage.is_local()`; deleted
the `explicit_backend` var and the `explicit_backend_without_embedder` bail. `Cloud` arms added to the
`build_local_indexer` match, the build-failure fail-open/closed match (both `unreachable!`), and the
marker `db_path` match (`Cloud => None`, so a cloud mount advertises no local vec db).

**The two production responsibilities, re-homed**
1. *Cloud routing* → `StorageChoice::Cloud` (set `SEMFS_STORAGE_BACKEND=cloud`, not the old
   `SEMFS_EMBED_BACKEND=hash` hack). Round-trips through the `.semfs` marker.
2. *Offline floor* → dropped (YAGNI); offline = cached local fastembed models.

**Adversarial review (Codex), round 1 → fixed**
- *Medium:* removing `hash` made `SEMFS_EMBED_BACKEND=hash` *silently* resolve to local instead of
  signalling the migration. Fixed: `build_embedder` now rejects `hash` with a message naming the
  replacement (`SEMFS_STORAGE_BACKEND=cloud`). Because the guard lives where a real local embedder is
  needed, a `hash`+SQLite mount still fails OPEN to cloud (old behavior) with a clear log; `hash`+pgvector/
  pglite fails closed (old hard error); `cloud` storage never reaches it. New unit test locks the message.

**Verification**
- `cargo clippy`/`build` green on `default`, `pg`, `pglite`. Tests: semfs-core **282**, semfs **47**.
- **Routing E2E** (real `target/debug/semfs grep` + mock Supermemory `/v4/search`, daemon-unreachable
  path): `backend=cloud` → cloud index + hit; `backend=pglite` → fails closed, no cloud; `backend=sqlite`
  no-index → documented cloud degraded-fallback.
- **Real mount E2E (macOS NFSv3 — no macFUSE needed; that was the wrong check).** Mounting works
  unprivileged via the in-process localhost NFS server (`mount/nfs.rs`):
  - *Cloud mount* (`SEMFS_STORAGE_BACKEND=cloud`, ephemeral, mock API): mounted; daemon log shows **no**
    "local semantic index enabled" (gate `is_local()`=false → no index built); `grep` over the **live
    mount** routed through the daemon **IPC** path to cloud (`/x.md:cloud hit`, mock got `/v4/search`);
    NFS read/write worked; clean unmount.
  - *Local sqlite mount* (default storage, `--no-sync --no-push`, keyless): daemon built the index
    ("local semantic index enabled" + code lane + L5 reranker, real fastembed); wrote a file into the
    live NFS mount; a **semantic** `grep "which product sold the most"` answered from the LOCAL index
    (matched "best-selling product… outsold every other item") with **no key/network**; clean unmount.

## Related
- The "first-class cloud-only search mode (no embedder)" is **folded into this ticket** as
  `StorageChoice::Cloud` (design §1 / migration step 1) — no separate prerequisite ticket. It also
  advances `decouple-backends-from-supermemory` (makes cloud a real, honest backend choice).
- `tickets/decouple-backends-from-supermemory/` — parent backend-agnostic effort.
- `tickets/explore-agent-search-behavior/` — the Supermemory-trace work that exposed the `hash` hack.
