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
use semfs_core::cache::{Db, CacheFs};
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
            // L7: attach the entity-graph extractor when an LLM is available.
            if let Some(llm) = crate::cmd::resolve::build_llm(env) {
                store = store.with_graph_extractor(Arc::new(llm));
                eprintln!("entity-graph extraction enabled (L7)");
            }
            let store = Arc::new(store);
            Ok((store.clone(), store))
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

    let opts = MountOpts::new(cfg.mount_path.clone(), cfg.backend).with_ownership(uid, gid);

    startup.report("validating_key", "validating API key")?;
    let session = if cfg.ephemeral {
        semfs_core::api::ApiClient::validate_key(&cfg.api_url, &cfg.api_key)
            .await
            .ok()
    } else {
        Some(
            semfs_core::api::ApiClient::validate_key(&cfg.api_url, &cfg.api_key)
                .await
                .context("validating API key (required to scope cache by org)")?,
        )
    };

    // SECURITY: `org_id` comes from the server's session response and gets joined
    // into cache paths (`cache_db_path`, and the pglite data dir) that `--clean`
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
        let org_id = session
            .as_ref()
            .and_then(|s| s.org_id.as_deref())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "server did not return org id; cannot open cache. Run `semfs login` and retry."
                )
            })?;
        startup.report("opening_cache", format!("opening cache for org {org_id}"))?;
        let db_path = semfs_core::config::cache_db_path(org_id, &cfg.container_tag);
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
        api.update_memory_paths(paths).await?;
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
    // Org scope for backends that persist per-org on local disk (embedded pglite),
    // mirroring the SQLite cache layout (`cache_db_path(org_id, tag)`). Ephemeral
    // mounts may have no validated org → a stable sentinel keeps same-tag ephemeral
    // mounts off the persistent per-org tree.
    let org_scope = session
        .as_ref()
        .and_then(|s| s.org_id.as_deref())
        .unwrap_or("_ephemeral");
    // `resolve_env` and `storage` were computed up front (before the marker write).
    use crate::cmd::resolve::StorageChoice;
    let explicit_backend = matches!(storage, StorageChoice::Pgvector | StorageChoice::Pglite);
    // An explicitly-selected external/embedded backend with the `hash` floor is a
    // contradiction (no semantic vectors). Fail closed with a clear message rather
    // than silently mounting with no usable index — caught BEFORE the indexing
    // gate, because `local_indexing_enabled()` is false for hash and would
    // otherwise skip the backend entirely.
    if let Some(name) = crate::cmd::resolve::explicit_backend_without_embedder(&resolve_env) {
        anyhow::bail!(
            "SEMFS_STORAGE_BACKEND={name} requires a real embedder, but \
             SEMFS_EMBED_BACKEND=hash provides no semantic vectors; set a local or \
             cloud embedder, or use the default SQLite backend"
        );
    }
    // Build the index when a real embedder is configured OR an external/embedded
    // backend was explicitly requested (the latter must not be skipped by the
    // hash short-circuit — that case already errored out above).
    let fs = if crate::cmd::resolve::local_indexing_enabled(&resolve_env) || explicit_backend {
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

    startup.report("warming_profile", "warming profile")?;
    fs.warm_profile().await;

    startup.report("initial_sync", "starting initial sync")?;
    let pull_succeeded = match semfs_core::sync::SyncEngine::initial_pull_with_progress(
        &fs,
        |progress| match progress {
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
        },
    )
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
                match fs.import_file_with_ownership(rel_path, &contents, uid, gid).await {
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
    let sync_tasks = semfs_core::sync::SyncEngine::start(fs.clone(), sync_opts, shutdown_rx.clone());

    // L7 background entity-graph worker: drains the indexer's extraction queue
    // with bounded concurrency so the per-file blocking LLM call never sits on
    // the synchronous index/flush path. No-op (exits immediately) when the
    // indexer has no graph extractor attached.
    if let Some(idx) = graph_indexer {
        let gw_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            semfs_core::cache::run_graph_worker(idx, gw_shutdown).await;
        });
    }

    startup.report("mounting_fs", "mounting filesystem")?;
    let handle = mount_fs(fs.clone(), opts).await?;

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
                StorageChoice::Pgvector | StorageChoice::Pglite => None,
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

    semfs_core::sync::SyncEngine::unmount_scan(&fs).await;

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
            out.extend(collect_file_paths_recursive(&path, root));
        } else if ft.is_file() {
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
