//! Shared daemon runtime. Called by both `semfs mount --foreground` and
//! `semfs daemon-inner` (the hidden subcommand invoked by the forking
//! parent). Owns the full lifecycle: open the cache, start the sync
//! engine, mount the filesystem, expose the IPC control socket, then
//! block on SIGTERM / SIGINT / IPC unmount and run the drain/unmount path.
//!
//! The one thing this module does NOT do is any TTY detachment
//! (`setsid` + stdio redirection) — those are the caller's responsibility
//! because the two caller profiles (foreground vs daemon child) differ.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::Notify;

use super::startup::StartupReporter;
use semfs_core::cache::{CacheFs, Db};
use semfs_core::daemon;
use semfs_core::mount::{mount_fs, MountBackend, MountOpts};
use semfs_core::vfs::traits::FileSystem;
use semfs_core::vfs::types::SetAttr;

const ROOT_INO: u64 = 1;

/// The daemon's index, as BOTH trait objects over one shared store: a
/// `LocalIndexer` for the write path (`CacheFs`) and a `SemanticIndex` for the
/// IPC search handler. One object, one connection, one owner.
type IndexPair = (
    Arc<dyn semfs_core::cache::LocalIndexer>,
    Arc<dyn semfs_core::backend::SemanticIndex>,
);

/// Build the daemon's index over the cache db. Uses the same resolver as `grep`,
/// so embedder/backend match. Storage backend is SQLite by default, or external
/// Postgres (`SEMFS_STORAGE_BACKEND=pgvector`, `pg` feature) or embedded pglite
/// (`=pglite`, `pglite` feature). Returns both trait objects over the one store.
async fn build_local_indexer(
    db: Arc<Db>,
    org_id: &str,
    container: &str,
    ephemeral: bool,
    clean: bool,
    env: &crate::cmd::resolve::ResolveEnv,
) -> anyhow::Result<IndexPair> {
    use crate::cmd::resolve::{choose_storage, StorageChoice};
    let embedder = crate::cmd::resolve::build_embedder(env)?;

    match choose_storage(env) {
        StorageChoice::Pgvector => {
            let _ = (&db, org_id, ephemeral, clean); // external Postgres isolation
                                                     // is the operator's job (connection string /
                                                     // database); local cache db, per-org dir, and
                                                     // ephemeral/clean lifecycle don't apply here.
            #[cfg(feature = "pg")]
            {
                let store =
                    Arc::new(crate::cmd::resolve::build_pg_store(env, container, embedder).await?);
                eprintln!("storage backend: pgvector (external Postgres), container={container}");
                Ok((store.clone(), store))
            }
            #[cfg(not(feature = "pg"))]
            {
                let _ = (container, embedder);
                anyhow::bail!(
                    "SEMFS_STORAGE_BACKEND=pgvector but this binary was built without the `pg` \
                     feature — rebuild with `cargo build --features pg`"
                )
            }
        }
        StorageChoice::Pglite => {
            let _ = &db; // pglite hosts the index; the SQLite cache db is unused here.
            #[cfg(feature = "pglite")]
            {
                let store = Arc::new(
                    crate::cmd::resolve::build_pglite_store(
                        env, org_id, container, ephemeral, clean, embedder,
                    )
                    .await?,
                );
                eprintln!(
                    "storage backend: embedded pglite, org={org_id}, container={container}, \
                     ephemeral={ephemeral}"
                );
                Ok((store.clone(), store))
            }
            #[cfg(not(feature = "pglite"))]
            {
                let _ = (org_id, container, ephemeral, clean, embedder);
                anyhow::bail!(
                    "SEMFS_STORAGE_BACKEND=pglite but this binary was built without the `pglite` \
                     feature — rebuild with `cargo build --features pglite`"
                )
            }
        }
        StorageChoice::Sqlite => {
            // Default: local SQLite (vec0 + fts5), with the dual-lane code embedder.
            let mut store = semfs_core::backend::SqliteVecStore::new(db, embedder)?;
            // FAIL-OPEN code lane: a code-model failure must not fail the mount —
            // code files fall back to the text lane (the floor).
            match crate::cmd::resolve::build_code_embedder(env) {
                Ok(Some(code)) => match store.enable_code_indexing(code) {
                    Ok(()) => eprintln!("code embedder enabled (vchunks_code lane)"),
                    Err(e) => eprintln!("code lane unavailable ({e}); indexing text lane only"),
                },
                Ok(None) => {}
                Err(e) => eprintln!("code embedder unavailable ({e}); indexing text lane only"),
            }
            // L5: cross-encoder rerank. FAIL-OPEN like the code lane — a reranker
            // model failure must NOT fail the mount; search falls back to RRF-only
            // ranking (the floor). Without this the sqlite daemon returned the raw,
            // UNRANKED RRF candidate pool (65–324 hits), so `grep` dumped the whole
            // pool and the agent saw no token savings. pg + offline-grep already
            // attach it via resolve.rs; this brings the sqlite daemon to parity.
            match crate::cmd::resolve::build_reranker(env) {
                Ok(Some(reranker)) => {
                    store = store.with_reranker(reranker);
                    eprintln!("L5 cross-encoder reranker enabled");
                }
                Ok(None) => {}
                Err(e) => eprintln!("reranker unavailable ({e}); RRF-only ranking"),
            }
            // L7: attach the entity-graph extractor when an LLM is available.
            if let Some(llm) = crate::cmd::resolve::build_llm(env) {
                store = store.with_graph_extractor(Arc::new(llm));
                eprintln!("entity-graph extraction enabled (L7)");
            }
            let store = Arc::new(store);
            Ok((store.clone(), store))
        }
        // Cloud builds no local index — callers gate on `storage.is_local()`.
        StorageChoice::Cloud => {
            unreachable!("build_local_indexer is only called for local storage backends")
        }
    }
}

