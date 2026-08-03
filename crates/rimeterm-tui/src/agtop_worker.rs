//! Background worker for the Native [`AgtopPane`].
//!
//! Owns a single OS thread that receives [`AgtopRequest`]s over an
//! mpsc channel and returns [`AgtopResponse`]s. Follows the same
//! generation-counter pattern [`crate::sysmon_worker`] uses so stale
//! replies from a slow scan land harmlessly when a fresher request
//! has already superseded them.
//!
//! Each tick:
//!
//! 1. [`sysinfo::System::refresh_processes_specifics`] with a
//!    narrowed [`ProcessRefreshKind`] (`cpu`, `memory`, `cmd`, `cwd`,
//!    `exe`, tasks off).
//! 2. Classify every process through [`crate::agtop_matchers`].
//! 3. Enrich Claude rows with session data
//!    ([`crate::agtop_session::enrich_claude`]).
//! 4. Compute cache-aware cost via [`crate::agtop_pricing`].
//! 5. Roll the per-pid CPU% + tokens history rings so the pane's
//!    sparklines are ready to render.
//!
//! Plugins + skills are cached across ticks (plugins host-globally;
//! skills per-cwd) so we don't re-walk the filesystem every 1.5 s
//! for state that changes at most a few times per session.
//!
//! [`AgtopPane`]: crate::agtop_pane::AgtopPane

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::agtop_matchers::{Matcher, UserMatcher, builtin, classify};
use crate::agtop_model::{
    AgentInfo, AgentStatus, AgtopRequest, AgtopResponse, CMDLINE_TRUNCATE, HISTORY_CAP, Snapshot,
};
use crate::agtop_pricing::{CostBasis, PriceTable};
use crate::agtop_session::{
    LiveAgentRef, SessionSummary, dangerous_flag, enabled_plugins, enrich_claude, skills_for_cwd,
};

/// How often we re-check the host-global plugin list. Users install
/// plugins one at a time; re-scanning every worker tick is pure I/O
/// waste.
const PLUGIN_CACHE_TTL: Duration = Duration::from_secs(30);
/// Cap on cached per-cwd skill lists. Keeps the sampler from holding
/// onto skills for cwds it will never see again (agent exited, user
/// cd'd elsewhere).
const SKILLS_CACHE_CAP: usize = 64;

/// Handle to the running worker thread. Cloneable only through the
/// `send` API — the sending side is `Sender` which is already
/// `Clone`, but the receive side is single-owner so we keep
/// `AgtopWorker` non-clonable to enforce it.
pub struct AgtopWorker {
    request_tx: Sender<AgtopRequest>,
    response_rx: Receiver<AgtopResponse>,
}

impl AgtopWorker {
    pub fn spawn() -> Self {
        Self::spawn_with_user_matchers(Vec::new())
    }

    /// Spawn with user-supplied `label=regex` matchers already
    /// parsed. Reserved for future IPC / config wiring — the pane
    /// currently starts with `spawn()` and gets the builtins only.
    pub fn spawn_with_user_matchers(user: Vec<UserMatcher>) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<AgtopRequest>();
        let (resp_tx, resp_rx) = mpsc::channel::<AgtopResponse>();
        thread::Builder::new()
            .name("rimeterm-agtop-worker".into())
            .spawn(move || run(req_rx, resp_tx, user))
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
    /// call. Non-blocking.
    pub fn drain(&self) -> Vec<AgtopResponse> {
        let mut out = Vec::new();
        while let Ok(response) = self.response_rx.try_recv() {
            out.push(response);
        }
        out
    }
}

// Channel endpoints are moved into the thread so they drop when the
// thread exits — that's the pane-dropped shutdown signal. Clippy's
// "not consumed in the function body" heuristic misses the drop.
#[allow(clippy::needless_pass_by_value)]
fn run(
    req_rx: Receiver<AgtopRequest>,
    resp_tx: Sender<AgtopResponse>,
    user_matchers: Vec<UserMatcher>,
) {
    let mut sampler = Sampler::new(user_matchers);
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
                    return; // pane dropped, exit thread
                }
            }
        }
    }
}

