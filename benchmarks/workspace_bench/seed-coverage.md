# chanpin seed/cloud coverage — environments + missing-file lists

_Maintained tracking doc. Last updated: 2026-06-08._
_Regenerate: `python3 /tmp/coverage.py` on the box (sources `benchmark.env`), then
`rsync` `/tmp/seed-coverage-missing/` → `benchmarks/workspace_bench/seed-coverage-missing/`._

## TL;DR
**Nothing is fully seeded.** The canonical workspace is **`chanpin_standard` = 1,452 files**.
The best environment (cloud `workspace-bench-chanpin`) covers ~74% by raw doc count
(45% by exact path-match — see caveat). All local seeds index <50% (incomplete-warm bug,
`rcas/2026-06-08-partial-seed-indexing.md`).

## Corpus variants (the source matters)
| corpus | files | what it is |
|---|---:|---|
| `chanpin_raw` | 2,212 | `standard` + a `node_modules/` dir (760 js/ts/json cruft). NOT a seed source. |
| **`chanpin_standard`** | **1,452** | **canonical workspace** (node_modules stripped; includes 84 `.extracted.md` Office sidecars). The runner restores the agent workdir from this. **SEED FROM THIS.** |
| `chanpin_seed` (`extract-test/`) | 1,368 | `standard` − the 84 `.extracted.md` sidecars. The (wrong) source our seeds used. |

## Environments + coverage (vs canonical 1,452 / 1,351 unique basenames)
| environment | source | embedder | raw count | canonical coverage | missing list |
|---|---|---|---:|---:|---|
| cloud `workspace-bench-chanpin` | (cloud push) | Supermemory SuperRAG | **1,073 docs** | ~74% raw / **45% path-match** | `seed-coverage-missing/cloud__workspace-bench-chanpin.txt` (747) |
| local `chanpin-e5-nosum.db` | chanpin_seed | e5-small | 725 files | **43%** | `seed-coverage-missing/local__chanpin-e5-nosum.txt` (764) |
| local `chanpin-gemma-clean.db` | chanpin_seed (cleaned) | gemma fp32 | 647 files | **41%** | `seed-coverage-missing/local__chanpin-gemma-clean.txt` (790) |
| local `chanpin-gemma-full.db` | **chanpin_standard** | gemma fp32 | re-seeding | (in progress) | `local__chanpin-gemma-fullin-progress.txt` (regenerate when done) |
| cloud `chanpin-e5-nosum` | (cloud) | — | 702 docs | — | — |
| cloud `chanpin-gemma` | (cloud) | — | **29 docs** | ~2% (effectively unseeded) | — |
| cloud `chanpin-e5` / `chanpin-e5-sum` / `chanpin-pglite` | — | — | 1 / 0 / 0 | empty | — |

## What's missing (the lists)
Per-environment missing-file lists are the `.txt` files in `seed-coverage-missing/`
(one canonical path per line). Dominant missing type across all envs: **docx/pptx/pdf
documents** (e.g. `/chanpin/compliance_and_risk_control/.../content_moderation_standards_*.docx`).
The complete corpus is at `chanpin_standard`; any missing path can be re-seeded from there.

## Method + caveats
- Matching key = basename, lowercased, `.extracted.md`/`.extracted.txt` stripped (robust to
  the prefix/case differences between cloud paths, seed paths, and corpus paths).
- **Cloud path-match (45%) is a LOWER BOUND.** Some Supermemory `filepath`s are *slugified*
  from the doc title (e.g. `/desktop/project/sichuan-…-pub`) and won't match a real corpus
  basename, so the cloud's true coverage is between **45% (path-match)** and **74% (raw 1,073/1,452)**.
- Local seed coverage is exact (paths come from `chunks.filepath` = real corpus paths).
- 1,452 files → 1,351 unique basenames (≈101 duplicate basenames across dirs); basename
  matching can't distinguish those — minor.

## Environment / access (for regeneration)
- Box: `ubuntu@13.201.35.159` · key `~/.ssh/semfs-benchmark`.
- Seeds: `~/.semfs/<tag>.db`. Corpus: `/srv/semfs-benchmark/Workspace-Bench/evaluation/filesys/chanpin_{raw,standard}`.
- **Supermemory key: `/srv/semfs-benchmark/benchmark.env`** (`SUPERMEMORY_API_KEY` + `SUPERMEMORY_API_URL`).
  ⚠️ The `~/.semfs_seed_env` `SUPERMEMORY_API_KEY` is EMPTY — use `benchmark.env`. Never print keys.
- Cloud count: `POST $SUPERMEMORY_API_URL/v3/documents/list` `{"containerTags":["<tag>"],"limit":1,"page":1}`
  → `.pagination.totalItems`.
- Full-workspace seed procedure (waits for index completion): `benchmarks/workspace_bench/seed_complete.sh`.
