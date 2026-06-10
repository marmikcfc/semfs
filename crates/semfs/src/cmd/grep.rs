//! `semfs grep` — semantic search across a mounted container.

use anyhow::Result;
use clap::Args as ClapArgs;
use semfs_core::backend::{CloudIndex, SemanticIndex, SqliteVecStore};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Resolve the search backend — **config-driven, no flag, no network.**
///
/// Routing keys off the PERSISTED storage backend in the `.semfs` marker
/// (`StorageChoice::is_local()`), never this process's embedder env. A local
/// backend (sqlite/pgvector/pglite) uses the container's LOCAL index (full L1–L5:
/// resolved embedder + reranker, read-only via `open_existing`) when the daemon
/// recorded a usable `db_path`; the `cloud` backend (and any unusable local index)
/// falls back to `CloudIndex`. No `validate_key`, so a local search needs neither
/// credentials nor connectivity.
///
/// Returns `(index, used_local)`. `used_local` lets the caller retry through the
/// cloud if a local query fails at runtime (e.g. a cloud-backed query embedder
/// hits a provider outage) — a degraded-dependency state that should fall back,
/// not abort the command.
async fn resolve_index(
    db_path: Option<&str>,
    backend: Option<&str>,
    api_url: &str,
    key: Option<&str>,
    tag: &str,
) -> Result<(Arc<dyn SemanticIndex>, bool)> {
    use super::resolve::StorageChoice;
    // Decide the backend from the PERSISTED marker (how the daemon actually
    // mounted), NOT this process's env — env can drift between mount and grep, and
    // defaulting a pglite mount to SQLite here would reopen a stale on-disk vec
    // index as authoritative. `env` is still used only to BUILD the resolved store.
    let choice = super::resolve::storage_choice_from(backend);
    let env = super::resolve::ResolveEnv::from_env();

    // pglite is DAEMON-OWNED with no direct path (single connection, embedded in
    // the daemon). Reaching here means the daemon is UNREACHABLE — so there is no
    // valid local route, and cloud search would omit unsynced daemon-local writes.
    // Fail CLOSED (not cloud, not the SQLite db_path) regardless of this process's
    // embedder env, mirroring the daemon-`Failed` policy: a pglite container can
    // only be searched through its daemon. Checked before the embedder gate so a
    // grep without a configured embedder can't slip through to cloud.
    if choice == StorageChoice::Pglite {
        anyhow::bail!(
            "pglite mount '{tag}' is daemon-owned and its daemon is not reachable; its index \
             lives only in the daemon (no direct path), and cloud search would omit unsynced \
             local writes. Re-mount the container to search it."
        );
    }

    // Route on the mounted STORAGE backend (from the marker), not the embedder env:
    // a local backend builds/searches a local index, `cloud` goes straight to the
    // cloud index. `env` below only BUILDS the resolved local store/embedder.
    if choice.is_local() {
        match choice {
            // Postgres/pgvector storage backend (opt-in via SEMFS_STORAGE_BACKEND).
            // Connects directly to Postgres — no local cache db. Fail-open to cloud.
            StorageChoice::Pgvector => {
                #[cfg(feature = "pg")]
                {
                    let embedder = super::resolve::build_embedder(&env)?;
                    match super::resolve::build_pg_store(&env, tag, embedder).await {
                        // Gate on readiness, mirroring SQLite's is_searchable(): an
                        // empty Postgres index would return Ok([]) (a false "no
                        // results") and bypass the Err-only cloud retry — so fall
                        // back to cloud unless the container actually has rows.
                        Ok(store) if store.is_searchable().await => {
                            return Ok((Arc::new(store), true))
                        }
                        Ok(_) => tracing::warn!(
                            "pgvector index for '{tag}' is empty/unready; falling back to cloud"
                        ),
                        Err(e) => tracing::warn!(
                            "pgvector backend unavailable ({e}); falling back to cloud search"
                        ),
                    }
                }
                #[cfg(not(feature = "pg"))]
                tracing::warn!(
                    "SEMFS_STORAGE_BACKEND=pgvector but this binary was built without the `pg` \
                     feature; falling back to cloud search"
                );
            }
            StorageChoice::Sqlite => {
                if let Some(p) = db_path.filter(|p| std::path::Path::new(p).exists()) {
                    // Default SQLite. Degraded-dependency states (corrupt cache,
                    // stale model dir) fall back to cloud, not abort grep — local
                    // init errors are logged and we continue rather than `?`.
                    match build_local_store(&env, p) {
                        Ok(Some(store)) => return Ok((Arc::new(store), true)),
                        Ok(None) => tracing::warn!(
                            "local index at {p} is missing or model-incompatible; \
                             falling back to cloud search"
                        ),
                        Err(e) => tracing::warn!(
                            "local backend init failed for {p} ({e}); falling back to cloud search"
                        ),
                    }
                }
            }
            // Handled above (fail-closed) before the storage gate.
            StorageChoice::Pglite => unreachable!("pglite returns early above"),
            // Cloud is not local — excluded by the `choice.is_local()` gate.
            StorageChoice::Cloud => unreachable!("cloud is not local; gated out above"),
        }
    }
    // Cloud fallback (Supermemory) — requires a key.
    Ok((Arc::new(cloud_index(api_url, key, tag)?), false))
}

