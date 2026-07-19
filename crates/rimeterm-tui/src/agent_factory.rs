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
) -> Result<ExternalSpawn> {
    let cfg = SessionConfig {
        program: program.clone(),
        args,
        cwd: Some(cwd),
        // §6.2 Windows column: force UTF-8; harmless on other OSes.
        env: vec![
            ("PYTHONIOENCODING".into(), "utf-8".into()),
            ("TERM".into(), "xterm-256color".into()),
        ],
        cols: initial_cols,
        rows: initial_rows,
        backend: PtyBackend::Native,
    };

    let (session, mut rx) = Session::spawn(cfg)
        .with_context(|| format!("spawning `{}`", program.display()))?;

    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            if redraw.send(()).is_err() {
                break;
            }
        }
    });

    Ok(ExternalSpawn {
        pane: PtyPane::new(session, display_name),
    })
}

/// Alias kept for M3 callers.
pub fn spawn_agent(
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    display_name: String,
    initial_cols: u16,
    initial_rows: u16,
    redraw: UnboundedSender<()>,
) -> Result<ExternalSpawn> {
    spawn_external(program, args, cwd, display_name, initial_cols, initial_rows, redraw)
}
