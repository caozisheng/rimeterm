//! Generation-safe async embedded application.
//!
//! [`EmbeddedApp`] wraps the full [`App`] with generational ownership so
//! that a host can swap workspace roots, abort stale background work, and
//! serialise/restore state without data races.
//!
//! Unlike the simplified `EmbeddedApp` in `lib.rs`, this version:
//! - Uses `tokio::sync::mpsc` for async completion delivery.
//! - Holds a `tokio::runtime::Handle` to spawn background tasks.
//! - Delegates input to `controller::{handle_key, handle_mouse}`.
//! - Renders through `ui::render` with an arbitrary `Rect`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Frame, layout::Rect};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::app::{App, Tab};
use crate::controller::{self, ControllerOutcome, HostAction};
use crate::event::Event;
use crate::ui;
use crate::{GlabError, install_guide};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// How aggressively the embedded view may cache data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CachePolicy {
    /// Always fetch fresh data on activation.
    AlwaysFresh,
    /// Reuse data younger than `max_age` seconds.
    Ttl { max_age_secs: u64 },
    /// Never automatically refresh; the host controls timing.
    Manual,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self::Ttl { max_age_secs: 300 }
    }
}

/// Feature gates the host can toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddedFeatures {
    /// Allow mutation commands (merge, close, approve, …).
    pub mutations: bool,
    /// Allow launching an external editor for text fields.
    pub external_editor: bool,
    /// Allow opening URLs in the system browser.
    pub open_browser: bool,
    /// Show the repository-switcher overlay.
    pub repo_switcher: bool,
    /// Persist upstream config changes to disk.
    pub save_upstream_config: bool,
}

impl Default for EmbeddedFeatures {
    fn default() -> Self {
        Self {
            mutations: true,
            external_editor: true,
            open_browser: true,
            repo_switcher: false,
            save_upstream_config: false,
        }
    }
}

/// Options for constructing an [`EmbeddedApp`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddedOptions {
    /// Root of the git working tree to inspect.
    pub workspace_root: PathBuf,
    /// Tab to activate on start-up.
    pub initial_tab: Option<Tab>,
    /// Caching strategy.
    #[serde(default)]
    pub cache_policy: CachePolicy,
    /// Auto-refresh interval.  `None` disables periodic refresh.
    pub refresh: Option<Duration>,
    /// Feature gates.
    #[serde(default)]
    pub features: EmbeddedFeatures,
}

// ---------------------------------------------------------------------------
// Outcome / state types
// ---------------------------------------------------------------------------

/// Result of a completed embedded run or a single interaction cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedOutcome {
    /// Nothing changed; the host may skip a redraw.
    Unchanged,
    /// App state mutated; the host should redraw.
    Changed,
    /// The user explicitly requested to leave this pane.
    ExitRequested,
    /// The controller needs the host to perform a side-effect.
    HostAction(HostAction),
}

/// Describes why the workspace cannot proceed (CLI missing, auth, etc.).
#[derive(Debug, Clone)]
pub struct SetupProblem {
    /// The underlying error that prevented loading.
    pub error: GlabError,
    /// Human-readable install / auth guide text.
    pub guide: &'static str,
}

/// Bootstrap state machine for the embedded pane.
///
/// The host constructs the pane in [`Detecting`](AppShell::Detecting), and the
/// first successful or failing poll transitions to one of the other states.
pub enum AppShell {
    /// Waiting for the first backend probe to complete.
    Detecting,
    /// CLI or authentication is missing; show the install guide.
    Setup(SetupProblem),
    /// Fully operational — normal rendering and interaction.
    Ready(App),
    /// Previously loaded data is available but the last refresh failed.
    Offline(App, String),
}

/// Serialisable state snapshot that the host can persist and later restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddedState {
    /// The workspace root active when this snapshot was taken.
    pub workspace_root: PathBuf,
    /// The tab that was active.
    pub active_tab: Tab,
    /// Generation at snapshot time; used to detect staleness on restore.
    pub generation: u64,
}

