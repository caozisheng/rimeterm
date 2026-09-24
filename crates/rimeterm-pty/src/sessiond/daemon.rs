//! The session daemon: a per-user process that owns every PTY from birth.
//!
//! Lifetime model (tmux-style):
//!
//! - The TUI (or any client) starts the daemon if absent, then attaches.
//! - Attach with an unknown key + a [`SpawnSpec`] births the child under a
//!   fresh PTY *here*; the client never spawns children itself.
//! - Client EOF = detach. Sessions keep running; their output keeps
//!   accumulating into the per-session ring.
//! - Re-attach replays the ring into the client's replica grid, then
//!   streams live output.
//! - `Kill` kills the child and forgets the session. Natural child exit
//!   leaves a dead record so a later attach-with-spawn respawns fresh.
//! - The daemon exits after [`IDLE_EXIT_AFTER`] with zero connections and
//!   zero live sessions.
//!
//! Why the daemon parses VT at all (it was supposed to be a dumb pump):
//! detached children still issue DA/DA1/DSR-CPR queries and block until
//! answered. The client cannot answer while detached, so the daemon feeds
//! every byte into a scrollback-less headless [`Term`] purely to keep the
//! responder's cursor state accurate. Full-grid parsing still happens only
//! in the client's replica.
//!
//! Ordering guarantees (the load-bearing part):
//!
//! - Every outbound path to a connection funnels through that connection's
//!   single mpsc channel, drained by its writer task.
//! - Ring snapshot + sender registration happen under the registry lock;
//!   Welcome + replay frames are enqueued **while still holding that
//!   lock**, and the child pump pushes only under the same lock. So a live
//!   byte pushed after an attach can never overtake the replay that
//!   precedes it: real-time send order on an unbounded channel is arrival
//!   order.
//! - The child pump also owns reaping: it `wait()`s only after the reader
//!   hit EOF, so `Exited` is always the last frame after all output.

use std::collections::HashMap;
use std::sync::Arc;

use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::Processor;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::ring::ByteRing;
use super::transport::{DuplexStream, FrameWriter, Incoming, SessionConn};
use super::{
    Attach, ClientMsg, DaemonMsg, Frame, GRACE_NEVER, IDLE_EXIT_AFTER, MAX_FRAME_BODY,
    RING_CAPACITY_BYTES, SessionInfo, SpawnSpec, Welcome, grace_to_atomic,
};
use crate::pty_host::{NativePty, open_native_pty};
use crate::session::{
    Listener, PtyBackend, SessionConfig, new_headless_term, parse_chunks, resize_term,
    respond_to_terminal_queries,
};

/// A live child that has not written a single byte to its PTY for this
/// long is treated as console-less (Windows ConPTY burst-create hazard)
/// and respawned once on the next attach. Shells and agents print a
/// banner within a second or two of starting, so 15 s is a conservative
/// threshold that a healthy child never crosses.
const SILENT_CHILD_RESPAWN_AFTER: std::time::Duration = std::time::Duration::from_secs(15);

/// Pure decision for the silent-child respawn (unit-testable): a live
/// session qualifies when its ring never received a byte, it has not
/// been respawned for silence before, the child is old enough to have
/// printed its banner, and the reattaching client carries a spawn spec
/// to replace it with.
fn is_silent_child(
    ring_len: usize,
    silence_respawned: bool,
    age: std::time::Duration,
    has_spawn_spec: bool,
) -> bool {
    ring_len == 0 && !silence_respawned && age >= SILENT_CHILD_RESPAWN_AFTER && has_spawn_spec
}

#[cfg(test)]
mod silence_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn silent_child_qualifies_after_threshold() {
        assert!(is_silent_child(0, false, SILENT_CHILD_RESPAWN_AFTER, true));
        // A child that wrote anything is healthy.
        assert!(!is_silent_child(1, false, SILENT_CHILD_RESPAWN_AFTER, true));
        // Young children get the benefit of the doubt.
        assert!(!is_silent_child(
            0,
            false,
            SILENT_CHILD_RESPAWN_AFTER - Duration::from_millis(1),
            true
        ));
        // One respawn per key — no loop on deliberately quiet children.
        assert!(!is_silent_child(0, true, Duration::from_secs(3600), true));
        // No spawn spec → nothing to respawn with; attach as-is.
        assert!(!is_silent_child(0, false, Duration::from_secs(3600), false));
    }
}

