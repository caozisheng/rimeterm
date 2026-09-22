//! Discovery regression tests: the lockfile candidate walk.
//!
//! Reproduces the v0.3.1 Explorer-redirect failure: an exited instance's
//! orphan lockfile outranks the live server in mtime order, so
//! newest-first discovery dials a dead pid and every
//! `--workspace-in-tab` redirect opens a new window instead of joining
//! the running one. The walk must skip the orphan, delete the dead
//! entry along the way, and land on the live server.
//!
//! The server must run under a pid that is genuinely alive — the walk
//! filters dead pids before connecting — so these tests use the test
//! process's own pid. [`PinnedHome`] serializes every test in this
//! binary on `ENV_LOCK` (env is process-wide) and pins
//! `RIMETERM_HOME` to a throwaway root, restoring on drop.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rimeterm_ipc::{
    Handler, Request, Response, discover_latest_pid, lockfile_for_pid, send_once, send_to_latest,
    spawn,
};

/// Pids no real process owns; the walk must see them as dead and remove
/// their lockfiles. Far above plausible pid ranges on every platform
/// (same assumption the existing `very_high_pid_is_dead` test makes).
const DEAD_A: u32 = 4_000_000;
const DEAD_B: u32 = 4_200_000;

/// Pin `RIMETERM_HOME` to a fresh temp root for the rest of the test.
///
/// Holds `rimeterm_config::test_util::ENV_LOCK` for its whole lifetime —
/// including across `.await`s in the owning test — which serializes the
/// tests in this binary against each other. That is deliberate: env is
/// process-wide, and each test's lockfile assertions read it. The guard
/// is dropped last, so [`Drop`] restores env while still under the lock.
struct PinnedHome {
    root: std::path::PathBuf,
    prev: Option<String>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl PinnedHome {
    fn pin() -> Self {
        // `ENV_LOCK` is a cross-crate `static`, so the guard borrows it
        // for `'static` and can be held across awaits.
        let _guard = rimeterm_config::test_util::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev = std::env::var("RIMETERM_HOME").ok();
        let root = std::env::temp_dir().join(format!(
            "rimeterm-ipc-walk-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(root.join("data").join("run")).unwrap();
        // SAFETY: serialized by ENV_LOCK and restored in Drop.
        unsafe { std::env::set_var("RIMETERM_HOME", &root) };
        Self { root, prev, _guard }
    }
}

impl Drop for PinnedHome {
    fn drop(&mut self) {
        // Runs before field drop, so `_guard` is still held here.
        // SAFETY: ENV_LOCK still held via `_guard`; `prev` was captured
        // under the same lock.
        match self.prev.take() {
            Some(v) => unsafe { std::env::set_var("RIMETERM_HOME", v) },
            None => unsafe { std::env::remove_var("RIMETERM_HOME") },
        }
        std::fs::remove_dir_all(&self.root).ok();
    }
}

/// Plant a lockfile for a pid that owns no server.
async fn plant_orphan(pid: u32) {
    let path = lockfile_for_pid(pid).expect("lockfile path");
    tokio::fs::write(&path, format!("{pid}"))
        .await
        .expect("write orphan lockfile");
}

fn echo_handler(hits: Arc<AtomicUsize>) -> Handler {
    Arc::new(move |req: Request| {
        if req.cmd == "test.echo" {
            hits.fetch_add(1, Ordering::Relaxed);
            Response::success(serde_json::json!({"cmd": "test.echo"}))
        } else {
            Response::err("unknown command")
        }
    })
}

/// Spawn an echo server under the test process's own (live) pid and
/// wait for its endpoint to be connectable.
async fn spawn_live_server(hits: Arc<AtomicUsize>) -> tokio::sync::mpsc::Sender<()> {
    let shutdown = spawn(std::process::id(), echo_handler(hits))
        .await
        .expect("server up");
    // The accept loop creates its pipe/uds instance inside a spawned
    // task; give it a beat (same pattern as `round_trip.rs`).
    tokio::time::sleep(Duration::from_millis(50)).await;
    shutdown
}

/// The v0.3.1 failure, reproduced: a dead pid's orphan lockfile is newer
/// than the live server's. Discovery must skip the orphan (and remove
/// it), landing on the live server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphan_lockfile_newer_than_live_server_is_skipped_and_removed() {
    let _home = PinnedHome::pin();
    let hits = Arc::new(AtomicUsize::new(0));
    let shutdown = spawn_live_server(Arc::clone(&hits)).await;

    // Orphan planted after the server started, so its mtime outranks
    // the live server's — the exact poison of the shipped bug.
    plant_orphan(DEAD_A).await;

    let found = discover_latest_pid().await.expect("discovery");
    assert_eq!(
        found,
        Some(std::process::id()),
        "must skip the dead pid and find the live server"
    );

    let orphan = lockfile_for_pid(DEAD_A).expect("path");
    assert!(
        !orphan.exists(),
        "dead pid lockfile should be removed by the walk"
    );

    let _ = shutdown.send(()).await;
}

/// Multiple orphans: the walk keeps going past every dead entry rather
/// than stopping at the first unreachable candidate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn walk_past_multiple_orphans() {
    let _home = PinnedHome::pin();
    let hits = Arc::new(AtomicUsize::new(0));
    let shutdown = spawn_live_server(Arc::clone(&hits)).await;

    plant_orphan(DEAD_A).await;
    plant_orphan(DEAD_B).await;

    let found = discover_latest_pid().await.expect("discovery");
    assert_eq!(found, Some(std::process::id()));

    let _ = shutdown.send(()).await;
}

/// `send_to_latest` is what the Explorer redirect path uses: it must
/// reach the live server even when the newest lockfile belongs to a
/// dead pid, and round-trip the request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_to_latest_prefers_live_server_behind_orphan() {
    let _home = PinnedHome::pin();
    let hits = Arc::new(AtomicUsize::new(0));
    let shutdown = spawn_live_server(Arc::clone(&hits)).await;

