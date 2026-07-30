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

/// One glyph in the commit-graph column of the Commits list.
///
/// The names mirror `serie`'s `EdgeType` (MIT — see ACKNOWLEDGEMENTS) so that
/// the port of `calc_edges` can be read side-by-side with the upstream; the
/// glyphs are what we actually paint on the terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum GraphEdge {
    Vertical,    // │
    Horizontal,  // ─
    Up,          // ╵
    Down,        // ╷
    Left,        // ╴
    Right,       // ╶
    RightTop,    // ╮
    RightBottom, // ╯
    LeftTop,     // ╭
    LeftBottom,  // ╰
}

impl GraphEdge {
    /// The Unicode glyph rendered for this edge.
    pub fn glyph(self) -> char {
        match self {
            GraphEdge::Vertical => '│',
            GraphEdge::Horizontal => '─',
            GraphEdge::Up => '╵',
            GraphEdge::Down => '╷',
            GraphEdge::Left => '╴',
            GraphEdge::Right => '╶',
            GraphEdge::RightTop => '╮',
            GraphEdge::RightBottom => '╯',
            GraphEdge::LeftTop => '╭',
            GraphEdge::LeftBottom => '╰',
        }
    }
}

/// One edge segment drawn on a single commit's row.
///
/// `column` is the horizontal cell (0-based). `lane` is the column of the
/// commit that *owns* the edge — used to pick a stable color so a branch
/// line keeps the same hue across the rows it traverses.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct GraphCell {
    pub edge: GraphEdge,
    pub column: usize,
    pub lane: usize,
}

/// A named reference pinned to a commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitRef {
    /// Local branch, e.g. `main`.
    Branch(String),
    /// Remote-tracking branch, e.g. `origin/main`.
    RemoteBranch(String),
    /// Tag, e.g. `v1.0` — annotated or lightweight, we don't distinguish.
    Tag(String),
}

impl GitRef {
    pub fn name(&self) -> &str {
        match self {
            GitRef::Branch(name) | GitRef::RemoteBranch(name) | GitRef::Tag(name) => name,
        }
    }
}

/// Compact commit info suitable for a scrolling history list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitSummary {
    pub id: String,
    pub short_id: String,
    pub summary: String,
    pub author: String,
    pub seconds_since_epoch: i64,
    /// Column of this commit's dot in the graph grid (0-based).
    pub graph_column: usize,
    /// Edges drawn on this commit's row.
    ///
    /// Painted in vector order; the commit dot is rendered *after* and
    /// overwrites `graph_column`.
    pub graph_edges: Vec<GraphCell>,
    /// True when this commit is the current `HEAD`.
    pub is_head: bool,
    /// Refs (branches / remote-branches / tags) pinned to this commit.
    ///
    /// Sorted by [`GitRef`]'s natural derive order (Branch < RemoteBranch
    /// < Tag when the enum variants are compared); refs badges are
    /// rendered in this order on the commits list.
    pub refs: Vec<GitRef>,
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
    /// Widest column index touched by any commit dot or edge — the
    /// renderer draws `graph_width + 1` graph cells per row.
    pub graph_width: usize,
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
            graph_width: 0,
            scanned_at: Instant::now(),
        }
    }
}

/// A worker request, tagged by generation so stale replies are dropped.
#[derive(Clone, Debug)]
pub enum GitRequest {
    /// Discover a repository from the supplied directory and produce a snapshot.
    Snapshot {
        generation: u64,
        cwd: PathBuf,
        /// Maximum number of commits to walk. Grows as the user scrolls
        /// past the current window's bottom.
        commit_limit: usize,
    },
    /// Compute a diff against a specific worktree change.
    WorktreeDiff {
        generation: u64,
        repo_root: PathBuf,
        change: GitChange,
    },
    /// Compute a full detail view (metadata + per-file diff) for a
    /// specific commit.
    CommitDetail {
        generation: u64,
        repo_root: PathBuf,
        commit_id: String,
    },
}

/// Worker response, wrapped in the completing generation.
#[derive(Debug)]
pub enum GitResponse {
    Snapshot(GitSnapshot),
    Diff(DiffSnapshot),
    CommitDetail(CommitDetail),
}

impl GitResponse {
    pub fn generation(&self) -> u64 {
        match self {
            GitResponse::Snapshot(snap) => snap.generation,
            GitResponse::Diff(diff) => diff.generation,
            GitResponse::CommitDetail(detail) => detail.generation,
        }
    }
}

/// One resolved commit — everything the detail overlay needs.
#[derive(Clone, Debug)]
pub struct CommitDetail {
    pub generation: u64,
    pub id: String,
    pub short_id: String,
    pub author_name: String,
    pub author_email: String,
    pub author_seconds: i64,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_seconds: i64,
    /// Full first-line subject.
    pub subject: String,
    /// Message body without the subject line; may be empty.
    pub body: String,
    pub parents: Vec<String>,
    pub refs: Vec<GitRef>,
    /// One `FileDiff` per changed path — reuses the same shape as
    /// worktree diffs so the render code can call the existing hunk
    /// path.
    pub files: Vec<FileDiff>,
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
