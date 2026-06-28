# CLAUDE.md

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

---

## 0. Workspace map — where things live & what to edit where

The project's working knowledge is split across the repo and three external services
(set up 2026-06-15). **Route new artifacts to the right home; don't dump everything in the repo.**

| Kind of thing | Lives in | What to do |
|---|---|---|
| **Code, tests, scripts** | this repo (`crates/`, `benchmarks/`, …) | Edit in-repo as usual. |
| **Tickets / tasks / experiments** | **Linear** — team `SemFS` (key `SEM`), project [`SemFS`](https://linear.app/semfs/project/semfs-a5658a47a671) | One issue per `tickets/<name>/` folder. New work → new Linear issue; keep the folder name in the issue body. |
| **RCAs** | **Notion** — [RCAs database](https://app.notion.com/p/cce12444aeac40f2acf3f4ba4d7c24ce) (under the [SemFS](https://app.notion.com/p/380136091157819baf33ec8e81e8c3d6) page) | The repo `rcas/*.md` files remain the **canonical, full** source (CLAUDE.md §5 still applies: write every RCA to `rcas/`). Then mirror a digest row to the Notion DB with properties Date / Component / Status / Source. |
| **Architecture & design docs** | **Notion** — child pages under the [SemFS](https://app.notion.com/p/380136091157819baf33ec8e81e8c3d6) page | Author/maintain the human-facing docs here. |
| **Large binary artifacts** (seeds, `.tgz`, CSVs, HTML reports, datasets) | **Google Drive** — [`semfs/`](https://drive.google.com/drive/folders/1zUdEtLlN6CK6OP0cuuBI1og80Y-mAYZk) → `experiments/`, `research/`, `logs/` | Upload the artifact to Drive, then **link it from the relevant Linear issue** (Drive is the store; Linear holds the pointer). Don't commit large binaries to the repo. |

**Service IDs (for tooling):**
- Linear team `dabd4d11-1e91-4e78-8f92-eecfc2f3ad98` (`SEM`); project `4c5e9b65-c449-4238-85b1-1ab3295fa4d0`.
- Notion SemFS page `380136091157819baf33ec8e81e8c3d6`; RCAs data source `dc2a521a-58b9-493b-95ee-45506c5281bc`.
- Drive `semfs/` `1zUdEtLlN6CK6OP0cuuBI1og80Y-mAYZk` (experiments `1LSjqUl2eM_FiY_CVFM8JX5vBsCcXnRpL`, research `18_7VSeb-lmLXcIWN4fqN2rLWKQFL5pwe`, logs `1-ghiNLihz8zI6XSQ2hjxngacQAzB32du`).

**Note:** the 70 MB `tickets/workspace-bench-5arm-matrix/artifacts/matrix_artifacts_FULL.tgz`
is too large for the MCP API — upload it to Drive `semfs/experiments/` manually (drag-drop).

---

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

---

## 5. Debugging
For Debugging always read the code execution path, data flow, then create hypothesis and if needed discuss the hypothesis with the user before creating RCA.
Note down every RCA in rcas folder to remember it  later.

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.

---

## 6. Git workflow

Branch flow: **`feature/*` → `develop` → `master`.**
- Do work on a `feature/*` branch off `develop`.
- Merge `feature/*` into `develop` (integration).
- Promote `develop` → `master` for production.

The `saral-gateway/` subdirectory is mirrored to the standalone repo
`https://github.com/marmikcfc/saral-gateway.git` by the `sync-saral-gateway`
GitHub Action, which runs on pushes to `develop` and `master` (branch-for-branch).
Requires repo secret `GATEWAY_REPO_TOKEN` (PAT with write access to that repo).

---

## 7. Testing
- Always maintain a test cases scenarios in tests folder
- After every changes, run test suite.
- Always run test
- NEVER EVER CUT CORNERS. ALWAYS RUN END TO END TESTS BY SPINNING UP A SERVER AND USING RELEVANT CURLS

## 8. Logging
- Always put detailed debug logging

## 9. Users
- Our users are described in users.md. Always think about the users and their behaviours because they're who we're building for.

## 10. Output
- While explaining anything always prefer html, ASCII diagrams, interactivity within html and going from eli5 to expert level in 5 steps. 
- When explaining anything ground your explaination in current architecture what are we storing, how's the data being passed around components. A picture is louder than a 1000 words, so use UML diagrams and animation and interactivity in explaining data flows, current system etc.
- Use simple language on chat. The goal is to help me build intuitive understanding of the system.