//! Background Git worker.
//!
//! Owns a single OS thread that receives [`GitRequest`]s over an mpsc channel
//! and returns [`GitResponse`]s. The pane checks generations before applying.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use gix::bstr::ByteSlice;
use gix::progress::Discard;
use gix::status::{Item, UntrackedFiles};
use gix_diff::blob::{Algorithm, InternedInput, diff_with_slider_heuristics};
use tracing::{debug, warn};

use crate::git_model::{
    ChangeKind, ChangeSide, CommitDetail, CommitSummary, DiffHunk, DiffLine, DiffLineOrigin,
    DiffSnapshot, FileDiff, GitChange, GitRef, GitRequest, GitResponse, GitSnapshot, GraphCell,
    GraphEdge, HeadSummary, UpstreamSummary,
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
        // (identified by max generation), and preserve every Diff /
        // CommitDetail request in arrival order — those correspond to
        // distinct rows the user clicked and can't be coalesced.
        let mut latest_snapshot: Option<GitRequest> = None;
        let mut diffs: Vec<GitRequest> = Vec::new();
        let mut details: Vec<GitRequest> = Vec::new();
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
            GitRequest::CommitDetail { .. } => details.push(req),
        };
        classify(request);
        while let Ok(more) = rx.try_recv() {
            classify(more);
        }

        if let Some(GitRequest::Snapshot {
            generation,
            cwd,
            commit_limit,
        }) = latest_snapshot
        {
            let started = std::time::Instant::now();
            let snapshot = build_snapshot(generation, &cwd, commit_limit);
            debug!(
                generation,
                cwd = %cwd.display(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                changes = snapshot.changes.len(),
                commits = snapshot.commits.len(),
                commit_limit,
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
        for detail_req in details {
            if let GitRequest::CommitDetail {
                generation,
                repo_root,
                commit_id,
            } = detail_req
            {
                let detail = build_commit_detail(generation, &repo_root, &commit_id);
                if tx.send(GitResponse::CommitDetail(detail)).is_err() {
                    return;
                }
            }
        }
    }
}

fn build_snapshot(generation: u64, cwd: &Path, commit_limit: usize) -> GitSnapshot {
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
    // `commit_limit` starts at COMMIT_PAGE (see GitPane) and grows in
    // page-sized chunks whenever the user scrolls past the current
    // bottom. Empirically each commit read (find_commit + decode
    // author/message/time) runs ~1-2 ms; capping the window keeps
    // the initial cwd-change latency under a frame budget.
    let refs_by_commit = collect_refs(&repo);
    let head_id = resolve_head_id(&repo);
    let (commits, graph_width) = collect_commits(&repo, commit_limit, &refs_by_commit, head_id);

    GitSnapshot {
        generation,
        repo_root,
        head,
        upstream,
        changes,
        commits,
        graph_width,
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

/// Group all branches / remote-branches / tags by the commit they resolve
/// to. Non-fatal — a broken ref is logged and skipped.
fn collect_refs(repo: &gix::Repository) -> HashMap<gix::ObjectId, Vec<GitRef>> {
    let mut out: HashMap<gix::ObjectId, Vec<GitRef>> = HashMap::new();
    let platform = match repo.references() {
        Ok(p) => p,
        Err(error) => {
            debug!(error = %error, "gix references platform failed");
            return out;
        }
    };
    let iter = match platform.all() {
        Ok(iter) => iter,
        Err(error) => {
            debug!(error = %error, "gix references iter failed");
            return out;
        }
    };
    for r in iter {
        let mut r = match r {
            Ok(r) => r,
            Err(error) => {
                debug!(error = %error, "gix reference decode failed");
                continue;
            }
        };
        let name = r.name().as_bstr().to_str_lossy().into_owned();
        let category = if let Some(rest) = name.strip_prefix("refs/heads/") {
            GitRef::Branch(rest.to_owned())
        } else if let Some(rest) = name.strip_prefix("refs/remotes/") {
            // Skip `origin/HEAD` — it's a symref pointing at a real
            // branch we'll list separately.
            if rest.ends_with("/HEAD") {
                continue;
            }
            GitRef::RemoteBranch(rest.to_owned())
        } else if let Some(rest) = name.strip_prefix("refs/tags/") {
            GitRef::Tag(rest.to_owned())
        } else {
            continue;
        };
        // `peel_to_id_in_place` follows tag chains + symrefs so annotated
        // tags land on their target commit, not the tag object.
        let id = match r.peel_to_id() {
            Ok(id) => id.detach(),
            Err(error) => {
                debug!(name = %name, error = %error, "gix peel_to_id failed");
                continue;
            }
        };
        out.entry(id).or_default().push(category);
    }
    // Stable order per commit — Branch < RemoteBranch < Tag by variant
    // discriminant, then by name.
    for refs in out.values_mut() {
        refs.sort_by(|a, b| {
            match_discriminant(a)
                .cmp(&match_discriminant(b))
                .then_with(|| a.name().cmp(b.name()))
        });
    }
    out
}

fn match_discriminant(r: &GitRef) -> u8 {
    match r {
        GitRef::Branch(_) => 0,
        GitRef::RemoteBranch(_) => 1,
        GitRef::Tag(_) => 2,
    }
}

/// Resolve HEAD to a commit id (or `None` when HEAD is unborn).
fn resolve_head_id(repo: &gix::Repository) -> Option<gix::ObjectId> {
    let head = repo.head().ok()?;
    head.id().map(|id| id.detach())
}

/// Collected commit metadata + graph topology.
///
/// Rows are ordered so no commit ever precedes any of its children — the
/// child-before-parent invariant is what the two-pass port of
/// `serie::graph::calc` (MIT — see `ACKNOWLEDGEMENTS.md`) relies on to
/// place every lane correctly.
fn collect_commits(
    repo: &gix::Repository,
    limit: usize,
    refs_by_commit: &HashMap<gix::ObjectId, Vec<GitRef>>,
    head_id: Option<gix::ObjectId>,
) -> (Vec<CommitSummary>, usize) {
    let walked = match walk_commits(repo, limit) {
        Some(v) => v,
        None => return (Vec::new(), 0),
    };
    if walked.is_empty() {
        return (Vec::new(), 0);
    }
    // `gix::rev_walk` defaults to breadth-first, which can emit a parent
    // before its child when the parent is reachable via a shorter path
    // (e.g. through a merge's first-parent edge). The layout algorithm
    // needs strict topo order, so re-sort here.
    let walked = topo_sort(walked);
    let index: HashMap<gix::ObjectId, usize> =
        walked.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
    let children = build_children(&walked, &index);
    let pos_x = assign_columns(&walked, &children);
    let (edges, max_x) = build_edges(&walked, &pos_x, &children, &index);

    let commits = walked
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let is_head = head_id.map(|h| h == c.id).unwrap_or(false);
            let refs = refs_by_commit.get(&c.id).cloned().unwrap_or_default();
            CommitSummary {
                id: c.id_hex,
                short_id: c.short_id,
                summary: c.summary,
                author: c.author,
                seconds_since_epoch: c.seconds,
                graph_column: pos_x[i],
                graph_edges: edges[i].clone(),
                is_head,
                refs,
            }
        })
        .collect();
    (commits, max_x)
}

/// Reorder `walked` so every commit appears before any of its (in-walked)
/// parents. Ties are broken by *earlier* walk position, which keeps
/// newest-first commits (as gix emitted them) close to the top.
///
/// This is Kahn's algorithm on the child→parent DAG restricted to the walked
/// window. Complexity is O(n log n) via a max-heap tiebreaker; for a 50-row
/// commit window that's a few microseconds.
fn topo_sort(walked: Vec<WalkedCommit>) -> Vec<WalkedCommit> {
    use std::collections::BinaryHeap;

    if walked.len() <= 1 {
        return walked;
    }
    let orig_index: HashMap<gix::ObjectId, usize> =
        walked.iter().enumerate().map(|(i, c)| (c.id, i)).collect();

    // For each commit, count how many of its *children in the window*
    // still need to be emitted before we can emit it.
    let mut pending_children: Vec<usize> = vec![0; walked.len()];
    for c in &walked {
        for parent in &c.parents {
            if let Some(&pi) = orig_index.get(parent) {
                pending_children[pi] += 1;
            }
        }
    }

    // Max-heap on (seconds, -original_index): pop newest, or if tied,
    // whichever gix yielded earlier. `-i` gives the smaller original
    // index the larger key.
    let mut heap: BinaryHeap<(i64, i64, usize)> = BinaryHeap::new();
    for (i, c) in walked.iter().enumerate() {
        if pending_children[i] == 0 {
            heap.push((c.seconds, -(i as i64), i));
        }
    }

    let mut order: Vec<usize> = Vec::with_capacity(walked.len());
    while let Some((_, _, idx)) = heap.pop() {
        order.push(idx);
        for parent in &walked[idx].parents {
            if let Some(&pi) = orig_index.get(parent) {
                pending_children[pi] -= 1;
                if pending_children[pi] == 0 {
                    heap.push((walked[pi].seconds, -(pi as i64), pi));
                }
            }
        }
    }

    // Any commits skipped by the topo pass (impossible in a real git DAG,
    // but be defensive) get appended in original order so we never lose
    // metadata.
    if order.len() < walked.len() {
        let mut emitted = vec![false; walked.len()];
        for &i in &order {
            emitted[i] = true;
        }
        for i in 0..walked.len() {
            if !emitted[i] {
                order.push(i);
            }
        }
    }

    // Permute the vec via take() so we don't need Clone on WalkedCommit.
    let mut slots: Vec<Option<WalkedCommit>> = walked.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|i| {
            slots[i]
                .take()
                .expect("topo_sort emits each index at most once")
        })
        .collect()
}

