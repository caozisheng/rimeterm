//! Session daemon (`sessiond`) — persistent PTY host.
//!
//! tmux-style architecture (B2 replica-grid model from
//! `docs/rimeterm-server-client-split.md`):
//!
//! - The **daemon** (one per user, spawned as `rimeterm --sessiond`) owns
//!   every PTY from birth. It never parses VT — it is a pure byte pump:
//!   child stdout → ring buffer + broadcast to attached clients, client
//!   writes → child stdin.
//! - The **client** (rimeterm TUI, in-process) keeps its own
//!   `alacritty_terminal::Term` replica and replays the ring on attach,
//!   so every existing `with_term` callsite keeps working unchanged.
//!
//! Closing the TUI simply drops the client connections: the child keeps
//! running under the daemon ("关闭后台不断流"). Reattaching on next
//! launch replays the ring into a fresh replica ("持久化会话").
//!
//! Wire format (one connection per session, plus `List`-only probes):
//!
//! ```text
//! frame = [u32 BE length][u8 kind][body]      length counts kind + body
//! kind 1  C2D JSON   Attach / Resize / Kill / List / Detach
//! kind 2  D2C JSON   Welcome / Denied / Exited / Sessions
//! kind 3  C2D binary child stdin bytes
//! kind 4  D2C binary child stdout bytes
//! ```
//!
//! A client that wants a session opens a connection and sends
//! `Attach { key, spawn? }`. The daemon either attaches to a live
//! session (replaying the ring) or spawns the requested child. EOF on
//! the connection means detach — the session survives.

use std::io;
#[cfg(not(windows))]
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod client;
pub mod daemon;
pub mod ring;
pub mod transport;

/// Per-session output retention. 8 MiB ≈ a full 5000-line scrollback of
/// 80-column UTF-8 with escape overhead, with headroom.
pub const RING_CAPACITY_BYTES: usize = 8 * 1024 * 1024;

/// Hard cap on a single frame body. Output chunks are ≤ 8 KiB in
/// practice; writes are chunked by callers. The cap exists so a corrupt
/// length prefix fails fast instead of allocating unbounded memory.
pub const MAX_FRAME_BODY: usize = 1024 * 1024;

/// Idle exit: the daemon shuts itself down once it has hosted zero
/// sessions and zero connections for this long. Prevents orphan daemons
/// after the user kills every session, while keeping the daemon alive
/// through the "TUI closed, sessions still running" window.
pub const IDLE_EXIT_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// Sentinel stored in the daemon's grace atomic meaning "never exit".
/// `u64::MAX` seconds — no wall clock can reach it.
pub const GRACE_NEVER: u64 = u64::MAX;

/// Clamp a configured grace (seconds; `None` = never) to the atomic
/// representation. Zero/negative-ish inputs clamp to 1s so a misconfigured
/// value cannot spin-exit the daemon instantly.
pub fn grace_to_atomic(secs: Option<u64>) -> u64 {
    match secs {
        None => GRACE_NEVER,
        Some(s) => s.max(1),
    }
}

// ---------------------------------------------------------------------------
// Endpoint
// ---------------------------------------------------------------------------

/// Transport endpoint for the session daemon.
///
/// A string for wire-friendliness: a Unix domain socket path, or a
/// Windows named pipe name (`\\.\pipe\...`). Resolution order:
///
/// 1. `RIMETERM_SESSIOND_ENDPOINT` env var (tests + power users),
/// 2. `$XDG_RUNTIME_DIR/rimeterm/sessiond.sock` on Unix when set,
/// 3. `<rimeterm home>/data/run/sessiond.sock` (matches the
///    `~/.rimeterm` layout from `rimeterm-config::paths`),
/// 4. `\\.\pipe\rimeterm-sessiond` on Windows.
pub fn default_endpoint() -> String {
    if let Some(explicit) = std::env::var_os("RIMETERM_SESSIOND_ENDPOINT")
        && !explicit.is_empty()
    {
        return explicit.to_string_lossy().into_owned();
    }

    #[cfg(windows)]
    {
        return r"\\.\pipe\rimeterm-sessiond".to_string();
    }
    #[cfg(not(windows))]
    {
        if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR")
            && !runtime.is_empty()
        {
            return format!("{runtime}/rimeterm/sessiond.sock");
        }
        rimeterm_home()
            .map(|home| {
                let path = home.join("data").join("run").join("sessiond.sock");
                path.to_string_lossy().into_owned()
            })
            .unwrap_or_else(|| "/tmp/rimeterm-sessiond.sock".to_string())
    }
}

