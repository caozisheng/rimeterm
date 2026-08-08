//! Native adapter for the embedded Tuxedo todo.txt application.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use std::any::Any;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tuxedo::embed::{EmbeddedApp, EmbeddedFeatures, EmbeddedOutcome};
use tuxedo::theme::Theme as TuxedoTheme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoAction {
    ExitRequested,
    /// User pressed Ctrl+J on a task row. The App runs the project-map
    /// gate (`~/.rimeterm/tuxedo/project.txt`) against `projects`, and
    /// on pass it opens the agent picker anchored inside `anchor` and
    /// eventually injects the cleaned form of `raw`
    /// (`tuxedo::todo::body_only`) into the spawned agent's stdin.
    ///
    /// `raw` is the todo.txt line verbatim — App owns cleanup so the
    /// pane doesn't need to know which tuxedo helper to call.
    DispatchToAgent {
        raw: String,
        projects: Vec<String>,
        anchor: Rect,
    },
}

enum TodoState {
    Ready(EmbeddedApp),
    Error(String),
}

pub struct TodoPane {
    id: PaneId,
    action_tx: mpsc::UnboundedSender<TodoAction>,
    todo_path: PathBuf,
    archive_path: PathBuf,
    theme: rimeterm_markdown::Theme,
    next_poll: Instant,
    state: TodoState,
    /// Last outer rect passed to `render`. Used by `on_key` to anchor the
    /// agent picker inside this pane. `None` before the first render (a
    /// pre-render key press cannot occur in practice — the pane is drawn
    /// once per frame before input is polled — but we default to a zero
    /// rect and let App fall back to `Centered` if it ever does).
    last_outer_area: Option<Rect>,
}

impl TodoPane {
    pub fn new(
        action_tx: mpsc::UnboundedSender<TodoAction>,
        theme: rimeterm_markdown::Theme,
    ) -> Self {
        let todo_path = rimeterm_config::paths::todo_file()
            .unwrap_or_else(|| PathBuf::from(".rimeterm/tuxedo/todo.txt"));
        let archive_path = rimeterm_config::paths::archive_file()
            .unwrap_or_else(|| PathBuf::from(".rimeterm/tuxedo/archive.txt"));
        Self::with_paths(action_tx, theme, todo_path, archive_path)
    }

    fn with_paths(
        action_tx: mpsc::UnboundedSender<TodoAction>,
        theme: rimeterm_markdown::Theme,
        todo_path: PathBuf,
        archive_path: PathBuf,
    ) -> Self {
        let state = load_embedded(&todo_path, &archive_path);
        Self {
            id: PaneId::next(),
            action_tx,
            todo_path,
            archive_path,
            theme,
            next_poll: Instant::now(),
            state,
            last_outer_area: None,
        }
    }

    pub fn set_theme(&mut self, theme: rimeterm_markdown::Theme) {
        self.theme = theme;
    }
    fn retry_load(&mut self) {
        self.state = load_embedded(&self.todo_path, &self.archive_path);
    }

    #[cfg(test)]
    fn paths(&self) -> (&Path, &Path) {
        (&self.todo_path, &self.archive_path)
    }

    pub(crate) fn snapshot_state(&self) -> rimeterm_config::memory_state::PaneState {
        let mut values = std::collections::BTreeMap::new();
        if let TodoState::Ready(app) = &self.state {
            let inner = app.app();
            values.insert(
                "view".into(),
                match inner.view() {
                    tuxedo::app::View::List => "list",
                    tuxedo::app::View::Archive => "archive",
                }
                .into(),
            );
            values.insert("sort".into(), inner.sort_label().into());
            values.insert("search".into(), inner.filter().search.clone());
            if let Some(project) = &inner.filter().project {
                values.insert("project".into(), project.clone());
            }
            if let Some(context) = &inner.filter().context {
                values.insert("context".into(), context.clone());
            }
        }
        rimeterm_config::memory_state::PaneState { values }
    }

    pub(crate) fn restore_state(&mut self, state: &rimeterm_config::memory_state::PaneState) {
        let TodoState::Ready(app) = &mut self.state else {
            return;
        };
        let inner = app.app_mut();
        inner.set_view(match state.values.get("view").map(String::as_str) {
            Some("archive") => tuxedo::app::View::Archive,
            _ => tuxedo::app::View::List,
        });
        if let Some(sort) = state
            .values
            .get("sort")
            .and_then(|value| value.parse().ok())
        {
            inner.prefs.sort = sort;
        }
        inner.set_search(state.values.get("search").cloned().unwrap_or_default());
        inner.set_project_filter(state.values.get("project").cloned());
        inner.set_context_filter(state.values.get("context").cloned());
    }
}

