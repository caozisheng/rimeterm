//! Background worker for the Native [`AgtopPane`].
//!
//! Owns a single OS thread that receives [`AgtopRequest`]s over an mpsc
//! channel and returns [`AgtopResponse`]s. Follows the exact pattern of
//! [`crate::sysmon_worker`] — the pane checks generation numbers before
//! applying snapshots so stale replies land harmlessly.
//!
//! Sampling wraps [`sysinfo::System`] with a `ProcessRefreshKind` that
//! turns on `cpu`, `memory`, `cmd`, and `cwd` — enough to classify every
//! process and enrich the matched agents with human-readable context.
//! `tasks` stays disabled: the Linux per-thread task walk dwarfs the
//! rest of the pass and we don't need per-thread breakdowns here.
//!
//! Cadence is caller-driven: the pane sends a `Snapshot` request on its
//! own tick (~1500 ms, matching upstream agtop's default `--interval`).
//! An idle rimeterm therefore still blocks in `Receiver::recv` — no
//! wakeups when nobody's asking.
//!
//! [`AgtopPane`]: crate::agtop_pane::AgtopPane

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

use crate::agtop_matchers::{Matcher, UserMatcher, builtin, classify};
use crate::agtop_model::{
    AgentInfo, AgentStatus, AgtopRequest, AgtopResponse, CMDLINE_TRUNCATE, Snapshot,
};

/// Handle to the running worker thread. Cloneable only through the
/// `send` API — the sending side is `Sender` which is already
/// `Clone`, but the receive side is single-owner so we keep `AgtopWorker`
/// non-clonable to enforce it.
pub struct AgtopWorker {
    request_tx: Sender<AgtopRequest>,
    response_rx: Receiver<AgtopResponse>,
}

impl AgtopWorker {
    /// Start the worker thread. Panics only if the OS refuses to spawn
    /// the thread — mirrors `SysmonWorker::spawn`.
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<AgtopRequest>();
        let (resp_tx, resp_rx) = mpsc::channel::<AgtopResponse>();
        thread::Builder::new()
            .name("rimeterm-agtop-worker".into())
            .spawn(move || run(req_rx, resp_tx))
            .expect("spawn agtop worker");
        Self {
            request_tx: req_tx,
            response_rx: resp_rx,
        }
    }

    pub fn send(&self, request: AgtopRequest) {
        let _ = self.request_tx.send(request);
    }

    /// Drain every response the worker has produced since the last
    /// call. Non-blocking: returns an empty vec when the worker is
    /// mid-scan.
    pub fn drain(&self) -> Vec<AgtopResponse> {
        let mut out = Vec::new();
        while let Ok(response) = self.response_rx.try_recv() {
            out.push(response);
        }
        out
    }
}

fn run(req_rx: Receiver<AgtopRequest>, resp_tx: Sender<AgtopResponse>) {
    let mut sampler = Sampler::new();
    // Blocking recv — an idle rimeterm parks the worker here forever.
    while let Ok(req) = req_rx.recv() {
        match req {
            AgtopRequest::Snapshot { generation } => {
                let snapshot = sampler.refresh();
                if resp_tx
                    .send(AgtopResponse::Snapshot {
                        generation,
                        snapshot,
                    })
                    .is_err()
                {
                    // Receiver is gone — pane dropped, kill the worker.
                    return;
                }
            }
        }
    }
}

/// One `Sampler` per worker thread. Holds the `sysinfo::System` plus
/// the compiled matcher table so we don't rebuild regexes on every tick.
struct Sampler {
    system: System,
    matchers: Vec<Matcher>,
    /// User-supplied matchers — reserved slot; empty in v0.3 because
    /// nothing plumbs custom `-m` values into the pane yet.
    user_matchers: Vec<UserMatcher>,
}

impl Sampler {
    fn new() -> Self {
        // `System::new` is cheap; the actual scan happens in `refresh`.
        // `refresh_cpu_usage` primes sysinfo's delta counter so the very
        // first sample returns real CPU% instead of the flat 0.0 you'd
        // get from an unprimed backend.
        let mut system = System::new();
        system.refresh_cpu_usage();
        Self {
            system,
            matchers: builtin(),
            user_matchers: Vec::new(),
        }
    }

