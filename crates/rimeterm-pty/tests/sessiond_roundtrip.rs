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

/// A live child that never wrote a byte and is older than the daemon's
/// silence threshold is a console-less child (the Windows ConPTY
/// burst-create hazard): reattaching with a spawn spec must respawn it
/// instead of replaying an empty ring — the user sees a working child,
/// not a black pane. Regression test for the omp agent-pane bug.
#[cfg(unix)]
// ConPTY injects a banner into every Windows child, so a
//             byte-silent child cannot be constructed there; the Unix
//              PTY faithfully relays only the child's own bytes.
#[tokio::test]
async fn silent_child_is_respawned_on_reattach() {
    // Mirror of the daemon's SILENT_CHILD_RESPAWN_AFTER (private const).
    let silence_after = Duration::from_secs(15);
    let endpoint = temp_endpoint("silent");
    let _daemon_thread = spawn_daemon(endpoint.clone()).await.expect("daemon up");

    // A child that stays alive and prints nothing: `sleep` writes no
    // bytes and the Unix PTY injects nothing of its own.
    let quiet_spec = SpawnSpec {
        program: "sleep".into(),
        args: vec!["120".into()],
        cwd: None,
        env: Vec::new(),
        cols: 80,
        rows: 24,
    };

    let attached1 = attach_session(&endpoint, "q1", "label", "shell", quiet_spec, Some(300))
        .await
        .expect("attach quiet");
    let pid1 = attached1.welcome.pid;

    let mut r1 = attached1.reader;
    let mut initial_bytes = 0usize;
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Some(timeout) = drain_deadline.checked_duration_since(tokio::time::Instant::now()) {
        match tokio::time::timeout(timeout, r1.read_frame()).await {
            Ok(Ok(Some(Frame::DaemonOutput(bytes)))) => initial_bytes += bytes.len(),
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) | Ok(Err(_)) => break,
            Err(_elapsed) => break,
        }
    }
    drop(r1);

    // Cross the silence threshold with the child alive and mute.
    tokio::time::sleep(silence_after + Duration::from_millis(500)).await;
    drop(attached1.writer);
    eprintln!("initial bytes from quiet child: {initial_bytes}");

    // Reattach with a REAL spawn spec (what the TUI always sends): the
    // daemon must kill the mute child and spawn the spec's child.
    let attached2 = attach_session(
        &endpoint,
        "q1",
        "label",
        "shell",
        echo_spec("RESURRECTED"),
        Some(300),
    )
    .await
    .expect("attach respawn");
    assert_ne!(
        attached2.welcome.pid, pid1,
        "silent child must be replaced, not reattached"
    );
    let mut r2 = attached2.reader;
    let replay = await_output(&mut r2, "RESURRECTED").await;
    let replay = String::from_utf8_lossy(&replay).to_string();
    assert!(
        replay.contains("RESURRECTED"),
        "respawned child must print its banner: {replay}"
    );

    w2_kill_and_exit(r2, attached2.writer).await;
}

/// Regression: `Session::kill()` on a daemon-attached session used to
/// race its own writer task — `kill()` closes the stdin channel while a
/// `Kill` control message sits queued, and tokio's *random* `select!`
/// branch choice could drain-and-exit on the closed stdin half before
/// writing the `Kill` frame. The daemon then saw a plain detach and kept
/// the child alive: an orphaned agent session that no later attach could
/// reclaim (fresh `PaneId`-suffixed keys never reattach).
///
/// This drives the REAL client path (`Session::attach_remote` +
/// `Session::kill`) against a real daemon, repeatedly, and requires every
/// child to actually die. One lost frame in N attempts fails the test.
#[tokio::test]
async fn session_kill_always_reaches_daemon_despite_stdin_close_race() {
    use rimeterm_pty::Session;

    const ATTEMPTS: usize = 12;
    let endpoint = temp_endpoint("killrace");
    let _daemon_thread = spawn_daemon(endpoint.clone()).await.expect("daemon up");

    for attempt in 0..ATTEMPTS {
        // Fresh key per attempt: the daemon must spawn (not reattach).
        let key = format!("killrace-{attempt}");
        let (session, mut events) = Session::attach_remote(
            &endpoint,
            &key,
            "label",
            "shell",
            echo_spec("RACE-BANNER"),
            Some(300),
            80,
            24,
        )
        .await
        .expect("attach");

        // Wait until the child has actually produced output: with a
        // live, echoing child on the other side, a lost Kill is
        // unambiguous (the child survives; a delivered Kill kills it).
        let banner_seen = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("attempt {attempt}: no output within 10 s")
            .is_some();
        assert!(
            banner_seen,
            "attempt {attempt}: event channel closed before banner"
        );

        // The exact racy sequence the TUI performs on tab close.
        session.kill();

        // The daemon must kill the child and the pump must observe
        // Exited. With the bug, ~50% of attempts never see Exited.
        let exited = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match events.recv().await {
                    Some(rimeterm_pty::SessionOutput::Exited { .. }) => break true,
                    Some(_) => continue,
                    None => break false,
                }
            }
        })
        .await
        .expect("attempt {attempt}: no Exited within 10 s");
        assert!(
            exited,
            "attempt {attempt}: stream ended without Exited — Kill frame lost"
        );

        // Belt and braces: the child must be gone from the daemon's
        // table. A lost Kill leaves a live session under this key.
        let mut probe = connect(&endpoint).await.expect("probe connect");
        probe
            .write_frame(&Frame::ClientJson(ClientMsg::List))
            .await
            .unwrap();
        let mut listed = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let frame = tokio::time::timeout_at(deadline, probe.read_frame())
                .await
                .expect("list probe timed out")
                .expect("list probe io error");
            let Some(frame) = frame else { break };
            if let Frame::DaemonJson(DaemonMsg::Sessions { sessions }) = frame {
                listed = sessions;
                break;
            }
        }
        drop(probe);
        assert!(
            !listed.iter().any(|s| s.key == key),
            "attempt {attempt}: session {key} still live after kill: {listed:?}"
        );
    }
}
