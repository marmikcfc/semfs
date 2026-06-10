# RCA: mount reports "ready" before the KG is materialized → codex reads an empty KG → fabricates

**Date:** 2026-06-08 · **Host:** ubuntu@13.201.35.159
**Severity:** high (invalidated a graph-fs benchmark run; any agent that reads `kg/` the instant
the mount is live can get an empty file). **Surfaced by:** the first codex graph-fs E2E on the
q4 seed scoring **0/15** with a fabricated answer.

## Symptom
A codex graph-fs run on case 289 (`SEMFS_GRAPH_FS=on`, `SEMFS_KG=on`, q4 seed) finished in 3 tool
calls / 52K tokens and **fabricated** product data → judge **0/15**. The trace:
```
1. cat kg/KNOWLEDGE_GRAPH.md                         → out=0   (EMPTY)
2. cat …/best_selling_product_core_data_list.txt     → out=0   (an empty placeholder stub)
3. cat > model_output/… <<EOF  1. 2015春季… | 成交金额：37200元 …   (MADE UP)
```
codex never ran `semfs grep`, never touched `/by-topic` (byTopic=0), never saw the 403.

## Investigation (data flow, before hypothesis)
Mounted the SAME seed by hand and waited: `kg/KNOWLEDGE_GRAPH.md` = **61,873 bytes**, `/by-topic/`
overlay live with real community dirs. So the KG + graph-fs **work** — yet codex read 0 bytes.
The only variable is *timing*. `daemon_runtime.rs` order was:
```
mount_fs()                  ← FUSE goes live; mountpoint browsable, ls/cat work
refresh_knowledge_graph()   ← materializes kg/KNOWLEDGE_GRAPH.md (load 9,298 entities,
                              Louvain, build digest) — takes SECONDS
bind IPC socket
```
The harness starts codex as soon as the **mountpoint is responsive** (after `mount_fs`), but
`kg/KNOWLEDGE_GRAPH.md` isn't written until `refresh_knowledge_graph` finishes a few steps later.

## Root cause
**"Mount ready" and "KG materialized" were sequential, not synchronized.** The daemon announced the
filesystem live (FUSE up) *before* it wrote the KG digest. For a large KG (q4: 9,298 entities over
696 files), materialization is slow enough that codex's first `cat kg/KNOWLEDGE_GRAPH.md` lands in
the window before the file exists → 0 bytes → with no KG and no grep it falls back to `cat`-ing a
plausibly-named **empty placeholder stub** (one of the 747 empties) and **fabricates**.

Why it didn't bite the earlier e5 runs (10/15): their **smaller** KG materialized fast enough to win
the race. Ironically, **better coverage made the KG bigger → slower → it started losing the race.**
This is a classic "readiness ≠ initialized": the system reports up when it can *respond*, not when
it's *populated*.

## Fix (applied)
`crates/semfs/src/cmd/daemon_runtime.rs`: **moved `refresh_knowledge_graph()` to BEFORE `mount_fs()`.**
The KG (and the injected `AGENTS.md`/`CLAUDE.md` hints) are now materialized before the filesystem
is browsable, so "mount ready" implies "KG ready" — the mount blocks on materialization (a few extra
seconds) instead of racing the consumer. Safe because `refresh_knowledge_graph` only touches the
cache DB + derived siblings (it does NOT depend on the FUSE mount being live).

## Verification (re-run, fixed binary)
| Signal | broken | fixed |
|--------|--------|-------|
| `cat kg/KNOWLEDGE_GRAPH.md` | 0 B | **1,727 B** ✓ |
| `/by-topic` used | 0 | **21×** ✓ |
| `semfs grep` | no | yes (3×) ✓ |
| saw 403 | no | yes ✓ |
| tool calls | 3 (fabricated) | 18 (real investigation) |
| judge | **0/15** | **6/15** |

The run is now valid: codex engages the KG + graph-fs and stops fabricating.

## Follow-on finding (separate problem, see EXPERIMENTS.md §8)
The fixed run cost **493K tokens** because codex hit the **format trap** 6× — it used `/by-topic` to
*find* spreadsheets, then parsed them with `openpyxl`/`pandas`/`libreoffice`. Graph-fs fixes
*engagement*, not the *format trap*. The token lever is steering the agent to the `.xlsx.extracted.md`
summaries instead of letting it reach for binary parsers. Single high-variance run; not a verdict.

## Prevention
- Readiness must mean *initialized*, not *responsive* — any artifact an agent reads on mount
  (KG, hints) must exist before "ready" fires. (Done for the KG; audit other on-mount artifacts.)
- Benchmark harnesses that depend on a materialized artifact should additionally gate on its presence
  (e.g. poll `kg/KNOWLEDGE_GRAPH.md` non-empty) as defense-in-depth.

## Refs
`crates/semfs/src/cmd/daemon_runtime.rs` (refresh moved before `mount_fs`),
`crates/semfs-core/src/cache/fs.rs::refresh_knowledge_graph` (mount-independent),
EXPERIMENTS.md §8 (codex graph-fs E2E + format trap).