/// Outcome the host feeds back after completing a [`HostAction`].
#[derive(Debug, Clone)]
pub enum HostActionResult {
    /// An `EditText` action completed; the edited content is returned.
    EditedText(String),
    /// An `OpenUrl` action completed (success or failure is informational).
    OpenUrlCompleted,
    /// A `CopyText` action completed.
    CopyCompleted,
    /// The host cancelled the action.
    Cancelled,
}

// ---------------------------------------------------------------------------
// Tagged completion
// ---------------------------------------------------------------------------

/// Every background completion carries the generation it was spawned under.
/// Completions whose generation doesn't match the current `EmbeddedApp`
/// generation are silently discarded in [`EmbeddedApp::poll_background`].
#[derive(Debug)]
pub struct TaggedCompletion {
    pub generation: u64,
    pub event: Event,
}

// ---------------------------------------------------------------------------
// EmbeddedApp
// ---------------------------------------------------------------------------

/// Generation-safe async wrapper around the full [`App`].
///
/// The host constructs it once, forwards key/mouse events, polls for
/// background work, and calls `render` each frame.  Swapping workspace
/// roots bumps the generation counter, causing in-flight completions to be
/// silently dropped.
pub struct EmbeddedApp {
    shell: AppShell,
    generation: u64,
    rx: mpsc::UnboundedReceiver<TaggedCompletion>,
    tx: mpsc::UnboundedSender<TaggedCompletion>,
    handle: tokio::runtime::Handle,
    event_tx: tokio::sync::mpsc::UnboundedSender<Event>,
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<Event>>,
    visible: bool,
    next_refresh: Option<Instant>,
    refresh_interval: Option<Duration>,
    last_refresh: Instant,
    options: EmbeddedOptions,
}

impl EmbeddedApp {
    /// Create a new embedded app.
    ///
    /// Returns immediately in [`AppShell::Detecting`] state.  The initial
    /// backend probe is spawned on the supplied `handle`.
    pub fn new(options: EmbeddedOptions, handle: tokio::runtime::Handle) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();

        let now = Instant::now();
        let next_refresh = options.refresh.map(|d| now + d);

