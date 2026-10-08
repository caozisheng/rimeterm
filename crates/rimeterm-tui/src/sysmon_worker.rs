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
    /// Windows WDDM GPU telemetry (DXGI identity + PDH counters).
    /// `None` when init failed — GPU rows fall back to name-only.
    #[cfg(target_os = "windows")]
    wddm: Option<WddmGpuCollector>,
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
            #[cfg(target_os = "windows")]
            wddm: WddmGpuCollector::try_init(),
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

        let other_gpus = self.collect_gpus_os_side();
        let gpus = compose_gpu_list(
            &self.os_gpu_names,
            collect_gpus_nvml(self.nvml.as_ref()),
            other_gpus,
        );
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

    /// OS-side GPU telemetry for non-NVIDIA cards: WDDM counters on
    /// Windows, amdgpu sysfs on Linux, nothing elsewhere.
    fn collect_gpus_os_side(&mut self) -> Vec<crate::sysmon_model::GpuStats> {
        #[cfg(target_os = "windows")]
        {
            self.wddm.as_mut().map(|w| w.sample()).unwrap_or_default()
        }
        #[cfg(target_os = "linux")]
        {
            linux_collect_amdgpu(&self.os_gpu_names)
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        {
            Vec::new()
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
        windows_enumerate_gpus()
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
        && out.status.success()
    {
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
        && out.status.success()
    {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let name = line.trim();
            if !name.is_empty() {
                names.push(name.to_string());
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

/// Extract the LUID key (`0x…_0x…`, lowercase) from a PDH GPU counter
/// instance name. GPU Engine instances look like
/// `pid_1234_luid_0x00000000_0x0000c117_phys_0_eng_0_engtype_3D`;
/// GPU Adapter Memory instances are the bare `luid_0x…_0x…` pair.
/// Returns `None` when no well-formed `luid_0x…_0x…` segment exists.
#[cfg(any(target_os = "windows", test))]
fn luid_from_pdh_instance(instance: &str) -> Option<String> {
    let lower = instance.to_lowercase();
    let start = lower.find("luid_")?;
    let rest = lower[start + "luid_".len()..].strip_prefix("0x")?;
    // Two `0x<hex>` halves joined by `_`. `x` is not a hex digit, so
    // each half is scanned as pure hex with the literal `0x` prefix
    // required explicitly — `luid_not_hex` and truncated LUIDs fail.
    let low_len = rest.find(|c: char| !c.is_ascii_hexdigit())?;
    let (low, rest) = rest.split_at(low_len);
    let rest = rest.strip_prefix("_0x")?;
    let high_len = rest
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    let high = &rest[..high_len];
    if low.is_empty() || high.is_empty() {
        return None;
    }
    Some(format!("0x{low}_0x{high}"))
}

// ── Windows WDDM GPU telemetry (DXGI identity + PDH counters) ────

/// In-process GPU telemetry from Windows performance counters — the
/// same data source Task Manager uses. DXGI supplies the stable
/// adapter identity (LUID + name + dedicated VRAM, enumerated once);
/// PDH supplies per-engine utilization and adapter memory usage keyed
/// by LUID (sampled each tick).
///
/// Init failure (no counters, old OS, access denied) degrades to
/// `None` and the pane falls back to name-only GPU rows — identical
/// behaviour to NVML init failure.
#[cfg(target_os = "windows")]
struct WddmGpuCollector {
    /// LUID key (as produced by `luid_from_pdh_instance`) →
    /// (adapter name, dedicated VRAM bytes).
    adapters: Vec<(String, String, u64)>,
    query: windows::Win32::System::Performance::PDH_HQUERY,
    util_counter: windows::Win32::System::Performance::PDH_HCOUNTER,
    mem_counter: windows::Win32::System::Performance::PDH_HCOUNTER,
}

#[cfg(target_os = "windows")]
impl WddmGpuCollector {
    fn try_init() -> Option<Self> {
        use windows::Win32::Graphics::Dxgi::{
            CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIAdapter1, IDXGIFactory1,
        };
        use windows::Win32::System::Performance::{
            PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW, PdhCollectQueryData, PdhOpenQueryW,
        };
        use windows::core::PCWSTR;

        // ── DXGI: LUID + name + dedicated VRAM per hardware adapter ──
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }
            .map_err(|err| tracing::debug!(error = %err, "CreateDXGIFactory1 failed"))
            .ok()?;
        let mut adapters: Vec<(String, String, u64)> = Vec::new();
        for idx in 0u32.. {
            let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(idx) } {
                Ok(adapter) => adapter,
                Err(_) => break, // DXGI_ERROR_NOT_FOUND: end of list
            };
            let desc = match unsafe { adapter.GetDesc1() } {
                Ok(desc) => desc,
                Err(_) => continue,
            };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue; // Microsoft Basic Render Driver etc.
            }
            if desc.DedicatedVideoMemory == 0 {
                continue; // no dedicated VRAM ⇒ not a real render target
            }
            let name = String::from_utf16_lossy(&desc.Description)
                .trim_end_matches('\0')
                .to_string();
            // LUID formatted to match `luid_from_pdh_instance` output
            // (lowercase hex halves).
            let luid = format!(
                "0x{:08x}_0x{:08x}",
                desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart as u32
            );
            adapters.push((luid, name, desc.DedicatedVideoMemory as u64));
        }
        if adapters.is_empty() {
            return None;
        }

        // ── PDH: GPU Engine util + GPU Adapter Memory dedicated ──
        let mut query = PDH_HQUERY::default();
        let mut util_counter = PDH_HCOUNTER::default();
        let mut mem_counter = PDH_HCOUNTER::default();
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != 0 {
            tracing::debug!("PdhOpenQuery failed");
            return None;
        }
        let util_path: windows::core::HSTRING = r"\GPU Engine(*)\Utilization Percentage".into();
        let mem_path: windows::core::HSTRING = r"\GPU Adapter Memory(*)\Dedicated Usage".into();
        if unsafe { PdhAddEnglishCounterW(query, &util_path, 0, &mut util_counter) } != 0
            || unsafe { PdhAddEnglishCounterW(query, &mem_path, 0, &mut mem_counter) } != 0
            || unsafe { PdhCollectQueryData(query) } != 0
        {
            tracing::debug!("PDH GPU counters unavailable");
            return None;
        }
        tracing::info!(adapters = adapters.len(), "WDDM GPU telemetry init ok");
        Some(Self {
            adapters,
            query,
            util_counter,
            mem_counter,
        })
    }

    /// One `GpuStats` per DXGI adapter with current counter data.
    /// LUIDs the counters know but DXGI didn't list are dropped.
    fn sample(&mut self) -> Vec<crate::sysmon_model::GpuStats> {
        use windows::Win32::System::Performance::PdhCollectQueryData;

        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return Vec::new();
        }
        let util_map = read_counter_array(self.util_counter);
        let mem_map = read_counter_array(self.mem_counter);
        self.adapters
            .iter()
            .map(|(luid, name, vram_total)| {
                let utilization = util_map.get(luid).map(|v| v.clamp(0.0, 100.0) as f32);
                let memory_used = mem_map.get(luid).map(|v| *v as u64).unwrap_or(0);
                crate::sysmon_model::GpuStats {
                    name: name.clone(),
                    utilization,
                    memory_used,
                    memory_total: *vram_total,
                    temperature: None, // no OS-level GPU temp counter
                }
            })
            .collect()
    }
}

/// Read all instances of a wildcard counter, summing per LUID into a
/// map. Instance names without a parsable LUID are skipped.
#[cfg(target_os = "windows")]
fn read_counter_array(
    counter: windows::Win32::System::Performance::PDH_HCOUNTER,
) -> std::collections::HashMap<String, f64> {
    use windows::Win32::System::Performance::{
        PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PdhGetFormattedCounterArrayW,
    };

    // First call sizes the buffer (returns PDH_MORE_DATA).
    let mut buf_size = 0u32;
    let mut item_count = 0u32;
    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut buf_size,
            &mut item_count,
            None,
        )
    };
    // 0 = ERROR_SUCCESS (single-instance case), 0x800007EA =
    // PDH_MORE_DATA (normal wildcard case).
    if status != 0 && status != 0x8000_07EA {
        return std::collections::HashMap::new();
    }
    let mut items = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); item_count as usize];
    let mut read_back = 0u32;
    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut buf_size,
            &mut read_back,
            Some(items.as_mut_ptr()),
        )
    };
    if status != 0 {
        return std::collections::HashMap::new();
    }
    let mut map = std::collections::HashMap::with_capacity(items.len());
    for item in &items[..read_back as usize] {
        // szName is a NUL-terminated UTF-16 string; the PDH array
        // allocation owns the backing memory.
        let name = unsafe {
            let mut len = 0usize;
            while *item.szName.0.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(item.szName.0, len))
        };
        if let Some(luid) = luid_from_pdh_instance(&name) {
            let value = unsafe { item.FmtValue.Anonymous.doubleValue };
            *map.entry(luid).or_insert(0.0) += value;
        }
    }
    map
}

