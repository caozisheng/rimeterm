//! Session-host selection for PTY panes (B2 model,
//! `docs/rimeterm-server-client-split.md`).
//!
//! Default is daemon hosting (`[core] sessiond = true` since v0.3):
//! every shell / agent / external tool is born inside the session
//! daemon (`rimeterm --sessiond`) via `Session::attach_remote`, so
//! quitting the TUI detaches instead of killing children — the next
//! launch reattaches to the same sessions under stable
//! workspace-scoped keys. Opt out with `sessiond = false` in
//! `config.toml` (in-process native PTYs, `Session::spawn`) or from
//! the Settings modal.
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
    /// Per-user session daemon at `endpoint`. `grace_secs` is forwarded
    /// on daemon autostart and via the live `SetGrace` push after a
    /// Settings change.
    Daemon {
        endpoint: String,
        grace_secs: Option<u64>,
    },
}

impl SessionHost {
    /// Resolve the hosting mode from `[core] sessiond`, honoring the
    /// Settings override file (`sessiond.state.toml`) for both the
    /// toggle and the grace period.
    pub fn from_config(config: &rimeterm_config::Config) -> Self {
        let overrides = rimeterm_config::sessiond_state::load_current();
        let enabled = overrides.enabled.unwrap_or(config.core.sessiond);
        if enabled {
            // `grace_secs: null` in the state file = "never exit while a
            // session is alive" — only expressible via Settings, since
            // config.toml's u64 has no null.
            let grace_secs = match overrides.grace_secs {
                Some(secs) => Some(secs),
                None if config.core.sessiond_grace_secs == 0 => None,
                None => Some(config.core.sessiond_grace_secs),
            };
            Self::Daemon {
                endpoint: rimeterm_pty::sessiond::default_endpoint(),
                grace_secs,
            }
        } else {
            Self::Native
        }
    }

    /// True when sessions live in the daemon (quit = detach).
    pub fn is_daemon(&self) -> bool {
        matches!(self, Self::Daemon { .. })
    }

    /// Daemon endpoint when hosting is on — used by the exit dialog and
    /// live grace pushes.
    pub fn daemon_endpoint(&self) -> Option<&str> {
        match self {
            Self::Native => None,
            Self::Daemon { endpoint, .. } => Some(endpoint),
        }
    }

    /// Configured grace period for daemon autostart / live updates.
    pub fn grace_secs(&self) -> Option<u64> {
        match self {
            Self::Native => None,
            Self::Daemon { grace_secs, .. } => *grace_secs,
        }
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
        SessionHost::Daemon {
            endpoint,
            grace_secs,
        } => {
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
            let attach = Session::attach_remote(
                endpoint,
                key,
                label,
                kind,
                spec,
                *grace_secs,
                cfg.cols,
                cfg.rows,
            );
            tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(attach))
                .with_context(|| format!("attaching sessiond session `{key}`"))
        }
    }
}