/// Build the Supermemory-backed cloud search index. Requires an API key.
fn cloud_index(api_url: &str, key: Option<&str>, tag: &str) -> Result<CloudIndex> {
    let key = key.ok_or_else(|| {
        anyhow::anyhow!(
            "no local index for this container and no API key for cloud search \
             (configure a local embedder, or run `semfs login`)"
        )
    })?;
    let api = Arc::new(semfs_core::api::ApiClient::new(api_url, key, tag));
    Ok(CloudIndex::new(api))
}

/// Outcome of asking the running mount daemon to search (the primary path: the
/// daemon owns the index connection, so this works for every backend — SQLite,
/// embedded pglite, external Postgres).
enum DaemonSearch {
    /// Daemon answered from its local index (hits may be empty = genuine miss).
    Hits(Vec<semfs_core::backend::SearchHit>),
    /// Daemon is up but has no usable local index → caller should try cloud.
    /// Carries the daemon's authoritative backend (for the fail-closed decision).
    NoIndex { backend: Option<String> },
    /// Daemon is up and OWNS the index, but the search itself errored (backend
    /// fault, embedder outage, timeout). The caller must NOT silently re-resolve
    /// a different backend: for pglite there's no direct path, so falling back
    /// would return stale cloud results that omit unsynced local writes and mask
    /// the real fault. Carries the daemon's authoritative backend (from the SAME
    /// response — no separate Status RPC to race) so the policy is decided right.
    Failed { message: String, backend: Option<String> },
    /// No daemon reachable for this tag → caller falls back to direct/cloud.
    Unreachable,
}

/// Ask the daemon (if running) to run the search over its owned index.
async fn daemon_search(tag: &str, query: &str, filepath: Option<&str>) -> DaemonSearch {
    use semfs_core::daemon::client::SendError;
    use semfs_core::daemon::protocol::{Request, Response};
    let req = Request::Search {
        query: query.to_string(),
        filepath: filepath.map(|s| s.to_string()),
    };
    match semfs_core::daemon::client::send_request_classified(tag, req).await {
        Ok(Response::SearchHits { hits, searchable: true, .. }) => DaemonSearch::Hits(hits),
        Ok(Response::SearchHits { searchable: false, backend, .. }) => {
            DaemonSearch::NoIndex { backend }
        }
        // Genuine search fault from a daemon that DID understand the request. The
        // backend rides on this same response, so the fail-closed decision needs
        // no separate (race-prone) Status lookup.
        Ok(Response::SearchError { message, backend }) => DaemonSearch::Failed { message, backend },
        // Version skew: an OLDER daemon (pre-IPC-search) can't deserialize the new
        // `Search` request, so its handler replies `invalid request: <serde err>`
        // (see daemon::ipc::handle_conn). That's not a search fault — the daemon
        // simply doesn't speak this request. Fall back to the direct backend (what
        // grep did before IPC search existed) so a new CLI still works against an
        // already-running old daemon during a rolling upgrade.
        Ok(Response::Error { message }) if message.starts_with("invalid request") => {
            tracing::debug!(
                "daemon does not understand the Search request ({message}); \
                 falling back to direct search (likely an older daemon)"
            );
            DaemonSearch::Unreachable
        }
        // Any other generic error / wrong-type response from a live daemon is a
        // protocol fault — surface it (no backend carried → policy uses the marker).
        Ok(Response::Error { message }) => DaemonSearch::Failed { message, backend: None },
        Ok(other) => DaemonSearch::Failed {
            message: format!("unexpected daemon response: {other:?}"),
            backend: None,
        },
        // Only a genuinely ABSENT daemon (no socket / connect refused/timeout)
        // falls back to a directly-resolved backend.
        Err(SendError::Unreachable(_)) => DaemonSearch::Unreachable,
        // Daemon was reachable but the exchange failed mid-flight (read timeout,
        // disconnect, malformed reply) — a daemon-side fault. Surface it.
        Err(SendError::PostConnect(e)) => DaemonSearch::Failed {
            message: format!("daemon transport error: {e}"),
            backend: None,
        },
    }
}

/// Build the local SQLite store for `grep`, or `Ok(None)` if the on-disk index
/// isn't usable (missing tables / incompatible model). Hard init failures
/// (cache open, embedder build) return `Err` so the caller falls back to cloud;
/// reranker construction failure is non-fatal — we search without reranking.
fn build_local_store(
    env: &super::resolve::ResolveEnv,
    p: &str,
) -> Result<Option<SqliteVecStore>> {
    let db = Arc::new(semfs_core::cache::Db::open(Path::new(p))?);
    let embedder = super::resolve::build_embedder(env)?;
    let mut store = SqliteVecStore::open_existing(db.clone(), embedder);
    // Reader path: only bother with the code embedder if the cache advertises a
    // code lane (a text-only cache needs no code model). FAIL-OPEN: if the code
    // model can't be built, log and search the text lane only — the code lane is
    // additive, never a precondition for local search. When attached, `search`
    // queries the code lane only if its identity matches the stamp.
    if db.has_code_lane() {
        match super::resolve::build_code_embedder(env) {
            Ok(Some(code)) => store = store.with_code_embedder(code),
            Ok(None) => {}
            Err(e) => tracing::warn!(
                "code lane advertised but code embedder unavailable ({e}); \
                 searching text lane only"
            ),
        }
    }
    match super::resolve::build_reranker(env) {
        Ok(Some(reranker)) => store = store.with_reranker(reranker),
        Ok(None) => {}
        Err(e) => tracing::warn!("local reranker unavailable ({e}); searching without rerank"),
    }
    Ok(store.is_searchable().then_some(store))
}