struct WalkedCommit {
    id: gix::ObjectId,
    id_hex: String,
    short_id: String,
    summary: String,
    author: String,
    seconds: i64,
    parents: Vec<gix::ObjectId>,
}

fn walk_commits(repo: &gix::Repository, limit: usize) -> Option<Vec<WalkedCommit>> {
    let head = repo.head().ok()?;
    let head_id = head.id()?;
    let walk = match repo.rev_walk([head_id]).all() {
        Ok(w) => w,
        Err(error) => {
            debug!(error = %error, "rev_walk failed");
            return None;
        }
    };
    let mut out = Vec::with_capacity(limit.min(64));
    for (idx, info) in walk.enumerate() {
        if idx >= limit {
            break;
        }
        let Ok(info) = info else { continue };
        let Ok(commit) = repo.find_commit(info.id) else {
            continue;
        };
        let summary = commit
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
        let parents = commit.parent_ids().map(|p| p.detach()).collect();
        out.push(WalkedCommit {
            id: info.id,
            id_hex,
            short_id: short,
            summary,
            author,
            seconds,
            parents,
        });
    }
    Some(out)
}

/// Inverted parent-of edges, keyed by the position of the parent in `walked`
/// (children indexes point back into the same array). Children of commits
/// outside the walked window are simply absent, which is what the layout
/// algorithm expects.
fn build_children(
    walked: &[WalkedCommit],
    index: &HashMap<gix::ObjectId, usize>,
) -> Vec<Vec<usize>> {
    let mut children = vec![Vec::<usize>::new(); walked.len()];
    for (i, c) in walked.iter().enumerate() {
        for parent in &c.parents {
            if let Some(&pi) = index.get(parent) {
                children[pi].push(i);
            }
        }
    }
    children
}

