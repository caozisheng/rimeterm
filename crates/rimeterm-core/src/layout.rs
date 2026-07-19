//! Layout tree — the split/tabs skeleton the frontend renders.
//!
//! §2.3 and §19.2 of the design doc. In M1 we implement enough of the tree to
//! host: `Split (2 children) | TabGroup (N tabs, one active) | Leaf (a Pane)`.
//! The tree carries **ids and ratios**, not pane trait objects — mapping id →
//! provider is the caller's job (kept out of core to keep this crate stable).
//!
//! ## Invariants encoded here
//!
//! - A `Split` MUST have `ratio.len() == children.len()` (`assert` at ctor).
//! - Ratios are normalized so they sum to 1.0 (guarantees no drift).
//! - `TabGroup::active` is always `< members.len()` (kept via `try_add` / `try_close`).
//! - Leaf ids are unique per tree (checked in `LayoutTree::new`).

use std::collections::HashSet;

use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::pane::PaneId;
use crate::tabs::{TabGroup, TabGroupId};

/// A node in the layout tree.
#[derive(Debug)]
pub enum LayoutNode {
    /// Horizontal or vertical split with 2+ children and matching ratios.
    Split {
        direction: Direction,
        /// Normalized ratios (sum to 1.0). `ratios.len() == children.len()`.
        ratios: Vec<f32>,
        children: Vec<LayoutNode>,
    },
    /// A tab group. Only its active member is rendered; other members stay in
    /// state (running / suspended) but don't consume screen space.
    Tabs(TabGroup),
    /// Single pane leaf.
    Leaf(PaneId),
}

impl LayoutNode {
    pub fn split(direction: Direction, ratios: Vec<f32>, children: Vec<LayoutNode>) -> Self {
        assert!(
            !children.is_empty(),
            "split must have at least one child (use Leaf for singletons)"
        );
        assert_eq!(
            ratios.len(),
            children.len(),
            "split ratios and children length must match"
        );
        let sum: f32 = ratios.iter().sum();
        assert!(sum > f32::EPSILON, "split ratios must sum > 0");
        let ratios = ratios.into_iter().map(|r| r / sum).collect();
        Self::Split {
            direction,
            ratios,
            children,
        }
    }

    pub fn tabs(group: TabGroup) -> Self {
        Self::Tabs(group)
    }

    pub fn leaf(pane: PaneId) -> Self {
        Self::Leaf(pane)
    }

    /// Return the pane id that should be **rendered** in this node's rect.
    /// For a tab group that's the active member; for a leaf it's itself; for a
    /// split it's `None` (splits don't paint directly).
    pub fn painted_pane(&self) -> Option<PaneId> {
        match self {
            LayoutNode::Leaf(id) => Some(*id),
            LayoutNode::Tabs(group) => group.active_pane(),
            LayoutNode::Split { .. } => None,
        }
    }
}

/// The full layout tree owned by the app.
#[derive(Debug)]
pub struct LayoutTree {
    root: LayoutNode,
}

impl LayoutTree {
    /// Build a tree, checking pane-id uniqueness across all leaves + tab members.
    pub fn new(root: LayoutNode) -> Result<Self, LayoutError> {
        let mut seen = HashSet::new();
        check_unique(&root, &mut seen)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &LayoutNode {
        &self.root
    }

    pub fn root_mut(&mut self) -> &mut LayoutNode {
        &mut self.root
    }

    /// Compute rects for every pane visible right now (leaves + active tab of
    /// each `Tabs` node). Returned in painter order (parent → children).
    pub fn compute_rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        walk(&self.root, area, &mut out);
        out
    }

    /// Locate the [`TabGroup`] with the given id.
    pub fn find_tab_group(&self, id: TabGroupId) -> Option<&TabGroup> {
        find_tab_group(&self.root, id)
    }

    pub fn find_tab_group_mut(&mut self, id: TabGroupId) -> Option<&mut TabGroup> {
        find_tab_group_mut(&mut self.root, id)
    }

    /// Iterate every [`TabGroup`] in the tree, in DFS order.
    pub fn tab_groups(&self) -> Vec<&TabGroup> {
        let mut out = Vec::new();
        collect_tab_groups(&self.root, &mut out);
        out
    }

    /// Enumerate every visible divider (seam between siblings inside a Split).
    /// v0.1 uses the current `area` to compute rects; the caller is expected
    /// to call this once per frame after layout, and cache within the frame.
    pub fn dividers(&self, area: Rect) -> Vec<Divider> {
        let mut out = Vec::new();
        collect_dividers(&self.root, area, SplitPath::root(), &mut out);
        out
    }