fn load_embedded(todo_path: &Path, archive_path: &Path) -> TodoState {
    let body = match std::fs::read_to_string(todo_path) {
        Ok(body) => body,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return TodoState::Error(format!("failed to read {}: {error}", todo_path.display()));
        }
    };
    let mut app = EmbeddedApp::with_features(
        todo_path.to_path_buf(),
        archive_path.to_path_buf(),
        body,
        EmbeddedFeatures {
            clipboard: true,
            ..EmbeddedFeatures::default()
        },
    );
    // Advertise the host-only Ctrl+J shortcut in tuxedo's Normal-mode
    // status hint. The prompt is prefilled in the target agent's input
    // widget on dispatch; the user reviews and presses Enter to submit
    // — that "confirm" leg needs a discoverable hint so users know the
    // shortcut exists.
    app.app_mut().embedded_hint = Some("Ctrl+J → agent (Enter to run)");
    TodoState::Ready(app)
}

fn map_theme(theme: rimeterm_markdown::Theme) -> TuxedoTheme {
    let palette = rimeterm_markdown::Palette::from_theme(theme);
    TuxedoTheme {
        name: "rimeterm",
        bg: palette.background,
        panel: palette.code_bg,
        border: palette.border,
        fg: palette.foreground,
        dim: palette.dim,
        accent: palette.accent,
        cursor: palette.selection_bg,
        selection: palette.selection_bg,
        statusbar: palette.status_bar_bg,
        status_fg: palette.status_bar_fg,
        mode_fg: palette.on_accent_fg,
        mode_bg: palette.accent,
        pri_a: palette.git_modified,
        pri_b: palette.accent_alt,
        pri_c: palette.git_new,
        pri_d: palette.link,
        pri_other: palette.heading_other,
        project: palette.git_new,
        context: palette.accent_alt,
        due: palette.accent_alt,
        overdue: palette.git_modified,
        today: palette.current_match_bg,
        done: palette.dim,
        selected: palette.selection_bg,
        matched: palette.search_match_bg,
    }
}

impl PaneProvider for TodoPane {
    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn id(&self) -> PaneId {
        self.id
    }

    fn title(&self) -> &str {
        "Tuxedo"
    }

    fn caps(&self) -> PaneCaps {
        PaneCaps::default()
    }

