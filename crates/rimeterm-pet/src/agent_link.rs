use std::time::{Duration, Instant};

const AURA_DURATION: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentPhase {
    Unbound,
    Starting,
    Busy,
    Spawning,
    Active,
    Idle,
    Waiting,
    Completed,
    Exited,
    Stale,
    MonitorStale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectiveMeters {
    pub happiness: u8,
    pub discipline: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentAura {
    happiness_bonus: u8,
    discipline_bonus: u8,
    expires_at: Instant,
    source_seq: u64,
}

impl AgentAura {
    pub fn for_phase(phase: AgentPhase, source_seq: u64, now: Instant) -> Self {
        let (happiness_bonus, discipline_bonus) = bonuses(phase);
        Self {
            happiness_bonus,
            discipline_bonus,
            expires_at: now + AURA_DURATION,
            source_seq,
        }
    }

    pub fn transition(&self, phase: AgentPhase, source_seq: u64, now: Instant) -> Self {
        Self::for_phase(phase, source_seq, now)
    }

    pub fn effective_meters(&self, happiness: u8, discipline: u8, now: Instant) -> EffectiveMeters {
        if now >= self.expires_at {
            return EffectiveMeters {
                happiness,
                discipline,
            };
        }
        EffectiveMeters {
            happiness: happiness.saturating_add(self.happiness_bonus).min(4),
            discipline: discipline.saturating_add(self.discipline_bonus).min(100),
        }
    }

    pub fn is_active(&self, now: Instant) -> bool {
        now < self.expires_at
    }

    pub fn happiness_bonus(&self, now: Instant) -> u8 {
        self.is_active(now)
            .then_some(self.happiness_bonus)
            .unwrap_or(0)
    }

    pub fn discipline_bonus(&self, now: Instant) -> u8 {
        self.is_active(now)
            .then_some(self.discipline_bonus)
            .unwrap_or(0)
    }

    pub fn source_seq(&self) -> u64 {
        self.source_seq
    }

    pub fn remaining(&self, now: Instant) -> Duration {
        self.expires_at.saturating_duration_since(now)
    }
}

fn bonuses(phase: AgentPhase) -> (u8, u8) {
    match phase {
        AgentPhase::Busy | AgentPhase::Active | AgentPhase::Completed => (1, 0),
        AgentPhase::Spawning => (0, 25),
        AgentPhase::Unbound
        | AgentPhase::Starting
        | AgentPhase::Idle
        | AgentPhase::Waiting
        | AgentPhase::Exited
        | AgentPhase::Stale
        | AgentPhase::MonitorStale => (0, 0),
    }
}