/// Per-pid carry-over between ticks. Keyed on `(pid, start_time_ms)`
/// so a recycled pid can't inherit a dead one's history — a
/// `(pid=1234, uptime=3)` after we saw `(pid=1234, uptime=9999)`
/// is treated as a fresh process.
#[derive(Clone, Debug, Default)]
struct HistoryEntry {
    start_ms: u64,
    cpu_history: Vec<f64>,
    tokens_history: Vec<f64>,
    prev_tokens_total: u64,
}

/// Cached skills list for a cwd — kept alongside its last-seen tick
/// so we can prune stale entries when the agent exits or moves.
#[derive(Clone, Debug)]
struct SkillsCacheEntry {
    skills: Vec<String>,
    last_seen_tick: u64,
}

/// One `Sampler` per worker thread. Holds the `sysinfo::System`
/// plus the compiled matcher table + pricing table + host-global
/// plugin cache so we don't re-parse them per tick.
struct Sampler {
    system: System,
    matchers: Vec<Matcher>,
    user_matchers: Vec<UserMatcher>,
    prices: PriceTable,
    plugins_cache: Vec<String>,
    plugins_cached_at: Instant,
    /// Per-cwd skills cache — LRU-ish; capacity-bounded by evicting
    /// entries not seen in the last few ticks.
    skills_cache: HashMap<String, SkillsCacheEntry>,
    history: HashMap<u32, HistoryEntry>,
    tick: u64,
}

impl Sampler {
    fn new(user_matchers: Vec<UserMatcher>) -> Self {
        let mut system = System::new();
        // Prime the delta counter so the very first sample returns
        // real CPU% instead of 0.0.
        system.refresh_cpu_usage();
        Self {
            system,
            matchers: builtin(),
            user_matchers,
            prices: PriceTable::builtin(),
            plugins_cache: Vec::new(),
            plugins_cached_at: Instant::now() - PLUGIN_CACHE_TTL * 2,
            skills_cache: HashMap::new(),
            history: HashMap::new(),
            tick: 0,
        }
    }

    fn refresh(&mut self) -> Snapshot {
        self.tick = self.tick.saturating_add(1);
        self.system.refresh_cpu_usage();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            process_refresh_kind(),
        );

        // Phase 1 — walk sysinfo, produce a bare per-process bucket.
        let mut raw: Vec<RawAgent> = Vec::new();
        for (pid, process) in self.system.processes() {
            let cmdline = join_cmdline(process.cmd());
            // Fall back to the exe path when argv is hidden (Windows
            // kernel threads, some sysinfo backends). Without this
            // an agent launched via a shim whose argv is elided
            // would silently miss the matcher table.
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
            let exe_str = process
                .exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let project = project_label(cwd_path, process.exe());
            let cmd_stored = truncate_utf8(&match_target, CMDLINE_TRUNCATE);
            let cpu = process.cpu_usage();
            let rss = process.memory();

            raw.push(RawAgent {
                label: label.to_string(),
                pid: pid.as_u32(),
                cpu,
                rss,
                uptime_sec: process.run_time(),
                cwd: cwd_str,
                exe: exe_str,
                project,
                cmdline: cmd_stored,
                ppid: process.parent().map(Pid::as_u32).unwrap_or(0),
            });
        }

        // Dedupe launcher / worker chains. oh-my-pi's `omp.exe`
        // spawns a `bun … @oh-my-pi/pi-coding-agent/cli.js` child;
        // both match the matcher table (unified label `omp` — see
        // `agtop_matchers::builtin`), which used to make a single
        // logical session render as TWO rows. General rule: if a
        // matched agent's parent pid is ALSO a matched agent with
        // the same label AND cwd, the child is a worker of the
        // parent — drop it and keep the launcher. Different label
        // OR different cwd (`bash` → `claude` in a subshell) keeps
        // both rows because they're independent logical sessions.
        let raw = dedupe_child_workers(raw);