const DEFAULT_API_URL: &str = "https://api.supermemory.ai";

/// Resolve `(tag, api_url)` by precedence: explicit `--tag` > the `.semfs` marker
/// in CWD > the `.semfs` marker at the path argument. The path-argument marker is
/// what lets `grep "<q>" /path/to/mount/` work from *outside* the mount. Markers
/// are passed as `(tag, api_url)` tuples so this is unit-testable without I/O.
fn resolve_tag_url(
    explicit_tag: Option<&str>,
    explicit_api_url: Option<&str>,
    cwd_marker: Option<(&str, &str)>,
    path_marker: Option<(&str, &str)>,
) -> Result<(String, String)> {
    if let Some(tag) = explicit_tag {
        return Ok((
            tag.to_string(),
            explicit_api_url.unwrap_or(DEFAULT_API_URL).to_string(),
        ));
    }
    if let Some((tag, url)) = cwd_marker {
        return Ok((tag.to_string(), explicit_api_url.unwrap_or(url).to_string()));
    }
    if let Some((tag, url)) = path_marker {
        return Ok((tag.to_string(), explicit_api_url.unwrap_or(url).to_string()));
    }
    anyhow::bail!(
        "No container tag found. Either run from inside a mounted directory, pass --tag, \
         or give a path inside a mounted directory."
    )
}

#[cfg(test)]
mod resolve_tests {
    use super::resolve_tag_url;

    #[test]
    fn explicit_tag_wins_over_markers() {
        let (t, u) = resolve_tag_url(
            Some("x"),
            None,
            Some(("c", "http://cwd")),
            Some(("p", "http://path")),
        )
        .unwrap();
        assert_eq!(t, "x");
        assert_eq!(u, "https://api.supermemory.ai");
    }

    #[test]
    fn cwd_marker_used_when_no_explicit_tag() {
        let (t, u) = resolve_tag_url(
            None,
            None,
            Some(("c", "http://cwd")),
            Some(("p", "http://path")),
        )
        .unwrap();
        assert_eq!(t, "c");
        assert_eq!(u, "http://cwd");
    }

    #[test]
    fn path_marker_used_when_no_cwd_marker() {
        let (t, u) = resolve_tag_url(None, None, None, Some(("p", "http://path"))).unwrap();
        assert_eq!(t, "p");
        assert_eq!(u, "http://path");
    }

    #[test]
    fn explicit_api_url_overrides_marker_url() {
        let (_t, u) =
            resolve_tag_url(None, Some("http://flag"), Some(("c", "http://cwd")), None).unwrap();
        assert_eq!(u, "http://flag");
    }

    #[test]
    fn errors_when_nothing_resolves() {
        assert!(resolve_tag_url(None, None, None, None).is_err());
    }
}

/// Bound on a single line-range file read off the FUSE mount. The line range is
/// a display nicety — the agent needs `<file>:<chunk>` — but the daemon serves
/// BOTH the IPC search and the FUSE reads, so under CPU contention a `read()` to
/// it can block indefinitely. Output formatting must never hang on the mount it
/// just searched (RCA 2026-06-04-semfs-grep-hangs-post-search-under-load).
const LINE_RANGE_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// A matched file whose entire content is at or under this size is printed in
/// full (instead of just the one matched chunk) and marked COMPLETE FILE. Small
/// enough that inlining costs only a few hundred tokens, which is far cheaper
/// than the re-greps an agent does when it distrusts a partial excerpt.
const SMALL_FILE_INLINE_BYTES: usize = 8 * 1024;

/// Outcome of a bounded read of one path off the mount.
enum ReadOutcome {
    /// Read succeeded.
    Content(String),
    /// Read completed but the file isn't present (real path nor sidecar).
    Missing,
    /// The read did not return within the budget — the mount is unresponsive.
    TimedOut,
}

/// Read `path` to a string with a hard timeout. The blocking `read_to_string`
/// runs on a throwaway thread; if it doesn't finish in `budget` we return
/// `TimedOut` and abandon the thread — a hung FUSE read can't be cancelled, but a
/// short-lived CLI leaking one blocked thread (reaped at process exit) is far
/// better than hanging the whole grep forever.
fn read_file_timed(path: PathBuf, budget: Duration) -> ReadOutcome {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(std::fs::read_to_string(&path).ok());
    });
    match rx.recv_timeout(budget) {
        Ok(Some(c)) => ReadOutcome::Content(c),
        Ok(None) => ReadOutcome::Missing,
        Err(_) => ReadOutcome::TimedOut,
    }
}

/// Read a hit's content (real file, else a transcription sidecar) for line-range
/// computation, with each read time-bounded. A timeout on the primary read
/// short-circuits — we don't then pay the budget on five more sidecar reads.
/// Binary file types whose extracted text is inlined from the `chunks` table
/// at grep time, so the agent never needs to open or parse the binary.
fn is_binary_ext(filepath: &str) -> bool {
    let ext = filepath
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "xlsx" | "xls" | "pdf" | "docx" | "pptx" | "ppt" | "doc"
    )
}

