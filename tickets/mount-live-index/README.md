# Does mounting a local folder index its content? — analysis

**Folder:** `tickets/mount-live-index/` · **Linear:** [SEM-54](https://linear.app/semfs/issue/SEM-54) · Date: 2026-07-05

**Question.** If you `semfs mount` a local folder that already has files in it, does the
mount **index** that content (so it becomes semantically searchable) — or does it only
serve the files? And is the resulting index good enough to use as a benchmark seed?

---

## TL;DR (empirically verified, not just code-read)

| aspect | result | evidence |
|---|---|---|
| **Search index** (embeddings / `chunks`) | ✅ **YES** — built on import | mounted a 2-file folder → `chunks=2`, both files' text embedded + searchable |
| **FS tree** (`fs_dentry`, mountable/`cat`-able) | ✅ **YES** — built on import | `fs_dentry=8` |
| **Knowledge graph** (`graph_entity`/`graph_relation`) | ❌ **NO** — not built on import | `graph_entity=0`, `graph_relation=0` |
| **Keyless / offline** | ⚠️ **only with `--no-push --no-sync`** | fresh mount without them → `Error: API key required` |
| **KG builder used** (if it *did* run) | ⚠️ LLM co-mention (`extract_entities`), **not gliner**, no Leiden communities | code trace + `graph_*=0` |

**One-liner:** mounting a local folder **does** index it *for search* (embeddings), but it does
**not** produce an arms-ready seed — no gliner KG, no typed relations, no Leiden communities.

---

## The experiment

```
folder/  notes.md   → "The Quokka authentication module issues JWT tokens for the Zephyr gateway…"
         handler.go → "// QuokkaHandler validates JWT tokens and forwards to the Wombat datastore."

SEMFS_EMBED_BACKEND=local  semfs mount mtest --path folder --backend nfs --no-push --no-sync
# wait ~12s for the async on-import indexer
sqlite3 ~/.semfs/mtest.db:
   chunks: 2        ← both files embedded  (Quokka content present in BOTH /handler.go and /notes.md)
   fs_dentry: 8     ← FS tree materialized
   graph_entity: 0  ← KG NOT built
```

So the import path embedded the content (search works) and materialized the tree (mount serves
files), but built **no KG**.

## Why — the code path

Mount-over-a-non-empty-dir imports each existing file, and import goes through the *write* path
which triggers the local indexer:

```
daemon import loop            crates/semfs/src/cmd/daemon_runtime.rs:472-482
  → import_file_with_ownership → handle.write() → handle.flush()   crates/semfs-core/src/cache/fs.rs:931
      → flush() calls indexer.index(ino, path, text)               crates/semfs-core/src/cache/file.rs:307-315
          → SqliteVecStore embeds → vchunks (SEARCH)   ✅ fires on import
```

But the **KG** is a *separate* method, `index_graph()`, which:
- calls `graph::extract_entities(&llm, …)` — the **LLM** path (needs an LLM key), entities-only
  **co-mention**, NOT the typed `extract_graph`, and NOT gliner
  (`crates/semfs-core/src/backend/sqlite_vec.rs`);
- did **not** run here (no LLM configured on a local-only mount → `graph_entity=0`).

And **Leiden communities** (needed for `ppr_map`) are only built by the batch `materialize_kg`
step — never incrementally on the live path (`crates/semfs-core/src/backend/community.rs`).

## Keyless / offline caveat

`semfs mount` is a Supermemory-container client. A **fresh** tag tries to register a cloud
container → `Error: API key required`. The keyless offline path is gated on
`local_only = --no-push && --no-sync` (`crates/semfs/src/cmd/mount.rs:114-124`,
ref `tickets/decouple-sqlite-cache-scoping-from-supermemory`). Mounting a *pre-built* `.db`
already at `~/.semfs/<tag>.db` also works keyless (that's how the sftpgo seed mounted earlier).

---

## Two live builders vs one batch builder (the root of it)

semfs has **two** KG builders and this question sits on the seam:

| | **live** (daemon, on-write/import) | **batch** (`build_kg` example) |
|---|---|---|
| trigger | per file, on flush | whole corpus, one run |
| embeddings | ✅ | ✅ (`seed_dir`) |
| KG | LLM `extract_entities` (co-mention) | gliner `extract_graph` (typed) + tree-sitter AST |
| communities | ❌ | ✅ (`materialize_kg`, global Leiden) |
| gliner (this session's work) | ❌ not wired into the live path | ✅ wired into `build_kg` |
| deterministic | ❌ | ✅ |

Communities are inherently **global** (you can't Leiden-refine one file at a time), so `ppr_map`
can only come from the batch finalize. The gliner integration went into the **batch** builder.

---

## Implications

1. **For "point semfs at a local project and get semantic search"** → mounting works: it indexes
   on import (with a local embedder + `--no-push --no-sync`). Good for supermemory-style search.
2. **For the SWE-Atlas arms seed** → mounting is **not** a substitute for the batch build. It would
   yield a search index + (at best) a weak LLM co-mention KG and **no `ppr_map`**. Use
   `seed_dir → materialize_fs → build_kg --gliner → materialize_kg`, then mount the finished `.db`.

## Follow-ups
- ✅ **DONE — gliner wired into the daemon's `index_graph`** (uncommitted). The live path now builds
  the **typed gliner KG** on import (GPU-free), not the LLM co-mention path. Behind the `gliner-kg`
  feature (default when compiled; `SEMFS_KG_EXTRACTOR=llm` forces LLM). `GlinerCell` loads the model
  once (`OnceLock` + `Mutex`, fail-open). Verified: mounting `/tmp/glf` → `graph_entity=7`,
  `graph_relation=5` with gliner kinds (database/module/service/library) + typed relations
  (calls/depends on/…), vs the `graph_entity=0` baseline above. Default suite stays 380 green.
  Changes: `crates/semfs-core/src/backend/sqlite_vec.rs`, `crates/semfs/Cargo.toml`.
  **So "mount = search + typed KG" is now TRUE.**
- ✅ **CORRECTED (SEM-56): Leiden communities are NOT batch-only.** The daemon already materializes
  them on the live path (debounced queue-settle → `kg_refresh` → `materialize_projection`, from commit
  `60a9b11`) — verified: a live mount yields a populated `graph_community`. An earlier note here
  claiming "batch-only / ppr_map broken on live" was wrong (unverified). Remaining open item is
  community *quality* for code-heavy repos (file-qualified AST entities → fragmented communities),
  which is entity-resolution (SEM-51), not scheduling.
- Confirm the earlier mac `semfs grep` "configure a local embedder" was a search-side embedder
  resolution issue (indexing itself demonstrably works — chunks were written).

## Reproduce
```
cargo build --release -p semfs --bin semfs
mkdir /tmp/f && echo "The Quokka module issues JWT for Zephyr." > /tmp/f/notes.md
SEMFS_EMBED_BACKEND=local target/release/semfs mount t --path /tmp/f --backend nfs --no-push --no-sync
sleep 12; sqlite3 ~/.semfs/t.db "SELECT COUNT(*) FROM chunks; SELECT COUNT(*) FROM graph_entity;"
target/release/semfs unmount t
```