        // Phase 2 — enrich rows from session transcripts. Claude +
        // oh-my-pi enrichers run in parallel; results merge per pid,
        // with oh-my-pi's provider-computed cost preferred over the
        // pricing-table estimate when present.
        let live: Vec<LiveAgentRef<'_>> = raw
            .iter()
            .map(|r| LiveAgentRef {
                pid: r.pid,
                cwd: &r.cwd,
                label: &r.label,
                uptime_sec: r.uptime_sec,
            })
            .collect();
        let now_ms = now_ms();
        let claude_sessions = enrich_claude(&live, now_ms);
        let omp_sessions = crate::agtop_omp::enrich_omp(&live, now_ms);
        // Merge: omp wins on conflict (its cost is authoritative).
        let mut sessions_by_pid = claude_sessions;
        for (pid, summary) in omp_sessions {
            sessions_by_pid.insert(pid, summary);
        }

        // Phase 3 — refresh caches (plugins host-wide; skills per-cwd).
        self.refresh_plugin_cache();
        self.refresh_skills_cache(&raw);

        // Phase 4 — resolve parent-process names via sysinfo lookup.
        // Done after the walk so `System::process(Pid)` sees a fully
        // refreshed table.
        let ppid_names: HashMap<u32, String> = self.resolve_ppid_names(&raw);

        // Phase 5 — fold sysinfo + session data into `AgentInfo`,
        // apply the cost calc, and roll history rings.
        let mut agents: Vec<AgentInfo> = Vec::with_capacity(raw.len());
        let mut alive: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for r in raw {
            alive.insert(r.pid);
            let session = sessions_by_pid.get(&r.pid).cloned();
            agents.push(self.build_agent(r, session, &ppid_names, now_ms));
        }

        // Retire history for pids that have exited so the map
        // doesn't grow unbounded on long-running rimeterms.
        self.history.retain(|pid, _| alive.contains(pid));
        // Evict skills-cache entries not seen in the last few ticks.
        self.evict_stale_skills();

