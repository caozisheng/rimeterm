use crate::AgentStatusSnapshot;
use crate::adapter::{AdapterState, AgentEvent, AgentStatusAdapter, apply_common};

pub struct CodexAdapter {
    state: AdapterState,
}

impl CodexAdapter {
    pub fn new(session_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            state: AdapterState::new("codex", &session_id.into(), &cwd.into()),
        }
    }
}

impl AgentStatusAdapter for CodexAdapter {
    fn apply(&mut self, event: AgentEvent) -> AgentStatusSnapshot {
        apply_common(&mut self.state, event);
        self.state.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_interrupt_and_success() {
        let mut adapter = CodexAdapter::new("s1", "C:\\work");
        assert_eq!(
            adapter.apply(AgentEvent::Interrupted).state,
            crate::AgentLifecycle::Interrupted
        );
        assert_eq!(
            adapter.apply(AgentEvent::AgentEnd { error: false }).state,
            crate::AgentLifecycle::Success
        );
    }
}
