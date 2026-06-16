# RCA — semfs agent doesn't transcribe fully-available grep content (cases 53/171)

Date: 2026-06-16 · Component: semfs grep delivery + agent workflow · Status: root-caused; fixes TESTED (small n) — block-render alone insufficient, prompt fix non-deterministic; FUSE-enumeration fix outstanding
Related: SEM-35 (WB matrix), SEM-19 (dedup), `tickets/workspace-bench-5arm-matrix/PM_MATRIX_RESULT_2026-06-16.md`

## Problem (precise)

On WB-Lite cases **53 and 171**, the semfs (`nokg`) arm scores ~0–11% vs plain ~21–45%, **even on
verified-healthy mounts**. On case 53 the semfs cells either write an **empty deliverable**
(`c0a`: status ok, 0 files) or write one that **scores 0/11** (`c0b`). The deliverable that *was*
written (`c0b`) contains **none** of the values the rubric grades.

## The decisive evidence (case 53, working-mount cell `c0b`)

- **`semfs grep` returned the source records VERBATIM** into the agent's context, multiple times
  (~50 KB total): `/Desktop/Downloads/interaction_document_6.txt:[Interaction Design Document]
  Design ID: DES-0006 … Design Date: 2024-12-18 …` — all 4 docs (DES-0006/8/10/13), all dates,
  thresholds, specs. Verified: `has all 4 DES records present? True`.
- **The deliverable `c0b` wrote contains 0× `DES-0006`, 0× `2024-12-18`, 0× `< 200ms`, 0× `< 500ms`.**
  It produced a generic "Integrated Data Interaction Report" template and **never transcribed the data
  it was looking at.**

→ The agent **had the exact answer in context and did not use it.** Retrieval was perfect.

## Investigation arc (honest — several wrong turns, each corrected by the artifact)

