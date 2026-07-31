//! Native embedding boundary for rendering Tuxedo inside a host-owned terminal.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::app::App;
use crate::config::Config;
use crate::controller::{self, ControllerFeatures, ControllerOutcome};
use crate::keybinds::KeyBindings;
use crate::theme::Theme;

/// Capabilities allowed while Tuxedo is hosted as a native pane.
///
/// The defaults deliberately disable operations that own process-global UI,
/// launch external programs, check for updates, or persist host-independent
/// configuration. Task mutations, archive handling, and local overlays remain
/// available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddedFeatures {
    pub share: bool,
    pub notes: bool,
    pub config: bool,
    pub theme: bool,
    pub clipboard: bool,
    pub updates: bool,
}

/// Result of an embedded input or background operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddedOutcome {
    /// State may have changed and the host should redraw the pane.
    Changed,
    /// The input did not request removal of the pane.
    Unchanged,
    /// Normal-mode quit requested that the host close this pane.
    ExitRequested,
}

/// Controller for a Tuxedo pane that never initializes, polls, or restores a terminal.
pub struct EmbeddedApp {
    app: App,
    keybinds: KeyBindings,
    done_path: PathBuf,
    features: EmbeddedFeatures,
}

impl EmbeddedApp {
    /// Construct from explicit todo/done paths and an already-read todo body.
    ///
    /// This uses [`Config::default`], default key bindings, and does not read
    /// independent Tuxedo config, keybinding, theme, or update state.
    pub fn new(todo_path: PathBuf, done_path: PathBuf, todo_body: String) -> Self {
        Self::with_features(todo_path, done_path, todo_body, EmbeddedFeatures::default())
    }

    /// Construct with explicit embedded feature flags.
    pub fn with_features(
        todo_path: PathBuf,
        done_path: PathBuf,
        todo_body: String,
        features: EmbeddedFeatures,
    ) -> Self {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let app = App::new_with_done(
            todo_path,
            done_path.clone(),
            todo_body,
            today,
            Config::default(),
        );
        Self {
            app,
            keybinds: KeyBindings::default(),
            done_path,
            features,
        }
    }

    /// Read-only access for host integration and inspection.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// Explicit archive path supplied at construction.
    pub fn done_path(&self) -> &Path {
        &self.done_path
    }

    /// Handle one key without reading terminal events or launching an editor.
    pub fn handle_key(&mut self, key: KeyEvent) -> EmbeddedOutcome {
        let features = ControllerFeatures {
            share: self.features.share,
            notes: self.features.notes,
            config: self.features.config,
            theme: self.features.theme,
            clipboard: self.features.clipboard,
        };
        match controller::handle_key(&mut self.app, key, &self.keybinds, features) {
            ControllerOutcome::Handled => EmbeddedOutcome::Changed,
            ControllerOutcome::ExitRequested => EmbeddedOutcome::ExitRequested,
        }
    }