    fn refresh(&mut self) -> Snapshot {
        self.system.refresh_cpu_usage();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            process_refresh_kind(),
        );

        let mut agents: Vec<AgentInfo> = Vec::new();
        for (pid, process) in self.system.processes() {
            // Build the cmdline once, then classify.  `cmd()` returns
            // an argv slice of `OsString`s; join with a single space so
            // matchers see the same shape as they would on the CLI.
            let cmdline = join_cmdline(process.cmd());
            // Fall back to the raw exe path when the argv is empty
            // (Windows kernel threads, sysinfo backends without cmd
            // access). Without this, agents launched via a shim whose
            // argv is hidden would silently miss the matcher table.
            let match_target = if cmdline.is_empty() {
                process
                    .exe()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                cmdline.clone()
            };
            let Some(label) = classify(&match_target, &self.matchers, &self.user_matchers) else {
                continue;
            };

            let cwd_path = process.cwd();
            let cwd_str = cwd_path
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let project = project_label(cwd_path, process.exe());
            let cmd_stored = truncate_utf8(&match_target, CMDLINE_TRUNCATE);
            let cpu = process.cpu_usage();
            let rss = process.memory();
            let status = classify_status(cpu, process.status());

            agents.push(AgentInfo {
                label: label.to_string(),
                pid: pid.as_u32(),
                cpu,
                rss,
                uptime_sec: process.run_time(),
                cwd: cwd_str,
                project,
                cmdline: cmd_stored,
                status,
            });
        }
        // Newest-cpu-first default ordering matches the sysmon pane's
        // convention; the pane may re-sort per user preference.
        agents.sort_by(|a, b| {
            b.cpu
                .partial_cmp(&a.cpu)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.pid.cmp(&b.pid))
        });
        let total_cpu = agents.iter().map(|a| a.cpu).sum();
        let total_rss = agents.iter().map(|a| a.rss).sum();

        Snapshot {
            agents,
            total_cpu,
            total_rss,
            sampled_at: Instant::now(),
        }
    }
}

/// Refresh kind used for every `refresh_processes_specifics` call.
///
/// Turns on:
/// - `cpu` — required for the sort/status classification
/// - `memory` — RSS column
/// - `cmd` (`Always`) — argv-based matcher classification
/// - `cwd` (`Always`) — project label + filter
///
/// Everything else stays off, and `tasks` stays disabled explicitly so
/// the Linux per-thread `/proc/<pid>/task/<tid>/` walk doesn't dominate
/// the sample cost on heavily-multithreaded systems.
fn process_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_cmd(UpdateKind::Always)
        .with_cwd(UpdateKind::Always)
        .with_exe(UpdateKind::Always)
        .without_tasks()
}

/// Space-join argv into the classifier's expected shape. Uses
/// `to_string_lossy` so non-UTF-8 argv (rare, but legal on Linux and
/// occasionally Windows) round-trips as `U+FFFD` rather than dropping
/// the sample entirely.
fn join_cmdline(cmd: &[std::ffi::OsString]) -> String {
    let mut out = String::new();
    for (idx, part) in cmd.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        out.push_str(&part.to_string_lossy());
    }
    out
}