    /// Read the current ratios of the split at `path`. Returns an owned copy.
    pub fn ratios_at(&self, path: &SplitPath) -> Option<Vec<f32>> {
        find_split(&self.root, path).and_then(|node| match node {
            LayoutNode::Split { ratios, .. } => Some(ratios.clone()),
            _ => None,
        })
    }

    /// Adjust the seam between children `boundary` and `boundary + 1` inside
    /// the split at `path` by `delta` (fraction of the split's extent). The
    /// call rejects if either neighbor would drop below its normalized floor.
    ///
    /// `floors` is a slice of the same length as the split's children,
    /// expressed as fractions of the parent extent (e.g. `24 cols / 120 cols`
    /// → `0.20`). Pass zeros to disable floor checks.
    pub fn adjust_ratio(
        &mut self,
        path: &SplitPath,
        boundary: usize,
        delta: f32,
        floors: &[f32],
    ) -> Result<(), RatioError> {
        let node = find_split_mut(&mut self.root, path)
            .ok_or_else(|| RatioError::NoSplit(path.0.clone()))?;
        let LayoutNode::Split { ratios, .. } = node else {
            return Err(RatioError::NoSplit(path.0.clone()));
        };
        let n = ratios.len();
        if boundary + 1 >= n {
            return Err(RatioError::BoundaryOutOfRange {
                boundary,
                children: n,
            });
        }
        // Only move the two children on either side of the seam; everything
        // else stays. Preserves the sum invariant so no renormalize needed.
        let a = ratios[boundary] + delta;
        let b = ratios[boundary + 1] - delta;
        let floor_a = floors.get(boundary).copied().unwrap_or(0.0);
        let floor_b = floors.get(boundary + 1).copied().unwrap_or(0.0);
        if a < floor_a || b < floor_b {
            return Err(RatioError::BelowMinSize);
        }
        ratios[boundary] = a;
        ratios[boundary + 1] = b;
        Ok(())
    }

    /// Overwrite the ratios of the split at `path` with `new_ratios`.
    /// Automatically re-normalizes to sum 1.0.
    pub fn set_ratios(&mut self, path: &SplitPath, new_ratios: Vec<f32>) -> Result<(), RatioError> {
        let node = find_split_mut(&mut self.root, path)
            .ok_or_else(|| RatioError::NoSplit(path.0.clone()))?;
        let LayoutNode::Split { ratios, .. } = node else {
            return Err(RatioError::NoSplit(path.0.clone()));
        };
        if new_ratios.len() != ratios.len() {
            return Err(RatioError::BoundaryOutOfRange {
                boundary: new_ratios.len(),
                children: ratios.len(),
            });
        }
        let sum: f32 = new_ratios.iter().sum();
        if sum <= f32::EPSILON {
            return Err(RatioError::BelowMinSize);
        }
        *ratios = new_ratios.into_iter().map(|r| r / sum).collect();
        Ok(())
    }
}

impl Divider {
    /// Axis of the split this divider belongs to.
    pub fn axis(&self) -> Direction {
        self.visual.axis
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("duplicate PaneId `{0:?}` in layout tree")]
    DuplicatePaneId(PaneId),
}

fn check_unique(node: &LayoutNode, seen: &mut HashSet<PaneId>) -> Result<(), LayoutError> {
    match node {
        LayoutNode::Leaf(id) => {
            if !seen.insert(*id) {
                return Err(LayoutError::DuplicatePaneId(*id));
            }
        }
        LayoutNode::Tabs(group) => {
            for id in group.members() {
                if !seen.insert(*id) {
                    return Err(LayoutError::DuplicatePaneId(*id));
                }
            }
        }
        LayoutNode::Split { children, .. } => {
            for c in children {
                check_unique(c, seen)?;
            }
        }
    }
    Ok(())
}


/// Stable identifier of a Split node, encoded as the sequence of child-indexes
/// leading from the root. Cheap to clone (small vec) and stable across
/// re-renders as long as the tree structure doesn't mutate.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct SplitPath(pub Vec<u8>);

impl SplitPath {
    pub const fn root() -> Self {
        Self(Vec::new())
    }
    pub fn push(mut self, child: u8) -> Self {
        self.0.push(child);
        self
    }
}

/// A resizable seam between two children of the same Split node.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DividerRect {
    pub axis: Direction,
    /// The 1-cell-wide rect the seam occupies (a column for Horizontal splits,
    /// a row for Vertical splits).
    pub rect: Rect,
}

