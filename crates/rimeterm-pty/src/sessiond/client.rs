//! Client half of sessiond: attach with autostart.
//!
//! The TUI never spawns PTY children itself in daemon mode — it calls
//! [`attach_session`], which:
//!
//! 1. connects to the daemon at `endpoint`, starting one first if absent
//!    (see [`ensure_daemon`]);
//! 2. sends `Attach { key, spawn }` and awaits `Welcome`;
//! 3. hands the socket halves back to [`crate::session::Session`], whose
//!    remote pump feeds the local grid replica.

use anyhow::{Context, Result, bail};

use super::transport::{FrameReader, FrameWriter, SessionConn, connect};
use super::{Attach, ClientMsg, DaemonMsg, Frame, SpawnSpec, Welcome};

/// How long to wait for a just-spawned daemon to bind its endpoint.
const DAEMON_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const DAEMON_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Test/CI escape hatch: never spawn `current_exe() --sessiond`.
///
/// In a `cargo test` binary `current_exe()` is the test harness itself —
/// autostart there would re-run the suite recursively. Tests host the
/// daemon in-process instead and flip this switch once.
static NO_AUTOSTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Disable daemon autostart for this process (see [`NO_AUTOSTART`]).
pub fn disable_autostart() {
    NO_AUTOSTART.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Result of a successful attach: the two socket halves plus the Welcome
/// handshake payload (root pid, current grid size).
pub struct Attached {
    pub welcome: Welcome,
    pub reader: FrameReader,
    pub writer: FrameWriter,
}

/// Attach to `key`, spawning per `spec` when the daemon doesn't know it.
/// Starts the daemon when there is none.
pub async fn attach_session(
    endpoint: &str,
    key: &str,
    label: &str,
    kind: &str,
    spec: SpawnSpec,
) -> Result<Attached> {
    let conn = ensure_daemon(endpoint)
        .await
        .with_context(|| format!("sessiond at {endpoint}"))?;
    let (mut reader, mut writer) = conn.into_split();

    let attach = Attach {
        key: key.to_owned(),
        label: label.to_owned(),
        kind: kind.to_owned(),
        spawn: Some(spec),
    };
    writer
        .write_frame(&Frame::ClientJson(ClientMsg::Attach(attach)))
        .await
        .context("sending Attach")?;

    match reader.read_frame().await.context("awaiting Welcome")? {
        Some(Frame::DaemonJson(DaemonMsg::Welcome(welcome))) => Ok(Attached {
            welcome,
            reader,
            writer,
        }),
        Some(Frame::DaemonJson(DaemonMsg::Denied { reason })) => {
            bail!("sessiond denied attach to `{key}`: {reason}")
        }
        Some(other) => bail!("sessiond: unexpected first frame {other:?}"),
        None => bail!("sessiond closed the connection during handshake"),
    }
}

/// Connect to the daemon at `endpoint`, starting one when absent.
///
/// The daemon is `current_exe() --sessiond` spawned detached: on Windows
/// with `DETACHED_PROCESS`, on Unix in its own process group, stdio bound
/// to null (it logs to the tracing file sink like every rimeterm process).
pub async fn ensure_daemon(endpoint: &str) -> Result<SessionConn> {
    if NO_AUTOSTART.load(std::sync::atomic::Ordering::Relaxed) {
        // Autostart disabled — one connect attempt, no spawn, no retry.
        return connect(endpoint).await;
    }
    // Fast path: listener already there.
    if let Ok(conn) = connect(endpoint).await {
        return Ok(conn);
    }
    spawn_detached_daemon(endpoint)?;
    let deadline = tokio::time::Instant::now() + DAEMON_START_TIMEOUT;
    loop {
        if let Ok(conn) = connect(endpoint).await {
            return Ok(conn);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("daemon did not come up within {DAEMON_START_TIMEOUT:?}");
        }
        tokio::time::sleep(DAEMON_POLL_INTERVAL).await;
    }
}

fn spawn_detached_daemon(endpoint: &str) -> Result<()> {
    let exe = std::env::current_exe().context("resolving current executable")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--sessiond")
        .arg("--endpoint")
        .arg(endpoint)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New process group: the daemon outlives the TUI and must ignore
        // the terminal's process-group signals (SIGHUP on close).
        cmd.process_group(0);
    }

    cmd.spawn()
        .with_context(|| format!("spawning sessiond for {endpoint}"))?;
    Ok(())
}