        let mut this = Self {
            shell: AppShell::Detecting,
            generation: 0,
            rx,
            tx,
            handle,
            event_tx,
            event_rx: Some(event_rx),
            visible: true,
            next_refresh,
            refresh_interval: options.refresh,
            last_refresh: now,
            options,
        };
        this.start_load();
        this
    }

    // -- Input handling -----------------------------------------------------

    /// Forward a key event.  Returns the outcome the host should act on.
    /// In non-ready states the key is ignored.
    pub fn handle_key(&mut self, key: KeyEvent) -> EmbeddedOutcome {
        let app = match &mut self.shell {
            AppShell::Ready(app) | AppShell::Offline(app, _) => app,
            _ => return EmbeddedOutcome::Unchanged,
        };
        let outcome =
            controller::handle_key(app, key, self.event_tx.clone(), &mut self.last_refresh);
        Self::translate(outcome)
    }

    /// Forward a mouse event.  `area` is the rect this view occupies.
    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> EmbeddedOutcome {
        let app = match &mut self.shell {
            AppShell::Ready(app) | AppShell::Offline(app, _) => app,
            _ => return EmbeddedOutcome::Unchanged,
        };
        let outcome = controller::handle_mouse(app, mouse, area);
        Self::translate(outcome)
    }

    // -- Background work ----------------------------------------------------

    /// Drain the completion channel and apply results whose generation
    /// matches.  Returns `true` when state changed (host should redraw).
    pub fn poll_background(&mut self) -> bool {
        let mut changed = false;

        // Drain tagged completions.
        while let Ok(tc) = self.rx.try_recv() {
            if tc.generation != self.generation {
                continue;
            }
            self.apply_event(tc.event);
            changed = true;
        }

        // Check timed refresh.
        if self.visible {
            if let Some(deadline) = self.next_refresh {
                if Instant::now() >= deadline {
                    self.start_load();
                    changed = true;
                }
            }
        }

        changed
    }

    // -- Rendering ----------------------------------------------------------

    /// Render into the provided frame and rect.
    ///
    /// - `Detecting` → shows a brief "loading…" placeholder.
    /// - `Setup` → shows the install / auth guide from [`install_guide`].
    /// - `Ready` → delegates to [`ui::draw_in`].
    /// - `Offline` → delegates to [`ui::draw_in`] with an error banner set.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        match &mut self.shell {
            AppShell::Detecting => {
                let block = ratatui::widgets::Block::bordered().title(" glab ");
                let inner = block.inner(area);
                frame.render_widget(block, area);
                let text = ratatui::widgets::Paragraph::new("Detecting workspace…");
                frame.render_widget(text, inner);
            }
            AppShell::Setup(problem) => {
                let block = ratatui::widgets::Block::bordered().title(" Setup required ");
                let inner = block.inner(area);
                frame.render_widget(block, area);
                let text = ratatui::widgets::Paragraph::new(problem.guide)
                    .wrap(ratatui::widgets::Wrap { trim: false });
                frame.render_widget(text, inner);
            }
            AppShell::Ready(app) => {
                ui::draw_in(frame, area, app);
            }
            AppShell::Offline(app, err) => {
                app.error_message = Some(err.clone());
                ui::draw_in(frame, area, app);
            }
        }
    }

    // -- Deadline / visibility ----------------------------------------------

    /// Returns the next instant at which `poll_background` should be called,
    /// or `None` if no timed work is pending.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.next_refresh
    }

    /// Tell the embedded app whether it is currently visible.  When hidden,
    /// timed refreshes are suppressed.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        if visible && self.next_refresh.is_none() {
            if let Some(interval) = self.refresh_interval {
                self.next_refresh = Some(Instant::now() + interval);
            }
        }
    }

    // -- Workspace management -----------------------------------------------

    /// Switch to a different workspace root.  Bumps the generation counter
    /// so all in-flight completions for the old root are discarded.
    pub fn set_workspace_root(&mut self, root: &Path) {
        self.options.workspace_root = root.to_path_buf();
        self.generation = self.generation.wrapping_add(1);
        self.shell = AppShell::Detecting;
        self.start_load();
    }

    /// Manually trigger a reload of the current workspace data.
    pub fn reload(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.start_load();
    }

    // -- Snapshot / restore -------------------------------------------------

    /// Take a serialisable snapshot of the current state.
    pub fn snapshot(&self) -> EmbeddedState {
        let active_tab = match &self.shell {
            AppShell::Ready(app) | AppShell::Offline(app, _) => app.active_tab,
            _ => Tab::Issues,
        };
        EmbeddedState {
            workspace_root: self.options.workspace_root.clone(),
            active_tab,
            generation: self.generation,
        }
    }

    /// Restore from a previously taken snapshot.  If the workspace root
    /// differs, a fresh load is triggered.
    pub fn restore(&mut self, state: EmbeddedState) {
        let root_changed = self.options.workspace_root != state.workspace_root;
        self.options.workspace_root = state.workspace_root;
        if let AppShell::Ready(app) | AppShell::Offline(app, _) = &mut self.shell {
            app.active_tab = state.active_tab;
        }
        if root_changed {
            self.generation = self.generation.wrapping_add(1);
            self.shell = AppShell::Detecting;
            self.start_load();
        }
    }

    /// Feed the result of a completed [`HostAction`] back into the app.
    pub fn complete_host_action(&mut self, result: HostActionResult) {
        match result {
            HostActionResult::EditedText(text) => {
                let generation_id = self.generation;
                let tx = self.tx.clone();
                if let Some(app) = self.app_mut() {
                    let tab = app.active_tab;
                    let _ = tx.send(TaggedCompletion {
                        generation: generation_id,
                        event: Event::CommandCompleted(
                            tab,
                            if text.is_empty() {
                                Err("empty edit".into())
                            } else {
                                Ok(())
                            },
                        ),
                    });
                }
            }
            HostActionResult::Cancelled
            | HostActionResult::OpenUrlCompleted
            | HostActionResult::CopyCompleted => {}
        }
    }

    /// Gracefully shut down, dropping background task channels.
    pub fn shutdown(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.next_refresh = None;
        // Close the receiver so senders get errors.
        self.rx.close();
        // Drop the event receiver.
        self.event_rx.take();
    }

    // -- Accessors ----------------------------------------------------------

    /// Borrow the current bootstrap shell state.
    pub fn shell(&self) -> &AppShell {
        &self.shell
    }

    /// Borrow the inner `App` for read-only inspection.
    /// Returns `None` in `Detecting` or `Setup` states.
    pub fn app(&self) -> Option<&App> {
        match &self.shell {
            AppShell::Ready(app) | AppShell::Offline(app, _) => Some(app),
            _ => None,
        }
    }

    /// Mutable borrow of the inner `App`.
    /// Returns `None` in `Detecting` or `Setup` states.
    pub fn app_mut(&mut self) -> Option<&mut App> {
        match &mut self.shell {
            AppShell::Ready(app) | AppShell::Offline(app, _) => Some(app),
            _ => None,
        }
    }

    /// Current generation counter.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Current feature configuration.
    pub fn features(&self) -> &EmbeddedFeatures {
        &self.options.features
    }

    /// Current workspace root.
    pub fn workspace_root(&self) -> &Path {
        &self.options.workspace_root
    }

    // -- Private helpers ----------------------------------------------------

    fn start_load(&mut self) {
        let generation_id = self.generation;
        let tx = self.tx.clone();
        let now = Instant::now();
        self.last_refresh = now;
        self.next_refresh = self.refresh_interval.map(|d| now + d);

        // Spawn a task that sends a Tick to signal load initiation.
        // In a real integration the host's backend populates event data;
        // here we signal that loading started.
        self.handle.spawn(async move {
            let _ = tx.send(TaggedCompletion {
                generation: generation_id,
                event: Event::Tick,
            });
        });
    }

    fn apply_event(&mut self, event: Event) {
        // In Detecting state, the first event determines the shell outcome.
        if matches!(self.shell, AppShell::Detecting) {
            match &event {
                Event::FetchFailed(_tab, msg) => {
                    // Try to parse the message as a setup problem via
                    // known error patterns.
                    let problem = Self::detect_setup_problem(msg);
                    if let Some(p) = problem {
                        self.shell = AppShell::Setup(p);
                    } else {
                        // Unknown failure while detecting — promote to
                        // Ready with the error message shown.
                        let mut app = App::new();
                        if let Some(tab) = self.options.initial_tab {
                            app.active_tab = tab;
                        }
                        app.error_message = Some(msg.clone());
                        self.shell = AppShell::Ready(app);
                    }
                    return;
                }
                _ => {
                    // Any non-failure event → backend is alive → Ready.
                    let mut app = App::new();
                    if let Some(tab) = self.options.initial_tab {
                        app.active_tab = tab;
                    }
                    self.shell = AppShell::Ready(app);
                    // Fall through to apply the event to the new app.
                }
            }
        }

        // Now apply to the inner app (Ready or Offline).
        let app = match &mut self.shell {
            AppShell::Ready(app) | AppShell::Offline(app, _) => app,
            _ => return,
        };

        match event {
            Event::Tick => {
                app.tick();
            }
            Event::IssuesFetched(issues) => {
                app.issues.items = issues;
                // Successful fetch in Offline → promote back to Ready.
                self.promote_to_ready_if_offline();
            }
            Event::MrsFetched(mrs) => {
                app.mrs.items = mrs;
                self.promote_to_ready_if_offline();
            }
            Event::PipelinesFetched(pipelines) => {
                app.pipelines.items = pipelines;
                self.promote_to_ready_if_offline();
            }
            Event::RunnersFetched(runners) => {
                app.runners.items = runners;
                self.promote_to_ready_if_offline();
            }
            Event::ReleasesFetched(releases) => {
                app.releases.items = releases;
                self.promote_to_ready_if_offline();
            }
            Event::BranchesFetched(branches) => {
                app.branches.items = branches;
                self.promote_to_ready_if_offline();
            }
            Event::MilestonesFetched(milestones) => {
                app.milestones.items = milestones;
                self.promote_to_ready_if_offline();
            }
            Event::FetchFailed(_tab, msg) => {
                // Transition Ready → Offline, keeping cached data.
                if matches!(self.shell, AppShell::Ready(_)) {
                    if let AppShell::Ready(app) = std::mem::replace(
                        &mut self.shell,
                        AppShell::Detecting, // placeholder
                    ) {
                        self.shell = AppShell::Offline(app, msg);
                    }
                } else if let AppShell::Offline(_, ref mut err) = self.shell {
                    *err = msg;
                }
            }
            _ => { /* other events handled as needed */ }
        }
    }

    /// If currently `Offline`, promote back to `Ready` (clears the error).
    fn promote_to_ready_if_offline(&mut self) {
        if matches!(self.shell, AppShell::Offline(_, _)) {
            if let AppShell::Offline(app, _) = std::mem::replace(
                &mut self.shell,
                AppShell::Detecting, // placeholder
            ) {
                self.shell = AppShell::Ready(app);
            }
        }
    }

    /// Try to map an error message into a [`SetupProblem`].
    fn detect_setup_problem(msg: &str) -> Option<SetupProblem> {
        // Match the Display output of known GlabError variants.
        let error = if msg.contains("is not installed") {
            if msg.contains("glab") {
                GlabError::CliMissing {
                    cli: "glab".into(),
                    host: None,
                }
            } else if msg.contains("gh") {
                GlabError::CliMissing {
                    cli: "gh".into(),
                    host: None,
                }
            } else {
                GlabError::CliMissing {
                    cli: msg.to_string(),
                    host: None,
                }
            }
        } else if msg.contains("not authenticated") {
            // Default to GitLab; the guide still helps.
            GlabError::NotAuthenticated {
                message: msg.to_string(),
                host: crate::ProjectHost::GitLab,
            }
        } else if msg.contains("not a Git repository") {
            GlabError::NotRepository
        } else {
            return None;
        };

        install_guide(&error).map(|guide| SetupProblem { error, guide })
    }

    fn translate(outcome: ControllerOutcome) -> EmbeddedOutcome {
        match outcome {
            ControllerOutcome::Unchanged => EmbeddedOutcome::Unchanged,
            ControllerOutcome::Changed => EmbeddedOutcome::Changed,
            ControllerOutcome::ExitRequested => EmbeddedOutcome::ExitRequested,
            ControllerOutcome::HostAction(ha) => EmbeddedOutcome::HostAction(ha),
            ControllerOutcome::Command(_) => EmbeddedOutcome::Changed,
        }
    }
}

