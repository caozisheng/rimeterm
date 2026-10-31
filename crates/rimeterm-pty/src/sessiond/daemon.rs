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
//! - The registry lock covers metadata, ring pushes and sender
//!   registration only. VT parsing, query replies, the input enqueue,
//!   and every blocking control-path OS call (`resize`, tree `kill`, and
//!   the master drop that closes the pseudoconsole) run outside it — a
//!   slow pane can never stall another session, an attach, or a Tokio
//!   worker. Resizes are serialized per session; kills are not, so a
//!   resize parked in `ResizePseudoConsole` can never delay termination.

use std::collections::HashMap;
use std::io::Write as _;
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
use crate::pty_control::ControlWorker;
use crate::pty_host::{NativePty, open_native_pty, terminate_tree};
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
    /// Dedicated resize + kill OS threads fed by mailboxes (see
    /// [`crate::pty_control::ControlWorker`]). The connection loop only
    /// enqueues: `ResizePseudoConsole` can park and the tree kill spawns
    /// `taskkill`, so neither may run on that task, and a parked resize
    /// must never delay termination. The resize worker also keeps the
    /// last `master` Arc after eviction alive, so pseudoconsole teardown
    /// happens on its own thread rather than under the registry lock.
    control: Arc<ControlWorker>,
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
    //
    // The sessions leave the registry under the lock but are killed and
    // dropped on the blocking pool: `kill` shells out (`taskkill`) and
    // the final master drop (`ClosePseudoConsole`) are blocking OS calls
    // that must never run under the registry lock or on a Tokio worker.
    let doomed: Vec<Entry> = {
        let mut reg = registry.lock();
        reg.sessions.drain().map(|(_, entry)| entry).collect()
    };
    if !doomed.is_empty() {
        let _ = tokio::task::spawn_blocking(move || {
            for entry in doomed {
                if let Entry::Live(s) = entry {
                    s.pty.kill();
                }
            }
        })
        .await;
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
        //
        // The guard is scoped to this block: the silent-respawn cleanup
        // below awaits a blocking task, and a parking_lot guard must not
        // be live across an await point.
        let stale = {
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
                    respawned_from_silence = true;
                    reg.sessions.remove(&key)
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
            } else {
                None
            }
        };
        // Kill and drop the stale child OFF the registry lock, on the
        // blocking pool: `kill` spawns `taskkill` and the entry's drop
        // releases its control workers / master (`ClosePseudoConsole`) —
        // all blocking OS calls. NOT breaking 'attach: fall to the slow
        // path below.
        if let Some(stale) = stale {
            let _ = tokio::task::spawn_blocking(move || {
                if let Entry::Live(s) = stale {
                    s.pty.kill();
                }
            })
            .await;
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
        // Raced: another connection spawned this key first and inserted
        // its Live entry while we were outside the lock. Kill our
        // duplicate child and attach to theirs (replay under the same
        // lock, same as the fast path).
        //
        // The probe guard is scoped so it never lives across the await
        // below.
        let raced = {
            let reg = registry.lock();
            reg.sessions
                .get(&key)
                .is_some_and(|e| matches!(e, Entry::Live(_)))
        };
        if raced {
            // Our duplicate child never got a Live entry: kill it and let
            // the PTY + its reader drop on the blocking pool — `kill`
            // (`taskkill`) and the master's `ClosePseudoConsole` are both
            // blocking OS calls, never to run under the registry lock.
            let _ = tokio::task::spawn_blocking(move || {
                session.pty.kill();
                drop(reader);
                drop(session);
            })
            .await;
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
        let mut reg = registry.lock();
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
        // Only a `Dead` record can be evicted here; still drop it outside
        // the lock so the "no PTY teardown under the registry lock" rule
        // has no exception.
        let replaced = reg.sessions.insert(key.clone(), Entry::Live(session));
        drop(reg);
        drop(replaced);
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
                // The PTY writer is a dedicated-thread queue: this is an
                // enqueue, not an OS write, so it stays on this task with
                // only the (short) writer-slot mutex held.
                let writer = {
                    let reg = registry.lock();
                    let Some(Entry::Live(s)) = reg.sessions.get(&key) else {
                        break;
                    };
                    Arc::clone(&s.pty.writer)
                };
                if let Err(e) = write_stdin(&writer, &bytes) {
                    debug!(key = %key, error = %e, "sessiond write to child failed");
                }
            }
            Frame::ClientJson(ClientMsg::Resize { cols, rows }) => {
                // Record the *requested* size for replay metadata and
                // hand the actual resize to the session's own worker,
                // both under one short lock. The enqueue never blocks, so
                // this connection keeps processing (Kill/Detach included)
                // while a previous resize is parked in
                // `ResizePseudoConsole`. The worker captured *this*
                // session's headless grid + master, so a respawn under
                // the same key can never be resized by the old worker.
                let control = {
                    let mut reg = registry.lock();
                    let Some(Entry::Live(s)) = reg.sessions.get_mut(&key) else {
                        break;
                    };
                    s.set_grid(cols, rows);
                    Arc::clone(&s.control)
                };
                if !control.resize(cols, rows) {
                    // Worker already shut down: the session is going away.
                    break;
                }
            }
            Frame::ClientJson(ClientMsg::Kill) => {
                // Explicit close: kill the child but leave the Live entry —
                // the reaper finalizes it (wait → broadcast Exited →
                // forget, no Dead record, no respawn on next attach). The
                // connection stays open until that Exited arrives.
                //
                // The kill is fired at the session's dedicated kill
                // worker: it never queues behind a resize parked in
                // `ResizePseudoConsole`, and this connection never waits
                // for the tree kill.
                if let Some(control) = request_kill(registry, &key) {
                    control.kill();
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
                // Step 1 — short registry lock: verify the generation,
                // retain the bytes and broadcast them. Atomic w.r.t.
                // attach snapshots, so replay can never interleave with
                // live output. Nothing else runs under this lock.
                let (headless, writer) = {
                    let mut reg = registry.lock();
                    let Some(Entry::Live(s)) = reg.sessions.get_mut(&key) else {
                        return;
                    };
                    if s.generation != generation {
                        // Finalized, forgotten, or re-spawned — stop pumping.
                        return;
                    }
                    s.ring.push(slice);
                    for (_, tx) in &s.attached {
                        let _ = tx.send(Frame::DaemonOutput(slice.to_vec()));
                    }
                    (Arc::clone(&s.headless), Arc::clone(&s.pty.writer))
                };
                // Step 2 — parse into the headless replica outside the
                // lock: 8 KiB of VT from one busy pane must never stall
                // every attach, every other session, or the control loop.
                {
                    let mut t = headless.lock();
                    for chunk in parse_chunks(slice) {
                        processor.advance(&mut *t, chunk);
                    }
                }
                // Step 3 — answer DA/DSR queries from the child, after the
                // bytes are retained/broadcast and parsed. The writer is
                // the queued handle, so this cannot block on the OS, and
                // no registry lock is held.
                respond_to_terminal_queries(slice, &headless, &writer);
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

    finalize_exit(&registry, &key, generation, status);
}

/// Finalize a session whose child exited: queue `Exited` after every
/// output frame the connections already hold (the pump pushes under the
/// same lock), then evict the entry.
///
/// The evicted session is *dropped outside* the registry lock: its drop
/// releases the master (`ClosePseudoConsole`) and blocks until conhost
/// lets go — exactly the call that must never run under the lock. The
/// caller is a blocking thread, so the teardown also never stalls the
/// Tokio runtime. Publishing `Exited` before the eviction keeps the
/// master alive until the exit frame is queued (no ConPTY teardown while
/// the pump may still be broadcasting).
fn finalize_exit(registry: &Arc<Mutex<Registry>>, key: &str, generation: u64, status: u32) {
    let evicted: Option<Entry> = {
        let mut reg = registry.lock();
        let Some(Entry::Live(s)) = reg.sessions.get_mut(key) else {
            return;
        };
        if s.generation != generation {
            return;
        }
        info!(key, status, "sessiond session exited");
        for (_, tx) in &s.attached {
            let _ = tx.send(Frame::DaemonJson(DaemonMsg::Exited { status }));
        }
        s.attached.clear();
        let killed = s.killed;
        if killed {
            // Explicit close: forget entirely — no respawn on next attach.
            reg.sessions.remove(key)
        } else {
            reg.sessions.insert(key.to_string(), Entry::Dead)
        }
    };
    // Off-lock, on this (blocking) thread: dropping the evicted session
    // releases the master and closes the pseudoconsole.
    drop(evicted);
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
    // Control workers: `ResizePseudoConsole` parks on a wedged ConPTY and
    // the tree kill spawns `taskkill`; both run on dedicated OS threads
    // fed by mailboxes. The resize closure holds this session's headless
    // grid + master only (never the registry), so a respawn under the
    // same key gets a fresh worker and no stale resize can cross
    // generations.
    let master_for_resize = Arc::clone(&pty.master);
    let headless_for_resize = Arc::clone(&headless);
    let killer_for_kill = Arc::clone(&pty.killer);
    let root_pid_for_kill = pty.root_pid();
    let control = match ControlWorker::spawn(
        move |cols, rows| resize_session(&headless_for_resize, &master_for_resize, cols, rows),
        move || terminate_tree(root_pid_for_kill, &killer_for_kill),
    ) {
        Ok(control) => control,
        Err(e) => {
            // Worker threads failed to start; never leave an unmanaged
            // child behind (same cleanup as the native `Session`).
            terminate_tree(pty.root_pid(), &pty.killer);
            return Err(format!("spawn `{key}`: control worker: {e}"));
        }
    };
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
        control: Arc::new(control),
    })
}

