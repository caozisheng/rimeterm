use crate::AgentStatusSnapshot;
use crate::adapter::{AdapterState, AgentEvent, AgentStatusAdapter, apply_common};

/// OpenCode: plugin hooks emit Claude-compatible lifecycle names
/// (SessionStart, UserPromptSubmit, PreToolUse via tool.execute.before,
/// PostToolUse via tool.execute.after, session.compacting, Stop), so the
/// event mapping matches [`crate::ClaudeCodeAdapter`].
pub struct OpenCodeAdapter {
    state: AdapterState,
}

impl OpenCodeAdapter {
    pub fn new(session_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            state: AdapterState::new("opencode", &session_id.into(), &cwd.into()),
        }
    }
}

impl AgentStatusAdapter for OpenCodeAdapter {
    fn apply(&mut self, event: AgentEvent) -> AgentStatusSnapshot {
        apply_common(&mut self.state, event);
        self.state.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_opencode_stop_to_success() {
        let mut adapter = OpenCodeAdapter::new("s1", "C:\\work");
        assert_eq!(
            adapter.apply(AgentEvent::AgentEnd { error: false }).state,
            crate::AgentLifecycle::Success
        );
    }
}
