# Module 4: The Outside World — Backends and the Couriers That Reach Them

### Teaching Arc
- **Metaphor:** A **shipping logistics network**. Your local cache is the warehouse next door (instant). The backend is a distant fulfillment center. Between them runs a fleet of couriers, each with a different route and schedule: one drives new packages out (push), one checks for packages that got recalled (deletion scan), one keeps watching a slow package until it clears customs (inflight poller), one brings in packages ordered by others (pull). The couriers run on their own timers so YOU never wait at the loading dock.
- **Opening hook:** "In module 1 your write was 'durable the moment the prompt returned' — but it had not reached the cloud yet. So who actually drives it there? And what happens if the cloud is slow, or offline, or someone deleted a file on another machine? Meet the courier fleet."
- **Key insight:** semfs separates *durability* (local, synchronous, instant) from *delivery* (remote, asynchronous, background). A set of independent background loops reconciles the local cache with whatever backend you chose. You can turn whole halves of this fleet off (`--no-sync`, `--no-push`) to get read-only or write-only behavior.
- **"Why should I care?":** This is where real-world failure modes live — rate limits, slow server-side processing, costs, "my edit synced but my teammate's delete didn't show up for 5 minutes." Knowing the loops and their cadences lets you reason about *why* something is or is not showing up yet, and what each mount flag actually does.

### The three backend tiers (pattern cards)
From README — render as 3 pattern cards:
- **Embedded** — SQLite + sqlite-vec + FTS5. Local, offline, single binary. The default.
- **Server** — Postgres + pgvector. Concurrent multi-writer, large corpora (HNSW index).
- **Cloud** — Supermemory API. Zero local index, server-side memory graph.
Teaching line: "the filesystem behaves identically across all of them — *graceful degradation, not lowest common denominator*."

### The sync loops (from sync/mod.rs and push.rs doc comments)
Render as numbered step cards or icon rows. Canonical names from the code:
- **Loop A — delta pull.** Every ~30s, fetch documents sorted by most-recently-updated and reconcile anything newer than our watermark into the local cache.
- **Loop C — deletion scan.** Every ~5min, compare the full set of remote IDs against local records and unlink anything that disappeared remotely.
- **Loop D — push worker.** Sends queued local writes up to the backend. Coalesces rapid edits to AT MOST 2 requests per file (one in-flight + one pending). (Deep dive in module 5.)
- **Loop E — inflight poller.** Watches documents the server is still processing (extract → chunk → embed → index → done) and updates status.
- **Loop F — hydration worker.** Pulls file *contents* on demand.

### Code Snippets (pre-extracted — use verbatim)

**Snippet A — the sync knobs, with production-sane defaults.** File: `crates/semfs-core/src/sync/mod.rs` (lines 37-54)
```rust
/// Knobs for the sync engine. All optional — defaults are production-sane.
#[derive(Debug, Clone, Copy)]
pub struct SyncOptions {
    pub delta_interval: Duration,
    pub deletion_scan_interval: Duration,
    pub pull_enabled: bool,
    pub push_enabled: bool,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            delta_interval: Duration::from_secs(30),
            deletion_scan_interval: Duration::from_secs(300),
            pull_enabled: true,
            push_enabled: true,
        }
    }
}
```
English: the engine has four dials. Pull every 30 seconds, scan for deletions every 300 seconds (5 min), and two on/off switches. `--no-sync` flips `pull_enabled` off; `--no-push` flips `push_enabled` off.

**Snippet B — the flags literally just gate which loops start (with the actual test that proves it).** File: `crates/semfs-core/src/sync/mod.rs` (lines 266-273)
```rust
    #[tokio::test]
    async fn start_gates_loops_on_pull_and_push_flags() {
        let (_tx, rx) = watch::channel(false);
        assert_eq!(SyncEngine::start(fs(), opts(true, true), rx.clone()).len(), 5, "pull+push");
        assert_eq!(SyncEngine::start(fs(), opts(true, false), rx.clone()).len(), 3, "--no-push: pull loops only");
        assert_eq!(SyncEngine::start(fs(), opts(false, true), rx.clone()).len(), 2, "--no-sync: push loops only");
        assert_eq!(SyncEngine::start(fs(), opts(false, false), rx.clone()).len(), 0, "both off");
    }
```
English: this is a test, but it is the clearest spec in the codebase. With both switches on, 5 background loops run. `--no-push` → only the 3 pull-side loops. `--no-sync` → only the 2 push-side loops. Both off → silence. So a mount flag is not magic — it just decides which couriers clock in.