    fn render(
        &mut self,
        area: Rect,
        frame: &mut Frame<'_>,
        ctx: &PaneRenderCtx<'_>,
    ) -> RenderOutcome {
        self.last_outer_area = Some(area);
        match &mut self.state {
            TodoState::Ready(app) => {
                let border_style = if ctx.focused {
                    Style::default().fg(ctx.focus_color)
                } else {
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM)
                };
                let block = Block::default()
                    .title(" Tuxedo ")
                    .borders(Borders::ALL)
                    .border_style(border_style);
                let inner = block.inner(area);
                frame.render_widget(block, area);
                if inner.height == 0 || inner.width == 0 {
                    return RenderOutcome::default();
                }
                app.render(frame, inner, &map_theme(self.theme));
            }
            TodoState::Error(message) => {
                use ratatui::text::{Line, Text};
                use ratatui::widgets::Paragraph;

                let palette = rimeterm_markdown::Palette::from_theme(self.theme);
                let block = Block::default()
                    .title(" todo unavailable ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.git_modified));
                let inner = block.inner(area);
                frame.render_widget(block, area);
                frame.render_widget(
                    Paragraph::new(Text::from(vec![
                        Line::raw(message.clone()),
                        Line::raw(""),
                        Line::raw("Press F5 to retry."),
                    ]))
                    .style(Style::default().fg(palette.foreground)),
                    inner,
                );
            }
        }
        RenderOutcome::default()
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        let TodoState::Ready(app) = &mut self.state else {
            return false;
        };
        // Intercept Ctrl+J BEFORE forwarding to tuxedo. tuxedo binds
        // `Char('j')` (any modifier) to CursorDown, so a bare forward
        // would still move the cursor even with CONTROL held. We only
        // trigger dispatch when CONTROL is present and ALT/SHIFT are
        // not — tightest match so future modifier chords (e.g. a
        // hypothetical Ctrl+Alt+J) stay free.
        if key.code == KeyCode::Char('j')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            let Some(task) = app.app().cur_task() else {
                // No task under the cursor (empty list, empty archive
                // view, filter matched nothing). Consume the key so it
                // doesn't fall through to tuxedo's CursorDown (which
                // is a no-op here anyway) and stay silent — no toast
                // for an obviously-empty situation.
                return true;
            };
            let raw = task.raw.clone();
            let projects = task.projects.clone();
            let anchor = self.last_outer_area.unwrap_or_default();
            let _ = self.action_tx.send(TodoAction::DispatchToAgent {
                raw,
                projects,
                anchor,
            });
            return true;
        }
        if app.handle_key(key) == EmbeddedOutcome::ExitRequested {
            let _ = self.action_tx.send(TodoAction::ExitRequested);
        }
        true
    }

    fn on_mouse(&mut self, event: MouseEvent, outer: Rect) -> bool {
        let TodoState::Ready(app) = &mut self.state else {
            return false;
        };
        app.on_mouse(event, outer)
    }

    fn scrollbar_dragging(&self) -> bool {
        matches!(&self.state, TodoState::Ready(app) if app.scrollbar_dragging())
    }

    fn wants_mouse_priority(&self, _shift_held: bool) -> bool {
        matches!(&self.state, TodoState::Ready(_))
    }
    fn poll_background(&mut self) -> bool {
        if Instant::now() < self.next_poll {
            return false;
        }
        self.next_poll = Instant::now() + Duration::from_millis(250);
        let TodoState::Ready(app) = &mut self.state else {
            return false;
        };
        app.poll_background() == EmbeddedOutcome::Changed
    }

    fn reload(&mut self) {
        match &mut self.state {
            TodoState::Ready(app) => {
                if let Err(error) = app.reload() {
                    self.state = TodoState::Error(format!(
                        "failed to read {}: {error}",
                        self.todo_path.display()
                    ));
                }
            }
            TodoState::Error(_) => self.retry_load(),
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    #[test]
    fn ready_pane_draws_titled_outer_border() {
        let (todo, done) = fixture("render-border");
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut pane = TodoPane::with_paths(tx, rimeterm_markdown::Theme::Default, todo, done);
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();

        terminal
            .draw(|frame| {
                pane.render(
                    frame.area(),
                    frame,
                    &PaneRenderCtx {
                        focused: false,
                        title_override: None,
                        focus_color: Color::Reset,
                    },
                );
            })
            .unwrap();

        let top: String = (0..40)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect();
        assert!(
            top.starts_with("┌ Tuxedo "),
            "unexpected top border: {top:?}"
        );
    }

    #[test]
    fn hint_bar_advertises_ctrl_j_shortcut() {
        // Regression guard: rimeterm's TodoPane must inject a hint
        // into tuxedo's Normal-mode status bar so the Ctrl+J → agent
        // dispatch shortcut is discoverable from inside the pane.
        // Standalone tuxedo doesn't own this binding; only the
        // embedded host advertises it.
        let (todo, done) = fixture("hint-bar");
        std::fs::create_dir_all(todo.parent().unwrap()).unwrap();
        std::fs::write(&todo, "one task\n").unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut pane = TodoPane::with_paths(tx, rimeterm_markdown::Theme::Default, todo, done);
        // 120x30 gives the status bar comfortable room so the hint
        // isn't truncated by the middle-area layout.
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| {
                pane.render(
                    frame.area(),
                    frame,
                    &PaneRenderCtx {
                        focused: true,
                        title_override: None,
                        focus_color: Color::Reset,
                    },
                );
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut rendered = String::new();
        for y in 0..30 {
            for x in 0..120 {
                rendered.push_str(buf[(x, y)].symbol());
            }
            rendered.push('\n');
        }
        assert!(
            rendered.contains("Ctrl+J"),
            "status bar must advertise Ctrl+J; rendered =\n{rendered}"
        );
    }

    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "rimeterm-todo-pane-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        (root.join("todo.txt"), root.join("archive.txt"))
    }

    #[test]
    fn pane_keeps_explicit_global_paths() {
        let (todo, done) = fixture("paths");
        let (tx, _rx) = mpsc::unbounded_channel();
        let pane = TodoPane::with_paths(
            tx,
            rimeterm_markdown::Theme::Default,
            todo.clone(),
            done.clone(),
        );

        assert_eq!(pane.paths(), (todo.as_path(), done.as_path()));
    }

    #[test]
    fn normal_quit_requests_files_activation() {
        let (todo, done) = fixture("quit");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pane = TodoPane::with_paths(tx, rimeterm_markdown::Theme::Default, todo, done);

        assert!(pane.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert_eq!(rx.try_recv(), Ok(TodoAction::ExitRequested));
    }

    #[test]
    fn ctrl_j_on_task_emits_dispatch_action() {
        let (todo, done) = fixture("ctrl-j-dispatch");
        std::fs::create_dir_all(todo.parent().unwrap()).unwrap();
        std::fs::write(&todo, "wire up new feature +work @home\nsecond task\n").unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pane = TodoPane::with_paths(tx, rimeterm_markdown::Theme::Default, todo, done);
        // Render once so `last_outer_area` is populated. Ctrl+J before
        // any render works too (falls back to Rect::default()) but the
        // realistic path goes through render first.
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
        terminal
            .draw(|frame| {
                pane.render(
                    frame.area(),
                    frame,
                    &PaneRenderCtx {
                        focused: true,
                        title_override: None,
                        focus_color: Color::Reset,
                    },
                );
            })
            .unwrap();

        assert!(pane.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL,)));
        match rx.try_recv() {
            Ok(TodoAction::DispatchToAgent {
                raw,
                projects,
                anchor,
            }) => {
                assert_eq!(raw, "wire up new feature +work @home");
                assert_eq!(projects, vec!["work".to_string()]);
                assert_eq!(anchor.width, 40);
                assert_eq!(anchor.height, 6);
            }
            other => panic!("expected DispatchToAgent, got {other:?}"),
        }
    }

    #[test]
    fn plain_j_does_not_emit_dispatch() {
        // Regression guard: bare `j` MUST fall through to tuxedo's
        // CursorDown, never fire dispatch. This protects the primary
        // vim binding after the Ctrl+J intercept was added.
        let (todo, done) = fixture("plain-j-passthrough");
        std::fs::create_dir_all(todo.parent().unwrap()).unwrap();
        std::fs::write(&todo, "one\ntwo\n").unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pane = TodoPane::with_paths(tx, rimeterm_markdown::Theme::Default, todo, done);

        assert!(pane.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)));
        assert!(rx.try_recv().is_err(), "plain j must not emit a TodoAction",);
    }

    #[test]
    fn ctrl_j_on_empty_list_swallows_key_without_emitting() {
        let (todo, done) = fixture("ctrl-j-empty");
        // No file created — tuxedo loads an empty todo list.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pane = TodoPane::with_paths(tx, rimeterm_markdown::Theme::Default, todo, done);

        assert!(pane.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL,)));
        assert!(
            rx.try_recv().is_err(),
            "Ctrl+J on empty list must not emit an action",
        );
    }
    #[test]
    fn theme_mapping_uses_rimeterm_palette_roles() {
        let source = rimeterm_markdown::Palette::from_theme(rimeterm_markdown::Theme::Dracula);
        let mapped = map_theme(rimeterm_markdown::Theme::Dracula);

        assert_eq!(mapped.bg, source.background);
        assert_eq!(mapped.accent, source.accent);
        assert_eq!(mapped.project, source.git_new);
        assert_eq!(mapped.overdue, source.git_modified);
    }

    #[test]
    fn missing_file_opens_empty_without_creating_workspace_state() {
        let (todo, done) = fixture("missing");
        let (tx, _rx) = mpsc::unbounded_channel();
        let pane = TodoPane::with_paths(tx, rimeterm_markdown::Theme::Default, todo.clone(), done);

        assert!(matches!(pane.state, TodoState::Ready(_)));
        assert!(!todo.exists());
    }
    #[test]
    fn stable_state_round_trips_view_sort_and_filters() {
        let (todo, done) = fixture("memory-source");
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut source =
            TodoPane::with_paths(tx.clone(), rimeterm_markdown::Theme::Default, todo, done);
        let TodoState::Ready(app) = &mut source.state else {
            panic!("ready");
        };
        app.app_mut().set_view(tuxedo::app::View::Archive);
        app.app_mut().prefs.sort = tuxedo::app::Sort::Due;
        app.app_mut().set_search("urgent".into());
        app.app_mut().set_project_filter(Some("work".into()));
        let state = source.snapshot_state();

        let (todo, done) = fixture("memory-restored");
        let mut restored = TodoPane::with_paths(tx, rimeterm_markdown::Theme::Default, todo, done);
        restored.restore_state(&state);
        let TodoState::Ready(app) = &restored.state else {
            panic!("ready");
        };
        assert_eq!(app.app().view(), tuxedo::app::View::Archive);
        assert_eq!(app.app().sort_label(), "due");
        assert_eq!(app.app().filter().search, "urgent");
        assert_eq!(app.app().filter().project.as_deref(), Some("work"));
    }
}