/// Shared daemon state. Locked with a parking_lot mutex; never held across
/// an `.await`.
struct Registry {
    sessions: HashMap<String, Entry>,
    /// Open client connections (idle-exit bookkeeping).
    conns: usize,
    /// Monotonic spawn generation. A key can be re-spawned right after its
    /// `Dead` record lands (or its killed entry is removed) — the retired
    /// session's pump/reaper threads must not touch the newcomer. They
    /// carry the generation they were born with and check it under the
    /// lock.
    next_generation: u64,
    /// Monotonic connection id. mpsc senders have no identity, so each
    /// connection's broadcast registration is tagged with the id minted
    /// here — `unregister_sender` removes exactly that entry on detach.
    next_conn_id: u64,
    /// Serializes child spawns (openpty + CreateProcess). Windows ConPTY
    /// creation is not race-free under a burst: a TUI startup that opens
    /// 6+ pseudoconsoles within one second has produced children whose
    /// console host never came up — alive, silent forever, black pane.
    /// One spawn at a time trades ~150 ms of startup latency for never
    /// shipping a console-less child. `Arc` so a connection can hold the
    /// gate guard without borrowing the registry.
    spawn_gate: Arc<tokio::sync::Mutex<()>>,
}

enum Entry {
    Live(LiveSession),
    /// Child exited (naturally) while hosted. Kept so a later
    /// attach-with-spawn can respawn the same key; not listed by `List`.
    Dead,
}

struct LiveSession {
    label: String,
    kind: String,
    pty: NativePty,
    ring: ByteRing,
    /// Scrollback-less replica used only for DSR/CPR cursor answers.
    headless: Arc<Mutex<Term<Listener>>>,
    cols: u16,
    rows: u16,
    /// Set by an explicit `Kill`; the child pump then forgets the entry
    /// after broadcasting `Exited` (instead of leaving a Dead record).
    killed: bool,
    /// Which registry generation spawned this session; see
    /// [`Registry::next_generation`].
    generation: u64,
    /// When the child was born. Feeds the silent-child respawn check:
    /// a session whose ring is still empty long after birth produced a
    /// console-less child (observed on Windows when several ConPTY
    /// consoles are created back-to-back during a TUI startup storm —
    /// the child runs, writes into a dead console, the pane stays black).
    spawned_at: std::time::Instant,
    /// Already respawned once because the previous child never wrote a
    /// byte. Deliberately-silent children must not loop.
    silence_respawned: bool,
    /// Outbound channels of attached clients, tagged with the owning
    /// connection's id so a detach removes exactly its own entry.
    attached: Vec<(u64, mpsc::UnboundedSender<Frame>)>,
}

impl Registry {
    fn has_live_sessions(&self) -> bool {
        self.sessions.values().any(|e| matches!(e, Entry::Live(_)))
    }

    fn live_infos(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .filter_map(|(key, entry)| match entry {
                Entry::Live(s) => Some(SessionInfo {
                    key: key.clone(),
                    label: s.label.clone(),
                    kind: s.kind.clone(),
                }),
                Entry::Dead => None,
            })
            .collect()
    }
}