    /// Render exclusively inside `area` with the host-provided theme.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        crate::ui::draw_in(frame, area, &mut self.app, theme);
    }

    /// Earliest local flash/chord deadline; hosts can schedule their next poll.
    pub fn next_deadline(&self) -> Option<Instant> {
        controller::next_deadline(&self.app)
    }

    /// Poll archive loading, external todo/done edits, inbox, midnight, and expiries.
    ///
    /// No network update checker or configuration watcher is started.
    pub fn poll_background(&mut self) -> EmbeddedOutcome {
        let mut changed = self
            .app
            .refresh_today(chrono::Local::now().format("%Y-%m-%d").to_string());
        changed |= self.app.poll_archive();
        changed |= self.app.poll_external_changes();
        changed |= controller::clear_expired(&mut self.app);
        if changed {
            EmbeddedOutcome::Changed
        } else {
            EmbeddedOutcome::Unchanged
        }
    }

    /// Strictly reread the todo file, preserving in-memory state on read failure.
    pub fn reload(&mut self) -> io::Result<EmbeddedOutcome> {
        let body = fs::read_to_string(&self.app.file_path)?;
        let todo_path = self.app.file_path.clone();
        self.app.open_file(todo_path, self.done_path.clone(), body);
        Ok(EmbeddedOutcome::Changed)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    use super::{EmbeddedApp, EmbeddedOutcome};
    use crate::theme;

    fn paths(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "tuxedo-embed-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create fixture directory");
        (root.join("todo.txt"), root.join("custom-done.txt"))
    }

    #[test]
    fn constructor_uses_explicit_done_path() {
        let (todo, done) = paths("done-path");
        std::fs::write(&todo, "live\n").expect("write todo");
        std::fs::write(&done, "x 2026-05-02 archived\n").expect("write done");
        let mut embedded = EmbeddedApp::new(todo, done.clone(), "live\n".into());

        let deadline = Instant::now() + Duration::from_secs(2);
        while embedded.app().archive().is_empty() && Instant::now() < deadline {
            embedded.poll_background();
            std::thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(embedded.done_path(), done);
        assert_eq!(embedded.app().archive().len(), 1);
    }

    #[test]
    fn normal_quit_requests_host_exit() {
        let (todo, done) = paths("quit");
        std::fs::write(&todo, "live\n").expect("write todo");
        let mut embedded = EmbeddedApp::new(todo, done, "live\n".into());

        let outcome = embedded.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));

        assert_eq!(outcome, EmbeddedOutcome::ExitRequested);
        assert!(!embedded.app().should_quit);
    }

    #[test]
    fn embedded_copy_does_not_write_host_clipboard() {
        let (todo, done) = paths("clipboard");
        std::fs::write(&todo, "live\n").expect("write todo");
        let mut embedded = EmbeddedApp::new(todo, done, "live\n".into());

        let _ = embedded.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        let outcome = embedded.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

        assert_eq!(outcome, EmbeddedOutcome::Changed);
        assert_eq!(
            embedded.app().flash_active(),
            Some("clipboard unavailable when embedded")
        );
    }

    #[test]
    fn render_does_not_touch_cells_outside_caller_rect() {
        let (todo, done) = paths("bounded");
        std::fs::write(&todo, "live\n").expect("write todo");
        let mut embedded = EmbeddedApp::new(todo, done, "live\n".into());
        let backend = TestBackend::new(30, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let pane = Rect::new(5, 3, 18, 6);

        terminal
            .draw(|frame| {
                for y in 0..frame.area().height {
                    for x in 0..frame.area().width {
                        frame.buffer_mut()[(x, y)].set_symbol("#");
                    }
                }
                embedded.render(frame, pane, &theme::MUTED);
            })
            .expect("render");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "#");
        assert_eq!(buffer[(29, 11)].symbol(), "#");
        assert_ne!(buffer[(pane.x, pane.y)].symbol(), "#");
    }

    #[test]
    fn render_uses_injected_theme() {
        let (todo, done) = paths("theme");
        std::fs::write(&todo, "live\n").expect("write todo");
        let mut embedded = EmbeddedApp::new(todo, done, "live\n".into());
        let backend = TestBackend::new(24, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut injected = theme::MUTED;
        injected.bg = Color::Rgb(1, 2, 3);
        let pane = Rect::new(2, 1, 20, 6);

        terminal
            .draw(|frame| embedded.render(frame, pane, &injected))
            .expect("render");
        let buffer = terminal.backend().buffer();
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.bg == Color::Rgb(1, 2, 3)),
            "caller theme background must appear inside the pane"
        );
    }
    #[test]
    fn narrow_pane_keeps_task_body_visible() {
        let (todo, done) = paths("narrow");
        std::fs::write(&todo, "task-visible-marker\n").expect("write todo");
        let mut embedded = EmbeddedApp::new(todo, done, "task-visible-marker\n".into());
        let backend = TestBackend::new(48, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");

        terminal
            .draw(|frame| embedded.render(frame, frame.area(), &theme::MUTED))
            .expect("render");

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            text.contains("task-visible-marker"),
            "task body was hidden: {text}"
        );
    }
}
