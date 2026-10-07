//! Aggregate Agent activity for workspace-tab presentation.

use crate::agtop_model::AgentStatus;

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

    /// Higher values win when several agents belong to one workspace.
    const fn priority(self) -> u8 {
        match self {
            Self::Quiet => 0,
            Self::Completed => 1,
            Self::Working => 2,
            Self::NeedsInput => 3,
            Self::Failed => 4,
        }
    }

    pub const fn from_agent_status(status: AgentStatus) -> Self {
        match status {
            AgentStatus::Busy | AgentStatus::Spawning => Self::Working,
            AgentStatus::Active | AgentStatus::Idle | AgentStatus::Stale => Self::Quiet,
            AgentStatus::Waiting => Self::NeedsInput,
            AgentStatus::Completed => Self::Completed,
        }
    }

    pub fn fold(self, status: AgentStatus) -> Self {
        let next = Self::from_agent_status(status);
        if next.priority() > self.priority() {
            next
        } else {
            self
        }
    }

    pub const fn from_agent_signal(status: AgentStatus, has_session: bool) -> Self {
        if !has_session {
            return Self::Quiet;
        }
        Self::from_agent_status(status)
    }

    pub fn fold_agent_signal(self, status: AgentStatus, has_session: bool) -> Self {
        let next = Self::from_agent_signal(status, has_session);
        if next.priority() > self.priority() {
            next
        } else {
            self
        }
    }
    pub const fn from_agent_activity(
        status: AgentStatus,
        has_session: bool,
        has_active_work: bool,
    ) -> Self {
        if !has_session || (matches!(status, AgentStatus::Busy) && !has_active_work) {
            return Self::Quiet;
        }
        Self::from_agent_status(status)
    }

    pub fn fold_agent_activity(
        self,
        status: AgentStatus,
        has_session: bool,
        has_active_work: bool,
    ) -> Self {
        let next = Self::from_agent_activity(status, has_session, has_active_work);
        if next.priority() > self.priority() {
            next
        } else {
            self
        }
    }
}

const QUIET_SAMPLES_TO_CLEAR_WORKING: u8 = 2;

/// Apply one sampled aggregate while suppressing a one-sample Working→Quiet
/// transition caused by the monitor's CPU/session classification boundary.
pub(crate) fn stabilize(
    previous: WorkspaceActivity,
    observed: WorkspaceActivity,
    quiet_samples: u8,
) -> (WorkspaceActivity, u8) {
    if previous == WorkspaceActivity::Working && observed == WorkspaceActivity::Quiet {
        let quiet_samples = quiet_samples.saturating_add(1);
        if quiet_samples >= QUIET_SAMPLES_TO_CLEAR_WORKING {
            (WorkspaceActivity::Quiet, 0)
        } else {
            (WorkspaceActivity::Working, quiet_samples)
        }
    } else {
        (observed, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_stable_and_semantic() {
        assert_eq!(WorkspaceActivity::Quiet.glyph(), None);
        assert_eq!(WorkspaceActivity::Working.glyph(), Some("●"));
        assert_eq!(WorkspaceActivity::NeedsInput.glyph(), Some("?"));
        assert_eq!(WorkspaceActivity::Completed.glyph(), Some("✓"));
        assert_eq!(WorkspaceActivity::Failed.glyph(), Some("!"));
    }

    #[test]
    fn higher_attention_state_wins() {
        let state = WorkspaceActivity::default()
            .fold(AgentStatus::Completed)
            .fold(AgentStatus::Active)
            .fold(AgentStatus::Waiting);
        assert_eq!(state, WorkspaceActivity::NeedsInput);
        assert_eq!(
            state.fold(AgentStatus::Completed),
            WorkspaceActivity::NeedsInput
        );
    }
    #[test]
    fn idle_agent_is_quiet_by_default() {
        assert_eq!(
            WorkspaceActivity::default().fold(AgentStatus::Idle),
            WorkspaceActivity::Quiet
        );
    }
    #[test]
    fn quieting_requires_two_consecutive_quiet_samples_after_working() {
        let first = stabilize(WorkspaceActivity::Working, WorkspaceActivity::Quiet, 0);
        assert_eq!(first, (WorkspaceActivity::Working, 1));
        let second = stabilize(first.0, WorkspaceActivity::Quiet, first.1);
        assert_eq!(second, (WorkspaceActivity::Quiet, 0));
    }

    #[test]
    fn quiet_workspace_stays_quiet_for_idle_samples() {
        assert_eq!(
            stabilize(WorkspaceActivity::Quiet, WorkspaceActivity::Quiet, 0),
            (WorkspaceActivity::Quiet, 0)
        );
    }
    #[test]
    fn cpu_only_agent_does_not_mark_workspace_working() {
        assert_eq!(
            WorkspaceActivity::Quiet.fold_agent_signal(AgentStatus::Active, false),
            WorkspaceActivity::Quiet
        );
    }
    #[test]
    fn recently_active_session_does_not_mark_workspace_working() {
        assert_eq!(
            WorkspaceActivity::Quiet.fold_agent_signal(AgentStatus::Active, true),
            WorkspaceActivity::Quiet
        );
    }
    #[test]
    fn busy_without_active_tool_does_not_mark_workspace_working() {
        assert_eq!(
            WorkspaceActivity::Quiet.fold_agent_activity(AgentStatus::Busy, true, false),
            WorkspaceActivity::Quiet
        );
    }
}