/// Run the daemon until it decides to shut down. Binds `endpoint`,
/// accepts clients, hosts sessions.
///
/// `grace_secs` (`None` = never): how long the daemon keeps hosting live
/// sessions after the last client connection closes, before exiting and
/// taking the children with it. Zero sessions + zero connections still
/// exits after [`IDLE_EXIT_AFTER`] regardless of grace.
pub async fn run(endpoint: &str, grace_secs: Option<u64>) -> anyhow::Result<()> {
    let grace = Arc::new(std::sync::atomic::AtomicU64::new(grace_to_atomic(
        grace_secs,
    )));
    let mut incoming = Incoming::bind(endpoint).await?;
    let registry = Arc::new(Mutex::new(Registry {
        sessions: HashMap::new(),
        conns: 0,
        next_generation: 0,
        next_conn_id: 0,
        spawn_gate: Arc::new(tokio::sync::Mutex::new(())),
    }));
    // client that had already connected to it would strand its bytes —
    // nothing would ever read them. `recv()` below is cancel-safe, so the
    // select loop can be cancelled freely without losing acceptances.
    let (accept_tx, mut accept_rx) =
        mpsc::unbounded_channel::<anyhow::Result<Box<dyn DuplexStream>>>();
    tokio::spawn(async move {
        loop {
            debug!("sessiond accept loop: instance created, waiting for client");
            let accepted = incoming.accept().await;
            debug!(
                ok = accepted.is_ok(),
                "sessiond accept loop: client connected"
            );
            let fatal = accepted.is_err();
            if accept_tx.send(accepted).is_err() {
                break; // daemon gone
            }
            if fatal {
                break;
            }
        }
    });

    let mut idle_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    idle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut idle_since: Option<std::time::Instant> = None;
    let shutdown = Arc::new(tokio::sync::Notify::new());

    loop {
        tokio::select! {
            accepted = accept_rx.recv() => {
                let Some(accepted) = accepted else { break };
                let stream = accepted?;
                idle_since = None;
                let registry = Arc::clone(&registry);
                tokio::spawn(handle_conn(
                    stream,
                    registry,
                    Arc::clone(&grace),
                    Arc::clone(&shutdown),
                ));
            }
            _ = shutdown.notified() => {
                break;
            }
            _ = idle_tick.tick() => {
                let (conns, live) = {
                    let reg = registry.lock();
                    (reg.conns, reg.has_live_sessions())
                };
                if conns > 0 {
                    idle_since = None;
                } else if !live {
                    // Nothing hosted at all — the classic fast idle exit.
                    let since = *idle_since.get_or_insert_with(std::time::Instant::now);
                    if since.elapsed() >= IDLE_EXIT_AFTER {
                        info!("sessiond idle (no conns, no live sessions) — exiting");
                        break;
                    }
                } else {
                    // Detached but still hosting: honor the grace window
                    // (None = never exit while a session is alive).
                    let secs = grace.load(std::sync::atomic::Ordering::Relaxed);
                    if secs != GRACE_NEVER {
                        let since = *idle_since.get_or_insert_with(std::time::Instant::now);
                        if since.elapsed() >= std::time::Duration::from_secs(secs) {
                            info!(
                                grace_secs = secs,
                                "sessiond grace expired (no conns, live sessions) — exiting"
                            );
                            break;
                        }
                    }
                }
            }
        }
    }

    // Live sessions at this point can only exist if `conns == 0` was
    // shadowed between tick and break — it cannot; break only happens when
    // both are zero. Kill any stragglers defensively anyway.
    {
        let mut reg = registry.lock();
        for entry in reg.sessions.values_mut() {
            if let Entry::Live(s) = entry {
                s.pty.kill();
            }
        }
    }
    // Best-effort remove of a bound Unix socket so a restart rebinds
    // cleanly (named pipes vanish with the process).
    #[cfg(unix)]
    if !super::is_pipe_endpoint(endpoint) {
        let _ = tokio::fs::remove_file(endpoint).await;
    }
    Ok(())
}

