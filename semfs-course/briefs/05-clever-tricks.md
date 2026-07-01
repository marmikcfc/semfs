# Module 5: The Clever Tricks

### Teaching Arc
- **Metaphor:** A **veteran bicycle courier in a chaotic city**. A rookie tries to deliver every single package the instant it is handed over, gets stuck in traffic, crashes, loses packages, and re-rides the same street ten times. A veteran batches deliveries (coalescing), waits a little longer each time a building buzzer is broken before retrying (backoff), staggers departure times so couriers do not all hit the same intersection at once (jitter), keeps a waterproof manifest so a crash never loses a package (crash-safe queue), and knows a secret shortcut that avoids the toll bridge entirely (the macOS NFS trick). These are the tricks that separate "works in a demo" from "works in production."
- **Opening hook:** "You can build something that syncs files. Making it survive flaky networks, rapid-fire saves, server hiccups, and a laptop that sleeps mid-write — THAT is engineering. Here are the tricks semfs uses."
- **Key insight:** Production reliability is a stack of small, deliberate patterns — each one solving a specific real-world failure. None is complicated alone; together they are why semfs does not lose your data.
- **"Why should I care?":** These are exactly the things an AI assistant *will not add unless you ask*. Knowing they exist — "add exponential backoff," "coalesce rapid writes," "make the retry queue crash-safe," "add jitter so retries do not stampede" — turns you from someone who asks for features into someone who asks for *robust* features.

### The tricks (render as pattern cards, then deep-dive 2-3 with code)
1. **Write coalescing** — rapid edits to one file collapse to at-most-2 server requests (one in-flight + one pending). No matter how fast you hit save.
2. **Exponential backoff** — when a send fails, wait longer before each retry (0.5s, 1s, 2s, 5s, 15s, 30s, then 60s) instead of hammering a struggling server.
3. **Adaptive interval + jitter** — poll faster right after activity, stretch out when idle, and add a little randomness so many mounts do not all call home at the same instant.
4. **Crash-safe queue (WAL)** — the push queue lives in SQLite with Write-Ahead Logging, so a crash or a sleeping laptop never loses queued writes.
5. **The macOS NFS trick** — Apple removed kernel FUSE, so semfs runs a tiny NFS server on localhost and asks macOS to mount *that*. No kernel extension, no macFUSE install.
6. **Agent hint injection** — on mount, semfs writes a path-scoped note into `~/.claude/CLAUDE.md` (and Codex/Gemini equivalents) telling the AI to use `semfs grep` inside that folder. On unmount, it removes the note.
7. **Hybrid semantic grep** — search fuses *vector similarity* (meaning) with *keyword BM25* (exact tokens), so it finds the right line whether you remember the gist or the exact word.

### Code Snippets (pre-extracted — use verbatim)

**Snippet A — exponential backoff, the whole policy in one match.** File: `crates/semfs-core/src/sync/push.rs` (lines 33-45)
```rust
/// Exponential backoff in milliseconds for the Nth failed attempt
/// (attempt=0 → first retry, already-failed once).
fn backoff_ms(attempt: i64) -> i64 {
    match attempt {
        0 => 500,
        1 => 1_000,
        2 => 2_000,
        3 => 5_000,
        4 => 15_000,
        5 => 30_000,
        _ => 60_000,
    }
}
```
English: the more times a send has failed, the longer semfs waits before trying again — half a second, then 1, 2, 5, 15, 30, and finally capped at 60 seconds. This gives a struggling server room to recover instead of being machine-gunned with retries.

**Snippet B — adaptive cadence with jitter.** File: `crates/semfs-core/src/sync/mod.rs` (lines 219-230)
```rust
/// Adaptive cadence: shorter after activity, stretch when idle, add ±jitter.
fn adaptive_interval(base: Duration, empty_streak: u32) -> Duration {
    let secs = base.as_secs_f64();
    let adjusted = if empty_streak == 0 {
        (secs / 3.0).max(10.0)
    } else if empty_streak >= 3 {
        (secs * 2.0).min(60.0)
    } else {
        secs
    };
    jittered(Duration::from_secs_f64(adjusted), 5)
}
```
English: if the last poll found new changes (`empty_streak == 0`), check again sooner (but never under 10s). If nothing has changed three times in a row, slow down (up to 60s) to save work. Then add a few seconds of randomness so a thousand mounts do not all knock on the server at the same second.

**Snippet C — the macOS trick, in the code's own words.** File: `crates/semfs-core/src/mount/mod.rs` (lines 42-48)
```rust
    /// NFSv3 over localhost — works on both macOS and Linux.
    ///
    /// The daemon binds an in-process NFSv3 server on `127.0.0.1:<auto-port>`
    /// and asks the operating system's native NFS client to mount it. No
    /// kernel extension, no third-party driver — on macOS this is the trick
    /// that replaces FUSE entirely.
    Nfs,
```
English: macOS no longer allows third-party kernel filesystem drivers. So instead of installing one, semfs starts a mini network-file-server *inside itself*, on your own machine (127.0.0.1 = "this computer"), and uses the NFS client macOS already ships with to mount it. Your folder is technically a network drive pointing at a server one millimeter away.

