//! Factory for spawning a fresh shell [`PtyPane`].
//!
//! v0.1 (M0) returned the raw per-session receiver; M1 forwards it into a
//! shared "redraw" signal so the app main loop can `select!` over one channel
//! regardless of how many sessions exist.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use rimeterm_pty::{PtyBackend, Session, SessionConfig, ShellChoice};
use tokio::sync::mpsc::UnboundedSender;

use crate::pty_pane::PtyPane;

/// Result of a successful spawn: the pane and nothing else (the session's
/// output has been wired directly into the shared redraw signal).
pub struct ShellSpawn {
    pub pane: PtyPane,
}
pub fn spawn_shell(
    shell: &ShellChoice,
    cwd: PathBuf,
    display_name: String,
    initial_cols: u16,
    initial_rows: u16,
    redraw: UnboundedSender<()>,
    osc_tx: UnboundedSender<(rimeterm_core::pane::PaneId, String)>,
) -> Result<ShellSpawn> {
    let program = shell
        .path()
        .ok_or_else(|| anyhow!("no shell resolved; set [core].shell_win / shell_unix"))?
        .to_path_buf();

    let cfg = SessionConfig {
        program: program.clone(),
        args: Vec::new(),
        cwd: Some(cwd),
        // C21.5: shells get the augmented PATH but no per-tool sandbox
        // env — they're user-driven, not a specific essentials tab.
        env: rimeterm_config::env::default_env(None),
        cols: initial_cols,
        rows: initial_rows,
        backend: PtyBackend::Native,
    };

    let (session, mut rx) =
        Session::spawn(cfg).with_context(|| format!("spawning shell `{}`", program.display()))?;

    // Mint the PaneId up-front so the forwarder can tag OSC events with
    // the origin pane. Downstream `PtyPane::with_id` reuses it — no
    // double-mint.
    let pane_id = rimeterm_core::pane::PaneId::next();

    // Forwarder: coalesce Redraw / Exited into the app-wide redraw pulse
    // and route OscRimeterm payloads onto the OSC channel (C18-D).
    tokio::spawn(async move {
        while let Some(evt) = rx.recv().await {
            match evt {
                rimeterm_pty::SessionOutput::OscRimeterm { payload } => {
                    if osc_tx.send((pane_id, payload)).is_err() {
                        break;
                    }
                }
                _ => {
                    if redraw.send(()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    Ok(ShellSpawn {
        pane: crate::pty_pane::PtyPane::with_id(pane_id, session, display_name),
    })
}
