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
    sync::mpsc,
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
    pub fn run(&self) -> Result<String, GlabError> {
        let output = Command::new(&self.program)
            .args(&self.args)
            .current_dir(&self.cwd)
            .output()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    GlabError::CliMissing(self.program.clone())
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
pub fn parse_notifications(body: &str) -> Result<Vec<Notification>, GlabError> {
    serde_json::from_str(body).map_err(|error| GlabError::Parse(error.to_string()))
}
pub trait Backend: Send + 'static {
    fn load(&self, root: &Path) -> Result<GlabSnapshot, GlabError>;
}
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessBackend;
impl Backend for ProcessBackend {
    fn load(&self, root: &Path) -> Result<GlabSnapshot, GlabError> {
        let remote = CommandSpec::new(root, "git", &["remote", "-v"])
            .run()
            .map_err(|_| GlabError::NotRepository)?;
        let project = identify_project(&remote).ok_or(GlabError::NotRepository)?;
        let (cli, todo_endpoint, notification_endpoint) = match project.host {
            ProjectHost::GitLab => ("glab", vec!["api", "/todos"], vec!["api", "/notifications"]),
            ProjectHost::GitHub => (
                "gh",
                vec!["api", "notifications"],
                vec!["api", "notifications"],
            ),
        };
        let todo_args: Vec<&str> = todo_endpoint.to_vec();
        let notification_args: Vec<&str> = notification_endpoint.to_vec();
        let todos = parse_todos(&CommandSpec::new(root, cli, &todo_args).run()?)?;
        let notifications =
            parse_notifications(&CommandSpec::new(root, cli, &notification_args).run()?)?;
        Ok(GlabSnapshot::ready(Some(project), todos, notifications))
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
}
impl EmbeddedApp {
    pub fn new(root: &Path, theme: Color) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            workspace_root: root.to_path_buf(),
            area: Rect::default(),
            theme,
            snapshot: GlabSnapshot::loading(),
            selected: 0,
            generation: 0,
            rx,
            tx,
        }
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
    }
    pub fn set_workspace_root(&mut self, root: &Path) {
        self.workspace_root = root.to_path_buf();
        self.refresh();
    }
    pub fn refresh(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.snapshot = GlabSnapshot::loading();
        let generation = self.generation;
        let root = self.workspace_root.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Completion {
                generation,
                result: ProcessBackend.load(&root),
            });
        });
    }
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                self.selected = self.selected.saturating_add(1);
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
            changed = true;
        }
        changed
    }
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.area = area;
        let block = Block::default()
            .title(" Glab ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme));
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
        lines.extend(
            self.snapshot
                .todos
                .iter()
                .map(|todo| Line::from(format!("  [{}] {}", todo.state, todo.title))),
        );
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
    use crossterm::event::{KeyEvent, KeyModifiers};
    #[test]
    fn identifies_gitlab_and_github_projects_from_remotes() {
        assert_eq!(
            identify_project("origin\thttps://gitlab.com/acme/app.git (fetch)"),
            Some(ProjectRef::new(ProjectHost::GitLab, "acme", "app"))
        );
        assert_eq!(
            identify_project("origin\tgit@github.com:acme/app.git (fetch)"),
            Some(ProjectRef::new(ProjectHost::GitHub, "acme", "app"))
        );
    }
    #[test]
    fn parses_todos_and_notifications() {
        let todos = parse_todos(r#"[{"id":3,"body":"Review MR","state":"pending"}]"#).unwrap();
        assert_eq!(todos[0].title, "Review MR");
        let notifications = parse_notifications(
            r#"[{"id":"n1","reason":"mention","title":"Fix bug","unread":true}]"#,
        )
        .unwrap();
        assert_eq!(notifications[0].title, "Fix bug");
    }
    #[test]
    fn command_spec_requires_explicit_workspace_root() {
        let spec = CommandSpec::new(Path::new("C:/repo"), "glab", &["api", "todos"]);
        assert_eq!(spec.cwd, Path::new("C:/repo"));
        assert_eq!(spec.program, "glab");
    }
    #[test]
    fn embedded_app_handles_input_and_area() {
        let mut app = EmbeddedApp::new(Path::new("C:/repo"), Color::White);
        assert!(app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(app.selected(), 1);
        assert_eq!(app.area(), Rect::default());
    }
}
