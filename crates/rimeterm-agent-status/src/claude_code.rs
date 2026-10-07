use crate::AgentStatusSnapshot;
use crate::adapter::{AdapterState, AgentEvent, AgentStatusAdapter, apply_common};

pub struct ClaudeCodeAdapter {
    state: AdapterState,
}

impl ClaudeCodeAdapter {
    pub fn new(session_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            state: AdapterState::new("claude", &session_id.into(), &cwd.into()),
        }
    }
}

impl AgentStatusAdapter for ClaudeCodeAdapter {
    fn apply(&mut self, event: AgentEvent) -> AgentStatusSnapshot {
        apply_common(&mut self.state, event);
        self.state.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_permission_wait_and_error() {
        let mut adapter = ClaudeCodeAdapter::new("s1", "C:\\work");
        assert_eq!(
            adapter.apply(AgentEvent::ToolApprovalRequested).state,
            crate::AgentLifecycle::WaitingUser
        );
        assert_eq!(
            adapter.apply(AgentEvent::ToolEnd { error: true }).state,
            crate::AgentLifecycle::Error
        );
    }
}
