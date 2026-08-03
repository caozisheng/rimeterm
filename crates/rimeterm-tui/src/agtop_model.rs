//! Owned data model for the Native [`AgtopPane`].
//!
//! Everything here is a plain `Send + Sync` DTO the worker fills in
//! and hands to the pane. No `sysinfo::System` handle crosses the
//! worker boundary — the pane only ever sees the snapshotted result.
//!
//! Field selection mirrors upstream `agtop::model::Agent` (v2.4.24,
//! MIT) so cross-referencing behaviour with `agtop --json` stays 1:1.
//! We drop the truly OS-specific fields (`writing_files`,
//! `reading_files`, native TCP counts) because they require the
//! per-OS FFI stack upstream ships and we intentionally don't. The
//! pane surfaces what it can from `sysinfo` + Claude JSONL enrichment.
//!
//! [`AgtopPane`]: crate::agtop_pane::AgtopPane

use std::time::Instant;

use crate::agtop_pricing::CostBasis;
use crate::agtop_session::SessionState;

/// Recent-CPU / recent-tokens ring capacity. 24 samples × 1.5 s
/// interval ≈ 36 s of history — enough to spot a token burst without
/// stealing more than one screen-worth of per-agent state.
pub const HISTORY_CAP: usize = 24;

/// Coarse activity classification used by the table row's status
/// column. Mirrors upstream `agtop::Status` so the badges line up 1:1
/// with the standalone binary. Blending process CPU with session
/// activity happens in [`AgentStatus::classify`] — the worker never
/// hands the pane a raw CPU-only bucket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentStatus {
    /// Live pid + fresh JSONL write (≤30 s), or CPU% ≥ 10, or any
    /// tool in flight.
    Busy,
    /// Live pid + one or more `Task` / `Agent` subagents in flight.
    Spawning,
    /// Live pid + session touched in the last 5 min, or CPU% ≥ 3.
    Active,
    /// Live pid, otherwise quiet.
    Idle,
    /// No live pid, session touched in the last 24 h. Model paused
    /// waiting on the user.
    Waiting,
    /// Session ended (`stop_reason: end_turn`). Row lingers so a
    /// just-finished agent doesn't pop out of the pane instantly.
    Completed,
    /// Session older than 24 h.
    Stale,
}

impl AgentStatus {
    /// One-char glyph for the status column — same visual language
    /// upstream `agtop` uses so screenshots stay legible.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Busy | Self::Active | Self::Spawning => "●",
            Self::Idle => "○",
            Self::Waiting => "◌",
            Self::Completed => "✓",
            Self::Stale => "·",
        }
    }

    /// Four-char label so the status column is a fixed width
    /// regardless of variant.
    pub fn label(self) -> &'static str {
        match self {
            Self::Busy => "BUSY",
            Self::Spawning => "SPWN",
            Self::Active => "ACTV",
            Self::Idle => "IDLE",
            Self::Waiting => "WAIT",
            Self::Completed => "DONE",
            Self::Stale => "STAL",
        }
    }

    /// Sort rank — busy first, stale last. Used by [`SortKey::Status`]
    /// and by the smart-sort tiebreaker.
    pub fn rank(self) -> u8 {
        match self {
            Self::Busy => 0,
            Self::Spawning => 1,
            Self::Active => 2,
            Self::Idle => 3,
            Self::Waiting => 4,
            Self::Completed => 5,
            Self::Stale => 6,
        }
    }

    /// Fold `(cpu%, session-state)` into a single pane-visible status.
    ///
    /// Session state wins when the transcript reader has meaningful
    /// signal (subagent in flight, mid-turn write, stop_reason). CPU
    /// is the fallback so pure-`sysinfo` rows without a matching
    /// session file still classify sensibly.
    pub fn classify(cpu: f32, session: Option<SessionState>) -> Self {
        // Session signal — strongest bucket wins.
        if let Some(s) = session {
            return match s {
                SessionState::Spawning => Self::Spawning,
                SessionState::Busy => Self::Busy,
                SessionState::Active => Self::Active,
                SessionState::Idle => {
                    // A live-but-quiet Claude with real CPU usage
                    // should read as Active, not Idle — matches
                    // upstream's CPU-blended classifier.
                    if cpu >= 3.0 { Self::Active } else { Self::Idle }
                }
                SessionState::Waiting => Self::Waiting,
                SessionState::Completed => Self::Completed,
                SessionState::Stale => Self::Stale,
            };
        }
        // No session — bucket by CPU alone.
        if cpu >= 10.0 {
            Self::Busy
        } else if cpu >= 3.0 {
            Self::Active
        } else {
            // Below the Active threshold. Both the "some CPU but
            // small" and "truly quiescent" buckets read as Idle in
            // the pane — we don't distinguish them without session
            // signal.
            Self::Idle
        }
    }
}

