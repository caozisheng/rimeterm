use crate::AgentStatusSnapshot;
use crate::adapter::{AdapterState, AgentEvent, AgentStatusAdapter, apply_common};

pub struct OmpAdapter {
    state: AdapterState,
}

impl OmpAdapter {
    pub fn new(session_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            state: AdapterState::new("omp", &session_id.into(), &cwd.into()),
        }
    }
}

impl AgentStatusAdapter for OmpAdapter {
    fn apply(&mut self, event: AgentEvent) -> AgentStatusSnapshot {
        apply_common(&mut self.state, event);
        self.state.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_omp_tool_lifecycle() {
        let mut adapter = OmpAdapter::new("s1", "C:\\work");
        let snapshot = adapter.apply(AgentEvent::ToolStart {
            tool: "bash".into(),
            activity: Some("cargo test".into()),
        });
        assert_eq!(snapshot.state, crate::AgentLifecycle::ToolRunning);
        assert_eq!(snapshot.seq, 1);
        let snapshot = adapter.apply(AgentEvent::ToolEnd { error: false });
        assert_eq!(snapshot.state, crate::AgentLifecycle::Thinking);
    }
}
