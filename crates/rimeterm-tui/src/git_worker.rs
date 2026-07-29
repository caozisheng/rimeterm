//! Background Git worker.
//!
//! Owns a single OS thread that receives [`GitRequest`]s over an mpsc channel
//! and returns [`GitResponse`]s. The pane checks generations before applying.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use gix::bstr::ByteSlice;
use gix::progress::Discard;
use gix::status::{Item, UntrackedFiles};
use gix_diff::blob::{Algorithm, InternedInput, diff_with_slider_heuristics};
use tracing::{debug, warn};

use crate::git_model::{
    ChangeKind, ChangeSide, CommitSummary, DiffHunk, DiffLine, DiffLineOrigin, DiffSnapshot,
    FileDiff, GitChange, GitRequest, GitResponse, GitSnapshot, HeadSummary, UpstreamSummary,
};

/// A handle to the running worker.
pub struct GitWorker {
    request_tx: Sender<GitRequest>,
    response_rx: Receiver<GitResponse>,
}

impl GitWorker {
    /// Start the worker thread.
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<GitRequest>();
        let (resp_tx, resp_rx) = mpsc::channel::<GitResponse>();
        thread::Builder::new()
            .name("rimeterm-git-worker".into())
            .spawn(move || run(req_rx, resp_tx))
            .expect("spawn git worker");
        Self {
            request_tx: req_tx,
            response_rx: resp_rx,
        }
    }

    pub fn send(&self, request: GitRequest) {
        let _ = self.request_tx.send(request);
    }

    pub fn drain(&self) -> Vec<GitResponse> {
        let mut out = Vec::new();
        while let Ok(response) = self.response_rx.try_recv() {
            out.push(response);
        }
        out
    }
}

