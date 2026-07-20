//! Local IPC server.
//!
//! Accepts one client at a time (v1), reads one JSON request per connection,
//! dispatches through a caller-supplied handler, and writes back one response.
//! Kept intentionally sequential — palette + keymap already funnel through the
//! same command registry, so we don't need concurrent request handling yet.

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::protocol::{Request, Response, encode_response};

/// A boxed handler the server calls for each request. Returning an owned
/// [`Response`] keeps the handler `Send + Sync + 'static` friendly and lets
/// the caller run inside a `tokio::task`.
pub type Handler = std::sync::Arc<dyn Fn(Request) -> Response + Send + Sync + 'static>;

/// Spawn the IPC server on a background tokio task. Returns a shutdown handle
/// (`tx.send(()).ok()` to stop) so tests and the main app can bring it down
/// deterministically.
pub async fn spawn(pid: u32, handler: Handler) -> Result<mpsc::Sender<()>> {
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    sweep_stale_lockfiles().await;
    write_lockfile(pid).await?;

    #[cfg(unix)]
    {
        use tokio::net::UnixListener;
        let path = crate::endpoint::socket_for_pid(pid).context("resolving IPC socket path")?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        // Best-effort cleanup of a stale socket from a previous crashed run.
        let _ = tokio::fs::remove_file(&path).await;
        let listener =
            UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Ok((stream, _)) = listener.accept() => {
                        let handler = handler.clone();
                        tokio::spawn(async move { serve_one(stream, handler).await; });
                    }
                    _ = shutdown_rx.recv() => {
                        debug!("ipc server shutting down");
                        let _ = tokio::fs::remove_file(&path).await;
                        return;
                    }
                }
            }
        });
        Ok(shutdown_tx)
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ServerOptions;
        let pipe_name = crate::endpoint::pipe_name_for_pid(pid).context("resolving pipe name")?;
        tokio::spawn(async move {
            loop {
                // Every connection gets its own instance (§ NamedPipe basics).
                let server = match ServerOptions::new()
                    .first_pipe_instance(false)
                    .create(&pipe_name)
                {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "failed to create pipe instance");
                        return;
                    }
                };
                tokio::select! {
                    r = server.connect() => {
                        if let Err(e) = r {
                            warn!(error = %e, "pipe connect failed");
                            continue;
                        }
                        let handler = handler.clone();
                        tokio::spawn(async move { serve_one(server, handler).await; });
                    }
                    _ = shutdown_rx.recv() => {
                        debug!("ipc server shutting down");
                        return;
                    }
                }
            }
        });
        Ok(shutdown_tx)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        let _ = handler;
        let _ = shutdown_rx;
        anyhow::bail!("rimeterm IPC not supported on this platform");
    }
}

/// Handle a single client connection: read one request line, invoke the
/// handler, write one response line.
async fn serve_one<S>(stream: S, handler: Handler)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (rd, mut wr) = tokio::io::split(stream);
    let mut reader = BufReader::new(rd);
    let mut line = String::new();
    // `handler` is sync (see `Handler` type). Some commands block for
    // seconds (e.g. `workspace.pane.wait`), so run them on the blocking
    // pool instead of pinning a tokio worker. `spawn_blocking` panicking
    // would be a real bug — surface it as an internal-error response.
    let response = match reader.read_line(&mut line).await {
        Ok(0) => Response::err("empty request"),
        Ok(_) => match serde_json::from_str::<Request>(line.trim_end()) {
            Ok(req) => match tokio::task::spawn_blocking(move || handler(req)).await {
                Ok(resp) => resp,
                Err(e) => Response::err(format!("handler panicked: {e}")),
            },
            Err(e) => Response::err(format!("invalid JSON: {e}")),
        },
        Err(e) => Response::err(format!("read error: {e}")),
    };
    match encode_response(&response) {
        Ok(bytes) => {
            if let Err(e) = wr.write_all(&bytes).await {
                warn!(error = %e, "failed to write response");
            }
        }
        Err(e) => warn!(error = %e, "failed to encode response"),
    }
}

async fn write_lockfile(pid: u32) -> Result<()> {
    if let Some(path) = crate::endpoint::lockfile_for_pid(pid) {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(&path, format!("{pid}"))
            .await
            .with_context(|| format!("write lockfile {}", path.display()))?;
    }
    Ok(())
}

/// Walk the lockfile directory and remove any `<pid>.pid` file whose pid
/// is no longer alive. Also removes any orphan `<pid>.sock` next to it
/// (Unix only — Windows has no filesystem socket to clean).
///
/// Called on every server startup so a crash doesn't accumulate dead entries
/// that would confuse `rimectl --list-endpoints`.
async fn sweep_stale_lockfiles() {
    let Some(dir) = crate::endpoint::lockfile_dir() else {
        return;
    };
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(_) => return,
    };
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(_) => break,
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some((stem, ext)) = name.rsplit_once('.') else {
            continue;
        };
        if !matches!(ext, "pid" | "sock") {
            continue;
        }
        let Ok(pid) = stem.parse::<u32>() else {
            continue;
        };
        if crate::pid_liveness::probe(pid).is_dead() {
            if let Err(e) = tokio::fs::remove_file(&path).await {
                tracing::debug!(path = %path.display(), error = %e, "sweep: remove failed");
            } else {
                tracing::info!(pid, path = %path.display(), "sweep: removed stale lockfile");
            }
        }
    }
}
