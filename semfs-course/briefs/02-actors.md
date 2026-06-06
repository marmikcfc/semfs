# Module 2: Meet the Cast

### Teaching Arc
- **Metaphor:** A **theater production**. The audience (you, or the AI agent) sees one seamless show — "a folder." Behind the curtain there's a whole crew: a translator at the door, a contract everyone signs, a fast local vault, a courier, a librarian, and a stage manager who keeps the whole thing running. Each has ONE job. The magic is in the division of labor.
- **Opening hook:** "Last module you watched your words pass through four hands on the way from your keyboard to the cloud. Those hands have names. Let's meet the crew."
- **Key insight:** semfs is built from a handful of components, each with a single responsibility, talking to each other through clean boundaries. Knowing who does what is what lets you tell an AI "the bug is in the sync layer, not the mount layer" instead of "it's broken somewhere."
- **"Why should I care?":** When something misbehaves — a write doesn't appear, search returns nothing, the mount hangs — the FIRST debugging move is figuring out *which actor* owns the problem. This module gives you the map.

### The cast (give each a persona, an emoji, and a one-liner; these are the canonical names — reuse them in later modules)
1. **The CLI** (`crates/semfs/`) 🎫 — the front desk. Parses your `semfs mount/grep/unmount` command and hands off. It's "a thin CLI dispatch layer. All real logic lives in the library."
2. **The Mount adapter** (`mount/`) 🚪 — the translator at the door. Turns kernel filesystem calls (FUSE on Linux, NFS on macOS) into plain method calls. "The *only* place in the codebase that knows about FUSE or NFS."
3. **The VFS / `FileSystem` trait** (`vfs/`) 📜 — the contract everyone signs. Defines what a filesystem must be able to do (lookup, read, write, mkdir…). (Module 3 goes deep on this — here just introduce it as "the rulebook.")
4. **The Cache / `CacheFs`** (`cache/`) 🗄️ — the fast local vault. A SQLite database that stores your bytes durably and instantly.
5. **The SyncEngine** (`sync/`) 🚚 — the courier. Background loops that carry writes up to the backend and pull remote changes down.
6. **The Backend / `SemanticIndex`** (`backend/`) 📚 — the librarian. Answers `grep` by meaning. Can be cloud, Postgres, or local.
7. **The Daemon** (`daemon/`) 🧑‍🔧 — the stage manager. The long-running background process that owns an active mount, with its socket/pid/log files.

### Code Snippets (pre-extracted — use verbatim)

**Snippet A — the binary is thin; the library does everything.** File: `crates/semfs/src/main.rs` (lines 29-34)
```rust
#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    cmd::dispatch(cli.command).await
}
```
English: parse the command line, then dispatch to the right handler. That's it. The interesting code lives in the `semfs_core` library, not here.

**Snippet B — the library's table of contents (the cast list, in the code's own words).** File: `crates/semfs-core/src/lib.rs` (lines 19-27)
```rust
pub mod agent_hint;
pub mod api;
pub mod backend;
pub mod cache;
pub mod config;
pub mod daemon;
pub mod mount;
pub mod sync;
pub mod vfs;
```
English: each `pub mod` is one crew member. `mount` is the door, `cache` is the vault, `sync` is the courier, `backend` is the librarian, `daemon` is the stage manager, `vfs` is the contract.

**Snippet C — the Mount is the only one who knows about FUSE/NFS.** File: `crates/semfs-core/src/mount/mod.rs` (lines 35-49)
```rust
pub enum MountBackend {
    /// FUSE — Linux only. Uses the `fuser` crate.
    Fuse,

    /// NFSv3 over localhost — works on both macOS and Linux.
    Nfs,
}
```
English: there are two ways to make the OS see a folder. Linux uses FUSE; macOS uses a tiny local NFS server (more on that trick in module 5). Everything *else* in semfs is blissfully unaware which one is in play — that's the point of separating this out.

### Interactive Elements
- [x] **Group chat animation (the hero visual — REQUIRED course element, lives here)** — actors with distinct colors: CLI 🎫, Mount 🚪, Cache 🗄️, SyncEngine 🚚, Backend 📚. Script a short conversation showing a write:
  1. CLI → Mount: "User ran `echo > deploy.md`. Your turn, door."
  2. Mount → Cache: "Save these bytes for deploy.md, please."
  3. Cache → Mount: "Stored and durable. You can tell them it is done."
  4. Mount → CLI: "Prompt can return now."
  5. SyncEngine (chiming in later): "I will carry deploy.md up to the backend in the background — nobody has to wait for me."
  6. Backend → SyncEngine: "Got it. Embedding and indexing it now so grep can find it by meaning."
  Use `.chat-window` with unique id `chat-module2`. NO apostrophes issues here (chat uses plain `<p>`, fine to use normal punctuation).
- [x] **Drag-and-drop matching** — match each actor to its ONE job. Chips: Mount, Cache, SyncEngine, Backend, Daemon. Zones: "Translates kernel filesystem calls (FUSE/NFS)" → Mount; "Stores your bytes durably and instantly (SQLite)" → Cache; "Carries writes to the backend and pulls remote changes" → SyncEngine; "Answers grep by meaning" → Backend; "The long-running process that owns the mount" → Daemon.
- [x] **Code↔English translation** — Snippet B (the `pub mod` list) is the perfect "cast list in the code" translation.
- [x] **Quiz** — 3 questions, debugging/architecture style. Q1: "Search returns nothing relevant, but your files read back fine with `cat`. Which actor is the prime suspect?" (The Backend/SemanticIndex or SyncEngine — files reading fine means Cache & Mount are healthy; meaning-search is the Backend's job.) Q2: "You want to support a brand-new way of mounting folders on some future OS. Which ONE component should change?" (The Mount adapter — it's the only one that knows about FUSE/NFS.) Q3: "Why is `main.rs` only ~6 lines of real logic?" (Separation: the binary just dispatches; all behavior lives in the reusable `semfs_core` library — testable, and reusable by other front-ends.)
- [x] **Glossary tooltips** — kernel, FUSE, NFS, trait, module/crate, library vs binary, dispatch, daemon, socket, PID, async, SQLite.
- [ ] Callout (1): "separation of concerns" — each actor has one job; this is why you can reason about (and fix) one without breaking the others.
- [ ] Visual file tree (optional): map the cast to `crates/semfs-core/src/{mount,cache,sync,backend,daemon,vfs}/`.

### Reference Files to Read
- `references/interactive-elements.md` → "Group Chat Animation", "Drag-and-Drop Matching", "Code ↔ English Translation Blocks", "Multiple-Choice Quizzes", "Glossary Tooltips", "Callout Boxes", "Icon-Label Rows", "Visual File Tree"
- `references/content-philosophy.md` → all
- `references/gotchas.md` → all

### Connections
- **Previous module:** "Where did your words go?" — traced a write end-to-end through 4 stages. This module names the components behind those stages.
- **Next module:** "The trait seam" — module 3 zooms into ONE actor's design: the `FileSystem` trait (the contract) and the `SemanticIndex` trait, and why "programming to an interface" is what makes the storage swappable. Tease: "One of these actors — the Contract — is the secret to how semfs swaps its entire storage engine without anyone noticing. Next."
- **Tone/style notes:** Accent = teal. CANONICAL ACTOR NAMES + emoji defined above — module 3-6 will reuse them, so keep them exactly. Module file = ONLY a `<section class="module" id="module-2">…</section>` block.
