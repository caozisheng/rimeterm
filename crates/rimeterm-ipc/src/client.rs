//! Client half of rimeterm's IPC.
//!
//! One connection per invocation: connect, write one JSON request line, read
//! one JSON response line, close. Same shape used by `rimectl` and any test.

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::protocol::{encode_request, Request, Response};

/// Connect + send + await one response. Blocks (asynchronously) on the round
/// trip and returns whatever the server writes back.
pub async fn send_once(pid: u32, req: &Request) -> Result<Response> {
    #[cfg(unix)]
    {
        use tokio::net::UnixStream;
        let path = crate::endpoint::socket_for_pid(pid)
            .context("resolving IPC socket path")?;
        let stream = UnixStream::connect(&path)
            .await
            .with_context(|| format!("connect {}", path.display()))?;
        write_and_read(stream, req).await
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        let pipe = crate::endpoint::pipe_name_for_pid(pid)
            .context("resolving pipe name")?;
        // Small retry loop — the server may accept-race between drops.
        let mut attempts = 0;
        let stream = loop {
            match ClientOptions::new().open(&pipe) {
                Ok(s) => break s,
                Err(e) if attempts < 5 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    if attempts == 5 {
                        return Err(e).with_context(|| format!("connect {}", pipe));
                    }
                }
                Err(e) => return Err(e).with_context(|| format!("connect {}", pipe)),
            }
        };
        write_and_read(stream, req).await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        let _ = req;
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
    reader
        .read_line(&mut line)
        .await
        .context("read response")?;
    if line.is_empty() {
        anyhow::bail!("server closed connection without responding");
    }
    let resp: Response = serde_json::from_str(line.trim_end())
        .context("decode response")?;
    Ok(resp)
}

/// Discover the newest rimeterm pid by scanning the lockfile directory.
/// Returns `None` when no server is running (empty / missing directory).
pub async fn discover_latest_pid() -> Result<Option<u32>> {
    let Some(dir) = crate::endpoint::lockfile_dir() else {
        return Ok(None);
    };
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    };
    let mut best: Option<(std::time::SystemTime, u32)> = None;
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
        let mtime = entry
            .metadata()
            .await
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        match best {
            Some((prev, _)) if prev >= mtime => {}
            _ => best = Some((mtime, pid)),
        }
    }
    Ok(best.map(|(_, pid)| pid))
}
