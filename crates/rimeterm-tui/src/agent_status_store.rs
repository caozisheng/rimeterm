use std::collections::HashMap;
use std::time::Instant;

use rimeterm_agent_status::AgentStatusSnapshot;
use rimeterm_core::pane::PaneId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusSource {
    Ipc,
    Osc,
}

impl StatusSource {
    fn priority(self) -> u8 {
        match self {
            Self::Ipc => 0,
            Self::Osc => 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LiveAgentStatus {
    pub snapshot: AgentStatusSnapshot,
    pub source: StatusSource,
    pub last_seen: Instant,
}

#[derive(Default)]
pub struct AgentStatusStore {
    by_pane: HashMap<PaneId, LiveAgentStatus>,
}

impl AgentStatusStore {
    pub fn update(
        &mut self,
        pane_id: PaneId,
        snapshot: AgentStatusSnapshot,
        source: StatusSource,
        now: Instant,
    ) -> bool {
        if let Some(current) = self.by_pane.get(&pane_id) {
            let same_identity = current.snapshot.agent == snapshot.agent
                && current.snapshot.session_id == snapshot.session_id;
            if same_identity && snapshot.seq < current.snapshot.seq {
                return false;
            }
            if same_identity
                && snapshot.seq == current.snapshot.seq
                && source.priority() < current.source.priority()
            {
                return false;
            }
        }
        self.by_pane.insert(
            pane_id,
            LiveAgentStatus {
                snapshot,
                source,
                last_seen: now,
            },
        );
        true
    }

    pub fn get(&self, pane_id: PaneId, _now: Instant) -> Option<&LiveAgentStatus> {
        self.by_pane.get(&pane_id)
    }
    pub fn remove_identity(&mut self, agent: &str, session_id: &str) -> usize {
        let before = self.by_pane.len();
        self.by_pane.retain(|_, status| {
            status.snapshot.agent != agent || status.snapshot.session_id != session_id
        });
        before - self.by_pane.len()
    }

    pub fn remove_pane(&mut self, pane_id: PaneId) {
        self.by_pane.remove(&pane_id);
    }

    pub fn entries(&self, _now: Instant) -> impl Iterator<Item = (PaneId, &LiveAgentStatus)> {
        self.by_pane.iter().map(|(pane, status)| (*pane, status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimeterm_agent_status::AgentLifecycle;

    fn snapshot(seq: u64) -> AgentStatusSnapshot {
        let mut snapshot =
            AgentStatusSnapshot::new("omp", "s1", "C:\\work", AgentLifecycle::Thinking);
        snapshot.seq = seq;
        snapshot
    }

    #[test]
    fn rejects_older_sequence() {
        let now = Instant::now();
        let pane = PaneId(1);
        let mut store = AgentStatusStore::default();
        assert!(store.update(pane, snapshot(2), StatusSource::Osc, now));
        assert!(!store.update(pane, snapshot(1), StatusSource::Osc, now));
        assert_eq!(store.get(pane, now).unwrap().snapshot.seq, 2);
    }

    #[test]
    fn osc_wins_equal_sequence_over_ipc() {
        let now = Instant::now();
        let pane = PaneId(1);
        let mut store = AgentStatusStore::default();
        store.update(pane, snapshot(1), StatusSource::Osc, now);
        assert!(!store.update(pane, snapshot(1), StatusSource::Ipc, now));
    }
}
