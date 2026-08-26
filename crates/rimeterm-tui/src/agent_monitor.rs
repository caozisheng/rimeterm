use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use rimeterm_core::pane::PaneId;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::agtop_model::{AgentInfo, AgentStatus, AgtopRequest, AgtopResponse, Snapshot};
use crate::agtop_worker::AgtopWorker;

const SAMPLE_INTERVAL: Duration = Duration::from_millis(1500);

pub type SharedAgentSnapshot = Arc<RwLock<Snapshot>>;
pub type SharedMainAgentSignal = Arc<RwLock<MainAgentSignal>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MainAgentPhase {
    Unbound,
    Starting,
    Observed(AgentStatus),
    MonitorStale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MainAgentSignal {
    pub pane_id: Option<PaneId>,
    pub agent_id: Option<String>,
    pub phase: MainAgentPhase,
    pub transition_seq: u64,
}

impl Default for MainAgentSignal {
    fn default() -> Self {
        Self {
            pane_id: None,
            agent_id: None,
            phase: MainAgentPhase::Unbound,
            transition_seq: 0,
        }
    }
}

pub struct AgentMonitor {
    worker: AgtopWorker,
    snapshot: SharedAgentSnapshot,
    requested_generation: u64,
    applied_generation: u64,
    last_request: Instant,
}

impl AgentMonitor {
    pub fn new(snapshot: SharedAgentSnapshot) -> Self {
        let worker = AgtopWorker::spawn();
        worker.send(AgtopRequest::Snapshot { generation: 1 });
        Self {
            worker,
            snapshot,
            requested_generation: 1,
            applied_generation: 0,
            last_request: Instant::now(),
        }
    }

    pub fn poll(&mut self) -> bool {
        if self.last_request.elapsed() >= SAMPLE_INTERVAL {
            self.requested_generation = self.requested_generation.saturating_add(1);
            self.worker.send(AgtopRequest::Snapshot {
                generation: self.requested_generation,
            });
            self.last_request = Instant::now();
        }
        let mut changed = false;
        for response in self.worker.drain() {
            let AgtopResponse::Snapshot {
                generation,
                snapshot,
            } = response;
            if generation < self.applied_generation {
                continue;
            }
            self.applied_generation = generation;
            *self.snapshot.write() = snapshot;
            changed = true;
        }
        changed
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.read().clone()
    }
}

pub fn match_main_agent(root_pid: u32, snapshot: &Snapshot) -> Option<&AgentInfo> {
    let exact = snapshot.agents.iter().find(|agent| agent.pid == root_pid);
    if let Some(launcher) = exact
        && launcher.label == "omp"
        && let Some(worker) = snapshot.agents.iter().find(|agent| {
            agent.label == "omp"
                && agent.session_id.is_some()
                && descendant_of_snapshot(snapshot, agent.pid, root_pid)
        })
    {
        return Some(worker);
    }
    if exact.is_some() {
        return exact;
    }
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
    );
    snapshot
        .agents
        .iter()
        .filter_map(|agent| {
            descendant_depth(&system, agent.pid, root_pid).map(|depth| (depth, agent))
        })
        .min_by_key(|(depth, agent)| (*depth, agent.status.rank(), agent.pid))
        .map(|(_, agent)| agent)
}

fn descendant_of_snapshot(snapshot: &Snapshot, pid: u32, ancestor: u32) -> bool {
    let mut current = pid;
    let mut seen = Vec::with_capacity(8);
    for _ in 0..32 {
        if current == ancestor {
            return true;
        }
        if seen.contains(&current) {
            return false;
        }
        seen.push(current);
        current = snapshot
            .agents
            .iter()
            .find(|agent| agent.pid == current)
            .map(|agent| agent.ppid)
            .unwrap_or(0);
        if current == 0 {
            return false;
        }
    }
    false
}

