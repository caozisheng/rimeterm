use std::any::Any;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::agent_monitor::{MainAgentPhase, SharedMainAgentSignal};
use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Widget},
};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use rimeterm_pet::{
    actions, engine,
    persistence::{PetStore, StoreMode},
    sprites,
};

const KEY_HINTS: &str = " f/m meal · s snack · d discipline · c clean · l light · i med · n hatch ";
const SIMULATION_INTERVAL: Duration = Duration::from_secs(60);
const ANIMATION_INTERVAL: Duration = Duration::from_millis(250);
const SPECTATOR_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

pub struct PetPane {
    id: PaneId,
    title: String,
    store: PetStore,
    main_agent: SharedMainAgentSignal,
    last_agent_seq: u64,
    aura: Option<rimeterm_pet::agent_link::AgentAura>,
    next_simulation: Instant,
    next_animation: Instant,
    next_spectator_refresh: Instant,
    animation_frame: u8,
    visible: bool,
    hint: Option<String>,
}

impl PetPane {
    pub fn try_new(
        state_path: PathBuf,
        lock_path: PathBuf,
        main_agent: SharedMainAgentSignal,
    ) -> Result<Self, rimeterm_pet::persistence::StoreError> {
        let now = Utc::now();
        let store = PetStore::open(&state_path, &lock_path, now)?;
        Ok(Self {
            id: PaneId::next(),
            title: "Pet".to_string(),
            store,
            main_agent,
            last_agent_seq: 0,
            aura: None,
            next_simulation: Instant::now() + SIMULATION_INTERVAL,
            next_spectator_refresh: Instant::now() + SPECTATOR_REFRESH_INTERVAL,
            next_animation: Instant::now() + ANIMATION_INTERVAL,
            animation_frame: 0,
            visible: false,
            hint: None,
        })
    }

    pub fn new(state_path: PathBuf, lock_path: PathBuf, main_agent: SharedMainAgentSignal) -> Self {
        Self::try_new(state_path, lock_path, main_agent.clone()).unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to open pet state; using temporary pet store");
            let now = Utc::now();
            let mut store = PetStore::open(
                &std::env::temp_dir().join("rimeterm-pet-state.json"),
                &std::env::temp_dir().join("rimeterm-pet.lock"),
                now,
            )
            .unwrap_or_else(|fallback| panic!("pet fallback store unavailable: {fallback}"));
            store.state_mut().last_tick = now;
            Self {
                id: PaneId::next(),
                title: "Pet".to_string(),
                store,
                main_agent,
                last_agent_seq: 0,
                aura: None,
                next_simulation: Instant::now() + SIMULATION_INTERVAL,
                next_spectator_refresh: Instant::now() + SPECTATOR_REFRESH_INTERVAL,
                next_animation: Instant::now() + ANIMATION_INTERVAL,
                animation_frame: 0,
                visible: false,
                hint: Some(format!("pet state unavailable: {error}")),
            }
        })
    }

    fn save(&mut self) {
        if let Err(error) = self.store.save() {
            self.hint = Some(format!("save failed: {error}"));
        }
    }

    fn ensure_owner(&mut self) -> bool {
        if self.store.mode() == StoreMode::Owner {
            true
        } else {
            self.hint = Some("read-only spectator · another RimeTerm owns this pet".to_string());
            false
        }
    }

    fn status_line(&self) -> String {
        let state = self.store.state();
        if !state.is_alive {
            return "DEAD · press n for a new egg".to_string();
        }
        if state.is_sick {
            return "SICK · press i for medicine".to_string();
        }
        if state.pending_lights_deadline.is_some() {
            return "BEDTIME · press l to turn lights off".to_string();
        }
        if state.is_sleeping {
            return "ZZZ · sleeping".to_string();
        }
        format!("OK · {}", self.agent_status())
    }

    fn agent_status(&self) -> String {
        let signal = self.main_agent.read();
        match &signal.phase {
            MainAgentPhase::Unbound => "no main agent".to_string(),
            MainAgentPhase::Starting => {
                format!("{} starting", signal.agent_id.as_deref().unwrap_or("agent"))
            }
            MainAgentPhase::Observed(status) => format!(
                "{} {}",
                signal.agent_id.as_deref().unwrap_or("agent"),
                status.label()
            ),
            MainAgentPhase::MonitorStale => "status unavailable".to_string(),
        }
    }

    fn agent_scene(&self) -> &'static str {
        match self.main_agent.read().phase {
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Busy) => "typing...",
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Spawning) => {
                "calling helpers..."
            }
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Active) => "working...",
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Waiting) => {
                "waiting for input"
            }
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Completed) => "done!",
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Idle) => "resting",
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Stale) => "away",
            MainAgentPhase::Starting => "starting...",
            MainAgentPhase::MonitorStale => "status unavailable",
            MainAgentPhase::Unbound => "alone",
        }
    }

    fn apply_agent_signal(&mut self, now: Instant) -> bool {
        let signal = self.main_agent.read().clone();
        if signal.transition_seq == self.last_agent_seq {
            return false;
        }
        self.last_agent_seq = signal.transition_seq;
        let phase = match signal.phase {
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Busy) => {
                Some(rimeterm_pet::agent_link::AgentPhase::Busy)
            }
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Spawning) => {
                Some(rimeterm_pet::agent_link::AgentPhase::Spawning)
            }
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Active) => {
                Some(rimeterm_pet::agent_link::AgentPhase::Active)
            }
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Completed) => {
                Some(rimeterm_pet::agent_link::AgentPhase::Completed)
            }
            _ => None,
        };
        self.aura = phase.map(|phase| {
            rimeterm_pet::agent_link::AgentAura::for_phase(phase, signal.transition_seq, now)
        });
        true
    }

    fn action(&mut self, result: Result<actions::ActionResult, actions::ActionError>) {
        match result {
            Ok(result) => {
                self.hint = Some(format!("{result:?}"));
                self.save();
            }
            Err(error) => self.hint = Some(error.to_string()),
        }
    }

    fn hatch(&mut self) {
        if self.store.mode() != StoreMode::Owner || self.store.state().is_alive {
            return;
        }
        *self.store.state_mut() = rimeterm_pet::state::PetState::new_egg(Utc::now());
        self.save();
    }
}

