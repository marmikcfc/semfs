# Graph-as-filesystem: make every traversal call a knowledge-graph walk

**Status:** BUILT + MEASURED (2026-06-08) · 2026-06-07 · **Parent:** `ls-kg-semantic-readdir/`

> **IMPLEMENTED** (Option B / overlay `/by-topic`, `SEMFS_GRAPH_FS` flag): persisted Louvain
> projection (`graph_community`/`graph_god_node`), bounded read model, synthetic inodes
> (`ino≥1<<48`), readdir/lookup/getattr branching, symlink cross-edges, kind-tiered god-node
> labels. 314 lib tests green; real-FUSE verified (os.walk bounded 31 dirs/323 files, no explosion).
> **RESULT (case 289, e5 seed):** HIGH VARIANCE — gfs1 87K/5calls/10-15/HONEST ✅, gfs2 490K,
> gfs3 686K. Necessary-not-sufficient: kills os.walk-blowup but the tail is the post-grep FORMAT
> TRAP (→ H1 trust marker, which compressed it 686K→207K but didn't reach <100K). The binding
> lever is TURN COUNT. See root `EXPERIMENTS.md` §5b/5c/5d, `TOKEN_REDUCTION_HYPOTHESES.md`,
> `CURRENT_STATE.md`, and `rcas/2026-06-08-partial-seed-indexing.md` (seed only ~half-indexed).
**Prereq:** KG digest (`KNOWLEDGE_GRAPH.md`) + communities/god-nodes already built
(`graph_file.rs`, `digest.rs`, `community.rs`). This ticket reuses that graph; it does
not recompute it.

---

## 1. Motivation (why)

Measured on Workspace-Bench case 289 (clean, no protocol coaching), codex's token
blow-up and wrong answers come from **filesystem crawling**, not from grep:

| run | setup | tool calls | of which crawl (`os.walk`/`ls -R`/`find`) | tokens |
|---|---|---|---|---|
| kg2 | protocol forced "1 grep, stop" | 3 | 0 | 55K |
| kg4 | clean, grep-shadow on | 11 | **7** | 203K |

In kg4 the agent ran **1 `semfs grep`, then 8 crawl commands** (`os.walk`, `ls -R`,
`find ×4`, `grep` on filenames) hunting for a concrete "source file," found the decoy,
and copied it. We **confirmed via canary** that the injected `AGENTS.md` ("don't crawl;
use semfs grep") *is* read by codex on every turn — and is ignored. So a passive
instruction will not stop the crawl. We must change **what the crawl returns.**

**Idea:** the agent will crawl regardless — so make crawling *itself* a semantic act.
If the directory tree the mount exposes *is* the knowledge graph (top-level entries =
god-nodes / topic communities, descending = the files about that concept), then
`ls` / `find` / `os.walk` become a guided graph walk instead of a blind tree scan. The
agent's own habit becomes the feature.

---

## 2. Key realization: we rewrite FUSE ops, NOT shell commands

`ls`, `find`, `os.walk`, `ls -R`, `tree` are external programs we don't control. They
**all bottom out in the same three FUSE operations**:

```
  ls / find / os.walk / ls -R
        │  (syscalls)
        ▼
   readdir(ino)      ← list a directory's children
   lookup(parent,nm) ← resolve a name → inode
   getattr(ino)      ← stat: is it a dir? size? mode?
```

So there is **no per-command rewrite**. We change what `CacheFs` returns for those ops
and *every* traversal tool becomes a graph walk for free. The functions:

| op | location (today) | change |
|---|---|---|
| `readdir` | `cache/fs.rs:1251` | for graph nodes, return god-node / member-file entries from the graph, not `fs_dentry` rows |
| `readdir_plus` | `cache/fs.rs:1310` | same, with attrs |
| `lookup` | `cache/fs.rs:1118` | resolve a god-node name → synthetic dir inode; resolve a member file → the **real** file inode |
| `getattr` | `cache/fs.rs:1198` | report synthetic god-node inodes as directories |

Data source already exists: `graph_entity(path,name,kind)`, `edges(file↔entity)`, and the
community/god-node projection in `graph_file.rs::build_digest` (Louvain → communities →
top-degree god-nodes). We expose that projection through the dir ops.

---

## 3. THE central question — does `os.walk` / `ls -R` return the *entire graph*?

**Short answer: yes, and worse, unless we deliberately project the graph into a bounded
tree. This is the make-or-break design decision.**

A filesystem tree and a knowledge graph are different shapes:

| property | directory tree (what `os.walk` assumes) | knowledge graph |
|---|---|---|
| parents per node | exactly 1 | **many** (a file belongs to many communities/god-nodes) |
| cycles | none | **yes** (entity↔entity relations loop) |
| size of full walk | = #files | **= Σ memberships** (files × concepts), can be 10–100× |

If we expose the **raw graph** as directories (god-node dirs that contain other god-node
dirs by relation, files hard-linked under every concept they touch), then:

- **`os.walk` / `ls -R` WOULD return (much more than) the entire graph** — every file
  duplicated under each concept it relates to, and
- **it can loop forever** on entity↔entity cycles, or explode combinatorially.

That is unacceptable. Two ways to bound it:

### Option A — tree projection (each file under ONE primary god-node)
Assign every file to its single top community/god-node (we already pick a primary
community in `build_digest`). The tree is then strictly 2–3 levels:
`/<god-node>/<file>`. `os.walk`/`ls -R` return ≈ one entry per file (bounded ≈ corpus
size). **Loses** multi-membership (a file about both "成交金额" and "畅销产品" shows under
only one).

### Option B — primary tree + **symlinks** for cross-edges (RECOMMENDED)
Primary membership is a real dir entry; every *other* community/relation a file
participates in is a **symlink** to the primary path. Why this is the answer to the
question:

- `os.walk(followlinks=False)` — **the Python default** — does **not** descend symlinks.
- `ls -R` — does **not** follow symlinks.
- `find` — follows symlinks **only** with `-L`.

So with symlinks for cross-edges, **`os.walk` / `ls -R` return the bounded primary tree
(≈ corpus size), NOT the entire graph.** The full graph is still *navigable* — an agent
that wants the cross-links can `ls` a god-node dir and follow a symlink — but a blind
recursive crawl can't blow up. We get graph richness without the explosion.

```
/畅销产品/                         (god-node = top-level dir)
   ├─ top10_product_status_table.xlsx   → real entry (primary)   [SOURCE INACCESSIBLE note]
   ├─ best_selling_list.txt             → real entry (primary)
   └─ 成交金额/                          → SYMLINK to /成交金额/  (cross-edge; os.walk skips)
/成交金额/
   ├─ best_selling_list.txt             → SYMLINK to /畅销产品/best_selling_list.txt
   └─ ...
```

**Decision needed:** A (lossy, simplest) vs B (recommended; bounded crawl + navigable
graph). Default to **B**.

### Bound guarantees we must encode regardless of A/B
- **Depth cap** on synthetic nesting — see configurable bounds below.
- **Cycle guard** in `lookup`/`readdir` (synthetic dir inodes never resolve back to an
  ancestor synthetic inode).
- **Fan-out cap** per god-node dir (top-N member files inline; rest reachable by grep) —
  mirrors `FILES_PER_TOPIC`/`MAX_TOPICS` thinking so a single `ls` stays small.
- **De-dup**: a file appears as a *real* entry exactly once (primary); everywhere else a
  symlink. So `os.walk` visiting a file twice cannot happen.

### Traversal model: a bounded beam-BFS from god-node roots
The graph has **many roots — one per god-node** (a topic's central entity). Traversal is a
**breadth-first expansion from each god-node root, to a bounded depth, with a bounded beam
(fan-out) per layer.** The FS *is* this BFS: `readdir(god-node)` expands one BFS layer;
the agent's `os.walk`/`find` walks layers until the depth cap.

```
  god-node root (layer 0)
     ├─ member files            (layer 1, ≤ FILES_PER_NODE)
     └─ typed-edge neighbors    (layer 1, ≤ XLINKS_PER_NODE)  → expand to layer 2 … until MAX_DEPTH
```

Why a **beam** and not plain depth-BFS: a single node can have thousands of neighbors in
one hop, so depth alone does NOT bound size (depth-2 with 160 roots × 600 files = 96K
entries). `MAX_DEPTH` bounds *layers*; the per-node caps bound *fan-out per layer*. Both
are required.

### Configurable bounds (the BFS parameters — ALL tunable variables)
Every limit that bounds the walk is a tunable variable — a `const` default that an env
var overrides — so the traversal can never run unbounded and can be tuned per workload
(mirrors the existing `MAX_TOPICS`/`FILES_PER_TOPIC` pattern in `digest.rs`). Each maps to
a BFS parameter:
- `MAX_DEPTH` = number of BFS layers (hops) from a god-node root.
- `TOP_TOPICS` = number of god-node roots exposed (BFS start points).
- `FILES_PER_NODE` / `XLINKS_PER_NODE` = beam width (neighbors expanded per node per layer).

| variable | env override | default | bounds |
|---|---|---|---|
| `GRAPH_MAX_DEPTH` | `SEMFS_GRAPH_MAX_DEPTH` | `2` | **max synthetic levels** below the graph root (god-node → [sub-god-node …] → files). Enforced in `readdir`/`lookup`: at depth == max, only files are returned, never another synthetic dir. This is THE knob that makes `os.walk`/`ls -R` finite. |
| `GRAPH_TOP_TOPICS` | `SEMFS_GRAPH_TOP_TOPICS` | `30` | god-node dirs listed at the root (reuse `MAX_TOPICS`) |
| `GRAPH_FILES_PER_NODE` | `SEMFS_GRAPH_FILES_PER_NODE` | `25` | member files (real entries) listed per god-node dir before "… +N more (grep to reach)" |
| `GRAPH_XLINKS_PER_NODE` | `SEMFS_GRAPH_XLINKS_PER_NODE` | `10` | cross-edge symlinks listed per god-node dir (the typed relations — see §4.5) |

`GRAPH_MAX_DEPTH` is checked on **every** synthetic `readdir`/`lookup` by tracking the
node's depth (derived from the synthetic-inode → community mapping). Worst-case full-walk
size is then bounded by `TOP_TOPICS × (FILES_PER_NODE) × MAX_DEPTH` — a fixed ceiling
independent of graph density. A unit test asserts a deep/cyclic graph still terminates at
`GRAPH_MAX_DEPTH`.

---

## 4. Design sketch

### Inode space
Reserve a synthetic inode range for graph dirs (e.g. `ino >= 1<<48`) so they never
collide with real `fs_inode` rowids. Map `synthetic_ino → community_id / god_node_id`
via an in-memory table (or a `graph_dentry` cache table, deterministic from the graph).

### readdir(ino)
```
if ino == ROOT and graph_view_enabled:
    return [god-node dir names...]   (top communities, largest-first, capped)
elif ino is a synthetic god-node dir:
    return [primary member files (real inodes)] + [cross-edge symlinks] + [sub-god-nodes]
else:
    (unchanged) real fs_dentry children
```
Coexistence: the real corpus tree must still resolve for **reads** (tasks reference real
paths; `cat /desktop/…/file` must work). So either (a) the graph view is the root and
real paths still resolve via `lookup` by absolute path, or (b) the graph view lives under
a synthetic prefix (`/by-topic/…`) alongside the real tree. **Decision needed** —
"top-level god-nodes" (per the request) implies (a); (b) is safer for path-referencing
tasks. Recommend shipping (b) first (`/by-topic/`), then evaluate (a).

### lookup(parent, name)
- god-node name under a graph dir → synthetic dir inode (getattr says S_IFDIR).
- member-file name under a graph dir → the **real** file inode (so reads hit real bytes
  + existing annotations, incl. `SOURCE INACCESSIBLE`).
- symlink name → S_IFLNK whose target is the primary path.

### getattr(ino)
Synthetic god-node inodes report mode `S_IFDIR|0o555`, size 0, derived. Symlinks report
`S_IFLNK`. (Requires the FUSE bridge to support `readlink`; verify it does.)

### Gating
Behind an env flag (`SEMFS_GRAPH_FS=on`, default off) + the existing `SEMFS_KG`, so it's
A/B-able against the current real-tree behavior — same discipline as the digest.

---

## 4.4 Data source — tables, NOT the kg/ files

The FS does **not** read `KNOWLEDGE_GRAPH.md` or `graph.json` at traversal time. Both are
*rendered outputs*. The walk reads the **backing tables** they are rendered from — the
single source of truth — because `readdir`/`lookup` already hold a DB connection:

| FS need | source | status |
|---|---|---|
| typed cross-edges (symlinks) | `graph_relation` (entity↔entity, typed, confidence) | ✅ persisted |
| file↔entity membership | `edges` | ✅ persisted |
| entity display names / kinds | `graph_entity` | ✅ persisted |
| **community → god-node → member-file skeleton** | Louvain in `build_digest` | ⚠️ **ephemeral — must persist** |
| integrity markers (inaccessible sources) | error-page detection (chunk annotations) | ✅ exists |

**Why not the files:** `KNOWLEDGE_GRAPH.md` is lossy (capped `FILES_PER_TOPIC=4`,
`MAX_TOPICS=30`, markdown — fragile to parse, incomplete). `graph.json` is the right
*shape* (full typed graph) but is a multi-MB file — parsing it per `readdir` is a non-starter.

**New work this implies:** the community/god-node projection is currently recomputed by
Louvain *inside* `build_digest` and never stored. We can't run Louvain per `ls`. So
`refresh_knowledge_graph` must **materialize the projection** into a queryable form the FS
reads cheaply:

- a `graph_community(file_path, community_id, is_primary)` table (primary = the file's home
  god-node dir; others become symlinks), and
- a `graph_god_node(community_id, entity_path, rank)` table (ranked god-nodes per community).

`KNOWLEDGE_GRAPH.md` and `graph.json` then become a *third* consumer of the same persisted
projection (re-rendered from the tables) — one source of truth, three views (digest file,
json file, filesystem).

## 4.5 Surfacing the OTHER two kg/ files into traversal

The `kg/` folder has three artifacts. `KNOWLEDGE_GRAPH.md` (communities + god-nodes)
gives the **dir skeleton** above. The other two are not redundant — they carry exactly
the data that makes the walk *useful*, so we pull from them:

### `graph.json` — the typed edges → become the cross-edge symlinks
`KNOWLEDGE_GRAPH.md` only knows "files share entities." `graph.json` (and
`GRAPH_REPORT.md`) carry the **typed entity→entity relations** with confidence
(`references`, `depends_on`, `implements`, `contradicts`, `cites`, `shares_data_with`,
…). Instead of generic "related" symlinks, the cross-edges become **typed**:

```
/畅销产品/
   ├─ best_selling_list.txt              (real, primary)
   ├─ depends_on → /成交金额/             (typed symlink, from graph.json edge)
   └─ contradicts → /problem_product_tracking/   (typed symlink)
```

So `graph.json` is the **backing store** for the walk: `readdir`/`lookup` read the
typed-edge list (already in the `graph_relation`/`edges` tables that back `graph.json`) to
emit the symlinks. The `GRAPH_XLINKS_PER_NODE` cap (top edges by confidence) keeps an `ls`
small. This turns navigation into "what does this topic *depend on / contradict*," which
is real graph-walking, not just clustering.

### `GRAPH_REPORT.md` — the integrity / knowledge-gap data → surfaced AT the file
`GRAPH_REPORT.md`'s **"Data integrity — inaccessible source files"** section (the 403
list) is the honesty-critical payload. In a flat `ls` it's easy to miss; in the graph
walk we surface it **at the node the agent is standing on**:

- An inaccessible file's traversal entry is **marked** (e.g. listed as
  `top10_product_status_table.xlsx  ⚠SOURCE-INACCESSIBLE` in `readdir`, or its `getattr`
  flags it), and reading it returns the existing `SOURCE INACCESSIBLE` annotation. The
  agent can't navigate *past* it without seeing it's broken.
- Optional: a tiny per-god-node summary file `/by-topic/<node>/_TOPIC.md` drawn from
  `GRAPH_REPORT.md` (this cluster's god-nodes, its typed relations, and **any
  inaccessible sources in it**). `cat`-ing into a topic then gives orientation + the
  integrity warning in one read — the same content `GRAPH_REPORT.md` has, but *local* to
  where the agent is looking.

**Net:** `KNOWLEDGE_GRAPH.md` → directory skeleton; `graph.json` → typed cross-edge
symlinks; `GRAPH_REPORT.md` → integrity markers + optional per-topic summary. All three
already exist and stay the single source of truth — we read their backing tables, we do
not recompute. **Caveat:** the integrity marker helps the agent *notice* a broken source
during the walk, but (as established) it does not *force* honest reporting — that remains a
separate behavioral gap.

## 5. Open questions / decisions to make before building
1. **A vs B** (tree-projection vs primary+symlinks). → recommend **B**.
2. **Root replacement vs `/by-topic/` overlay.** The request says "top-level god-nodes =
   top-level dirs" (root replacement). But tasks reference real paths that must still
   resolve for reads. → recommend **overlay first** (`/by-topic/`), measure, then decide
   on root replacement.
3. **Does the FUSE bridge support `readlink`/symlinks?** Must confirm before B.
4. **God-node dir naming** with CJK + `/` collisions (slugify is lossy; names can contain
   spaces/punct). Need a stable, collision-free naming scheme.
5. **Primary-community assignment** stability across re-index (don't churn paths every
   refresh).
6. **Interaction with `SEARCH_ONLY`** (today hides leaf files in `readdir`). Define how
   graph-view + search-only compose.

---

## 6. Success criteria (this is an experiment, like the digest was)
- On exploratory tasks, a clean (no-protocol) run **crawls the graph, not the raw tree**,
  and total tool-calls + tokens **drop materially vs kg4** (target: back toward the kg2
  band, ~3–5 calls, <80K tokens) **without** the protocol crutch.
- `os.walk` / `ls -R` from the mount root **terminate** and return a **bounded** listing
  (≈ corpus size, not Σ memberships) — verified by a test that builds a cyclic graph and
  asserts the recursive walk is finite and de-duped.
- Reads of real files (incl. the `SOURCE INACCESSIBLE` annotation) still work through the
  graph paths.
- Revert if it doesn't beat the baseline (YAGNI discipline from the parent ticket).

---

## 7. Risks
- **Crawl explosion / infinite loop** if bounding (§3) is wrong — the headline risk; the
  symlink design + cycle guard + caps are the mitigations, and the finite-walk test is the
  gate.
- **Path-resolution breakage**: tasks that reference real absolute paths must keep
  working; root replacement (decision 2) is where this bites — overlay avoids it.
- **Refresh churn**: synthetic inodes/paths must be stable across KG recompute or the
  agent's cached paths go stale mid-task.
- **Confirmed-irrelevant levers**: this does **not** fix the *honesty* gap (agent saw 403,
  wrote fabricated data). That's a separate, behavioral problem; this ticket targets only
  the *crawl/token* problem.

---

## 8. Test plan
- Unit: build a small cyclic, multi-membership graph; assert `readdir` of a god-node is
  capped; assert a simulated recursive walk (readdir + follow real dirs, skip symlinks)
  terminates and yields each file exactly once.
- Integration: mount a seeded corpus with `SEMFS_GRAPH_FS=on`; run `ls`, `ls -R`,
  `python -c "os.walk"`; assert bounded output + real-file reads succeed.
- E2E: re-run case 289 clean (no protocol) with graph-fs on; compare tool-calls/tokens to
  kg4; check the agent navigates by topic instead of `find`-sweeping.
