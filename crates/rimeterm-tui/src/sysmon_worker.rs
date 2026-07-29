//! Background system-monitor worker.
//!
//! Owns a single OS thread that receives [`SysmonRequest`]s over an
//! mpsc channel and returns [`SysmonResponse`]s. The pane checks
//! generations before applying snapshots. Sampling wraps
//! [`sysinfo::System`] with the same `refresh_processes_specifics(
//! ProcessesToUpdate::All, true, ProcessRefreshKind::nothing()
//!     .with_cpu().with_memory().without_tasks())` combo tokimono
//! validated — avoids the Linux per-thread `/proc/<pid>/task/<tid>/`
//! walk that dwarfs everything else on heavily-multithreaded systems.
//!
//! Optional collectors gated by cargo features:
//!
//! - `sysmon-nvidia`: NVIDIA GPU utilization / VRAM / temperature via
//!   `nvml-wrapper` (dynamic-loads `libnvidia-ml.so` / `nvml.dll` at
//!   worker start; missing driver = feature is silently no-op).
//! - `sysmon-docker`: Docker container counts (running / paused /
//!   stopped) via `bollard` on an embedded current-thread tokio runtime.
//!   Missing daemon = feature is silently no-op.
//! - `sysmon-procfs` (Linux only): cgroup context from
//!   `/proc/self/cgroup` via `procfs`.
//!
//! The pane drives the sampling cadence: it sends a `Snapshot` request
//! at its own 200 ms tick (see `SysmonPane::poll_background`). The
//! worker only samples when asked, so an idle rimeterm process still
//! sleeps in `Receiver::recv`.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

use sysinfo::{
    Components, Disks, Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System,
};

use crate::sysmon_model::{
    DiskStats, MemoryStats, NetworkStats, ProcessInfo, Snapshot, SysmonRequest, SysmonResponse,
};

/// Handle to the running worker thread.
pub struct SysmonWorker {
    request_tx: Sender<SysmonRequest>,
    response_rx: Receiver<SysmonResponse>,
}

impl SysmonWorker {
    /// Start the worker thread.
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<SysmonRequest>();
        let (resp_tx, resp_rx) = mpsc::channel::<SysmonResponse>();
        thread::Builder::new()
            .name("rimeterm-sysmon-worker".into())
            .spawn(move || run(req_rx, resp_tx))
            .expect("spawn sysmon worker");
        Self {
            request_tx: req_tx,
            response_rx: resp_rx,
        }
    }

    pub fn send(&self, request: SysmonRequest) {
        let _ = self.request_tx.send(request);
    }

    pub fn drain(&self) -> Vec<SysmonResponse> {
        let mut out = Vec::new();
        while let Ok(response) = self.response_rx.try_recv() {
            out.push(response);
        }
        out
    }
}

/// `System::load_average()` is a static function that returns
/// zero-triples on platforms without loadavg (Windows). Map those to
/// `None` so the UI can print `n/a` instead of "0.00 0.00 0.00".
fn read_load_average() -> Option<(f32, f32, f32)> {
    let la = System::load_average();
    if la.one == 0.0 && la.five == 0.0 && la.fifteen == 0.0 {
        None
    } else {
        Some((la.one as f32, la.five as f32, la.fifteen as f32))
    }
}

/// Ask sysinfo for exactly pid / name / cpu / memory per process. Any
/// heavier refresh kind pulls in per-thread task walks that are the
/// dominant cost on Linux systems with many processes.
fn process_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .without_tasks()
}

/// Long-running sampler. `sysinfo::System` is created once and
/// refreshed in place so counters keep valid across ticks; network,
/// disk, and components live in sibling handles for the same reason.
/// NVML + Docker collectors are always attempted; either failure to
/// initialise degrades to `None` so the pane just hides its section.
struct Collector {
    system: System,
    networks: Networks,
    disks: Disks,
    components: Components,
    last_refresh: Instant,
    nvml: Option<nvml_wrapper::Nvml>,
    docker: Option<DockerCollector>,
    /// Every graphics adapter reported by the OS at worker startup.
    /// GPUs don't hot-plug in normal use so we cache this once — each
    /// snapshot combines this list with fresh NVML telemetry via
    /// [`compose_gpu_list`]. Empty on any OS where enumeration failed
    /// or the process has no permission to query.
    os_gpu_names: Vec<String>,
}

