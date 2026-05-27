# Semantic Filesystem — Progress & Handoff

**Branch:** `feat/backend-agnostic-store`
**Last updated:** 2026-05-27 (**all 4 spikes LOCKED — §11**; Phase 2 started. Phase 1 + 1.5 already landed.)
**Recent scope:** Settled every spike gating Phases 3/4/6/8 (decisions in §11, with three
interactive decision docs under `docs/`). Began **Phase 2** (local index schema: sqlite-vec
extension + chunks/fts5/vec0 tables). Phase 1 (trait seam) + 1.5 (grep tag-from-path) done (§10).
(Prior sessions: §9 bench harness.)

> **For the next agent:** §1–§5 are the *done* record (TypeScript, the reference implementation).
> §6 is the **frontier — the Rust/FUSE daemon (`crates/`)**; **§10 is the most recent work and the
> live entry point** (Phase 1/1.5 of backend-agnosticism + the full plan). §7 is the per-persona
> FUSE plan. §9 is the prior benchmark-harness session.
>
> NOTE: this supersedes the older layer×backend matrix (which incorrectly listed L6/L7/L8 as
> unimplemented locally — they are implemented in `sqlite-vec.ts` + `memory-volume.ts` and pass
> `tests/layers-cross-backend.test.ts`).

---

## 11. All spikes LOCKED + Phase 2 started (2026-05-27)

The four spikes that gated Phases 3/4/6/8 are **resolved by decision** (proofs roll into the phase
work). Decisions below are authoritative; three interactive decision docs capture the reasoning:
`docs/spike3-embedder-decision.html`, `docs/spike8-graph-decision.html`,
`docs/rust-backend-agnosticism-roadmap.html`.

**Spike 3 — Embedder.** Local runtime = **`fastembed`** (`5.13.4`, ONNX Runtime under the hood;
NOT raw `ort`, NOT `candle` — fastembed gives text+code+rerank in one API, correct tokenization/
pooling, and a curated model registry). Pinned models:
- text   = `EmbeddingModel::BGESmallENV15` (384d) — drop-in for the 384d schema width.
- code   = `EmbeddingModel::JinaEmbeddingsV2BaseCode` (768d) — opt-in per container.
- rerank = `RerankerModel::JinaRerankerV1TurboEN` (~150MB) — optional, fail-open.
- **cloud embedders** (HTTP, mirror the TS trait): OpenAI `text-embedding-3-small` (1536d) ·
  Relace (code, 2560d) · ZeroEntropy `zembed-1` (Matryoshka) + `zerank-2` (rerank).
- Fail-open ladder: text → HashEmbedder(384d); rerank → identity order. (Port of TS FallbackEmbedder.)
- **Multilingual fork (open, English is the default):** swap text → `MultilingualE5Small` (still 384d,
  zero schema change) + rerank → `JinaRerankerV2BaseMultilingual` if non-English content matters.
- **e2b constraint baked in:** model *download size* = mount-warm time. Big models (BGE-M3 ~2.3GB,
  bge-reranker-v2-m3) are workstation-only. Prefer small + quantized (`…Q`) variants.
- **Proof still to run (rolls into Phase 3):** fastembed compiles in-workspace, `embedding.len()`
  == 384/768, **offline pre-cache** works under `unshare -n` (bundle model dir, no first-use HF
  download), onnxruntime links on linux-x86_64. If the native dep fails on e2b, escape hatch = candle.

**Spike 4 — sqlite-vec.** The TS `sqlite-vec.ts` resolves the SQL **verbatim** (DDL, `MATCH … k …
ORDER BY distance` KNN, fts5 `rank`, RRF `1/(60+rank)` summed in app code). Two Rust-specific locks:
- **Extension load = static auto-registration** via `sqlite3_auto_extension(sqlite3_vec_init)` BEFORE
  opening connections (the `sqlite-vec` crate) — compiled into the binary, **no `.so`/`.dylib` to
  ship** (clean for e2b). NOT runtime `load_extension`.
