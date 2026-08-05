//! Native adapter for the embedded Tuxedo todo.txt application.

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use std::any::Any;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tuxedo::embed::{EmbeddedApp, EmbeddedFeatures, EmbeddedOutcome};
use tuxedo::theme::Theme as TuxedoTheme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoAction {
    ExitRequested,
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
    TodoState::Ready(EmbeddedApp::with_features(
        todo_path.to_path_buf(),
        archive_path.to_path_buf(),
        body,
        EmbeddedFeatures {
            clipboard: true,
            ..EmbeddedFeatures::default()
        },
    ))
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
        "todo"
    }

    fn caps(&self) -> PaneCaps {
        PaneCaps::default()
    }

    fn render(
        &mut self,
        area: Rect,
        frame: &mut Frame<'_>,
        _ctx: &PaneRenderCtx<'_>,
    ) -> RenderOutcome {
        match &mut self.state {
            TodoState::Ready(app) => app.render(frame, area, &map_theme(self.theme)),
            TodoState::Error(message) => {
                use ratatui::style::Style;
                use ratatui::text::{Line, Text};
                use ratatui::widgets::{Block, Borders, Paragraph};

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
