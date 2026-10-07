use crate::{AgentStatusSnapshot, PROTOCOL_VERSION};

pub const MAX_PAYLOAD_BYTES: usize = 16 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StatusValidationError {
    #[error("agent status payload exceeds {MAX_PAYLOAD_BYTES} bytes")]
    TooLarge,
    #[error("unsupported agent status protocol version {0}")]
    UnsupportedVersion(u16),
    #[error("agent status type must be `agent_status`")]
    WrongType,
    #[error("agent status agent is empty")]
    EmptyAgent,
    #[error("unsupported realtime Agent `{0}`")]
    UnsupportedAgent(String),
    #[error("agent status session_id is empty")]
    EmptySession,
    #[error("agent status cwd is empty")]
    EmptyCwd,
}

pub fn validate_snapshot(snapshot: &AgentStatusSnapshot) -> Result<(), StatusValidationError> {
    if snapshot.version != PROTOCOL_VERSION {
        return Err(StatusValidationError::UnsupportedVersion(snapshot.version));
    }
    if snapshot.r#type != "agent_status" {
        return Err(StatusValidationError::WrongType);
    }
    if snapshot.agent.trim().is_empty() {
        return Err(StatusValidationError::EmptyAgent);
    }
    if !crate::supports_agent(snapshot.agent.as_str()) {
        return Err(StatusValidationError::UnsupportedAgent(
            snapshot.agent.clone(),
        ));
    }
    if snapshot.session_id.trim().is_empty() {
        return Err(StatusValidationError::EmptySession);
    }
    if snapshot.cwd.trim().is_empty() {
        return Err(StatusValidationError::EmptyCwd);
    }
    Ok(())
}

pub fn validate_json_payload(bytes: &[u8]) -> Result<AgentStatusSnapshot, StatusValidationError> {
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(StatusValidationError::TooLarge);
    }
    let snapshot: AgentStatusSnapshot =
        serde_json::from_slice(bytes).map_err(|_| StatusValidationError::WrongType)?;
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentLifecycle;

    fn valid() -> AgentStatusSnapshot {
        AgentStatusSnapshot::new("omp", "s1", "C:\\work", AgentLifecycle::Idle)
    }

    #[test]
    fn rejects_wrong_version() {
        let mut snapshot = valid();
        snapshot.version = 2;
        assert_eq!(
            validate_snapshot(&snapshot),
            Err(StatusValidationError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn rejects_empty_identity_fields() {
        let mut snapshot = valid();
        snapshot.session_id.clear();
        assert_eq!(
            validate_snapshot(&snapshot),
            Err(StatusValidationError::EmptySession)
        );
    }

    #[test]
    fn rejects_oversized_payload() {
        assert_eq!(
            validate_json_payload(&vec![b' '; MAX_PAYLOAD_BYTES + 1]),
            Err(StatusValidationError::TooLarge)
        );
    }
}