1. "Retrieval accuracy" — WRONG (grep surfaced the files; ranking log confirmed).
2. "Affordance / over-exploration / DB-spelunking" — WRONG as a general cause; it was an **infra
   artifact**: the cell first analyzed (`53_nokg_rfd2`) had a **DEAD MOUNT** (`semfs list` → "no active
   mounts"), so the agent reverse-engineered the raw `.semfs/chanpin.db`. Localized to the fd2 batch
   (5 cells); 54/59 nokg cells had live mounts.
3. "Query rewriter corrupts search" (`PO_4`→"phosphate") — REFUTED by the controlled test: rewrite-OFF
   did not beat rewrite-ON.
4. "FUSE enumeration defect" — REAL but **secondary**: `find /ws/mnt/Desktop -maxdepth 5 -type f` → 0
   files while `ls` lists the dir and grep returns content. It makes the agent flail, but it is not why
   the answer is wrong (the content was available via grep regardless).
5. **VERIFIED root cause (this RCA):** the agent does not **transcribe** content that is fully in its
   context. `c0a` flails and writes nothing; `c0b` writes a generic report omitting every specific value.

## Root cause

**Delivery-form + workflow mismatch, not retrieval.** Plain `cat`s four clean discrete files → the
model faithfully transcribes the records (8/11). semfs delivers the *same content* as a **large, ranked,
repeated semantic-search blob** (file headers + similarity scores + each record ×3, ~50 KB) → the model
**summarizes generically instead of transcribing** (0/11). Compounded by the **enumeration defect**
(step 4): the agent can't fall back to plain's clean per-file `cat` (find/ls don't surface the files),
so it's stuck with the blob it synthesizes poorly from — or gives up empty.

- NOT retrieval (grep delivers verbatim records), NOT coverage, NOT infra (mount live), NOT the rewriter.
- IS: (a) the agent not transcribing available content, (b) the grep delivery form encouraging generic
  summary, (c) the enumeration defect blocking the clean-`cat` fallback.

## Fixes

**Immediate (prompt/affordance — no rebuild; under test):** a **turn-based transcription prompt** that
tells the agent the grep results ARE the source content, to STOP hunting for files, and to **transcribe
the exact values verbatim** (not summarize). Combined with **dedup ON** (SEM-19, suppresses re-sent
blobs), **KG off** (`nokg`), **rewrite OFF**. Knob: `benchmarks/e2b/knobs/fix_v1.json`.

**Deeper (code — needs Modal rebuild, separate):**
- **Fix the FUSE enumeration** so `find`/`ls` surface files that are in `fs_dentry` → restores the
  agent's clean "locate → cat each file → transcribe" path (the thing plain does). (Verify cause via a
  direct mount probe + reading the FUSE `readdir`/`getattr`.)
- **Improve grep delivery form**: return hits as clean per-file content (path + full text, deduped),
  not a ranked repeated blob, so the model transcribes rather than summarizes.

## Verification plan

1. Test the prompt fix on cases 53/171, n=2, vs plain, mount-gated (this pass).
2. Re-run the **c0b-style breakdown with rewrite off**: does the agent now transcribe the verbatim
   records (DES-0006 / 2024-12-18 / thresholds present in the deliverable)?
3. If the prompt fix closes the gap → behavior/affordance confirmed. If not → the code fixes
   (enumeration + delivery form) are required.

## Verification RESULTS (2026-06-16) — artifact-grounded, small n, mount-gated

Scores pulled from `…/artifacts/e2b_runs/pm_codex_{53,171}_*/rubrics_judge--*.json` (field `passed`):

| case | plain | **fix_v2** (block-render code + dedup + rw0, NO prompt) | **fix_v1** (block-render + dedup + rw0 + transcription prompt) |
|---|---|---|---|
| 53 (/11)  | 1, 5         | 0 (1 cell; 1 cell failed to produce judge) | 0, 0, 0, **8**, **11** |
| 171 (/18) | 11, 12       | 0, 11                                       | 0, 0, 0, 0, **11** |

**Verdict (gated on accuracy, per `analyze-benchmark-results`):**
- **Block-render code fix (`print_block`) alone does NOT close the gap.** Confirmed it *fires*
  (`=== end <path> ===` present in grep output) but case 53 still scored 0/11 on the cell that ran,
  and case 171 was 0 / 11 — i.e. no better than not having it. The delivery-form change is *necessary
  hygiene* but **not sufficient**.
- **The transcription prompt (fix_v1) is bimodal / non-deterministic.** When it lands it is a *large*
  win (case 53 **11/11** and **8/11**, far above plain's 1–5/11) — which **confirms the root cause**:
  told explicitly to transcribe, the agent uses the content it already has. But it collapses to **0**
  on most reps and on case 171 (only 1 of 5 reps scored). A prompt band-aid is not shippable.
- **Plain itself is noisy** (53: {1,5}; 171: {11,12}) — case 53 is partly a rubric/variance lottery.
- **n is tiny (1–2 reps/arm) → variance-dominated. NOT a clean verdict.** The 0-vs-11 swing on an
  *identical* config is the headline; any real conclusion needs n≥3–5.

**Conclusion:** the behavior hypothesis is confirmed (prompt → big wins prove the content was usable),
but no delivery-only or prompt-only fix is *reliable*. **The remaining durable lever is the FUSE
enumeration fix** — make `find`/`ls`/`readdir` surface files that are in `fs_dentry` so the agent can
fall back to plain's clean "locate → cat each file → transcribe" path deterministically, instead of
synthesizing from a ranked blob. That needs a Modal rebuild and a direct mount probe of the FUSE
`readdir`/`getattr` path (not yet done).

**Code status:** `print_block` (+ dedup `SessionCache`) is in the working tree
(`crates/semfs/src/cmd/grep.rs`, `crates/semfs-core/src/daemon/{ipc.rs,mod.rs,session_cache.rs}`) and
compiled into the ephemeral `benchmarks/e2b/assets/semfs-fixed` binary, but is **NOT git-committed.**