/// One divider observation: which split it belongs to, which boundary index
/// inside that split (`0` = between children 0 and 1, etc.), and its rect.
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    pub path: SplitPath,
    pub boundary: usize,
    pub visual: DividerRect,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RatioError {
    #[error("no split at path {0:?}")]
    NoSplit(Vec<u8>),
    #[error("boundary {boundary} out of range for split of {children} children")]
    BoundaryOutOfRange { boundary: usize, children: usize },
    #[error("adjustment would drop a child below its min-size floor")]
    BelowMinSize,
}
fn walk(node: &LayoutNode, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    match node {
        LayoutNode::Leaf(id) => out.push((*id, area)),
        LayoutNode::Tabs(group) => {
            if let Some(id) = group.active_pane() {
                out.push((id, area));
            }
        }
        LayoutNode::Split {
            direction,
            ratios,
            children,
        } => {
            let constraints: Vec<Constraint> = ratios
                .iter()
                .map(|r| Constraint::Ratio((*r * 10_000.0).round() as u32, 10_000))
                .collect();
            let rects = Layout::default()
                .direction(*direction)
                .constraints(constraints)
                .split(area);
            for (child, rect) in children.iter().zip(rects.iter()) {
                walk(child, *rect, out);
            }
        }
    }
}

fn find_tab_group<'a>(node: &'a LayoutNode, id: TabGroupId) -> Option<&'a TabGroup> {
    match node {
        LayoutNode::Tabs(g) if g.id() == id => Some(g),
        LayoutNode::Split { children, .. } => {
            children.iter().find_map(|c| find_tab_group(c, id))
        }
        _ => None,
    }
}

fn find_tab_group_mut<'a>(node: &'a mut LayoutNode, id: TabGroupId) -> Option<&'a mut TabGroup> {
    match node {
        LayoutNode::Tabs(g) if g.id() == id => Some(g),
        LayoutNode::Split { children, .. } => {
            children.iter_mut().find_map(|c| find_tab_group_mut(c, id))
        }
        _ => None,
    }
}

fn collect_tab_groups<'a>(node: &'a LayoutNode, out: &mut Vec<&'a TabGroup>) {
    match node {
        LayoutNode::Tabs(g) => out.push(g),
        LayoutNode::Split { children, .. } => {
            for c in children {
                collect_tab_groups(c, out);
            }
        }
        LayoutNode::Leaf(_) => {}
    }
}

fn find_split<'a>(node: &'a LayoutNode, path: &SplitPath) -> Option<&'a LayoutNode> {
    let mut cursor = node;
    for &step in &path.0 {
        match cursor {
            LayoutNode::Split { children, .. } => {
                cursor = children.get(step as usize)?;
            }
            _ => return None,
        }
    }
    Some(cursor)
}

fn find_split_mut<'a>(
    node: &'a mut LayoutNode,
    path: &SplitPath,
) -> Option<&'a mut LayoutNode> {
    let mut cursor = node;
    for &step in &path.0 {
        cursor = match cursor {
            LayoutNode::Split { children, .. } => children.get_mut(step as usize)?,
            _ => return None,
        };
    }
    Some(cursor)
}