fn read_local_or_sidecar(mount: &Path, filepath: &str) -> ReadOutcome {
    let stripped = filepath.trim_start_matches('/');
    match read_file_timed(mount.join(stripped), LINE_RANGE_READ_TIMEOUT) {
        ReadOutcome::Content(c) => return ReadOutcome::Content(c),
        ReadOutcome::TimedOut => return ReadOutcome::TimedOut,
        ReadOutcome::Missing => {}
    }
    for suffix in &[
        ".pdf-transcription.md",
        ".image-transcription.md",
        ".video-transcription.md",
        ".audio-transcription.md",
        ".webpage-transcription.md",
    ] {
        match read_file_timed(
            mount.join(format!("{stripped}{suffix}")),
            LINE_RANGE_READ_TIMEOUT,
        ) {
            ReadOutcome::Content(c) => return ReadOutcome::Content(c),
            ReadOutcome::TimedOut => return ReadOutcome::TimedOut,
            ReadOutcome::Missing => {}
        }
    }
    ReadOutcome::Missing
}

/// Decide how to present a hit's excerpt, given the file's full `content` and the
/// matched `chunk`. Returns `(complete_file, inline_full)`:
/// - the chunk already spans the whole file → `(true, None)` (print the chunk, mark COMPLETE);
/// - the file is small but chunked → `(true, Some(full))` (print the whole file, mark COMPLETE);
/// - otherwise → `(false, None)` (print the partial chunk, no marker).
///
/// Inlining a small file is the case-289 trust fix: a chunked-but-tiny answer
/// file (e.g. 908 B, >200 words) can never earn the COMPLETE marker from a single
/// partial window, so the agent distrusts the excerpt and re-greps 4–9×. Printing
/// it whole keeps the marker truthful and lets the agent answer in one grep.
fn present_excerpt(content: &str, chunk: &str) -> (bool, Option<String>) {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let fc = norm(content);
    if !fc.is_empty() && norm(chunk).contains(&fc) {
        (true, None)
    } else if !content.is_empty() && content.len() <= SMALL_FILE_INLINE_BYTES {
        (true, Some(content.to_string()))
    } else {
        (false, None)
    }
}

fn line_range_in_file(file_content: &str, chunk: &str) -> Option<(usize, usize)> {
    if chunk.is_empty() {
        return None;
    }

    if let Some(pos) = file_content.find(chunk) {
        let start = file_content[..pos].matches('\n').count() + 1;
        let last_char_len = chunk.chars().next_back()?.len_utf8();
        let last_char_start = pos + chunk.len() - last_char_len;
        let end = file_content[..last_char_start].matches('\n').count() + 1;
        return Some((start, end));
    }

    let norm = |s: &str| -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") };
    let normed_file = norm(file_content);
    let normed_chunk = norm(chunk);
    if normed_chunk.is_empty() {
        return None;
    }
    let norm_pos_byte = normed_file.find(&normed_chunk)?;
    let target_start = normed_file[..norm_pos_byte].chars().count();
    let normed_chunk_chars = normed_chunk.chars().count();
    let target_end_inclusive = target_start + normed_chunk_chars - 1;

    let mut orig_start_byte: Option<usize> = None;
    let mut orig_end_byte: Option<usize> = None;
    let mut norm_idx: usize = 0;
    let mut need_separator = false;
    for (i, ch) in file_content.char_indices() {
        if ch.is_whitespace() {
            if norm_idx > 0 {
                need_separator = true;
            }
            continue;
        }
        if need_separator {
            norm_idx += 1;
            need_separator = false;
        }
        if norm_idx == target_start && orig_start_byte.is_none() {
            orig_start_byte = Some(i);
        }
        if norm_idx == target_end_inclusive {
            orig_end_byte = Some(i);
            break;
        }
        norm_idx += 1;
    }

    let start_byte = orig_start_byte?;
    let end_byte = orig_end_byte?;
    let start = file_content[..start_byte].matches('\n').count() + 1;
    let end = file_content[..end_byte].matches('\n').count() + 1;
    Some((start, end))
}

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Search query.
    pub query: String,

    /// Directory path to scope the search (optional).
    pub path: Option<String>,

    /// Container tag (auto-detected from .semfs marker if not given).
    #[arg(long)]
    pub tag: Option<String>,

    /// Supermemory API key (resolved from stored credentials if omitted).
    #[arg(long)]
    pub key: Option<String>,

    /// Override the Supermemory API base URL.
    #[arg(long, env = "SUPERMEMORY_API_URL")]
    pub api_url: Option<String>,

    /// L4: rewrite/expand the query with an LLM (OpenRouter gpt-4.1-nano) before
    /// searching. Opt-in; falls back to the original query if the LLM is
    /// unavailable or errors.
    #[arg(long)]
    pub rewrite: bool,
}