/// `~/.rimeterm` or `$RIMETERM_HOME` — mirrors
/// `rimeterm_config::paths::home` without dragging the config crate
/// into this one's dependency graph.
#[cfg(not(windows))]
fn rimeterm_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("RIMETERM_HOME")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".rimeterm"))
}

/// True when the endpoint string names a Windows named pipe.
pub fn is_pipe_endpoint(endpoint: &str) -> bool {
    endpoint.starts_with(r"\\.\pipe\")
}

// ---------------------------------------------------------------------------
// Protocol messages
// ---------------------------------------------------------------------------

/// First client message on a fresh connection. `spawn: Some` asks the
/// daemon to create the session when the key is unknown; `spawn: None`
/// is a pure reattach and fails with [`DaemonMsg::Denied`] when the key
/// is gone. When the key already hosts a *live* session, the spawn
/// payload is ignored (idempotent attach — the TUI's list-then-spawn
/// race collapses into one round trip).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attach {
    /// Registry key, e.g. `<workspace-hash>:shell-3`.
    pub key: String,
    /// Human label shown in tab strips (mirrors `PtyPane` titles).
    pub label: String,
    /// Opaque kind tag (`"shell"` / `"agent"`); round-tripped by `List`.
    pub kind: String,
    /// Spawn payload used when (and only when) the key is unknown.
    pub spawn: Option<SpawnSpec>,
}

/// Everything the daemon needs to birth a child under a fresh PTY.
/// `program`/`cwd` are `String` (not `PathBuf`/`OsString`) because they
/// cross a JSON boundary; non-UTF-8 paths are rejected at spawn with an
/// error instead of silently mangling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

/// Daemon's verdict on an [`Attach`]. `exit: Some` marks a session that
/// died while detached — the client surfaces it as an immediate
/// `SessionOutput::Exited` and no live stream follows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub pid: Option<u32>,
    pub cols: u16,
    pub rows: u16,
    pub exit: Option<u32>,
}

/// Everything a `List` probe knows about one hosted session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub key: String,
    pub label: String,
    pub kind: String,
}

/// JSON control payloads. Binary hot-path frames (`Write`, `Output`)
/// never serialize through here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Attach(Attach),
    Resize {
        cols: u16,
        rows: u16,
    },
    Kill,
    /// Graceful "done, keep the session" close. Plain EOF is equivalent.
    Detach,
    /// List-only probe connection: reply with `DaemonMsg::Sessions`,
    /// then close. Never attaches.
    List,
    /// Kill every live session and shut the daemon down now. Probe-style
    /// message: send as the first frame of a short connection, like `List`.
    Shutdown,
    /// Adjust the after-last-close grace period without restarting the
    /// daemon. Probe-style first-frame message, like `List`.
    SetGrace {
        /// New grace in seconds; `None` = never idle-exit.
        secs: Option<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMsg {
    Welcome(Welcome),
    Denied {
        reason: String,
    },
    Exited {
        status: u32,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    /// Ack for `Shutdown` / `SetGrace`: the daemon confirms the operation
    /// (the probe connection then closes; for `Shutdown` the whole daemon
    /// process exits right after).
    Ack,
}

/// Frame kind byte on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// JSON [`ClientMsg`].
    ClientJson = 1,
    /// JSON [`DaemonMsg`].
    DaemonJson = 2,
    /// Raw bytes → child stdin.
    ClientWrite = 3,
    /// Raw bytes ← child stdout (ring replay and live output alike).
    DaemonOutput = 4,
}

impl TryFrom<u8> for Kind {
    type Error = io::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Kind::ClientJson),
            2 => Ok(Kind::DaemonJson),
            3 => Ok(Kind::ClientWrite),
            4 => Ok(Kind::DaemonOutput),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown sessiond frame kind {other}"),
            )),
        }
    }
}

/// One decoded frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    ClientJson(ClientMsg),
    DaemonJson(DaemonMsg),
    ClientWrite(Vec<u8>),
    DaemonOutput(Vec<u8>),
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("message exceeds frame cap ({cap} bytes): {len}")]
    Oversized { len: usize, cap: usize },
    #[error("json decode error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Encode a frame into `out` as `[u32 BE length][kind][body]`.