impl LiveSession {
    /// Requested grid size for replay `Welcome` frames.
    ///
    /// Set under the registry lock when a `Resize` is accepted. The PTY
    /// itself is resized asynchronously by this session's control worker
    /// (which coalesces storms), so this may briefly lead the real size.
    /// A stale worker can never overwrite it: the worker captured only
    /// *this* session's headless grid + master and never touches the
    /// registry.
    fn set_grid(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
    }
}

/// Resize the headless replica and the PTY master: grid first, then the
/// PTY (mirrors `Session::resize`). Runs on the session's resize worker
/// only — never on a Tokio worker and never under the registry lock.
fn resize_session(
    headless: &Arc<Mutex<Term<Listener>>>,
    master: &Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    cols: u16,
    rows: u16,
) {
    resize_term(&mut headless.lock(), cols, rows);
    if let Err(e) = master.lock().resize(portable_pty::PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        debug!(cols, rows, error = %e, "sessiond pty resize failed");
    }
}

/// Enqueue `bytes` on the session's PTY writer.
///
/// Production writers are queue-backed: `PtyWriter::write` only appends to
/// the dedicated writer thread's queue, so this stays on the connection
/// task (no blocking-pool hop, no OS write). The writer-slot mutex is held
/// just for the enqueue. `BrokenPipe` means the child is already gone.
fn write_stdin(
    writer: &Arc<Mutex<Option<Box<dyn std::io::Write + Send>>>>,
    bytes: &[u8],
) -> std::io::Result<()> {
    let mut slot = writer.lock();
    let Some(writer) = slot.as_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "child already exited",
        ));
    };
    writer.write_all(bytes)?;
    writer.flush()
}