/// Port of `serie::graph::calc::calc_commit_positions`.
///
/// `lane_state[x]` holds the row index of the (already-placed) child that is
/// currently waiting for its first-parent commit on lane `x`. When we process
/// that parent, we free the child's lane and the parent inherits it.
fn assign_columns(walked: &[WalkedCommit], children: &[Vec<usize>]) -> Vec<usize> {
    let mut pos = vec![0usize; walked.len()];
    let mut lane_state: Vec<Option<usize>> = Vec::new();

    for (i, commit) in walked.iter().enumerate() {
        // Keep only children whose *first* parent is this commit — the
        // rest are merge relationships and are drawn later as detours.
        let first_parent_children: Vec<usize> = children[i]
            .iter()
            .copied()
            .filter(|&ci| walked[ci].parents.first() == Some(&commit.id))
            .collect();

        if first_parent_children.is_empty() {
            // Nothing is waiting for us — grab the leftmost free lane.
            let x = lane_state
                .iter()
                .position(|s| s.is_none())
                .unwrap_or(lane_state.len());
            if x < lane_state.len() {
                lane_state[x] = Some(i);
            } else {
                lane_state.push(Some(i));
            }
            pos[i] = x;
        } else {
            // Free every waiting child, then take the leftmost freed lane.
            let mut min_x = lane_state.len().saturating_sub(1);
            for &child_idx in &first_parent_children {
                for (x, slot) in lane_state.iter_mut().enumerate() {
                    if *slot == Some(child_idx) {
                        *slot = None;
                        if min_x > x {
                            min_x = x;
                        }
                        break;
                    }
                }
            }
            if min_x < lane_state.len() {
                lane_state[min_x] = Some(i);
            } else {
                lane_state.push(Some(i));
            }
            pos[i] = min_x;
        }
    }
    pos
}

/// Same shape as serie's `WrappedEdge` — we keep the parent-hash tag so the
/// merge-detour pass can distinguish "our own vertical" from "somebody else's
/// vertical passing through this column".
#[derive(Clone, Copy)]
struct WrappedEdge {
    cell: GraphCell,
    owner: usize,
}