/// Sort key for the agent table.
///
/// `Smart` is the default — orders by status rank, then by tokens,
/// then by CPU%. Matches upstream `agtop`'s `--sort smart` behaviour
/// so a "just launch it" invocation always leads with the row the
/// user is most likely to care about.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortKey {
    Smart,
    Cpu,
    Memory,
    Tokens,
    Pid,
    Label,
    Uptime,
    Status,
}

impl SortKey {
    /// Short label surfaced in the pane title (`sort:cpu↓`, etc.).
    pub fn short(self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::Cpu => "cpu",
            Self::Memory => "mem",
            Self::Tokens => "tokens",
            Self::Pid => "pid",
            Self::Label => "agent",
            Self::Uptime => "uptime",
            Self::Status => "status",
        }
    }
}

/// Ascending / descending toggle. Same-key second press flips.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

impl SortOrder {
    pub fn flip(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }

    pub fn arrow(self) -> &'static str {
        match self {
            Self::Ascending => "↑",
            Self::Descending => "↓",
        }
    }
}

/// One detected AI-agent process. Field selection follows upstream
/// `agtop::model::Agent` minus the pieces we don't display (native
/// FFI file lists, per-OS-specific network counts).
#[derive(Clone, Debug)]
pub struct AgentInfo {
    // --- process shape -----------------------------------------------------
    pub label: String,
    pub pid: u32,
    /// CPU% averaged over the last worker sample — same normalisation
    /// `sysinfo` uses (0..N*100 where N = core count).
    pub cpu: f32,
    /// Resident set size in bytes.
    pub rss: u64,
    /// Seconds since the process started.
    pub uptime_sec: u64,
    /// Absolute cwd path (basename shown in the table, full path in
    /// the detail popup). Empty when unavailable.
    pub cwd: String,
    /// Executable path. Populated when `sysinfo` returns one; empty
    /// otherwise.
    pub exe: String,
    /// Short project label — cwd basename or, when cwd is empty, the
    /// executable name. Pre-computed so the pane doesn't `basename`
    /// per frame.
    pub project: String,
    /// Full cmdline joined by single spaces — for filter matching
    /// and the detail popup. Truncated at [`CMDLINE_TRUNCATE`].
    pub cmdline: String,
    /// Parent pid — surfaces the launcher (`zsh`, `tmux`, VS Code, …)
    /// in the detail popup.
    pub ppid: u32,
    /// Parent process name (`zsh`, `bash`, `tmux`, `code`, …).
    /// Resolved from `sysinfo::Process::name()` for the ppid.
    pub ppid_name: String,

    // --- classification / session enrichment -------------------------------
    pub status: AgentStatus,
    /// Populated when the classifier flagged the cmdline. Empty
    /// otherwise. The pane paints a `▍` marker at the start of the
    /// row when non-empty.
    pub dangerous_flag: String,
    pub dangerous: bool,
    pub model: Option<String>,
    pub session_id: Option<String>,
    /// Unix ms — first-record timestamp from the JSONL. Diverges from
    /// process start time when the agent was invoked with `--resume`.
    pub session_started_ms: u64,
    pub current_tool: Option<String>,
    pub current_task: Option<String>,
    pub subagents: u32,
    pub in_flight_subagents: Vec<String>,
    pub recent_activity: Vec<String>,