impl Collector {
    fn new() -> Self {
        // `System::new_all()` would do a one-time full refresh
        // (including the per-thread task walk we're avoiding). We
        // drive the individual `refresh_*` calls on demand instead.
        let mut system = System::new();
        // sysinfo's CPU usage is a delta between two samples; the very
        // first `refresh_cpu_usage` returns 0.0 for every core because
        // no baseline exists yet. Prime the counter now so the first
        // Snapshot request (~10-200 ms later) already has a real
        // delta to report — otherwise the CPU chart shows a flat "0.0%"
        // for the first ~200 ms after launch and users think the
        // widget is broken.
        system.refresh_cpu_usage();
        let os_gpu_names = enumerate_all_gpu_names();
        tracing::info!(
            os_gpu_count = os_gpu_names.len(),
            gpus = ?os_gpu_names,
            "OS-level GPU enumeration"
        );
        Self {
            system,
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            last_refresh: Instant::now(),
            nvml: init_nvml(),
            docker: DockerCollector::try_init(),
            os_gpu_names,
        }
    }

    fn refresh(&mut self, generation: u64) -> Snapshot {
        // Bytes-since-last-refresh counters need REAL elapsed time to
        // convert into an accurate bytes/sec rate; the pane's 200 ms
        // tick is a target, not a guarantee (a busy main loop or a
        // suspend/resume gap could stretch it).
        let elapsed_secs = self.last_refresh.elapsed().as_secs_f64().max(0.001);
        self.last_refresh = Instant::now();

        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            process_refresh_kind(),
        );

        let mut top_processes: Vec<ProcessInfo> = self
            .system
            .processes()
            .iter()
            .map(|(pid, process)| ProcessInfo {
                pid: pid.as_u32(),
                name: process.name().to_string_lossy().into_owned(),
                cpu: process.cpu_usage(),
                memory: process.memory(),
            })
            .collect();
        // Newest-cpu-first default ordering; the pane may re-sort per
        // user preference. Stable-by-pid tiebreak so cursor tracking is
        // predictable across identical-cpu rows.
        top_processes.sort_by(|a, b| {
            b.cpu
                .partial_cmp(&a.cpu)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.pid.cmp(&b.pid))
        });

        self.networks.refresh(true);
        let mut networks: Vec<NetworkStats> = self
            .networks
            .iter()
            .map(|(name, data)| NetworkStats {
                name: name.clone(),
                rx_rate: data.received() as f64 / elapsed_secs,
                tx_rate: data.transmitted() as f64 / elapsed_secs,
            })
            .collect();
        networks.sort_by(|a, b| a.name.cmp(&b.name));

        self.disks.refresh(true);
        let mut disks: Vec<DiskStats> = self
            .disks
            .list()
            .iter()
            .map(|disk| {
                let usage = disk.usage();
                DiskStats {
                    mount: disk.mount_point().to_path_buf(),
                    total: disk.total_space(),
                    available: disk.available_space(),
                    read_rate: usage.read_bytes as f64 / elapsed_secs,
                    write_rate: usage.written_bytes as f64 / elapsed_secs,
                }
            })
            .collect();
        disks.sort_by(|a, b| a.mount.cmp(&b.mount));

        self.components.refresh(true);
        let cpu_temp = self
            .components
            .iter()
            .filter_map(|c| c.temperature())
            .fold(None, |hottest: Option<f32>, t| {
                Some(hottest.map_or(t, |h| h.max(t)))
            });

        let cpu_per_core: Vec<f32> = self.system.cpus().iter().map(|c| c.cpu_usage()).collect();
        let cpu_avg = Snapshot::cpu_avg_from_cores(&cpu_per_core);
        // Nominal frequency of the first core; sysinfo reports it in
        // MHz. Zero when the platform can't report it — treat as
        // "unknown" downstream.
        let cpu_frequency_mhz = self
            .system
            .cpus()
            .first()
            .map(|c| c.frequency())
            .unwrap_or(0);

        let gpus = compose_gpu_list(&self.os_gpu_names, collect_gpus_nvml(self.nvml.as_ref()));
        let docker = self.docker.as_mut().and_then(DockerCollector::poll);
        #[cfg(target_os = "linux")]
        let cgroup = read_cgroup();
        #[cfg(not(target_os = "linux"))]
        let cgroup = None;

        // Cross-platform host + OS + uptime — these all work on
        // Windows / macOS / Linux, so the System block always has real
        // data even when Temp / Load stay unavailable.
        let host_name = System::host_name().filter(|s| !s.is_empty());
        let os_display = System::long_os_version()
            .or_else(System::name)
            .filter(|s| !s.is_empty());
        let uptime_seconds = System::uptime();

        Snapshot {
            generation,
            cpu_per_core,
            cpu_avg,
            cpu_frequency_mhz,
            load_avg: read_load_average(),
            memory: MemoryStats {
                used: self.system.used_memory(),
                total: self.system.total_memory(),
            },
            swap: MemoryStats {
                used: self.system.used_swap(),
                total: self.system.total_swap(),
            },
            cpu_temp,
            top_processes,
            networks,
            disks,
            gpus,
            docker,
            cgroup,
            host_name,
            os_display,
            uptime_seconds,
            scanned_at: Instant::now(),
        }
    }

    /// Send `signal` to the given pid. Returns `false` if the process
    /// no longer exists, the platform doesn't support the signal (e.g.
    /// Windows), or the send failed (typically insufficient permissions).
    fn kill(&mut self, pid: u32) -> bool {
        // Refresh so the sysinfo cache knows about newly-spawned pids
        // that landed between the last snapshot and this kill request.
        // Missing this makes `kill_with` return `false` on legitimate
        // targets seconds after they appear.
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            process_refresh_kind(),
        );
        match self.system.process(Pid::from_u32(pid)) {
            Some(process) => process.kill_with(Signal::Term).unwrap_or(false),
            None => false,
        }
    }
}