        // Default row order: hottest CPU first, matching sysmon_pane
        // convention. The pane re-sorts per user preference.
        agents.sort_by(|a, b| {
            b.cpu
                .partial_cmp(&a.cpu)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.pid.cmp(&b.pid))
        });

        let total_cpu = agents.iter().map(|a| a.cpu).sum();
        let total_rss = agents.iter().map(|a| a.rss).sum();
        let total_tokens = agents.iter().map(|a| a.tokens_total).sum();
        let total_cost_usd = agents
            .iter()
            .filter(|a| a.cost_basis == CostBasis::Api)
            .map(|a| a.cost_usd)
            .sum();

        Snapshot {
            agents,
            total_cpu,
            total_rss,
            total_tokens,
            total_cost_usd,
            sampled_at: Instant::now(),
            sampled_at_ms: now_ms,
        }
    }

    fn refresh_plugin_cache(&mut self) {
        if self.plugins_cached_at.elapsed() < PLUGIN_CACHE_TTL {
            return;
        }
        self.plugins_cache = enabled_plugins();
        self.plugins_cached_at = Instant::now();
    }

    fn refresh_skills_cache(&mut self, raw: &[RawAgent]) {
        // Scan skills for every UNIQUE cwd we've seen this tick —
        // multiple `claude` processes in the same repo share a
        // skills list. Cache lives for as long as we keep seeing the
        // cwd.
        let mut seen_cwds: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for r in raw {
            if r.label != "claude" && r.label != "claude-code" {
                continue;
            }
            if r.cwd.is_empty() || !seen_cwds.insert(r.cwd.as_str()) {
                continue;
            }
            let entry =
                self.skills_cache
                    .entry(r.cwd.clone())
                    .or_insert_with(|| SkillsCacheEntry {
                        skills: Vec::new(),
                        last_seen_tick: self.tick,
                    });
            entry.last_seen_tick = self.tick;
            // Refresh the list every ~10 ticks (~15 s) so
            // newly-dropped-in skills show up quickly without
            // filesystem-walking every tick.
            if self.tick.is_multiple_of(10) || entry.skills.is_empty() {
                entry.skills = skills_for_cwd(&r.cwd);
            }
        }
    }

    fn evict_stale_skills(&mut self) {
        // Drop entries not seen in the last 20 ticks (~30 s) OR when
        // the cache would exceed the cap.
        let tick = self.tick;
        self.skills_cache
            .retain(|_, v| tick.saturating_sub(v.last_seen_tick) < 20);
        if self.skills_cache.len() > SKILLS_CACHE_CAP {
            let mut entries: Vec<(String, u64)> = self
                .skills_cache
                .iter()
                .map(|(k, v)| (k.clone(), v.last_seen_tick))
                .collect();
            entries.sort_by_key(|(_, t)| *t);
            let over = self.skills_cache.len() - SKILLS_CACHE_CAP;
            for (k, _) in entries.into_iter().take(over) {
                self.skills_cache.remove(&k);
            }
        }
    }

    fn resolve_ppid_names(&self, raw: &[RawAgent]) -> HashMap<u32, String> {
        let mut out = HashMap::new();
        for r in raw {
            if r.ppid == 0 || out.contains_key(&r.ppid) {
                continue;
            }
            if let Some(parent) = self.system.process(Pid::from_u32(r.ppid)) {
                let name = parent.name().to_string_lossy().into_owned();
                out.insert(r.ppid, name);
            }
        }
        out
    }

    // `session` is owned so callers can pass a freshly-removed
    // HashMap entry without keeping a live borrow around the
    // whole `refresh()` scope.
    #[allow(clippy::needless_pass_by_value)]
    fn build_agent(
        &mut self,
        r: RawAgent,
        session: Option<SessionSummary>,
        ppid_names: &HashMap<u32, String>,
        now_ms: u64,
    ) -> AgentInfo {
        let is_claude = r.label == "claude" || r.label == "claude-code";
        let (loaded_skills, loaded_plugins) = if is_claude {
            let skills = self
                .skills_cache
                .get(&r.cwd)
                .map(|e| e.skills.clone())
                .unwrap_or_default();
            (skills, self.plugins_cache.clone())
        } else {
            (Vec::new(), Vec::new())
        };

        let dangerous_str = dangerous_flag(&r.cmdline).unwrap_or("");
        let dangerous = !dangerous_str.is_empty();

        // Session-derived fields (all `None`/`0`/`default` when the
        // Claude enricher didn't return anything for this pid).
        let session_state = session.as_ref().map(|s| s.state);
        let model = session.as_ref().and_then(|s| s.model.clone());
        let session_id = session.as_ref().map(|s| s.session_id.clone());
        let session_started_ms = session.as_ref().map(|s| s.session_started_ms).unwrap_or(0);
        let current_tool = session.as_ref().and_then(|s| s.current_tool.clone());
        let current_task = session.as_ref().and_then(|s| s.current_task.clone());
        let in_flight_subagents = session
            .as_ref()
            .map(|s| s.in_flight_subagents.clone())
            .unwrap_or_default();
        let subagents = session.as_ref().map(|s| s.subagents_count).unwrap_or(0);
        let recent_activity = session
            .as_ref()
            .map(|s| s.recent_activity.clone())
            .unwrap_or_default();
        let tokens_input = session.as_ref().map(|s| s.tokens_input).unwrap_or(0);
        let tokens_output = session.as_ref().map(|s| s.tokens_output).unwrap_or(0);
        let tokens_cache_read = session.as_ref().map(|s| s.tokens_cache_read).unwrap_or(0);
        let tokens_cache_write = session.as_ref().map(|s| s.tokens_cache_write).unwrap_or(0);
        let context_used = session.as_ref().map(|s| s.context_used).unwrap_or(0);
        let tool_counts = session
            .as_ref()
            .map(|s| s.tool_counts.clone())
            .unwrap_or_default();
        let tokens_total = session.as_ref().map(|s| s.tokens_total()).unwrap_or(0);
        let context_limit = model
            .as_deref()
            .map(|m| self.prices.context_limit(m))
            .unwrap_or(200_000);
        // Cost + basis resolution. Priority order:
        //   1. Vendor-emitted `cost_provider` (oh-my-pi's authoritative
        //      per-turn bill) — always Api basis when present.
        //   2. Pricing-table lookup on the model id (Claude native
        //      transcripts + any vendor that hasn't emitted a cost).
        //   3. `Unknown` basis (no model / no table hit) → $0 with
        //      the `unknown` tag so the row doesn't misread as free.
        let provider_cost = session.as_ref().and_then(|s| s.cost_provider);
        let (cost_basis, cost_usd) = if let Some(c) = provider_cost {
            (CostBasis::Api, c)
        } else {
            let basis = model
                .as_deref()
                .map(|m| self.prices.cost_basis(m))
                .unwrap_or(CostBasis::Unknown);
            let cost = model
                .as_deref()
                .map(|m| {
                    self.prices.cost_with_cache(
                        m,
                        tokens_input,
                        tokens_output,
                        tokens_cache_read,
                        tokens_cache_write,
                    )
                })
                .unwrap_or(0.0);
            (basis, cost)
        };

        let status = AgentStatus::classify(r.cpu, session_state);
        let ppid_name = ppid_names.get(&r.ppid).cloned().unwrap_or_default();

        // History rings. `start_ms` is derived from now-uptime so a
        // recycled pid gets a fresh ring rather than inheriting the
        // dead one's.
        let start_ms = now_ms.saturating_sub(r.uptime_sec.saturating_mul(1000));
        let (cpu_history, tokens_history) = self.roll_history(r.pid, start_ms, r.cpu, tokens_total);

        AgentInfo {
            label: r.label,
            pid: r.pid,
            cpu: r.cpu,
            rss: r.rss,
            uptime_sec: r.uptime_sec,
            cwd: r.cwd,
            exe: r.exe,
            project: r.project,
            cmdline: r.cmdline,
            ppid: r.ppid,
            ppid_name,
            status,
            dangerous_flag: dangerous_str.to_string(),
            dangerous,
            model,
            session_id,
            session_started_ms,
            current_tool,
            current_task,
            subagents,
            in_flight_subagents,
            recent_activity,
            tokens_input,
            tokens_output,
            tokens_cache_read,
            tokens_cache_write,
            tokens_total,
            cost_usd,
            cost_basis,
            context_used,
            context_limit,
            tool_counts,
            loaded_skills,
            loaded_plugins,
            cpu_history,
            tokens_history,
        }
    }

    fn roll_history(
        &mut self,
        pid: u32,
        start_ms: u64,
        cpu: f32,
        tokens_total: u64,
    ) -> (Vec<f64>, Vec<f64>) {
        // ±2 s wobble tolerance on the derived start time — sysinfo
        // rounds `run_time` to whole seconds so successive samples
        // can differ by one even for a stable process.
        let entry = self
            .history
            .entry(pid)
            .and_modify(|e| {
                if start_ms.abs_diff(e.start_ms) > 2000 {
                    // pid reused; wipe the ring.
                    e.cpu_history.clear();
                    e.tokens_history.clear();
                    e.prev_tokens_total = 0;
                    e.start_ms = start_ms;
                }
            })
            .or_insert_with(|| HistoryEntry {
                start_ms,
                cpu_history: Vec::with_capacity(HISTORY_CAP),
                tokens_history: Vec::with_capacity(HISTORY_CAP),
                prev_tokens_total: tokens_total,
            });

        push_ring(&mut entry.cpu_history, cpu as f64, HISTORY_CAP);
        let delta = tokens_total.saturating_sub(entry.prev_tokens_total) as f64;
        push_ring(&mut entry.tokens_history, delta, HISTORY_CAP);
        entry.prev_tokens_total = tokens_total;

        (entry.cpu_history.clone(), entry.tokens_history.clone())
    }
}

