//! Owned data model for the Native SysmonPane.
//!
//! Everything in this module is a plain, `Send + Sync` DTO that the
//! background worker fills in and hands to the pane on the main thread.
//! `sysinfo::System` handles never cross the worker boundary.
//!
//! Optional metrics (GPU, Docker containers, Linux cgroup id) live as
//! `Option` / `Vec` fields that stay empty when the corresponding
//! `sysmon-*` feature is off — so the model shape is identical across
//! feature combinations and the UI can render "n/a" uniformly.

use std::path::PathBuf;
use std::time::Instant;

/// Which sub-view the SysmonPane currently renders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SysmonView {
    /// CPU waveform + memory / swap gauges + GPU / docker summary.
    Overview,
    /// Sortable / filterable process table with kill affordance.
    Processes,
}

impl SysmonView {
    pub(crate) fn cycle_next(self) -> Self {
        match self {
            Self::Overview => Self::Processes,
            Self::Processes => Self::Overview,
        }
    }
}

/// How the process table is currently sorted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortKey {
    Cpu,
    Memory,
    Pid,
    Name,
}

/// Ascending vs descending toggle for [`SortKey`]. Same-key second press flips.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

impl SortOrder {
    pub(crate) fn flip(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }
}

/// Memory or swap usage pair (bytes).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MemoryStats {
    pub used: u64,
    pub total: u64,
}

impl MemoryStats {
    pub fn ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.used as f64 / self.total as f64
        }
    }
}

/// Per-process snapshot row.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub memory: u64,
}

/// Bytes-per-second throughput on one network interface.
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkStats {
    pub name: String,
    pub rx_rate: f64,
    pub tx_rate: f64,
}

/// Capacity + I/O throughput on one mounted disk.
#[derive(Clone, Debug, PartialEq)]
pub struct DiskStats {
    pub mount: PathBuf,
    pub total: u64,
    pub available: u64,
    pub read_rate: f64,
    pub write_rate: f64,
}

/// Per-GPU snapshot row populated by the `sysmon-nvidia` feature via
/// `nvml-wrapper`. When the feature is off or NVML init fails, the
/// enclosing `Snapshot.gpus` vec stays empty.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuStats {
    pub name: String,
    /// GPU-core utilization percent (0..=100). `None` when the driver
    /// declines to report it (rare, older Tesla parts).
    pub utilization: Option<f32>,
    pub memory_used: u64,
    pub memory_total: u64,
    /// GPU die temperature in °C. `None` when the driver doesn't
    /// expose a sensor (integrated GPUs in some configs).
    pub temperature: Option<f32>,
}

/// Docker daemon summary populated by the `sysmon-docker` feature via
/// `bollard`. `None` = feature disabled OR daemon unreachable OR API
/// error (the worker downgrades any of the three to "not shown").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DockerStats {
    pub running: u32,
    pub paused: u32,
    pub stopped: u32,
}

impl DockerStats {
    pub fn total(&self) -> u32 {
        self.running + self.paused + self.stopped
    }
}

/// Linux cgroup context sourced from `/proc/self/cgroup` via `procfs`
/// when the `sysmon-procfs` feature is on. Blank string on cgroup-less
/// systems (BSD, macOS, older Linux); `None` on any other OS or with
/// the feature disabled.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CgroupInfo {
    pub path: String,
    /// True when the path is more specific than `/` — a real cgroup
    /// (systemd unit, container, k8s pod), not the root.
    pub is_container: bool,
}