// ── GPU enumeration (all vendors) + NVIDIA telemetry overlay ─────────

/// Enumerate every graphics adapter the OS reports, regardless of
/// vendor. Results are stable for the lifetime of the process — we
/// cache once in `Collector::new()` and reuse across snapshots.
///
/// Backend per OS:
/// - **Windows**: `wmic path win32_VideoController get name /format:list`
/// - **Linux**: parse `lspci -mm` (fall back to `/sys/bus/pci` scan)
/// - **macOS**: `system_profiler SPDisplaysDataType`
///
/// Any failure (missing binary, permission denied, unparseable output)
/// degrades to an empty vec; the caller then falls back to NVML-only
/// output — same behaviour as before this enumeration existed.
fn enumerate_all_gpu_names() -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        return windows_enumerate_gpus();
    }
    #[cfg(target_os = "linux")]
    {
        return linux_enumerate_gpus();
    }
    #[cfg(target_os = "macos")]
    {
        return macos_enumerate_gpus();
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "windows")]
fn windows_enumerate_gpus() -> Vec<String> {
    use std::process::Command;
    // `wmic path win32_VideoController get name /format:list` prints
    // one `Name=…` line per adapter plus blanks. Deprecated in Win11
    // but still present on every 24H2 install we care about; if it's
    // been fully removed we fall through to PowerShell.
    let mut names = Vec::new();
    if let Ok(out) = Command::new("wmic")
        .args([
            "path",
            "win32_VideoController",
            "get",
            "name",
            "/format:list",
        ])
        .output()
    {
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                let line = line.trim();
                if let Some(name) = line.strip_prefix("Name=") {
                    let name = name.trim();
                    if !name.is_empty() {
                        names.push(name.to_string());
                    }
                }
            }
        }
    }
    if !names.is_empty() {
        return names;
    }
    // PowerShell fallback (slower but ships with every Windows release
    // wmic is being removed from).
    if let Ok(out) = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name",
        ])
        .output()
    {
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                let name = line.trim();
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

#[cfg(target_os = "linux")]
fn linux_enumerate_gpus() -> Vec<String> {
    use std::process::Command;
    let mut names = Vec::new();
    if let Ok(out) = Command::new("lspci").arg("-mm").output() {
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                // Each line: `<slot> "<class>" "<vendor>" "<device>" …`
                // Display controllers (class 0300, 0301, 0302, 0380).
                if !(line.contains("\"VGA compatible controller\"")
                    || line.contains("\"3D controller\"")
                    || line.contains("\"Display controller\""))
                {
                    continue;
                }
                let mut fields = line.split('"');
                // Sequence: [slot ][class][ ][vendor][ ][device][ ][rest…]
                let vendor = fields.nth(3).map(str::trim).unwrap_or("");
                let device = fields.nth(1).map(str::trim).unwrap_or("");
                let name = match (vendor, device) {
                    ("", "") => continue,
                    ("", d) => d.to_string(),
                    (v, "") => v.to_string(),
                    (v, d) => format!("{v} {d}"),
                };
                names.push(name);
            }
        }
    }
    names
}