/// Per-tick scratch: sysinfo bucket before session enrichment.
#[derive(Clone)]
struct RawAgent {
    label: String,
    pid: u32,
    cpu: f32,
    rss: u64,
    uptime_sec: u64,
    cwd: String,
    exe: String,
    project: String,
    cmdline: String,
    ppid: u32,
}

/// Collapse launcher / worker chains to one row per logical session
/// while preserving the descendants' resource usage.
///
/// A matched agent whose parent pid is ALSO a matched agent with the
/// SAME label AND cwd is treated as a worker of the parent (e.g.
/// `omp.exe` → `bun … @oh-my-pi/pi-coding-agent`). Instead of showing
/// two rows for one session, we drop the child and roll its `cpu` +
/// `rss` into the nearest surviving ancestor — otherwise the pane
/// would render the (idle) launcher's 0.0% CPU and miss the bun /
/// node worker that owns the real load.
///
/// Two guards on the drop:
/// - Same `label` required: a `bash`-child `claude` inside an `omp`
///   subshell is a distinct logical session and MUST NOT be dropped.
/// - Same `cwd` required: a launcher whose worker `cd`d elsewhere
///   still counts as a separate session because the JSONL it maps
///   to lives under a different encoded dir.
fn dedupe_child_workers(mut raw: Vec<RawAgent>) -> Vec<RawAgent> {
    if raw.len() < 2 {
        return raw;
    }

    // Snapshot label + cwd + ppid before we start mutating — the
    // fold rewrites cpu/rss on the retained rows but we need the
    // original relationships to decide who drops and who inherits.
    #[derive(Clone)]
    struct Node {
        ppid: u32,
        label: String,
        cwd: String,
    }
    let nodes: HashMap<u32, Node> = raw
        .iter()
        .filter(|r| r.pid != 0)
        .map(|r| {
            (
                r.pid,
                Node {
                    ppid: r.ppid,
                    label: r.label.clone(),
                    cwd: r.cwd.clone(),
                },
            )
        })
        .collect();

    // Resolve each pid to the pid it should FOLD INTO — walk up the
    // ppid chain as long as parent+child share label+cwd (i.e. the
    // parent is a worker's launcher). The terminal pid is either a
    // matched agent whose parent is unmatched / different, or has no
    // parent in the table. `HashMap<u32, u32>` memoises so a deep
    // chain resolves in a single traversal.
    let mut fold_into: HashMap<u32, u32> = HashMap::with_capacity(raw.len());
    for r in &raw {
        if r.pid == 0 {
            continue;
        }
        let mut cur = r.pid;
        loop {
            let Some(node) = nodes.get(&cur) else { break };
            let Some(parent) = nodes.get(&node.ppid) else {
                break;
            };
            if parent.label != node.label || parent.cwd != node.cwd {
                break;
            }
            cur = node.ppid;
        }
        fold_into.insert(r.pid, cur);
    }

    // Fold cpu + rss from every child into the ancestor it resolves
    // to. Two-pass so the mutable borrow of `raw` doesn't conflict
    // with the immutable index we needed to compute contributions.
    let mut contrib: HashMap<u32, (f32, u64)> = HashMap::new();
    for r in &raw {
        let Some(&anchor) = fold_into.get(&r.pid) else {
            continue;
        };
        if anchor == r.pid {
            continue; // survivor — its own cpu/rss stay put.
        }
        let entry = contrib.entry(anchor).or_insert((0.0, 0));
        entry.0 += r.cpu;
        entry.1 = entry.1.saturating_add(r.rss);
    }

    raw.retain(|r| fold_into.get(&r.pid).copied() == Some(r.pid) || r.pid == 0);
    for r in &mut raw {
        if let Some((cpu_add, rss_add)) = contrib.get(&r.pid) {
            r.cpu += *cpu_add;
            r.rss = r.rss.saturating_add(*rss_add);
        }
    }
    raw
}