/// One complete sample produced by [`crate::sysmon_worker::SysmonWorker`],
/// generation-tagged so stale replies can be dropped by the pane.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub cpu_per_core: Vec<f32>,
    pub cpu_avg: f32,
    /// Reported CPU nominal frequency in MHz (first core; assumes
    /// homogeneous topology). Zero when sysinfo doesn't expose it on
    /// the current platform.
    pub cpu_frequency_mhz: u64,
    /// Unix-only. `None` on Windows, where sysinfo returns zeros.
    pub load_avg: Option<(f32, f32, f32)>,
    pub memory: MemoryStats,
    pub swap: MemoryStats,
    /// Hottest reading across all temperature sensors, in °C. `None` when
    /// the platform exposes no sensors (VMs / containers / Windows).
    pub cpu_temp: Option<f32>,
    pub top_processes: Vec<ProcessInfo>,
    pub networks: Vec<NetworkStats>,
    pub disks: Vec<DiskStats>,
    /// Populated when `sysmon-nvidia` is enabled and NVML init succeeded.
    /// Empty otherwise — the UI renders a "GPU: n/a" placeholder row.
    pub gpus: Vec<GpuStats>,
    /// Populated when `sysmon-docker` is enabled and the Docker daemon
    /// answered. `None` otherwise — the UI hides the docker row.
    pub docker: Option<DockerStats>,
    /// Populated when `sysmon-procfs` is enabled AND running on Linux.
    /// The path helps identify container context inside a shared host.
    pub cgroup: Option<CgroupInfo>,
    /// Host machine name. `None` when sysinfo can't resolve it.
    pub host_name: Option<String>,
    /// Human-readable OS / distro string ("Windows 11 Pro" / "Ubuntu
    /// 22.04" / "macOS 14.5"). `None` when sysinfo can't format it.
    pub os_display: Option<String>,
    /// Seconds since the host booted.
    pub uptime_seconds: u64,
    pub scanned_at: Instant,
}

impl Snapshot {
    /// Empty snapshot bootstrap value used before the first worker reply
    /// lands. `applied_generation` starts at 0 so any real snapshot
    /// (generation >= 1) supersedes it.
    pub fn empty() -> Self {
        Self {
            generation: 0,
            cpu_per_core: Vec::new(),
            cpu_avg: 0.0,
            cpu_frequency_mhz: 0,
            load_avg: None,
            memory: MemoryStats::default(),
            swap: MemoryStats::default(),
            cpu_temp: None,
            top_processes: Vec::new(),
            networks: Vec::new(),
            disks: Vec::new(),
            gpus: Vec::new(),
            docker: None,
            cgroup: None,
            host_name: None,
            os_display: None,
            uptime_seconds: 0,
            scanned_at: Instant::now(),
        }
    }

    /// Compute `used / total` across all cores; treats zero-core boxes as 0.
    pub fn cpu_avg_from_cores(cores: &[f32]) -> f32 {
        if cores.is_empty() {
            0.0
        } else {
            cores.iter().sum::<f32>() / cores.len() as f32
        }
    }
}

/// Sortable, filterable process view derived from a [`Snapshot`].
#[derive(Clone, Debug)]
pub struct ProcessView {
    pub sort_key: SortKey,
    pub order: SortOrder,
    pub filter: Option<String>,
    /// Copy of the source snapshot's process rows AFTER filter + sort.
    pub rows: Vec<ProcessInfo>,
}

impl ProcessView {
    /// Derive a view from a fresh snapshot. `filter` matches `pid` (exact
    /// digits) or `name` substring (case-insensitive); Regex is deferred.
    pub fn from_snapshot(
        snapshot: &Snapshot,
        sort_key: SortKey,
        order: SortOrder,
        filter: Option<&str>,
    ) -> Self {
        let filter_owned = filter.map(str::to_owned);
        let mut rows: Vec<ProcessInfo> = snapshot
            .top_processes
            .iter()
            .filter(|p| Self::row_matches(p, filter))
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

    fn row_matches(row: &ProcessInfo, filter: Option<&str>) -> bool {
        match filter {
            None => true,
            Some(f) if f.is_empty() => true,
            Some(f) => {
                // Numeric filter → exact pid match.
                if let Ok(pid) = f.parse::<u32>() {
                    return row.pid == pid;
                }
                row.name.to_lowercase().contains(&f.to_lowercase())
            }
        }
    }

    fn compare(a: &ProcessInfo, b: &ProcessInfo, key: SortKey) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match key {
            SortKey::Cpu => a
                .cpu
                .partial_cmp(&b.cpu)
                .unwrap_or(Ordering::Equal)
                .then(a.pid.cmp(&b.pid)),
            SortKey::Memory => a.memory.cmp(&b.memory).then(a.pid.cmp(&b.pid)),
            SortKey::Pid => a.pid.cmp(&b.pid),
            SortKey::Name => a
                .name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.pid.cmp(&b.pid)),
        }
    }
}