#[cfg(target_os = "macos")]
fn macos_enumerate_gpus() -> Vec<String> {
    use std::process::Command;
    let mut names = Vec::new();
    if let Ok(out) = Command::new("system_profiler")
        .args(["SPDisplaysDataType"])
        .output()
    {
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            // system_profiler prints `      Chipset Model: <name>` under
            // each adapter block. Parse those lines.
            for line in stdout.lines() {
                if let Some(rest) = line.trim().strip_prefix("Chipset Model:") {
                    let name = rest.trim();
                    if !name.is_empty() {
                        names.push(name.to_string());
                    }
                }
            }
        }
    }
    names
}

// ── NVIDIA GPU (always-compiled, runtime-guarded) ────────────────────

/// Try to bring up NVML. Any failure — driver missing, wrong version,
/// permission denied — degrades to `None` so the worker still boots.
fn init_nvml() -> Option<nvml_wrapper::Nvml> {
    match nvml_wrapper::Nvml::init() {
        Ok(nvml) => {
            let count = nvml.device_count().unwrap_or(0);
            tracing::info!(nvml_device_count = count, "NVML init ok");
            Some(nvml)
        }
        Err(err) => {
            tracing::debug!(error = %err, "NVML init failed — GPU telemetry disabled");
            None
        }
    }
}

fn collect_gpus_nvml(nvml: Option<&nvml_wrapper::Nvml>) -> Vec<crate::sysmon_model::GpuStats> {
    let Some(nvml) = nvml else { return Vec::new() };
    let Ok(count) = nvml.device_count() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(count as usize);
    for idx in 0..count {
        let Ok(device) = nvml.device_by_index(idx) else {
            tracing::debug!(idx, "nvml.device_by_index failed — skipping");
            continue;
        };
        let name = device.name().unwrap_or_else(|_| format!("GPU {idx}"));
        let utilization = device.utilization_rates().ok().map(|u| u.gpu as f32);
        let (memory_used, memory_total) = device
            .memory_info()
            .map(|m| (m.used, m.total))
            .unwrap_or((0, 0));
        let temperature = device
            .temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)
            .ok()
            .map(|t| t as f32);
        out.push(crate::sysmon_model::GpuStats {
            name,
            utilization,
            memory_used,
            memory_total,
            temperature,
        });
    }
    out
}

/// Merge OS-enumerated GPU names with NVML telemetry.
///
/// - When both sides have entries: use the OS list as authoritative
///   (covers iGPU + AMD dGPU that NVML can't see) and overlay NVML
///   metrics onto matching entries by name substring.
/// - When only NVML has entries (WSL2, containers where lspci/wmic
///   can't see the passthrough device): return NVML-only.
/// - When only OS enumeration has entries: return them with no
///   telemetry (UI shows the name, "no telemetry" for util/mem/temp).
/// - When both empty: `[]` — UI shows "no GPU detected".
fn compose_gpu_list(
    os_names: &[String],
    mut nvml_gpus: Vec<crate::sysmon_model::GpuStats>,
) -> Vec<crate::sysmon_model::GpuStats> {
    if os_names.is_empty() {
        return nvml_gpus;
    }
    let mut result: Vec<crate::sysmon_model::GpuStats> = Vec::with_capacity(os_names.len());
    for os_name in os_names {
        // Match by name substring — NVML reports "NVIDIA GeForce RTX
        // 3080", wmic reports "NVIDIA GeForce RTX 3080" (identical) or
        // sometimes with slight variations. Substring both ways is
        // conservative enough that near-matches still hit.
        let matched = nvml_gpus.iter().position(|g| {
            let a = g.name.to_lowercase();
            let b = os_name.to_lowercase();
            a == b || a.contains(&b) || b.contains(&a)
        });
        match matched {
            Some(idx) => {
                let mut g = nvml_gpus.remove(idx);
                // Prefer the OS-formatted name (wmic uses "NVIDIA GeForce
                // RTX 3080 Laptop GPU" while NVML abbreviates).
                g.name = os_name.clone();
                result.push(g);
            }
            None => result.push(crate::sysmon_model::GpuStats {
                name: os_name.clone(),
                utilization: None,
                memory_used: 0,
                memory_total: 0,
                temperature: None,
            }),
        }
    }
    // Any NVML entries that didn't match (e.g. WSL2 passthrough not
    // visible to wmic/lspci) still get appended so telemetry isn't lost.
    result.extend(nvml_gpus);
    result
}

