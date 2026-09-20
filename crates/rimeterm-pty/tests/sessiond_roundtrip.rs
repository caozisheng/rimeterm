//! Integration tests for the sessiond daemon lifecycle.
//!
//! These spin up `daemon::run` on a temp endpoint (UDS on Unix, a unique
//! named pipe on Windows) and drive it through the real client transport —
//! attach-with-spawn, replay, write/output echo, detach-persistence, kill.
//!
//! The daemon runs on a **dedicated OS thread with its own tokio runtime**.
//! That is not cosmetic: `daemon::run` hosts blocking child pumps whose
//! `reader.read()` only unblocks when the child dies. If the daemon lived
//! on the test's runtime, a failing test would hang in runtime shutdown
//! joining those blocked pumps — the exact deadlock these tests exist to
//! catch. A dedicated thread lets a panicked test abandon the thread; the
//! process-level cleanup below kills any stragglers.

use std::time::Duration;

use rimeterm_pty::sessiond::client::attach_session;
use rimeterm_pty::sessiond::daemon;
use rimeterm_pty::sessiond::transport::connect;
use rimeterm_pty::sessiond::transport::{FrameReader, FrameWriter};
use rimeterm_pty::sessiond::{ClientMsg, DaemonMsg, Frame, SpawnSpec};

/// Unique endpoint per test run: UDS under the system temp dir on Unix,
/// unique pipe name on Windows.
fn temp_endpoint(tag: &str) -> String {
    #[cfg(unix)]
    {
        let dir = std::env::temp_dir().join(format!(
            "rimeterm-sessiond-test-{}-{}",
            tag,
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("sessiond.sock").display().to_string()
    }
    #[cfg(windows)]
    {
        format!(
            r"\\.\pipe\rimeterm-sessiond-test-{tag}-{}",
            std::process::id()
        )
    }
}
/// Handle to a daemon hosted on a dedicated thread. Dropping it signals
/// shutdown; the daemon thread owns its runtime and tears it down itself
/// (with a timeout, so a stuck child pump cannot hang the join).
struct DaemonThread {
    _shutdown: tokio::sync::oneshot::Sender<()>,
}
/// Drive `daemon::run` on a dedicated OS thread + runtime, after disabling
/// autostart (the test binary must never `current_exe() --sessiond`
/// itself) and waiting for the endpoint to accept connections.
async fn spawn_daemon(endpoint: String) -> anyhow::Result<DaemonThread> {
    let _ = tracing_subscriber::fmt::try_init();

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let bind_endpoint = endpoint.clone();
    std::thread::Builder::new()
        .name("sessiond-under-test".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("daemon test runtime");
            rt.block_on(async move {
                // Run until told to stop (or the daemon's own idle-exit).
                tokio::select! {
                    _ = daemon::run(&bind_endpoint, Some(300)) => {},
                    _ = &mut shutdown_rx => {},
                }
            });
            // Timed shutdown: a stuck child pump cannot hang the join.
            rt.shutdown_timeout(Duration::from_secs(2));
        })
        .expect("spawn daemon thread");

    // Wait for bind: connect attempts must find a listener, never trigger
    // autostart fallbacks.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if connect(&endpoint).await.is_ok() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("daemon did not bind {endpoint} within 10s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(DaemonThread {
        _shutdown: shutdown_tx,
    })
}

/// Simplest cross-platform echo child: `sh -c` on Unix, `cmd /C` on Windows.
fn echo_spec(marker: &str) -> SpawnSpec {
    #[cfg(unix)]
    {
        SpawnSpec {
            program: "sh".into(),
            args: vec!["-c".into(), format!("echo {marker}; cat")],
            cwd: None,
            env: Vec::new(),
            cols: 80,
            rows: 24,
        }
    }
    #[cfg(windows)]
    {
        SpawnSpec {
            program: "cmd".into(),
            args: vec![
                "/C".into(),
                // `findstr .` reads stdin line by line and echoes every
                // non-empty line back — the Windows stand-in for `cat`.
                // (The previous `for /L … rem` spin never read stdin, so
                // the echo round-trip could never complete.)
                format!("echo {marker} & findstr ."),
            ],
            cwd: None,
            env: Vec::new(),
            cols: 80,
            rows: 24,
        }
    }
}