/// Handle one client connection end-to-end.
async fn handle_conn(
    stream: Box<dyn DuplexStream>,
    registry: Arc<Mutex<Registry>>,
    grace: Arc<std::sync::atomic::AtomicU64>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    let conn_id = {
        let mut reg = registry.lock();
        reg.conns += 1;
        let id = reg.next_conn_id;
        reg.next_conn_id += 1;
        id
    };
    let conn = SessionConn::from_stream(stream);
    let (mut reader, writer) = conn.into_split();
    let (tx, rx) = mpsc::unbounded_channel::<Frame>();
    let writer_task = tokio::spawn(pump_writer(conn_id, writer, rx));

    let outcome = serve_client(&mut reader, &tx, &registry, conn_id, &grace).await;

    // Detach: remove our broadcast registration FIRST (exact removal by
    // conn id), then drop our sender and await the writer draining what
    // is already queued. Removing first is what lets `writer_task` end
    // at all — without exact removal the registration held a sender
    // clone forever, the channel never closed, and this await
    // deadlocked (leaking a task per detach and stalling idle-exit).
    debug!(?outcome.session_key, conn_id, "sessiond conn: detached; unregistering sender");
    unregister_sender(&registry, &outcome.session_key, &outcome.conn_id);
    drop(tx);
    debug!(conn_id, "sessiond conn: sender dropped; awaiting writer");
    if let Err(e) = writer_task.await {
        debug!(error = %e, "sessiond writer task join error");
    }
    debug!(conn_id, "sessiond conn: writer drained; conns decremented");
    registry.lock().conns -= 1;

    // Shutdown request fully flushed (Ack included): tell the run loop to
    // exit now. Only after this connection is completely torn down, so
    // the conns count is consistent when the daemon dies.
    if outcome.shutdown {
        info!("sessiond: shutdown requested — exiting");
        shutdown.notify_waiters();
    }
}

/// Per-connection bookkeeping returned by `serve_client` so the detach path
/// can remove exactly its own sender.
struct ConnOutcome {
    session_key: Option<String>,
    /// Identity of this connection's sender inside `LiveSession::attached`.
    /// Senders have no identity, so the registration is tagged with the
    /// connection's id at attach time.
    conn_id: u64,
    /// Set by a `Shutdown` probe: after this connection's Ack has been
    /// flushed, the daemon should kill everything and exit.
    shutdown: bool,
}
/// Per-connection bookkeeping returned by `serve_client` so the detach path
/// can remove exactly its own sender.
async fn serve_client(
    reader: &mut super::transport::FrameReader,
    tx: &mpsc::UnboundedSender<Frame>,
    registry: &Arc<Mutex<Registry>>,
    conn_id: u64,
    grace: &Arc<std::sync::atomic::AtomicU64>,
) -> ConnOutcome {
    let first = match reader.read_frame().await {
        Ok(Some(frame)) => frame,
        Ok(None) => return outcome_none(conn_id),
        Err(e) => {
            warn!(error = %e, "sessiond: malformed first frame");
            return outcome_none(conn_id);
        }
    };
    debug!(conn_id, "sessiond conn: first frame read");
    match first {
        Frame::ClientJson(ClientMsg::List) => {
            let sessions = registry.lock().live_infos();
            let _ = tx.send(Frame::DaemonJson(DaemonMsg::Sessions { sessions }));
            outcome_none(conn_id)
        }
        Frame::ClientJson(ClientMsg::Shutdown) => {
            // Ack below is queued; the exit trigger fires only after this
            // connection fully drains (see `handle_conn`).
            let _ = tx.send(Frame::DaemonJson(DaemonMsg::Ack));
            ConnOutcome {
                session_key: None,
                conn_id,
                shutdown: true,
            }
        }
        Frame::ClientJson(ClientMsg::SetGrace { secs }) => {
            grace.store(grace_to_atomic(secs), std::sync::atomic::Ordering::Relaxed);
            info!(?secs, "sessiond: grace period updated");
            let _ = tx.send(Frame::DaemonJson(DaemonMsg::Ack));
            outcome_none(conn_id)
        }
        Frame::ClientJson(ClientMsg::Attach(attach)) => {
            serve_attached(reader, tx, registry, attach, conn_id).await
        }
        other => {
            warn!(
                ?other,
                "sessiond: expected Attach/List first, got something else"
            );
            let _ = tx.send(Frame::DaemonJson(DaemonMsg::Denied {
                reason: "expected Attach, List, Shutdown or SetGrace as the first frame".into(),
            }));
            outcome_none(conn_id)
        }
    }
}