    // --- token / cost / context -------------------------------------------
    /// Full input bucket (raw + cache_read + cache_write).
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_cache_read: u64,
    pub tokens_cache_write: u64,
    /// Convenience roll-up shown in the `TOKENS` column.
    pub tokens_total: u64,
    pub cost_usd: f64,
    pub cost_basis: CostBasis,
    /// Latest-turn input window size. Drives the context-fill bar.
    pub context_used: u64,
    pub context_limit: u64,
    pub tool_counts: Vec<(String, u32)>,

    // --- discovery signal --------------------------------------------------
    /// Claude Code skills visible to this cwd. Empty for non-Claude
    /// agents.
    pub loaded_skills: Vec<String>,
    /// Claude Code plugins enabled host-wide. Same value for every
    /// Claude row in the snapshot.
    pub loaded_plugins: Vec<String>,

    // --- history rings (24 samples each, oldest → newest) ------------------
    /// Recent CPU% samples driving the inline sparkline.
    pub cpu_history: Vec<f64>,
    /// Recent per-tick token deltas driving the popup's token-rate
    /// sparkline. Always length [`HISTORY_CAP`] once the agent has
    /// been observed that many ticks; leading zeros before then.
    pub tokens_history: Vec<f64>,
}

/// Cap on the stored cmdline so a hostile process with a megabyte-
/// scale argv can't inflate every snapshot's memory footprint.
/// 512 chars is comfortably more than any real agent invocation.
pub const CMDLINE_TRUNCATE: usize = 512;

/// Complete snapshot handed from worker → pane. Kept flat / cloneable
/// so the pane can hold onto the "latest" and re-render without
/// touching the worker again.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub agents: Vec<AgentInfo>,
    /// Sum of CPU% across every detected agent — cached because the
    /// title bar re-uses it per frame.
    pub total_cpu: f32,
    /// Sum of RSS bytes across every detected agent.
    pub total_rss: u64,
    /// Sum of tokens across every detected agent.
    pub total_tokens: u64,
    /// Sum of session-cost USD across every detected agent (Api
    /// basis only — Local / Unknown rows contribute 0).
    pub total_cost_usd: f64,
    pub sampled_at: Instant,
    /// Unix ms wall-clock time the snapshot was taken. Used by the
    /// detail popup for "session started N ago" lines that survive
    /// system-clock adjustments better than `Instant` deltas.
    pub sampled_at_ms: u64,
}

impl Snapshot {
    pub fn empty() -> Self {
        Self {
            agents: Vec::new(),
            total_cpu: 0.0,
            total_rss: 0,
            total_tokens: 0,
            total_cost_usd: 0.0,
            sampled_at: Instant::now(),
            sampled_at_ms: 0,
        }
    }

    /// Header aggregates for the pane title bar. Kept small so a
    /// narrow terminal doesn't spill the sort indicator off-screen.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Number of rows in a given status bucket. Used for the title
    /// summary (`3 busy · 2 active`).
    pub fn count_status(&self, status: AgentStatus) -> usize {
        self.agents.iter().filter(|a| a.status == status).count()
    }
}

/// Filtered + sorted derivation of [`Snapshot::agents`]. Recomputed
/// on each render so cursor movement / sort flips are cheap; a full
/// re-scan still runs in microseconds for a realistic agent count.
#[derive(Clone, Debug)]
pub struct AgentView {
    pub sort_key: SortKey,
    pub order: SortOrder,
    pub filter: Option<String>,
    pub rows: Vec<AgentInfo>,
}

impl AgentView {
    pub fn from_snapshot(
        snapshot: &Snapshot,
        sort_key: SortKey,
        order: SortOrder,
        filter: Option<&str>,
    ) -> Self {
        let filter_owned = filter.map(|s| s.to_string());
        let mut rows: Vec<AgentInfo> = snapshot
            .agents
            .iter()
            .filter(|row| Self::row_matches(row, filter))
            .cloned()
            .collect();
        rows.sort_by(|a, b| Self::compare(a, b, sort_key));
        if matches!(order, SortOrder::Descending) {
            rows.reverse();
        }
        Self {
            sort_key,
            order,
            filter: filter_owned,
            rows,
        }
    }

