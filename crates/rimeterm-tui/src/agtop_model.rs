//! Owned data model for the Native [`AgtopPane`].
//!
//! Everything here is a plain `Send + Sync` DTO the worker fills in and
//! hands to the pane. No `sysinfo::System` handle crosses the worker
//! boundary — the pane only ever sees the snapshotted result.
//!
//! The shape mirrors `sysmon_model` deliberately so the pane can reuse
//! the same view / sort / filter mental model.
//!
//! [`AgtopPane`]: crate::agtop_pane::AgtopPane

use std::time::Instant;

/// Coarse activity classification derived from CPU and process state.
///
/// We deliberately keep this simple compared to upstream `agtop::Status`
/// (Busy / Spawning / Active / Idle / Waiting / Completed / Stale) —
/// without transcript parsing we can't reliably tell "waiting on user"
/// from "computing", so we bucket by CPU% only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentStatus {
    /// CPU ≥ 5% — model is actively processing / streaming tokens.
    Busy,
    /// 0.5% ≤ CPU < 5% — background work (waiting on API, syntax
    /// highlighting, etc.).
    Active,
    /// CPU < 0.5% — process is scheduled but not doing real work.
    Idle,
    /// OS process state is `Sleeping` / `Stopped` — parked awaiting
    /// input.  Only reliably distinct from Idle on Linux/macOS.
    Sleeping,
}

impl AgentStatus {
    /// One-char glyph for the status column — same visual language as
    /// upstream agtop.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Busy => "●",
            Self::Active => "●",
            Self::Idle => "○",
            Self::Sleeping => "◌",
        }
    }

    /// Four-char label so status column can be a fixed width regardless
    /// of variant.
    pub fn label(self) -> &'static str {
        match self {
            Self::Busy => "BUSY",
            Self::Active => "ACTV",
            Self::Idle => "IDLE",
            Self::Sleeping => "WAIT",
        }
    }

    /// Sort rank — busy first, sleeping last.
    pub fn rank(self) -> u8 {
        match self {
            Self::Busy => 0,
            Self::Active => 1,
            Self::Idle => 2,
            Self::Sleeping => 3,
        }
    }
}

/// Sort key for the agent table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortKey {
    Cpu,
    Memory,
    Pid,
    Label,
    Uptime,
    Status,
}

/// Ascending / descending toggle for [`SortKey`]. Same-key second press
/// flips the order.
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
}

/// One detected AI-agent process.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentInfo {
    /// Canonical name from the matcher table (e.g. `"claude"`,
    /// `"codex"`, `"cursor-agent"`).
    pub label: String,
    pub pid: u32,
    /// CPU% averaged over the last worker sample — same normalisation
    /// [`sysinfo`] uses (0..N*100 where N = core count).
    pub cpu: f32,
    /// Resident set size in bytes.
    pub rss: u64,
    /// Seconds since the process started.
    pub uptime_sec: u64,
    /// Absolute cwd path (basename shown in the table, full path in
    /// the tooltip / footer). Empty when unavailable.
    pub cwd: String,
    /// Short project label — cwd basename or, when cwd is empty, the
    /// executable name. Pre-computed so the pane doesn't `basename`
    /// per frame.
    pub project: String,
    /// Full cmdline joined by single spaces — for filter matching and
    /// the info-footer preview. Truncated at [`CMDLINE_TRUNCATE`].
    pub cmdline: String,
    /// Classified activity level; see [`AgentStatus`].
    pub status: AgentStatus,
}

/// Cap the stored cmdline so a hostile process with megabyte-scale
/// argv can't inflate every snapshot's memory footprint. 512 chars
/// is comfortably more than any real agent invocation.
pub const CMDLINE_TRUNCATE: usize = 512;

/// Complete snapshot handed from worker → pane. Kept flat / cloneable
/// so the pane can hold onto the "latest" and re-render without
/// touching the worker again.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub agents: Vec<AgentInfo>,
    /// Aggregate CPU% across every detected agent — cached because the
    /// footer re-uses it per frame.
    pub total_cpu: f32,
    /// Aggregate RSS bytes across every detected agent.
    pub total_rss: u64,
    pub sampled_at: Instant,
}

impl Snapshot {
    pub fn empty() -> Self {
        Self {
            agents: Vec::new(),
            total_cpu: 0.0,
            total_rss: 0,
            sampled_at: Instant::now(),
        }
    }
}