fn outcome_none(conn_id: u64) -> ConnOutcome {
    ConnOutcome {
        session_key: None,
        conn_id,
        shutdown: false,
    }
}

/// Attachment established; pump control frames until EOF / Detach.
async fn serve_attached(
    reader: &mut super::transport::FrameReader,
    tx: &mpsc::UnboundedSender<Frame>,
    registry: &Arc<Mutex<Registry>>,
    attach: Attach,
    conn_id: u64,
) -> ConnOutcome {
    let key = attach.key.clone();
    debug!(conn_id, key = %key, spawn = attach.spawn.is_some(), "sessiond serve_attached: enter");

    let mut respawned_from_silence = false;
    let attached: bool = 'attach: {
        // Fast path: attach to an existing live session under one lock
        // hold — replay snapshot + sender registration are atomic w.r.t.
        // the child pump (same lock), so live output can never interleave
        // between replay chunks. The entry never leaves the map here, so
        // the pump keeps running throughout.
        {
            let mut reg = registry.lock();
            if let Some(Entry::Live(session)) = reg.sessions.get_mut(&key) {
                // Silent-console child: never wrote a byte, old enough to
                // have printed its banner. Respawn once instead of
                // replaying nothing at the user (see `is_silent_child`).
                let silent_child = is_silent_child(
                    session.ring.len(),
                    session.silence_respawned,
                    session.spawned_at.elapsed(),
                    attach.spawn.is_some(),
                );
                if silent_child {
                    warn!(
                        conn_id,
                        key = %key,
                        pid = ?session.pty.root_pid(),
                        age_secs = session.spawned_at.elapsed().as_secs(),
                        "sessiond: live child never produced output; respawning (console-less ConPTY child)"
                    );
                    // Detach every sender first so the old child's Exited
                    // broadcast reaches nobody, mark killed so the reaper
                    // forgets the entry instead of recording Dead, then
                    // remove it and fall through to the slow path which
                    // spawns a replacement under the same key.
                    session.attached.clear();
                    session.killed = true;
                    session.pty.kill();
                    reg.sessions.remove(&key);
                    respawned_from_silence = true;
                    drop(reg);
                    // NOT breaking 'attach: fall to the slow path below.
                } else {
                    // Register the sender BEFORE queuing frames: everything
                    // below happens under the same lock hold as the child
                    // pump's broadcast, so no live output can be missed
                    // between the replay snapshot and this registration.
                    // (The slow path and the spawn-race path both register;
                    // this one used to skip it — live frames and Exited
                    // never reached a reattached client.)
                    session.attached.push((conn_id, tx.clone()));
                    let snapshot = session.ring.snapshot();
                    debug!(conn_id, key = %key, snap_bytes = snapshot.len(), "sessiond fast-path: hit; queuing welcome+replay");
                    let _ = tx.send(Frame::DaemonJson(DaemonMsg::Welcome(Welcome {
                        pid: session.pty.root_pid(),
                        cols: session.cols,
                        rows: session.rows,
                        exit: None,
                    })));
                    for chunk in snapshot.chunks(MAX_FRAME_BODY) {
                        let _ = tx.send(Frame::DaemonOutput(chunk.to_vec()));
                    }
                    break 'attach true;
                }
            }
        }

        debug!(conn_id, key = %key, "sessiond fast-path: MISS; slow path");
        // Slow path: no live session under this key (or the silent-child
        // respawn above just removed one). Spawn outside the registry
        // lock, serialized by the spawn gate.
        let Some(spec) = attach.spawn.clone() else {
            let reason = match registry.lock().sessions.get(&key) {
                Some(Entry::Dead) => "session exited".to_string(),
                _ => "unknown session".to_string(),
            };
            let _ = tx.send(Frame::DaemonJson(DaemonMsg::Denied { reason }));
            return ConnOutcome {
                session_key: None,
                conn_id: 0,
                shutdown: false,
            };
        };
        // Spawn gate: one openpty+CreateProcess at a time (Windows ConPTY
        // burst-create hazard — see `Registry::spawn_gate`). The gate is
        // an async mutex and `spawn_session` is blocking, so acquire the
        // gate through a bound handle (never hold the registry lock
        // across an `.await`) and run the spawn on the blocking pool.
        let session = {
            // Bound Arc handle: the registry guard drops here, before any
            // `.await`; the gate guard then lives to the end of the block.
            let gate = {
                let reg = registry.lock();
                Arc::clone(&reg.spawn_gate)
            };
            let _gate = gate.lock().await;
            let spawn_result = tokio::task::spawn_blocking({
                let key = key.clone();
                let attach = attach.clone();
                move || spawn_session(&key, &attach, spec)
            })
            .await
            .map_err(|e| format!("spawn task panicked: {e}"))
            .and_then(|r| r);
            match spawn_result {
                Ok(session) => session,
                Err(reason) => {
                    warn!(key = %key, %reason, "sessiond spawn failed");
                    let _ = tx.send(Frame::DaemonJson(DaemonMsg::Denied { reason }));
                    return ConnOutcome {
                        session_key: None,
                        conn_id: 0,
                        shutdown: false,
                    };
                }
            }
        };
        // A respawn that exists to replace a silent child must not be
        // respawned again for silence — deliberately quiet children
        // would otherwise loop forever.
        let mut session = session;
        session.silence_respawned = respawned_from_silence;
        let reader = session.pty.reader.take();
        let mut reg = registry.lock();
        // Raced: another connection spawned this key first and inserted
        // its Live entry while we were outside the lock. Kill our
        // duplicate child and attach to theirs (replay under the same
        // lock, same as the fast path).
        if reg
            .sessions
            .get(&key)
            .is_some_and(|e| matches!(e, Entry::Live(_)))
        {
            drop(reg);
            session.pty.kill();
            let mut reg = registry.lock();
            if let Some(Entry::Live(earlier)) = reg.sessions.get_mut(&key) {
                let snapshot = earlier.ring.snapshot();
                earlier.attached.push((conn_id, tx.clone()));
                let _ = tx.send(Frame::DaemonJson(DaemonMsg::Welcome(Welcome {
                    pid: earlier.pty.root_pid(),
                    cols: earlier.cols,
                    rows: earlier.rows,
                    exit: None,
                })));
                for chunk in snapshot.chunks(MAX_FRAME_BODY) {
                    let _ = tx.send(Frame::DaemonOutput(chunk.to_vec()));
                }
            }
            break 'attach true;
        }
        session.attached.push((conn_id, tx.clone()));
        let _ = tx.send(Frame::DaemonJson(DaemonMsg::Welcome(Welcome {
            pid: session.pty.root_pid(),
            cols: session.cols,
            rows: session.rows,
            exit: None,
        })));
        // Insert BEFORE starting the pump: the pump's first read can
        // race ahead of this insert and otherwise finds no Live entry
        // (its `else { return }` arm) — silently killing the stream.
        let generation = reg.next_generation;
        reg.next_generation += 1;
        session.generation = generation;
        reg.sessions.insert(key.clone(), Entry::Live(session));
        drop(reg);
        let pump_registry = Arc::clone(registry);
        let pump_key = key.clone();
        let reaper_registry = Arc::clone(registry);
        let reaper_key = key.clone();
        // Pump and reaper each get a blocking thread: portable-pty
        // readers and `wait()` are sync. The reaper owns exit detection —
        // EOF is NOT a reliable exit signal under ConPTY (see
        // `child_reaper`). Both carry the generation so a stale thread
        // can never touch a newer session reusing this key.
        tokio::task::spawn_blocking(move || {
            child_pump(pump_registry, pump_key, generation, reader)
        });
        tokio::task::spawn_blocking(move || child_reaper(reaper_registry, reaper_key, generation));
        true
    };
    if !attached {
        return ConnOutcome {
            session_key: None,
            conn_id: 0,
            shutdown: false,
        };
    }

    // Control/input loop.
    while let Ok(Some(frame)) = reader.read_frame().await {
        match frame {
            Frame::ClientWrite(bytes) => {
                debug!(conn_id, key = %key, len = bytes.len(), "sessiond ClientWrite received");
                let reg = registry.lock();
                let Some(Entry::Live(s)) = reg.sessions.get(&key) else {
                    break;
                };
                if let Err(e) = s.pty.write(&bytes) {
                    debug!(key = %key, error = %e, "sessiond write to child failed");
                }
            }
            Frame::ClientJson(ClientMsg::Resize { cols, rows }) => {
                let mut reg = registry.lock();
                let Some(Entry::Live(s)) = reg.sessions.get_mut(&key) else {
                    break;
                };
                s.resize(cols, rows);
            }
            Frame::ClientJson(ClientMsg::Kill) => {
                // Explicit close: kill the child but leave the Live entry —
                // the reaper finalizes it (wait → broadcast Exited →
                // forget, no Dead record, no respawn on next attach). The
                // connection stays open until that Exited arrives.
                let mut reg = registry.lock();
                if let Some(Entry::Live(s)) = reg.sessions.get_mut(&key) {
                    s.killed = true;
                    s.pty.kill();
                }
                continue;
            }
            Frame::ClientJson(ClientMsg::Detach) => break,
            Frame::ClientJson(ClientMsg::Attach(_)) => {
                // Second Attach on one connection — protocol violation.
                break;
            }
            _ => {}
        }
    }

    debug!(conn_id, key = %key, "sessiond serve_attached: control loop ended");
    ConnOutcome {
        session_key: Some(key),
        conn_id,
        shutdown: false,
    }
}
/// Writer half of one connection: drain outbound frames to the socket.
async fn pump_writer(
    conn_id: u64,
    mut writer: FrameWriter,
    mut rx: mpsc::UnboundedReceiver<Frame>,
) {
    debug!(conn_id, "sessiond pump_writer: started");
    while let Some(frame) = rx.recv().await {
        debug!(conn_id, "sessiond pump_writer: writing frame");
        if let Err(e) = writer.write_frame(&frame).await {
            debug!(conn_id, error = %e, "sessiond conn write failed; closing");
            break;
        }
        debug!(conn_id, "sessiond pump_writer: frame written");
    }
    debug!(conn_id, "sessiond pump_writer: rx closed; exiting");
}