    fn row_matches(row: &AgentInfo, filter: Option<&str>) -> bool {
        match filter {
            None => true,
            Some(f) if f.is_empty() => true,
            Some(f) => {
                // Numeric filter → exact pid match (mirrors sysmon).
                if let Ok(pid) = f.parse::<u32>() {
                    return row.pid == pid;
                }
                let needle = f.to_lowercase();
                row.label.to_lowercase().contains(&needle)
                    || row.project.to_lowercase().contains(&needle)
                    || row.cmdline.to_lowercase().contains(&needle)
                    || row
                        .model
                        .as_deref()
                        .map(|m| m.to_lowercase().contains(&needle))
                        .unwrap_or(false)
                    || row
                        .current_tool
                        .as_deref()
                        .map(|t| t.to_lowercase().contains(&needle))
                        .unwrap_or(false)
            }
        }
    }

    fn compare(a: &AgentInfo, b: &AgentInfo, key: SortKey) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match key {
            // Smart: status rank → tokens → cpu → pid. Ascending order
            // puts the busiest row first BECAUSE the pane defaults to
            // Descending — the `.reverse()` in `from_snapshot` flips
            // it. Users hitting Tab to switch to Ascending get the
            // idle-first / oldest-first view they expect.
            SortKey::Smart => a
                .status
                .rank()
                .cmp(&b.status.rank())
                .reverse() // busiest → highest sort key
                .then_with(|| a.tokens_total.cmp(&b.tokens_total))
                .then_with(|| a.cpu.partial_cmp(&b.cpu).unwrap_or(Ordering::Equal))
                .then_with(|| b.pid.cmp(&a.pid)),
            SortKey::Cpu => a
                .cpu
                .partial_cmp(&b.cpu)
                .unwrap_or(Ordering::Equal)
                .then(a.pid.cmp(&b.pid)),
            SortKey::Memory => a.rss.cmp(&b.rss).then(a.pid.cmp(&b.pid)),
            SortKey::Tokens => a.tokens_total.cmp(&b.tokens_total).then(a.pid.cmp(&b.pid)),
            SortKey::Pid => a.pid.cmp(&b.pid),
            SortKey::Label => a
                .label
                .to_lowercase()
                .cmp(&b.label.to_lowercase())
                .then(a.pid.cmp(&b.pid)),
            SortKey::Uptime => a.uptime_sec.cmp(&b.uptime_sec).then(a.pid.cmp(&b.pid)),
            SortKey::Status => a
                .status
                .rank()
                .cmp(&b.status.rank())
                .then(a.pid.cmp(&b.pid)),
        }
    }
}

/// Worker request. Snapshot requests carry a generation so late
/// replies (worker delayed by a slow scan) land harmlessly when a
/// fresh request has already superseded them.
#[derive(Clone, Debug)]
pub enum AgtopRequest {
    Snapshot { generation: u64 },
}

/// Worker response, one-to-one with a request.
#[derive(Clone, Debug)]
pub enum AgtopResponse {
    Snapshot { generation: u64, snapshot: Snapshot },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(label: &str, pid: u32, cpu: f32, rss: u64, uptime: u64, tokens: u64) -> AgentInfo {
        AgentInfo {
            label: label.into(),
            pid,
            cpu,
            rss,
            uptime_sec: uptime,
            cwd: String::new(),
            exe: String::new(),
            project: label.into(),
            cmdline: String::new(),
            ppid: 0,
            ppid_name: String::new(),
            status: AgentStatus::classify(cpu, None),
            dangerous_flag: String::new(),
            dangerous: false,
            model: None,
            session_id: None,
            session_started_ms: 0,
            current_tool: None,
            current_task: None,
            subagents: 0,
            in_flight_subagents: Vec::new(),
            recent_activity: Vec::new(),
            tokens_input: 0,
            tokens_output: 0,
            tokens_cache_read: 0,
            tokens_cache_write: 0,
            tokens_total: tokens,
            cost_usd: 0.0,
            cost_basis: CostBasis::Unknown,
            context_used: 0,
            context_limit: 200_000,
            tool_counts: Vec::new(),
            loaded_skills: Vec::new(),
            loaded_plugins: Vec::new(),
            cpu_history: Vec::new(),
            tokens_history: Vec::new(),
        }
    }