/// Filtered + sorted derivation of [`Snapshot::agents`]. Recomputed on
/// each render so cursor movement / sort flips are cheap; a full re-scan
/// still runs in microseconds for a realistic agent count (<20).
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
            }
        }
    }

    fn compare(a: &AgentInfo, b: &AgentInfo, key: SortKey) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match key {
            SortKey::Cpu => a
                .cpu
                .partial_cmp(&b.cpu)
                .unwrap_or(Ordering::Equal)
                .then(a.pid.cmp(&b.pid)),
            SortKey::Memory => a.rss.cmp(&b.rss).then(a.pid.cmp(&b.pid)),
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

/// Worker request. Snapshot requests carry a generation so late replies
/// (worker delayed by a slow scan) land harmlessly when a fresh request
/// has already superseded them.
#[derive(Clone, Debug)]
pub enum AgtopRequest {
    Snapshot { generation: u64 },
}

/// Worker response, one-to-one with a request. `Snapshot` carries the
/// completing generation.
#[derive(Clone, Debug)]
pub enum AgtopResponse {
    Snapshot { generation: u64, snapshot: Snapshot },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(label: &str, pid: u32, cpu: f32, rss: u64, uptime: u64) -> AgentInfo {
        AgentInfo {
            label: label.into(),
            pid,
            cpu,
            rss,
            uptime_sec: uptime,
            cwd: String::new(),
            project: label.into(),
            cmdline: String::new(),
            status: if cpu >= 5.0 {
                AgentStatus::Busy
            } else if cpu >= 0.5 {
                AgentStatus::Active
            } else {
                AgentStatus::Idle
            },
        }
    }

    #[test]
    fn empty_filter_matches_every_row() {
        let snap = Snapshot {
            agents: vec![row("claude", 1, 3.0, 100, 60)],
            ..Snapshot::empty()
        };
        let v = AgentView::from_snapshot(&snap, SortKey::Cpu, SortOrder::Descending, Some(""));
        assert_eq!(v.rows.len(), 1);
    }

    #[test]
    fn filter_by_pid_is_exact_match() {
        let snap = Snapshot {
            agents: vec![row("claude", 1, 0.0, 0, 0), row("codex", 42, 0.0, 0, 0)],
            ..Snapshot::empty()
        };
        let v = AgentView::from_snapshot(&snap, SortKey::Pid, SortOrder::Ascending, Some("42"));
        assert_eq!(v.rows.len(), 1);
        assert_eq!(v.rows[0].pid, 42);
    }

    #[test]
    fn filter_matches_label_case_insensitive() {
        let snap = Snapshot {
            agents: vec![row("Claude", 1, 0.0, 0, 0), row("codex", 2, 0.0, 0, 0)],
            ..Snapshot::empty()
        };
        let v = AgentView::from_snapshot(&snap, SortKey::Pid, SortOrder::Ascending, Some("claude"));
        assert_eq!(v.rows.len(), 1);
        assert_eq!(v.rows[0].pid, 1);
    }

    #[test]
    fn filter_matches_project_or_cmdline() {
        let mut a = row("codex", 7, 0.0, 0, 0);
        a.project = "my-repo".into();
        let mut b = row("claude", 8, 0.0, 0, 0);
        b.cmdline = "/usr/bin/claude --resume".into();
        let snap = Snapshot {
            agents: vec![a, b],
            ..Snapshot::empty()
        };
        // Project hit
        let v =
            AgentView::from_snapshot(&snap, SortKey::Pid, SortOrder::Ascending, Some("my-repo"));
        assert_eq!(v.rows.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![7]);
        // Cmdline hit
        let v =
            AgentView::from_snapshot(&snap, SortKey::Pid, SortOrder::Ascending, Some("--resume"));
        assert_eq!(v.rows.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![8]);
    }

    #[test]
    fn sort_cpu_descending_puts_hottest_first() {
        let snap = Snapshot {
            agents: vec![
                row("aider", 1, 1.0, 0, 0),
                row("claude", 2, 5.0, 0, 0),
                row("codex", 3, 3.0, 0, 0),
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
    fn sort_status_prefers_busy_over_idle() {
        let snap = Snapshot {
            agents: vec![
                row("a", 1, 0.0, 0, 0),  // Idle
                row("b", 2, 10.0, 0, 0), // Busy
                row("c", 3, 1.0, 0, 0),  // Active
            ],
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
    }

    #[test]
    fn status_rank_orders_busy_first() {
        assert!(AgentStatus::Busy.rank() < AgentStatus::Idle.rank());
        assert!(AgentStatus::Active.rank() < AgentStatus::Idle.rank());
        assert!(AgentStatus::Idle.rank() < AgentStatus::Sleeping.rank());
    }
}