/// Port of `serie::graph::calc::calc_edges`. Two passes:
///  1. Emit direct-parent edges (commit continues on same lane, or branches
///     off horizontally on a first-parent relationship).
///  2. Emit merge edges — detour horizontally to the right when a vertical
///     from another lane would overlap our own.
fn build_edges(
    walked: &[WalkedCommit],
    pos_x: &[usize],
    children: &[Vec<usize>],
    index: &HashMap<gix::ObjectId, usize>,
) -> (Vec<Vec<GraphCell>>, usize) {
    let mut max_pos_x = 0usize;
    let mut edges: Vec<Vec<WrappedEdge>> = vec![Vec::new(); walked.len()];

    // Pass 1: commit / branch edges + fall-off-window vertical for the first
    // parent when the walk was truncated.
    for (i, commit) in walked.iter().enumerate() {
        let x = pos_x[i];
        max_pos_x = max_pos_x.max(x);

        for &child_idx in &children[i] {
            let cx = pos_x[child_idx];
            let cy = child_idx;
            let py = i;
            if x == cx {
                // Straight vertical from child to this commit.
                edges[py].push(wrap(GraphEdge::Up, x, x, i));
                for y in (cy + 1)..py {
                    edges[y].push(wrap(GraphEdge::Vertical, x, x, i));
                }
                edges[cy].push(wrap(GraphEdge::Down, x, x, i));
            } else {
                let child_first_parent = walked[child_idx].parents.first();
                if child_first_parent == Some(&commit.id) {
                    // Branch: child's first parent is us but on a different lane.
                    if x < cx {
                        edges[py].push(wrap(GraphEdge::Right, x, cx, i));
                        for xi in (x + 1)..cx {
                            edges[py].push(wrap(GraphEdge::Horizontal, xi, cx, i));
                        }
                        edges[py].push(wrap(GraphEdge::RightBottom, cx, cx, i));
                    } else {
                        edges[py].push(wrap(GraphEdge::Left, x, cx, i));
                        for xi in (cx + 1)..x {
                            edges[py].push(wrap(GraphEdge::Horizontal, xi, cx, i));
                        }
                        edges[py].push(wrap(GraphEdge::LeftBottom, cx, cx, i));
                    }
                    for y in (cy + 1)..py {
                        edges[y].push(wrap(GraphEdge::Vertical, cx, cx, i));
                    }
                    edges[cy].push(wrap(GraphEdge::Down, cx, cx, i));
                } else {
                    // Merge — handled by pass 2 so we can detour cleanly.
                }
            }
        }

        // If our first parent is *outside* the walked window, drop a
        // vertical dangling downward so the user sees the history continues.
        if let Some(first_parent) = commit.parents.first()
            && !index.contains_key(first_parent)
        {
            edges[i].push(wrap(GraphEdge::Down, x, x, i));
            for y in (i + 1)..walked.len() {
                edges[y].push(wrap(GraphEdge::Vertical, x, x, i));
            }
        }
    }

    // Pass 2: merges. For each child that lists us as a *non-first* parent,
    // draw an edge from our dot up to their dot, detouring around any lane
    // that would collide.
    for (i, commit) in walked.iter().enumerate() {
        let x = pos_x[i];
        for &child_idx in &children[i] {
            let cx = pos_x[child_idx];
            let cy = child_idx;
            let py = i;
            if x == cx {
                continue;
            }
            let child_first_parent = walked[child_idx].parents.first();
            if child_first_parent == Some(&commit.id) {
                continue;
            }
            // Try to place a vertical at `new_pos_x`, sliding right until
            // no other commit dot or vertical owned by a different commit
            // blocks us.
            let mut new_pos_x = x;
            let mut overlap = false;

            let mut needs_check = true;
            // Fast-path: if the region above is empty of same-column dots
            // and same-column verticals, the naive `Up` from our dot is fine.
            for y in (cy + 1)..py {
                if pos_x[y] == new_pos_x {
                    needs_check = false;
                    break;
                }
                if edges[y]
                    .iter()
                    .filter(|e| e.cell.column == x)
                    .filter(|e| matches!(e.cell.edge, GraphEdge::Vertical))
                    .any(|e| e.owner != i)
                {
                    needs_check = false;
                    break;
                }
            }

            if !needs_check {
                for y in (cy + 1)..py {
                    let p = pos_x[y];
                    if p == new_pos_x {
                        overlap = true;
                        if new_pos_x < p + 1 {
                            new_pos_x = p + 1;
                        }
                    }
                    for edge in &edges[y] {
                        if edge.cell.column >= new_pos_x
                            && edge.owner != i
                            && matches!(edge.cell.edge, GraphEdge::Vertical)
                        {
                            overlap = true;
                            if new_pos_x < edge.cell.column + 1 {
                                new_pos_x = edge.cell.column + 1;
                            }
                        }
                    }
                }
            }

            if overlap {
                // Detour: right along our row, up to child row, back left.
                edges[py].push(wrap(GraphEdge::Right, x, x, i));
                for xi in (x + 1)..new_pos_x {
                    edges[py].push(wrap(GraphEdge::Horizontal, xi, x, i));
                }
                edges[py].push(wrap(GraphEdge::RightBottom, new_pos_x, x, i));
                for y in (cy + 1)..py {
                    edges[y].push(wrap(GraphEdge::Vertical, new_pos_x, x, i));
                }
                edges[cy].push(wrap(GraphEdge::RightTop, new_pos_x, x, i));
                for xi in (cx + 1)..new_pos_x {
                    edges[cy].push(wrap(GraphEdge::Horizontal, xi, x, i));
                }
                edges[cy].push(wrap(GraphEdge::Right, cx, x, i));
                max_pos_x = max_pos_x.max(new_pos_x);
            } else {
                // Straight-up then hook into the child's row.
                edges[py].push(wrap(GraphEdge::Up, x, x, i));
                for y in (cy + 1)..py {
                    edges[y].push(wrap(GraphEdge::Vertical, x, x, i));
                }
                if x < cx {
                    edges[cy].push(wrap(GraphEdge::LeftTop, x, x, i));
                    for xi in (x + 1)..cx {
                        edges[cy].push(wrap(GraphEdge::Horizontal, xi, x, i));
                    }
                    edges[cy].push(wrap(GraphEdge::Left, cx, x, i));
                } else {
                    edges[cy].push(wrap(GraphEdge::RightTop, x, x, i));
                    for xi in (cx + 1)..x {
                        edges[cy].push(wrap(GraphEdge::Horizontal, xi, x, i));
                    }
                    edges[cy].push(wrap(GraphEdge::Right, cx, x, i));
                }
            }
        }
        max_pos_x = max_pos_x.max(x);
    }

    // Sort + dedupe each row identically to serie so the render order is
    // deterministic and repeated edges (same edge type at same column
    // owned by same lane) collapse.
    let out: Vec<Vec<GraphCell>> = edges
        .into_iter()
        .map(|row| {
            let mut cells: Vec<GraphCell> = row.into_iter().map(|w| w.cell).collect();
            cells.sort_by_key(|c| (c.lane, c.column, c.edge));
            cells.dedup();
            cells
        })
        .collect();
    (out, max_pos_x)
}