impl Drop for EmbeddedApp {
    fn drop(&mut self) {
        // Bump generation to invalidate any in-flight completions and close
        // the channel so background senders fail fast.
        self.generation = self.generation.wrapping_add(1);
        self.rx.close();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create an `EmbeddedApp` with a fake tokio runtime and return
    /// the sender so tests can push completions directly.
    fn test_app() -> (EmbeddedApp, mpsc::UnboundedSender<TaggedCompletion>) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = rt.handle().clone();

        let opts = EmbeddedOptions {
            workspace_root: PathBuf::from("/test/repo"),
            initial_tab: None,
            cache_policy: CachePolicy::Manual,
            refresh: None,
            features: EmbeddedFeatures::default(),
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = EmbeddedApp {
            shell: AppShell::Ready(App::new()),
            generation: 1,
            rx,
            tx: tx.clone(),
            handle,
            event_tx,
            event_rx: Some(event_rx),
            visible: true,
            next_refresh: None,
            refresh_interval: None,
            last_refresh: Instant::now(),
            options: opts,
        };

        // Keep rt alive by leaking — test-only.
        std::mem::forget(rt);

        (app, tx)
    }

    /// Helper: create an `EmbeddedApp` in `Detecting` state.
    fn test_app_detecting() -> (EmbeddedApp, mpsc::UnboundedSender<TaggedCompletion>) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = rt.handle().clone();

        let opts = EmbeddedOptions {
            workspace_root: PathBuf::from("/test/repo"),
            initial_tab: None,
            cache_policy: CachePolicy::Manual,
            refresh: None,
            features: EmbeddedFeatures::default(),
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = EmbeddedApp {
            shell: AppShell::Detecting,
            generation: 1,
            rx,
            tx: tx.clone(),
            handle,
            event_tx,
            event_rx: Some(event_rx),
            visible: true,
            next_refresh: None,
            refresh_interval: None,
            last_refresh: Instant::now(),
            options: opts,
        };

        std::mem::forget(rt);

        (app, tx)
    }

    #[test]
    fn poll_applies_matching_generation() {
        let (mut app, tx) = test_app();
        let generation_id = app.generation();

        tx.send(TaggedCompletion {
            generation: generation_id,
            event: Event::Tick,
        })
        .unwrap();

        let changed = app.poll_background();
        assert!(
            changed,
            "matching-generation completion should mutate state"
        );
    }

    #[test]
    fn poll_discards_stale_generation() {
        let (mut app, tx) = test_app();
        let stale_gen = app.generation().wrapping_sub(1);

        tx.send(TaggedCompletion {
            generation: stale_gen,
            event: Event::Tick,
        })
        .unwrap();

        let changed = app.poll_background();
        assert!(
            !changed,
            "stale-generation completion should be silently dropped"
        );
    }

    #[test]
    fn set_workspace_root_bumps_generation() {
        let (mut app, _tx) = test_app();
        let old_gen = app.generation();

        app.set_workspace_root(Path::new("/new/root"));

        assert_eq!(app.generation(), old_gen.wrapping_add(1));
        assert_eq!(app.workspace_root(), Path::new("/new/root"));
    }

    #[test]
    fn snapshot_restore_roundtrip() {
        let (mut app, _tx) = test_app();
        if let Some(inner) = app.app_mut() {
            inner.active_tab = Tab::Pipelines;
        }

        let snap = app.snapshot();
        assert_eq!(snap.active_tab, Tab::Pipelines);

        // Restore to same root should not bump generation.
        let gen_before = app.generation();
        app.restore(snap.clone());
        assert_eq!(app.generation(), gen_before);

        // Restore to different root should bump generation.
        let mut different = snap;
        different.workspace_root = PathBuf::from("/other");
        app.restore(different);
        assert_eq!(app.generation(), gen_before.wrapping_add(1));
    }

    #[test]
    fn shutdown_closes_channel() {
        let (mut app, tx) = test_app();
        app.shutdown();

        let generation_id = app.generation();
        let _ = tx.send(TaggedCompletion {
            generation: generation_id,
            event: Event::Tick,
        });
        // Channel is closed, try_recv returns error.
        let changed = app.poll_background();
        assert!(!changed);
    }

    #[test]
    fn visibility_toggle() {
        let (mut app, _tx) = test_app();
        assert!(app.visible);

        app.set_visible(false);
        assert!(!app.visible);

        app.set_visible(true);
        assert!(app.visible);
    }

    // -- AppShell bootstrap tests -------------------------------------------

    #[test]
    fn detecting_stays_when_no_completions() {
        let (mut app, _tx) = test_app_detecting();
        assert!(matches!(app.shell(), &AppShell::Detecting));

        let changed = app.poll_background();
        assert!(!changed, "no completions → no state change");
        assert!(matches!(app.shell(), &AppShell::Detecting));
    }

    #[test]
    fn detecting_transitions_to_setup_on_cli_missing() {
        let (mut app, tx) = test_app_detecting();
        let generation_id = app.generation();

        tx.send(TaggedCompletion {
            generation: generation_id,
            event: Event::FetchFailed(Tab::Issues, "glab is not installed".into()),
        })
        .unwrap();

        let changed = app.poll_background();
        assert!(changed);
        assert!(
            matches!(app.shell(), AppShell::Setup(_)),
            "CLI-missing failure in Detecting should transition to Setup"
        );
    }

    #[test]
    fn detecting_transitions_to_ready_on_success() {
        let (mut app, tx) = test_app_detecting();
        let generation_id = app.generation();

        tx.send(TaggedCompletion {
            generation: generation_id,
            event: Event::Tick,
        })
        .unwrap();

        let changed = app.poll_background();
        assert!(changed);
        assert!(
            matches!(app.shell(), AppShell::Ready(_)),
            "successful event in Detecting should transition to Ready"
        );
    }
}
