# Tech debt: make Supermemory / sqlite / pg(lite) truly independent backends — local backends must not call Supermemory

- **Type:** Tech debt / architecture (backend-agnosticism, decoupling)
- **Status:** OPEN (design)
- **Created:** 2026-06-02
- **Component:** `semfs` mount/daemon (`cmd/daemon_runtime.rs`, `sync/*`), backend resolution (`cmd/resolve.rs`)
- **Branch context:** `feat/backend-agnostic-store`

---

## Desired end state

Treat **Supermemory, sqlite, and pg(lite)/hosted-pg as three *separate, self-contained* backends.** The backend choice should select the **source of truth**, not just where vectors are stored:

- **`supermemory`** backend → cloud store + cloud search (today's behavior).
- **`sqlite`** / **`pglite`** / **hosted `pgvector`** backend → **local/self-hosted source of truth**, seeded from a local corpus, running with **zero Supermemory calls** — no `validate_key`, no document pull, no R2 hydration, no sync/push, no cloud-search fallback.

> **If I choose sqlite / pglite / hosted-pg, Supermemory must not be contacted at all** (the daemon should run fully offline / air-gapped).

This is NOT how it works today.

---

## Current structure (as-built)

semfs is architected as **"a local filesystem + search cache over a Supermemory container."** Supermemory **owns the documents**; the local mount is a **synced projection**. The `SEMFS_STORAGE_BACKEND` choice (`sqlite` | `pgvector` | `pglite`) only decides **where the local vector index lives** — it does **not** change content ownership. So **every mount, regardless of backend, contacts Supermemory.**

```
                 Supermemory (cloud) = SOURCE OF TRUTH (owns docs + R2 bytes)
                        │  every mount: auth + pull + hydrate + sync
        ┌───────────────┼────────────────────────────┐
        ▼ (vectors→sqlite)        ▼ (vectors→pg/pglite)
   sqlite-backed mount        pg-backed mount
   — still pulls/hydrates      — still pulls/hydrates
     from Supermemory            from Supermemory
```

### The coupling points (every one must become a no-op for local backends)

| Coupling | Where | What it calls |
|---|---|---|
| **Auth** | `cmd/daemon_runtime.rs` `ApiClient::validate_key(...)` (non-ephemeral mount **fails** without it) | Supermemory key validation |
| **Document pull** | `cmd/daemon_runtime.rs` → `SyncEngine::initial_pull_with_progress` (runs **unconditionally**, even with `--no-sync`) → `sync/pull.rs` `full_pull`/`delta_pull` | `GET /v3/documents/list` |
| **Content hydration** | `sync/pull.rs` `rehydrate_if_possible` (on read of stub files) | `GET` file bytes from Supermemory R2 |
| **Periodic sync** | `sync/mod.rs` `SyncEngine::start` delta + deletion loops | gated by `--no-sync` (can disable) |
| **Push** | push worker | gated by `--no-push` (can disable) |
| **Search fallback** | `cmd/grep.rs` `CloudIndex` (`/v4/search`) | only on daemon-unreachable / failure |

### What is already local (no change needed)
- **Search on a local backend is local:** `SqliteVecStore` / `PgVectorStore` query the local store; query embedding is local (`LocalEmbedder`). No Supermemory call for a local search.
- **Offline `grep`** of a seeded sqlite `.db` (via the `.semfs` marker's `db_path`) needs no network.

So today: **local search is decoupled, but the *daemon/mount* is not** — `validate_key` + `initial_pull` + hydration always hit the cloud. `--no-sync`/`--no-push` only disable the *periodic* loops and push, **not** the initial auth/pull/hydrate.

### Why it's this way (rationale, not a bug)
The product premise is "mount your Supermemory memory as a semantic filesystem." The cloud is the durable, shared, authoritative store; the local side is a cache. A cache must authenticate, discover what to cache, fetch bytes, and stay coherent — hence the cloud calls. The decoupling below is a **new posture (local-first)**, not a defect fix.

---

## The gap

There is no **local-authoritative** mode. Choosing `sqlite`/`pglite`/`pg` changes the vector store but keeps Supermemory as the corpus owner, so the daemon still phones home. A user who wants a self-contained local/self-hosted index (offline, air-gapped, no cloud dependency, no per-mount cloud round-trips) cannot get it.

---

## Proposed change

Introduce a **source-of-truth dimension** so a chosen backend can be **local-authoritative**:

1. **Backend selection picks the source, not just the store.** e.g. `SEMFS_BACKEND=supermemory|sqlite|pglite|pgvector` (or a `--source local|supermemory` flag). For local/pg backends, the **local store + a local corpus directory is the source of truth.**

2. **For local backends, make every Supermemory call a no-op:**
   - **`validate_key`:** skip (no cloud auth; local backends are unauthenticated/self-hosted).
   - **`initial_pull`:** skip entirely (don't `list_documents`). The corpus is the local seed.
   - **Hydration:** never R2-fetch; all content is local (no stubs).
   - **Sync loops + push:** not started (already gated, but ensure they're off).
   - **Search fallback:** never fall back to `CloudIndex` (already fail-closed for pglite; extend to "local backend never touches cloud").

3. **Seeding for local backends = import a local directory once** (the `import` path, `--no-push`), producing the durable L1–L7 index. Re-seed only when the corpus changes (or add a dir-watch). This is the "seed once, reuse" model — and the local index is the source, not a cache.

4. **Supermemory remains a first-class backend** (the cloud one) — unchanged behavior when selected.

Net: backend = `supermemory` → cloud (auth + pull + hydrate + cloud search). backend = `sqlite`/`pglite`/`pgvector` → **local/self-hosted, zero Supermemory traffic.**

---

## Design decisions to settle
- **Auth for local backends:** none (self-hosted) — confirm that's acceptable, or a local key.
- **Corpus source for local backends:** a directory path (seed via import) vs an already-built `.db`/pg store. Probably both: "seed from dir" + "open existing seeded store."
- **Updates:** re-import on change vs a watcher vs explicit `semfs reseed`.
- **`hosted-pg` (external pgvector):** same local-authoritative treatment (it's just a non-embedded Postgres) — content seeded by import, no Supermemory.
- **Resolution interaction:** how `SEMFS_BACKEND` (source) composes with `SEMFS_EMBED_BACKEND` (embedder) and `SEMFS_RERANK_BACKEND`.

## Acceptance criteria
- With `SEMFS_BACKEND=sqlite|pglite|pgvector`, the daemon mounts and serves search **with the network disabled** (no Supermemory/R2 calls — verifiable by running air-gapped, or asserting zero outbound requests to `*.supermemory.ai` / R2).
- `validate_key`, `/v3/documents/list`, and R2 hydration are **never issued** for a local backend.
- `supermemory` backend behavior is unchanged.
- The 3 backends are independently seedable from the same local corpus and produce independent indexes.

## Why it matters
- **Benchmarking:** a clean 3-engine comparison (cloud vs sqlite vs pg) needs the local engines to be genuinely independent — not silently round-tripping through the cloud (which also confounds latency/cost measurements).
- **Offline / air-gapped / privacy:** self-hosted use with no cloud dependency.
- **Operational simplicity & cost:** no per-mount cloud auth/pull/hydrate for local users.
- Removes the last conceptual coupling behind the per-mount cloud calls we traced in the benchmark work.

## Related
- `tickets/bench-per-case-remount-redundancy/` (per-case re-import; the "seed once, reuse" half of this).
- `tickets/parallelize-l7/`, `tickets/solve-oom-issue/` (the local L1–L7 pipeline these backends run).