/// Short project identifier for the pane's `PROJECT` column: basename
/// of cwd when available, else basename of exe, else empty. Callers
/// that need the full path pull it directly from [`AgentInfo::cwd`].
fn project_label(cwd: Option<&Path>, exe: Option<&Path>) -> String {
    cwd.and_then(Path::file_name)
        .or_else(|| exe.and_then(Path::file_name))
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Bucket a process by activity level. Sleeping / Stop / Parked etc.
/// override the CPU% classifier so `WAIT` shows up even when a Linux
/// backend reports the last-tick CPU as 0.
fn classify_status(cpu: f32, status: ProcessStatus) -> AgentStatus {
    match status {
        ProcessStatus::Sleep
        | ProcessStatus::Stop
        | ProcessStatus::Parked
        | ProcessStatus::Wakekill
        | ProcessStatus::Waking
        | ProcessStatus::Suspended => {
            // OS-observed inactivity — trust the kernel over the
            // last-tick CPU% delta.
            AgentStatus::Sleeping
        }
        _ => {
            if cpu >= 5.0 {
                AgentStatus::Busy
            } else if cpu >= 0.5 {
                AgentStatus::Active
            } else {
                AgentStatus::Idle
            }
        }
    }
}

/// Truncate `s` to at most `max` bytes at a valid UTF-8 boundary.
/// Used to bound the stored cmdline so a hostile process with a huge
/// argv can't inflate every snapshot's memory footprint.
fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_cmdline_space_separates_argv() {
        use std::ffi::OsString;
        let cmd = vec![
            OsString::from("/usr/bin/claude"),
            OsString::from("--resume"),
            OsString::from("foo bar"),
        ];
        assert_eq!(join_cmdline(&cmd), "/usr/bin/claude --resume foo bar");
    }

    #[test]
    fn join_cmdline_empty_returns_empty() {
        assert_eq!(join_cmdline(&[]), "");
    }

    #[test]
    fn project_label_prefers_cwd_over_exe() {
        let cwd = Path::new("/home/u/projects/rimeterm");
        let exe = Path::new("/usr/bin/claude");
        assert_eq!(project_label(Some(cwd), Some(exe)), "rimeterm");
    }

    #[test]
    fn project_label_falls_back_to_exe() {
        let exe = Path::new("/usr/bin/claude");
        assert_eq!(project_label(None, Some(exe)), "claude");
    }

    #[test]
    fn project_label_empty_when_both_missing() {
        assert!(project_label(None, None).is_empty());
    }

    #[test]
    fn classify_status_busy_when_cpu_high() {
        assert_eq!(classify_status(50.0, ProcessStatus::Run), AgentStatus::Busy);
    }

    #[test]
    fn classify_status_active_between_thresholds() {
        assert_eq!(
            classify_status(1.5, ProcessStatus::Run),
            AgentStatus::Active
        );
    }

    #[test]
    fn classify_status_idle_below_lower_threshold() {
        assert_eq!(classify_status(0.1, ProcessStatus::Run), AgentStatus::Idle);
    }

    #[test]
    fn classify_status_sleeping_overrides_cpu() {
        // Even a spurious high CPU sample should classify as WAIT when
        // the OS reports the process as parked.  This can happen right
        // after a resume-from-sleep and would otherwise mislabel every
        // waiting agent as "Busy" for a full tick.
        assert_eq!(
            classify_status(50.0, ProcessStatus::Sleep),
            AgentStatus::Sleeping
        );
    }

    #[test]
    fn truncate_utf8_respects_char_boundary() {
        let s = "aあb"; // 5 bytes: 1 + 3 + 1
        assert_eq!(truncate_utf8(s, 2), "a"); // 2 lands mid-`あ`, rewinds to 1
        assert_eq!(truncate_utf8(s, 4), "aあ"); // 4 lands mid-`b`, rewinds to 4? actually 4 IS a boundary
        assert_eq!(truncate_utf8(s, 5), "aあb"); // hits ceiling
        assert_eq!(truncate_utf8(s, 100), "aあb");
    }

    #[test]
    fn worker_produces_snapshot_with_matching_generation() {
        // Real-process smoke test: spawn a worker, ask for one
        // snapshot, verify the reply matches the requested generation.
        // Not asserting `agents.len() > 0` because CI shouldn't be
        // running an AI agent — but the machinery has to work.
        let worker = AgtopWorker::spawn();
        worker.send(AgtopRequest::Snapshot { generation: 7 });

        // Wait directly on the worker's response channel. A full
        // cross-platform process scan can exceed 2 s on Windows when
        // the all-targets test suite is running concurrently (the
        // previous 20 ms polling loop flaked under that exact CI load).
        // 15 s is still a hard failure bound, but leaves enough room
        // for a contended sysinfo handle-table walk.
        let response = worker
            .response_rx
            .recv_timeout(std::time::Duration::from_secs(15))
            .expect("agtop worker never produced a snapshot within 15 s");
        let AgtopResponse::Snapshot {
            generation,
            snapshot,
        } = response;
        assert_eq!(generation, 7);
        // Snapshot must at least carry a valid timestamp — we don't
        // assert on `agents.len()` because CI has none.
        assert!(snapshot.sampled_at <= Instant::now());
    }
}
