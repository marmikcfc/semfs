# Module 6: When Things Break (and the Big Picture)

### Teaching Arc
- **Metaphor:** A **building superintendent's keyring and logbook**. When a tenant says "my apartment is acting weird," the super does not panic — they check: Is the building's caretaker on duty (is the daemon alive)? Is there a stale "out of order" sign left on a door from a crash (a leftover socket/pid file)? What does the logbook say (the per-tag log)? Debugging semfs is the same disciplined routine: check the process, clear the stale state, read the log.
- **Opening hook:** "Your mount is hanging. Or a file will not sync. Or `semfs grep` returns nothing. Panicking and re-running random commands is the rookie move. Here is the operator's routine — the same one that lets you break an AI out of a doom-loop."
- **Key insight:** Every semfs mount is backed by a long-running daemon with three little files on disk: a socket, a pid, and a log. Almost every "stuck mount" is explained by checking whether the daemon is alive and whether stale files were left behind. The system is designed to be *inspectable*.
- **"Why should I care?":** This is the module that pays for the whole course. When your AI agent is stuck in a loop ("the file is not there!" — when it is), the skill is knowing WHERE to look and WHAT question to ask. semfs externalizes its state to inspectable files and logs precisely so a human (or a smarter prompt) can intervene. That is the vibe-coder superpower: not writing the fix, but *locating* the problem and directing the fix.

### The daemon's footprint on disk (from daemon/mod.rs — render as a visual file tree)
```
<cache_dir>/
├── <tag>.db                  # SQLite cache (your bytes + queue live here)
├── sockets/<tag>.sock        # Unix IPC socket — how the CLI talks to the daemon
├── pids/<tag>.pid            # the daemon process ID
└── logs/<tag>.log            # per-tag rolling log — your first stop when debugging
```
Teaching point: one mount = one tag = one daemon = these four files. `semfs status` and `semfs logs` read them for you.

### Code Snippets (pre-extracted — use verbatim)

**Snippet A — "is the caretaker alive?" without any unsafe code.** File: `crates/semfs-core/src/daemon/mod.rs` (lines 62-76)
```rust
/// Is the given pid alive? POSIX: shells out to `kill -0 <pid>`, which
/// does not actually send a signal — it's just a liveness probe that
/// succeeds iff the process exists and we're permitted to signal it.
pub fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
```
English: to check whether the daemon is still running, semfs runs `kill -0 <pid>`. Despite the scary name, `kill -0` sends NO signal — it just asks the OS "does a process with this ID exist, and am I allowed to poke it?" If yes, the daemon is alive. (semfs uses a subprocess here to avoid `unsafe` code — the whole crate forbids it.)

**Snippet B — clearing the stale "out of order" sign after a crash.** File: `crates/semfs-core/src/daemon/mod.rs` (lines 86-104)
```rust
/// Remove leftover socket/pid files from a previous run whose daemon
/// isn't alive anymore. Returns true if anything was cleaned.
pub fn cleanup_stale(tag: &str) -> bool {
    let mut cleaned = false;
    match read_pid(tag) {
        Some(pid) if !pid_alive(pid) => {
            let _ = std::fs::remove_file(pid_path(tag));
            let _ = std::fs::remove_file(socket_path(tag));
            let _ = std::fs::remove_file(startup_path(tag));
            cleaned = true;
        }
        None if socket_path(tag).exists() => {
            let _ = std::fs::remove_file(socket_path(tag));
            let _ = std::fs::remove_file(startup_path(tag));
            cleaned = true;
        }
        _ => {}
    }
    cleaned
}
```
English: if a daemon crashed without cleaning up, it leaves a pid file and a socket file lying around — like an "out of order" sign on a door nobody is behind anymore. This function checks "is the pid in this file actually alive? No? Then these files are stale — delete them" so a fresh mount can start cleanly.

**Snippet C — how to turn on the firehose of detail.** File: `crates/semfs/src/main.rs` (lines 40-46)
```rust
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("semfs=info,semfs_core=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
```
English: by default semfs logs at "info" level. Set the `RUST_LOG` environment variable to crank up the detail — e.g. `RUST_LOG=semfs=debug,semfs_core=trace semfs mount …` makes it narrate nearly everything it does. This is the single best tool when a mount is misbehaving.

