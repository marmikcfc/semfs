# Materialize KG communities on the live/daemon path → `ppr_map` on mount

**Folder:** `tickets/live-communities-ppr-map/` · **Linear:** [SEM-56](https://linear.app/semfs/issue/SEM-56) · Date: 2026-07-06

## ✅ RESOLVED — the premise was wrong (2026-07-06)

**Communities were never batch-only.** Investigation (before writing code) found the settle→materialize
mechanism already in the tree: `graph_queue.rs:164` debounces the KG recompute (`if dirty &&
set.is_empty() && queue.is_settled() { kg_refresh() }`, 500ms tick), and `daemon_runtime.rs` wires
`kg_refresh → fs.refresh_knowledge_graph → graph_file::materialize_projection`, gated on `kg_enabled()`
(both a one-shot pre-mount call and the debounced settle callback). From commit `60a9b11` (2026-06-07).

So `ppr_map`'s community layer **already works on a live mount**. Verified (independent E2E): mount a
folder → `graph_community` populated (4 communities / 7 entities in a mixed Go+README corpus); grows
on live edits. No production fix needed — added 2 **regression tests** (`graph_queue.rs`) the mechanism
lacked: once-per-settle-not-per-write, and settle→`materialize_projection` populates communities.

**Real remaining item (NOT scheduling):** community *quality* for code-heavy corpora — the AST lane's
file-qualified entity paths (`pkgA.widget.Widget`) share no entities across files, so Louvain
fragments them toward singletons. Batch sftpgo got 7 real communities because its doc lane adds shared
prose entities. This is **entity-resolution / cross-lane linking — SEM-51 territory**, worth a separate
ticket if `ppr_map` quality on pure-code seeds matters.

---

## Problem (as originally — and wrongly — framed)

`ppr_map` needs Leiden **communities** (`graph_community` / `graph_god_node`). Those are produced
ONLY by the batch `materialize_kg` step — the daemon's **live** path (import/on-write) builds
entities/relations/edges but **never the community projection**. So a **live-mounted** repo has no
communities → **`ppr_map` is broken** on it (and `semfs_map.py`'s workspace map is empty). This is
the last live-vs-batch gap after SEM-54 (live gliner KG) and SEM-55 (live AST code lane).

## Key fact — this is a *scheduling* problem, not "incremental Leiden"

The finalize is a single **cheap public** function:
`semfs_core::cache::graph_file::materialize_projection(&conn)` — Louvain modularity + Leiden
refinement over the file↔entity **edge table** (no LLM, no network, no embeddings). `materialize_kg`
is just a ~20-line wrapper around it. `hidden_kg.rs` reads `graph_community` (community prior);
`benchmarks/e2b/semfs_map.py` reads it for the map. So we do NOT need incremental clustering — we
need to **call `materialize_projection` on the daemon's DB once the graph has settled** (the
`materialize_kg` doc note: *"the mount can't run Louvain per ls, so it must be materialized once"*).

## Goal

After a repo is mounted + fully indexed, `graph_community` is populated automatically, so `ppr_map`
works on a live-built seed — completing the "mount a repo → full KG (search + dual-lane KG +
communities)" story.

## Approach — trigger `materialize_projection` when the KG settles

**Recommended: auto, debounced, on the daemon.** The live KG runs through the async `graph_queue`
(SEM-54). Watch it: when the queue **drains and stays empty for a debounce window** (e.g. 2–5 s of
no new graph work), run `materialize_projection(&conn)` once on the daemon's connection. Subsequent
writes re-enqueue → on the next settle, re-materialize (bounded: it's cheap, and debounce prevents
per-write churn). This makes a mounted repo self-complete: import → index (dual-lane) → settle →
communities.

Implementer decides the exact mechanism but MUST satisfy:
- **Runs once per settle, NOT per file write** (Louvain over the whole graph is O(graph); per-write
  is the thing materialize_kg explicitly forbids).
- **Single-writer safe** — run on the daemon's own `conn` (the sole writer) inside its lock, or
  otherwise serialize against `index_graph` writes; do not open a second writer on the same WAL db.
- **Gated** so it only runs when there's a KG to project (skip on empty graphs / non-KG mounts). It
  can reuse the existing KG-enabled signal (`kg_enabled()` / `gliner_mode_active()` context).
- If a clean debounce hook into the queue is hard, a **fallback** is acceptable: an on-demand
  trigger (an IPC command or `semfs materialize-kg <tag>` subcommand that routes to the daemon to
  run `materialize_projection`), which the benchmark harness calls after polling the queue to empty.
  Document whichever you ship.

## Scope / constraints (CLAUDE.md)

- Reuse `materialize_projection` — do NOT reimplement Louvain. Surgical; match daemon style.
- No new heavy deps; GPU-free; no cloud/GPU during dev.
- Keep it behind the same live-KG gate; a non-KG or search-only mount must not pay for it.
- Don't regress the default suite.

## Testing (verify, don't assume)

1. `cargo test -p semfs-core` (default, ~382) + `--features gliner-kg` suite green. NOTE:
   `mount::nfs::tests::find_free_port_returns_bindable_port` is known-flaky (TOCTOU) — if ONLY it
   fails, re-run in isolation; not a regression.
2. Both feature builds compile.
3. Focused test for the settle→materialize trigger (can drive the projection path directly or via a
   small in-memory graph).
4. **Decisive live E2E:** build `semfs` with the feature; `mkdir /tmp/commlf` with ~4–6 files that
   form ≥2 topical clusters (e.g. two small Go packages that don't reference each other + a shared
   util). Mount keyless:
   `SEMFS_EMBED_BACKEND=local SEMFS_EMBED_MODEL=gemma <semfs> mount commlf --path /tmp/commlf --backend nfs --no-push --no-sync`
   Wait for indexing to settle (a bit past the debounce), then
   `sqlite3 ~/.semfs/commlf.db "SELECT COUNT(DISTINCT community_id) FROM graph_community; SELECT COUNT(*) FROM graph_community; SELECT COUNT(*) FROM graph_god_node;"`
   **EXPECT:** `graph_community` populated (≥1 community, member rows > 0) — where the baseline
   (current live path) is **0**. Report the actual numbers. Unmount + clean up + kill stray daemon.
   (If the gemma embed model download is blocked, land code + unit/build green and give the exact
   E2E command; model is likely HF-cached ~1.8GB.)

## Reference
- `examples/materialize_kg.rs` + `cache::graph_file::materialize_projection` — the exact call to reuse.
- **SEM-54** (`tickets/mount-live-index/`) — the live `graph_queue` + `GlinerCell` patterns; the
  settle-detection likely hooks the same queue.
- `crates/semfs-core/src/backend/community.rs` — the Louvain/Leiden detector (do not touch; just feed it).

Related: SEM-50 (arms; `ppr_map` arm), SEM-51 (KG), SEM-54, SEM-55.
