//! Resolution for the router and compressor backends.
//!
//! Both follow the same shape: a default **model** name (overridable via
//! `<PREFIX>_MODEL` independent of which endpoint serves it), and an **endpoint**
//! resolved by this precedence (highest first):
//!
//!   1. `<PREFIX>_API_KEY` and/or `<PREFIX>_BASE_URL` set  → "byo" (user's own).
//!   2. `~/.tokopt/config.json` `backend_url` set          → "config" (set via
//!      the gateway's own HTML config page — see `configweb.rs`).
//!   3. `TOKOPT_ENV=dev` (and no config)                   → "dev-localhost"
//!      (`http://127.0.0.1:8788`, the local `backend-server/`).
//!   4. otherwise (and no config)                          → "default-hosted"
//!      (our hosted endpoint).
//!
//! The default-hosted endpoint is ONE backend (`backend-server/`, `/optimize`)
//! shared by both router and compressor — when we control both sides there's no
//! reason to split them. It has no real deployment yet (see
//! `tickets/proxy-gateway-rs/README.md`): `DEFAULT_HOSTED_BACKEND` is empty out
//! of the box, so `is_unreachable()` is true and callers fail open rather than
//! invent a host. `TOKOPT_DEFAULT_BACKEND_URL` overrides it as a deploy escape
//! hatch; the HTML config page overrides everything but BYO env.
//!
//! Everything here is read *per request* (config.json is re-read each call), so
//! saving in the HTML page takes effect with no gateway restart.

use std::env;
use std::fs;
use std::path::PathBuf;

/// The deployed hosted `/optimize` URL goes here once it exists. Empty until
/// then — an empty base_url means "unreachable", which makes the gateway fail
/// open (pass the request straight through) instead of POSTing to a fake host.
const DEFAULT_HOSTED_BACKEND: &str = "";
/// Where the local `backend-server/` listens (see its `PORT` default).
const DEV_LOCALHOST_BACKEND: &str = "http://127.0.0.1:8788";

#[derive(Debug, Clone)]
pub struct BackendConfig {
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    /// "byo" | "config" | "dev-localhost" | "default-hosted" — surfaced in
    /// /health and the config page for transparency. Only "byo" changes call
    /// shape; the other three are all `/optimize`-style default backends.
    pub source: &'static str,
}

impl BackendConfig {
    /// True when there's nowhere to actually send a request (default-hosted
    /// endpoint not configured on this deploy yet, and the user didn't BYO or
    /// set a config).
    pub fn is_unreachable(&self) -> bool {
        self.base_url.as_deref().unwrap_or("").is_empty()
    }
}

/// User-editable config written by the gateway's HTML config page and read here
/// on every request. All fields optional; a missing/empty `backend_url` means
/// "no config set" and resolution falls through to dev-localhost/default-hosted.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub backend_url: Option<String>,
    #[serde(default)]
    pub backend_api_key: Option<String>,
    #[serde(default)]
    pub router_model: Option<String>,
    #[serde(default)]
    pub compressor_model: Option<String>,
}

fn env_nonempty(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

fn default_backend_key() -> Option<String> {
    env_nonempty("TOKOPT_DEFAULT_BACKEND_API_KEY")
}

pub fn config_dir() -> PathBuf {
    let home = env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".tokopt")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Read `~/.tokopt/config.json`. Missing or malformed → defaults (fail open).
pub fn load_app_config() -> AppConfig {
    match fs::read(config_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}

/// Persist `~/.tokopt/config.json` (0600 on unix — it can hold an api key).
pub fn save_app_config(c: &AppConfig) -> std::io::Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    let path = config_path();
    fs::write(&path, serde_json::to_vec_pretty(c).unwrap_or_default())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn tokopt_env_is_dev() -> bool {
    env_nonempty("TOKOPT_ENV").as_deref() == Some("dev")
}

/// The shared default-hosted `/optimize` backend (used by both router and
/// compressor when neither is BYO'd): (base_url, api_key, source), following
/// precedence steps 2–4 above. `app` is passed in so callers read config.json
/// once per resolve.
fn default_backend(app: &AppConfig) -> (Option<String>, Option<String>, &'static str) {
    // 2. HTML-page config.
    if let Some(url) = app.backend_url.clone().filter(|u| !u.is_empty()) {
        let key = app.backend_api_key.clone().filter(|k| !k.is_empty());
        return (Some(url), key, "config");
    }
    // 3. dev → localhost.
    if tokopt_env_is_dev() {
        return (Some(DEV_LOCALHOST_BACKEND.to_string()), default_backend_key(), "dev-localhost");
    }
    // 4. our hosted endpoint (env override → constant, empty until deployed).
    let hosted = env_nonempty("TOKOPT_DEFAULT_BACKEND_URL")
        .unwrap_or_else(|| DEFAULT_HOSTED_BACKEND.to_string());
    (Some(hosted).filter(|u| !u.is_empty()), default_backend_key(), "default-hosted")
}

pub fn resolve_router_config() -> BackendConfig {
    let app = load_app_config();
    let model = env_nonempty("ROUTER_MODEL")
        .or_else(|| app.router_model.clone())
        .unwrap_or_else(|| "gpt-4.1-nano".to_string());
    let api_key = env_nonempty("ROUTER_API_KEY");
    let base_url = env_nonempty("ROUTER_BASE_URL");
    if api_key.is_some() || base_url.is_some() {
        return BackendConfig {
            model,
            base_url: Some(base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string())),
            api_key,
            source: "byo",
        };
    }
    let (base_url, api_key, source) = default_backend(&app);
    BackendConfig { model, base_url, api_key, source }
}

pub fn resolve_compressor_config() -> BackendConfig {
    let app = load_app_config();
    let model = env_nonempty("COMPRESSOR_MODEL")
        .or_else(|| app.compressor_model.clone())
        .unwrap_or_else(|| "chopratejas/kompress-v2-base".to_string());
    let api_key = env_nonempty("COMPRESSOR_API_KEY");
    let base_url = env_nonempty("COMPRESSOR_BASE_URL");
    if api_key.is_some() || base_url.is_some() {
        return BackendConfig {
            model,
            base_url,
            api_key,
            source: "byo",
        };
    }
    let (base_url, api_key, source) = default_backend(&app);
    BackendConfig { model, base_url, api_key, source }
}