/// Mark `key`'s session killed and hand back its control worker.
///
/// The `killed` flag (the reaper forgets the entry instead of recording
/// `Dead`) and the worker snapshot are taken under one short registry
/// lock; the caller then fires the worker's kill channel, which is a
/// notify — not an OS call. The kill worker owns only the killer handle
/// and pid, so a resize parked in `ResizePseudoConsole` can never delay
/// termination. `None` means the session is already gone.
fn request_kill(registry: &Arc<Mutex<Registry>>, key: &str) -> Option<Arc<ControlWorker>> {
    let mut reg = registry.lock();
    let Some(Entry::Live(s)) = reg.sessions.get_mut(key) else {
        return None;
    };
    s.killed = true;
    Some(Arc::clone(&s.control))
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

#[cfg(test)]
mod io_isolation_tests {
    //! Lock-isolation regressions for the daemon's I/O paths.
    //!
    //! Real PTYs cannot be made slow on demand, so these tests pair the
    //! daemon's actual resize/kill/pump/finalize paths with fake
    //! `portable_pty` handles whose blocking OS calls park on a gate.
    //! That makes the load-bearing invariant observable: while a pane's
    //! resize/kill/teardown is in flight, the registry lock is free and
    //! other sessions keep working. End-to-end attach/replay behaviour is
    //! covered by `tests/sessiond_roundtrip.rs`.

    use super::*;
    use std::collections::HashMap;
    use std::io;
    use std::io::Read;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use parking_lot::{Condvar, Mutex as PlMutex};
    use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty, PtySize};

    /// A blocking fake OS call that a test can hold open.
    #[derive(Default)]
    struct Gate {
        /// `(entered, released)`.
        state: PlMutex<(bool, bool)>,
        cv: Condvar,
    }

    impl Gate {
        fn enter_and_wait(&self) {
            let mut st = self.state.lock();
            st.0 = true;
            self.cv.notify_all();
            while !st.1 {
                self.cv.wait(&mut st);
            }
        }

        fn wait_entered(&self, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            let mut st = self.state.lock();
            while !st.0 {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return false;
                }
                self.cv.wait_for(&mut st, left);
            }
            true
        }

        fn release(&self) {
            let mut st = self.state.lock();
            st.1 = true;
            self.cv.notify_all();
        }
    }

    /// Wait (off the runtime workers) until `gate` has been entered.
    async fn gate_entered(gate: &Arc<Gate>) {
        let gate = Arc::clone(gate);
        let ok = tokio::task::spawn_blocking(move || gate.wait_entered(Duration::from_secs(10)))
            .await
            .expect("gate waiter panicked");
        assert!(ok, "the fake PTY op never reached its blocking point");
    }

    /// PTY stand-in: every OS call a control path makes goes through a
    /// gate (when configured) and records what happened.
    struct FakeMaster {
        resize_gate: Option<Arc<Gate>>,
        drop_gate: Option<Arc<Gate>>,
        resizes: Arc<PlMutex<Vec<(u16, u16)>>>,
    }

    impl MasterPty for FakeMaster {
        fn resize(&self, size: PtySize) -> anyhow::Result<()> {
            self.resizes.lock().push((size.cols, size.rows));
            if let Some(gate) = &self.resize_gate {
                gate.enter_and_wait();
            }
            Ok(())
        }

        fn get_size(&self) -> anyhow::Result<PtySize> {
            Ok(PtySize::default())
        }

        fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
            Ok(Box::new(io::empty()))
        }

        fn take_writer(&self) -> anyhow::Result<Box<dyn io::Write + Send>> {
            Ok(Box::new(io::sink()))
        }

        #[cfg(unix)]
        fn process_group_leader(&self) -> Option<libc::pid_t> {
            None
        }

        #[cfg(unix)]
        fn as_raw_fd(&self) -> Option<portable_pty::unix::RawFd> {
            None
        }

        #[cfg(unix)]
        fn tty_name(&self) -> Option<std::path::PathBuf> {
            None
        }
    }

    impl Drop for FakeMaster {
        fn drop(&mut self) {
            // Stand in for `ClosePseudoConsole`, which blocks until
            // conhost lets go of the pipes.
            if let Some(gate) = &self.drop_gate {
                gate.enter_and_wait();
            }
        }
    }

    struct FakeKiller {
        kills: Arc<AtomicUsize>,
    }

    impl std::fmt::Debug for FakeKiller {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("FakeKiller")
        }
    }

    impl ChildKiller for FakeKiller {
        fn kill(&mut self) -> io::Result<()> {
            self.kills.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(FakeKiller {
                kills: Arc::clone(&self.kills),
            })
        }
    }

    #[derive(Debug)]
    struct FakeChild {
        killer: FakeKiller,
    }

    impl ChildKiller for FakeChild {
        fn kill(&mut self) -> io::Result<()> {
            self.killer.kill()
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            self.killer.clone_killer()
        }
    }

    impl Child for FakeChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            Ok(Some(ExitStatus::with_exit_code(0)))
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }

        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }

    /// Configurable bits of a fake session.
    #[derive(Default)]
    struct FakePty {
        resize_gate: Option<Arc<Gate>>,
        drop_gate: Option<Arc<Gate>>,
        resizes: Arc<PlMutex<Vec<(u16, u16)>>>,
        kills: Arc<AtomicUsize>,
    }

    fn fake_session(parts: FakePty) -> LiveSession {
        let FakePty {
            resize_gate,
            drop_gate,
            resizes,
            kills,
        } = parts;
        let headless = Arc::new(Mutex::new(new_headless_term(80, 24)));
        let master: Arc<Mutex<Box<dyn MasterPty + Send>>> =
            Arc::new(Mutex::new(Box::new(FakeMaster {
                resize_gate,
                drop_gate,
                resizes,
            })));
        let killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>> =
            Arc::new(Mutex::new(Box::new(FakeKiller { kills })));
        // Same shape as production: the resize worker captures only this
        // session's headless grid + master (it never touches the
        // registry), the kill worker only the killer handle + pid.
        let resize_headless = Arc::clone(&headless);
        let resize_master = Arc::clone(&master);
        let kill_killer = Arc::clone(&killer);
        let control = ControlWorker::spawn(
            move |cols, rows| resize_session(&resize_headless, &resize_master, cols, rows),
            move || terminate_tree(None, &kill_killer),
        )
        .expect("test control workers start");
        LiveSession {
            label: "test".into(),
            kind: "test".into(),
            pty: NativePty {
                child: Arc::new(Mutex::new(Box::new(FakeChild {
                    killer: FakeKiller {
                        kills: Arc::new(AtomicUsize::new(0)),
                    },
                }))),
                killer,
                master,
                // The queued writer is a fast enqueue in production; a
                // sink keeps the pump's query answers observable in tests.
                writer: Arc::new(Mutex::new(Some(Box::new(io::sink())))),
                reader: None,
                root_pid: None,
            },
            ring: ByteRing::new(RING_CAPACITY_BYTES),
            headless,
            cols: 80,
            rows: 24,
            killed: false,
            generation: 7,
            spawned_at: std::time::Instant::now(),
            silence_respawned: false,
            attached: Vec::new(),
            control: Arc::new(control),
        }
    }

    /// Poll `pred` from the test task; the control workers are plain OS
    /// threads, so their effects land asynchronously.
    async fn eventually(mut pred: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pred() {
            assert!(Instant::now() < deadline, "condition never became true");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn control_of(registry: &Arc<Mutex<Registry>>, key: &str) -> Arc<ControlWorker> {
        let reg = registry.lock();
        let Some(Entry::Live(s)) = reg.sessions.get(key) else {
            panic!("test session missing");
        };
        Arc::clone(&s.control)
    }

    fn test_registry() -> Registry {
        Registry {
            sessions: HashMap::new(),
            conns: 1,
            next_generation: 100,
            next_conn_id: 0,
            spawn_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn registry_with(key: &str, session: LiveSession) -> Arc<Mutex<Registry>> {
        let mut reg = test_registry();
        reg.sessions.insert(key.to_string(), Entry::Live(session));
        Arc::new(Mutex::new(reg))
    }

    /// A resize request only enqueues: the connection loop is never
    /// parked in `ResizePseudoConsole`, the registry lock stays free
    /// while the PTY call is in flight, and other panes keep working.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resize_enqueue_is_non_blocking_and_off_lock() {
        let gate = Arc::new(Gate::default());
        let resizes = Arc::new(PlMutex::new(Vec::new()));
        let other_resizes = Arc::new(PlMutex::new(Vec::new()));
        let registry = registry_with(
            "slow",
            fake_session(FakePty {
                resize_gate: Some(Arc::clone(&gate)),
                resizes: Arc::clone(&resizes),
                ..FakePty::default()
            }),
        );
        registry.lock().sessions.insert(
            "other".into(),
            Entry::Live(fake_session(FakePty {
                resizes: Arc::clone(&other_resizes),
                ..FakePty::default()
            })),
        );
        let other = control_of(&registry, "other");

        // Exactly what the connection loop does: one short lock, then the
        // mailbox enqueue.
        let start = Instant::now();
        let control = {
            let mut reg = registry.lock();
            let Some(Entry::Live(s)) = reg.sessions.get_mut("slow") else {
                panic!("test session missing");
            };
            s.set_grid(120, 40);
            Arc::clone(&s.control)
        };
        assert!(control.resize(120, 40));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "resize enqueue blocked the connection loop"
        );

        gate_entered(&gate).await;
        // Parked inside the OS call: the registry lock is free...
        let start = Instant::now();
        {
            let reg = registry.lock();
            assert_eq!(reg.live_infos().len(), 2);
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "registry lock blocked behind a PTY resize"
        );
        // ...and another pane's resize still applies.
        assert!(other.resize(90, 30));
        eventually(|| *other_resizes.lock() == vec![(90u16, 30u16)]).await;

        gate.release();
        eventually(|| *resizes.lock() == vec![(120u16, 40u16)]).await;
        // Requested metadata is what replay reports.
        let reg = registry.lock();
        let Some(Entry::Live(s)) = reg.sessions.get("slow") else {
            panic!("test session missing");
        };
        assert_eq!((s.cols, s.rows), (120, 40));
    }

    /// End-to-end through the real control loop: a `Resize` parked in
    /// `ResizePseudoConsole` must not stop the *same connection* from
    /// delivering `Kill` (the loop only enqueues; it never awaits the
    /// worker), and `Detach` still closes it cleanly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kill_is_processed_while_the_same_connection_resizes() {
        let gate = Arc::new(Gate::default());
        let kills = Arc::new(AtomicUsize::new(0));
        let registry = registry_with(
            "s",
            fake_session(FakePty {
                resize_gate: Some(Arc::clone(&gate)),
                kills: Arc::clone(&kills),
                ..FakePty::default()
            }),
        );

        // Real framed connection: the server half is driven by the
        // production `serve_attached` loop, the client half writes
        // control frames the way the TUI does.
        let (server, client) = tokio::io::duplex(4096);
        let (mut reader, _server_writer) = SessionConn::from_stream(Box::new(server)).into_split();
        let mut client = SessionConn::from_stream(Box::new(client));
        let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
        let attach = Attach {
            key: "s".into(),
            label: "test".into(),
            kind: "test".into(),
            spawn: None,
        };
        let reg = Arc::clone(&registry);
        let serve =
            tokio::spawn(async move { serve_attached(&mut reader, &tx, &reg, attach, 1).await });

        // Attach fast path: replay is empty, so only Welcome arrives.
        assert!(matches!(
            rx.recv().await,
            Some(Frame::DaemonJson(DaemonMsg::Welcome(_)))
        ));

        client
            .write_frame(&Frame::ClientJson(ClientMsg::Resize {
                cols: 120,
                rows: 40,
            }))
            .await
            .expect("write resize");
        gate_entered(&gate).await;

        // Same connection, resize still parked: the loop must hand the
        // Kill to the kill worker instead of awaiting the resize.
        client
            .write_frame(&Frame::ClientJson(ClientMsg::Kill))
            .await
            .expect("write kill");
        eventually(|| kills.load(Ordering::SeqCst) == 1).await;
        {
            let reg = registry.lock();
            let Some(Entry::Live(s)) = reg.sessions.get("s") else {
                panic!("test session missing");
            };
            assert!(s.killed, "kill flag not set");
        }

        gate.release();
        client
            .write_frame(&Frame::ClientJson(ClientMsg::Detach))
            .await
            .expect("write detach");
        let outcome = tokio::time::timeout(Duration::from_secs(10), serve)
            .await
            .expect("serve_attached did not return after Detach")
            .expect("serve_attached panicked");
        assert_eq!(outcome.session_key.as_deref(), Some("s"));
    }

    /// The exiting session's PTY teardown (`ClosePseudoConsole`) happens
    /// after the registry lock is released — and after `Exited` is
    /// queued — so it cannot wedge the daemon.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exiting_session_is_dropped_outside_the_registry_lock() {
        let drop_gate = Arc::new(Gate::default());
        let registry = registry_with(
            "x",
            fake_session(FakePty {
                drop_gate: Some(Arc::clone(&drop_gate)),
                ..FakePty::default()
            }),
        );
        let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
        {
            let mut reg = registry.lock();
            let Some(Entry::Live(s)) = reg.sessions.get_mut("x") else {
                panic!("test session missing");
            };
            s.attached.push((1, tx));
        }

        let reg = Arc::clone(&registry);
        let finalize = tokio::task::spawn_blocking(move || finalize_exit(&reg, "x", 7, 0));
        gate_entered(&drop_gate).await;

        // `Exited` is already queued, the entry is evicted, and the
        // registry lock is free while the PTY drop is still blocked.
        assert!(matches!(
            rx.try_recv(),
            Ok(Frame::DaemonJson(DaemonMsg::Exited { status: 0 }))
        ));
        let start = Instant::now();
        {
            let reg = registry.lock();
            assert!(!reg.has_live_sessions());
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "registry lock blocked behind the PTY teardown"
        );

        drop_gate.release();
        finalize.await.expect("finalize task panicked");
        assert!(!registry.lock().has_live_sessions());
    }

    struct ScriptedReader {
        data: Vec<u8>,
        pos: usize,
    }

    impl Read for ScriptedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = std::cmp::min(buf.len(), self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    struct CaptureWriter(Arc<PlMutex<Vec<u8>>>);

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn capture_writer() -> (
        Arc<PlMutex<Vec<u8>>>,
        Arc<Mutex<Option<Box<dyn io::Write + Send>>>>,
    ) {
        let replies = Arc::new(PlMutex::new(Vec::new()));
        let writer: Box<dyn io::Write + Send> = Box::new(CaptureWriter(Arc::clone(&replies)));
        (replies, Arc::new(Mutex::new(Some(writer))))
    }

    /// The restructured pump still retains + broadcasts the chunk and
    /// answers DA/DSR queries from the headless replica (now parsed
    /// outside the registry lock).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pump_retains_broadcasts_and_answers_queries() {
        // CUP to (3,5), then DSR-CPR.
        let bytes = b"\x1b[3;5H\x1b[6n".to_vec();
        let (replies, writer) = capture_writer();
        let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
        let registry = registry_with("p", fake_session(FakePty::default()));
        {
            let mut reg = registry.lock();
            let Some(Entry::Live(s)) = reg.sessions.get_mut("p") else {
                panic!("test session missing");
            };
            s.generation = 7;
            s.pty.writer = writer;
            s.attached.push((1, tx));
        }

        let reg = Arc::clone(&registry);
        let scripted = bytes.clone();
        tokio::task::spawn_blocking(move || {
            child_pump(
                reg,
                "p".to_string(),
                7,
                Some(Box::new(ScriptedReader {
                    data: scripted,
                    pos: 0,
                })),
            );
        })
        .await
        .expect("pump panicked");

        assert!(
            matches!(rx.try_recv(), Ok(Frame::DaemonOutput(out)) if out == bytes),
            "live chunk was not broadcast"
        );
        let reg = registry.lock();
        let Some(Entry::Live(s)) = reg.sessions.get("p") else {
            panic!("pump removed the live session");
        };
        assert_eq!(s.ring.snapshot(), bytes);
        assert_eq!(&*replies.lock(), b"\x1b[3;5R");
    }

    /// A stale pump generation never touches the (re-born) session.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pump_stops_on_generation_mismatch() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
        let registry = registry_with("p", fake_session(FakePty::default()));
        {
            let mut reg = registry.lock();
            let Some(Entry::Live(s)) = reg.sessions.get_mut("p") else {
                panic!("test session missing");
            };
            s.generation = 7;
            s.attached.push((1, tx));
        }

        let reg = Arc::clone(&registry);
        tokio::task::spawn_blocking(move || {
            child_pump(
                reg,
                "p".to_string(),
                6, // retired generation
                Some(Box::new(ScriptedReader {
                    data: b"stale".to_vec(),
                    pos: 0,
                })),
            );
        })
        .await
        .expect("pump panicked");

        assert!(rx.try_recv().is_err(), "stale pump broadcast output");
        let reg = registry.lock();
        let Some(Entry::Live(s)) = reg.sessions.get("p") else {
            panic!("stale pump removed the live session");
        };
        assert!(s.ring.is_empty());
    }
}
