# Documentation map

_Last updated: 2026-06-25. Where the project's knowledge lives and which docs are current. Stale docs are
kept (not deleted) but carry a `STALE-BANNER` at the top — grep `STALE-BANNER` to list them._

## Start here (current, authoritative)

| doc | what it is |
|---|---|
| [CURRENT_STATE.md](CURRENT_STATE.md) | **Living state snapshot.** Latest first; the entry point. |
| [SEEDS.md](SEEDS.md) | **Seeds — authoritative.** What seeds exist, the 3 layers, build/embed pipeline, gotchas, verify/rebuild. |
| [CLAUDE.md](CLAUDE.md) | Behavioural guidelines + **workspace map** (§0: where new artifacts go — Linear / Notion / Drive). |
| [AGENTS.md](AGENTS.md) · [users.md](users.md) · [MODELS.md](MODELS.md) | Agent affordances · user personas · model selection. |

## Active experiments

| doc | status |
|---|---|
| [tickets/wblite-ppr-ab/EXPERIMENT.md](tickets/wblite-ppr-ab/EXPERIMENT.md) | **PPR A/B** (1-hop vs Personalized PageRank hidden-KG prior), 4 personas n=3 on GLM-5.1-NVFP4 — RUNNING. |
| [tickets/wblite-plain-4persona-n3/EXPERIMENT.md](tickets/wblite-plain-4persona-n3/EXPERIMENT.md) | 4-persona plain baseline (SEM-39). |

## Reference (historical but still valid)

- `tickets/workspace-bench-5arm-matrix/`: `RESULTS.md`, `LEARNINGS.md`, `ANALYSIS.md` (5-arm matrix
  findings), `E2B_RUNBOOK.md` + `E2B_EXPERIMENT_LEDGER.md` (E2B harness ops + run ledger), `issue.md`.
- `tickets/fresh-seeds-gemma-uniform/` (SEM-38), `tickets/kg-quality/` (Leiden+kNN),
  `tickets/embedder-config-search/` (gemma-q4 choice), `tickets/ast-kg-code-lane/DESIGN.md`.
- `rcas/*.md` — root-cause analyses (canonical; Notion holds digests).

## Stale / superseded (kept for history — `STALE-BANNER` at top)

Marked stale 2026-06-25 (not deleted; per the workspace policy, history is preserved):

- **Root scratch (non-project):** `build_small_hackathon_ideas.md`, `creativity.md`, `opinions_forming.md`,
  `DASHBOARD.md` (old matrix-dashboard snapshot).
- **Superseded hidden-KG design** (now shipped as `crates/semfs-core/src/backend/hidden_kg.rs`):
  `tickets/workspace-bench-5arm-matrix/{HIDDEN_KG_EXPERIMENT_PLAN, HIDDEN_KG_IMPLEMENTATION_PLAN,
  HIDDEN_KG_IMPLEMENTATION_TICKET, KG_SCOPED_RETRIEVAL_TICKET, KG_CANDIDATE_LANE_IMPLEMENTATION_PLAN}.md`.
- **Historical handoffs:** `tickets/workspace-bench-5arm-matrix/{HANDOFF, HANDOFF_NEXT_SESSION,
  SESSION_HANDOFF_2026-06-11}.md`.
- **Stale/duplicate reports:** `tickets/workspace-bench-5arm-matrix/{EXPERIMENTS_REPORT, EXPERIMENT_RUN_2}.md`.
- **Frozen reference (older root logs):** `EXPERIMENTS.md` (through 2026-06-09), `progress.md`
  (backend-agnosticism roadmap, 2026-05) — superseded by `CURRENT_STATE.md` for anything later.

## Where things live (CLAUDE.md §0, recap)

Code/tests → this repo · Tickets/experiments → **Linear** (team `SemFS`) · RCAs → `rcas/*.md` (canonical)
+ **Notion** digest · Architecture/design docs → **Notion** SemFS page · Large binaries (seeds, `.tgz`,
CSVs, reports) → **Google Drive** `semfs/` (linked from Linear).
