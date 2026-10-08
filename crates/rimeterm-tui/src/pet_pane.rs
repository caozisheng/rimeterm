use std::any::Any;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::agent_monitor::SharedMainAgentSignal;
use crate::agent_status_store::SharedAgentStatusStore;
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
    agent_statuses: SharedAgentStatusStore,
    last_agent_seq: u64,
    last_agent_identity: Option<(String, String)>,
    aura: Option<rimeterm_pet::agent_link::AgentAura>,
    next_simulation: Instant,
    next_spectator_refresh: Instant,
    next_animation: Instant,
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
        Self::try_new_with_status(
            state_path,
            lock_path,
            main_agent,
            std::sync::Arc::new(parking_lot::RwLock::new(
                crate::agent_status_store::AgentStatusStore::default(),
            )),
        )
    }

    pub fn try_new_with_status(
        state_path: PathBuf,
        lock_path: PathBuf,
        main_agent: SharedMainAgentSignal,
        agent_statuses: SharedAgentStatusStore,
    ) -> Result<Self, rimeterm_pet::persistence::StoreError> {
        let now = Utc::now();
        let store = PetStore::open(&state_path, &lock_path, now)?;
        Ok(Self {
            id: PaneId::next(),
            title: "Pet".to_string(),
            store,
            main_agent,
            agent_statuses,
            last_agent_identity: None,
            last_agent_seq: 0,
            next_simulation: Instant::now() + SIMULATION_INTERVAL,
            next_spectator_refresh: Instant::now() + SPECTATOR_REFRESH_INTERVAL,
            next_animation: Instant::now() + ANIMATION_INTERVAL,
            animation_frame: 0,
            visible: false,
            hint: None,
            aura: None,
        })
    }

    pub fn new(
        state_path: PathBuf,
        lock_path: PathBuf,
        main_agent: SharedMainAgentSignal,
        agent_statuses: SharedAgentStatusStore,
    ) -> Self {
        Self::try_new_with_status(
            state_path,
            lock_path,
            main_agent.clone(),
            agent_statuses.clone(),
        )
        .unwrap_or_else(|error| {
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
                agent_statuses,
                last_agent_identity: None,
                last_agent_seq: 0,
                next_simulation: Instant::now() + SIMULATION_INTERVAL,
                next_spectator_refresh: Instant::now() + SPECTATOR_REFRESH_INTERVAL,
                next_animation: Instant::now() + ANIMATION_INTERVAL,
                animation_frame: 0,
                visible: false,
                hint: Some(format!("pet state unavailable: {error}")),
                aura: None,
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

    fn current_snapshot(&self) -> Option<rimeterm_agent_status::AgentStatusSnapshot> {
        let pane_id = self.main_agent.read().pane_id?;
        self.agent_statuses
            .read()
            .snapshot_for(pane_id, Instant::now())
    }

    fn agent_status(&self) -> String {
        self.current_snapshot()
            .map(|snapshot| format!("{} {}", snapshot.agent, status_label(snapshot.state)))
            .unwrap_or_else(|| "status unavailable".to_string())
    }

    fn agent_scene(&self) -> String {
        status_scene(self.current_snapshot().as_ref())
    }

    fn apply_agent_signal(&mut self, now: Instant) -> bool {
        let snapshot = self.current_snapshot();
        let identity = snapshot
            .as_ref()
            .map(|snapshot| (snapshot.agent.clone(), snapshot.session_id.clone()));
        let seq = snapshot.as_ref().map_or(0, |snapshot| snapshot.seq);
        if identity == self.last_agent_identity && seq == self.last_agent_seq {
            return false;
        }
        self.last_agent_identity = identity;
        self.last_agent_seq = seq;
        self.aura = snapshot.as_ref().and_then(|snapshot| {
            let phase = pet_phase_for_status(snapshot);
            matches!(
                phase,
                rimeterm_pet::agent_link::AgentPhase::Busy
                    | rimeterm_pet::agent_link::AgentPhase::Spawning
                    | rimeterm_pet::agent_link::AgentPhase::Active
                    | rimeterm_pet::agent_link::AgentPhase::Completed
            )
            .then(|| rimeterm_pet::agent_link::AgentAura::for_phase(phase, snapshot.seq, now))
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
        let agent_phase = self
            .current_snapshot()
            .as_ref()
            .map(pet_phase_for_status)
            .unwrap_or(rimeterm_pet::agent_link::AgentPhase::MonitorStale);
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

    fn is_visible(&self) -> bool {
        self.visible
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
fn pet_phase_for_status(
    snapshot: &rimeterm_agent_status::AgentStatusSnapshot,
) -> rimeterm_pet::agent_link::AgentPhase {
    use rimeterm_agent_status::AgentLifecycle;
    use rimeterm_pet::agent_link::AgentPhase;

    match snapshot.state {
        AgentLifecycle::Idle => AgentPhase::Idle,
        AgentLifecycle::Thinking | AgentLifecycle::ToolRunning | AgentLifecycle::Compacting => {
            AgentPhase::Busy
        }
        AgentLifecycle::WaitingUser => AgentPhase::Waiting,
        AgentLifecycle::Success => AgentPhase::Completed,
        AgentLifecycle::Error | AgentLifecycle::Interrupted => AgentPhase::Exited,
    }
}

fn status_label(state: rimeterm_agent_status::AgentLifecycle) -> &'static str {
    use rimeterm_agent_status::AgentLifecycle;
    match state {
        AgentLifecycle::Idle => "IDLE",
        AgentLifecycle::Thinking => "THINKING",
        AgentLifecycle::ToolRunning => "WORKING",
        AgentLifecycle::WaitingUser => "WAITING",
        AgentLifecycle::Success => "SUCCESS",
        AgentLifecycle::Error => "ERROR",
        AgentLifecycle::Interrupted => "INTERRUPTED",
        AgentLifecycle::Compacting => "COMPACTING",
    }
}

fn status_scene_for_snapshot(snapshot: &rimeterm_agent_status::AgentStatusSnapshot) -> String {
    let scene = match snapshot.state {
        rimeterm_agent_status::AgentLifecycle::Idle => "resting",
        rimeterm_agent_status::AgentLifecycle::Thinking => "thinking",
        rimeterm_agent_status::AgentLifecycle::ToolRunning => "working",
        rimeterm_agent_status::AgentLifecycle::WaitingUser => "waiting",
        rimeterm_agent_status::AgentLifecycle::Success => "done",
        rimeterm_agent_status::AgentLifecycle::Error => "error",
        rimeterm_agent_status::AgentLifecycle::Interrupted => "interrupted",
        rimeterm_agent_status::AgentLifecycle::Compacting => "compacting",
    };
    match (snapshot.tool.as_deref(), snapshot.activity.as_deref()) {
        (Some(tool), Some(activity)) => format!("{tool} · {activity}"),
        (Some(tool), None) => tool.to_string(),
        (None, Some(activity)) => format!("{scene} · {activity}"),
        (None, None) => scene.to_string(),
    }
}

fn status_scene(snapshot: Option<&rimeterm_agent_status::AgentStatusSnapshot>) -> String {
    snapshot
        .map(status_scene_for_snapshot)
        .unwrap_or_else(|| "status unavailable".to_string())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;
    use tempfile::tempdir;

    use super::{PetPane, pet_phase_for_status, status_scene, status_scene_for_snapshot};

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
    fn protocol_status_renders_working_scene_and_agent_title() {
        use crate::agent_status_store::{AgentStatusStore, StatusSource};
        use ratatui::{Terminal, backend::TestBackend, style::Color};
        use rimeterm_agent_status::{AgentLifecycle, AgentStatusSnapshot};
        use rimeterm_core::pane::{PaneProvider, PaneRenderCtx};

        let directory = tempdir().expect("create fixture directory");
        let signal = std::sync::Arc::new(parking_lot::RwLock::new(
            crate::agent_monitor::MainAgentSignal::default(),
        ));
        let statuses = std::sync::Arc::new(parking_lot::RwLock::new(AgentStatusStore::default()));
        let mut pane = PetPane::try_new_with_status(
            directory.path().join("state.json"),
            directory.path().join("pet.lock"),
            signal.clone(),
            statuses.clone(),
        )
        .expect("create pet pane");
        let pane_id = pane.id();
        signal.write().pane_id = Some(pane_id);
        let mut snapshot =
            AgentStatusSnapshot::new("omp", "session", "C:\\work", AgentLifecycle::ToolRunning);
        snapshot.tool = Some("bash".into());
        snapshot.activity = Some("Checking Tidy availability".into());
        statuses
            .write()
            .update(pane_id, snapshot, StatusSource::Ipc, Instant::now());

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

        assert!(rendered.contains("omp WORKING"), "{rendered}");
        assert!(
            rendered.contains("bash · Checking Tidy availability"),
            "{rendered}"
        );
    }
    #[test]
    fn agent_status_snapshot_maps_to_pet_phase_and_scene() {
        use rimeterm_agent_status::{AgentLifecycle, AgentStatusSnapshot};

        let mut snapshot =
            AgentStatusSnapshot::new("omp", "session", "C:\\work", AgentLifecycle::ToolRunning);
        snapshot.tool = Some("bash".into());
        snapshot.activity = Some("cargo test".into());

        assert_eq!(
            pet_phase_for_status(&snapshot),
            rimeterm_pet::agent_link::AgentPhase::Busy
        );
        assert_eq!(status_scene_for_snapshot(&snapshot), "bash · cargo test");
    }

    #[test]
    fn successful_status_maps_to_completed_pet_phase() {
        use rimeterm_agent_status::{AgentLifecycle, AgentStatusSnapshot};

        let snapshot =
            AgentStatusSnapshot::new("claude", "session", "C:\\work", AgentLifecycle::Success);

        assert_eq!(
            pet_phase_for_status(&snapshot),
            rimeterm_pet::agent_link::AgentPhase::Completed
        );
        assert_eq!(status_scene_for_snapshot(&snapshot), "done");
    }

    #[test]
    fn missing_status_renders_unavailable_scene() {
        assert_eq!(status_scene(None), "status unavailable");
    }
}