/// Config needed to run the daemon body — subset of `mount::Args` that
/// drives behavior. Built once by `mount::run` and passed through either
/// an inline call (foreground) or a re-exec into `daemon-inner`.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub container_tag: String,
    pub mount_path: PathBuf,
    pub backend: MountBackend,
    pub api_key: String,
    pub api_url: String,
    pub memory_paths: Option<String>,
    pub ephemeral: bool,
    pub clean: bool,
    pub sync_interval: u64,
    pub deletion_scan_interval: u64,
    pub no_sync: bool,
    pub no_push: bool,
    pub drain_timeout: u64,
    pub import_existing: bool,
}

pub async fn run(cfg: DaemonConfig) -> Result<()> {
    let mut startup = StartupReporter::new(&cfg.container_tag);
    startup.report("creating_mountpoint", "preparing mountpoint")?;

    let created_dir = !cfg.mount_path.exists();
    if created_dir {
        std::fs::create_dir_all(&cfg.mount_path)?;
    }

    let pre_existing_paths = if cfg.import_existing && !created_dir {
        let files = collect_file_paths_recursive(&cfg.mount_path, &cfg.mount_path);
        if !files.is_empty() {
            eprintln!("collected {} file(s) for import", files.len());
        }
        files
    } else {
        Vec::new()
    };

    // uid/gid of the invoking user for the mount ownership.
    #[allow(unsafe_code)]
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };

    let marker_path = cfg
        .mount_path
        .parent()
        .unwrap_or(&cfg.mount_path)
        .join(".semfs");
    // The `.semfs` marker is written AFTER the cache db is opened (below), so it
    // can record the db path — letting `grep` open the local index with no
    // network. `marker_path` is computed above.

    // The cache home (`~/.semfs`) is a DIRECTORY; the per-mount marker is a FILE
    // also named `.semfs`, written at the mount's parent. They collide iff you
    // mount directly inside $HOME (parent == ~). Refuse that up front with a clear
    // message rather than fail obscurely later on create_dir_all / marker write.
    if marker_path == semfs_core::config::semfs_home() {
        anyhow::bail!(
            "refusing to mount directly inside your home directory: the per-mount \
             marker ({}) collides with the semfs cache home. Mount in a subdirectory instead.",
            marker_path.display()
        );
    }

    let opts = MountOpts::new(cfg.mount_path.clone(), cfg.backend).with_ownership(uid, gid);

    startup.report("validating_key", "validating API key")?;
    // A local-only mount (`--no-push --no-sync`) never talks to the Supermemory
    // server — push/pull are off and the cache is org-independent
    // (`~/.semfs/<tag>.db`) — so there is nothing to validate the key FOR. Skip the
    // call entirely: no network round-trip, works offline and with no/any key.
    // (tickets/decouple-sqlite-cache-scoping-from-supermemory.)
    let local_only = cfg.no_push && cfg.no_sync;
    let session = if local_only {
        None
    } else if cfg.ephemeral {
        semfs_core::api::ApiClient::validate_key(&cfg.api_url, &cfg.api_key)
            .await
            .ok()
    } else {
        Some(
            semfs_core::api::ApiClient::validate_key(&cfg.api_url, &cfg.api_key)
                .await
                .context("validating API key (required for push/sync)")?,
        )
    };

    // SECURITY: `org_id` comes from the server's session response and gets joined
    // into the embedded-pglite data dir (via `org_scope`, below) that `--clean`
    // / ephemeral cleanup hand to `remove_dir_all`. A hostile or compromised
    // server returning an org id with path separators or `..` would escape the
    // cache subtree and delete an unintended location. Reject it at the boundary,
    // before any path is built — `container_tag` is already validated at parse time.
    if let Some(org_id) = session.as_ref().and_then(|s| s.org_id.as_deref()) {
        if !semfs_core::config::is_safe_path_component(org_id) {
            anyhow::bail!(
                "server returned an org id that is not a safe path component \
                 ({org_id:?}); refusing to build cache paths from it"
            );
        }
    }

    // Captured for the marker so a separate `grep` can find the cache offline.
    let mut local_db_path: Option<String> = None;
    let db = if cfg.ephemeral {
        eprintln!("using ephemeral in-memory cache (nothing persists after unmount)");
        Arc::new(Db::open_in_memory()?)
    } else {
        // Org-independent, fixed location (`~/.semfs/<tag>.db`) — no session/org
        // needed, so this opens offline and keyless.
        startup.report(
            "opening_cache",
            format!("opening cache {}", cfg.container_tag),
        )?;
        let db_path = semfs_core::config::cache_db_path(&cfg.container_tag);
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let legacy_path = semfs_core::config::legacy_cache_db_path(&cfg.container_tag);
        if legacy_path.exists() && legacy_path != db_path {
            let _ = std::fs::remove_file(&legacy_path);
            let _ = std::fs::remove_file(legacy_path.with_extension("db-wal"));
            let _ = std::fs::remove_file(legacy_path.with_extension("db-shm"));
        }
        if cfg.clean {
            let _ = std::fs::remove_file(&db_path);
            let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
            let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
            eprintln!("cache cleared");
        }
        local_db_path = Some(db_path.display().to_string());
        Arc::new(Db::open(&db_path)?)
    };

    // Resolve the storage backend up front so the marker can RECORD it — `grep`'s
    // daemon-unreachable fallback must use the persisted backend, never its own
    // (possibly-drifted) env. Reused for the index gate below.
    let resolve_env = crate::cmd::resolve::ResolveEnv::from_env();
    let storage = crate::cmd::resolve::choose_storage(&resolve_env);

    // NOTE: the `.semfs` marker is written LATER — only after the index, mount,
    // and IPC are all up (see below). Writing it here (before mount success) would
    // leave a stale marker advertising a backend/db_path/tag for a mount that
    // never came up if any startup step fails, so commands run from that directory
    // would resolve a phantom mount. db_path + backend are captured now
    // (`local_db_path`, `storage`) and used at the deferred write.

    startup.report("configuring_api", "configuring API client")?;
    let mut api_client =
        semfs_core::api::ApiClient::new(&cfg.api_url, &cfg.api_key, &cfg.container_tag);
    if let Some(uid) = session.as_ref().and_then(|s| s.user_id.clone()) {
        api_client = api_client.with_user_id(uid);
    }
    let api = Arc::new(api_client);
    let session_user_id = session.as_ref().and_then(|s| s.user_id.clone());
    let session_user_name = session.as_ref().and_then(|s| s.user_name.clone());
    let session_org_name = session.as_ref().map(|s| s.org_name.clone());

    if let Some(raw) = &cfg.memory_paths {
        let paths: Vec<String> = if raw.is_empty() {
            Vec::new()
        } else {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        // `update_memory_paths` configures CLOUD memory generation. Skip it when
        // there's nothing to set (honors the documented `--memory-paths "" →
        // disable`) or for a local-only mount (no cloud container to configure —
        // calling it 404s and crashes the mount via `?`).
        // (tickets/local-mount-residual-cloud-calls.)
        if !local_only && !paths.is_empty() {
            api.update_memory_paths(paths).await?;
        }
    }

    // Optional local semantic index: the capability resolver decides whether a
    // real (non-hash) embedder is configured (local model dir or cloud key). If
    // so, build a SqliteVecStore over this same cache db and attach it so writes
    // (on flush), deletes, and renames maintain the local index alongside cloud
    // sync. Otherwise behavior is unchanged.
    let fs_base = CacheFs::with_api(db.clone(), api);
    // The daemon owns the index as BOTH a write-path indexer and a search index;
    // the search half is handed to the IPC server so `grep` can query it without
    // opening its own backend connection.
    let mut search_index: Option<Arc<dyn semfs_core::backend::SemanticIndex>> = None;
    // Held for the background L7 (entity-graph) worker — see run_graph_worker.
    let mut graph_indexer: Option<Arc<dyn semfs_core::cache::LocalIndexer>> = None;
    // Org scope for backends that still persist per-org on local disk (embedded
    // pglite). The SQLite cache is now org-independent (`~/.semfs/<tag>.db`), but
    // pglite keeps its per-org data dir. A missing/skipped session (ephemeral or
    // local-only mounts) → a stable sentinel keeps those off the persistent
    // per-org tree.
    let org_scope = session
        .as_ref()
        .and_then(|s| s.org_id.as_deref())
        .unwrap_or("_ephemeral");
    // `resolve_env` and `storage` were computed up front (before the marker write).
    use crate::cmd::resolve::StorageChoice;
    // Build a local index for every storage backend EXCEPT `cloud`, which has no
    // local index (Supermemory embeds + searches). The embedder is always real now
    // — `hash` is gone — so there's no embedder/backend contradiction to guard.
    let fs = if storage.is_local() {
        match build_local_indexer(
            db.clone(),
            org_scope,
            &cfg.container_tag,
            cfg.ephemeral,
            cfg.clean,
            &resolve_env,
        )
        .await
        {
            Ok((indexer, index)) => {
                eprintln!("local semantic index enabled");
                search_index = Some(index);
                graph_indexer = Some(indexer.clone());
                Arc::new(fs_base.with_indexer(indexer))
            }
            Err(e) => {
                match storage {
                    // An EXPLICITLY-selected external/embedded backend OWNS the
                    // index — it's the only search path (pglite has no direct
                    // route). If it fails to start, mounting anyway would leave a
                    // filesystem whose `grep` silently falls back to cloud and
                    // omits unsynced local writes. Fail the mount instead, so the
                    // failure is loud, not a stale-result trap.
                    StorageChoice::Pgvector | StorageChoice::Pglite => {
                        return Err(e.context(
                            "selected storage backend failed to start its index; \
                             refusing to mount (would silently degrade search to cloud)",
                        ));
                    }
                    // Default SQLite stays fail-OPEN: a degraded local index (e.g.
                    // a stale model dir) drops to cloud search rather than blocking
                    // the mount — the long-standing behavior.
                    StorageChoice::Sqlite => {
                        eprintln!("local index disabled: {e}");
                        Arc::new(fs_base)
                    }
                    // Cloud never builds a local index (gated out by is_local()).
                    StorageChoice::Cloud => unreachable!("cloud builds no local index"),
                }
            }
        }
    } else {
        Arc::new(fs_base)
    };
    fs.setattr(
        ROOT_INO,
        SetAttr {
            uid: Some(uid),
            gid: Some(gid),
            ..Default::default()
        },
    )
    .await
    .context("setting root ownership")?;

    // A local-only mount has no cloud to pull from. Skip the initial pull entirely
    // (no network) and treat it as "succeeded" so auto-import still runs — local
    // dedup is reliable against the warm cache without remote reconciliation.
    // (tickets/local-mount-residual-cloud-calls.)
    let pull_succeeded = if local_only {
        true
    } else {
        startup.report("initial_sync", "starting initial sync")?;
        match semfs_core::sync::SyncEngine::initial_pull_with_progress(&fs, |progress| {
            match progress {
                semfs_core::sync::InitialPullProgress::DeletionScan(progress) => {
                    if progress.remote_seen == 1 || progress.remote_seen % 100 == 0 {
                        let _ = startup.report_counts(
                            "initial_sync",
                            format!(
                                "deletion scan saw {} remote docs (page {}/{})",
                                progress.remote_seen, progress.page, progress.total_pages
                            ),
                            progress.remote_seen,
                            progress.total_items,
                        );
                    }
                }
                semfs_core::sync::InitialPullProgress::Pull(progress) => {
                    if progress.reconciled == 1 || progress.reconciled % 100 == 0 {
                        let _ = startup.report_counts(
                            "initial_sync",
                            format!(
                                "reconciled {} docs (page {}/{})",
                                progress.reconciled, progress.page, progress.total_pages
                            ),
                            progress.reconciled,
                            progress.total_items,
                        );
                    }
                }
            }
        })
        .await
        {
            Ok((removed, reconciled)) => {
                eprintln!(
                    "initial sync: {reconciled} docs reconciled, {removed} stale entries removed"
                );
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "initial sync failed; mount will continue without auto-import");
                false
            }
        }
    };

    if !pre_existing_paths.is_empty() {
        if !pull_succeeded {
            eprintln!(
                "skipping auto-import of {} file(s): initial sync failed, \
                 cache cannot reliably detect duplicates. Remount when online to import.",
                pre_existing_paths.len()
            );
        } else {
            let mut imported = 0usize;
            let mut skipped = 0usize;
            let mut errors = 0usize;
            // Read each file's content lazily, one at a time, so we never hold
            // the whole corpus in memory (the import runs before `mount_fs`, so
            // the underlying files are still readable by their real path).
            for (rel_path, real_path) in &pre_existing_paths {
                let contents = match std::fs::read(real_path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(path = %real_path.display(), error = %e, "import: read failed");
                        errors += 1;
                        continue;
                    }
                };
                match fs
                    .import_file_with_ownership(rel_path, &contents, uid, gid)
                    .await
                {
                    Ok(true) => imported += 1,
                    Ok(false) => skipped += 1,
                    Err(e) => {
                        tracing::warn!(path = %rel_path, error = %e, "import failed");
                        errors += 1;
                    }
                }
            }
            eprintln!("import: {imported} imported, {skipped} already existed, {errors} failed");
        }
    }

    // Sync engine: pull gated by --no-sync, push gated by --no-push.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let sync_opts = semfs_core::sync::SyncOptions {
        delta_interval: std::time::Duration::from_secs(cfg.sync_interval),
        deletion_scan_interval: std::time::Duration::from_secs(cfg.deletion_scan_interval),
        pull_enabled: !cfg.no_sync,
        push_enabled: !cfg.no_push,
    };
    let sync_tasks =
        semfs_core::sync::SyncEngine::start(fs.clone(), sync_opts, shutdown_rx.clone());

    // Capture the L7 queue handle for IPC `Status` (lets a client/warm wait for
    // graph extraction to fully drain) BEFORE the worker spawn moves the indexer.
    let graph_queue = graph_indexer.as_ref().and_then(|i| i.graph_queue());

    // L7 background entity-graph worker: drains the indexer's extraction queue
    // with bounded concurrency so the per-file blocking LLM call never sits on
    // the synchronous index/flush path. No-op (exits immediately) when the
    // indexer has no graph extractor attached.
    if let Some(idx) = graph_indexer {
        let gw_shutdown = shutdown_rx.clone();
        // L6: when entity extraction settles after add/remove, recompute the KG
        // and refresh `/KNOWLEDGE_GRAPH.md` (debounced inside the worker).
        let kg_fs = fs.clone();
        let kg_refresh: Arc<dyn Fn() + Send + Sync + 'static> = Arc::new(move || {
            if let Err(e) = kg_fs.refresh_knowledge_graph() {
                tracing::warn!("dynamic knowledge-graph refresh failed (non-fatal): {e}");
            }
        });
        tokio::spawn(async move {
            semfs_core::cache::run_graph_worker(idx, gw_shutdown, Some(kg_refresh)).await;
        });
    }

    startup.report("mounting_fs", "mounting filesystem")?;
    let handle = mount_fs(fs.clone(), opts).await?;

    // Materialize the workspace knowledge graph (`/KNOWLEDGE_GRAPH.md`) from the
    // warm cache's entity edges so `ls` shows it and `cat` serves it immediately.
    // Fail-soft: a graph error never blocks the mount. On a fresh seed the graph
    // is sparse → a structural map now, refreshed by the L7 worker as edges land.
    if let Err(e) = fs.refresh_knowledge_graph() {
        tracing::warn!("knowledge-graph build failed (non-fatal): {e}");
    }

    // Auto-install grep wrapper on first mount.
    if let Ok(true) = super::init::ensure_grep_wrapper_present() {
        eprintln!(
            "semantic grep enabled. run: source ~/.zshrc (new terminals have it automatically)"
        );
    }

    // Bring up the IPC control socket. Clients use it for status/sync/unmount.
    startup.report("starting_ipc", "starting IPC socket")?;
    daemon::ensure_dirs().context("creating daemon state dirs")?;
    let ipc_shutdown_notify = Arc::new(Notify::new());
    let state = Arc::new(semfs_core::daemon::ipc::IpcState {
        tag: cfg.container_tag.clone(),
        mount_path: cfg.mount_path.display().to_string(),
        fs: fs.clone(),
        index: search_index,
        started_at: Instant::now(),
        pull_enabled: !cfg.no_sync,
        backend: storage.as_str().to_string(),
        user_id: session_user_id,
        user_name: session_user_name,
        org_name: session_org_name,
        graph_queue,
        shutdown_notify: ipc_shutdown_notify.clone(),
    });
    let socket_path = daemon::socket_path(&cfg.container_tag);
    // Bind the control socket SYNCHRONOUSLY here so a bind failure aborts the
    // mount (via `?`) BEFORE we publish the pid file / `.semfs` marker / `ready`.
    // Binding inside the spawned task would only log the failure, leaving a marker
    // that advertises a control plane which never came up (and a parent waiting on
    // Ping would then time out and SIGKILL the child, bypassing marker cleanup).
    let ipc_listener = daemon::ipc::bind(&socket_path).context("binding IPC control socket")?;
    let ipc_shutdown_rx = shutdown_rx.clone();
    let ipc_socket = socket_path.clone();
    let ipc_handle = tokio::spawn(async move {
        if let Err(e) = daemon::ipc::serve(state, ipc_listener, ipc_socket, ipc_shutdown_rx).await {
            tracing::warn!(error = %e, "ipc server exited with error");
        }
    });

    // Write our PID. Keep it alive for the life of the process; cleaned
    // up at the end of this function.
    let pid_path = daemon::pid_path(&cfg.container_tag);
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&pid_path, std::process::id().to_string())?;

    // Write the `.semfs` marker ONLY now — index, mount_fs, IPC socket, and the
    // pid file have all succeeded, so the mount is known-good. A failure on any
    // earlier step returned before reaching here, leaving NO marker for a mount
    // that never came up. The unmount epilogue removes this entry.
    {
        use super::marker::{format_marker, parse_all_markers, SmfsMarker};
        let new_entry = SmfsMarker {
            tag: cfg.container_tag.clone(),
            api_url: cfg.api_url.clone(),
            mount_path: Some(cfg.mount_path.display().to_string()),
            // Record db_path ONLY for the SQLite backend. It exists so `grep` can
            // reopen the SQLite *vector* index offline — but pgvector/pglite don't
            // store vectors there (the SQLite cache holds only file metadata for
            // those backends). Advertising it for a non-SQLite mount is exactly the
            // stale-result trap: grep could reopen a leftover SQLite vec index from
            // an earlier config. So a non-SQLite marker carries NO db_path, closing
            // that route structurally regardless of the backend field or env.
            db_path: match storage {
                StorageChoice::Sqlite => local_db_path.clone(),
                // Non-SQLite (incl. cloud, which has no local vector index at all)
                // carries no db_path — closing the stale-SQLite-reopen route.
                StorageChoice::Pgvector | StorageChoice::Pglite | StorageChoice::Cloud => None,
            },
            backend: Some(storage.as_str().to_string()),
        };
        let content = if marker_path.exists() {
            let existing = std::fs::read_to_string(&marker_path).unwrap_or_default();
            let mut entries = parse_all_markers(&existing);
            entries.retain(|m| m.tag != cfg.container_tag);
            entries.push(new_entry);
            entries
                .iter()
                .map(format_marker)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            format_marker(&new_entry)
        };
        std::fs::write(&marker_path, content)?;
    }

    startup.report("ready", "filesystem mounted and IPC ready")?;

    eprintln!(
        "semfs mounted at {} (backend: {}, tag: {})",
        handle.mountpoint().display(),
        handle.backend(),
        cfg.container_tag,
    );

    // Wait for SIGTERM, SIGINT, or IPC `Unmount` request.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("register SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
            _ = ipc_shutdown_notify.notified() => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = ipc_shutdown_notify.notified() => {},
        }
    }
    eprintln!("\nunmounting...");

    // Drain the push queue. Push-side op — runs regardless of --no-sync.
    // Skipped under --no-push: the push worker never started, so any queued rows
    // (from local writes) can never drain — discard them with the cache instead of
    // blocking unmount for the full drain_timeout.
    if cfg.no_push {
        let pending = fs.push_queue_len();
        if pending > 0 {
            eprintln!("--no-push: discarding {pending} unpushed local write(s) at unmount");
        }
    } else {
        let deadline = Instant::now() + std::time::Duration::from_secs(cfg.drain_timeout);
        let mut last_report = 0usize;
        loop {
            let n = fs.push_queue_len();
            if n == 0 {
                if last_report > 0 {
                    eprintln!("push queue drained");
                }
                break;
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    pending = n,
                    "push queue drain timeout; rows persist and will resume next mount"
                );
                eprintln!("push queue drain timed out with {n} pending (will resume next mount)");
                break;
            }
            if n != last_report {
                eprintln!("draining push queue: {n} pending");
                last_report = n;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    // Signal sync + IPC loops to exit.
    let _ = shutdown_tx.send(true);
    let mut set = sync_tasks;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while set.join_next().await.is_some() {}
    })
    .await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ipc_handle).await;

    // The final deletion scan reconciles against REMOTE docs (a cloud pull). Skip
    // it for a local-only mount — there is no remote to reconcile against, and it
    // would be a residual network call. (tickets/local-mount-residual-cloud-calls.)
    if !local_only {
        semfs_core::sync::SyncEngine::unmount_scan(&fs).await;
    }

    drop(handle);
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("umount")
            .arg(&cfg.mount_path)
            .output();
    }
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    {
        use super::marker::{format_marker, parse_all_markers};
        if let Ok(existing) = std::fs::read_to_string(&marker_path) {
            let remaining: Vec<_> = parse_all_markers(&existing)
                .into_iter()
                .filter(|m| m.tag != cfg.container_tag)
                .collect();
            if remaining.is_empty() {
                let _ = std::fs::remove_file(&marker_path);
            } else {
                let out = remaining
                    .iter()
                    .map(format_marker)
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = std::fs::write(&marker_path, out);
            }
        } else {
            let _ = std::fs::remove_file(&marker_path);
        }
    }
    let _ = std::fs::remove_file(&pid_path);
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(daemon::startup_path(&cfg.container_tag));
    if created_dir {
        let _ = std::fs::remove_dir(&cfg.mount_path);
    }
    Ok(())
}