// ── Docker daemon (always-compiled, runtime-guarded) ─────────────────

/// Owns the Docker client + a single-thread tokio runtime the client
/// needs for its async API. Both are dropped together with the worker.
struct DockerCollector {
    runtime: tokio::runtime::Runtime,
    docker: bollard::Docker,
}

impl DockerCollector {
    fn try_init() -> Option<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        let docker = bollard::Docker::connect_with_defaults()
            .map_err(|err| {
                tracing::debug!(error = %err, "docker connect failed — feature no-op");
                err
            })
            .ok()?;
        // Cheap round-trip; if the daemon is down `ping` errors and
        // we skip installing the collector so tick-time doesn't pay
        // for a doomed request.
        runtime.block_on(async {
            docker
                .ping()
                .await
                .map_err(|err| tracing::debug!(error = %err, "docker ping failed"))
                .ok()
        })?;
        Some(Self { runtime, docker })
    }

    fn poll(&mut self) -> Option<crate::sysmon_model::DockerStats> {
        let opts: bollard::query_parameters::ListContainersOptions =
            bollard::query_parameters::ListContainersOptionsBuilder::new()
                .all(true)
                .build();
        let listing = self
            .runtime
            .block_on(self.docker.list_containers(Some(opts)))
            .ok()?;
        let mut stats = crate::sysmon_model::DockerStats::default();
        use bollard::models::ContainerSummaryStateEnum::{PAUSED, RUNNING};
        for c in listing {
            match c.state {
                Some(RUNNING) => stats.running += 1,
                Some(PAUSED) => stats.paused += 1,
                _ => stats.stopped += 1,
            }
        }
        Some(stats)
    }
}

// ── Linux cgroup via procfs (Linux-only crate) ───────────────────────

/// Best-effort read of `/proc/self/cgroup`. The path helps identify
/// container context inside a shared host — a non-`/` path typically
/// means "inside a docker / systemd unit / k8s pod".
#[cfg(target_os = "linux")]
fn read_cgroup() -> Option<crate::sysmon_model::CgroupInfo> {
    let me = procfs::process::Process::myself().ok()?;
    let groups = me.cgroups().ok()?;
    // cgroup v2 is a single entry with an empty controllers list; v1
    // may have many. Prefer the first non-root path.
    let path = groups
        .0
        .iter()
        .map(|g| g.pathname.clone())
        .find(|p| p != "/")
        .unwrap_or_else(|| "/".to_string());
    let is_container = path != "/";
    Some(crate::sysmon_model::CgroupInfo { path, is_container })
}

