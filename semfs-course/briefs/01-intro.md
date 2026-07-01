# Module 1: Where Did Your Words Go?

### Teaching Arc
- **Metaphor:** A **dropbox at a post office**. You slide a letter through the slot — the moment it drops, it's safe (the post office now owns it, it won't blow away). But it hasn't been *delivered* yet. A courier picks it up later and routes it to its destination. semfs writes work exactly like this: the write is *durable* the instant it lands locally, but *delivery* to the real backend happens later, in the background.
- **Opening hook:** "You type `echo "remember the deploy runs migrations first" > ./notes/deploy.md` into a folder. Hit enter. The prompt comes back instantly. But where did your words actually go?"
- **Key insight:** semfs makes a *storage service* look like an *ordinary folder*. Every tool you already own — `cat`, `ls`, `grep`, your editor, git — speaks "files," so memory delivered as files needs no new SDK or special tool. And the bytes are safe locally the instant you write them; syncing to the cloud happens after.
- **"Why should I care?":** This is the whole reason semfs exists for AI agents — it gives an agent a *memory* it can read and write with plain file operations, and lets you `grep` that memory **by meaning**. Understanding "write lands local first, syncs later" is the #1 thing that explains every "my note didn't show up on the other machine yet" question you'll ever have.

### Codebase facts (use these — do not invent)
- semfs = "**sem**antic **f**ile**s**ystem." A single self-contained Rust binary. No SDK, no language bindings.
- It mounts a "container" (a named bucket of memory, identified by a **tag**) as a real folder on your machine.
- The killer feature: `semfs grep "<query>"` finds the *relevant* lines **by meaning**, not by exact word match. Query and file need not share a single word.
- The storage underneath is **pluggable** (local SQLite, Postgres, or a cloud memory service) — chosen at mount time. The folder behaves identically regardless.

### The architecture (from README — render as a flow diagram / numbered step cards, NOT ascii)
```
  your agent / editor / shell   →  ls · cat · write · mv · rm · grep
            ▼
   semfs mount (FUSE/NFS)        →  a real folder on your machine
            ▼
   local SQLite cache            →  bytes persist across restarts; writes are
   (instant, offline)               durable the moment they return
            ▼ async, coalesced
   backend (you choose)          →  SQLite-vec · Postgres/pgvector · Supermemory
   embed · index · search
```
Key line to teach: **"Writes land in a local SQLite cache first (fast and durable), then drain to the configured backend in the background. Reads return verbatim bytes."**

### Code Snippets (pre-extracted — use verbatim in code↔English blocks)

**Snippet A — the quickstart, the concrete user journey.** File: `README.md`
```sh
# 2. mount a memory container as a folder
semfs mount my-notes --path ./my-notes

# 3. use it like any folder
echo "the deploy pipeline runs migrations before swapping traffic" > ./my-notes/deploy.md
cat ./my-notes/deploy.md

# 4. search by meaning (run from inside the mount)
cd ./my-notes && semfs grep "how are schema changes applied during a release"
#   → deploy.md:1:the deploy pipeline runs migrations before swapping traffic
```
Teaching point: the query ("schema changes during a release") shares almost NO words with the file ("deploy pipeline runs migrations") — yet it matches. That's semantic search.

**Snippet B — grep output format.** File: `README.md`
```
$ semfs grep "credential renewal flow"
auth.md:12:the access token is refreshed by the middleware before each request
```
Teaching point: output is `filepath:line:chunk`. The chunk is verbatim text from the file, so an AI agent can grab *exactly* the relevant lines instead of reading whole files into its limited context window.

### Interactive Elements
- [x] **Code↔English translation** — Snippet A (the mount + write + grep sequence). English column explains each line in plain terms; emphasize "the prompt returns instantly because the write only had to reach your local disk, not the cloud."
- [x] **Data flow animation (the hero visual)** — actors: `You / Shell` → `semfs mount` → `Local SQLite cache` → `Backend (cloud)`. Steps: (1) highlight Shell — "you run echo"; (2) packet Shell→mount — "the folder receives your text"; (3) packet mount→cache — "bytes saved to local SQLite — now durable"; (4) highlight cache — "prompt returns to you HERE — your data is already safe"; (5) packet cache→backend — "later, in the background, it drains to the chosen backend to be embedded & indexed". **Remember: no apostrophes in data-steps labels — use "you" not "you've", etc.**
- [x] **Quiz** — 3 questions, scenario style. Q1 (tracing): "You write a file and the prompt returns. You yank your network cable a millisecond later. Is your data lost?" (No — it's already in the local SQLite cache; only the background sync is delayed.) Q2 (decision): "Why expose memory as a *folder* instead of giving the AI a custom 'save_memory' tool?" (Every tool already speaks files — editors, git, grep, shells — so no new SDK or tool schema is needed.) Q3 (concept): "`semfs grep 'how do I roll back a release'` returns a line that never contains the word 'rollback'. How?" (Semantic search matches by meaning/embeddings, not by exact text.)
- [x] **Glossary tooltips** — tooltip aggressively: filesystem, mount, container, tag, SQLite, cache, durable, backend, embed/embedding, index, semantic search, binary, SDK, POSIX, verbatim, context window, CLI, async/background.
- [ ] Callout box (1): an "aha" on why "memory as files" is a big deal for AI agents — a stable, cache-friendly surface the model already knows how to use.

### Reference Files to Read
- `references/interactive-elements.md` → "Code ↔ English Translation Blocks", "Message Flow / Data Flow Animation", "Scenario Quiz", "Multiple-Choice Quizzes", "Glossary Tooltips", "Callout Boxes", "Numbered Step Cards", "Flow Diagrams"
- `references/content-philosophy.md` → all (content rules)
- `references/gotchas.md` → all (the checklist)
- `references/design-system.md` → only if you need specific token names

### Connections
- **Previous module:** none — this is the opener. Start by introducing what semfs is in plain language before any code.
- **Next module:** "Meet the cast" — module 2 introduces the actual software components (Mount, Cache, Sync, Backend, Daemon) as characters. End module 1 by teasing: "You just watched your words travel through four hands. Next, let's meet each of them by name."
- **Tone/style notes:** Accent color is **teal**. This is the FIRST module — set a warm, friendly, "smart friend explaining" tone. Target learner is a vibe coder (builds with AI, no CS degree). Module file must contain ONLY a `<section class="module" id="module-1">…</section>` block — no `<html>/<head>/<body>/<style>/<script>`.
