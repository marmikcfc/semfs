//! `semfs grep` — semantic search across a mounted container.

use anyhow::Result;
use clap::Args as ClapArgs;
use semfs_core::backend::{CloudIndex, SemanticIndex, SqliteVecStore};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Resolve the search backend — **config-driven, no flag, no network.**
///
/// Uses the container's LOCAL SQLite index (full L1–L5: resolved embedder +
/// reranker, read-only via `open_existing`) when BOTH hold: the daemon recorded
/// a cache `db_path` in the `.semfs` marker, and this process can build the
/// matching embedder (`local_indexing_enabled` — same resolver the daemon used).
/// Otherwise falls back to the cloud (`CloudIndex`). No `validate_key`, so a
/// local search needs neither credentials nor connectivity.
///
/// Returns `(index, used_local)`. `used_local` lets the caller retry through the
/// cloud if a local query fails at runtime (e.g. a cloud-backed query embedder
/// hits a provider outage) — a degraded-dependency state that should fall back,
/// not abort the command.
async fn resolve_index(
    db_path: Option<&str>,
    api_url: &str,
    key: Option<&str>,
    tag: &str,
) -> Result<(Arc<dyn SemanticIndex>, bool)> {
    use super::resolve::{choose_storage, StorageChoice};
    let env = super::resolve::ResolveEnv::from_env();
    if super::resolve::local_indexing_enabled(&env) {
        // Postgres/pgvector storage backend (opt-in via SEMFS_STORAGE_BACKEND).
        // Connects directly to Postgres — no local cache db. Fail-open to cloud.
        if choose_storage(&env) == StorageChoice::Pgvector {
            #[cfg(feature = "pg")]
            {
                let embedder = super::resolve::build_embedder(&env)?;
                match super::resolve::build_pg_store(&env, tag, embedder).await {
                    // Gate on readiness, mirroring SQLite's is_searchable(): an
                    // empty Postgres index would return Ok([]) (a false "no
                    // results") and bypass the Err-only cloud retry — so fall back
                    // to cloud unless the container actually has rows.
                    Ok(store) if store.is_searchable().await => {
                        return Ok((Arc::new(store), true))
                    }
                    Ok(_) => tracing::warn!(
                        "pgvector index for '{tag}' is empty/unready; falling back to cloud search"
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
        } else if let Some(p) = db_path.filter(|p| std::path::Path::new(p).exists()) {
            // Default SQLite. Degraded-dependency states (corrupt cache, stale
            // model dir) fall back to cloud, not abort grep — local init errors
            // are logged and we continue rather than `?`-propagating.
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
    NoIndex,
    /// Daemon is up and OWNS the index, but the search itself errored (backend
    /// fault, embedder outage, timeout). The caller must NOT silently re-resolve
    /// a different backend: for pglite there's no direct path, so falling back
    /// would return stale cloud results that omit unsynced local writes and mask
    /// the real fault. Surface it instead.
    Failed(String),
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
        Ok(Response::SearchHits { hits, searchable: true }) => DaemonSearch::Hits(hits),
        Ok(Response::SearchHits { searchable: false, .. }) => DaemonSearch::NoIndex,
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
        // Daemon reachable and it DID understand the request but the search failed
        // — a real fault, not absence. Don't reclassify as Unreachable (which would
        // silently route to a different backend); surface it.
        Ok(Response::Error { message }) => DaemonSearch::Failed(message),
        // A response of the wrong type from a live daemon is a protocol fault,
        // not absence — surface it rather than silently falling back.
        Ok(other) => DaemonSearch::Failed(format!("unexpected daemon response: {other:?}")),
        // Only a genuinely ABSENT daemon (no socket / connect refused/timeout)
        // falls back to a directly-resolved backend.
        Err(SendError::Unreachable(_)) => DaemonSearch::Unreachable,
        // Daemon was reachable but the exchange failed mid-flight (read timeout,
        // disconnect, malformed reply) — a daemon-side fault. Surface it.
        Err(SendError::PostConnect(e)) => {
            DaemonSearch::Failed(format!("daemon transport error: {e}"))
        }
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

fn read_local_or_sidecar(mount: &Path, filepath: &str) -> Option<String> {
    let stripped = filepath.trim_start_matches('/');
    let local = mount.join(stripped);
    if let Ok(c) = std::fs::read_to_string(&local) {
        return Some(c);
    }
    for suffix in &[
        ".pdf-transcription.md",
        ".image-transcription.md",
        ".video-transcription.md",
        ".audio-transcription.md",
        ".webpage-transcription.md",
    ] {
        let sidecar = mount.join(format!("{stripped}{suffix}"));
        if let Ok(c) = std::fs::read_to_string(&sidecar) {
            return Some(c);
        }
    }
    None
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

    // mount_path drives project-scoped credential lookup. Mirror the tag
    // precedence: CWD marker, then the path-argument marker — otherwise project
    // credentials for a mount are ignored when grep runs from outside it.
    let mount_path = marker
        .as_ref()
        .and_then(|m| m.mount_path.as_deref())
        .or_else(|| path_marker.as_ref().and_then(|m| m.mount_path.as_deref()))
        .map(std::path::Path::new);
    // Key is only needed for the cloud fallback; a local search needs none.
    let key = super::auth::resolve_api_key(args.key.as_deref(), mount_path).ok();

    // Local cache db path from the marker (CWD marker, then path-argument marker)
    // — opening it needs no network.
    let db_path = marker
        .as_ref()
        .and_then(|m| m.db_path.as_deref())
        .or_else(|| path_marker.as_ref().and_then(|m| m.db_path.as_deref()));

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

    // L4: optional LLM query rewrite (opt-in via --rewrite; fail-open to original).
    let effective_query = if args.rewrite {
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
        DaemonSearch::NoIndex => {
            // Daemon up but no local index (hash embedder) → cloud.
            cloud_index(&api_url, key.as_deref(), &tag)?
                .search(&effective_query, filepath.as_deref())
                .await?
        }
        DaemonSearch::Failed(message) => {
            // Daemon owns the index and the search FAILED. Do not silently fall
            // back to a different backend — that would mask the fault and (for
            // pglite, which has no direct path) return stale cloud results that
            // omit unsynced local writes. Surface the error.
            return Err(anyhow::anyhow!(
                "search failed on the mount daemon for '{tag}': {message}\n\
                 (the daemon owns this container's index; not falling back to a \
                 different backend, which could return stale results)"
            ));
        }
        DaemonSearch::Unreachable => {
            // No daemon: resolve a backend directly (SQLite file / external
            // Postgres / cloud). pglite has no direct path — it lives in the
            // daemon — so an un-mounted pglite container resolves to cloud here.
            let (index, used_local) =
                resolve_index(db_path, &api_url, key.as_deref(), &tag).await?;
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
    eprintln!("#   grep \"natural language query\"          search all files");
    eprintln!("#   grep \"query\" path/to/dir/              search within directory");
    eprintln!("# output: <filepath>:<line_start>-<line_end>:<chunk>");
    eprintln!(
        "# chunk text is verbatim from the file. extract by the line range. never read or cat whole files."
    );
    eprintln!();

    let mut file_cache: HashMap<String, Option<String>> = HashMap::new();

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
        let escaped = chunk
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r");

        let line_range = canonical_mount
            .as_ref()
            .zip(result.filepath.as_deref())
            .and_then(|(cm, path)| {
                let content = file_cache
                    .entry(path.to_string())
                    .or_insert_with(|| read_local_or_sidecar(cm, path))
                    .as_deref()?;
                line_range_in_file(content, chunk)
            });

        if let Some((start, end)) = line_range {
            if start == end {
                println!("{}:{}:{}", fp, start, escaped);
            } else {
                println!("{}:{}-{}:{}", fp, start, end, escaped);
            }
        } else {
            println!("{}:{}", fp, escaped);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::line_range_in_file;

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
