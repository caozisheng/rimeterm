#![allow(dead_code, unused_variables)]

//! Bounded embedded GitLab/GitHub data view.
//!
//! This library deliberately does not own a terminal, cwd, PTY, or process
//! loop. The host supplies the workspace root and forwards input and frames.

mod app;
mod backend;
mod cli;
mod command;
mod config;
pub mod controller;
mod domain;
mod editor;
pub mod embed;
mod entity_editor;
mod event;
mod fetch;
mod git_helpers;
pub mod handlers;
mod keybinding;
mod templates;
mod ui;
pub mod utils;

type AppTerminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

use serde::Deserialize;

// Re-export the full embedded API so callers can write `glab_tui::EmbeddedApp`.
pub use embed::*;

// ---------------------------------------------------------------------------
// Shared domain types (used by embed, backend, and the host)
// ---------------------------------------------------------------------------

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlabError {
    CliMissing {
        cli: String,
        host: Option<ProjectHost>,
    },
    NotAuthenticated {
        message: String,
        host: ProjectHost,
    },
    NotRepository,
    Parse(String),
    Command(String),
}

impl GlabError {
    fn with_host(self, host: ProjectHost) -> Self {
        match self {
            Self::CliMissing { cli, .. } => Self::CliMissing {
                cli,
                host: Some(host),
            },
            Self::NotAuthenticated { message, .. } => Self::NotAuthenticated { message, host },
            error => error,
        }
    }
}

impl std::fmt::Display for GlabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CliMissing { cli, .. } => write!(f, "{cli} is not installed"),
            Self::NotAuthenticated { message, .. } => {
                write!(f, "not authenticated: {message}")
            }
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

// ---------------------------------------------------------------------------
// Install / auth guides
// ---------------------------------------------------------------------------

const GITLAB_INSTALL_GUIDE: &str = "GitLab setup required

RimeTerm contains the Glab pane UI. Do not install the glab-tui binary.

1. Verify git and the origin repository:
   git --version
   git remote get-url origin
   If origin is missing:
   git remote add origin https://gitlab.com/OWNER/REPOSITORY.git

2. Install the glab business backend for your platform:
   Windows (winget): winget install --id GLab.GLab
   Windows (Scoop): scoop install glab
   macOS (Homebrew): brew install glab
   Linux and other platforms: use your package manager or download an official release from https://gitlab.com/gitlab-org/cli/-/releases

3. Authenticate in your terminal:
   glab auth login

RimeTerm does not read or save tokens; glab owns its authentication data.
Reload this pane with F5 or r after setup.";

const GITHUB_INSTALL_GUIDE: &str = "GitHub setup required

RimeTerm contains the Glab pane UI. Do not install the glab-tui binary.

1. Verify git and the origin repository:
   git --version
   git remote get-url origin
   If origin is missing:
   git remote add origin https://github.com/OWNER/REPOSITORY.git

2. Install the gh business backend for your platform:
   Windows (winget): winget install --id GitHub.cli
   Windows (Scoop): scoop install gh
   macOS (Homebrew): brew install gh
   Linux: use your distribution package manager or follow the official instructions at https://cli.github.com

3. Authenticate in your terminal:
   gh auth login

RimeTerm does not read or save tokens; gh owns its authentication data.
Reload this pane with F5 or r after setup.";

const REPOSITORY_INSTALL_GUIDE: &str = "Repository setup required

RimeTerm needs git and an origin that identifies a GitLab or GitHub repository.

1. Verify git and inspect origin:
   git --version
   git remote get-url origin

2. If this directory is not a repository, initialize it:
   git init

3. Add or correct origin using one of these forms:
   git remote add origin https://gitlab.com/OWNER/REPOSITORY.git
   git remote add origin https://github.com/OWNER/REPOSITORY.git
   Existing origin: git remote set-url origin URL

4. Install and authenticate the backend for that host:
   GitLab: install glab, then run glab auth login
   GitHub: install gh, then run gh auth login

RimeTerm contains the UI, does not install glab-tui, and does not read or save tokens.
Reload this pane with F5 or r after setup.";

const GENERIC_SETUP_GUIDE: &str = "Glab pane could not connect

The pane failed during workspace detection. Check the following:

1. Verify this directory is a git repository with a valid remote:
   git remote get-url origin

2. Make sure the appropriate CLI is installed and authenticated:
   GitLab: glab auth login
   GitHub: gh auth login

3. You can also configure the repository and token manually:
   Press F10 → Settings → Glab tab

Reload this pane with F5 or r after setup.";

pub fn install_guide(error: &GlabError) -> Option<&'static str> {
    match error {
        GlabError::CliMissing {
            host: Some(ProjectHost::GitLab),
            ..
        }
        | GlabError::NotAuthenticated {
            host: ProjectHost::GitLab,
            ..
        } => Some(GITLAB_INSTALL_GUIDE),
        GlabError::CliMissing {
            host: Some(ProjectHost::GitHub),
            ..
        }
        | GlabError::NotAuthenticated {
            host: ProjectHost::GitHub,
            ..
        } => Some(GITHUB_INSTALL_GUIDE),
        GlabError::CliMissing { cli, host: None } if cli == "glab" => Some(GITLAB_INSTALL_GUIDE),
        GlabError::CliMissing { cli, host: None } if cli == "gh" => Some(GITHUB_INSTALL_GUIDE),
        GlabError::CliMissing { .. } | GlabError::NotRepository => Some(REPOSITORY_INSTALL_GUIDE),
        GlabError::Parse(_) | GlabError::Command(_) => Some(GENERIC_SETUP_GUIDE),
    }
}
