//! Owned data model for the Native GitPane.
//!
//! Everything in this module is a plain, `Send + Sync` DTO that the
//! background worker fills in and hands to the pane on the main thread.
//! `gix::Repository` handles never cross the pane boundary.

use std::path::PathBuf;
use std::time::Instant;

/// Which side of a change a diff request targets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeSide {
    /// HEAD tree ↔ index (staged).
    Staged,
    /// Index ↔ worktree (unstaged / untracked).
    Worktree,
}

/// A change kind coarse enough for UI presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    TypeChange,
    Untracked,
    Conflict,
}

impl ChangeKind {
    /// Single-character status marker (mirrors `git status --short`).
    pub fn short(self) -> char {
        match self {
            ChangeKind::Added => 'A',
            ChangeKind::Modified => 'M',
            ChangeKind::Deleted => 'D',
            ChangeKind::Renamed => 'R',
            ChangeKind::TypeChange => 'T',
            ChangeKind::Untracked => '?',
            ChangeKind::Conflict => 'U',
        }
    }
}

/// One entry in the changes list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitChange {
    pub side: ChangeSide,
    pub kind: ChangeKind,
    pub path: PathBuf,
    pub previous_path: Option<PathBuf>,
    pub is_binary: bool,
}

/// Concise info about the current HEAD.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadSummary {
    /// Human-readable label ("main", "detached@abc1234", "(unborn)").
    pub label: String,
    /// Explicit detached-HEAD marker.
    pub detached: bool,
    /// True when HEAD points to an unborn branch (no commits yet).
    pub unborn: bool,
}

/// Upstream tracking summary for the current branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpstreamSummary {
    pub remote: String,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
}

/// Compact commit info suitable for a scrolling history list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitSummary {
    pub id: String,
    pub short_id: String,
    pub summary: String,
    pub author: String,
    pub seconds_since_epoch: i64,
}

/// A single snapshot produced by the worker; monotonically generation-tagged.
#[derive(Clone, Debug)]
pub struct GitSnapshot {
    pub generation: u64,
    pub repo_root: Option<PathBuf>,
    pub head: Option<HeadSummary>,
    pub upstream: Option<UpstreamSummary>,
    pub changes: Vec<GitChange>,
    pub commits: Vec<CommitSummary>,
    pub scanned_at: Instant,
}

impl GitSnapshot {
    pub fn empty(generation: u64) -> Self {
        Self {
            generation,
            repo_root: None,
            head: None,
            upstream: None,
            changes: Vec::new(),
            commits: Vec::new(),
            scanned_at: Instant::now(),
        }
    }
}

/// A worker request, tagged by generation so stale replies are dropped.
#[derive(Clone, Debug)]
pub enum GitRequest {
    /// Discover a repository from the supplied directory and produce a snapshot.
    Snapshot { generation: u64, cwd: PathBuf },
    /// Compute a diff against a specific worktree change.
    WorktreeDiff {
        generation: u64,
        repo_root: PathBuf,
        change: GitChange,
    },
}

/// Worker response, wrapped in the completing generation.
#[derive(Debug)]
pub enum GitResponse {
    Snapshot(GitSnapshot),
    Diff(DiffSnapshot),
}

impl GitResponse {
    pub fn generation(&self) -> u64 {
        match self {
            GitResponse::Snapshot(snap) => snap.generation,
            GitResponse::Diff(diff) => diff.generation,
        }
    }
}

/// One hunk inside a file diff (unified format).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

/// One physical line inside a hunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffLine {
    pub origin: DiffLineOrigin,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffLineOrigin {
    Context,
    Addition,
    Removal,
}

/// A resolved diff for a single file.
#[derive(Clone, Debug)]
pub struct FileDiff {
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
    pub is_binary: bool,
    pub hunks: Vec<DiffHunk>,
}

/// A batched diff snapshot; today always one file.
#[derive(Clone, Debug)]
pub struct DiffSnapshot {
    pub generation: u64,
    pub files: Vec<FileDiff>,
}