**Snippet D — the coalescing guarantee (doc comment, paraphrase + quote).** File: `crates/semfs-core/src/sync/push.rs` (lines 18-22)
```rust
//! Together with the dirty_since flag set at write-time, these loops give
//! the mount a durable, coalescing, crash-safe write path: any rapid save
//! burst collapses to at-most-2 server requests per filepath (one inflight
//! plus one pending), retries survive `wrangler dev` restarts, and an
//! unmount drains the queue before releasing the mount.
```
English: hammer Ctrl-S fifty times in two seconds and the cloud still only gets at most two requests for that file — the one currently sending, plus one "latest" waiting its turn. The fifty intermediate versions never need to be sent; only the final state matters.

**Snippet E (optional, agent-hint) — semfs steers your AI for you.** File: `crates/semfs-core/src/agent_hint.rs` (lines 1-9, 16-17)
```rust
//! Inject and remove path-scoped semantic-search hints in agent
//! instruction files (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`,
//! `~/.gemini/GEMINI.md`).
//!
//! Each `semfs mount` writes a delimited block scoped to the absolute mount
//! path, telling Claude Code / Codex / Gemini CLI to use `semfs grep` when
//! searching inside that path. `semfs unmount` removes the block.
```
And the honesty note (lines 16-17): *"What this is not: a guarantee. Anthropic's docs concede ~no compliance guarantee on CLAUDE.md. Treat as a steer, not a contract."*
English: when you mount, semfs politely edits your AI's instruction file to say "inside this folder, search with `semfs grep`." It is a nudge, not a law — the model may or may not obey, and the code is refreshingly honest about that.

### Interactive Elements
- [x] **Pattern cards (hero)** — the 7 tricks, each a card with icon + one-line plain-English description.
- [x] **Code↔English translation** — Snippet A (backoff) AND Snippet C (the NFS trick). Both are short, punchy, and surprising — ideal teaching moments. (At least one required; do both.)
- [x] **Spot-the-bug challenge** — show a NAIVE retry loop (no backoff, retries instantly in a tight loop) and have the learner click the line that would hammer a struggling server. Reveal: "No backoff — this retries instantly forever and can take down the very server it is waiting on. The fix is exponential backoff (snippet A)." Write a small 5-line plausible Rust-ish snippet for this; mark the retry-without-delay line as the bug target.
- [x] **Quiz** — 3-4 questions, application/decision. Q1 (coalescing): "Your editor autosaves every keystroke — 200 saves in 10 seconds. How many of those 200 reach the cloud, worst case, for that file?" (At most 2: one in-flight + one pending.) Q2 (why jitter): "Why add RANDOM jitter to the poll interval instead of a clean round number?" (So many independent mounts do not all hit the server at the exact same instant — prevents a synchronized stampede / thundering herd.) Q3 (transfer): "You ask an AI to add a feature that calls a flaky third-party API. Name two of this module's tricks you would explicitly request." (e.g., exponential backoff on retries; a crash-safe/persisted queue; coalescing duplicate requests.) Q4 (macOS): "Why does semfs run a network file server on your own laptop instead of installing a macOS filesystem driver?" (Apple blocks third-party kernel extensions; the localhost-NFS trick needs no driver install.)
- [x] **Glossary tooltips** — exponential backoff, jitter, thundering herd, coalescing, idempotent, WAL (write-ahead logging), retry, kernel extension, localhost/127.0.0.1, NFS, BM25, vector similarity, hybrid search, debounce.
- [ ] Callout (1-2): "aha" on coalescing ("the intermediate states are disposable — only the latest matters"); optional second on "honest engineering" using the agent_hint "treat as a steer, not a contract" note.

### Reference Files to Read
- `references/interactive-elements.md` → "Pattern/Feature Cards", "Code ↔ English Translation Blocks", "'Spot the Bug' Challenge", "Multiple-Choice Quizzes", "Scenario Quiz", "Glossary Tooltips", "Callout Boxes"
- `references/content-philosophy.md` → all
- `references/gotchas.md` → all

### Connections
- **Previous module:** "The outside world" — covered the backend tiers and the courier loops (push, pull, deletion scan, inflight poller). This module is the craft INSIDE those loops.
- **Next module:** "When things break" — module 6 is the debugging toolkit: daemon lifecycle, pid/socket/log files, how to read the stuck-processing tiers, and how all this knowledge helps you escape an AI debug loop. Tease: "You now know the tricks that keep semfs healthy. Last stop: what to do when something goes wrong anyway — and how to debug it like an operator."
- **Tone/style notes:** Accent = teal. Reuse canonical actor names (SyncEngine 🚚, Mount 🚪, Backend 📚). This is the most fun module — lean into the surprise factor of each trick. Module file = ONLY `<section class="module" id="module-5">…</section>`.