fn descendant_depth(system: &System, pid: u32, ancestor: u32) -> Option<usize> {
    let mut current = pid;
    let mut seen = Vec::with_capacity(8);
    for depth in 0..32 {
        if current == ancestor {
            return Some(depth);
        }
        if seen.contains(&current) {
            return None;
        }
        seen.push(current);
        current = system.process(Pid::from_u32(current))?.parent()?.as_u32();
    }
    None
}
pub fn resolve_main_phase(
    root_pid: Option<u32>,
    snapshot: &Snapshot,
    now: Instant,
) -> MainAgentPhase {
    let Some(root_pid) = root_pid else {
        return MainAgentPhase::Unbound;
    };
    if now.saturating_duration_since(snapshot.sampled_at) > Duration::from_secs(5) {
        return MainAgentPhase::MonitorStale;
    }
    match_main_agent(root_pid, snapshot)
        .map(|agent| MainAgentPhase::Observed(agent.status))
        .unwrap_or(MainAgentPhase::Starting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agtop_pricing::CostBasis;

    fn agent(pid: u32) -> AgentInfo {
        AgentInfo {
            label: "pi".into(),
            pid,
            cpu: 0.0,
            rss: 0,
            uptime_sec: 0,
            cwd: String::new(),
            exe: String::new(),
            project: String::new(),
            cmdline: String::new(),
            ppid: 0,
            ppid_name: String::new(),
            status: AgentStatus::Idle,
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
            tokens_total: 0,
            cost_usd: 0.0,
            cost_basis: CostBasis::Unknown,
            context_used: 0,
            context_limit: 0,
            tool_counts: Vec::new(),
            loaded_skills: Vec::new(),
            loaded_plugins: Vec::new(),
            cpu_history: Vec::new(),
            tokens_history: Vec::new(),
        }
    }

    #[test]
    fn omp_child_busy_status_is_selected_for_pet_signal() {
        let mut launcher = agent(42);
        launcher.label = "omp".into();
        let mut worker = agent(43);
        worker.label = "omp".into();
        worker.ppid = 42;
        worker.session_id = Some("session".into());
        worker.status = AgentStatus::Busy;
        let mut snapshot = Snapshot::empty();
        snapshot.agents = vec![launcher, worker];

        assert_eq!(
            match_main_agent(42, &snapshot).map(|agent| agent.status),
            Some(AgentStatus::Busy)
        );
    }
    #[test]
    fn exact_root_pid_matches_agent() {
        let mut snapshot = Snapshot::empty();
        snapshot.agents.push(agent(42));
        assert_eq!(
            match_main_agent(42, &snapshot).map(|agent| agent.pid),
            Some(42)
        );
    }

    #[test]
    fn omp_launcher_prefers_enriched_bun_child() {
        let mut launcher = agent(42);
        launcher.label = "omp".into();
        launcher.status = AgentStatus::Idle;
        let mut worker = agent(43);
        worker.label = "omp".into();
        worker.ppid = 42;
        worker.session_id = Some("omp-session".into());
        worker.status = AgentStatus::Busy;
        let mut snapshot = Snapshot::empty();
        snapshot.agents = vec![launcher, worker];

        let matched = match_main_agent(42, &snapshot).expect("match OMP logical process");

        assert_eq!((matched.pid, matched.status), (43, AgentStatus::Busy));
    }
}

#[test]
fn missing_main_agent_row_is_starting_while_snapshot_is_fresh() {
    let snapshot = Snapshot::empty();

    assert_eq!(
        resolve_main_phase(Some(42), &snapshot, Instant::now()),
        MainAgentPhase::Starting
    );
}

#[test]
fn old_snapshot_is_monitor_stale() {
    let mut snapshot = Snapshot::empty();
    snapshot.sampled_at = Instant::now() - Duration::from_secs(6);

    assert_eq!(
        resolve_main_phase(Some(42), &snapshot, Instant::now()),
        MainAgentPhase::MonitorStale
    );
}
