pub mod actions;
pub mod agent_link;
pub mod characters;
pub mod engine;
pub mod evolution;
pub mod persistence;
pub mod sprites;
pub mod state;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::agent_link::{AgentAura, AgentPhase, EffectiveMeters};

    #[test]
    fn busy_aura_adds_one_temporary_happiness() {
        let now = Instant::now();
        let aura = AgentAura::for_phase(AgentPhase::Busy, 7, now);

        assert_eq!(
            aura.effective_meters(2, 50, now),
            EffectiveMeters {
                happiness: 3,
                discipline: 50
            }
        );
    }

    #[test]
    fn spawning_aura_adds_twenty_five_temporary_discipline() {
        let now = Instant::now();
        let aura = AgentAura::for_phase(AgentPhase::Spawning, 8, now);

        assert_eq!(
            aura.effective_meters(2, 50, now),
            EffectiveMeters {
                happiness: 2,
                discipline: 75
            }
        );
    }

    #[test]
    fn aura_expires_without_changing_base_meters() {
        let now = Instant::now();
        let aura = AgentAura::for_phase(AgentPhase::Busy, 9, now);

        assert_eq!(
            aura.effective_meters(2, 50, now + Duration::from_secs(601)),
            EffectiveMeters {
                happiness: 2,
                discipline: 50
            }
        );
    }

    #[test]
    fn repeated_phase_replaces_instead_of_stacking() {
        let now = Instant::now();
        let first = AgentAura::for_phase(AgentPhase::Busy, 10, now);
        let second = first.transition(AgentPhase::Busy, 11, now + Duration::from_secs(3));

        assert_eq!(
            second.effective_meters(3, 75, now),
            EffectiveMeters {
                happiness: 4,
                discipline: 75
            }
        );
    }
}
