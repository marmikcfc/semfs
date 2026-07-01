# Tech debt: `semfs grep` is stateless — it re-sends files the agent already has

Status: OPEN · Filed 2026-06-12 · **Design decided 2026-06-16** · Linear **SEM-19**
Rides the same daemon recompile as offload/N4 (Linear SEM-35, task: build linux binary → E2B).
Related: E16 adaptive-K (`tickets/workspace-bench-5arm-matrix/`).

## Problem

Each `semfs grep` invocation is **stateless**. It has no memory of what it returned on
previous calls in the same agent session. So when the agent re-queries with a similar
query (the common "re-grep with new wording" pattern), the same high-ranking files are
returned **again, in full**, and the agent **re-pays for content already in its context**.

Two concrete symptoms:

1. **Re-query re-sends.** `grep "store sales"` then `grep "best-selling products"` →
   the same top file comes back both times, full excerpt each time.
2. **`-n` is top-N, not paginated.** `grep "q" -n 1` then `grep "q" -n 2` returns hits
   {A} then {A, B} — hit A is sent a second time.

Under `cached_input=0` (the benchmark metric), every re-sent excerpt is re-paid on every
later turn. **Confirmed live** in the p2b/case-53 trace (2026-06-16): the agent issued
`grep "interaction document"` then 4 rephrases (`"interaction 6/8/10/13"`) → the same top
docs re-sent each time, then 4 `find` crawls, 9 discovery calls under a "~5 call" budget.
(The `find` half is a *separate* ranking/coverage cause — not addressed here.)

## Why a prompt hint is NOT the fix

We already ship a prompt nudge (`agent_hint.rs:200-210` + the p2b `WB_TURNBRAKE` knob):
*"Do NOT repeat a search you already ran — those files are already in your context."*
It is **non-deterministic** — the agent ignores it (the case-53 trace did 9 calls under a
"~5" budget; case-55 ignored an explicit filename it was handed). The fix must be
**server-side**, where the bytes are simply never produced — the agent cannot ignore what
it is never sent.

## Design (decided 2026-06-16)

Two facts from the code fix the architecture: (1) the `grep` **client is a fresh process
per call** → it cannot hold cross-call state; (2) the **daemon is long-lived per mount**
and its search path (`sqlite_vec.rs:62`) **already** reconstructs whole-doc for the top-N
and **already** reads `SEMFS_RESULT_LIMIT`. So the dedup memory lives in the daemon, and
seams in exactly where content is reconstructed.

**Core principle: DIFF, never REPLAY.** A query cache that replays the same response saves
daemon CPU but re-pays every agent token. The win is sending *less*: emit a pointer for
files already in context, content only for new ones.

### v1 — simple test, assume **one mount = one agent** (NO keying)

Under one-agent-per-mount, the daemon serves exactly one conversation, so a single
daemon-global sliding window IS the session memory. No session-id, no query-embeddings, no
similarity gate (file-in-context is query-independent — plain file-level recency is
correct).

```
SessionCache  (ONE per daemon, behind a Mutex)
  turn: u64
  ring: VecDeque<(path, turn)>          // last W turns only
  W = 5   (env SEMFS_DEDUP_WINDOW, 0 = off)   // the only knob

  on each Search:
    turn += 1
    for hit in top-N (result_limit()):
       path in ring?  → hit.seen_at_turn = Some(t); skip whole-doc reconstruct
       else           → reconstruct as today; ring.push((path, turn))
    evict where turn_seen < turn - W
```

Protocol: `Hit` gains `seen_at_turn: Option<u64>` (serde default `None` → wire-compatible).
Client (`grep.rs`): when `seen_at_turn = Some(n)`, print
`# already in your context (turn n): <path> — not resending` instead of the excerpt.
Everything else (caps, rendering) unchanged.

**Safety / failure shape:** an over-suppression always degrades to "agent reads the file it
was pointed to" — it can NEVER silently drop content the agent needed. Worst case = one
extra read, never a wrong answer. This bounded, soft failure is what licenses skipping all
session-disambiguation in v1.

**Degrade:** mountless/daemonless (Modal) has no daemon → no cache → exactly today's
stateless behavior. Zero risk to that path.

**Known v1 gap (accepted):** *sequential reuse* — if one daemon is reused across two
conversations, the first ~W queries of the second could be over-suppressed. Self-heals
within W (entries age out) and is soft (a needless read). Assumed low for now; v2 removes it.

### v2 — key by caller identity via `SO_PEERCRED` (zero client change, kernel-authenticated)

The grep client connects over a Unix socket. The daemon reads the connecting process's PID
straight from the kernel — `getsockopt(fd, SOL_SOCKET, SO_PEERCRED)` → `{pid, uid, gid}` —
no client/protocol change, and the client cannot spoof it. The peer PID is the ephemeral
grep process; walk `/proc/<peer_pid>` up the parent chain (skipping shells) to the
long-lived agent process, and key by **`(agent_pid, agent_starttime)`** (start-time guards
PID reuse).

```
cache: HashMap<(agent_pid, starttime), Ring>   // one sliding window per agent process
  + reap keys whose /proc/<pid> is gone (or unseen for a while)  → bounded memory
```

Same ring logic as v1, one map level added. Handles **both** concurrency (separate agents
on one mount get separate rings) and sequential reuse (new conversation = new process = new
key). Linux-only (`/proc`); the daemon is Linux/FUSE-only anyway, so this is fine.

## Plan

1. **v1 implement** — `SessionCache` on `IpcState`, partition loop in the `Request::Search`
   path, `seen_at_turn` on `Hit`, `grep.rs` render of the pointer line, `SEMFS_DEDUP_WINDOW`
   knob. Unit-test the ring (push/evict/seen) + the partition.
2. **v1 build + ship** — rebuild the linux x86_64 binary on Modal, re-bake into the E2B
   template (rides the offload/N4 rebuild — ship both in one binary).
3. **v1 A/B** — re-grep-heavy case (53; also 45/15) one-mount-one-agent, `W=5` vs `W=0`
   (off), n≥2. Measure: total tokens **at equal-or-better accuracy** (gate accuracy first).
4. **v2** — add `SO_PEERCRED` peer-cred keying + `(pid,starttime)` map + dead-key GC; re-run.

## Acceptance criteria

**v1 (simple test, one-mount-one-agent):**
- [ ] Daemon keeps a bounded global ring `(path, turn)`, window `W` via `SEMFS_DEDUP_WINDOW`.
- [ ] On a Search, top-N hits whose path is in-window get `seen_at_turn=Some(n)`, no excerpt
      re-sent; new files get content + are recorded.
- [ ] `grep.rs` prints the "already in your context (turn n)" pointer for seen hits.
- [ ] `SEMFS_DEDUP_WINDOW=0` disables; daemonless path unchanged.
- [ ] A/B on case 53 (re-grep-heavy): fewer total tokens at equal-or-better accuracy, n≥2.

**v2 (caller-keyed):**
- [ ] Daemon derives `(agent_pid, starttime)` via `SO_PEERCRED` + `/proc` ancestry; cache is
      `HashMap<key, ring>`; dead keys reaped.
- [ ] Concurrent agents on one mount do not cross-suppress; sequential reuse does not stale.

**Out of scope (separate levers):** `-n` offset pagination (symptom #2) — fold in after v1
if cheap; the `find`-crawl ranking/coverage cause from the case-53 trace.

## Interim mitigation (shipped)

`agent_hint.rs:200-210` tells the agent not to re-grep. Prompt-level nudge only, ignored in
practice — superseded by the daemon-side dedup above. The p2b `WB_TURNBRAKE` knob is the
same family.