**Snippet D — the inflight poller speaks in tiers (concept, from sync/push.rs doc).** File: `crates/semfs-core/src/sync/push.rs` (lines 11-16)
```rust
//! - **Loop E — inflight status poller.** For every doc we've POSTed whose
//!   `fs_remote.last_status` hasn't reached `done` yet, periodically
//!   `GET /v3/documents/:id` on an age-bucketed cadence. Updates
//!   `mirrored_updated_at` when status flips to `done` so the pull
//!   reconciler's watermark stays honest. Logs INFO/WARN/STOP tiers for
//!   stuck-processing detection.
```
English: when the backend is slowly processing your file, semfs keeps checking on it, and escalates its log messages — INFO (normal), WARN (this is taking a while), STOP (something is genuinely stuck). So your log tells you the difference between "be patient" and "something is wrong."

### Interactive Elements
- [x] **Visual file tree (hero)** — the daemon's four files (`<tag>.db`, `sockets/<tag>.sock`, `pids/<tag>.pid`, `logs/<tag>.log`) with plain-English annotations.
- [x] **Code↔English translation** — Snippet A (`pid_alive` / `kill -0`) — surprising and memorable — and/or Snippet B (`cleanup_stale`). At least one.
- [x] **Numbered step cards — "The Operator's Debug Routine"** — a reusable checklist: 1) `semfs status` — is the daemon alive? 2) `semfs logs` — what does the log say? 3) Look for INFO/WARN/STOP tiers — patient vs stuck? 4) Stale files after a crash? cleanup runs on next mount. 5) Still stuck? `semfs unmount --force` tears down a wedged mount. 6) Re-mount with `RUST_LOG=…debug` for the firehose.
- [x] **Scenario quiz (REQUIRED quiz; make it the capstone)** — 4 questions tying the WHOLE course together. Q1 (debugging): "`semfs grep` returns nothing, but `cat` shows your file content fine. Walk the actors — where do you look?" (Cache & Mount are healthy since cat works; the issue is upstream — has the file finished syncing/indexing on the backend? Check logs for INFO/WARN/STOP on that doc; the inflight poller / SemanticIndex is the suspect.) Q2 (lifecycle): "Your machine hard-crashed mid-session. You re-run `semfs mount notes`. Why does it usually just work?" (`cleanup_stale` detects the dead pid and removes the leftover socket/pid files; the SQLite cache + WAL preserved your data; queued writes resume.) Q3 (the big payoff — steering AI): "Your AI agent insists a file it wrote 'is not there.' Based on everything you learned, what is the most likely real explanation and the calmest next instruction?" (The write is durable locally and will sync in the background; nothing is lost. Calmly tell the agent to re-read the file from the mount / wait for sync, not to rewrite it — avoid the doom-loop.) Q4 (architecture recap): "In one sentence, why is semfs designed so its entire state lives in inspectable files and logs?" (So a human or a better prompt can SEE what is happening and intervene — observability is what makes the system debuggable and steerable.)
- [x] **Glossary tooltips** — daemon, pid (process ID), `kill -0`, signal, socket/IPC, stale, WAL, environment variable, `RUST_LOG`, tracing/log levels (info/debug/trace), observability, watermark, force unmount.
- [ ] Callout (1-2): "aha" on observability ("you cannot fix what you cannot see — good systems externalize their state"); a closing callout that ties back to the vibe coder: the skill is not writing the fix, it is *locating* the problem and directing the fix.
- [ ] **Course wrap-up** — end with a short "You now understand semfs end-to-end" recap: the write path (M1), the cast (M2), the trait seam (M3), backends & sync (M4), the tricks (M5), and debugging (M6). One closing line on how this fluency lets them steer AI like an engineer.

### Reference Files to Read
- `references/interactive-elements.md` → "Visual File Tree", "Code ↔ English Translation Blocks", "Numbered Step Cards", "Scenario Quiz", "Multiple-Choice Quizzes", "Glossary Tooltips", "Callout Boxes"
- `references/content-philosophy.md` → all
- `references/gotchas.md` → all

### Connections
- **Previous module:** "The clever tricks" — covered coalescing, backoff, jitter, crash-safe queue, the NFS trick. This module uses the crash-safe + daemon design when debugging.
- **Next module:** none — this is the FINALE. Include the course wrap-up recap above.
- **Tone/style notes:** Accent = teal. Reuse canonical actor names (Daemon 🧑‍🔧, Cache 🗄️, Backend 📚, SyncEngine 🚚). This module should feel empowering and conclusive. Module file = ONLY `<section class="module" id="module-6">…</section>`.
