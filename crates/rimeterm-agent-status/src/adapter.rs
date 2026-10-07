use crate::{AgentLifecycle, AgentStatusSnapshot};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentEvent {
    AgentStart,
    TurnStart,
    ToolApprovalRequested,
    ToolApprovalResolved {
        approved: bool,
    },
    ToolStart {
        tool: String,
        activity: Option<String>,
    },
    ToolEnd {
        error: bool,
    },
    CompactionStart,
    CompactionEnd,
    AgentEnd {
        error: bool,
    },
    Interrupted,
    SessionStop,
}

impl AgentEvent {
    pub fn from_name(
        name: &str,
        tool: Option<String>,
        activity: Option<String>,
        error: bool,
        approved: bool,
    ) -> Option<Self> {
        Some(match name {
            "agent_start" | "turn_start" => Self::TurnStart,
            "tool_approval_requested" => Self::ToolApprovalRequested,
            "tool_approval_resolved" => Self::ToolApprovalResolved { approved },
            "tool_start" | "tool_execution_start" => Self::ToolStart {
                tool: tool?,
                activity,
            },
            "tool_end" | "tool_execution_end" => Self::ToolEnd { error },
            "compaction_start" | "session.compacting" => Self::CompactionStart,
            "compaction_end" | "session_compact" => Self::CompactionEnd,
            "agent_end" => Self::AgentEnd { error },
            "interrupted" => Self::Interrupted,
            "session_stop" => Self::SessionStop,
            _ => return None,
        })
    }
}
pub trait AgentStatusAdapter {
    fn apply(&mut self, event: AgentEvent) -> AgentStatusSnapshot;
}

#[derive(Clone, Debug)]
pub(crate) struct AdapterState {
    pub agent: String,
    pub session_id: String,
    pub cwd: String,
    pub state: AgentLifecycle,
    pub tool: Option<String>,
    pub activity: Option<String>,
    pub message: Option<String>,
    pub seq: u64,
}

impl AdapterState {
    pub(crate) fn new(agent: &str, session_id: &str, cwd: &str) -> Self {
        Self {
            agent: agent.into(),
            session_id: session_id.into(),
            cwd: cwd.into(),
            state: AgentLifecycle::Idle,
            tool: None,
            activity: None,
            message: None,
            seq: 0,
        }
    }

    pub(crate) fn snapshot(&mut self) -> AgentStatusSnapshot {
        self.seq = self.seq.saturating_add(1);
        let mut snapshot = AgentStatusSnapshot::new(
            self.agent.clone(),
            self.session_id.clone(),
            self.cwd.clone(),
            self.state,
        );
        snapshot.tool = self.tool.clone();
        snapshot.activity = self.activity.clone();
        snapshot.message = self.message.clone();
        snapshot.seq = self.seq;
        snapshot
    }
}

pub(crate) fn apply_common(state: &mut AdapterState, event: AgentEvent) {
    match event {
        AgentEvent::AgentStart | AgentEvent::TurnStart => {
            state.state = AgentLifecycle::Thinking;
            state.tool = None;
            state.activity = None;
            state.message = None;
        }
        AgentEvent::ToolApprovalRequested => state.state = AgentLifecycle::WaitingUser,
        AgentEvent::ToolApprovalResolved { approved } => {
            state.state = if approved {
                AgentLifecycle::ToolRunning
            } else {
                AgentLifecycle::Thinking
            };
        }
        AgentEvent::ToolStart { tool, activity } => {
            state.state = AgentLifecycle::ToolRunning;
            state.tool = Some(tool);
            state.activity = activity;
        }
        AgentEvent::ToolEnd { error } => {
            state.state = if error {
                AgentLifecycle::Error
            } else {
                AgentLifecycle::Thinking
            };
            state.tool = None;
            state.activity = None;
        }
        AgentEvent::CompactionStart => state.state = AgentLifecycle::Compacting,
        AgentEvent::CompactionEnd => state.state = AgentLifecycle::Thinking,
        AgentEvent::AgentEnd { error } => {
            state.state = if error {
                AgentLifecycle::Error
            } else {
                AgentLifecycle::Success
            };
            state.tool = None;
            state.activity = None;
        }
        AgentEvent::Interrupted => state.state = AgentLifecycle::Interrupted,
        AgentEvent::SessionStop => {
            state.state = AgentLifecycle::Idle;
            state.tool = None;
            state.activity = None;
        }
    }
}
