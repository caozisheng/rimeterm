//! Background worker for the Native [`ModelsPane`].
//!
//! Owns a single OS thread that receives [`ModelsRequest`]s over an
//! mpsc channel and returns [`ModelsResponse`]s. Follows the exact
//! pattern of [`crate::agtop_worker::AgtopWorker`] — the pane checks
//! generation numbers before applying snapshots so stale replies land
//! harmlessly.
//!
//! Unlike `AgtopWorker` (which samples every ~1500 ms) this worker is
//! purely on-demand: the pane triggers one fetch at startup and one
//! per `r` / F5 press. `models.dev` is a static JSON endpoint that
//! only changes when a provider announces a new model, so we don't
//! poll — it would just burn bandwidth.
//!
//! [`ModelsPane`]: crate::models_pane::ModelsPane

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use rimeterm_models::{FetchError, fetch_providers};

use crate::models_model::{ModelsRequest, ModelsResponse, Snapshot};

/// Handle to the running worker thread. `Send`-only through the
/// `send` API — matches `AgtopWorker` conventions.
pub struct ModelsWorker {
    request_tx: Sender<ModelsRequest>,
    response_rx: Receiver<ModelsResponse>,
}

impl ModelsWorker {
    /// Start the worker thread. Panics only if the OS refuses to
    /// spawn — mirrors `AgtopWorker::spawn`.
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<ModelsRequest>();
        let (resp_tx, resp_rx) = mpsc::channel::<ModelsResponse>();
        thread::Builder::new()
            .name("rimeterm-models-worker".into())
            .spawn(move || run(req_rx, resp_tx))
            .expect("spawn models worker");
        Self {
            request_tx: req_tx,
            response_rx: resp_rx,
        }
    }

    pub fn send(&self, request: ModelsRequest) {
        let _ = self.request_tx.send(request);
    }

    /// Drain every response the worker has produced since the last
    /// call. Non-blocking: returns an empty vec when the worker is
    /// mid-fetch.
    pub fn drain(&self) -> Vec<ModelsResponse> {
        let mut out = Vec::new();
        while let Ok(response) = self.response_rx.try_recv() {
            out.push(response);
        }
        out
    }
}

fn run(req_rx: Receiver<ModelsRequest>, resp_tx: Sender<ModelsResponse>) {
    // Blocking recv — an idle rimeterm parks the worker here forever.
    while let Ok(req) = req_rx.recv() {
        match req {
            ModelsRequest::Fetch { generation } => {
                let result = match fetch_providers() {
                    Ok(map) => Ok(Snapshot::from_providers(&map)),
                    Err(e) => Err(format_fetch_error(&e)),
                };
                if resp_tx
                    .send(ModelsResponse::Fetch { generation, result })
                    .is_err()
                {
                    // Receiver dropped — pane closed, kill the worker.
                    return;
                }
            }
        }
    }
}

/// Render a `FetchError` as a short user-facing hint. Kept out of
/// `rimeterm-models` because that crate has no opinion on how a
/// terminal user would want to see the error — this is pure TUI
/// concern.
///
/// For `Network` failures we walk the error source chain to surface
/// the real cause (connection reset, DNS lookup failed, timed out,
/// TLS mismatch) instead of the useless outer `error sending request
/// for url (…): …` wrapper reqwest defaults to. Same behaviour as
/// `git`'s `fatal: unable to access '…': …` — the user knows
/// whether to try again, reach for a proxy, or check the URL.
pub(crate) fn format_fetch_error(e: &FetchError) -> String {
    match e {
        FetchError::Client(_) => "HTTP client build failed".to_owned(),
        FetchError::Network(err) => format_network_error(err),
        FetchError::Status { status, .. } => match *status {
            403 => "HTTP 403 — models.dev refused the request".to_owned(),
            429 => "HTTP 429 — models.dev rate-limited us".to_owned(),
            502 | 503 | 504 => format!("HTTP {status} — models.dev is down"),
            other => format!("HTTP {other} from models.dev"),
        },
        FetchError::Parse(_) => "models.dev returned malformed JSON".to_owned(),
    }
}