    #[test]
    fn status_glyphs_are_single_char() {
        for s in [
            AgentStatus::Busy,
            AgentStatus::Spawning,
            AgentStatus::Active,
            AgentStatus::Idle,
            AgentStatus::Waiting,
            AgentStatus::Completed,
            AgentStatus::Stale,
        ] {
            assert_eq!(s.glyph().chars().count(), 1);
            assert_eq!(s.label().len(), 4);
        }
    }

    #[test]
    fn status_rank_orders_busy_first_stale_last() {
        let mut order = vec![
            AgentStatus::Stale,
            AgentStatus::Busy,
            AgentStatus::Idle,
            AgentStatus::Spawning,
            AgentStatus::Active,
        ];
        order.sort_by_key(|s| s.rank());
        assert_eq!(order[0], AgentStatus::Busy);
        assert_eq!(order[1], AgentStatus::Spawning);
        assert_eq!(order[2], AgentStatus::Active);
        assert_eq!(*order.last().unwrap(), AgentStatus::Stale);
    }

    #[test]
    fn session_signal_beats_cpu_bucket() {
        // A live pid with an in-flight subagent should read as
        // Spawning even if its CPU is zero.
        assert_eq!(
            AgentStatus::classify(0.0, Some(SessionState::Spawning)),
            AgentStatus::Spawning
        );
        // A session claiming Idle but with real CPU usage upgrades
        // to Active.
        assert_eq!(
            AgentStatus::classify(5.0, Some(SessionState::Idle)),
            AgentStatus::Active
        );
    }

    #[test]
    fn cpu_fallback_when_no_session() {
        assert_eq!(AgentStatus::classify(20.0, None), AgentStatus::Busy);
        assert_eq!(AgentStatus::classify(5.0, None), AgentStatus::Active);
        assert_eq!(AgentStatus::classify(0.0, None), AgentStatus::Idle);
    }

    #[test]
    fn empty_filter_matches_every_row() {
        let snap = Snapshot {
            agents: vec![row("claude", 1, 3.0, 100, 60, 0)],
            ..Snapshot::empty()
        };
        let v = AgentView::from_snapshot(&snap, SortKey::Cpu, SortOrder::Descending, Some(""));
        assert_eq!(v.rows.len(), 1);
    }

    #[test]
    fn filter_by_pid_is_exact_match() {
        let snap = Snapshot {
            agents: vec![
                row("claude", 1, 0.0, 0, 0, 0),
                row("codex", 42, 0.0, 0, 0, 0),
            ],
            ..Snapshot::empty()
        };
        let v = AgentView::from_snapshot(&snap, SortKey::Pid, SortOrder::Ascending, Some("42"));
        assert_eq!(v.rows.len(), 1);
        assert_eq!(v.rows[0].pid, 42);
    }

    #[test]
    fn filter_matches_label_case_insensitive() {
        let snap = Snapshot {
            agents: vec![
                row("Claude", 1, 0.0, 0, 0, 0),
                row("codex", 2, 0.0, 0, 0, 0),
            ],
            ..Snapshot::empty()
        };
        let v = AgentView::from_snapshot(&snap, SortKey::Pid, SortOrder::Ascending, Some("claude"));
        assert_eq!(v.rows.len(), 1);
        assert_eq!(v.rows[0].pid, 1);
    }

