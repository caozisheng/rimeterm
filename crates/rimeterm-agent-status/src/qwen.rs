use crate::AgentStatusSnapshot;
use crate::adapter::{AdapterState, AgentEvent, AgentStatusAdapter, apply_common};

/// Qwen Code: Claude-compatible hook vocabulary (settings.json hooks with
/// PreToolUse / PostToolUse / SessionStart / Stop / ...), so event mapping
/// matches [`crate::ClaudeCodeAdapter`].
pub struct QwenAdapter {
    state: AdapterState,
}

impl QwenAdapter {
    pub fn new(session_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            state: AdapterState::new("qwen", &session_id.into(), &cwd.into()),
        }
    }
}

impl AgentStatusAdapter for QwenAdapter {
    fn apply(&mut self, event: AgentEvent) -> AgentStatusSnapshot {
        apply_common(&mut self.state, event);
        self.state.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_qwen_tool_lifecycle() {
        let mut adapter = QwenAdapter::new("s1", "C:\\work");
        assert_eq!(
            adapter
                .apply(AgentEvent::ToolStart {
                    tool: "shell".into(),
                    activity: Some("cargo test".into())
                })
                .state,
            crate::AgentLifecycle::ToolRunning
        );
        assert_eq!(
            adapter.apply(AgentEvent::ToolEnd { error: false }).state,
            crate::AgentLifecycle::Thinking
        );
    }
}
