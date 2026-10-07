use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLifecycle {
    Idle,
    Thinking,
    ToolRunning,
    WaitingUser,
    Success,
    Error,
    Interrupted,
    Compacting,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentStatusSnapshot {
    pub version: u16,
    pub r#type: String,
    pub agent: String,
    pub session_id: String,
    pub cwd: String,
    pub state: AgentLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub seq: u64,
}

impl AgentStatusSnapshot {
    pub fn new(
        agent: impl Into<String>,
        session_id: impl Into<String>,
        cwd: impl Into<String>,
        state: AgentLifecycle,
    ) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            r#type: "agent_status".into(),
            agent: agent.into(),
            session_id: session_id.into(),
            cwd: cwd.into(),
            state,
            tool: None,
            activity: None,
            message: None,
            seq: 0,
        }
    }
}

pub fn encode_osc(snapshot: &AgentStatusSnapshot) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string(snapshot)?;
    Ok(format!("\u{1b}]1337;rimeterm;{json}\u{7}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_standard_json() {
        let mut snapshot =
            AgentStatusSnapshot::new("omp", "s1", "C:\\work", AgentLifecycle::ToolRunning);
        snapshot.tool = Some("bash".into());
        snapshot.seq = 4;
        let json = serde_json::to_string(&snapshot).unwrap();
        let decoded: AgentStatusSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn osc_encoding_wraps_snapshot_json() {
        let snapshot = AgentStatusSnapshot::new("omp", "s1", "C:\\work", AgentLifecycle::Thinking);
        let encoded = encode_osc(&snapshot).unwrap();
        assert!(encoded.starts_with("\u{1b}]1337;rimeterm;{"));
        assert!(encoded.ends_with('\u{7}'));
    }
}