fn push_ring(ring: &mut Vec<f64>, sample: f64, cap: usize) {
    if ring.len() >= cap {
        // Drop the oldest — cheap on a `Vec` of 24 entries; a
        // VecDeque would be overkill.
        ring.remove(0);
    }
    ring.push(sample);
}

/// Refresh kind used for every `refresh_processes_specifics` call.
///
/// Turns on:
/// - `cpu` — required for the sort/status classification
/// - `memory` — RSS column
/// - `cmd` (`Always`) — argv-based matcher classification
/// - `cwd` (`Always`) — project label + Claude session pairing
/// - `exe` (`Always`) — detail popup / matcher fallback
///
/// `tasks` stays disabled so the Linux per-thread walk doesn't
/// dominate the sample cost on heavily-multithreaded systems.
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
/// occasionally Windows) round-trips as `U+FFFD` rather than
/// dropping the sample entirely.
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

/// Short project identifier for the pane's `PROJECT` column:
/// basename of cwd when available, else basename of exe, else empty.
fn project_label(cwd: Option<&Path>, exe: Option<&Path>) -> String {
    cwd.and_then(Path::file_name)
        .or_else(|| exe.and_then(Path::file_name))
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Truncate `s` to at most `max` bytes at a valid UTF-8 boundary.
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

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
    fn truncate_utf8_respects_char_boundary() {
        let s = "aあb"; // 5 bytes: 1 + 3 + 1
        assert_eq!(truncate_utf8(s, 2), "a");
        assert_eq!(truncate_utf8(s, 4), "aあ");
        assert_eq!(truncate_utf8(s, 5), "aあb");
        assert_eq!(truncate_utf8(s, 100), "aあb");
    }

    #[test]
    fn push_ring_evicts_oldest_at_capacity() {
        let mut ring = Vec::new();
        for i in 0..30 {
            push_ring(&mut ring, i as f64, 5);
        }
        assert_eq!(ring.len(), 5);
        // Last five in.
        assert_eq!(ring, vec![25.0, 26.0, 27.0, 28.0, 29.0]);
    }

    #[test]
    fn roll_history_wipes_ring_on_pid_reuse() {
        let mut sampler = Sampler::new(Vec::new());
        // Seed history for pid 42 with a start_ms of 1000.
        let (cpu, _) = sampler.roll_history(42, 1000, 5.0, 100);
        assert_eq!(cpu.len(), 1);
        let (cpu, _) = sampler.roll_history(42, 1000, 6.0, 200);
        assert_eq!(cpu.len(), 2);
        // Reuse pid 42 with a different start_ms — ring MUST reset.
        let (cpu, tokens) = sampler.roll_history(42, 9_999_999, 1.0, 50);
        assert_eq!(cpu.len(), 1);
        assert_eq!(cpu, vec![1.0]);
        assert_eq!(tokens, vec![50.0]);
    }

    #[test]
    fn worker_produces_snapshot_with_matching_generation() {
        // Real-process smoke test: spawn a worker, ask for one
        // snapshot, verify the reply matches the requested generation.
        // Not asserting `agents.len() > 0` because CI shouldn't be
        // running an AI agent — but the machinery has to work.
        let worker = AgtopWorker::spawn();
        worker.send(AgtopRequest::Snapshot { generation: 7 });

        let response = worker
            .response_rx
            .recv_timeout(Duration::from_secs(15))
            .expect("agtop worker never produced a snapshot within 15 s");
        let AgtopResponse::Snapshot {
            generation,
            snapshot,
        } = response;
        assert_eq!(generation, 7);
        assert!(snapshot.sampled_at <= Instant::now());
    }

    #[test]
    fn worker_second_request_reuses_history() {
        // Two back-to-back snapshots: the worker survives, the pane
        // sees the second reply. Doesn't assert on history contents
        // (host may have zero agents) but exercises the channel
        // handshake twice.
        let worker = AgtopWorker::spawn();
        worker.send(AgtopRequest::Snapshot { generation: 1 });
        let _ = worker
            .response_rx
            .recv_timeout(Duration::from_secs(15))
            .expect("first snapshot");
        worker.send(AgtopRequest::Snapshot { generation: 2 });
        let AgtopResponse::Snapshot { generation, .. } = worker
            .response_rx
            .recv_timeout(Duration::from_secs(15))
            .expect("second snapshot");
        assert_eq!(generation, 2);
    }

    #[test]
    fn worker_drain_empty_when_nothing_pending() {
        let (_req_tx, req_rx) = channel::<AgtopRequest>();
        let (resp_tx, resp_rx) = channel::<AgtopResponse>();
        // Never spawn a run loop — drain() must still return
        // immediately with an empty vec.
        drop(resp_tx);
        drop(req_rx);
        let worker = AgtopWorker {
            request_tx: {
                let (tx, _rx) = channel::<AgtopRequest>();
                tx
            },
            response_rx: resp_rx,
        };
        assert!(worker.drain().is_empty());
    }

    fn raw(pid: u32, ppid: u32, label: &str, cwd: &str) -> RawAgent {
        raw_with_load(pid, ppid, label, cwd, 0.0, 0)
    }

    fn raw_with_load(pid: u32, ppid: u32, label: &str, cwd: &str, cpu: f32, rss: u64) -> RawAgent {
        RawAgent {
            label: label.into(),
            pid,
            cpu,
            rss,
            uptime_sec: 0,
            cwd: cwd.into(),
            exe: String::new(),
            project: String::new(),
            cmdline: String::new(),
            ppid,
        }
    }

    #[test]
    fn dedupe_drops_worker_child_of_launcher() {
        // Real Windows shape: omp.exe launcher spawns a bun worker
        // running the @oh-my-pi npm shim; both classify as `omp`,
        // same cwd. The child MUST get dropped so the pane shows
        // exactly one row per logical session.
        let launcher = raw(30828, 26692, "omp", r"C:\Users\z\proj");
        let worker = raw(27308, 30828, "omp", r"C:\Users\z\proj");
        let out = dedupe_child_workers(vec![launcher, worker]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pid, 30828, "the launcher should survive");
    }

    #[test]
    fn dedupe_folds_child_load_into_launcher() {
        // The regression: omp.exe (launcher) is idle, but bun (child)
        // owns the real CPU + RSS. Before the fold-fix, keeping only
        // the launcher meant the pane displayed 0.0% CPU forever
        // even while a session was actively burning cycles.
        let launcher = raw_with_load(30828, 26692, "omp", r"C:\p", 0.1, 12 * 1024 * 1024);
        let worker = raw_with_load(27308, 30828, "omp", r"C:\p", 42.5, 480 * 1024 * 1024);
        let out = dedupe_child_workers(vec![launcher, worker]);
        assert_eq!(out.len(), 1);
        let survivor = &out[0];
        assert_eq!(survivor.pid, 30828);
        assert!(
            (survivor.cpu - 42.6).abs() < 0.001,
            "child cpu folded into launcher: got {}",
            survivor.cpu
        );
        assert_eq!(
            survivor.rss,
            (12 + 480) * 1024 * 1024,
            "child rss folded into launcher"
        );
    }

    #[test]
    fn dedupe_folds_multi_level_chain() {
        // launcher → middle → leaf, all same label + cwd. Every
        // descendant folds up to the launcher; middle + leaf drop.
        let launcher = raw_with_load(100, 1, "omp", "/p", 0.0, 0);
        let middle = raw_with_load(200, 100, "omp", "/p", 3.0, 100);
        let leaf = raw_with_load(300, 200, "omp", "/p", 7.0, 200);
        let out = dedupe_child_workers(vec![launcher, middle, leaf]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pid, 100);
        assert!((out[0].cpu - 10.0).abs() < 0.001);
        assert_eq!(out[0].rss, 300);
    }

    #[test]
    fn dedupe_keeps_bash_child_that_ran_claude() {
        // A `claude` invoked from a `bash` shell is a DISTINCT logical
        // session even though the parent bash isn't a matched agent
        // — we only compare against matched agents so this case is
        // trivially safe. But the OPPOSITE case matters: two matched
        // agents in a chain but with DIFFERENT labels (a bash-inside-
        // omp that spawned `claude`) MUST both survive.
        let omp = raw(100, 1, "omp", "/proj");
        let claude = raw(200, 100, "claude", "/proj");
        let out = dedupe_child_workers(vec![omp, claude]);
        assert_eq!(out.len(), 2, "different labels → both kept");
    }

    #[test]
    fn dedupe_keeps_child_that_cdd_elsewhere() {
        // Same label but different cwd → different session (a
        // subagent that changed directory to run a task).
        let launcher = raw(100, 1, "omp", "/projA");
        let child = raw(200, 100, "omp", "/projB");
        let out = dedupe_child_workers(vec![launcher, child]);
        assert_eq!(out.len(), 2, "different cwd → both kept");
    }

    #[test]
    fn dedupe_no_op_on_singleton_or_orphan_children() {
        // Only one agent → passes through untouched.
        let one = raw(100, 1, "omp", "/p");
        assert_eq!(dedupe_child_workers(vec![one]).len(), 1);

        // Parent ppid points at a non-agent pid (e.g. `bash`) → keep
        // the child.
        let orphan = raw(200, 999, "omp", "/p");
        assert_eq!(dedupe_child_workers(vec![orphan]).len(), 1);
    }
}
