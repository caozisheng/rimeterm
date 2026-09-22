//! Client half of rimeterm's IPC.
//!
//! One connection per invocation: connect, write one JSON request line, read
//! one JSON response line, close. Same shape used by `rimectl` and any test.
//!
//! Discovery is a candidate walk, not a single guess. Lockfiles are ranked
//! newest-mtime first, but mtime only *orders* candidates — it never
//! decides. Pids that are provably dead have their lockfile removed along
//! the way (self-healing), and a candidate that is alive but unreachable
//! (its pid got reused by an unrelated process) is skipped in favor of
//! older ones. That is what survives the failure mode where an exited
//! instance's orphan lockfile outranks the live server, so every
//! `--workspace-in-tab` redirect dials a dead pid and falls back to
//! opening a new window instead of joining the running one.

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::protocol::{Request, Response, encode_request};

/// Transport stream returned by [`connect_once`]: UDS on Unix, named pipe
/// on Windows. On any other platform it is never constructed —
/// [`connect_once`] bails first — but the alias must still satisfy the
/// stream bounds of [`write_and_read`], so it names a real type there.
#[cfg(unix)]
pub(crate) type IpcStream = tokio::net::UnixStream;
#[cfg(windows)]
pub(crate) type IpcStream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(not(any(unix, windows)))]
pub(crate) type IpcStream = tokio::net::TcpStream;

/// Connect + send + await one response. Blocks (asynchronously) on the round
/// trip and returns whatever the server writes back.
pub async fn send_once(pid: u32, req: &Request) -> Result<Response> {
    let stream = connect_once(pid).await?;
    write_and_read(stream, req).await
}

/// Send one request to the newest reachable rimeterm server.
///
/// Walks the lockfile candidates newest-first and sends on the first
/// connection that succeeds. Errors only when no candidate is reachable.
pub async fn send_to_latest(req: &Request) -> Result<Response> {
    for pid in live_candidates().await? {
        match connect_once(pid).await {
            Ok(stream) => return write_and_read(stream, req).await,
            Err(e) => {
                tracing::debug!(pid, error = %e, "ipc candidate unreachable; trying older one");
            }
        }
    }
    anyhow::bail!("no reachable rimeterm IPC server found");
}

/// Connect to `pid`'s endpoint.
///
/// On Windows, retries briefly on transient errors — the server creates
/// pipe instances one at a time, so a connect can land in the hand-off gap
/// (`ERROR_PIPE_BUSY`). A missing pipe is terminal: a pid with no endpoint
/// can never answer, and the candidate walk must not burn five retry
/// rounds on every dead entry.
async fn connect_once(pid: u32) -> Result<IpcStream> {
    #[cfg(unix)]
    {
        use tokio::net::UnixStream;
        let path = crate::endpoint::socket_for_pid(pid).context("resolving IPC socket path")?;
        UnixStream::connect(&path)
            .await
            .with_context(|| format!("connect {}", path.display()))
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        let pipe = crate::endpoint::pipe_name_for_pid(pid).context("resolving pipe name")?;
        let mut attempts = 0;
        loop {
            match ClientOptions::new().open(&pipe) {
                Ok(stream) => return Ok(stream),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("connect {pipe}"));
                }
                Err(e) => {
                    attempts += 1;
                    if attempts >= 5 {
                        return Err(e).with_context(|| format!("connect {pipe}"));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        anyhow::bail!("rimeterm IPC client unsupported on this platform");
    }
}

async fn write_and_read<S>(stream: S, req: &Request) -> Result<Response>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (rd, mut wr) = tokio::io::split(stream);
    let bytes = encode_request(req).context("encode request")?;
    wr.write_all(&bytes).await.context("write request")?;
    wr.shutdown().await.ok();
    let mut reader = BufReader::new(rd);
    let mut line = String::new();
    reader.read_line(&mut line).await.context("read response")?;
    if line.is_empty() {
        anyhow::bail!("server closed connection without responding");
    }
    let resp: Response = serde_json::from_str(line.trim_end()).context("decode response")?;
    Ok(resp)
}

/// Discover the newest rimeterm pid that is alive and accepts a
/// connection.
///
/// Lockfile mtime only orders the candidates; the winner is decided by
/// connecting. Dead pids' lockfiles are removed along the way, and
/// live-but-unreachable candidates are skipped. Returns `None` when no
/// candidate is reachable.
pub async fn discover_latest_pid() -> Result<Option<u32>> {
    for pid in live_candidates().await? {
        match connect_once(pid).await {
            Ok(stream) => {
                // Probe connection, dropped immediately: the server sees a
                // client that connects and leaves without a request.
                drop(stream);
                return Ok(Some(pid));
            }
            Err(e) => {
                tracing::debug!(pid, error = %e, "discovery: candidate unreachable; trying older one");
            }
        }
    }
    Ok(None)
}

/// Lockfile candidates, newest-mtime first, restricted to live pids.
///
/// Dead pids' lockfiles are removed as encountered — an orphan outranking
/// a live server in newest-first order is the exact poison this walk
/// exists to survive. A live pid whose endpoint won't connect (pid reused
/// by an unrelated process) keeps its file: liveness is all we can prove
/// from here, and unreachable candidates lose at connect time anyway.
async fn live_candidates() -> Result<Vec<u32>> {
    let Some(dir) = crate::endpoint::lockfile_dir() else {
        return Ok(Vec::new());
    };
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    };
    let mut candidates: Vec<(std::time::SystemTime, u32)> = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".pid") else {
            continue;
        };
        let Ok(pid) = stem.parse::<u32>() else {
            continue;
        };
        if crate::pid_liveness::probe(pid).is_dead() {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => tracing::debug!(pid, "discovery: removed dead pid lockfile"),
                Err(e) => {
                    tracing::debug!(pid, error = %e, "discovery: dead pid lockfile remove failed")
                }
            }
            continue;
        }
        let mtime = entry
            .metadata()
            .await
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((mtime, pid));
    }
    candidates.sort_by_key(|&(mtime, _)| std::cmp::Reverse(mtime));
    Ok(candidates.into_iter().map(|(_, pid)| pid).collect())
}