fn run(rx: Receiver<GitRequest>, tx: Sender<GitResponse>) {
    while let Ok(request) = rx.recv() {
        // Coalesce a burst of navigation requests. When the user
        // descends N directories rapidly the App enqueues N Snapshot
        // requests; without draining we'd compute all N serially and
        // the user would watch stale-then-stale-then-current land in
        // sequence. Drain the queue, keep only the freshest Snapshot
        // (identified by max generation), and preserve every Diff
        // request in arrival order — diffs correspond to distinct
        // files the user asked about and can't be coalesced.
        let mut latest_snapshot: Option<GitRequest> = None;
        let mut diffs: Vec<GitRequest> = Vec::new();
        let mut classify = |req: GitRequest| match req {
            GitRequest::Snapshot { generation, .. } => {
                let keep_new = match &latest_snapshot {
                    Some(GitRequest::Snapshot {
                        generation: prev, ..
                    }) => generation > *prev,
                    _ => true,
                };
                if keep_new {
                    latest_snapshot = Some(req);
                }
            }
            GitRequest::WorktreeDiff { .. } => diffs.push(req),
        };
        classify(request);
        while let Ok(more) = rx.try_recv() {
            classify(more);
        }

        if let Some(GitRequest::Snapshot { generation, cwd }) = latest_snapshot {
            let started = std::time::Instant::now();
            let snapshot = build_snapshot(generation, &cwd);
            debug!(
                generation,
                cwd = %cwd.display(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                changes = snapshot.changes.len(),
                commits = snapshot.commits.len(),
                "git snapshot built"
            );
            if tx.send(GitResponse::Snapshot(snapshot)).is_err() {
                break;
            }
        }
        for diff_req in diffs {
            if let GitRequest::WorktreeDiff {
                generation,
                repo_root,
                change,
            } = diff_req
            {
                let diff = build_worktree_diff(generation, &repo_root, &change);
                if tx.send(GitResponse::Diff(diff)).is_err() {
                    return;
                }
            }
        }
    }
}

fn build_snapshot(generation: u64, cwd: &Path) -> GitSnapshot {
    let repo = match gix::discover(cwd) {
        Ok(repo) => repo,
        Err(error) => {
            debug!(error = %error, "gix discover failed");
            return GitSnapshot::empty(generation);
        }
    };
    let repo_root = repo.workdir().map(|p| p.to_path_buf());
    let head = summarise_head(&repo);
    let upstream = summarise_upstream(&repo);
    let changes = collect_changes(&repo);
    // Cap the initial history walk at 50. Empirically each commit
    // read (find_commit + decode author/message/time) runs ~1-2 ms;
    // 200 rows meant 200-400 ms of unresponsive UI on every cwd
    // change. 50 covers the visible page for typical GitPane heights
    // (~30 rows) and a screen or two of scroll headroom; the user
    // can hit "load more" (future) to extend if needed.
    let commits = collect_commits(&repo, 50);

    GitSnapshot {
        generation,
        repo_root,
        head,
        upstream,
        changes,
        commits,
        scanned_at: std::time::Instant::now(),
    }
}

fn summarise_head(repo: &gix::Repository) -> Option<HeadSummary> {
    let head = match repo.head() {
        Ok(head) => head,
        Err(error) => {
            debug!(error = %error, "gix head lookup failed");
            return None;
        }
    };
    if let Some(name) = head.referent_name() {
        let label = name.shorten().to_str_lossy().into_owned();
        Some(HeadSummary {
            label,
            detached: false,
            unborn: head.id().is_none(),
        })
    } else if let Some(id) = head.id() {
        Some(HeadSummary {
            label: format!("detached@{}", short_hex(id.to_hex().to_string().as_str())),
            detached: true,
            unborn: false,
        })
    } else {
        Some(HeadSummary {
            label: "(unborn)".to_owned(),
            detached: false,
            unborn: true,
        })
    }
}

fn summarise_upstream(_repo: &gix::Repository) -> Option<UpstreamSummary> {
    // Ahead/behind + upstream tracking deferred to a follow-up patch — the
    // gix 0.86 API surface for these helpers changed shape and the pane
    // header still reads cleanly without them.
    None
}

fn collect_changes(repo: &gix::Repository) -> Vec<GitChange> {
    let mut changes = Vec::new();
    let platform = match repo.status(Discard) {
        Ok(p) => p.untracked_files(UntrackedFiles::Files),
        Err(error) => {
            warn!(error = %error, "gix status platform failed");
            return changes;
        }
    };
    let iter = match platform.into_iter(Vec::<gix::bstr::BString>::new()) {
        Ok(iter) => iter,
        Err(error) => {
            warn!(error = %error, "gix status iterator failed");
            return changes;
        }
    };
    for item in iter {
        let Ok(item) = item else { continue };
        match item {
            Item::TreeIndex(change) => {
                let path = change.location().to_str_lossy().into_owned();
                let kind = tree_index_kind(&change);
                changes.push(GitChange {
                    side: ChangeSide::Staged,
                    kind,
                    path: path.into(),
                    previous_path: None,
                    is_binary: false,
                });
            }
            Item::IndexWorktree(entry) => {
                let path = entry.rela_path().to_str_lossy().into_owned();
                let kind = index_worktree_kind(&entry);
                changes.push(GitChange {
                    side: ChangeSide::Worktree,
                    kind,
                    path: path.into(),
                    previous_path: None,
                    is_binary: false,
                });
            }
        }
    }
    changes
}

fn tree_index_kind(change: &gix_diff::index::Change) -> ChangeKind {
    match change {
        gix_diff::index::Change::Addition { .. } => ChangeKind::Added,
        gix_diff::index::Change::Deletion { .. } => ChangeKind::Deleted,
        gix_diff::index::Change::Modification { .. } => ChangeKind::Modified,
        gix_diff::index::Change::Rewrite { .. } => ChangeKind::Renamed,
    }
}

fn index_worktree_kind(item: &gix::status::index_worktree::Item) -> ChangeKind {
    use gix::status::index_worktree::Item as IWItem;
    match item {
        IWItem::Modification { .. } => ChangeKind::Modified,
        IWItem::DirectoryContents { .. } => ChangeKind::Untracked,
        IWItem::Rewrite { .. } => ChangeKind::Renamed,
    }
}

fn collect_commits(repo: &gix::Repository, limit: usize) -> Vec<CommitSummary> {
    let mut out = Vec::new();
    let Ok(head) = repo.head() else {
        return out;
    };
    let Some(head_id) = head.id() else {
        return out;
    };
    let walk = match repo.rev_walk([head_id]).all() {
        Ok(walk) => walk,
        Err(error) => {
            debug!(error = %error, "rev_walk failed");
            return out;
        }
    };
    for (idx, info) in walk.enumerate() {
        if idx >= limit {
            break;
        }
        let Ok(info) = info else { continue };
        let Ok(commit) = repo.find_commit(info.id) else {
            continue;
        };
        let message = commit
            .message()
            .ok()
            .map(|m| m.summary().to_str_lossy().into_owned())
            .unwrap_or_default();
        let author = commit
            .author()
            .ok()
            .and_then(|actor| actor.name.to_str().ok().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned());
        let seconds = commit.time().ok().map(|t| t.seconds).unwrap_or(0);
        let id_hex = info.id.to_hex().to_string();
        let short = short_hex(&id_hex).to_owned();
        out.push(CommitSummary {
            id: id_hex,
            short_id: short,
            summary: message,
            author,
            seconds_since_epoch: seconds,
        });
    }
    out
}

fn build_worktree_diff(generation: u64, repo_root: &Path, change: &GitChange) -> DiffSnapshot {
    let path = repo_root.join(&change.path);
    let new_content = std::fs::read_to_string(&path).unwrap_or_default();
    let old_content = if matches!(change.kind, ChangeKind::Untracked | ChangeKind::Added) {
        String::new()
    } else {
        blob_from_head(repo_root, &change.path).unwrap_or_default()
    };
    let hunks = compute_hunks(&old_content, &new_content);
    DiffSnapshot {
        generation,
        files: vec![FileDiff {
            old_path: Some(change.path.clone()),
            new_path: Some(change.path.clone()),
            is_binary: false,
            hunks,
        }],
    }
}

fn blob_from_head(repo_root: &Path, rel_path: &Path) -> Option<String> {
    let repo = gix::open(repo_root).ok()?;
    let commit = repo.head_commit().ok()?;
    let tree = commit.tree().ok()?;
    let rel = rel_path.to_string_lossy();
    let entry = tree.lookup_entry_by_path(rel.as_ref()).ok()??;
    let object = entry.object().ok()?;
    let blob = object.try_into_blob().ok()?;
    Some(String::from_utf8_lossy(&blob.data).into_owned())
}

fn compute_hunks(old: &str, new: &str) -> Vec<DiffHunk> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Histogram, &input);
    let mut out = Vec::new();
    diff.hunks().for_each(|hunk| {
        let mut lines = Vec::new();
        for &token in &input.before[hunk.before.start as usize..hunk.before.end as usize] {
            lines.push(DiffLine {
                origin: DiffLineOrigin::Removal,
                content: input.interner[token].to_owned(),
            });
        }
        for &token in &input.after[hunk.after.start as usize..hunk.after.end as usize] {
            lines.push(DiffLine {
                origin: DiffLineOrigin::Addition,
                content: input.interner[token].to_owned(),
            });
        }
        out.push(DiffHunk {
            old_start: hunk.before.start,
            old_lines: hunk.before.end - hunk.before.start,
            new_start: hunk.after.start,
            new_lines: hunk.after.end - hunk.after.start,
            lines,
        });
    });
    out
}

fn short_hex(id: &str) -> &str {
    if id.len() >= 7 { &id[..7] } else { id }
}