fn run(rx: Receiver<SysmonRequest>, tx: Sender<SysmonResponse>) {
    let mut collector = Collector::new();
    while let Ok(request) = rx.recv() {
        match request {
            SysmonRequest::Snapshot { generation } => {
                let snapshot = collector.refresh(generation);
                if tx.send(SysmonResponse::Snapshot(snapshot)).is_err() {
                    break;
                }
            }
            SysmonRequest::Kill { pid } => {
                let success = collector.kill(pid);
                if tx
                    .send(SysmonResponse::KillResult { pid, success })
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Spawn a worker, request one snapshot, verify a reply lands with
    /// the expected generation and at least some process rows populated
    /// on the host (any modern OS has more than zero processes).
    #[test]
    fn worker_produces_snapshot_with_matching_generation() {
        let worker = SysmonWorker::spawn();
        worker.send(SysmonRequest::Snapshot { generation: 42 });

        // Sampling is fast (< 100 ms typical) but CI machines vary
        // widely — poll with a generous ceiling.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = None;
        while Instant::now() < deadline {
            for response in worker.drain() {
                if let SysmonResponse::Snapshot(snap) = response {
                    got = Some(snap);
                }
            }
            if got.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        let snap = got.expect("worker must return a snapshot within 5s");
        assert_eq!(snap.generation, 42);
        assert!(
            !snap.top_processes.is_empty(),
            "host must have at least one process"
        );
        assert!(
            snap.memory.total > 0,
            "sysinfo must report positive total memory"
        );
    }

    /// Killing pid 0 (or any impossible pid) must return `false` — no
    /// process by that id exists on any platform, so the send path
    /// isn't exercised and permissions don't matter.
    #[test]
    fn kill_nonexistent_pid_returns_false() {
        let worker = SysmonWorker::spawn();
        worker.send(SysmonRequest::Kill { pid: u32::MAX });

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = None;
        while Instant::now() < deadline {
            for response in worker.drain() {
                if let SysmonResponse::KillResult { pid, success } = response {
                    got = Some((pid, success));
                }
            }
            if got.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        let (pid, success) = got.expect("worker must reply to kill within 5s");
        assert_eq!(pid, u32::MAX);
        assert!(!success, "impossible pid must never report success");
    }

    // ── compose_gpu_list ─────────────────────────────────────────────

    fn mk_nvml(name: &str) -> crate::sysmon_model::GpuStats {
        crate::sysmon_model::GpuStats {
            name: name.to_string(),
            utilization: Some(42.0),
            memory_used: 4 * 1024 * 1024 * 1024,
            memory_total: 10 * 1024 * 1024 * 1024,
            temperature: Some(58.0),
        }
    }

    #[test]
    fn compose_empty_os_falls_back_to_nvml_only() {
        let nvml = vec![mk_nvml("NVIDIA GeForce RTX 3080")];
        let composed = compose_gpu_list(&[], nvml.clone());
        assert_eq!(composed.len(), 1);
        assert_eq!(composed[0].name, "NVIDIA GeForce RTX 3080");
        assert_eq!(composed[0].utilization, Some(42.0));
    }

    #[test]
    fn compose_os_only_gpus_get_no_telemetry() {
        let os = vec![
            "Intel(R) Iris(R) Xe Graphics".to_string(),
            "AMD Radeon RX 6600".to_string(),
        ];
        let composed = compose_gpu_list(&os, Vec::new());
        assert_eq!(composed.len(), 2);
        assert_eq!(composed[0].name, "Intel(R) Iris(R) Xe Graphics");
        assert_eq!(composed[0].utilization, None);
        assert_eq!(composed[0].memory_total, 0);
        assert_eq!(composed[1].name, "AMD Radeon RX 6600");
    }

    #[test]
    fn compose_overlays_nvml_onto_matching_os_entry() {
        // Typical laptop: iGPU + NVIDIA dGPU. NVML sees only the
        // NVIDIA. Result: 2 GPUs total, the NVIDIA one carries
        // telemetry, the iGPU has none.
        let os = vec![
            "Intel(R) Iris(R) Xe Graphics".to_string(),
            "NVIDIA GeForce RTX 3080 Laptop GPU".to_string(),
        ];
        let nvml = vec![mk_nvml("NVIDIA GeForce RTX 3080")];
        let composed = compose_gpu_list(&os, nvml);
        assert_eq!(composed.len(), 2);
        assert_eq!(composed[0].name, "Intel(R) Iris(R) Xe Graphics");
        assert_eq!(composed[0].utilization, None);
        // OS-formatted name wins on the matched entry so the user sees
        // the more descriptive "Laptop GPU" suffix.
        assert_eq!(composed[1].name, "NVIDIA GeForce RTX 3080 Laptop GPU");
        assert_eq!(composed[1].utilization, Some(42.0));
    }

    #[test]
    fn compose_unmatched_nvml_entry_appended() {
        // WSL2: lspci in the container sees Intel iGPU but the
        // passthrough NVIDIA appears only via NVML. Both must show.
        let os = vec!["Intel(R) UHD Graphics".to_string()];
        let nvml = vec![mk_nvml("NVIDIA A100")];
        let composed = compose_gpu_list(&os, nvml);
        assert_eq!(composed.len(), 2);
        assert_eq!(composed[0].name, "Intel(R) UHD Graphics");
        assert_eq!(composed[0].utilization, None);
        assert_eq!(composed[1].name, "NVIDIA A100");
        assert_eq!(composed[1].utilization, Some(42.0));
    }
}