fn wrap(edge: GraphEdge, column: usize, lane: usize, owner: usize) -> WrappedEdge {
    WrappedEdge {
        cell: GraphCell { edge, column, lane },
        owner,
    }
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

/// Resolve one commit's full detail — metadata + per-file diff against the
/// commit's first parent (or the empty tree for a root commit).
fn build_commit_detail(generation: u64, repo_root: &Path, commit_id: &str) -> CommitDetail {
    let mut empty = CommitDetail {
        generation,
        id: commit_id.to_owned(),
        short_id: short_hex(commit_id).to_owned(),
        author_name: String::new(),
        author_email: String::new(),
        author_seconds: 0,
        committer_name: String::new(),
        committer_email: String::new(),
        committer_seconds: 0,
        subject: String::new(),
        body: String::new(),
        parents: Vec::new(),
        refs: Vec::new(),
        files: Vec::new(),
    };
    let repo = match gix::open(repo_root) {
        Ok(repo) => repo,
        Err(error) => {
            debug!(error = %error, "gix open failed for commit detail");
            return empty;
        }
    };
    let oid: gix::ObjectId = match gix::ObjectId::from_hex(commit_id.as_bytes()) {
        Ok(id) => id,
        Err(error) => {
            warn!(error = %error, commit_id, "invalid commit id");
            return empty;
        }
    };
    let commit = match repo.find_commit(oid) {
        Ok(c) => c,
        Err(error) => {
            debug!(error = %error, commit_id, "gix find_commit failed");
            return empty;
        }
    };

    // Metadata.
    if let Ok(actor) = commit.author() {
        empty.author_name = actor.name.to_str_lossy().into_owned();
        empty.author_email = actor.email.to_str_lossy().into_owned();
    }
    if let Ok(t) = commit.time() {
        empty.author_seconds = t.seconds;
    }
    if let Ok(actor) = commit.committer() {
        empty.committer_name = actor.name.to_str_lossy().into_owned();
        empty.committer_email = actor.email.to_str_lossy().into_owned();
    }
    empty.committer_seconds = empty.author_seconds; // gix exposes only commit time; fine.
    if let Ok(msg) = commit.message() {
        empty.subject = msg.summary().to_str_lossy().into_owned();
        if let Some(body) = msg.body {
            empty.body = body.to_str_lossy().into_owned();
        }
    }
    empty.parents = commit
        .parent_ids()
        .map(|p| p.detach().to_hex().to_string())
        .collect();
    let refs_by_commit = collect_refs(&repo);
    empty.refs = refs_by_commit.get(&oid).cloned().unwrap_or_default();

    // Tree diff vs. first parent (or empty tree for a root commit).
    empty.files = tree_diff_files(&repo, &commit);
    empty
}

/// Diff `commit`'s tree against its first-parent tree (empty for roots).
/// Emits one `FileDiff` per changed path with unified-format hunks.
fn tree_diff_files(repo: &gix::Repository, commit: &gix::Commit<'_>) -> Vec<FileDiff> {
    let new_tree = match commit.tree() {
        Ok(t) => t,
        Err(error) => {
            debug!(error = %error, "gix commit.tree failed");
            return Vec::new();
        }
    };
    let old_tree = commit
        .parent_ids()
        .next()
        .and_then(|pid| repo.find_commit(pid.detach()).ok())
        .and_then(|pc| pc.tree().ok());

    // gix's tree-diff API needs an old tree; for root commits fall back
    // to an empty tree by iterating the new tree's entries as additions.
    let Some(old_tree) = old_tree else {
        return root_commit_files(repo, &new_tree);
    };

    let mut files: Vec<FileDiff> = Vec::new();
    let mut platform = match old_tree.changes() {
        Ok(p) => p,
        Err(error) => {
            debug!(error = %error, "gix tree changes platform failed");
            return Vec::new();
        }
    };
    let _ = platform.for_each_to_obtain_tree(&new_tree, |change| {
        use gix::object::tree::diff::Change;
        let (old_path, new_path, old_blob, new_blob) = match &change {
            Change::Addition { location, id, .. } => (
                None,
                Some(location.to_path_lossy().to_path_buf()),
                None,
                blob_text(repo, id.detach()),
            ),
            Change::Deletion { location, id, .. } => (
                Some(location.to_path_lossy().to_path_buf()),
                None,
                blob_text(repo, id.detach()),
                None,
            ),
            Change::Modification {
                location,
                previous_id,
                id,
                ..
            } => (
                Some(location.to_path_lossy().to_path_buf()),
                Some(location.to_path_lossy().to_path_buf()),
                blob_text(repo, previous_id.detach()),
                blob_text(repo, id.detach()),
            ),
            Change::Rewrite {
                source_location,
                location,
                source_id,
                id,
                ..
            } => (
                Some(source_location.to_path_lossy().to_path_buf()),
                Some(location.to_path_lossy().to_path_buf()),
                blob_text(repo, source_id.detach()),
                blob_text(repo, id.detach()),
            ),
        };
        let old = old_blob.unwrap_or_default();
        let new = new_blob.unwrap_or_default();
        let hunks = compute_hunks(&old, &new);
        files.push(FileDiff {
            old_path,
            new_path,
            is_binary: false,
            hunks,
        });
        Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::<()>::Continue(()))
    });
    files
}

