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

/// Build the local semantic indexer (SqliteVecStore + the resolved embedder)
/// over the daemon's cache db. Returned as `dyn LocalIndexer` for
/// `CacheFs::with_indexer`. Uses the same resolver as `grep`, so the indexer's
/// embedder matches the searcher's (same model, same dims).
fn build_local_indexer(
    db: Arc<Db>,
    env: &crate::cmd::resolve::ResolveEnv,
) -> anyhow::Result<Arc<dyn semfs_core::cache::LocalIndexer>> {
    let embedder = crate::cmd::resolve::build_embedder(env)?;
    let mut store = semfs_core::backend::SqliteVecStore::new(db, embedder)?;
    // L7: when an LLM is available, attach the entity-graph extractor so writes
    // populate file→entity edges.
    if let Some(llm) = crate::cmd::resolve::build_llm(env) {
        store = store.with_graph_extractor(Arc::new(llm));
        eprintln!("entity-graph extraction enabled (L7)");
    }
    Ok(Arc::new(store))
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

    let pre_existing_files = if cfg.import_existing && !created_dir {
        let files = collect_files_recursive(&cfg.mount_path, &cfg.mount_path);
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

    // Write the `.semfs` marker now that the db path is known (db_path is set
    // only for persistent, non-ephemeral caches).
    {
        use super::marker::{format_marker, parse_all_markers, SmfsMarker};
        let new_entry = SmfsMarker {
            tag: cfg.container_tag.clone(),
            api_url: cfg.api_url.clone(),
            mount_path: Some(cfg.mount_path.display().to_string()),
            db_path: local_db_path.clone(),
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
    let resolve_env = crate::cmd::resolve::ResolveEnv::from_env();
    let fs = if crate::cmd::resolve::local_indexing_enabled(&resolve_env) {
        match build_local_indexer(db.clone(), &resolve_env) {
            Ok(indexer) => {
                eprintln!("local semantic index enabled");
                Arc::new(fs_base.with_indexer(indexer))
            }
            Err(e) => {
                eprintln!("local index disabled: {e}");
                Arc::new(fs_base)
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

    if !pre_existing_files.is_empty() {
        if !pull_succeeded {
            eprintln!(
                "skipping auto-import of {} file(s): initial sync failed, \
                 cache cannot reliably detect duplicates. Remount when online to import.",
                pre_existing_files.len()
            );
        } else {
            let mut imported = 0usize;
            let mut skipped = 0usize;
            let mut errors = 0usize;
            for (rel_path, contents) in &pre_existing_files {
                match fs.import_file_with_ownership(rel_path, contents, uid, gid).await {
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
        started_at: Instant::now(),
        pull_enabled: !cfg.no_sync,
        user_id: session_user_id,
        user_name: session_user_name,
        org_name: session_org_name,
        shutdown_notify: ipc_shutdown_notify.clone(),
    });
    let socket_path = daemon::socket_path(&cfg.container_tag);
    let ipc_shutdown_rx = shutdown_rx.clone();
    let ipc_socket = socket_path.clone();
    let ipc_handle = tokio::spawn(async move {
        if let Err(e) = daemon::ipc::serve(state, ipc_socket, ipc_shutdown_rx).await {
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

fn collect_files_recursive(
    dir: &std::path::Path,
    root: &std::path::Path,
) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            out.extend(collect_files_recursive(&path, root));
        } else if ft.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let vfs_path = format!("/{}", rel.to_string_lossy());
            if semfs_core::cache::is_macos_noise_path(&vfs_path) {
                continue;
            }
            match std::fs::read(&path) {
                Ok(data) => out.push((vfs_path, data)),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unreadable file");
                }
            }
        }
    }
    out
}