/// Collect `(vfs_path, real_path)` for every file under `dir`, WITHOUT reading
/// content. Content is read lazily at import time, one file at a time, so the
/// whole corpus is never buffered in memory at once — a non-empty mount of a
/// large container would otherwise hold every file's bytes simultaneously and
/// OOM (see `rcas/2026-06-01-semfs-prewarm-oom-import-collection.md`).
fn collect_file_paths_recursive(
    dir: &std::path::Path,
    root: &std::path::Path,
) -> Vec<(String, std::path::PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            // Skip vendored/dependency dirs. Their large minified bundles (e.g.
            // node_modules/*/dist/*.cjs) are distractor noise, not workspace
            // content, and the big ones stall the embedder — the deterministic
            // seed hang in rcas/2026-06-03-extract-uncapped-utf8-text-path-
            // node-modules-hang.md. See tickets/local-seed-coverage-gaps #1.
            if matches!(
                entry.file_name().to_str(),
                Some("node_modules" | ".git" | "target" | "__pycache__" | ".venv")
            ) {
                continue;
            }
            out.extend(collect_file_paths_recursive(&path, root));
        } else if ft.is_file() {
            // Skip the npm lockfile: it's a sibling of the (already-skipped)
            // node_modules tree, not workspace content, and its long dependency
            // listing pollutes the code lane on content queries (buries answer
            // files in RRF — rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-
            // pollution.md). Keep package.json so a generator task can still
            // `npm install`. See tickets/exclude-node-modules-from-wb-workspace.
            if entry.file_name().to_str() == Some("package-lock.json") {
                continue;
            }
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let vfs_path = format!("/{}", rel.to_string_lossy());
            if semfs_core::cache::is_macos_noise_path(&vfs_path) {
                continue;
            }
            out.push((vfs_path, path));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::collect_file_paths_recursive;
    use std::fs;

    /// The import collector must drop the vendored `node_modules` tree and the
    /// npm lockfile (corpus pollution) while keeping `package.json` and real
    /// workspace content. See tickets/exclude-node-modules-from-wb-workspace.
    #[test]
    fn collect_skips_node_modules_and_lockfile_keeps_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        fs::write(root.join("report.docx"), b"deliverable").unwrap();
        fs::write(root.join("package.json"), b"{}").unwrap();
        fs::write(root.join("package-lock.json"), b"{}").unwrap();
        let nm = root.join("node_modules").join("docx");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("index.js"), b"// dep").unwrap();

        let collected = collect_file_paths_recursive(root, root);
        let vfs: Vec<&str> = collected.iter().map(|(p, _)| p.as_str()).collect();

        assert!(vfs.contains(&"/report.docx"));
        assert!(vfs.contains(&"/package.json"));
        assert!(!vfs.iter().any(|p| p.contains("package-lock.json")));
        assert!(!vfs.iter().any(|p| p.contains("node_modules")));
    }
}