impl PaneProvider for PetPane {
    fn id(&self) -> PaneId {
        self.id
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn caps(&self) -> PaneCaps {
        PaneCaps::default()
    }
    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn render(
        &mut self,
        area: Rect,
        frame: &mut Frame<'_>,
        ctx: &PaneRenderCtx<'_>,
    ) -> RenderOutcome {
        let border_style = if ctx.focused {
            Style::default().fg(ctx.focus_color)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let state = self.store.state();
        let phase_label = self.agent_status();
        let scene = self.agent_scene();
        let title = format!(" pet · {:?} · {} ", state.character, phase_label);
        let block = Block::default()
            .title(title)
            .title_bottom(Line::styled(KEY_HINTS, border_style))
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        let agent_phase = match self.main_agent.read().phase {
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Busy) => {
                rimeterm_pet::agent_link::AgentPhase::Busy
            }
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Spawning) => {
                rimeterm_pet::agent_link::AgentPhase::Spawning
            }
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Active) => {
                rimeterm_pet::agent_link::AgentPhase::Active
            }
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Completed) => {
                rimeterm_pet::agent_link::AgentPhase::Completed
            }
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Waiting) => {
                rimeterm_pet::agent_link::AgentPhase::Waiting
            }
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Idle) => {
                rimeterm_pet::agent_link::AgentPhase::Idle
            }
            MainAgentPhase::Observed(crate::agtop_model::AgentStatus::Stale) => {
                rimeterm_pet::agent_link::AgentPhase::Stale
            }
            MainAgentPhase::Starting => rimeterm_pet::agent_link::AgentPhase::Starting,
            MainAgentPhase::MonitorStale => rimeterm_pet::agent_link::AgentPhase::MonitorStale,
            MainAgentPhase::Unbound => rimeterm_pet::agent_link::AgentPhase::Unbound,
        };
        if inner.width == 0 || inner.height == 0 {
            return RenderOutcome::default();
        }
        if inner.height < 6 || inner.width < 20 {
            frame.render_widget(
                Paragraph::new(format!(
                    "{:?} · {} · {}",
                    state.character,
                    scene,
                    self.status_line()
                )),
                inner,
            );
            return RenderOutcome::default();
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .split(inner);
        let sprite_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(4), Constraint::Length(1)])
            .split(chunks[0]);
        frame.render_widget(
            Paragraph::new(rimeterm_pet::sprites::agent_sprite(
                state,
                agent_phase,
                self.animation_frame,
            ))
            .alignment(ratatui::layout::Alignment::Center),
            sprite_rows[0],
        );
        frame.render_widget(
            Paragraph::new(scene)
                .alignment(ratatui::layout::Alignment::Center)
                .style(Style::default().fg(Color::Cyan)),
            sprite_rows[1],
        );
        frame.render_widget(
            Paragraph::new(format!("Hunger     {}", hearts(state.hunger, 4)))
                .style(Style::default().fg(Color::Yellow)),
            chunks[1],
        );
        let effective = self
            .aura
            .as_ref()
            .map(|aura| aura.effective_meters(state.happiness, state.discipline, Instant::now()))
            .unwrap_or(rimeterm_pet::agent_link::EffectiveMeters {
                happiness: state.happiness,
                discipline: state.discipline,
            });
        frame.render_widget(
            Paragraph::new(format!("Happiness  {}", hearts(effective.happiness, 4)))
                .style(Style::default().fg(Color::Magenta)),
            chunks[2],
        );
        frame.render_widget(
            Gauge::default()
                .ratio(effective.discipline as f64 / 100.0)
                .label(format!("Discipline {}%", effective.discipline))
                .gauge_style(Style::default().fg(Color::Cyan)),
            chunks[3],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "Age {} · Weight {} · Poop {}",
                state.age, state.weight, state.poop_count
            )),
            chunks[4],
        );
        let aura_hint = self.aura.as_ref().and_then(|aura| {
            let now = Instant::now();
            let remaining = aura.remaining(now).as_secs();
            (remaining > 0).then(|| {
                format!(
                    "aura +H{} +D{} {}s",
                    aura.happiness_bonus(now),
                    aura.discipline_bonus(now),
                    remaining
                )
            })
        });
        let footer = self
            .hint
            .clone()
            .or(aura_hint)
            .unwrap_or_else(|| self.status_line());
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                footer,
                Style::default().add_modifier(Modifier::DIM),
            )])),
            chunks[5],
        );
        self.hint = None;
        RenderOutcome::default()
    }
    fn on_key(&mut self, key: KeyEvent) -> bool {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        if !matches!(
            key.code,
            KeyCode::Char('f' | 'm' | 's' | 'd' | 'c' | 'l' | 'i' | 'n')
        ) {
            return false;
        }
        if !self.ensure_owner() {
            return true;
        }
        let now = Utc::now();
        let result = match key.code {
            KeyCode::Char('f') | KeyCode::Char('m') => {
                Some(actions::feed_meal(self.store.state_mut()))
            }
            KeyCode::Char('s') => Some(actions::feed_snack(self.store.state_mut())),
            KeyCode::Char('d') => Some(actions::discipline(self.store.state_mut())),
            KeyCode::Char('c') => Some(actions::clean_poop(self.store.state_mut())),
            KeyCode::Char('l') => Some(actions::toggle_lights(self.store.state_mut(), now)),
            KeyCode::Char('i') => Some(actions::give_medicine(self.store.state_mut())),
            KeyCode::Char('n') => {
                self.hatch();
                None
            }
            _ => None,
        };
        if let Some(result) = result {
            self.action(result);
        }
        true
    }

    fn poll_background(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = self.apply_agent_signal(now);
        if self.store.mode() == StoreMode::Spectator && now >= self.next_spectator_refresh {
            match self.store.reload(Utc::now()) {
                Ok(()) => changed = true,
                Err(error) => self.hint = Some(format!("reload failed: {error}")),
            }
            self.next_spectator_refresh = now + SPECTATOR_REFRESH_INTERVAL;
        }
        if self.store.mode() == StoreMode::Owner && now >= self.next_simulation {
            engine::tick(self.store.state_mut(), Utc::now());
            self.save();
            self.next_simulation = now + SIMULATION_INTERVAL;
            changed = true;
        }
        if self.visible && now >= self.next_animation {
            self.animation_frame = self.animation_frame.wrapping_add(1);
            self.next_animation = now + ANIMATION_INTERVAL;
            changed = true;
        }
        changed
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        if visible {
            self.next_animation = Instant::now() + ANIMATION_INTERVAL;
        }
    }

    fn reload(&mut self) {
        self.hint = Some("reload applies on restart".to_string());
    }
}