pub async fn run(args: Args) -> Result<()> {
    use super::marker::read_semfs_marker;

    if args.query.trim().is_empty() {
        eprintln!("# supermemory semantic search — provide a query");
        eprintln!(
            "# inside a mounted container, `grep` without flags is powered by semantic search."
        );
        eprintln!("# usage:");
        eprintln!("#   grep \"natural language query\"         search by meaning, all files");
        eprintln!("#   grep \"query\" path/to/dir/             scope to a directory");
        return Ok(());
    }

    let marker = read_semfs_marker();

    // Marker sitting at the path argument — lets grep resolve the tag (and mount
    // path, below) when run from OUTSIDE the mount, where CWD has no marker.
    let path_marker = args.path.as_deref().and_then(|p| {
        let mut dir = if p.starts_with('/') {
            std::path::PathBuf::from(p)
        } else {
            std::env::current_dir().ok()?.join(p)
        };
        // Climb to the nearest existing ancestor so a not-yet-created subpath
        // (e.g. grepping a directory that doesn't exist on disk yet) still
        // resolves the mount's marker instead of failing to canonicalize.
        while !dir.exists() {
            if !dir.pop() {
                return None;
            }
        }
        let dir = dir.canonicalize().ok()?;
        let dir = if dir.is_dir() {
            dir
        } else {
            dir.parent()?.to_path_buf()
        };
        super::marker::read_semfs_marker_for_path(&dir)
    });

    // Resolve container tag + API URL (precedence: --tag > CWD marker > path marker).
    let (tag, api_url) = resolve_tag_url(
        args.tag.as_deref(),
        args.api_url.as_deref(),
        marker.as_ref().map(|m| (m.tag.as_str(), m.api_url.as_str())),
        path_marker.as_ref().map(|m| (m.tag.as_str(), m.api_url.as_str())),
    )?;

    // Bind ALL fallback metadata to the RESOLVED tag, not to CWD/path marker
    // precedence. With an explicit `--tag`, the CWD/path markers may describe a
    // DIFFERENT container; borrowing their mount_path/db_path/backend would let
    // `grep --tag <other>` reopen THIS mount's SQLite cache (cross-container stale
    // results) or use the wrong project credentials. Pick the one marker entry
    // whose tag matches; if none matches the explicit tag, use no local metadata
    // (fall through to the daemon, then cloud) rather than an unrelated mount's.
    let meta = marker
        .as_ref()
        .filter(|m| m.tag == tag)
        .or_else(|| path_marker.as_ref().filter(|m| m.tag == tag));

    // mount_path drives project-scoped credential lookup + local line-range reads.
    let mount_path = meta
        .and_then(|m| m.mount_path.as_deref())
        .map(std::path::Path::new);
    // Key is only needed for the cloud fallback; a local search needs none.
    let key = super::auth::resolve_api_key(args.key.as_deref(), mount_path).ok();

    // Local cache db path from the SAME (tag-matched) marker — opening it needs no
    // network. Absent for non-SQLite mounts (they don't store vectors there).
    let db_path = meta.and_then(|m| m.db_path.as_deref());

    // Storage backend from the tag-matched marker. This is the fallback source for
    // the daemon-UNREACHABLE path (resolve_index). On the daemon-REACHABLE failure
    // paths (NoIndex/Failed) the daemon carries its AUTHORITATIVE backend in the
    // same response, which takes precedence (see below) — so no separate Status RPC
    // is needed and a flaky side-channel can't erase pglite's fail-closed policy.
    let backend = meta.and_then(|m| m.backend.as_deref()).map(String::from);

    // Determine filepath prefix from path arg, stripping the mount path if present.
    // `mount_path` already falls back to the path-argument marker above, so this
    // canonicalizes whichever mount we resolved (CWD or path marker).
    let canonical_mount = mount_path.and_then(|mp| mp.canonicalize().ok());

    let filepath = args.path.as_deref().and_then(|p| {
        let raw = if p.starts_with('/') {
            Path::new(p).to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(p))
                .unwrap_or_else(|_| Path::new(p).to_path_buf())
        };
        let abs = raw
            .canonicalize()
            .unwrap_or(raw)
            .to_string_lossy()
            .into_owned();

        let relative = if let Some(ref cm) = canonical_mount {
            let cm_str = cm.to_string_lossy();
            abs.strip_prefix(cm_str.as_ref())
                .map(|s| s.to_string())
                .unwrap_or(abs)
        } else {
            abs
        };

        if relative.is_empty() || relative == "/" {
            return None;
        }

        let relative = if relative.starts_with('/') {
            relative
        } else {
            format!("/{relative}")
        };

        let relative = if !relative.ends_with('/') && Path::new(&relative).extension().is_none() {
            format!("{relative}/")
        } else {
            relative
        };

        Some(relative)
    });

    // L4: optional LLM query rewrite (opt-in via --rewrite, or env SEMFS_REWRITE for
    // callers that can't pass the flag — e.g. an agent invoking plain `semfs grep`;
    // fail-open to original). Cross-lingual corpora benefit most: the rewrite appends
    // target-language terms so a same-language dense/lexical match becomes possible.
    let rewrite_enabled = args.rewrite
        || std::env::var("SEMFS_REWRITE")
            .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
    let effective_query = if rewrite_enabled {
        let env = super::resolve::ResolveEnv::from_env();
        match super::resolve::build_llm(&env) {
            Some(llm) => match semfs_core::llm::rewrite_query(&llm, &args.query) {
                Ok(q) => {
                    eprintln!("# rewritten query: {q:?}");
                    q
                }
                Err(e) => {
                    eprintln!("# query rewrite failed ({e}); using original");
                    args.query.clone()
                }
            },
            None => {
                eprintln!("# --rewrite needs OPENROUTER_API_KEY; using original query");
                args.query.clone()
            }
        }
    } else {
        args.query.clone()
    };

    // PRIMARY PATH: ask the running mount daemon to search over its own index.
    // The daemon is the sole owner of the backend connection, so this is the one
    // path that works for EVERY backend (SQLite, embedded pglite, external
    // Postgres) — grep never opens its own DB connection. Falls back only when no
    // daemon is reachable (e.g. grepping a persisted cache after unmount).
    let hits = match daemon_search(&tag, &effective_query, filepath.as_deref()).await {
        DaemonSearch::Hits(hits) => hits,
        DaemonSearch::NoIndex { backend: resp_backend } => {
            // Daemon up but reports no usable local index. For sqlite/hash that's
            // the cloud path. A pglite daemon can never report this (an index-build
            // failure is mount-fatal), but guard anyway: prefer the daemon's
            // authoritative backend, and fail closed if it somehow says pglite.
            use super::resolve::{storage_choice_from, StorageChoice};
            let effective = resp_backend.as_deref().or(backend.as_deref());
            if storage_choice_from(effective) == StorageChoice::Pglite {
                return Err(anyhow::anyhow!(
                    "pglite daemon for '{tag}' reports no usable index; not falling back to \
                     cloud (would omit local writes). Re-mount the container."
                ));
            }
            cloud_index(&api_url, key.as_deref(), &tag)?
                .search(&effective_query, filepath.as_deref())
                .await?
        }
        DaemonSearch::Failed { message, backend: resp_backend } => {
            // The daemon was REACHABLE and the search FAILED. Cloud fallback is
            // allowed ONLY with POSITIVE evidence of a cloud-safe backend
            // (sqlite/pgvector) plus a key. Anything else fails closed:
            //  - pglite is DAEMON-ONLY → cloud would omit unsynced local writes.
            //  - UNKNOWN backend (None) — e.g. a transport fault (timeout/
            //    disconnect/malformed reply) carries no backend, and there's no
            //    tag-matched marker — must NOT default to sqlite/cloud, because the
            //    reachable daemon could be pglite. Defaulting unknown to cloud is
            //    exactly the stale-result trap; surface it instead.
            // Backend is taken from the SAME response when present (resp_backend),
            // else the trusted tag-matched marker — never a second RPC.
            use super::resolve::{storage_choice_from, StorageChoice};
            let effective = resp_backend.as_deref().or(backend.as_deref());
            let cloud_safe = matches!(
                effective.map(|b| storage_choice_from(Some(b))),
                Some(StorageChoice::Sqlite)
                    | Some(StorageChoice::Pgvector)
                    | Some(StorageChoice::Cloud)
            );
            if !cloud_safe || key.is_none() {
                return Err(anyhow::anyhow!(
                    "search failed on the mount daemon for '{tag}': {message}\n\
                     (not falling back to a different backend, which could return stale \
                     results — the backend is daemon-only/unknown and/or no API key is set)"
                ));
            }
            tracing::warn!(
                "daemon search failed ({message}); falling back to cloud search \
                 (sqlite/pgvector degraded-dependency path)"
            );
            cloud_index(&api_url, key.as_deref(), &tag)?
                .search(&effective_query, filepath.as_deref())
                .await?
        }
        DaemonSearch::Unreachable => {
            // No daemon: resolve a backend directly (SQLite file / external
            // Postgres / cloud). pglite has no direct path and FAILS CLOSED inside
            // resolve_index (its index lives only in the daemon; cloud would be
            // stale) — so an un-mounted pglite container errors here, telling the
            // user to remount, rather than silently returning cloud results.
            let (index, used_local) =
                resolve_index(db_path, backend.as_deref(), &api_url, key.as_deref(), &tag).await?;
            match index.search(&effective_query, filepath.as_deref()).await {
                Ok(hits) => hits,
                Err(e) if used_local && key.is_some() => {
                    tracing::warn!("local search failed ({e}); falling back to cloud search");
                    cloud_index(&api_url, key.as_deref(), &tag)?
                        .search(&effective_query, filepath.as_deref())
                        .await?
                }
                Err(e) => return Err(e),
            }
        }
    };

    if hits.is_empty() {
        eprintln!(
            "# supermemory semantic search — no results for {:?}",
            args.query
        );
        eprintln!("# this searches by meaning, not exact text. try a natural language query.");
        return Ok(());
    }

    // Header: tells LLMs and users what this output is and how to use it.
    eprintln!(
        "# supermemory semantic search — {} results for {:?}",
        hits.len(),
        args.query
    );
    eprintln!("# searches by meaning across files in this container. usage:");
    eprintln!("#   grep \"2-4 key terms\"                    short focused queries rank best");
    eprintln!("#   grep \"query\" path/to/dir/              search within directory");
    eprintln!("# output: <filepath>:<line_start>-<line_end>:<chunk>  — RANKED BY RELEVANCE (top = best match)");
    eprintln!(
        "# chunk text is verbatim from the file (the matched line range is shown above)."
    );
    eprintln!();

    // Open the DB once for binary-file inline: binary hits (xlsx/pdf/docx/etc.)
    // are served from the `chunks` table instead of reading raw bytes off FUSE.
    // This eliminates the format trap (agent invoking openpyxl/libreoffice to
    // parse binaries) — the extracted text is already present in `chunks`.
    //
    // Knob: `SEMFS_GREP_INLINE=off` disables inlining (leaves `grep_db` None, so
    // the binary block below falls through to the normal chunk-excerpt path).
    // Use this when `.extracted.md` siblings deliver the text instead, so the
    // agent `cat`s a few lines on demand rather than receiving the whole file in
    // every grep result. Default is on (inline).
    let grep_inline_enabled = std::env::var("SEMFS_GREP_INLINE")
        .map(|v| !matches!(v.as_str(), "off" | "0" | "false"))
        .unwrap_or(true);
    let grep_db: Option<semfs_core::cache::Db> = if grep_inline_enabled {
        db_path.and_then(|p| semfs_core::cache::Db::open(std::path::Path::new(p)).ok())
    } else {
        None
    };

    let mut file_cache: HashMap<String, ReadOutcome> = HashMap::new();
    // Circuit breaker: once a line-range read times out (mount starved under the
    // search load), stop reading and print <file>:<chunk> for all remaining hits —
    // so the whole grep pays at most one timeout, never hangs.
    let mut mount_reads_ok = true;

    for (i, result) in hits.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let fp = result.filepath.as_deref().unwrap_or("(unknown)");

        if let Some(memory) = result.memory.as_deref() {
            let escaped = memory
                .replace('\\', "\\\\")
                .replace('\n', "\\n")
                .replace('\r', "\\r");
            println!("{}:{}", fp, escaped);
            continue;
        }

        let chunk = result.chunk.as_deref().unwrap_or("");

        // A semfs annotation chunk (e.g. the HTTP-error-page notice from the
        // backend) is authoritative — print it verbatim and do NOT read/inline
        // the underlying file (which is the corrupt page the annotation warns
        // about). This is how a "source inaccessible (403)" signal reaches the
        // agent so it can report the error instead of parsing garbage.
        if chunk.starts_with("[semfs:") {
            let escaped = chunk
                .replace('\\', "\\\\")
                .replace('\n', "\\n")
                .replace('\r', "\\r");
            println!("{}:{}", fp, escaped);
            continue;
        }

        // Binary inline: serve the full extracted text from `chunks` so the agent
        // never opens or parses the binary. Skips the FUSE read entirely.
        if is_binary_ext(fp) {
            if let Some(text) = grep_db.as_ref().and_then(|db| db.get_extracted_text(fp)) {
                let lines = text.lines().count().max(1);
                let escaped = text
                    .replace('\\', "\\\\")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r");
                println!("{}:1-{}:{}", fp, lines, escaped);
                println!(
                    "# ^ COMPLETE FILE — excerpt above is this file's entire content; use it directly, do not open it."
                );
                continue;
            }
        }

        // Whether the excerpt we print IS the file's entire content — then it is
        // authoritative and the agent should use it directly instead of opening
        // the file (the case-289 "trust fix": codex distrusting a partial excerpt
        // → opening the file → format-trap was the token sink).
        let mut complete_file = false;
        // When the whole file is small enough to inline cheaply, we print the
        // ENTIRE file instead of the one matched chunk. A small data file (e.g.
        // case-289's 908 B answer list) is split into >1 overlapping word-window
        // when it exceeds 200 words, so a single returned chunk is partial and
        // could never earn the COMPLETE marker — yet inlining the whole file
        // costs only a few hundred tokens and lets the agent answer in one grep
        // (vs the 4–9 re-greps codex does when it distrusts a partial excerpt).
        let mut inline_full: Option<String> = None;
        let line_range = if mount_reads_ok {
            canonical_mount
                .as_ref()
                .zip(result.filepath.as_deref())
                .and_then(|(cm, path)| {
                    let outcome = file_cache
                        .entry(path.to_string())
                        .or_insert_with(|| read_local_or_sidecar(cm, path));
                    match &*outcome {
                        ReadOutcome::Content(content) => {
                            let (cf, full) = present_excerpt(content, chunk);
                            complete_file = cf;
                            if let Some(f) = full {
                                // File is small but chunked — print it whole so the
                                // COMPLETE marker stays truthful. Line range is 1..N.
                                let lines = f.lines().count().max(1);
                                inline_full = Some(f);
                                return Some((1, lines));
                            }
                            line_range_in_file(content, chunk)
                        }
                        ReadOutcome::Missing => None,
                        ReadOutcome::TimedOut => {
                            // Mount is starved — stop reading; emit chunk-only from here.
                            mount_reads_ok = false;
                            eprintln!(
                                "# mount slow under load; printing <file>:<chunk> \
                                 without line ranges"
                            );
                            None
                        }
                    }
                })
        } else {
            None
        };

        // Print the full file when promoted to inline; otherwise the matched chunk.
        let escaped = inline_full
            .as_deref()
            .unwrap_or(chunk)
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r");

        if let Some((start, end)) = line_range {
            if start == end {
                println!("{}:{}:{}", fp, start, escaped);
            } else {
                println!("{}:{}-{}:{}", fp, start, end, escaped);
            }
        } else {
            println!("{}:{}", fp, escaped);
        }
        if complete_file {
            // Parse-safe comment: tells the agent the excerpt is the whole file —
            // copy it directly; no need to open the file or crawl to verify.
            println!(
                "# ^ COMPLETE FILE — excerpt above is this file's entire content; use it directly, do not open it."
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        line_range_in_file, present_excerpt, read_file_timed, ReadOutcome,
        SMALL_FILE_INLINE_BYTES,
    };
    use std::time::Duration;

    #[test]
    fn present_excerpt_marks_complete_when_chunk_covers_file() {
        let content = "alpha beta gamma";
        // Chunk contains the whole file (plus possible surrounding whitespace).
        let (complete, full) = present_excerpt(content, "alpha beta gamma");
        assert!(complete);
        assert!(full.is_none(), "no inline needed when chunk already covers");
    }

    #[test]
    fn present_excerpt_inlines_small_chunked_file() {
        // A small file whose returned chunk is only a partial window: the marker
        // can't fire from the chunk, so we promote to the full file content.
        let content = "row1 data\nrow2 data\nrow3 ANSWER\nrow4 data";
        let partial_chunk = "row1 data\nrow2 data"; // does not contain row3
        let (complete, full) = present_excerpt(content, partial_chunk);
        assert!(complete, "small chunked file must be marked complete");
        assert_eq!(full.as_deref(), Some(content), "must inline the whole file");
    }

    #[test]
    fn present_excerpt_leaves_large_file_as_partial() {
        // A file just over the inline threshold stays chunk-only (no false COMPLETE).
        let content = "x".repeat(SMALL_FILE_INLINE_BYTES + 1);
        let partial_chunk = "x".repeat(100);
        let (complete, full) = present_excerpt(&content, &partial_chunk);
        assert!(!complete, "oversized file must not be marked complete");
        assert!(full.is_none());
    }

    #[test]
    fn present_excerpt_ignores_empty_file() {
        let (complete, full) = present_excerpt("", "");
        assert!(!complete);
        assert!(full.is_none());
    }

    /// The core of the fix (RCA 2026-06-04-semfs-grep-hangs-post-search-under-load):
    /// a blocking read of an unresponsive path must TIME OUT, not hang. A FIFO with
    /// no writer makes `read_to_string` block in `open()` forever — the bound must
    /// fire instead of wedging grep's output formatting.
    #[cfg(unix)]
    #[test]
    fn read_file_timed_times_out_on_blocking_fifo() {
        let dir = std::env::temp_dir().join(format!("semfs_grep_fifo_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("hang.fifo");
        let _ = std::fs::remove_file(&fifo);
        let ok = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "mkfifo unavailable");

        let start = std::time::Instant::now();
        let outcome = read_file_timed(fifo.clone(), Duration::from_millis(200));
        assert!(
            matches!(outcome, ReadOutcome::TimedOut),
            "blocking read must time out"
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "timeout did not bound the read"
        );

        let _ = std::fs::remove_file(&fifo);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn read_file_timed_content_and_missing() {
        let dir = std::env::temp_dir().join(format!("semfs_grep_read_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "hello\nline two\n").unwrap();
        assert!(matches!(
            read_file_timed(f, Duration::from_secs(5)),
            ReadOutcome::Content(c) if c.contains("line two")
        ));
        assert!(matches!(
            read_file_timed(dir.join("nope.txt"), Duration::from_secs(5)),
            ReadOutcome::Missing
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verbatim_single_line() {
        let file = "alpha\nbeta\ngamma\n";
        assert_eq!(line_range_in_file(file, "beta"), Some((2, 2)));
    }

    #[test]
    fn verbatim_multiline_chunk() {
        let file = "alpha\nbeta\ngamma\ndelta\n";
        assert_eq!(line_range_in_file(file, "beta\ngamma"), Some((2, 3)));
    }

    #[test]
    fn first_line_match() {
        let file = "alpha\nbeta\n";
        assert_eq!(line_range_in_file(file, "alpha"), Some((1, 1)));
    }

    #[test]
    fn empty_chunk_returns_none() {
        assert_eq!(line_range_in_file("anything", ""), None);
    }

    #[test]
    fn no_match_returns_none() {
        assert_eq!(line_range_in_file("alpha\nbeta\n", "missing"), None);
    }

    #[test]
    fn verbatim_chunk_ending_in_multibyte_char() {
        let file = "alpha\nnaï\ngamma\n";
        assert_eq!(line_range_in_file(file, "naï"), Some((2, 2)));
    }

    #[test]
    fn verbatim_match_across_blank_line() {
        let file = "abc\n\ndef\n";
        assert_eq!(line_range_in_file(file, "def"), Some((3, 3)));
    }

    #[test]
    fn whitespace_normalized_match_across_blank_line() {
        let file = "abc\n\ndef\n";
        assert_eq!(line_range_in_file(file, "abc def"), Some((1, 3)));
    }

    #[test]
    fn whitespace_normalized_with_leading_whitespace() {
        let file = "  hello world\n";
        assert_eq!(line_range_in_file(file, "hello   world"), Some((1, 1)));
    }

    #[test]
    fn whitespace_normalized_chunk_spans_lines() {
        let file = "intro\n\nalpha beta\ngamma delta\nepsilon\n";
        assert_eq!(
            line_range_in_file(file, "alpha   beta\n\ngamma   delta"),
            Some((3, 4))
        );
    }
}