/// Walk the tree and emit one [`Divider`] for each seam we can see in `area`.
/// The 1-cell rect follows the ratatui `Layout::split` computation so hit
/// tests line up exactly with what the user clicked on.
fn collect_dividers(node: &LayoutNode, area: Rect, path: SplitPath, out: &mut Vec<Divider>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if let LayoutNode::Split {
        direction,
        ratios,
        children,
    } = node
    {
        let constraints: Vec<Constraint> = ratios
            .iter()
            .map(|r| Constraint::Ratio((*r * 10_000.0).round() as u32, 10_000))
            .collect();
        let rects = Layout::default()
            .direction(*direction)
            .constraints(constraints)
            .split(area);
        // A seam sits between rects[i] and rects[i+1]; the 1-cell strip is the
        // last column/row of rects[i] (matches how ratatui packs them without
        // gaps).
        for i in 0..rects.len().saturating_sub(1) {
            let left = rects[i];
            let strip = match direction {
                Direction::Horizontal => Rect {
                    x: left.x + left.width.saturating_sub(1),
                    y: left.y,
                    width: 1,
                    height: left.height,
                },
                Direction::Vertical => Rect {
                    x: left.x,
                    y: left.y + left.height.saturating_sub(1),
                    width: left.width,
                    height: 1,
                },
            };
            out.push(Divider {
                path: path.clone(),
                boundary: i,
                visual: DividerRect {
                    axis: *direction,
                    rect: strip,
                },
            });
        }
        for (idx, child) in children.iter().enumerate() {
            let child_path = path.clone().push(idx as u8);
            collect_dividers(child, rects[idx], child_path, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::PaneId;
    use crate::tabs::{MembersPolicy, PaneKind};

    fn leaf() -> PaneId {
        PaneId::next()
    }

    #[test]
    fn horizontal_split_two_halves() {
        let a = leaf();
        let b = leaf();
        let tree = LayoutTree::new(LayoutNode::split(
            Direction::Horizontal,
            vec![1.0, 1.0],
            vec![LayoutNode::leaf(a), LayoutNode::leaf(b)],
        ))
        .unwrap();
        let rects = tree.compute_rects(Rect::new(0, 0, 20, 10));
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].1.width + rects[1].1.width, 20);
    }

    #[test]
    fn tab_group_shows_only_active_member() {
        let a = leaf();
        let b = leaf();
        let group = TabGroup::new(
            TabGroupId::from_static("g"),
            vec![a, b],
            MembersPolicy::Fixed,
            PaneKind::Shell,
        );
        let tree = LayoutTree::new(LayoutNode::tabs(group)).unwrap();
        let rects = tree.compute_rects(Rect::new(0, 0, 20, 10));
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].0, a);
    }

    #[test]
    fn duplicate_pane_id_is_rejected() {
        let dup = leaf();
        let err = LayoutTree::new(LayoutNode::split(
            Direction::Horizontal,
            vec![1.0, 1.0],
            vec![LayoutNode::leaf(dup), LayoutNode::leaf(dup)],
        ))
        .unwrap_err();
        assert!(matches!(err, LayoutError::DuplicatePaneId(_)));
    }

    #[test]
    fn dividers_enumerate_root_and_child_splits() {
        // Layout:
        //   Horizontal [0.5 | 0.5]  →  seam at column 15 for a 30-col rect.
        //     child 0 = Vertical [0.4 | 0.6]  →  seam at row 4 for a 10-row rect.
        //     child 1 = Leaf
        let a = leaf();
        let b = leaf();
        let c = leaf();
        let tree = LayoutTree::new(LayoutNode::split(
            Direction::Horizontal,
            vec![1.0, 1.0],
            vec![
                LayoutNode::split(
                    Direction::Vertical,
                    vec![4.0, 6.0],
                    vec![LayoutNode::leaf(a), LayoutNode::leaf(b)],
                ),
                LayoutNode::leaf(c),
            ],
        ))
        .unwrap();
        let dividers = tree.dividers(Rect::new(0, 0, 30, 10));
        // 1 for the root horizontal seam, 1 for the nested vertical seam.
        assert_eq!(dividers.len(), 2);
        // Root divider is horizontal (axis = the split's direction).
        assert_eq!(dividers[0].axis(), Direction::Horizontal);
        assert_eq!(dividers[0].path, SplitPath::root());
        // Nested divider lives under root's first child (index 0).
        assert_eq!(dividers[1].path, SplitPath::root().push(0));
        assert_eq!(dividers[1].axis(), Direction::Vertical);
    }

    #[test]
    fn adjust_ratio_moves_boundary_and_preserves_sum() {
        let a = leaf();
        let b = leaf();
        let mut tree = LayoutTree::new(LayoutNode::split(
            Direction::Horizontal,
            vec![0.4, 0.6],
            vec![LayoutNode::leaf(a), LayoutNode::leaf(b)],
        ))
        .unwrap();
        tree.adjust_ratio(&SplitPath::root(), 0, 0.1, &[0.0, 0.0])
            .unwrap();
        let ratios = tree.ratios_at(&SplitPath::root()).unwrap();
        assert!((ratios[0] - 0.5).abs() < 1e-6);
        assert!((ratios[1] - 0.5).abs() < 1e-6);
        assert!((ratios.iter().sum::<f32>() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn adjust_ratio_rejects_below_min_size_floor() {
        let a = leaf();
        let b = leaf();
        let mut tree = LayoutTree::new(LayoutNode::split(
            Direction::Horizontal,
            vec![0.5, 0.5],
            vec![LayoutNode::leaf(a), LayoutNode::leaf(b)],
        ))
        .unwrap();
        // floor of 0.4 for child 0; -0.2 delta would send child 0 to 0.3.
        let err = tree
            .adjust_ratio(&SplitPath::root(), 0, -0.2, &[0.4, 0.0])
            .unwrap_err();
        assert_eq!(err, RatioError::BelowMinSize);
    }

    #[test]
    fn set_ratios_normalizes_to_unit_sum() {
        let a = leaf();
        let b = leaf();
        let mut tree = LayoutTree::new(LayoutNode::split(
            Direction::Horizontal,
            vec![0.5, 0.5],
            vec![LayoutNode::leaf(a), LayoutNode::leaf(b)],
        ))
        .unwrap();
        tree.set_ratios(&SplitPath::root(), vec![1.0, 3.0]).unwrap();
        let ratios = tree.ratios_at(&SplitPath::root()).unwrap();
        assert!((ratios[0] - 0.25).abs() < 1e-6);
        assert!((ratios[1] - 0.75).abs() < 1e-6);
    }
}
