# RCA — Phase-1 E2E "grep finds nothing": HTTP 402, then a masked grep-invocation bug

**Date:** 2026-05-26
**Context:** Verifying the Phase-1 backend-agnosticism refactor (`grep` → `Arc<dyn SemanticIndex>` → `CloudIndex`) end-to-end: mount a container, write a file, semantic-`grep` for it, unmount. The grep kept returning empty. Investigated with `superpowers:systematic-debugging`.

## Symptom
`crates/e2e/phase1_grep.sh` wrote `/auth-notes.md` into a fresh mount, then grepped a zero-lexical-overlap query (`"how does login credential renewal work"`). Grep returned nothing; the script reported INCONCLUSIVE / "not indexed yet".

## Three distinct causes (peeled in order)

### 1. HTTP 402 on push — free-plan credit exhaustion (external)
The daemon's push of the written file got `status=402`; daemon log: `push: poisoned; error sibling written status=402`. Direct `POST /v3/documents` reproduced it: body `{"error":"SuperRAG text limit reached","details":"You've run out of credits. Top up to continue."}`. `GET /v3/session` showed `plan: free`, org "saral". **Root cause: the account had no SuperRAG credits.** Not a code defect. **Fix:** use a key from an account with credits → push returns `200 {"status":"queued"}`.

### 2. The real, masked bug — grep tag resolution from outside the mount
After fixing the key, push + server-side indexing succeeded (`status=done`, doc searchable via `/v4/search` and via `semfs grep --tag`), yet the script's grep STILL returned empty. Isolation (re-mounting an already-indexed tag, so no timing confound):

- `semfs grep "<q>" "$MNT/"` run **from outside** the mount → **exit 1**, `Error: No container tag found. Either run from inside a mounted directory or pass --tag.`
- `cd "$MNT" && semfs grep "<q>"` (real agent usage) → **works**: `/auth-notes.md:1:the access token is refreshed by the auth middleware before each request`.

**Root cause:** `semfs grep` resolves the container tag from the **`.semfs` marker in CWD** (or an explicit `--tag`) — a positional *path* argument does NOT carry the tag (the path arg is only used to derive a filepath *prefix* for scoping). The harness ran grep from the repo root with the mount path as an arg, so tag resolution bailed. The script's `… 2>/dev/null || true` **swallowed the non-zero exit**, turning a hard error into silent empty output that masqueraded as "not indexed yet" for the entire poll window.
This is **not** a regression from the Phase-1 refactor (tag resolution is untouched) — it was a harness-invocation bug. Fix: grep from inside the mount (`cd "$MNT" && semfs grep "<q>"`).

### 3. Secondary finding — `status=done` lags search availability
While chasing cause #2, a status-based poll proved misleading: a doc reaches `status=done` (document *processing* complete) seconds before it becomes queryable in `/v4/search` (index *propagation*). Gate a search assertion on the **search returning the row**, never on document status (condition-based-waiting). Free-plan latency is also high and degrades under repeated writes to one account.

## Fixes applied (`crates/e2e/phase1_grep.sh`)
- Prefer an already-exported `SUPERMEMORY_API_KEY` over `bash/.env` (the old script overwrote a good env key with the stale one).
- Repo-root-relative paths via `git rev-parse --show-toplevel` (was hardcoded absolute).
- Poll **grep run from inside the mount** until it returns the file (the correct readiness gate), up to ~5 min.
- Corrected stale "401" wording to 402 + actionable failure message.

## Verification
Single deterministic run → `PASS: found via semantic search`, `/auth-notes.md:1:the access token…`, exit 0, clean teardown (`no active mounts`, no daemons). The Phase-1 refactor preserves cloud semantic-search behavior end-to-end.

## Lessons
- `|| true` on the command under test hides the exact failure you're debugging. Capture and inspect exit codes during diagnosis.
- For a search E2E, poll the search, not a processing-status proxy.
- `semfs grep` is CWD/marker- or `--tag`-driven; a path arg is prefix-scope only. (Candidate product improvement: resolve the tag from the path argument's marker too, so `grep <q> /path/to/mount/` works from outside.)
