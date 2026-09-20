//! Session-transport I/O: length-prefixed frames over the same dual
//! transports as `rimeterm-ipc` (Unix domain socket / Windows named
//! pipe), but long-lived and bidirectional — the daemon pushes
//! `DaemonOutput` frames asynchronously while the client pushes
//! `ClientWrite` / `ClientJson`.
//!
//! Why not reuse `rimeterm-ipc`'s transport? That crate is
//! one-request-per-connection, line-delimited JSON, pid-addressed, and
//! carries the CommandRegistry handler contract. Sessiond needs
//! long-lived binary streams on a singleton endpoint with concurrent
//! attaches. The accept/connect *patterns* below mirror the proven
//! rimeterm-ipc code (stale-socket cleanup, pipe connect loop) but the
//! framing and lifecycle differ.

use std::io;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{Frame, decode, encode_frame, parse_head};

/// Object-safe duplex stream both platforms carry. Implemented for any
/// tokio duplex type (`UnixStream`, named-pipe server/client instances).
pub trait DuplexStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T> DuplexStream for T where T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}

/// One framed connection (either side). Split with [`into_split`] so a
/// reader task can pump output frames while another parses input frames.
pub struct SessionConn {
    read: tokio::io::ReadHalf<Box<dyn DuplexStream>>,
    write: tokio::io::WriteHalf<Box<dyn DuplexStream>>,
}

impl SessionConn {
    pub(crate) fn from_stream(stream: Box<dyn DuplexStream>) -> Self {
        let (read, write) = tokio::io::split(stream);
        Self { read, write }
    }

    /// Read one frame. `Ok(None)` = clean EOF (peer detached).
    pub async fn read_frame(&mut self) -> Result<Option<Frame>> {
        read_frame_from(&mut self.read).await
    }

    /// Write one frame.
    pub async fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        write_frame_to(&mut self.write, frame).await
    }

    /// Consume into independent halves (reader task + writer task).
    pub fn into_split(self) -> (FrameReader, FrameWriter) {
        (
            FrameReader { read: self.read },
            FrameWriter { write: self.write },
        )
    }
}

/// Reading half of a [`SessionConn`].
pub struct FrameReader {
    read: tokio::io::ReadHalf<Box<dyn DuplexStream>>,
}

impl FrameReader {
    /// Read one frame. `Ok(None)` = clean EOF.
    pub async fn read_frame(&mut self) -> Result<Option<Frame>> {
        read_frame_from(&mut self.read).await
    }
}

/// Writing half of a [`SessionConn`].
pub struct FrameWriter {
    write: tokio::io::WriteHalf<Box<dyn DuplexStream>>,
}

impl FrameWriter {
    pub async fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        write_frame_to(&mut self.write, frame).await
    }
}

/// Shared frame reader: one head + one body read. `Ok(None)` = EOF.
async fn read_frame_from(
    read: &mut tokio::io::ReadHalf<Box<dyn DuplexStream>>,
) -> Result<Option<Frame>> {
    let mut head = [0u8; 5];
    match read.read_exact(&mut head).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e).context("sessiond frame head"),
    }
    let (len, kind) = parse_head(&head)?;
    let mut body = vec![0u8; len - 1];
    read.read_exact(&mut body)
        .await
        .context("sessiond frame body")?;
    decode(kind, body)
        .map(Some)
        .context("sessiond frame decode")
}

/// Shared frame writer.
async fn write_frame_to(
    write: &mut tokio::io::WriteHalf<Box<dyn DuplexStream>>,
    frame: &Frame,
) -> Result<()> {
    let mut buf = Vec::with_capacity(64);
    encode_frame(&mut buf, frame)?;
    write.write_all(&buf).await.context("sessiond frame write")
}

/// Client-side connect. `endpoint` is a UDS path or a `\\.\pipe\` name
/// (see [`super::default_endpoint`]).
pub async fn connect(endpoint: &str) -> Result<SessionConn> {
    if super::is_pipe_endpoint(endpoint) {
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ClientOptions;
            // Retry loop mirrors rimeterm-ipc: the daemon may be between
            // pipe instances during attach storms.
            let mut attempts = 0u32;
            let pipe = loop {
                match ClientOptions::new().open(endpoint) {
                    Ok(p) => break p,
                    Err(_e) if attempts < 25 => {
                        // Any transient failure — pipe instances all busy
                        // (os error 231, while the daemon is between
                        // accepts), not-yet-created instance (NotFound),
                        // refused handshakes. Mirrors rimeterm-ipc's
                        // retry-on-any-error loop; a wrong endpoint fails
                        // all 25 attempts in ~500 ms.
                        attempts += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                    Err(e) => {
                        return Err(e).with_context(|| format!("connect {endpoint}"));
                    }
                }
            };
            Ok(SessionConn::from_stream(Box::new(pipe)))
        }
        #[cfg(not(windows))]
        {
            bail!("named-pipe endpoint `{endpoint}` on a non-Windows platform");
        }
    } else {
        #[cfg(unix)]
        {
            let stream = tokio::net::UnixStream::connect(endpoint)
                .await
                .with_context(|| format!("connect {endpoint}"))?;
            Ok(SessionConn::from_stream(Box::new(stream)))
        }
        #[cfg(not(unix))]
        {
            bail!("unix-socket endpoint `{endpoint}` on a non-Unix platform");
        }
    }
}