// ── Linux amdgpu sysfs telemetry ──────────────────────────────────

/// Parse a decimal u64 from a sysfs file body (trailing newline
/// included). `None` on empty/garbage — callers treat that as
/// "sensor not exposed".
#[cfg(any(target_os = "linux", test))]
fn parse_sysfs_u64(s: &str) -> Option<u64> {
    s.trim().parse().ok()
}

/// Parse an hwmon `tempN_input` value (milli-°C integer) into °C.
#[cfg(any(target_os = "linux", test))]
fn parse_temp_milli_c(s: &str) -> Option<f32> {
    s.trim().parse::<i64>().ok().map(|m| m as f32 / 1000.0)
}

/// Per-card telemetry from the amdgpu driver sysfs interface
/// (kernel-documented). One `GpuStats` per AMD card found; empty vec
/// when the driver exposes nothing (no AMD GPU, non-Linux, permission
/// errors). File reads only — cheap enough for every 200 ms tick.
///
/// Paths:
/// - `/sys/class/drm/card*/device/gpu_busy_percent` → utilization %
/// - `/sys/class/drm/card*/device/mem_info_vram_used` / `_total`
/// - `/sys/class/drm/card*/device/hwmon/hwmon*/temp1_input` (milli-°C)
///
/// Name join: `device` is a symlink to
/// `/sys/bus/pci/devices/0000:XX:YY.Z`; its basename is the slot
/// `lspci` prints, which the OS enumeration list already carries.
#[cfg(target_os = "linux")]
fn linux_collect_amdgpu(os_gpu_names: &[String]) -> Vec<crate::sysmon_model::GpuStats> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return out;
    };
    for entry in entries.flatten() {
        let card = entry.file_name();
        let card = card.to_string_lossy();
        if !(card.starts_with("card") && card[4..].chars().all(|c| c.is_ascii_digit())) {
            continue; // cardN only — skip cardN-K partitions and render nodes
        }
        let base = entry.path().join("device");
        // AMD only: PCI vendor id 0x1002. Intel (0x8086) exposes no
        // gpu_busy_percent and would read as garbage zeros.
        let vendor = std::fs::read_to_string(base.join("vendor")).unwrap_or_default();
        if vendor.trim() != "0x1002" {
            continue;
        }
        let util = std::fs::read_to_string(base.join("gpu_busy_percent"))
            .ok()
            .and_then(|s| parse_sysfs_u64(&s))
            .map(|v| v as f32);
        let used = std::fs::read_to_string(base.join("mem_info_vram_used"))
            .ok()
            .and_then(|s| parse_sysfs_u64(&s))
            .unwrap_or(0);
        let total = std::fs::read_to_string(base.join("mem_info_vram_total"))
            .ok()
            .and_then(|s| parse_sysfs_u64(&s))
            .unwrap_or(0);
        // hwmon index varies per boot (hwmon2, hwmon5, …) — scan all.
        let temp = std::fs::read_dir(base.join("hwmon")).ok().and_then(|rd| {
            rd.flatten().find_map(|h| {
                std::fs::read_to_string(h.path().join("temp1_input"))
                    .ok()
                    .and_then(|s| parse_temp_milli_c(&s))
            })
        });
        if util.is_none() && used == 0 && total == 0 {
            continue; // card present but driver exposes nothing useful
        }
        // Resolve the PCI slot from the `device` symlink and match it
        // against the OS enumeration names (which came from lspci).
        let gpu_name = std::fs::read_link(&base)
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
            .and_then(|slot| {
                os_gpu_names
                    .iter()
                    .find(|n| n.to_lowercase().contains(&slot.to_lowercase()))
            })
            .cloned()
            .unwrap_or_else(|| format!("AMD GPU ({card})"));
        out.push(crate::sysmon_model::GpuStats {
            name: gpu_name,
            utilization: util,
            memory_used: used,
            memory_total: total,
            temperature: temp,
        });
    }
    out
}