    #[test]
    fn filter_matches_model_or_current_tool() {
        let mut a = row("claude", 1, 0.0, 0, 0, 0);
        a.model = Some("claude-opus-4-7".into());
        let mut b = row("codex", 2, 0.0, 0, 0, 0);
        b.current_tool = Some("Bash".into());
        let snap = Snapshot {
            agents: vec![a, b],
            ..Snapshot::empty()
        };
        // Model hit.
        let v = AgentView::from_snapshot(&snap, SortKey::Pid, SortOrder::Ascending, Some("opus"));
        assert_eq!(v.rows.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![1]);
        // Tool hit.
        let v = AgentView::from_snapshot(&snap, SortKey::Pid, SortOrder::Ascending, Some("bash"));
        assert_eq!(v.rows.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn sort_cpu_descending_puts_hottest_first() {
        let snap = Snapshot {
            agents: vec![
                row("aider", 1, 1.0, 0, 0, 0),
                row("claude", 2, 5.0, 0, 0, 0),
                row("codex", 3, 3.0, 0, 0, 0),
            ],
            ..Snapshot::empty()
        };
        let v = AgentView::from_snapshot(&snap, SortKey::Cpu, SortOrder::Descending, None);
        assert_eq!(
            v.rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn sort_tokens_descending_puts_biggest_bill_first() {
        let snap = Snapshot {
            agents: vec![
                row("a", 1, 0.0, 0, 0, 500),
                row("b", 2, 0.0, 0, 0, 5000),
                row("c", 3, 0.0, 0, 0, 50),
            ],
            ..Snapshot::empty()
        };
        let v = AgentView::from_snapshot(&snap, SortKey::Tokens, SortOrder::Descending, None);
        assert_eq!(
            v.rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![2, 1, 3]
        );
    }

    #[test]
    fn sort_smart_prefers_busy_then_tokens() {
        let mut a = row("a", 1, 0.0, 0, 0, 10_000); // Idle
        a.status = AgentStatus::Idle;
        let mut b = row("b", 2, 0.0, 0, 0, 1_000); // Busy
        b.status = AgentStatus::Busy;
        let mut c = row("c", 3, 0.0, 0, 0, 100); // Busy
        c.status = AgentStatus::Busy;
        let snap = Snapshot {
            agents: vec![a, b, c],
            ..Snapshot::empty()
        };
        let v = AgentView::from_snapshot(&snap, SortKey::Smart, SortOrder::Descending, None);
        // Both busys before the idle; within busy, higher tokens
        // wins.
        assert_eq!(
            v.rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn sort_status_prefers_busy_over_idle() {
        let mut a = row("a", 1, 0.0, 0, 0, 0);
        a.status = AgentStatus::Idle;
        let mut b = row("b", 2, 0.0, 0, 0, 0);
        b.status = AgentStatus::Busy;
        let mut c = row("c", 3, 0.0, 0, 0, 0);
        c.status = AgentStatus::Active;
        let snap = Snapshot {
            agents: vec![a, b, c],
            ..Snapshot::empty()
        };
        let v = AgentView::from_snapshot(&snap, SortKey::Status, SortOrder::Ascending, None);
        assert_eq!(
            v.rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn sort_order_flip_toggles() {
        assert_eq!(SortOrder::Ascending.flip(), SortOrder::Descending);
        assert_eq!(SortOrder::Descending.flip(), SortOrder::Ascending);
        assert_eq!(SortOrder::Ascending.arrow(), "↑");
        assert_eq!(SortOrder::Descending.arrow(), "↓");
    }

    #[test]
    fn snapshot_count_status_counts_matching_rows() {
        let mut a = row("a", 1, 0.0, 0, 0, 0);
        a.status = AgentStatus::Busy;
        let mut b = row("b", 2, 0.0, 0, 0, 0);
        b.status = AgentStatus::Busy;
        let mut c = row("c", 3, 0.0, 0, 0, 0);
        c.status = AgentStatus::Idle;
        let snap = Snapshot {
            agents: vec![a, b, c],
            ..Snapshot::empty()
        };
        assert_eq!(snap.count_status(AgentStatus::Busy), 2);
        assert_eq!(snap.count_status(AgentStatus::Idle), 1);
        assert_eq!(snap.count_status(AgentStatus::Stale), 0);
    }
}