/// Daemon-side acceptor over the platform transport.
pub struct Incoming {
    endpoint: String,
    #[cfg(unix)]
    listener: tokio::net::UnixListener,
}

impl Incoming {
    /// Bind the daemon endpoint. Unix: creates the parent dir and
    /// removes a stale socket left by a crashed previous daemon.
    /// Windows: the named pipe instance is created per-accept instead
    /// (see [`Incoming::accept`]).
    pub async fn bind(endpoint: &str) -> Result<Self> {
        if super::is_pipe_endpoint(endpoint) {
            #[cfg(windows)]
            {
                Ok(Self {
                    endpoint: endpoint.to_string(),
                })
            }
            #[cfg(not(windows))]
            {
                bail!("named-pipe endpoint `{endpoint}` on a non-Windows platform");
            }
        } else {
            #[cfg(unix)]
            {
                let path = std::path::Path::new(endpoint);
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await.ok();
                }
                // Best-effort cleanup of a stale socket from a previous
                // crashed daemon.
                let _ = tokio::fs::remove_file(path).await;
                let listener = tokio::net::UnixListener::bind(path)
                    .with_context(|| format!("bind {}", path.display()))?;
                Ok(Self {
                    endpoint: endpoint.to_string(),
                    listener,
                })
            }
            #[cfg(not(unix))]
            {
                bail!("unix-socket endpoint `{endpoint}` on a non-Unix platform");
            }
        }
    }

    /// Await the next connection. Windows creates a fresh pipe instance
    /// per call so concurrent attaches each get their own stream.
    pub async fn accept(&mut self) -> Result<Box<dyn DuplexStream>> {
        if super::is_pipe_endpoint(&self.endpoint) {
            #[cfg(windows)]
            {
                use tokio::net::windows::named_pipe::ServerOptions;
                let server = ServerOptions::new()
                    .create(&self.endpoint)
                    .with_context(|| format!("create pipe {}", self.endpoint))?;
                server
                    .connect()
                    .await
                    .with_context(|| format!("pipe connect {}", self.endpoint))?;
                Ok(Box::new(server))
            }
            #[cfg(not(windows))]
            {
                unreachable!("pipe endpoint rejected at bind on non-Windows")
            }
        } else {
            #[cfg(unix)]
            {
                let (stream, _addr) = self.listener.accept().await.context("sessiond accept")?;
                Ok(Box::new(stream))
            }
            #[cfg(not(unix))]
            {
                unreachable!("unix endpoint rejected at bind on non-Unix")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip_over_duplex() {
        // In-memory duplex pair via tokio duplex stream.
        let (client, server) = tokio::io::duplex(8 * 1024);
        let mut c = SessionConn::from_stream(Box::new(client));
        let mut s = SessionConn::from_stream(Box::new(server));

        c.write_frame(&Frame::ClientWrite(vec![1, 2, 3]))
            .await
            .unwrap();
        let f = s.read_frame().await.unwrap().expect("frame");
        assert!(matches!(f, Frame::ClientWrite(b) if b == vec![1, 2, 3]));

        s.write_frame(&Frame::DaemonOutput(vec![9])).await.unwrap();
        let f = c.read_frame().await.unwrap().expect("frame");
        assert!(matches!(f, Frame::DaemonOutput(b) if b == vec![9]));
    }

    #[tokio::test]
    async fn eof_reads_as_none() {
        let (client, server) = tokio::io::duplex(64);
        let mut c = SessionConn::from_stream(Box::new(client));
        drop(server);
        assert!(c.read_frame().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn halves_roundtrip_independently() {
        let (client, server) = tokio::io::duplex(8 * 1024);
        let c = SessionConn::from_stream(Box::new(client));
        let s = SessionConn::from_stream(Box::new(server));
        let (mut c_read, mut c_write) = c.into_split();
        let (mut s_read, mut s_write) = s.into_split();

        c_write
            .write_frame(&Frame::ClientWrite(vec![7]))
            .await
            .unwrap();
        let f = s_read.read_frame().await.unwrap().expect("frame");
        assert!(matches!(f, Frame::ClientWrite(b) if b == vec![7]));

        s_write
            .write_frame(&Frame::DaemonOutput(vec![8]))
            .await
            .unwrap();
        let f = c_read.read_frame().await.unwrap().expect("frame");
        assert!(matches!(f, Frame::DaemonOutput(b) if b == vec![8]));
    }
}
