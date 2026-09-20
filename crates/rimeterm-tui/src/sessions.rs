//! Session-host selection for PTY panes (B2 model,
//! `docs/rimeterm-server-client-split.md`).
//!
//! Default is in-process native PTYs (`Session::spawn`). With
//! `[core] sessiond = true` every shell / agent / external tool is born
//! inside the session daemon (`rimeterm --sessiond`) via
//! `Session::attach_remote`, so quitting the TUI detaches instead of
//! killing children — the next launch reattaches to the same sessions
//! under stable workspace-scoped keys.
//!
//! The factories (`shell_factory`, `agent_factory`) are sync; the daemon
//! attach path is async. [`launch`] bridges with `block_in_place` +
//! `Handle::block_on` — the tokio-documented combo for sync→async on a
//! multi-thread runtime, which is the only runtime the TUI runs
//! (`rimeterm/src/main.rs`). Legal both from worker tasks and from the
//! `block_on` driving thread (where `App::new` runs).

use anyhow::{Context, Result, anyhow};
use rimeterm_pty::sessiond::SpawnSpec;
use rimeterm_pty::{Session, SessionConfig, SessionOutput};
use tokio::sync::mpsc;

/// Which process hosts the PTY children.
#[derive(Clone, Debug)]
pub enum SessionHost {
    /// In-process native PTY (default).
    Native,
    /// Per-user session daemon at `endpoint`.
    Daemon { endpoint: String },
}

impl SessionHost {
    /// Resolve the hosting mode from `[core] sessiond`.
    pub fn from_config(config: &rimeterm_config::Config) -> Self {
        if config.core.sessiond {
            Self::Daemon {
                endpoint: rimeterm_pty::sessiond::default_endpoint(),
            }
        } else {
            Self::Native
        }
    }

    /// True when sessions live in the daemon (quit = detach).
    pub fn is_daemon(&self) -> bool {
        matches!(self, Self::Daemon { .. })
    }
}

/// Stable per-workspace daemon key prefix. The daemon is per-user, so
/// two rimeterm windows over different workspaces must never collide on
/// `shell-1`.
pub fn key_prefix(workspace_root: &std::path::Path) -> String {
    format!(
        "{}-",
        rimeterm_config::layout_state::workspace_hash(workspace_root)
    )
}

/// Birth a session under `host`. Native: in-process spawn. Daemon:
/// attach (respawn when the key is unknown, replay when it is live).
///
/// `key`/`label`/`kind` only matter for the daemon path.
pub fn launch(
    host: &SessionHost,
    key: &str,
    label: &str,
    kind: &str,
    cfg: &SessionConfig,
) -> Result<(Session, mpsc::UnboundedReceiver<SessionOutput>)> {
    match host {
        SessionHost::Native => Ok(Session::spawn(cfg.clone())?),
        SessionHost::Daemon { endpoint } => {
            // `SpawnSpec` crosses a JSON boundary as `String`; non-UTF-8
            // program/cwd are rejected here (not lossily mangled).
            let program = cfg
                .program
                .to_str()
                .ok_or_else(|| anyhow!("sessiond requires a UTF-8 program path"))?
                .to_owned();
            let cwd = match &cfg.cwd {
                Some(path) => Some(
                    path.to_str()
                        .ok_or_else(|| anyhow!("sessiond requires a UTF-8 cwd"))?
                        .to_owned(),
                ),
                None => None,
            };
            let spec = SpawnSpec {
                program,
                args: cfg.args.clone(),
                cwd,
                env: cfg.env.clone(),
                cols: cfg.cols,
                rows: cfg.rows,
            };
            let attach =
                Session::attach_remote(endpoint, key, label, kind, spec, cfg.cols, cfg.rows);
            tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(attach))
                .with_context(|| format!("attaching sessiond session `{key}`"))
        }
    }
}
