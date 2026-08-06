//! Bounded embedded GitLab/GitHub data view.
//!
//! This library deliberately does not own a terminal, cwd, PTY, or process
//! loop. The host supplies the workspace root and forwards input and frames.

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, mpsc},
    thread,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ProjectHost {
    GitLab,
    GitHub,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRef {
    pub host: ProjectHost,
    pub owner: String,
    pub name: String,
}
impl ProjectRef {
    pub fn new(host: ProjectHost, owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            host,
            owner: owner.into(),
            name: name.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub title: String,
    pub state: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Notification {
    pub id: String,
    pub title: String,
    pub reason: String,
    pub unread: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlabStatus {
    Loading,
    Ready,
    Error(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlabSnapshot {
    pub project: Option<ProjectRef>,
    pub todos: Vec<TodoItem>,
    pub notifications: Vec<Notification>,
    pub status: GlabStatus,
}
impl GlabSnapshot {
    pub fn ready(
        project: Option<ProjectRef>,
        todos: Vec<TodoItem>,
        notifications: Vec<Notification>,
    ) -> Self {
        Self {
            project,
            todos,
            notifications,
            status: GlabStatus::Ready,
        }
    }
    pub fn loading() -> Self {
        Self {
            project: None,
            todos: Vec::new(),
            notifications: Vec::new(),
            status: GlabStatus::Loading,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub cwd: PathBuf,
    pub program: String,
    pub args: Vec<String>,
}
impl CommandSpec {
    pub fn new(root: &Path, program: &str, args: &[&str]) -> Self {
        Self {
            cwd: root.to_path_buf(),
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        }
    }
}
pub trait CommandRunner: Send + Sync + 'static {
    fn run(&self, spec: &CommandSpec) -> Result<String, GlabError>;
}
#[derive(Debug, Default, Clone, Copy)]
struct ProcessCommandRunner;
impl CommandRunner for ProcessCommandRunner {
    fn run(&self, spec: &CommandSpec) -> Result<String, GlabError> {
        let output = Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .output()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    GlabError::CliMissing(spec.program.clone())
                } else {
                    GlabError::Command(error.to_string())
                }
            })?;
        if !output.status.success() {
            let text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(
                if text.to_ascii_lowercase().contains("auth")
                    || text.to_ascii_lowercase().contains("login")
                {
                    GlabError::NotAuthenticated(text)
                } else {
                    GlabError::Command(text)
                },
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlabError {
    CliMissing(String),
    NotAuthenticated(String),
    NotRepository,
    Parse(String),
    Command(String),
}
impl std::fmt::Display for GlabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CliMissing(cli) => write!(f, "{cli} is not installed"),
            Self::NotAuthenticated(message) => write!(f, "not authenticated: {message}"),
            Self::NotRepository => write!(f, "workspace is not a Git repository"),
            Self::Parse(message) | Self::Command(message) => f.write_str(message),
        }
    }
}
pub fn identify_project(remote: &str) -> Option<ProjectRef> {
    let url = remote.split_whitespace().nth(1)?;
    let (host, path) = if let Some(path) = url.strip_prefix("git@gitlab.com:") {
        (ProjectHost::GitLab, path)
    } else if let Some(path) = url.strip_prefix("git@github.com:") {
        (ProjectHost::GitHub, path)
    } else if let Some(path) = url.strip_prefix("https://gitlab.com/") {
        (ProjectHost::GitLab, path)
    } else if let Some(path) = url.strip_prefix("https://github.com/") {
        (ProjectHost::GitHub, path)
    } else {
        return None;
    };
    let mut parts = path.trim_end_matches(".git").split('/');
    Some(ProjectRef::new(host, parts.next()?, parts.next()?))
}
#[derive(Debug, Deserialize)]
struct TodoWire {
    id: serde_json::Value,
    #[serde(alias = "body", alias = "title")]
    title: String,
    #[serde(default = "pending")]
    state: String,
}
fn pending() -> String {
    "pending".into()
}
pub fn parse_todos(body: &str) -> Result<Vec<TodoItem>, GlabError> {
    serde_json::from_str::<Vec<TodoWire>>(body)
        .map(|items| {
            items
                .into_iter()
                .map(|item| TodoItem {
                    id: item.id.to_string().trim_matches('"').to_owned(),
                    title: item.title,
                    state: item.state,
                })
                .collect()
        })
        .map_err(|error| GlabError::Parse(error.to_string()))
}
#[derive(Debug, Deserialize)]
struct NotificationWire {
    id: String,
    reason: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    subject: Option<NotificationSubjectWire>,
    unread: bool,
}
#[derive(Debug, Deserialize)]
struct NotificationSubjectWire {
    title: String,
}
pub fn parse_notifications(body: &str) -> Result<Vec<Notification>, GlabError> {
    serde_json::from_str::<Vec<NotificationWire>>(body)
        .and_then(|items| {
            items
                .into_iter()
                .map(|item| {
                    let title = item
                        .title
                        .or_else(|| item.subject.map(|subject| subject.title))
                        .ok_or_else(|| serde::de::Error::missing_field("title or subject.title"))?;
                    Ok(Notification {
                        id: item.id,
                        title,
                        reason: item.reason,
                        unread: item.unread,
                    })
                })
                .collect()
        })
        .map_err(|error| GlabError::Parse(error.to_string()))
}
pub trait Backend: Send + Sync + 'static {
    fn load(&self, root: &Path) -> Result<GlabSnapshot, GlabError>;
}
#[derive(Clone)]
pub struct ProcessBackend {
    runner: Arc<dyn CommandRunner>,
}
impl Default for ProcessBackend {
    fn default() -> Self {
        Self {
            runner: Arc::new(ProcessCommandRunner),
        }
    }
}
impl ProcessBackend {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}
impl Backend for ProcessBackend {
    fn load(&self, root: &Path) -> Result<GlabSnapshot, GlabError> {
        let remote_spec = CommandSpec::new(root, "git", &["remote", "-v"]);
        let remote = self
            .runner
            .run(&remote_spec)
            .map_err(|_| GlabError::NotRepository)?;
        let project = identify_project(&remote).ok_or(GlabError::NotRepository)?;
        match project.host {
            ProjectHost::GitLab => {
                let spec = CommandSpec::new(root, "glab", &["api", "/todos"]);
                let todos = parse_todos(&self.runner.run(&spec)?)?;
                Ok(GlabSnapshot::ready(Some(project), todos, Vec::new()))
            }
            ProjectHost::GitHub => {
                let spec = CommandSpec::new(root, "gh", &["api", "notifications"]);
                let notifications = parse_notifications(&self.runner.run(&spec)?)?;
                Ok(GlabSnapshot::ready(
                    Some(project),
                    Vec::new(),
                    notifications,
                ))
            }
        }
    }
}
struct Completion {
    generation: u64,
    result: Result<GlabSnapshot, GlabError>,
}
pub struct EmbeddedApp {
    workspace_root: PathBuf,
    area: Rect,
    theme: Color,
    snapshot: GlabSnapshot,
    selected: usize,
    generation: u64,
    rx: mpsc::Receiver<Completion>,
    tx: mpsc::Sender<Completion>,
    backend: Arc<dyn Backend>,
}
impl EmbeddedApp {
    pub fn new(root: &Path, theme: Color) -> Self {
        Self::new_with_backend(root, theme, Arc::new(ProcessBackend::default()))
    }
    pub fn new_with_backend(root: &Path, theme: Color, backend: Arc<dyn Backend>) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            workspace_root: root.to_path_buf(),
            area: Rect::default(),
            theme,
            snapshot: GlabSnapshot::loading(),
            selected: 0,
            generation: 0,
            rx,
            tx,
            backend,
        };
        app.refresh();
        app
    }
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
    pub fn area(&self) -> Rect {
        self.area
    }
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn snapshot(&self) -> &GlabSnapshot {
        &self.snapshot
    }
    pub fn set_snapshot(&mut self, snapshot: GlabSnapshot) {
        self.snapshot = snapshot;
        self.clamp_selected();
    }
    pub fn set_workspace_root(&mut self, root: &Path) {
        self.workspace_root = root.to_path_buf();
        self.refresh();
    }
    pub fn refresh(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.snapshot = GlabSnapshot::loading();
        self.selected = 0;
        let generation = self.generation;
        let root = self.workspace_root.clone();
        let tx = self.tx.clone();
        let backend = self.backend.clone();
        thread::spawn(move || {
            let _ = tx.send(Completion {
                generation,
                result: backend.load(&root),
            });
        });
    }
    fn selectable_len(&self) -> usize {
        self.snapshot.todos.len()
    }
    fn clamp_selected(&mut self) {
        self.selected = self.selected.min(self.selectable_len().saturating_sub(1));
    }
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                self.selected = self.selected.saturating_add(1);
                self.clamp_selected();
                true
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                self.refresh();
                true
            }
            _ => false,
        }
    }
    pub fn on_mouse(&mut self, event: MouseEvent, area: Rect) -> bool {
        if !area.contains((event.column, event.row).into()) {
            return false;
        }
        if matches!(event.kind, MouseEventKind::ScrollUp) {
            self.selected = self.selected.saturating_sub(1);
            return true;
        }
        if matches!(event.kind, MouseEventKind::ScrollDown) {
            self.selected = self.selected.saturating_add(1);
            self.clamp_selected();
            return true;
        }
        false
    }
    pub fn poll_background(&mut self) -> bool {
        let mut changed = false;
        while let Ok(completion) = self.rx.try_recv() {
            if completion.generation != self.generation {
                continue;
            }
            self.snapshot = match completion.result {
                Ok(snapshot) => snapshot,
                Err(error) => GlabSnapshot {
                    project: None,
                    todos: Vec::new(),
                    notifications: Vec::new(),
                    status: GlabStatus::Error(error.to_string()),
                },
            };
            self.clamp_selected();
            changed = true;
        }
        changed
    }
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.render_with_context(frame, area, false, self.theme);
    }
    pub fn render_with_context(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        focused: bool,
        focus_color: Color,
    ) {
        self.area = area;
        let border_style = if focused {
            Style::default().fg(focus_color)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .title(" Glab ")
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let mut lines = vec![Line::from(match &self.snapshot.project {
            Some(project) => format!("{:?}: {}/{}", project.host, project.owner, project.name),
            None => "No project detected".into(),
        })];
        match &self.snapshot.status {
            GlabStatus::Loading => lines.push(Line::raw("Loading remote data...")),
            GlabStatus::Error(message) => lines.push(Line::from(Span::styled(
                message,
                Style::default().fg(Color::Red),
            ))),
            GlabStatus::Ready => {}
        }
        lines.push(Line::raw("Todos"));
        lines.extend(self.snapshot.todos.iter().enumerate().map(|(index, todo)| {
            let marker = if index == self.selected { ">" } else { " " };
            Line::from(format!("{marker} [{}] {}", todo.state, todo.title))
        }));
        lines.push(Line::raw("Notifications"));
        lines.extend(self.snapshot.notifications.iter().map(|item| {
            Line::from(format!(
                "  {} {}",
                if item.unread { "*" } else { " " },
                item.title
            ))
        }));
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().fg(Color::White)),
            inner,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use std::sync::{Arc, Mutex};

    struct FixtureBackend {
        snapshot: GlabSnapshot,
    }

    impl Backend for FixtureBackend {
        fn load(&self, _root: &Path) -> Result<GlabSnapshot, GlabError> {
            Ok(self.snapshot.clone())
        }
    }

    struct RecordingRunner {
        calls: Mutex<Vec<CommandSpec>>,
        responses: Mutex<Vec<String>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, spec: &CommandSpec) -> Result<String, GlabError> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(spec.clone());
            Ok(self
                .responses
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(0))
        }
    }

    #[test]
    fn parses_github_notification_subject_title() {
        let notifications = parse_notifications(
            r#"[{"id":"n1","reason":"mention","subject":{"title":"Fix bug"},"unread":true}]"#,
        )
        .unwrap();
        assert_eq!(notifications[0].title, "Fix bug");
    }

    #[test]
    fn process_backend_uses_only_supported_gitlab_endpoints_and_keeps_todos() {
        let runner = Arc::new(RecordingRunner {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(vec![
                "origin\thttps://gitlab.com/acme/app.git (fetch)".into(),
                "[{\"id\":3,\"body\":\"Review MR\",\"state\":\"pending\"}]".into(),
            ]),
        });
        let backend = ProcessBackend::with_runner(runner.clone());
        let snapshot = backend.load(Path::new("C:/repo")).unwrap();
        assert_eq!(snapshot.todos.len(), 1);
        let calls = runner
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].program, "git");
        assert_eq!(calls[0].args, vec!["remote", "-v"]);
        assert_eq!(calls[1].args, vec!["api", "/todos"]);
        assert!(
            calls
                .iter()
                .all(|call| !call.args.iter().any(|arg| arg == "/notifications"))
        );
    }

    #[test]
    fn embedded_app_starts_background_load_and_clamps_selection() {
        let snapshot = GlabSnapshot::ready(
            None,
            vec![TodoItem {
                id: "1".into(),
                title: "one".into(),
                state: "pending".into(),
            }],
            vec![],
        );
        let mut app = EmbeddedApp::new_with_backend(
            Path::new("C:/repo"),
            Color::White,
            Arc::new(FixtureBackend { snapshot }),
        );
        for _ in 0..100 {
            if app.poll_background() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(matches!(app.snapshot().status, GlabStatus::Ready));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected(), 0);
    }

    #[test]
    fn render_marks_selected_todo() {
        let mut app = EmbeddedApp::new(Path::new("C:/repo"), Color::White);
        app.set_snapshot(GlabSnapshot::ready(
            None,
            vec![TodoItem {
                id: "1".into(),
                title: "one".into(),
                state: "pending".into(),
            }],
            vec![],
        ));
        let backend = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| app.render(frame, Rect::new(0, 0, 40, 10)))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("> [pending] one"));
    }

    #[test]
    fn workspace_members_keep_tuxedo_and_add_glab_tui() {
        let manifest =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"))
                .unwrap();
        assert!(manifest.contains("    \"crates/tuxedo\""));
        assert!(manifest.contains("    \"crates/glab-tui\""));
    }
}