/// Non-Linux stub: no sysfs to read. (Windows uses the WDDM
/// collector; macOS stays name-only.)
#[cfg(not(target_os = "linux"))]
fn linux_collect_amdgpu(_os_gpu_names: &[String]) -> Vec<crate::sysmon_model::GpuStats> {
    Vec::new()
}

/// Merge OS-enumerated GPU names with telemetry from NVML (NVIDIA)
/// and the OS-side collector (WDDM on Windows / amdgpu sysfs on
/// Linux — both vendor-agnostic for their platform).
///
/// - OS list is authoritative when non-empty: NVML telemetry overlays
///   matching entries by name substring first (NVML wins over the
///   OS-side source when both cover a card — it's richer and
///   battle-tested); OS-side telemetry then fills remaining entries
///   the same way.
/// - When only NVML has entries (WSL2, containers where lspci/wmic
///   can't see the passthrough device): return NVML-only.
/// - When only OS enumeration has entries: name-only rows ("no
///   telemetry" in the UI).
/// - When both empty: `[]` — UI shows "no GPU detected".
/// - Unmatched NVML entries are appended (WSL2 passthrough scenario);
///   unmatched OS-side entries are dropped — unlike NVML, an
///   OS-resident collector has no scenario where the OS enumeration
///   missed its device.
fn compose_gpu_list(
    os_names: &[String],
    mut nvml_gpus: Vec<crate::sysmon_model::GpuStats>,
    mut other_gpus: Vec<crate::sysmon_model::GpuStats>,
) -> Vec<crate::sysmon_model::GpuStats> {
    if os_names.is_empty() {
        return nvml_gpus;
    }
    let matches_by_name = |g: &crate::sysmon_model::GpuStats, os_name: &str| {
        let a = g.name.to_lowercase();
        let b = os_name.to_lowercase();
        a == b || a.contains(&b) || b.contains(&a)
    };
    let mut result: Vec<crate::sysmon_model::GpuStats> = Vec::with_capacity(os_names.len());
    for os_name in os_names {
        // NVML first (richer data), then the OS-side collector.
        let matched = nvml_gpus
            .iter()
            .position(|g| matches_by_name(g, os_name))
            .map(|idx| nvml_gpus.remove(idx))
            .or_else(|| {
                other_gpus
                    .iter()
                    .position(|g| matches_by_name(g, os_name))
                    .map(|idx| other_gpus.remove(idx))
            });
        match matched {
            Some(mut g) => {
                // Prefer the OS-formatted name (wmic uses "NVIDIA
                // GeForce RTX 3080 Laptop GPU" while NVML abbreviates).
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
    // Unmatched NVML entries (WSL2 passthrough not visible to
    // wmic/lspci) still get appended so telemetry isn't lost.
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
    #[ignore = "flaky: sysinfo init can exceed 5s on CI"]
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
    #[ignore = "flaky: sysinfo init can exceed 5s on CI"]
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
        let composed = compose_gpu_list(&[], nvml.clone(), Vec::new());
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
        let composed = compose_gpu_list(&os, Vec::new(), Vec::new());
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
        let composed = compose_gpu_list(&os, nvml, Vec::new());
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
        let composed = compose_gpu_list(&os, nvml, Vec::new());
        assert_eq!(composed.len(), 2);
        assert_eq!(composed[0].name, "Intel(R) UHD Graphics");
        assert_eq!(composed[0].utilization, None);
        assert_eq!(composed[1].name, "NVIDIA A100");
        assert_eq!(composed[1].utilization, Some(42.0));
    }

    #[test]
    fn compose_nvml_beats_os_side_source_for_same_card() {
        // NVIDIA cards report through both NVML and WDDM on Windows;
        // NVML data (richer, includes temperature) must win.
        let os = vec!["NVIDIA GeForce RTX 4090".to_string()];
        let nvml = vec![mk_nvml("NVIDIA GeForce RTX 4090")];
        let mut other = mk_nvml("NVIDIA GeForce RTX 4090");
        other.utilization = Some(99.0);
        other.temperature = None;
        let composed = compose_gpu_list(&os, nvml, vec![other]);
        assert_eq!(composed.len(), 1);
        assert_eq!(composed[0].utilization, Some(42.0)); // NVML value
        assert_eq!(composed[0].temperature, Some(58.0));
    }

    #[test]
    fn compose_amd_gets_os_side_telemetry() {
        // The motivating case: NVIDIA dGPU via NVML + AMD iGPU via
        // WDDM/sysfs, both merged onto the OS enumeration.
        let os = vec![
            "AMD Radeon 780M Graphics".to_string(),
            "NVIDIA GeForce RTX 4090".to_string(),
        ];
        let nvml = vec![mk_nvml("NVIDIA GeForce RTX 4090")];
        let mut amd = mk_nvml("AMD Radeon 780M Graphics");
        amd.utilization = Some(7.0);
        amd.temperature = None;
        amd.memory_used = 512 * 1024 * 1024;
        amd.memory_total = 2 * 1024 * 1024 * 1024;
        let composed = compose_gpu_list(&os, nvml, vec![amd]);
        assert_eq!(composed.len(), 2);
        assert_eq!(composed[0].name, "AMD Radeon 780M Graphics");
        assert_eq!(composed[0].utilization, Some(7.0));
        assert_eq!(composed[0].memory_total, 2 * 1024 * 1024 * 1024);
        assert_eq!(composed[1].utilization, Some(42.0));
    }

    #[test]
    fn compose_unmatched_os_side_entry_dropped() {
        // OS-side collectors can't know a device the OS enumeration
        // missed, so unmatched entries are dropped (vs NVML's append).
        let os = vec!["AMD Radeon 780M Graphics".to_string()];
        let other = vec![
            mk_nvml("AMD Radeon 780M Graphics"),
            mk_nvml("Phantom Adapter"),
        ];
        let composed = compose_gpu_list(&os, Vec::new(), other);
        assert_eq!(composed.len(), 1);
        assert_eq!(composed[0].name, "AMD Radeon 780M Graphics");
    }

    // ── luid_from_pdh_instance ────────────────────────────────────

    #[test]
    fn luid_extracted_from_engine_instance() {
        let inst = "pid_1234_luid_0x00000000_0x0000c117_phys_0_eng_0_engtype_3D";
        assert_eq!(
            luid_from_pdh_instance(inst),
            Some("0x00000000_0x0000c117".to_string())
        );
    }

    #[test]
    fn luid_none_when_missing_or_malformed() {
        assert_eq!(luid_from_pdh_instance("pid_1234_phys_0"), None);
        assert_eq!(luid_from_pdh_instance(""), None);
        assert_eq!(luid_from_pdh_instance("luid_not_hex"), None);
    }

    #[test]
    fn luid_extracted_from_adapter_memory_instance() {
        // GPU Adapter Memory instances are the bare LUID pair.
        assert_eq!(
            luid_from_pdh_instance("luid_0x00000000_0x0000c117"),
            Some("0x00000000_0x0000c117".to_string())
        );
    }

    // ── sysfs value parsing ───────────────────────────────────────

    #[test]
    fn sysfs_u64_parses_trimmed_decimal() {
        assert_eq!(parse_sysfs_u64("4096\n"), Some(4096));
        assert_eq!(parse_sysfs_u64("42"), Some(42));
        assert_eq!(parse_sysfs_u64(""), None);
        assert_eq!(parse_sysfs_u64("abc\n"), None);
    }

    #[test]
    fn sysfs_temp_parses_milli_celsius() {
        assert_eq!(parse_temp_milli_c("45000\n"), Some(45.0));
        assert_eq!(parse_temp_milli_c("-12500\n"), Some(-12.5));
        assert_eq!(parse_temp_milli_c("x\n"), None);
    }
}