pub fn encode_frame(out: &mut Vec<u8>, frame: &Frame) -> Result<(), CodecError> {
    let (kind, body): (Kind, _) = match frame {
        Frame::ClientJson(msg) => (Kind::ClientJson, serde_json::to_vec(msg)?),
        Frame::DaemonJson(msg) => (Kind::DaemonJson, serde_json::to_vec(msg)?),
        Frame::ClientWrite(bytes) => (Kind::ClientWrite, bytes.clone()),
        Frame::DaemonOutput(bytes) => (Kind::DaemonOutput, bytes.clone()),
    };
    let len = 1 + body.len();
    if len > MAX_FRAME_BODY {
        return Err(CodecError::Oversized {
            len,
            cap: MAX_FRAME_BODY,
        });
    }
    out.reserve(4 + len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.push(kind as u8);
    out.extend_from_slice(&body);
    Ok(())
}

/// Validate a frame head (`[u32 BE length][kind]`). Shared by the sync
/// reader below and the async reader in `transport.rs`.
pub(crate) fn parse_head(head: &[u8; 5]) -> Result<(usize, Kind), CodecError> {
    let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    if len == 0 || len > MAX_FRAME_BODY {
        return Err(CodecError::Oversized {
            len,
            cap: MAX_FRAME_BODY,
        });
    }
    let kind = Kind::try_from(head[4])?;
    Ok((len, kind))
}

/// Decode a frame body. Shared by the sync and async readers.
pub(crate) fn decode(kind: Kind, body: Vec<u8>) -> Result<Frame, CodecError> {
    Ok(match kind {
        Kind::ClientJson => Frame::ClientJson(serde_json::from_slice(&body)?),
        Kind::DaemonJson => Frame::DaemonJson(serde_json::from_slice(&body)?),
        Kind::ClientWrite => Frame::ClientWrite(body),
        Kind::DaemonOutput => Frame::DaemonOutput(body),
    })
}

/// Read exactly one frame from `r`. Blocking; callers on async paths
/// wrap it over an async reader or use a dedicated blocking task.
pub fn read_frame(r: &mut impl io::Read) -> Result<Frame, CodecError> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let (len, kind) = parse_head(&head)?;
    let mut body = vec![0u8; len - 1];
    r.read_exact(&mut body)?;
    decode(kind, body)
}

#[cfg(test)]
mod codec_tests {
    use super::*;

    #[test]
    fn frames_round_trip_through_bytes() {
        let mut buf = Vec::new();
        encode_frame(
            &mut buf,
            &Frame::ClientJson(ClientMsg::Resize { cols: 80, rows: 24 }),
        )
        .unwrap();
        encode_frame(&mut buf, &Frame::ClientWrite(vec![1, 2, 3])).unwrap();
        encode_frame(
            &mut buf,
            &Frame::DaemonJson(DaemonMsg::Exited { status: 0 }),
        )
        .unwrap();

        let mut cursor: &[u8] = &buf;
        assert_eq!(
            read_frame(&mut cursor).unwrap(),
            Frame::ClientJson(ClientMsg::Resize { cols: 80, rows: 24 })
        );
        assert_eq!(
            read_frame(&mut cursor).unwrap(),
            Frame::ClientWrite(vec![1, 2, 3])
        );
        assert_eq!(
            read_frame(&mut cursor).unwrap(),
            Frame::DaemonJson(DaemonMsg::Exited { status: 0 })
        );
        assert!(cursor.is_empty());
    }

    #[test]
    fn length_prefix_rejects_oversized_frame() {
        let err = encode_frame(
            &mut Vec::new(),
            &Frame::DaemonOutput(vec![0; MAX_FRAME_BODY]),
        );
        assert!(matches!(err, Err(CodecError::Oversized { .. })));
    }

    #[test]
    fn attach_round_trips_with_spawn_spec() {
        let msg = ClientMsg::Attach(Attach {
            key: "abc123:shell-1".into(),
            label: "shell-1".into(),
            kind: "shell".into(),
            spawn: Some(SpawnSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "echo hi".into()],
                cwd: None,
                env: vec![("TERM".into(), "xterm-256color".into())],
                cols: 80,
                rows: 24,
            }),
        });
        let mut buf = Vec::new();
        encode_frame(&mut buf, &Frame::ClientJson(msg.clone())).unwrap();
        let mut cursor: &[u8] = &buf;
        assert_eq!(read_frame(&mut cursor).unwrap(), Frame::ClientJson(msg));
    }
}