/// Root commit fallback: treat every blob under the new tree as an addition
/// against the empty tree.
fn root_commit_files(repo: &gix::Repository, new_tree: &gix::Tree<'_>) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let iter = match new_tree.traverse().breadthfirst.files() {
        Ok(v) => v,
        Err(error) => {
            debug!(error = %error, "gix tree traverse failed for root commit");
            return files;
        }
    };
    for entry in iter {
        let path = entry.filepath.to_str_lossy().to_string();
        let new = blob_text(repo, entry.oid).unwrap_or_default();
        let hunks = compute_hunks("", &new);
        files.push(FileDiff {
            old_path: None,
            new_path: Some(path.into()),
            is_binary: false,
            hunks,
        });
    }
    files
}

/// Load a blob by id as text — returns `None` when the object isn't a blob.
fn blob_text(repo: &gix::Repository, id: gix::ObjectId) -> Option<String> {
    let object = repo.find_object(id).ok()?;
    let blob = object.try_into_blob().ok()?;
    Some(String::from_utf8_lossy(&blob.data).into_owned())
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

#[cfg(test)]
mod graph_tests {
    //! Direct exercises of the commit-graph algorithm ported from
    //! `serie::graph::calc`. Tests avoid touching a real repository — they
    //! synthesise `WalkedCommit` values whose `ObjectId`s are just the
    //! commit's `usize` index packed into 20 bytes.

    use super::*;
    use crate::git_model::GraphEdge;

    fn oid(i: usize) -> gix::ObjectId {
        let mut bytes = [0u8; 20];
        bytes[..std::mem::size_of::<usize>()].copy_from_slice(&i.to_le_bytes());
        // Sha1 flavour for tests — collect_commits uses whatever gix hands
        // it; the algorithm only cares about equality, not the hash kind.
        gix::ObjectId::from(bytes)
    }

    fn mk(i: usize, parents: &[usize]) -> WalkedCommit {
        WalkedCommit {
            id: oid(i),
            id_hex: format!("{i:040}"),
            short_id: format!("{i:07}"),
            summary: format!("commit {i}"),
            author: "test".into(),
            seconds: 0,
            parents: parents.iter().copied().map(oid).collect(),
        }
    }

    /// Build the `walked → index` map and drive the whole pipeline.
    fn run(walked: &[WalkedCommit]) -> (Vec<usize>, Vec<Vec<GraphCell>>, usize) {
        let index: HashMap<gix::ObjectId, usize> =
            walked.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
        let children = build_children(walked, &index);
        let pos = assign_columns(walked, &children);
        let (edges, max_x) = build_edges(walked, &pos, &children, &index);
        (pos, edges, max_x)
    }

    #[test]
    fn linear_history_stays_on_lane_zero() {
        // 0 → 1 → 2 → 3 (newest first, each pointing to its parent)
        let walked = vec![mk(0, &[1]), mk(1, &[2]), mk(2, &[3]), mk(3, &[])];
        let (pos, edges, max_x) = run(&walked);

        assert_eq!(pos, vec![0, 0, 0, 0], "linear history is single-lane");
        assert_eq!(max_x, 0);
        // Row 0 (HEAD): only Down toward row 1
        assert_eq!(
            edges[0],
            vec![GraphCell {
                edge: GraphEdge::Down,
                column: 0,
                lane: 0,
            }]
        );
        // Row 1: Up owned by row-0's edge + Down owned by row-1's edge.
        // `lane` is `associated_line_pos_x` in serie's terminology — for a
        // linear history all edges share column 0 so all lanes are 0.
        assert!(edges[1].contains(&GraphCell {
            edge: GraphEdge::Up,
            column: 0,
            lane: 0
        }));
        assert!(edges[1].contains(&GraphCell {
            edge: GraphEdge::Down,
            column: 0,
            lane: 0
        }));
        // Row 3 (root, oldest): only Up toward row 2
        assert_eq!(
            edges[3],
            vec![GraphCell {
                edge: GraphEdge::Up,
                column: 0,
                lane: 0,
            }]
        );
    }

    #[test]
    fn branch_off_shifts_child_to_new_lane() {
        // Topology (newest first):
        //   0: feature tip (parent = 2)
        //   1: main tip    (parent = 2)   ← merge base branch
        //   2: shared root
        //
        // Row 0 grabs lane 0; row 1 (main) has no in-window child so it
        // takes lane 1; row 2 has *two* first-parent children (0 on lane 0,
        // 1 on lane 1) so it settles on the leftmost freed lane, which is 0.
        let walked = vec![mk(0, &[2]), mk(1, &[2]), mk(2, &[])];
        let (pos, edges, max_x) = run(&walked);

        assert_eq!(pos, vec![0, 1, 0]);
        assert_eq!(max_x, 1);

        // Row 2 must draw a branch from the root (lane 0) up to child on
        // lane 1: `Right` on column 0 + `RightBottom` corner on column 1.
        assert!(
            edges[2]
                .iter()
                .any(|c| c.edge == GraphEdge::Right && c.column == 0),
            "expected Right at (0,2), got {:?}",
            edges[2]
        );
        assert!(
            edges[2]
                .iter()
                .any(|c| c.edge == GraphEdge::RightBottom && c.column == 1),
            "expected RightBottom at (1,2), got {:?}",
            edges[2]
        );
    }

    #[test]
    fn merge_commit_draws_second_parent_hook() {
        // Topology (newest first, chrono-reverse walk order):
        //   0: merge commit, first parent = 1 (main), second parent = 2 (feature)
        //   1: main tip     (parent = 3)
        //   2: feature tip  (parent = 3)
        //   3: shared root
        //
        // Layout:
        //   row 0 → lane 0 (merge)
        //   row 1 → lane 0 (main, inherits from merge because it *is* first parent)
        //   row 2 → lane 1 (feature, no free lane inherited)
        //   row 3 → lane 0 (root, leftmost freed)
        //
        // Visual expected (● = commit dot, one column per lane):
        //   row 0:  ●╮        merge starts right-hook toward feature
        //   row 1:  ●│        main continues, merge edge passes through col 1
        //   row 2:   ●        feature tip on lane 1
        //   row 3:  ●╯        root, feature branch corner closes back
        let walked = vec![mk(0, &[1, 2]), mk(1, &[3]), mk(2, &[3]), mk(3, &[])];
        let (pos, edges, max_x) = run(&walked);

        assert_eq!(pos, vec![0, 0, 1, 0]);
        assert!(max_x >= 1);

        // Row 2 (the parent side of the merge) drops an Up stub at
        // col 1 owned by the merge's lane (associated_line_pos_x = 1).
        assert!(
            edges[2]
                .iter()
                .any(|c| c.edge == GraphEdge::Up && c.column == 1 && c.lane == 1),
            "expected Up stub at (1, row 2), got {:?}",
            edges[2]
        );
        // Row 1 carries the vertical between them.
        assert!(
            edges[1]
                .iter()
                .any(|c| c.edge == GraphEdge::Vertical && c.column == 1 && c.lane == 1),
            "expected Vertical at (1, row 1), got {:?}",
            edges[1]
        );
        // Row 0 (the merge commit) hooks LEFT from the incoming vertical
        // back to the dot on lane 0: RightTop corner at col 1 + Right
        // stub at col 0 (hidden under the dot).
        assert!(
            edges[0]
                .iter()
                .any(|c| c.edge == GraphEdge::RightTop && c.column == 1 && c.lane == 1),
            "expected RightTop at (1, row 0), got {:?}",
            edges[0]
        );
        assert!(
            edges[0]
                .iter()
                .any(|c| c.edge == GraphEdge::Right && c.column == 0 && c.lane == 1),
            "expected Right at (0, row 0), got {:?}",
            edges[0]
        );
    }

    #[test]
    fn first_parent_outside_window_drops_vertical() {
        // Only the tip is in the window; its first parent doesn't
        // appear so we expect a dangling Down from row 0 continuing as
        // Vertical on all subsequent rows.
        let walked = vec![mk(0, &[42]), mk(1, &[])];
        let (pos, edges, _max_x) = run(&walked);
        assert_eq!(pos, vec![0, 1], "two roots take separate lanes");

        // Row 0 (HEAD-with-out-of-window-parent) must contain a Down at
        // column 0 owned by itself (lane 0).
        assert!(
            edges[0]
                .iter()
                .any(|c| c.edge == GraphEdge::Down && c.column == 0 && c.lane == 0),
            "expected dangling Down at (0,0), got {:?}",
            edges[0]
        );
        // Row 1 must carry that Vertical through.
        assert!(
            edges[1]
                .iter()
                .any(|c| c.edge == GraphEdge::Vertical && c.column == 0 && c.lane == 0),
            "expected dangling Vertical at (0,1), got {:?}",
            edges[1]
        );
    }

    #[test]
    fn detour_avoids_running_over_another_lane() {
        // Merge that has to detour: main lane 1 sits between the merge
        // and its side parent, so the merge edge must route right of it.
        //   0: merge, first parent = 2, second parent = 4    → lane 0
        //   1: main tip   (parent = 2)                        → lane 1
        //   2: main mid   (parent = 3)                        → lane 0 (inherits merge)
        //   3: main old   (parent = 5)                        → lane 0
        //   4: feature tip (parent = 5)                       → lane 1 (grabbed after main tip freed)
        //   5: root                                            → lane 0
        //
        // Row 4 (parent side of merge, lane 1) → Row 0 (child side, lane 0).
        // The vertical would want to run on lane 1 (its start column) but
        // row 1 sits on lane 1, so the algorithm slides to lane 2 (max_x).
        //
        // Expected shape on the affected rows:
        //   row 0 (child):  Right(0,l=1) Horiz(1,l=1) RightTop(2,l=1)   ← ●─╮
        //   rows 1..4:      Vertical(2, l=1)                            ←    │
        //   row 4 (parent): Right(1,l=1) [Horiz none] RightBottom(2,l=1) ← _●╯
        let walked = vec![
            mk(0, &[2, 4]),
            mk(1, &[2]),
            mk(2, &[3]),
            mk(3, &[5]),
            mk(4, &[5]),
            mk(5, &[]),
        ];
        let (pos, edges, max_x) = run(&walked);

        assert_eq!(pos, vec![0, 1, 0, 0, 1, 0]);
        assert!(max_x >= 2, "detour must widen the graph beyond lane 1");
        let detour_col = max_x;
        let detour_lane = 1; // the merge's start column (parent's column)

        // Child-row corner (RightTop) + horizontal + right stub.
        assert!(
            edges[0].iter().any(|c| {
                c.edge == GraphEdge::RightTop && c.column == detour_col && c.lane == detour_lane
            }),
            "expected RightTop at (col={detour_col}, row 0), got {:?}",
            edges[0]
        );
        assert!(
            edges[0]
                .iter()
                .any(|c| c.edge == GraphEdge::Right && c.column == 0 && c.lane == detour_lane),
            "expected Right at (0, row 0), got {:?}",
            edges[0]
        );

        // Parent-row corner (RightBottom) on row 4.
        assert!(
            edges[4].iter().any(|c| {
                c.edge == GraphEdge::RightBottom && c.column == detour_col && c.lane == detour_lane
            }),
            "expected RightBottom at (col={detour_col}, row 4), got {:?}",
            edges[4]
        );

        // Verticals in between at the detour column.
        for row in 1..=3 {
            assert!(
                edges[row].iter().any(|c| {
                    c.edge == GraphEdge::Vertical && c.column == detour_col && c.lane == detour_lane
                }),
                "expected Vertical at (col={detour_col}, row {row}), got {:?}",
                edges[row]
            );
        }
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let (pos, edges, max_x) = run(&[]);
        assert!(pos.is_empty());
        assert!(edges.is_empty());
        assert_eq!(max_x, 0);
    }

    /// Manual smoke test: dump the first N commits of the *current*
    /// repository as an ASCII graph so a human can eyeball that the
    /// two-pass port yields sensible output on a real history.
    ///
    /// Ignored by default — CI shouldn't depend on a git checkout.
    /// Invoke with `cargo test -p rimeterm-tui -- --ignored
    /// --nocapture dump_this_repos_graph`.
    #[test]
    #[ignore]
    fn dump_this_repos_graph() {
        let repo = gix::discover(env!("CARGO_MANIFEST_DIR")).expect("open repo");
        let refs_by_commit = super::collect_refs(&repo);
        let head_id = super::resolve_head_id(&repo);
        let (commits, width) = super::collect_commits(&repo, 30, &refs_by_commit, head_id);
        println!("graph_width = {width}");
        for c in &commits {
            let mut row = vec![' '; width + 1];
            for cell in &c.graph_edges {
                if cell.column < row.len() {
                    row[cell.column] = cell.edge.glyph();
                }
            }
            if c.graph_column < row.len() {
                row[c.graph_column] = '●';
            }
            let graph: String = row.into_iter().collect();
            let head = if c.is_head { " HEAD" } else { "" };
            let refs: String = c
                .refs
                .iter()
                .map(|r| match r {
                    crate::git_model::GitRef::Branch(n) => format!(" [{n}]"),
                    crate::git_model::GitRef::RemoteBranch(n) => format!(" [{n}]"),
                    crate::git_model::GitRef::Tag(n) => format!(" [tag: {n}]"),
                })
                .collect();
            println!("{graph}  {}{head}{refs}  {}", &c.short_id, c.summary);
        }
    }
}