/// Categorise a `reqwest::Error` by walking `.source()` to the root
/// cause and matching the resulting text. Kept string-based rather
/// than `downcast`-based because reqwest's error chain isn't part of
/// its stable public API — the strings ARE (they're what users see
/// from every reqwest-using CLI on the planet), so matching on them
/// is the least-fragile signal we can act on.
fn format_network_error(err: &reqwest::Error) -> String {
    // reqwest labels these three cleanly, before we even have to
    // read the chain.
    if err.is_timeout() {
        return "timed out reaching models.dev".to_owned();
    }
    if err.is_connect() {
        // Fall through to the chain walk to distinguish DNS vs. RST
        // vs. refused — `is_connect` alone doesn't tell us which.
    }
    let root = root_cause_string(err);
    let lower = root.to_ascii_lowercase();
    // Great-firewall RST — the exact string curl reports for the
    // same failure mode ("Recv failure: Connection was reset").
    if lower.contains("connection was reset") || lower.contains("connection reset") {
        return "connection reset — models.dev may be blocked (try HTTPS_PROXY)".to_owned();
    }
    if lower.contains("dns")
        || lower.contains("failed to lookup")
        || lower.contains("no such host")
        || lower.contains("name or service not known")
    {
        return "DNS lookup failed for models.dev".to_owned();
    }
    if lower.contains("connection refused") {
        return "connection refused by models.dev".to_owned();
    }
    if lower.contains("certificate")
        || lower.contains("tls")
        || lower.contains("ssl")
        || lower.contains("handshake")
    {
        return "TLS handshake failed with models.dev".to_owned();
    }
    if lower.contains("proxy") {
        return format!("proxy error: {}", truncate(&root, 40));
    }
    // Nothing matched — hand back the shortest non-empty message we
    // can find on the chain. Truncate so a 200-char reqwest tirade
    // doesn't overflow the footer.
    format!("network error: {}", truncate(&root, 40))
}

/// Follow `.source()` to the deepest error and return its `Display`
/// output. Falls back to the outer error's `Display` when no chain
/// is present.
fn root_cause_string(err: &(dyn std::error::Error + 'static)) -> String {
    let mut cur: &(dyn std::error::Error + 'static) = err;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

/// Character-count truncate with a trailing ellipsis. Duplicates the
/// helper in `models_pane` on purpose — pulling either into a shared
/// module for one call site would over-couple two files that just
/// happen to both truncate strings.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    if max <= 1 {
        return "…".to_owned();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_hints_are_short_enough_for_footer() {
        // Footer text is one row inside the pane border; the widest
        // hint we emit fits in ~65 chars so it survives a 60-col
        // split (the git group's typical bottom-left width) with
        // gentle ellipsis. Anything much wider crops mid-word.
        let hints = [
            format_fetch_error(&FetchError::Status {
                status: 503,
                body: "x".repeat(500),
            }),
            format_fetch_error(&FetchError::Status {
                status: 429,
                body: String::new(),
            }),
            "HTTP client build failed".to_owned(),
            "models.dev returned malformed JSON".to_owned(),
        ];
        for h in &hints {
            assert!(
                h.chars().count() <= 65,
                "hint too long for footer: {h:?} ({} chars)",
                h.chars().count()
            );
        }
    }

    #[test]
    fn root_cause_walks_to_deepest_source() {
        #[derive(Debug)]
        struct E(&'static str, Option<Box<dyn std::error::Error + 'static>>);
        impl std::fmt::Display for E {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.0)
            }
        }
        impl std::error::Error for E {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.1.as_deref()
            }
        }
        let deep = E("Connection was reset", None);
        let mid = E("os error 10054", Some(Box::new(deep)));
        let outer = E("error sending request", Some(Box::new(mid)));
        assert_eq!(root_cause_string(&outer), "Connection was reset");
    }

    #[test]
    fn status_variants_carry_specific_hints() {
        assert!(
            format_fetch_error(&FetchError::Status {
                status: 429,
                body: String::new(),
            })
            .contains("rate-limited")
        );
        assert!(
            format_fetch_error(&FetchError::Status {
                status: 503,
                body: String::new(),
            })
            .contains("is down")
        );
    }

    #[test]
    fn truncate_stays_below_ceiling() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("helloworld", 5), "hell…");
        // Zero-width fallback: refuse to truncate below one glyph.
        assert_eq!(truncate("abc", 1), "…");
    }
}