/// Read frames until a `DaemonOutput` containing `needle` arrives, or panic
/// after a generous timeout.
async fn await_output(reader: &mut FrameReader, needle: &str) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let frame = tokio::time::timeout_at(deadline, reader.read_frame())
            .await
            .expect("timed out waiting for daemon output")
            .expect("io error")
            .expect("daemon closed connection");
        if let Frame::DaemonOutput(bytes) = &frame {
            if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                return bytes.clone();
            }
        }
    }
}

#[tokio::test]
async fn attach_echo_detach_reattach_persists() {
    let endpoint = temp_endpoint("persist");
    let daemon_thread = spawn_daemon(endpoint.clone()).await.expect("daemon up");

    // Attach 1: spawn, see the banner.
    let attached = attach_session(
        &endpoint,
        "t1",
        "label",
        "shell",
        echo_spec("HELLO-ONE"),
        Some(300),
    )
    .await
    .expect("attach 1");
    assert_eq!(attached.welcome.cols, 80);
    let mut r1 = attached.reader;
    let mut w1 = attached.writer;
    let banner = await_output(&mut r1, "HELLO-ONE").await;
    assert!(String::from_utf8_lossy(&banner).contains("HELLO-ONE"));

    // Echo round trip: write, expect the bytes echoed back.
    w1.write_frame(&Frame::ClientWrite(b"ECHO-ME\n".to_vec()))
        .await
        .unwrap();
    await_output(&mut r1, "ECHO-ME").await;

    // Detach: drop the connection entirely.
    drop((r1, w1));

    // Attach 2 to the same key: Welcome reports the same pid, and the
    // replay contains the earlier bytes (ring buffer did its job).
    let attached2 = attach_session(
        &endpoint,
        "t1",
        "label",
        "shell",
        echo_spec("HELLO-TWO"),
        Some(300),
    )
    .await
    .expect("attach 2");
    let mut r2 = attached2.reader;
    assert_eq!(
        attached2.welcome.pid, attached.welcome.pid,
        "reattach must land on the same daemon-hosted child"
    );
    // Replay must contain BOTH the banner and the echoed bytes.
    let replay = await_output(&mut r2, "ECHO-ME").await;
    let replay = String::from_utf8_lossy(&replay).to_string();
    assert!(
        replay.contains("HELLO-ONE"),
        "replay lost the banner: {replay}"
    );

    // Kill via the control frame; expect Exited + connection close.
    w2_kill_and_exit(r2, attached2.writer).await;

    drop(daemon_thread);
}

async fn w2_kill_and_exit(mut r2: FrameReader, mut w2: FrameWriter) {
    w2.write_frame(&Frame::ClientJson(ClientMsg::Kill {}))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let frame = tokio::time::timeout_at(deadline, r2.read_frame())
            .await
            .expect("timed out waiting for exit")
            .expect("io error");
        match frame {
            Some(Frame::DaemonJson(DaemonMsg::Exited { .. })) => break,
            Some(_) => continue,
            None => panic!("closed before Exited"),
        }
    }
}

#[tokio::test]
async fn kill_removes_session_from_list() {
    let endpoint = temp_endpoint("kill");
    let _daemon_thread = spawn_daemon(endpoint.clone()).await.expect("daemon up");

    let attached = attach_session(
        &endpoint,
        "k1",
        "label",
        "shell",
        echo_spec("KILL-ME"),
        Some(300),
    )
    .await
    .expect("attach");
    let mut r = attached.reader;
    let mut w = attached.writer;
    await_output(&mut r, "KILL-ME").await;

    w.write_frame(&Frame::ClientJson(ClientMsg::Kill {}))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let frame = tokio::time::timeout_at(deadline, r.read_frame())
            .await
            .expect("timed out waiting for exit")
            .expect("io error");
        match frame {
            Some(Frame::DaemonJson(DaemonMsg::Exited { .. })) => break,
            Some(_) => continue,
            None => panic!("closed before Exited"),
        }
    }
}
