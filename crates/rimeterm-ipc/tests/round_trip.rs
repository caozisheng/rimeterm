//! End-to-end IPC round-trip test.
//!
//! Spawns a real server (Windows named pipe or Unix uds), sends one request,
//! reads one response. Confirms:
//!
//! - Known command → `ok: true` and result payload.
//! - Unknown command → `ok: false` and error string.
//!
//! Uses a synthetic pid derived from the test process pid so parallel tests
//! don't collide.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rimeterm_ipc::{Handler, Request, Response, send_once, spawn};

fn synth_pid(offset: u32) -> u32 {
    std::process::id().wrapping_add(offset)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn known_command_round_trips() {
    let pid = synth_pid(1_100_000);
    let called = Arc::new(AtomicBool::new(false));
    let called_c = Arc::clone(&called);
    let handler: Handler = Arc::new(move |req: Request| {
        if req.cmd == "test.echo" {
            called_c.store(true, Ordering::Relaxed);
            Response::success(serde_json::json!({"cmd": "test.echo"}))
        } else {
            Response::err("unknown command")
        }
    });
    let shutdown = spawn(pid, handler).await.expect("server up");

    // Small delay so the pipe/uds is listening. On Unix `bind` is sync; on
    // Windows the ServerOptions::create + connect handshake needs a beat.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let req = Request {
        cmd: "test.echo".into(),
        args: serde_json::Value::Null,
    };
    let resp = send_once(pid, &req).await.expect("send_once");
    assert!(resp.ok, "expected ok, got {resp:?}");
    assert_eq!(resp.result, Some(serde_json::json!({"cmd": "test.echo"})));
    assert!(called.load(Ordering::Relaxed), "handler was invoked");

    // Shut the server down.
    let _ = shutdown.send(()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_command_surfaces_error() {
    let pid = synth_pid(2_100_000);
    let handler: Handler = Arc::new(|_req: Request| Response::err("nope"));
    let shutdown = spawn(pid, handler).await.expect("server up");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let req = Request {
        cmd: "no.such.command".into(),
        args: serde_json::Value::Null,
    };
    let resp = send_once(pid, &req).await.expect("send_once");
    assert!(!resp.ok);
    assert!(resp.error.as_deref().unwrap_or_default().contains("nope"));

    let _ = shutdown.send(()).await;
}