fn hearts(value: u8, max: u8) -> String {
    (0..max)
        .map(|index| if index < value { "██" } else { "░░" })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::PetPane;

    #[test]
    fn pet_pane_starts_with_a_live_egg() {
        let directory = tempdir().expect("create fixture directory");
        let signal = std::sync::Arc::new(parking_lot::RwLock::new(
            crate::agent_monitor::MainAgentSignal::default(),
        ));
        let pane = PetPane::try_new(
            directory.path().join("state.json"),
            directory.path().join("pet.lock"),
            signal,
        )
        .expect("create pet pane");

        assert!(pane.store.state().is_alive);
    }

    #[test]
    fn compact_pet_chrome_shows_operation_hints() {
        use ratatui::{Terminal, backend::TestBackend};
        use rimeterm_core::pane::{PaneProvider, PaneRenderCtx};
        let directory = tempdir().expect("create fixture directory");
        let signal = std::sync::Arc::new(parking_lot::RwLock::new(
            crate::agent_monitor::MainAgentSignal::default(),
        ));
        let mut pane = PetPane::try_new(
            directory.path().join("state.json"),
            directory.path().join("pet.lock"),
            signal,
        )
        .expect("create pet pane");
        let mut terminal = Terminal::new(TestBackend::new(50, 8)).expect("test terminal");
        terminal
            .draw(|frame| {
                pane.render(
                    frame.area(),
                    frame,
                    &PaneRenderCtx {
                        focused: false,
                        title_override: None,
                        focus_color: ratatui::style::Color::Cyan,
                    },
                );
            })
            .expect("render compact pet pane");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("f/m meal"), "{rendered}");
    }

    #[test]
    fn render_shows_operation_key_hints() {
        use ratatui::{Terminal, backend::TestBackend};
        use rimeterm_core::pane::{PaneProvider, PaneRenderCtx};

        let directory = tempdir().expect("create fixture directory");
        let signal = std::sync::Arc::new(parking_lot::RwLock::new(
            crate::agent_monitor::MainAgentSignal::default(),
        ));
        let mut pane = PetPane::try_new(
            directory.path().join("state.json"),
            directory.path().join("pet.lock"),
            signal,
        )
        .expect("create pet pane");
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("test terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                pane.render(
                    area,
                    frame,
                    &PaneRenderCtx {
                        focused: true,
                        title_override: None,
                        focus_color: ratatui::style::Color::Cyan,
                    },
                );
            })
            .expect("render pet pane");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(
            rendered.contains("f/m meal") && rendered.contains("c clean"),
            "{rendered}"
        );
    }

    #[test]
    fn busy_main_agent_renders_working_scene() {
        use crate::agtop_model::AgentStatus;
        use ratatui::{Terminal, backend::TestBackend, style::Color};
        use rimeterm_core::pane::{PaneProvider, PaneRenderCtx};

        let directory = tempdir().expect("create fixture directory");
        let signal = std::sync::Arc::new(parking_lot::RwLock::new(
            crate::agent_monitor::MainAgentSignal {
                pane_id: None,
                agent_id: Some("omp".to_string()),
                phase: crate::agent_monitor::MainAgentPhase::Observed(AgentStatus::Busy),
                transition_seq: 1,
            },
        ));
        let mut pane = PetPane::try_new(
            directory.path().join("state.json"),
            directory.path().join("pet.lock"),
            signal,
        )
        .expect("create pet pane");
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("test terminal");
        terminal
            .draw(|frame| {
                pane.render(
                    frame.area(),
                    frame,
                    &PaneRenderCtx {
                        focused: false,
                        title_override: None,
                        focus_color: Color::Cyan,
                    },
                );
            })
            .expect("render working pet");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("typing"), "{rendered}");
    }
}