/// Blocking per-child task: read stdout → ring + broadcast + headless feed
/// + query response. Pure byte pump — exit detection belongs to the
/// reaper; this loop just drains until the pipe gives out.
fn child_pump(
    registry: Arc<Mutex<Registry>>,
    key: String,
    generation: u64,
    reader: Option<Box<dyn std::io::Read + Send>>,
) {
    let Some(mut reader) = reader else { return };
    let mut buf = [0u8; 8192];
    // Parser state must persist across reads (partial escape sequences).
    let mut processor: Processor = Processor::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                debug!(key = %key, "sessiond child stdout EOF");
                return;
            }
            Ok(n) => {
                let slice = &buf[..n];
                let mut reg = registry.lock();
                let Some(Entry::Live(s)) = reg.sessions.get_mut(&key) else {
                    return;
                };
                if s.generation != generation {
                    // Finalized, forgotten, or re-spawned — stop pumping.
                    return;
                }
                // 1. Feed the headless replica (query-answer accuracy).
                for chunk in parse_chunks(slice) {
                    let mut t = s.headless.lock();
                    processor.advance(&mut *t, chunk);
                }
                // 2. Retain + broadcast under the same lock (atomic w.r.t.
                // attach snapshots).
                s.ring.push(slice);
                for (_, tx) in &s.attached {
                    let _ = tx.send(Frame::DaemonOutput(slice.to_vec()));
                }
                // 3. Answer DA/DSR queries from the child.
                respond_to_terminal_queries(slice, &s.headless, &s.pty.writer);
            }
            Err(e) => {
                // ConPTY surfaces pipe-closed as an error at shutdown.
                debug!(key = %key, error = %e, "sessiond child read ended");
                return;
            }
        }
    }
}