    plant_orphan(DEAD_A).await;

    let req = Request {
        cmd: "test.echo".into(),
        args: serde_json::Value::Null,
    };
    let resp = send_to_latest(&req)
        .await
        .expect("send_to_latest reaches live server");
    assert!(resp.ok, "expected ok, got {resp:?}");
    assert_eq!(hits.load(Ordering::Relaxed), 1, "live server handled it");

    let _ = shutdown.send(()).await;
}

/// Graceful shutdown must remove the lockfile — otherwise every exit
/// manufactures the next orphan (the self-perpetuating half of the
/// shipped bug: each failed redirect window left a newer dead entry).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_shutdown_removes_lockfile() {
    let _home = PinnedHome::pin();
    let hits = Arc::new(AtomicUsize::new(0));
    let shutdown = spawn_live_server(Arc::clone(&hits)).await;

    let lf = lockfile_for_pid(std::process::id()).expect("path");
    assert!(lf.exists(), "lockfile written on startup");

    let _ = shutdown.send(()).await;
    // Give the shutdown branch a beat to run the removal.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !lf.exists(),
        "graceful shutdown must remove the lockfile, else the next discovery dials a dead pid"
    );
}

/// `send_to_latest` with zero candidates errors instead of hanging or
/// succeeding vacuously.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_to_latest_with_no_candidates_errors() {
    let _home = PinnedHome::pin();
    let req = Request {
        cmd: "test.echo".into(),
        args: serde_json::Value::Null,
    };
    let result = send_to_latest(&req).await;
    assert!(result.is_err(), "no server anywhere: must be an error");
}

/// Direct send against a dead pid fails fast (terminal `NotFound` on
/// Windows) — the property that keeps the candidate walk cheap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_once_to_dead_pid_fails() {
    let _home = PinnedHome::pin();
    let req = Request {
        cmd: "test.echo".into(),
        args: serde_json::Value::Null,
    };
    let result = send_once(DEAD_B, &req).await;
    assert!(result.is_err());
}