/// A worker request; snapshots are generation-tagged so stale replies land
/// harmlessly. Kills carry no generation because they don't produce a
/// snapshot, just a per-pid outcome.
#[derive(Clone, Debug)]
pub enum SysmonRequest {
    Snapshot { generation: u64 },
    Kill { pid: u32 },
}

/// Worker response, always mapped 1:1 with a request. `Snapshot` carries
/// the completing generation.
#[derive(Clone, Debug)]
pub enum SysmonResponse {
    Snapshot(Snapshot),
    KillResult { pid: u32, success: bool },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(pid: u32, name: &str, cpu: f32, mem: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: name.into(),
            cpu,
            memory: mem,
        }
    }

    #[test]
    fn cpu_avg_from_cores_handles_empty() {
        assert_eq!(Snapshot::cpu_avg_from_cores(&[]), 0.0);
        assert_eq!(Snapshot::cpu_avg_from_cores(&[10.0, 20.0, 30.0]), 20.0);
    }

    #[test]
    fn memory_ratio_zero_total_no_div_by_zero() {
        let m = MemoryStats { used: 5, total: 0 };
        assert_eq!(m.ratio(), 0.0);
        let m = MemoryStats {
            used: 512,
            total: 1024,
        };
        assert_eq!(m.ratio(), 0.5);
    }

    #[test]
    fn docker_stats_total_sums_lifecycle_states() {
        let d = DockerStats {
            running: 3,
            paused: 1,
            stopped: 5,
        };
        assert_eq!(d.total(), 9);
    }

    #[test]
    fn process_view_filter_by_pid_exact() {
        let snap = Snapshot {
            top_processes: vec![
                make_row(1, "init", 0.1, 100),
                make_row(42, "code", 5.0, 200),
            ],
            ..Snapshot::empty()
        };
        let view =
            ProcessView::from_snapshot(&snap, SortKey::Pid, SortOrder::Ascending, Some("42"));
        assert_eq!(view.rows.len(), 1);
        assert_eq!(view.rows[0].pid, 42);
    }

    #[test]
    fn process_view_filter_by_name_case_insensitive() {
        let snap = Snapshot {
            top_processes: vec![
                make_row(1, "Init", 0.1, 100),
                make_row(42, "Code", 5.0, 200),
                make_row(7, "background", 1.0, 50),
            ],
            ..Snapshot::empty()
        };
        let view =
            ProcessView::from_snapshot(&snap, SortKey::Pid, SortOrder::Ascending, Some("code"));
        assert_eq!(view.rows.len(), 1);
        assert_eq!(view.rows[0].pid, 42);
    }

    #[test]
    fn process_view_sort_cpu_desc_puts_hottest_first() {
        let snap = Snapshot {
            top_processes: vec![
                make_row(1, "a", 1.0, 100),
                make_row(2, "b", 5.0, 100),
                make_row(3, "c", 3.0, 100),
            ],
            ..Snapshot::empty()
        };
        let view = ProcessView::from_snapshot(&snap, SortKey::Cpu, SortOrder::Descending, None);
        assert_eq!(
            view.rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![2, 3, 1,]
        );
    }

    #[test]
    fn sysmon_view_cycle_next_wraps() {
        assert_eq!(SysmonView::Overview.cycle_next(), SysmonView::Processes);
        assert_eq!(SysmonView::Processes.cycle_next(), SysmonView::Overview);
    }

    #[test]
    fn sort_order_flip_toggles() {
        assert_eq!(SortOrder::Ascending.flip(), SortOrder::Descending);
        assert_eq!(SortOrder::Descending.flip(), SortOrder::Ascending);
    }
}
