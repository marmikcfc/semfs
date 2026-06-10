# RCA: gemma-q4 seed silently built an e5 index (stale incremental build)

**Date:** 2026-06-08 · **Severity:** high (wrong embedder silently used; whole seed invalid)
**Discovered by:** a user-requested "simple test to confirm embeddings are correct" — semfs grep
returned no results, and the index's stored embedder identity was e5, not gemma-q4.

## Symptom
A `SEMFS_EMBED_MODEL=gemma-q4` seed produced an index stamped with
`text_embed_model = fastembed:fe5.13.4:intfloat/multilingual-e5-small:384` (384-d e5),
NOT `byo:gemma-q4-onnx:768`. Search returned "no results" for every query (the 384↔768 dim/space
mismatch — the index was e5, any gemma-q4 query vector couldn't match).

## Investigation (data flow, before hypothesis)
1. Daemon env DID contain `SEMFS_EMBED_MODEL=gemma-q4` + `SEMFS_EMBED_ONNX_DIR` (`/proc/<pid>/environ`).
   ⇒ env propagation was fine.
2. The DEPLOYED source `crates/semfs/src/cmd/resolve.rs` DID contain the gemma-q4 route
   (`EmbedChoice::Local if embed_model==Some("gemma-q4") => from_onnx_dir`). ⇒ source was correct.
3. `daemon_runtime.rs:48` calls `resolve::build_embedder` — the patched function. ⇒ right call site.
4. **`strings /home/ubuntu/.local/bin/semfs | grep -c gemma-q4-onnx` = 0** — the BINARY did NOT
   contain the route's string literals (`gemma-q4-onnx`, `SEMFS_EMBED_ONNX_DIR`). The deployed
   `target/release/semfs` ALSO lacked them. ⇒ **the change was never compiled into the binary.**
5. The `embed_probe` sanity gate PASSED (cosine 0.81/0.24) because it's an example in `semfs-core`
   that calls `LocalEmbedder::from_onnx_dir` DIRECTLY — `semfs-core` WAS recompiled (the new example
   forced it), so the loader was fine. The bug was only in the `semfs` BINARY's `resolve.rs`.

## Root cause
**`rsync -az` preserves source mtimes; cargo's incremental build keyed off mtime/fingerprint and
SKIPPED recompiling `resolve.rs` into the `semfs` binary.** So `cargo build -p semfs` reported success
but the binary still ran the OLD `resolve.rs` (no gemma-q4 arm). With the route absent,
`SEMFS_EMBED_MODEL=gemma-q4` fell through to `text_embed_model("gemma-q4")` which **silently defaults
unknown names to `MultilingualE5Small`** (the `_ => TEXT_EMBED_MODEL` arm) → e5-small 384-d.

Two things made it SILENT:
- `text_embed_model()` returns the e5 default for ANY unknown model name — no warning/error.
- The sanity gate tested the loader directly (recompiled `semfs-core`), not the routing through the
  (stale) `semfs` binary — false confidence.

## Fix (applied + verified)
1. `touch crates/semfs/src/cmd/resolve.rs crates/semfs-core/src/embed/local.rs` (bump mtimes past the
   fingerprint), `cargo build --release -p semfs`, reinstall.
2. Verify: `strings <binary> | grep -c gemma-q4-onnx` = 1 ✅. Restart seed → stored identity now
   `byo:gemma-q4-onnx:768`, dims 768 ✅. Direct KNN on the index returns self + semantic neighbors ✅.

## Prevention
- **Deploy procedure:** after `rsync`, ALWAYS force-recompile changed sources — `touch` them, or
  `rsync --no-times`/`--checksum`, or `cargo clean -p <crate>` for the changed crate. (Added to the
  hardened `benchmarks/workspace_bench/seed_complete.sh` workflow / deploy notes.)
- **Verify every deploy by content, not exit code:** `strings <binary> | grep <a-new-literal>` (or a
  `--version`/build-hash that changes) to prove the change is actually in the binary.
- **Fail loud on unknown embedder:** `text_embed_model()` should WARN (or the resolver should error)
  when `SEMFS_EMBED_MODEL` is an unrecognized name instead of silently using e5 — a typo or a missing
  route then surfaces immediately. (Follow-up code change.)
- **Sanity-gate through the real path:** validate the embedder via `resolve::build_embedder(env)` (the
  path the seed uses), not just `LocalEmbedder::from_onnx_dir` directly, so a routing/build bug is caught.
- **Stamp + check identity before a long seed:** read `fs_config.text_embed_model` after ~1 file and
  assert it matches the intended embedder BEFORE letting a multi-hundred-file warm run.

## Impact / note
- One full gemma-q4 seed attempt was wasted (it was e5) — caught at ~300 files, not after completion,
  thanks to the explicit test. ⇒ always run an end-to-end retrieval/identity check on a fresh seed.
- Earlier `chanpin-gemma` (registry `gemma`, 768-d) was genuine — its route existed in the old binary
  (search logs showed `qvec_len=768`). Only the NEW `gemma-q4` route was missing. So this did not
  retroactively invalidate the fp32-gemma work, only the q4 attempt.

## Refs
`tickets/gemma-q4-embedder/issue.md`, `crates/semfs/src/cmd/resolve.rs` (route + `text_embed_model`
default), `crates/semfs-core/src/embed/local.rs` (`from_onnx_dir`),
`crates/semfs-core/examples/embed_probe.rs`, `benchmarks/workspace_bench/seed_complete.sh`.
