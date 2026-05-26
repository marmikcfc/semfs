

# semfs

**A semantic filesystem.** Mount your agent's memory as an ordinary folder — `ls` it, `cat` it, write to it, and `grep` it *by meaning* — backed by a storage tier you choose, not one you're locked into.

`semfs` ( **sem**antic **f**ile**s**ystem ) turns a memory store into a real POSIX filesystem. An LLM agent (or you) reads and writes plain files; a semantic `grep` retrieves the *relevant* lines by meaning rather than exact text. The store underneath is **pluggable** — a local SQLite index, a Postgres/pgvector database, or a cloud memory service — chosen at mount time without the agent or your tools noticing.

> Why a filesystem? Because every tool already speaks it. Editors, `git`, scripts, shells, and agent harnesses all read files — so memory delivered as files needs no new SDK, no bespoke tool schema, and stays a stable, cache-friendly surface for the model.

**Mount it as a directory.** A real local folder for editors, scripts, and any tool that reads files — wherever a kernel and a filesystem exist (macOS, Linux, devcontainers, Codespaces, Docker, microVMs). `semfs` is a single self-contained Rust binary; there is no SDK to import and no language binding to install.

---

## Contents
- [How it works](#how-it-works)
- [Quickstart](#quickstart)
- [Commands](#commands)
- [Semantic search](#semantic-search)
- [Backends](#backends)
- [Build from source](#build-from-source)
- [Status](#status)
- [Acknowledgements](#acknowledgements)
- [License](#license)

## How it works

```
  your agent / editor / shell
            │  ls · cat · write · mv · rm · grep
            ▼
   ┌─────────────────────────┐
   │  semfs mount (FUSE/NFS)  │   a real folder on your machine
   └─────────────┬───────────┘
                 ▼
   ┌─────────────────────────┐
   │  local SQLite cache      │   bytes persist across restarts; writes are
   │  (instant, offline)      │   durable the moment they return
   └─────────────┬───────────┘
                 ▼ async, coalesced
   ┌─────────────────────────┐
   │  backend (you choose)    │   SQLite-vec · Postgres/pgvector · Supermemory
   │  embed · index · search  │
   └─────────────────────────┘
```

Writes land in a local SQLite cache first (fast and durable), then drain to the configured backend in the background. Reads return verbatim bytes. `grep` runs **hybrid semantic search** — vector similarity *and* keyword (BM25) ranking, fused — so it finds the right passage whether you remember the meaning or the exact token.

## Quickstart

```sh
# 1. authenticate your backend once (stored locally, 0600)
semfs login

# 2. mount a memory container as a folder
semfs mount my-notes --path ./my-notes

# 3. use it like any folder
echo "the deploy pipeline runs migrations before swapping traffic" > ./my-notes/deploy.md
cat ./my-notes/deploy.md

# 4. search by meaning (run from inside the mount)
cd ./my-notes && semfs grep "how are schema changes applied during a release"
#   → deploy.md:1:the deploy pipeline runs migrations before swapping traffic

# 5. unmount when done
semfs unmount my-notes
```

## Commands

| Command | What it does |
|---|---|
| `semfs login` | Store backend credentials locally (`~/.config` / `~/Library/Application Support`). |
| `semfs mount <tag> --path <dir>` | Mount a container as a folder. |
| `semfs grep <query> [path]` | Semantic search. Run inside the mount, or pass `--tag`. |
| `semfs unmount <tag\|path>` | Graceful unmount (drains pending writes); `--force` tears down a stuck mount. |
| `semfs list` | Show active mounts. |
| `semfs status` / `semfs logs` | Inspect a running mount / tail its daemon log. |
| `semfs install` | Self-install the binary to `~/.local/bin`. |

Common `mount` flags: `--key` (backend key), `--backend fuse|nfs`, `--ephemeral` (in-memory, nothing persists), `--clean` (drop local cache, re-pull), `--no-sync` (don't poll the backend for remote changes), `--memory-paths` (scope which paths generate memories).

## Semantic search

`semfs grep` searches by **meaning**, not substring. The query and your files need not share any words:

```
$ semfs grep "credential renewal flow"
auth.md:12:the access token is refreshed by the middleware before each request
```

Output is `filepath:line:chunk` — the chunk is verbatim from the file, so an agent can extract exactly the relevant lines instead of `cat`-ing whole files into its context.

## Backends

The store is chosen at mount time, and the filesystem behaves identically across all of them — "graceful degradation, not lowest common denominator."

| Tier | Engine | Best for |
|---|---|---|
| **Embedded** | SQLite + sqlite-vec + FTS5 | local, offline, single binary — the default |
| **Server** | Postgres + pgvector | concurrent multi-writer, large corpora (HNSW) |
| **Cloud** | Supermemory API | zero local index, server-side memory graph |

A single unified, transactional store keeps bytes + vectors + keyword index consistent — which is what makes crashes recoverable and sandbox snapshots a one-file copy.

## Build from source

```sh
cd crates && cargo build --release
#   binary: target/release/semfs
```

`semfs` is a pure Rust codebase — a single self-contained binary, no language bindings. Requires Rust 1.95+. On macOS, mounting uses a local NFS server (Apple removed third-party kernel FUSE); on Linux it uses FUSE.

## Status

`semfs` is under active development. **Working today:** the POSIX filesystem (mount/read/write/rename/delete), the durable local cache, background sync, and semantic `grep` via the configured backend. **In progress:** a fully **local, offline** semantic index in the Rust daemon (SQLite-vec + a local embedding model) so search needs no network — tracked as the backend-agnosticism roadmap in `docs/` and `progress.md`.

## Acknowledgements

`semfs` originated from **[smfs](https://github.com/supermemoryai/smfs)** by [Supermemory](https://github.com/supermemoryai) — their "memory as a filesystem" idea and the original Rust FUSE/NFS daemon are the foundation this project grew from, and we're grateful for it.

**`semfs` has since been substantially redesigned around a different architecture.** Where `smfs` paired a cloud-coupled store with a TypeScript reference implementation, `semfs` is a **pure Rust, backend-agnostic** system: storage sits behind a small set of traits, so the same filesystem façade runs on a local SQLite (sqlite-vec) index, Postgres/pgvector, or a cloud backend — with **fully local, offline semantic search** as the design goal. There are no TypeScript or Python bindings.

Per `smfs`'s MIT license, Supermemory's original copyright is preserved in [`NOTICE`](./NOTICE).

Design inspiration also from **[gbrain](https://github.com/garrytan/gbrain)** by [Garry Tan](https://github.com/garrytan) — a self-hosted, markdown-first AI memory system whose direction informed `semfs`'s: hybrid (vector + keyword) retrieval, a self-wiring/typed knowledge graph, and pluggable PGLite/Postgres backends.

## License

The project is licensed under the [Elastic License 2.0](https://www.elastic.co/licensing/elastic-license) — see [`LICENSE`](./LICENSE). In short: free to use, copy, modify, and self-host, but you may not offer `semfs` to third parties as a hosted/managed service, and you may not remove its license notices.