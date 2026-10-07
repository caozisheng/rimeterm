//! Realtime Agent status projection for workspace-tab presentation.

/// The single status represented by one workspace tab.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkspaceActivity {
    #[default]
    Quiet,
    Working,
    NeedsInput,
    Completed,
    Failed,
}

impl From<rimeterm_agent_status::AgentLifecycle> for WorkspaceActivity {
    fn from(state: rimeterm_agent_status::AgentLifecycle) -> Self {
        use rimeterm_agent_status::AgentLifecycle;
        match state {
            AgentLifecycle::Idle => Self::Quiet,
            AgentLifecycle::Thinking | AgentLifecycle::ToolRunning | AgentLifecycle::Compacting => {
                Self::Working
            }
            AgentLifecycle::WaitingUser => Self::NeedsInput,
            AgentLifecycle::Success => Self::Completed,
            AgentLifecycle::Error | AgentLifecycle::Interrupted => Self::Failed,
        }
    }
}

impl WorkspaceActivity {
    /// Stable semantic marker rendered before a workspace title.
    pub const fn glyph(self) -> Option<&'static str> {
        match self {
            Self::Quiet => None,
            Self::Working => Some("●"),
            Self::NeedsInput => Some("?"),
            Self::Completed => Some("✓"),
            Self::Failed => Some("!"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimeterm_agent_status::AgentLifecycle;

    #[test]
    fn glyphs_are_stable_and_semantic() {
        assert_eq!(WorkspaceActivity::Quiet.glyph(), None);
        assert_eq!(WorkspaceActivity::Working.glyph(), Some("●"));
        assert_eq!(WorkspaceActivity::NeedsInput.glyph(), Some("?"));
        assert_eq!(WorkspaceActivity::Completed.glyph(), Some("✓"));
        assert_eq!(WorkspaceActivity::Failed.glyph(), Some("!"));
    }

    #[test]
    fn realtime_lifecycle_maps_to_workspace_activity() {
        assert_eq!(
            WorkspaceActivity::from(AgentLifecycle::Idle),
            WorkspaceActivity::Quiet
        );
        assert_eq!(
            WorkspaceActivity::from(AgentLifecycle::Thinking),
            WorkspaceActivity::Working
        );
        assert_eq!(
            WorkspaceActivity::from(AgentLifecycle::ToolRunning),
            WorkspaceActivity::Working
        );
        assert_eq!(
            WorkspaceActivity::from(AgentLifecycle::WaitingUser),
            WorkspaceActivity::NeedsInput
        );
        assert_eq!(
            WorkspaceActivity::from(AgentLifecycle::Success),
            WorkspaceActivity::Completed
        );
        assert_eq!(
            WorkspaceActivity::from(AgentLifecycle::Error),
            WorkspaceActivity::Failed
        );
    }
}