/// Blocking per-child task: `wait()` on the child; on exit, finalize the
/// session (broadcast `Exited`, forget or mark Dead).
///
/// Why a dedicated waiter (mirroring the in-process `Session`): under
/// ConPTY the child's death does NOT close the master read pipe — conhost
/// owns that pipe and lives as long as the pseudoconsole handle. Reader
/// EOF therefore only ever arrives *after* the master PTY is dropped, so
/// an EOF-driven reaper can never observe the exit it is waiting for
/// (this is exactly the Windows deadlock the roundtrip tests exposed).
/// Replacing/removing the `Live` entry here drops the last master Arc →
/// `ClosePseudoConsole` → conhost tears the pipes down → the pump wakes.
fn child_reaper(registry: Arc<Mutex<Registry>>, key: String, generation: u64) {
    // Take the child handle without holding the registry lock across the
    // blocking `wait()`.
    let child = {
        let reg = registry.lock();
        let Some(Entry::Live(s)) = reg.sessions.get(&key) else {
            return;
        };
        if s.generation != generation {
            return;
        }
        Arc::clone(&s.pty.child)
    };
    let status = match child.lock().wait() {
        Ok(status) => status.exit_code(),
        Err(e) => {
            warn!(key = %key, error = %e, "sessiond child wait() failed");
            return;
        }
    };

    // Finalize: `Exited` is queued after every output frame each
    // connection already holds (pump sends under the same lock).
    let mut reg = registry.lock();
    if let Some(Entry::Live(s)) = reg.sessions.get_mut(&key) {
        if s.generation != generation {
            return;
        }
        info!(key = %key, status, "sessiond session exited");
        for (_, tx) in &s.attached {
            let _ = tx.send(Frame::DaemonJson(DaemonMsg::Exited { status }));
        }
        s.attached.clear();
        if s.killed {
            // Explicit close: forget entirely — no respawn on next attach.
            reg.sessions.remove(&key);
        } else {
            reg.sessions.insert(key, Entry::Dead);
        }
    }
}
fn spawn_session(key: &str, attach: &Attach, spec: SpawnSpec) -> Result<LiveSession, String> {
    let cfg = SessionConfig {
        program: spec.program.into(),
        args: spec.args,
        cwd: spec.cwd.map(Into::into),
        env: spec.env,
        cols: spec.cols,
        rows: spec.rows,
        backend: PtyBackend::Native,
    };
    let pty = open_native_pty(&cfg).map_err(|e| format!("spawn `{key}`: {e}"))?;
    let headless = Arc::new(Mutex::new(new_headless_term(spec.cols, spec.rows)));
    info!(key = %key, label = %attach.label, "sessiond session spawned");
    Ok(LiveSession {
        label: attach.label.clone(),
        kind: attach.kind.clone(),
        pty,
        ring: ByteRing::new(RING_CAPACITY_BYTES),
        headless,
        cols: spec.cols,
        rows: spec.rows,
        killed: false,
        generation: 0, // assigned at registry insert
        spawned_at: std::time::Instant::now(),
        silence_respawned: false,
        attached: Vec::new(),
    })
}

impl LiveSession {
    fn resize(&mut self, cols: u16, rows: u16) {
        resize_term(&mut self.headless.lock(), cols, rows);
        self.cols = cols;
        self.rows = rows;
        let _ = self.pty.resize(cols, rows);
    }
}

/// Remove this connection's sender from the session's broadcast list.
/// Exact removal by `conn_id`: mpsc senders have no identity, so the
/// registration was tagged with the connection's id at attach time.
fn unregister_sender(registry: &Arc<Mutex<Registry>>, key: &Option<String>, conn_id: &u64) {
    let Some(key) = key else { return };
    let mut reg = registry.lock();
    if let Some(Entry::Live(s)) = reg.sessions.get_mut(key) {
        s.attached.retain(|(id, _)| id != conn_id);
    }
}
