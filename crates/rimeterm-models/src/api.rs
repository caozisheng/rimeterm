//! Blocking fetch from `https://models.dev/api.json`.
//!
//! Kept blocking on purpose — the pane worker is a bare `std::thread`
//! (matching `AgtopWorker` / `SysmonWorker`) and dragging tokio in
//! just for one HTTP round-trip would force the whole worker layer to
//! become async.
//!
//! ## Blocked / rate-limited endpoints
//!
//! `models.dev` sits on Cloudflare Pages, which is unreachable from a
//! handful of networks (notably mainland China: TLS-handshake RST).
//! Two escape hatches, both honored automatically:
//!
//! - `HTTPS_PROXY` / `HTTP_PROXY` env vars — reqwest reads them by
//!   default (also `system_proxy` on Windows). Set one to a working
//!   proxy and the fetch tunnels through.
//! - `RIMETERM_MODELS_URL` env var — full override for the JSON
//!   endpoint, e.g. a self-hosted mirror or a CDN copy. Empty /
//!   unset falls back to `API_URL`.
use std::time::Duration;

use crate::data::ProvidersMap;

/// Upstream models.dev API endpoint. `modelsdev::api::API_URL` verbatim.
pub const API_URL: &str = "https://models.dev/api.json";

/// Env var name for the URL override. Set to a full JSON endpoint URL
/// to route the fetch somewhere other than `models.dev` — useful when
/// `models.dev` is blocked in the user's region.
pub const MODELS_URL_ENV: &str = "RIMETERM_MODELS_URL";

/// User-agent header sent with every fetch. Kept distinct from the
/// upstream `modelsdev` string so a rate-limit incident on models.dev
/// can distinguish rimeterm traffic if they ever need to.
const USER_AGENT: &str = concat!("rimeterm-models/", env!("CARGO_PKG_VERSION"));

/// How long we're willing to wait for models.dev to answer. 15 s is well
/// beyond the observed ~1 s response time and short enough that a
/// hard-down endpoint surfaces as an error in the pane rather than
/// hanging the worker thread forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Errors surfaced by [`fetch_providers`]. Kept as a concrete enum
/// (rather than `anyhow::Error`) so the pane can pattern-match on
/// network vs. parse failure to build a nicer hint bar.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("HTTP client build failed: {0}")]
    Client(#[source] reqwest::Error),
    #[error("network error: {0}")]
    Network(#[source] reqwest::Error),
    #[error("HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("failed to parse models.dev response: {0}")]
    Parse(#[source] reqwest::Error),
}

/// Fetch the full `models.dev` catalog. Blocking — expected to be
/// called from a worker thread. Honors `RIMETERM_MODELS_URL` for
/// endpoint override.
pub fn fetch_providers() -> Result<ProvidersMap, FetchError> {
    let url_override = std::env::var(MODELS_URL_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty());
    let url = url_override.as_deref().unwrap_or(API_URL);
    fetch_providers_from(url)
}

/// Same as [`fetch_providers`] but against an arbitrary URL — useful
/// for tests (spin up a local httpmock server) and future mirroring.
pub fn fetch_providers_from(url: &str) -> Result<ProvidersMap, FetchError> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(FetchError::Client)?;

    let response = client.get(url).send().map_err(FetchError::Network)?;

    let status = response.status();
    if !status.is_success() {
        // Cap the body we drag around so a 500 with a megabyte of HTML
        // doesn't inflate the error path.
        let body = response.text().unwrap_or_default();
        let mut trimmed: String = body.chars().take(200).collect();
        if body.chars().count() > 200 {
            trimmed.push('…');
        }
        return Err(FetchError::Status {
            status: status.as_u16(),
            body: trimmed,
        });
    }

    response.json::<ProvidersMap>().map_err(FetchError::Parse)
}
