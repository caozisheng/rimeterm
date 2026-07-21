//! Spawn an external tool (`yazi`, `gitui`, `bottom`, `omp`, `claude`, or any
//! user-defined entry) into a fresh PTY, wrapped as a [`PtyPane`].
//!
//! Consolidates the "resolve program → build SessionConfig → spawn Session →
//! wrap in PtyPane" flow so every quadrant that hosts an external command
//! goes through one code path. See §19.10.9 (shells) and §19.10.7 (files /
//! sysmon / agents grouping) of the design doc.

use std::path::PathBuf;

use anyhow::{Context, Result};
use rimeterm_pty::{PtyBackend, Session, SessionConfig};
use tokio::sync::mpsc::UnboundedSender;

use crate::pty_pane::PtyPane;

pub struct ExternalSpawn {
    pub pane: PtyPane,
}

/// Alias kept for M3 callers.
pub type AgentSpawn = ExternalSpawn;

pub fn spawn_external(
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    display_name: String,
    initial_cols: u16,
    initial_rows: u16,
    redraw: UnboundedSender<()>,
    osc_tx: UnboundedSender<(rimeterm_core::pane::PaneId, String)>,
    // C21.5: tool identity so `default_env` can inject the config
    // sandbox env (`YAZI_CONFIG_HOME`, `XDG_CONFIG_HOME`/`APPDATA`,
    // `BTM_CONFIG_LOCATION`) for essentials. `None` for shells /
    // agents / plugins with no sandbox.
    tool_id: Option<&str>,
) -> Result<ExternalSpawn> {
    let cfg = SessionConfig {
        program: program.clone(),
        args,
        cwd: Some(cwd),
        env: rimeterm_config::env::default_env(tool_id),
        cols: initial_cols,
        rows: initial_rows,
        backend: PtyBackend::Native,
    };

    let (session, mut rx) =
        Session::spawn(cfg).with_context(|| format!("spawning `{}`", program.display()))?;

    // Mint the PaneId up-front so the forwarder can tag OSC events with
    // the origin pane. `PtyPane::with_id` reuses the same id below.
    let pane_id = rimeterm_core::pane::PaneId::next();

    let display_for_log = display_name.clone();
    // Wire the session's event stream to (a) the app-wide redraw pulse,
    // (b) a visible in-grid `[exit N]` marker when the child dies, and
    // (c) the OSC 1337 rimeterm bridge (C18-D). Without (b) the pane
    // just goes blank and the user can't tell whether their agent died
    // at spawn, immediately after, or is still starting.
    let session_for_events = session.clone();
    tokio::spawn(async move {
        while let Some(evt) = rx.recv().await {
            match evt {
                rimeterm_pty::SessionOutput::Redraw => {
                    if redraw.send(()).is_err() {
                        break;
                    }
                }
                rimeterm_pty::SessionOutput::OscRimeterm { payload } => {
                    if osc_tx.send((pane_id, payload)).is_err() {
                        // OSC receiver went away — still keep the pane
                        // alive; just stop tagging OSCs.
                    }
                }
                rimeterm_pty::SessionOutput::Exited { status } => {
                    tracing::warn!(
                        agent = display_for_log.as_str(),
                        status,
                        "external tool exited"
                    );
                    // Paint a visible marker into the alacritty grid. `\r\n` +
                    // reverse video + reset makes it stand out on any theme.
                    let msg = format!(
                        "\r\n\x1b[7m[{} exited: status {}]\x1b[0m\r\n",
                        display_for_log, status
                    );
                    session_for_events.inject_grid_bytes(msg.as_bytes());
                    let _ = redraw.send(());
                    break;
                }
            }
        }
    });

    Ok(ExternalSpawn {
        pane: crate::pty_pane::PtyPane::with_id(pane_id, session, display_name),
    })
}

/// Alias kept for M3 callers. Forwards to [`spawn_external`] with all
/// the same params — including the C18-D OSC channel.
pub fn spawn_agent(
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    display_name: String,
    initial_cols: u16,
    initial_rows: u16,
    redraw: UnboundedSender<()>,
    osc_tx: UnboundedSender<(rimeterm_core::pane::PaneId, String)>,
) -> Result<ExternalSpawn> {
    spawn_external(
        program,
        args,
        cwd,
        display_name,
        initial_cols,
        initial_rows,
        redraw,
        osc_tx,
        None, // agents don't get a config sandbox — they run their own
    )
}