- **Vector binding = little-endian `f32` BLOB** (sqlite-vec's native format), not the JSON string the
  TS uses. (JSON is the fallback if the BLOB path fights us.)
- **Risk to watch (Phase 2 proof):** `sqlite-vec`'s `libsqlite3-sys` vs rusqlite 0.32 bundled — version/
  symbol conflicts. This is exactly what the in-workspace vec0 KNN proof verifies.

**Spike 6 — Postgres tier.** Pinned, **spike deferred to Phase 6** (independent multi-writer tier,
partly a Linux concern): `sqlx` + `pgvector` (`0.4.2`) for cloud Postgres; `pglite-oxide` for local
embedded mode behind `--features pg-local`; one adapter selected by connection string. The only true
open verification — does `pglite-oxide` publish a **linux-x86_64** artifact — is checked at Phase-6
time on the actual target, not from macOS.

**Spike 8 — L7 graph.** **NOT a temporal graph.** semfs sits on a *mutable filesystem*, not Graphiti's
append-only episode stream — edits overwrite, deletes delete, `mtime`=freshness, git=audit. So bi-temporal
(`valid_to`/`is_latest`) modeling is rejected; it would re-implement the FS's own semantics. L7 =
**navigable file/entity link graph**, re-derived on write, pruned on delete. Tiers (fail-open, like the
embedder):
- **Tier 0 — regex floor:** port TS `extractEdges`/`EdgeMap` to Rust (`imports`/`references`/`wikilink`).
  Zero-LLM, always on. (The gbrain model — which semfs's TS already implements.)
- **Tier 1 — gpt-4.1-mini extraction:** structured-output JSON, steered by a built-in **default workspace
  ontology** (below). Entities → `/memories/<slug>.md` files (reuses the wikilink→file path). Async after
  flush, sampled, fail-open → regex floor. (Cloud + per-write cost; offline ⟺ no LLM.)
- **Default ontology (cross-functional workspace — PM/biz/finance/dev):** entity types
  `Person · Organization · Project · Decision · Task · Event · Artifact · Concept`; edges
  `owns/responsible_for · works_at/member_of · decided · about · depends_on/blocks · produced/authored ·
  participated_in · mentions/relates_to(fallback) · references/imports(regex floor)`.
- **Custom/role-specific ontology = DEFERRED** (the documented extension point; also unlocks a deterministic
  no-LLM offline path via gazetteer/patterns; optional GLiNER NER booster noted, not in scope).
- **Spike work (rolls into Phase 8):** pin the gpt-4.1-mini structured-output schema + prompt + measured
  per-write cost. NOT temporal.

**Phase 2 — DONE (this session, TDD, all on `feat/backend-agnostic-store`, not yet committed).**
Local index schema landed in `crates/semfs-core/src/cache/{db.rs,schema.sql}` + lib.rs. **166 tests
pass**, `cargo build` + `cargo clippy -p semfs-core` clean.
- **T1 (= spike-4 proof):** `sqlite-vec` registered statically via `sqlite3_auto_extension(
  sqlite3_vec_init)` in a `Once`, before `Connection::open` (in `Db::open`/`open_in_memory`). Test
  `vec0_knn_returns_nearest_neighbours_in_order` inserts f32-LE-BLOB vectors and asserts KNN order.
- **T2:** `chunks(id,ino,ord,text)` + `idx_chunks_ino` + `ffts` (fts5) added to `schema.sql`
  (dimension-independent; fts5 ships in rusqlite bundled).
- **T3/T4:** `Db::ensure_vector_tables(text_dims, code_dims)` creates `vchunks`/`vchunks_code` at
  runtime width + records dims in `fs_config`; F2 per-space migration drops/rebuilds **only** the vec0
  table whose dim changed (verified: 384→768 rebuilds vchunks, preserves chunks/ffts/POSIX).

**Spike-4 findings (recorded so the next agent doesn't re-hit them):**
1. **`sqlite-vec 0.1.10-alpha.x` is BROKEN** — `sqlite-vec.c` `#include`s `sqlite-vec-diskann.c`
   which isn't shipped → `cc` fatal error. **Pinned `sqlite-vec = "0.1.9"`** (stable; builds clean
   against rusqlite 0.32 bundled). Don't bump to the alphas until that packaging bug is fixed.
2. **`semfs-core` had `#![forbid(unsafe_code)]`** — FFI registration is unavoidably `unsafe`, and
   `forbid` can't be locally overridden. Relaxed to **`#![deny(unsafe_code)]`** + a single
   `#[allow(unsafe_code)]` on `register_sqlite_vec`. Unsafe still denied everywhere else.
3. vec0 tables can't be static (runtime `float[N]` width) → `chunks`/`ffts` static in `schema.sql`,
   `vchunks`/`vchunks_code` built in `ensure_vector_tables`. `chunks` keys on `ino` (no separate
   `files` table — content already lives in `fs_data`).

**Phase 3 — DONE + real-model verified (local AND cloud).** `crates/semfs-core/src/embed/`: `Embedder`
trait (sync — fastembed is blocking), `HashEmbedder` (deterministic, FNV-1a + L2-norm; used for tests),
`LocalEmbedder` (fastembed, `ort 2.0.0-rc.12` + onnxruntime), `OpenAiEmbedder` (cloud HTTP,
OpenAI-compatible `/embeddings`, blocking via `ureq` so it fits the sync trait inside async search; key
never logged — manual Debug). (FallbackEmbedder removed per request — no fail-open wrapper.)

**Reranker (L5) — local DONE + verified.** `crates/semfs-core/src/rerank/`: `Reranker` trait (scores
aligned to input order) + `LocalReranker` (fastembed `TextRerank` cross-encoder). Reuses the
**already-downloaded `Xenova/ms-marco-MiniLM-L-6-v2`** ONNX (87MB) via fastembed's user-defined path
(same `from_dir` + default-special-tokens trick as the embedder). Test
`local_reranker_scores_relevant_above_irrelevant` passes: password doc > banana doc for a password
query.

**Reranker — WIRED into search + cloud rerankers DONE/verified.** `SqliteVecStore` now has
`with_reranker(Arc<dyn Reranker>)`; `search` reranks the post-RRF candidates by chunk text and re-sorts
(L5). Cloud rerankers in `rerank/cloud.rs` (blocking `ureq`, key never logged): `CohereReranker`
(OpenRouter `/rerank`, `cohere/rerank-v3.5`) + `RelaceReranker` (`/v2/code/rank`, index-as-filename).
**Live-verified (gated):** both score the password doc above the banana doc; and
`search_with_cloud_reranker_applies_rerank_scores` proves the wired path runs — HashEmbedder index +
OpenRouter reranker → `/notes/auth.md` first with a reranker-scale score (≫ the ~1/60 RRF score).
Local reranker (`LocalReranker`/fastembed) is wired through the same seam but **tested only via the
cloud rerankers** per request. Keys live in `bash/.env` (`OPENROUTER_API_KEY`, `RELACE_API_KEY`); gated tests skip without them.

**WHOLE PIPELINE (L1→L5) verified.** `full_pipeline_local_embed_then_cloud_rerank` runs the complete
chain over a 5-doc workspace corpus: chunk → **real local embed (fastembed all-MiniLM)** → index
(vec0+fts5) → search (KNN ∪ BM25 → RRF) → **cloud rerank (OpenRouter/Cohere)**, with a zero-lexical-
overlap query (`"how does login credential renewal work"`) → ranks `/notes/auth.md` first with a
reranker-scale score. **181 tests pass with keys live (every gated stage actually runs), clippy clean.**
The local search pipeline through the reranker stage is complete and proven; remaining = LLM client
(OpenRouter `gpt-4.1-nano`, L7 graph) + the FUSE/NFS mount wiring (write-path hook + `grep --offline`).

**Mount wiring — both Codex adversarial-review findings FIXED + E2E tested (2026-05-27).**
1. *Write path now maintains the index.* New `cache::LocalIndexer` trait (defined in `cache` to avoid a
   cache↔backend cycle; `SqliteVecStore` impls it). `SqliteFile::flush` re-indexes the file's UTF-8
   content (independent of cloud sync); `CacheFs::unlink` calls `remove`; `CacheFs::with_indexer` threads
   it into file handles. `SqliteVecStore` gained `remove()` + a shared `drop_file_chunks` helper.
   `daemon_runtime` attaches the indexer when `SEMFS_MODEL_DIR` is set (live mounts maintain the index).
2. *grep can select the local backend.* `grep.rs::resolve_index` is no longer hardcoded to `CloudIndex`:
   `--offline` opens the container's local db + `LocalEmbedder` (`SEMFS_MODEL_DIR`) + `SqliteVecStore`.
   Uses a new **read-only** `SqliteVecStore::open_existing` (does NOT call `ensure_vector_tables`, so a
   reader can never drop the writer's vec0 tables on a dim mismatch — that surfaces as a query error, not
   data loss).
   - **E2E proof:** `write_path_maintains_index_and_unlink_removes` drives the REAL VFS write path
     (`create_file → write → flush → index`, then `unlink → remove`) on a `CacheFs.with_indexer` and
     confirms search finds the file, then that unlink drops it. **182 tests pass, clippy clean.**

**Still needed for a literal binary `mount → grep --offline` loop (the one piece not yet wired):**
(a) **cache-db path alignment** — the daemon writes the org-scoped `cache_db_path(org_id, tag)`, but
`grep --offline` reads `legacy_cache_db_path(tag)`; they must resolve the same file. (b) **offline daemon
mode** — the daemon currently *requires* cloud auth + org_id to open the cache (`daemon_runtime.rs:108–127`),
so a key-less local mount isn't supported yet. Both are real follow-on features; the write-path + grep
selection are fixed and proven at the VFS level.

**Capability resolver + full mount→grep L1–L5 wiring (2026-05-27).** Replaced the idea of an `--offline`
*mode* with a data-driven resolver (`crates/semfs/src/cmd/resolve.rs`): `choose_embed`/`choose_rerank`
pick each stage from signals (local model dirs + keys), so "local embedder + cloud reranker" is just the
result of `SEMFS_EMBED_MODEL_DIR` present + `OPENROUTER_API_KEY` present — no flag. 5 pure unit tests.
- **Daemon** (`daemon_runtime`) builds the indexer via `resolve::build_embedder` when
  `local_indexing_enabled` (any real embedder configured) → live mounts maintain the local index.
- **`grep --offline`** now: resolves org id via `validate_key` → opens the SAME org-scoped
  `cache_db_path(org_id, tag)` the daemon wrote → `resolve::build_embedder` (matches the daemon, so dims
  agree) + `resolve::build_reranker` → full **L1–L5** (the embedder + reranker wired in). Read-only via
  `open_existing`.
- **Harness:** `crates/e2e/phase_local_l1_l5.sh` — mount → write markdown through the mount → `grep
  --offline` (zero-overlap query) → assert `auth.md`. **Mounting needs root**, so run it:
  `sudo -E HOME="$HOME" bash crates/e2e/phase_local_l1_l5.sh`. (This env cannot mount: no sudo/docker/
  /dev/fuse — verified. The user runs the harness.)
- **Verified here:** workspace builds, `semfs` 42 + `semfs-core` 182 tests pass, clippy clean. The mount
  itself is unrun (privileged) — the harness is the runnable proof.
- **Still future:** key-less mount (daemon still needs a key for org id / cache path) and putting org id
  in the `.semfs` marker so `grep --offline` needs zero network.

**✅ COMPLETE L1–L5 THROUGH A REAL MOUNT — verified live (2026-05-27).** With a valid `SUPERMEMORY_API_KEY`:
mounted `semfs` on `~/semfs-play/mnt` (**unprivileged — NFS-over-localhost needs NO sudo on macOS; my
earlier "needs root" claim was WRONG**), wrote two markdown files through the mount, then
`grep --offline "how does login credential renewal work"` (zero lexical overlap) → ranked **`/auth.md`
first**. Config: **cloud embeddings (OpenAI `text-embedding-3-small` via OpenRouter, 1536d) + local
SQLite vec0/fts5 index + Cohere rerank** — i.e. `SEMFS_EMBED_MODEL_DIR` unset → resolver picks
`CloudOpenRouter` embed; `OPENROUTER_API_KEY` present → Cohere rerank; `--no-sync` so nothing hits the
cloud *index* (the docs exist ONLY in the local SQLite, so the hit proves local indexing). Clean
unmount, no orphan. **This is the full mounted pipeline working end to end.**
- Gotchas confirmed: (1) mount at a NESTED path — `~/.semfs` is a directory, so mounting with parent=`$HOME`
  makes the marker write (`<parent>/.semfs`) hit EISDIR. (2) the old `bash/.env` `SUPERMEMORY_API_KEY` is
  401; pass a valid key via the shell env and don't `source bash/.env` (it would clobber it).
- Repro: `OPENROUTER_API_KEY=… SUPERMEMORY_API_KEY=<valid> ./target/debug/semfs mount <tag>
  --path ~/x/mnt --no-sync` → write `.md` into the mount → `… semfs grep --offline "<q>" ~/x/mnt/`.

**L6 + L7(Tier-0) + L4 — built + tested (2026-05-27).** 189 semfs-core + 42 semfs tests pass, clippy clean.
- **L6 Salience** (`backend/sqlite_vec.rs` + `cache/schema.sql`): `chunks.last_accessed_at`/`access_count`
  (stamped on write, bumped on search hit). Pure `salience(now, last, count)` = exp recency decay
  (14-day half-life) + log-damped access, bounded to a ~0.9–1.1 nudge. Applied as a multiplier after RRF
  /rerank, then re-sort. Tests: bounded pure-fn + tie-break (more-accessed file wins).
- **L7 Entity-graph — LLM-DRIVEN** (`backend/graph.rs`). Heuristic/regex extractor REMOVED per request.
  `extract_entities(llm, content)` calls **gpt-4.1-nano** (OpenRouter) using **structured outputs**
  (`response_format: json_schema`, `strict: true` — `LlmClient::complete_structured`); the ontology is an
  enforced `type` **enum** in the schema, so the model can't emit a malformed shape or out-of-ontology
  type. Typed entities (`Person·Organization·Project·Decision·Task·Event·Artifact·
  Concept`). Each entity → a `/memories/<slug>.md` node + a typed `file→entity` edge (`edges` table).
  `SqliteVecStore::with_graph_extractor(Arc<LlmClient>)`; `index()` extracts BEFORE locking the db
  (network), **fail-open** (extraction error ⇒ no edges, write never fails). Re-derived on write, removed
  on delete. Search boost = **co-mention** (×1.05 if a hit shares an entity with another hit), fail-soft.
  Daemon attaches the extractor when an LLM key is present. Live-verified: "…Stripe…Phoenix project…Dana
  owns it" → Stripe(Organization), Phoenix(Project), Dana(Person). Deterministic tests: slugify, fence-
  strip, edge re-derive/delete, co-mention boost. LLM extraction test gated on OPENROUTER_API_KEY.

**Adversarial-review fixes — both HIGH findings closed + verified live (2026-05-27).**
1. **`--offline` flag REMOVED; grep is config + marker driven, network-free for auth.** The daemon now
   records the cache `db_path` in the `.semfs` marker (written after the db opens). `grep::resolve_index`
   picks LOCAL (`SqliteVecStore::open_existing` + resolved embedder/reranker) when `local_indexing_enabled`
   AND the marker has an existing `db_path`; else cloud. **No `validate_key`** — verified live with
   `env -u SUPERMEMORY_API_KEY` still finding the file. Key is now Option (only the cloud fallback needs it).
2. **Rename no longer leaves the index stale.** `LocalIndexer::rename(old,new)` + `SqliteVecStore::rename`
   relabel `chunks.filepath`/`edges.from_path` (no re-embed — vec0/fts keyed by rowid) and drop any
   overwritten destination's rows; wired into `CacheFs::rename` (best-effort). Verified live:
   `mv auth.md renewal.md` → grep returns `/renewal.md`, not `/auth.md`. Tests: `rename_relabels…`.
   191 semfs-core + 42 semfs tests, clippy clean.
- **L4 Query-rewrite** (`semfs-core/src/llm.rs` + `grep --rewrite`): `LlmClient` (OpenRouter
  `gpt-4.1-nano`, blocking `ureq`, key never logged) + `rewrite_query`. **Opt-in via `--rewrite`**,
  fail-open to the original query. Live-verified: "auth renewal" → expanded multi-term query.
  Resolver gained `build_llm`. Reused by L7 Tier-1 when built. **Spike-3 verified for
real on macOS:** fastembed `5.13` + ort compile and link; `LocalEmbedder::from_dir` loads the
**already-downloaded** Xenova `all-MiniLM-L6-v2` ONNX (no re-download) via fastembed's user-defined-model
path (supplies a default `special_tokens_map.json`, which that cache omits); semantic-closeness test
passes (synonym phrase > unrelated). linux-x86_64 link still TBD on target.

**Phase 4 — search ENGINE done + offline real-model E2E (write-path hook into the live mount still
pending).** `crates/semfs-core/src/backend/sqlite_vec.rs`: `SqliteVecStore` impls `SemanticIndex`;
`index()` chunks → embeds → writes `chunks`/`ffts`/`vchunks` in one txn (re-index replaces by filepath);
`search()` = vec0 KNN ∪ fts5 BM25 → RRF (k=60), collapse to files, optional filepath-prefix filter.
SQL ported verbatim from `sqlite-vec.ts`. **Proven end-to-end OFFLINE with the real model:**
`real_model_offline_semantic_search` writes "the access token is refreshed by the middleware…", queries
"how does login credential renewal work" (**zero lexical overlap**), and gets `/notes/auth.md` first —
genuine local semantic search, the offline twin of the Phase-1 cloud E2E.

**Also verified with CLOUD embeddings (gated on `OPENROUTER_API_KEY`):**
`cloud_model_local_index_semantic_search` runs the SAME zero-overlap query but with vectors from
OpenRouter `text-embedding-3-small` (**1536d**) stored/searched in the local vec0+fts5 index → finds
`/notes/auth.md`. Proves the pipeline is **embedder-agnostic** and the schema is **dimension-agnostic**
(1536d vs the local 384d), validating the F2 design. **177 tests pass, clippy clean** (2 gated cloud
tests skip without the key). Run them: `OPENROUTER_API_KEY=… cargo test -p semfs-core cloud`.

**STILL OPEN to literally "mount semfs on a dir and grep" (next chunk):**
1. **Write-path hook** — `CacheFs` (`cache/fs.rs`, built via `CacheFs::new(db)`) must hold an optional
   `Arc<SqliteVecStore>` and call `index(ino, path, content)` on flush/commit (read_all_content + path
   resolve). Testable without a live mount (HashEmbedder).
2. **Daemon wiring** — `daemon_runtime`/`mount` build the `LocalEmbedder` (model-dir config) + store and
   pass it to `CacheFs`.
3. **grep `--offline`** — `grep.rs` `resolve_index` opens the same cache db + `LocalEmbedder` +
   `SqliteVecStore` for local search (Phase 5 slice).
4. Live NFS mount E2E — mind the known orphaned-mount bug (§9) on teardown.
The SQLite search pipeline itself is complete and proven; what remains is plumbing it through the FUSE
write path + the grep CLI.

---

## 10. Rust backend-agnosticism — Phase 1 + 1.5 landed, E2E green (2026-05-26)

**Goal of this workstream:** pull the semantic pipeline (L1–L8) into the Rust daemon behind
pluggable backends so the FUSE/NFS mount can search a **local** index offline. Tiers:
SQLite+sqlite-vec (embedded default) · Postgres+pgvector (local via pglite-oxide / cloud) ·
Supermemory (existing). Resolved per-customer at startup (see design docs).

**The plan (8 phases, subagent-driven, TDD):**
`docs/superpowers/plans/2026-05-26-rust-backend-agnosticism.md`. Design + diagrams:
`docs/backend-agnosticism-design.html`, `docs/rust-architecture.html`. **L8 (dream) is deferred**
to a future Phase 9; L6 (salience) + L7 (dynamic, context-weighted graph) are Phases 7–8.

**Done this session (commits on `feat/backend-agnostic-store`):**
- **Phase 1 — trait seam (zero behavior change):** new `semfs_core::backend::{SemanticIndex,
  SearchHit, CloudIndex}`; `cmd/grep.rs` now depends on `Arc<dyn SemanticIndex>` via a
  `resolve_index` factory (returns `CloudIndex` today; local/offline plugs in at Phase 5).
  Commits `e996563`→`40d9b80`. 193 tests green, final review "ready to integrate."
- **Phase 1.5 — grep tag-from-path (`5abb9f3`):** `grep "<q>" /path/to/mount/` now resolves the
  container tag from the path argument's `.semfs` marker, so it works from **outside** the mount
  (previously only CWD-marker / `--tag`). Unit-tested `resolve_tag_url` (precedence: --tag > CWD >
  path marker), 15 grep tests green.
- **E2E harness fixed + green (`e6fe768`):** `crates/e2e/phase1_grep.sh`. Full round trip PASSES
  with a credited key: mount → write → push (200) → server index → `grep` (run from inside the
  mount) finds the file for a **zero-lexical-overlap** query (genuine semantic match).

**402 push — root-caused + resolved.** The old `bash/.env` key's account is `plan: free` and
**out of SuperRAG credits** → `POST /v3/documents` returns 402 → daemon `push: poisoned status=402`,
files never index. Fix: use a credited key (export `SUPERMEMORY_API_KEY`; the daemon picks it up via
`--key`, or `semfs login`). Earlier docs guessed 401 — it is **402**.

**Gotchas recorded** (RCA `rcas/2026-05-26-semfs-e2e-402-and-grep-tag-resolution.md`):
1. `semfs grep`'s tag comes from CWD `.semfs` / `--tag`, not a path arg → fixed in Phase 1.5.
2. `status=done` **lags** `/v4/search` availability — gate search tests on the search, not status.
3. `cargo test` does **not** rebuild `target/debug/semfs`; run `cargo build` before any E2E that
   shells out to the binary (a stale-binary test nearly produced a false negative).

**Next:** run the Phase-3/4/8 **spikes** (pin embedding crate candle-vs-ONNX, the
`sqlite-vec`+`rusqlite` extension-load API, the L7 extraction LLM + cost), then expand Phases 2–8
into full bite-sized plans. Phase 1 is a complete, behavior-preserving unit ready to merge to a
`develop` branch (none exists yet — only `main` + this feature branch).

---

## 9. Workspace-Bench `semfs-codex` harness fix (2026-05-25, later session)

**Context:** On the benchmark host (`ubuntu@REDACTED_BENCH_HOST`, repo at `/srv/semfs-benchmark/`), the
`semfs-codex` smoke case (`SEMFSCodex--GPT-5.4`, case 100) graded `failed` while plain `codex` passed.
Investigated live, root-caused, fixed, and verified e2e.

### Root cause (one cause; an earlier two-layer framing was wrong)
The benchmark grader (`Workspace-Bench/evaluation/src/agent_runner.py`) decides pass/fail from
`output_paths` (`final_status = "passed" if output_paths else "failed"`), built by
`_collect_output_paths`, which **gates every candidate on `os.path.isfile()` at grading time**.
`semfscodex.run()` mounts semfs over the workdir, the agent writes `model_output/…` **into the mount**,
then `semfscodex` **unmounts in its `finally` before returning** — so by the time the grader runs, the
deliverables have vanished with the mount → `output_paths=[]` → `failed`.

- **"Layer 1" (empty `returnedPaths`) was a red herring.** `agents/codex.py:731` hardcodes
  `"paths": []`, but the grader re-parses the path list out of `trace.lastText` independently. **Plain
  codex passes with `returnedPaths=[]`** — proof the empty field doesn't drive the grade. Not fixed
  (cosmetic; would touch the vendored tree).
- **The real cause is deliverable persistence (file existence at grade time).**

### Fix (all in `benchmarks/workspace_bench/semfscodex.py` — our adapter; no vendored/Rust changes)
1. `_stage_outputs_from_mount` — while the mount is live, copy deliverables OUT to
   `sandbox_dir/semfs_staged/` (union of paths parsed from `trace.lastText` ∪ `result["paths"]` ∪ the
   implied output subtree e.g. `model_output/` ∪ expected filenames; walks only those subtrees, never
   the whole ~3,800-file memory mount; bounded by `_MAX_STAGED_FILES`/`_MAX_STAGED_BYTES`).
2. `_force_clear_mount` + `_path_is_dead_or_mounted` — **required wrinkle the first fix attempt
   exposed:** `semfs unmount` leaves an **orphaned kernel FUSE entry** (daemon gone, mount still
   registered → ENOTCONN; same recurring bug as the orphaned-mount RCA, and `SEMFS_NO_SYNC=1` does NOT
   prevent it). Restore wrote into the dead mount and silently skipped → still failed. Now: between
   unmount and restore, detect the orphan and `fusermount3 -u` (fallbacks `fusermount -u` / `umount`).
3. `_restore_outputs_to_workdir` — copy staged files back into the now-bare workdir (rejects path
   escapes); then set `result["paths"]` to the restored files (so `returnedPaths` also reports right).
4. Fail-open throughout (`stageError`/`restoreError`/`forcedUnmount` recorded; never crashes the run).

### Tests + verification
- `benchmarks/workspace_bench/test_semfscodex_staging.py` — 5 unit tests
  (extract/escape/roundtrip/no-op/force-clear), pass local + host.
- **e2e on host (no stubs):** final run `layer2-fix-v2-20260525T173327Z` → **`passed=1, failed=0`**;
  `returned_paths_exist` passed (count=1); `returnedPaths=['model_output/onsite_hosting_execution_manual.doc']`;
  all 4 retrieval methods fired; `outputManifest` = the .doc (15,278 B); **no orphaned mount / no daemons
  left**. (First attempt staged OK but failed on ENOTCONN restore — that's what surfaced the force-clear
  need.)

### Host cleanup performed
- Killed/unmounted two stale debug daemons (`semfs-debug-fixed-1779726852`, `semfs-debug-houqin-1779726429`)
  that held undrained queues (3514 / 3405) on the workdir; `semfs list` → `no active mounts`.

### Still open (NOT this bug — product issues)
1. **semfs unmount orphaned-mount bug (Rust):** unmount leaves a registered FUSE entry → ENOTCONN. The
   adapter now works *around* it (force-clear); the real fix is a clean kernel teardown in the Rust
   crate, which would make the staging shim unnecessary. **Highest-value product follow-up.**
2. **`401` org-scoped push:** push queue never drains (free-plan/key-scope). Run benches with
   `SEMFS_NO_SYNC=1` / `--no-sync`, or fix the API-key scope. See `rcas/2026-05-25-semfs-bench-orphaned-fuse-mount.md`.
3. **Leaked + under-scoped API key:** `SUPERMEMORY_API_KEY` (`sm_pZug…`) is passed as a CLI arg to
   `semfs daemon-inner`, so it shows in `ps` on the shared host. **Rotate it** and pass via env/file.
4. **No real perf measurement yet:** smoke = 1 case; both arms pass. A credible semfs-vs-baseline %
   (accuracy/tokens/latency) needs the full dataset with repeats. Today only proved the harness is correct.

### RCAs
- `rcas/2026-05-25-semfs-bench-orphaned-fuse-mount.md` — infra (`status=error`, mount failed over stale entry).
- `rcas/2026-05-25-semfs-bench-empty-returned-paths.md` — this `failed`-grade mode (deliverable lost on
  unmount) + the force-clear wrinkle + verification record.

---

## 0. Orientation — where things live

| Path | What |
|------|------|
| `bash/` | TypeScript package (`@supermemory/bash`). All recent work is here. |
| `bash/src/backends/sqlite-vec.ts` | Local unified backend: SQLite + sqlite-vec. The 8-layer pipeline in one file. **The spec to port.** |
| `bash/src/create-bash.ts` | Factory: backend/embedder/reranker resolution, path derivation, ownership guard. |
| `bash/src/memory-volume.ts` | Store-agnostic orchestrator (L4–L8 wiring: rewrite, rerank, salience, graph, dream). |
| `bash/src/backends/embedder-*.ts` | `embedder.ts` (Hash), `-openai`, `-relace`, `-transformers`, `-fallback`. |
| `crates/semfs/` | **Rust CLI + FUSE daemon.** `src/cmd/grep.rs` = search (cloud-only today). |
| `crates/semfs-core/` | Rust core: `cache/` (SQLite FS cache), `mount/`, `sync/`, `api/`, `vfs/`. |
| `docs/requirements-analysis.html` | Live requirements matrix (Rust/TS/Python × FR/NFR). |
| `docs/codex-review-*.html` | The adversarial review + ELI5→expert explainers. |
| `docs/sqlite-multi-agent.html` | Multi-agent-on-one-machine concurrency behavior (local only, uncommitted). |
| `known_issues.md` | KI-1..4 (fixed) + F1..F5 (this review, fixed). |

**Run the TS suite:** `cd bash && ./node_modules/.bin/vitest run`. Use `./node_modules/.bin/tsc`
and `./node_modules/.bin/biome` (NOT `npx` — `npx tsc` tries a global install in this env).
Gated live tests read `bash/.env`: `SUPERMEMORY_API_KEY`, `RELACE_API_KEY`, `OPENROUTER_API_KEY`,
`RUN_REAL_EMBEDDER`, `RUN_REAL_RERANKER`. **There is no `OPENAI_API_KEY`.**

---

## 1. What was done this session (TypeScript)

| Commit | Title |
|--------|-------|
| `1f9a9a1` | fix(fr2): persistent local SQLite default + fast startup |
| `80df02e` | feat(fr3): local-first semantic defaults + text/code embedder split |
| `2bc11a9` | docs(nfr1): harden cache-friendly-prefix guarantee; TS/Py NFR1 → Full |
| `b72dd8d` | fix(fr2): collision-resistant local DB paths + container ownership guard (Finding 1) |
| `526b85e` | fix(fr3): resolve findings 2-4 (migration, fail-open, code routing) + doc |
| `f38f8ad` | test(fr3): real-model e2e (Transformers + Relace, no stubs) |
| `1c01e97` | docs: add AGENTS.md, CLAUDE.md, users.md |

### FR3 model-selection contract (the design the user cares about)
```
text embedder:  opts.textEmbedder → OpenAIEmbedder (OPENAI_API_KEY)
                                   → FallbackEmbedder(Transformers all-MiniLM 384d → HashEmbedder 384d)
code embedder:  opts.codeEmbedder → RelaceEmbedder (RELACE_API_KEY) → undefined (text handles code)
reranker:       opts.reranker     → RelaceReranker (RELACE_API_KEY) → LocalReranker   [only when no explicit backend]
```
Three invariants that the next agent must NOT regress when porting:
- **Routing is by intent** — `dualMode = codeEmbedder && codeEmbedder !== embedder`. A distinct code
  model gets its own `vchunks_code` table even at equal vector width (Finding 4).
- **Migration is per-vector-space** — changing the text dim rebuilds only `vchunks`; adding a code
  embedder drops nothing; `chunks`/`ffts` (BM25) are dimension-independent and never wiped, so files
  stay keyword-searchable while vectors lazily re-embed on next write (Finding 2).
- **Fail-open** — the default Transformers embedder degrades to HashEmbedder (one warning) if the model
  can't load; `search()` returns base similarity order if the reranker can't load (Finding 3).

---

## 2. Known issues — status

| ID | Issue | Status |
|----|-------|--------|
| KI-1 | PgVector edges had no container column (tenant bleed) | ✅ Fixed |
| KI-2 | Dream synthesis overwrote itself same-day | ✅ Fixed (ms-precision paths) |
| KI-3 | SqliteVecStore `:memory:` default (restart data loss) | ✅ Fixed (dbPath + auto-local) |
| KI-4 | HashEmbedder default (no semantic quality) | ✅ Fixed (real default + fallback) |
| F1 | `deriveLocalDbPath` collision across tags | ✅ Fixed (hash filename + ownership guard) |
| F2 | Adding a code embedder wiped the text index | ✅ Fixed (per-space migration) |
| F3 | Auto-local default fail-*closed* on model load | ✅ Fixed (FallbackEmbedder + fail-open rerank) |
| F4 | Same-dim explicit codeEmbedder ignored | ✅ Fixed (route by intent) |
| F5 | `known_issues.md` self-contradicted | ✅ Fixed (reconciled) |

---

## 3. Actual testing that happened (honest)

**Full suite:** `530 passed | 4 skipped`. Two transient failures appeared in *one* full run
(live-service / PGlite-WASM **timeouts**, "long-running test" warning) and were green on two
subsequent full runs — flaky infra, not logic. The 4 skipped are gated tests missing their env flag.

### Tested with REAL components (no stubs)
- **`tests/e2e-real-models.test.ts`** (gated `RUN_REAL_EMBEDDER`, ✅ 2 passed):
  1. **Default `createBash({})` path** — stored `"user sign-in and credential verification"`, queried
     `"authentication and login flow"` (zero shared words), `/auth.md` found. HashEmbedder *can't*
     bridge that → proves the **real all-MiniLM model loaded** (not the fallback) + ran the real
     RelaceReranker.
  2. **Real dual-embedder** — real Transformers(384d) text + **real Relace API**(2560d) code: `.ts` →
     `vchunks_code`, `.md` → `vchunks`, single query RRF-merged both. `code_embed_dims=2560` is a live
     Relace call that throws without a valid key.
- `tests/backend-sqlite-vec-semantic.test.ts` (real TransformersEmbedder, zero-lexical-overlap ranking).
- `tests/relace-embedder.test.ts` (live Relace), `tests/integration.test.ts` (live Supermemory, 22),
  `tests/llm-providers.test.ts` / `openrouter-providers.test.ts` (live OpenRouter).

### Tested with fakes (fast/deterministic)
- Findings 2/3/4 unit tests use `HashEmbedder` at two widths + **stub throwing** embedders/rerankers.
  The fail-open *mechanism* is proven; a *real offline failure* of Transformers is **not** simulated e2e.

### NOT covered (gaps for the next agent)
- **Real `OPENAI_API_KEY` path** — no key in env; only the no-key→Transformers default ran live.
- **OpenRouter embeddings**: live-probed via curl → `openai/text-embedding-3-small` returns **HTTP 200,
  1536-d** with the project key. Works, but **NOT auto-wired** in `resolveEmbedders` (only
  `OPENAI_API_KEY` is checked). Opt in: `new OpenAIEmbedder({ apiKey: OPENROUTER_API_KEY,
  baseURL: "https://openrouter.ai/api/v1", model: "openai/text-embedding-3-small" })`. (Decision in §5.)
- **FallbackEmbedder mixed-vector degraded state** — documented, not stress-tested.
- **The entire Rust/FUSE path has no local semantic test** — search there is cloud-only (§6).

---

## 4. Layer × backend status (current, corrected)

8 layers: 1 Chunk · 2 Embed · 3 Index · 4 Query-rewrite · 5 Rerank · 6 Salience · 7 Entity-graph · 8 Dream.

| Backend | L1 | L2 | L3 | L4 | L5 | L6 | L7 | L8 | Notes |
|---------|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|-------|
| **Supermemory** | ☁️ | ☁️ | ☁️ HNSW | ☁️ | ☁️ | ☁️ | ☁️ | ☁️ | All server-side; cloud-only. |
| **SqliteVec** | ✅ | ✅ | ⚠️ brute-force O(n) | opt | opt | ✅ | ✅ | ✅ | Local, offline-capable. **Recommended local backend.** ~50K-chunk practical KNN limit (sqlite-vec has no HNSW). |
| **PgVector** | ✅ | ✅ | ✅ HNSW opt | opt | opt | ✅ | ✅ | ✅ | Multi-tenant (`container` col); for concurrent multi-writer. |
| **Rust FUSE** | — | — | — | — | — | — | — | — | **No local layers.** `semfs grep` → Supermemory API. §6. |

Embedders: `HashEmbedder`(dev, configurable dims) · `TransformersEmbedder`(384, local ONNX) ·
`OpenAIEmbedder`(1536, API, baseURL-overridable) · `RelaceEmbedder`(2560, code, API) ·
`FallbackEmbedder`(wraps any two equal-dim embedders). Rerankers: `LocalReranker`(ONNX) ·
`RelaceReranker`(API) · `OpenRouterReranker`(Cohere schema) · `LLMReranker`.

---

## 5. Open TypeScript decisions (small, optional)
1. **Auto-wire OpenRouter embeddings** into `resolveEmbedders` (precedence: explicit → `OPENAI_API_KEY`
   → `OPENROUTER_API_KEY` via OpenRouter baseURL → local Transformers). Live-verified working; treat as
   fail-hard like OpenAI. ~10 lines + gated test.
2. **FallbackEmbedder all-or-nothing mode** — probe once at startup, pick one embedder for the session,
   to avoid the mixed-vector degraded state.
3. **Cross-process dream-gate race** — `last_dream_at` read-then-write isn't atomic across processes;
   two agents sharing one DB can double-dream. Low impact; wrap in a transaction if it matters.

---

## 6. The FRONTIER — Rust/FUSE daemon (`crates/`)

**Current state (verified by reading the code):**
- `crates/semfs-core/src/cache/schema.sql` has only POSIX tables (`fs_inode`, `fs_dentry`, `fs_data`,
  `fs_symlink`, `fs_config`, `fs_remote`, `push_queue`, `sync_meta`). **No `vec0`, `fts5`, `edges`, or
  embeddings.**
- `crates/semfs/src/cmd/grep.rs` resolves a container tag + `SUPERMEMORY_API_URL` and **delegates search
  to the cloud**. No local embedder, no local ranking.
- Result (`docs/requirements-analysis.html`, Rust ≈ 54%): **FR3 Partial** (search has no local L6/L7/L8),
  **FR5 Partial** (POSIX ops work offline via the SQLite cache, but `semfs grep` needs network).

**The path is half-built:** the Rust cache is *already SQLite*. The job is to extend that one DB with a
vector + keyword + graph index and port the (now-hardened) TS `SqliteVecStore` pipeline into Rust,
giving the FUSE mount fully local, offline semantic search.

**Build order (mirror the TS reference exactly):**
1. **Schema** — add to `cache/schema.sql`: a `vec0` virtual table (sqlite-vec loadable extension), an
   `fts5` table, a `chunks` table, an `edges` table, embed-dim keys in a meta table. Mirror
   `bash/src/backends/sqlite-vec.ts`.
2. **Embedder trait** — Rust `Embedder` trait; local model (candle or ONNX/`ort` for all-MiniLM) + HTTP
   embedders (OpenAI/OpenRouter/Relace). Port the **FallbackEmbedder** idea.
3. **Write path** — on file write/sync (`crates/semfs-core/src/sync/`), chunk + embed + index into the
   cache (the FUSE write already lands in `fs_data`; hook the indexer there).
4. **Search path** — rewrite `crates/semfs/src/cmd/grep.rs` to query the local hybrid index (vec + BM25
   RRF) and **fall back to it when offline / `--offline`**, keeping cloud as an option.
5. **Port the hardened invariants** (do NOT re-introduce fixed bugs): per-space dimension migration (F2),
   route-by-intent code embedder (F4), collision-resistant cache identity (F1), fail-open
   embedder/reranker (F3), L6 salience / L7 edges / L8 dream with ms-precision synthesis paths (KI-2).

---

## 7. Next steps per persona — for the FUSE codebase

Personas (`users.md`): **Type 1** = agentic-startup builders (sandboxes; latency/cold-start is
load-bearing). **Type 2** = local CLI devs (offline; instruction drift). **Type 3** = cloud/unattended
operators (throughput + correctness; multi-day; latency irrelevant).

### Type 1 — Agentic-startup builders (latency is load-bearing)
- **Goal:** fast mount + warm index; keep per-search latency off the cloud in the synchronous user loop.
- Ensure `semfs mount` + cache (incl. new vec index) warms in **<200ms** on a populated DB — port the FR2
  lesson (warm path index only, lazy content). Treat daemon cold-start like Freestyle treats microVM boot.
- Keep the system-prompt prefix stable (NFR1): FUSE serves files as tools, so memory is read on demand —
  don't let any daemon feature inject changing content into the prefix.
- Local embedder is *optional* for them (may stay on Supermemory), but a local vec index removes the
  per-`grep` network round-trip from the hot path.

### Type 2 — Local CLI devs (offline + no drift)
- **Goal:** `semfs grep` works **fully offline** with real semantic quality (closes FR5).
- Implement §6.1–§6.4 with the **local all-MiniLM embedder + FallbackEmbedder** (graceful first-use
  degradation), plus the **text/code split** (Relace optional via key).
- **Already good for them:** FUSE exposes `CLAUDE.md`/`AGENTS.md` as **real host files** (FR4 Full for
  Rust) — git diff, editors, standard tooling work. Preserve that.
- Drift is their top pain — port L8 dream + L6 salience to surface the right files; keep synthesis
  append-only (KI-2).

### Type 3 — Cloud / unattended operators (throughput + correctness, multi-day)
- **Goal:** durable cross-restart state, correctness over 13-day runs, cost gates.
- **Already good:** the SQLite cache persists across daemon restarts (FR1). Verify the new
  vec/edges/dream state survives an OOM-kill restart with no re-indexing (the FR2 guarantee, in Rust).
- Port **L8 dream** with ms-precision synthesis paths (KI-2) so multi-run-per-day synthesis accumulates
  instead of overwriting — drift / lost audit trail is *their* canonical failure.
- **Multi-writer:** if several unattended agents share one mount/cache, the **single-writer ceiling
  applies** (`docs/sqlite-multi-agent.html`): WAL = many readers, one writer, `SQLITE_BUSY` under load.
  For true concurrent multi-writer, route them to **PgVector/Postgres** (multi-tenant `container` column
  already exists) rather than a shared SQLite file.
- Reward-hacking / gamed-completion (`assert True`) is a harness concern, not the filesystem's — note it,
  don't solve it here.

---

## 8. Quick start for the next agent
```bash
# TS package (reference implementation — read before porting to Rust)
cd bash
./node_modules/.bin/tsc --noEmit                 # typecheck
./node_modules/.bin/vitest run                   # full suite (530 pass; live tests use .env)
RUN_REAL_EMBEDDER=1 ./node_modules/.bin/vitest run tests/e2e-real-models.test.ts   # real models

# Rust FUSE daemon (the frontier)
cd crates && cargo build && cargo test
#   search seam:  crates/semfs/src/cmd/grep.rs            (cloud-only today)
#   cache schema: crates/semfs-core/src/cache/schema.sql  (no vec/fts/edges yet)
```
**Mental model:** `bash/src/backends/sqlite-vec.ts` is the spec. Porting it into the Rust cache turns
the FUSE mount from "POSIX-offline + search-online" into a fully local, offline semantic filesystem —
closing the Rust FR3/FR5 gaps. That is the single highest-leverage next move.

---

## 9. Changing the embedding model on an existing index (operational note)

The local index records the embedder **identity** (model + dims; for local models a
content fingerprint of the ONNX/tokenizer bytes — `Embedder::identity()`). Both backends
**fail closed** when the current embedder doesn't match the index's recorded identity, so a
model swap can **never silently corrupt search**:

- **SQLite** (`grep` reader): `SqliteVecStore::is_searchable()` returns false on an
  identity mismatch (or an empty index) → `grep` falls back to cloud search.
- **Postgres**: `PgVectorStore::connect()` errors on an identity mismatch (parity with the
  existing dimension-drift check).

**To switch embedding models, start with a fresh index** (delete the cache DB / remount):
a fresh index has no identity stamp, so it adopts the new model cleanly. The old vectors are
never mutated or deleted in place — reverting to the prior model makes the existing index
searchable again.

**Deferred (by decision):** there is intentionally **no automatic backfill** that re-embeds
already-indexed files when the model changes. That would be a daemon feature (walk + re-embed
+ restamp, with blocking-vs-background / progress / trigger semantics) and is out of scope for
the backend-agnostic-store work. Until it exists, local search under a *changed* model on a
*reused* index is unavailable (degrades to cloud) until a fresh reindex — by design, safe.