**Snippet C — why the server cannot be written to twice at once (the async-processing gotcha).** File: `crates/semfs-core/src/sync/push.rs` (lines 54-60)
```rust
/// Block until the remote doc reaches `status=done` or the deadline passes.
///
/// The Supermemory server accepts POST and PATCH synchronously but processes
/// them asynchronously (extracting → chunking → embedding → indexing → done).
/// Issuing a second PATCH *while* the doc is still processing silently drops
/// the new content, so before we send a follow-up write on the same doc we
/// must wait for the previous one to finish.
```
English: when you save a file to the cloud backend, the server says "got it" immediately but then does slow work in the background (pull the text out, cut it into chunks, turn each chunk into a meaning-vector, file it away). If you send a SECOND edit before the first finishes, the server silently throws away your new content. So semfs waits for "done" before sending the next write — which is exactly why edits can take a moment to appear in search.

### Interactive Elements
- [x] **Pattern cards (hero visual)** — the 3 backend tiers (Embedded / Server / Cloud) with icon, engine, and "best for." Border-top in three different actor colors.
- [x] **Code↔English translation** — Snippet A (`SyncOptions` + defaults). Connect each field to the user-facing mount flag.
- [x] **Data flow animation** — the courier fleet. Actors: `Local Cache` ↔ `Push queue` → `Backend`, and `Backend` → `Local Cache` (pull). Steps walk Loop D (push: claim job → send → wait for done) and Loop A (pull: fetch newer-than-watermark → reconcile into cache). Apostrophe-free labels.
- [x] **Quiz** — 3-4 questions, debugging/decision. Q1 (tracing flags): "You run `semfs mount shared-notes --no-push`. You edit a file locally. Does your edit reach the cloud?" (No — `--no-push` disables the push loops; local writes never leave your machine. It is how you read a shared container without contaminating it.) Q2 (failure mode): "You save a file, then immediately save it again 1 second later. Why might the second save seem to 'not take' for a bit?" (The server processes the first write asynchronously; semfs must wait until status=done before sending the second, or the server would silently drop it.) Q3 (decision): "You have 2 million documents and several machines writing at once. Which backend tier fits?" (Server / Postgres+pgvector — built for concurrent multi-writer and large corpora.) Q4 (tracing): "A teammate deletes a file on another machine. Roughly how long until it disappears from your mount, worst case?" (Up to ~5 minutes — the deletion scan runs every 300s.)
- [x] **Glossary tooltips** — pgvector, HNSW, FTS5, sqlite-vec, embedding/vector, watermark, reconcile, POST/PATCH, idempotent, polling, asynchronous, rate limit, corpus, multi-writer, queue, push/pull.
- [ ] Callout (1): the "aha" — separating *durable* (instant, local) from *delivered* (eventual, remote) is a pattern called "write-behind caching"; it is how you get speed AND safety without making the user wait on the network.

### Reference Files to Read
- `references/interactive-elements.md` → "Pattern/Feature Cards", "Code ↔ English Translation Blocks", "Message Flow / Data Flow Animation", "Numbered Step Cards", "Icon-Label Rows", "Multiple-Choice Quizzes", "Glossary Tooltips", "Callout Boxes"
- `references/content-philosophy.md` → all
- `references/gotchas.md` → all

### Connections
- **Previous module:** "The trait seam" — explained how backends plug into the `SemanticIndex`/`FileSystem` sockets. This module looks at the actual backends and the sync loops that drive between local and remote.
- **Next module:** "The clever tricks" — module 5 zooms into the engineering craft inside these loops: write coalescing (at-most-2 per file), exponential backoff, jittered intervals, crash-safe queues, the macOS NFS trick, and how hybrid semantic search actually finds meaning. Tease: "You have met the couriers. Next, the clever driving — how they avoid traffic jams, retries, and crashes."
- **Tone/style notes:** Accent = teal. Reuse canonical actor names (Cache 🗄️, SyncEngine 🚚, Backend 📚). Module file = ONLY `<section class="module" id="module-4">…</section>`.
