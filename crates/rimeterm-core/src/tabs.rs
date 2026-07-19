//! Tab groups — the "one grid cell hosts multiple pane instances" primitive.
//!
//! §19.10.1 (data model) + §19.10.10 (Members Policy) of the design doc. This
//! file encodes the `Fixed` (built-in) vs `Open` (user extendable, same kind
//! only) split and the `try_add` / `try_close` guards that make it robust.

use crate::pane::PaneId;

/// Stable id for a tab group. Kept as `&'static str` because the design doc
/// pins the four built-in groups (`files`, `sysmon`, `agents`, `shells`) at
/// compile time.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TabGroupId(&'static str);

impl TabGroupId {
    pub const fn from_static(s: &'static str) -> Self {
        Self(s)
    }
    pub const fn as_str(&self) -> &'static str {
        self.0
    }
}

/// Built-in group ids the default workspace ships with.
/// See §19.10.7 / §19.10.9 of the design doc.
pub const BUILTIN_FILES: TabGroupId = TabGroupId::from_static("files");
pub const BUILTIN_SYSMON: TabGroupId = TabGroupId::from_static("sysmon");
pub const BUILTIN_AGENTS: TabGroupId = TabGroupId::from_static("agents");
pub const BUILTIN_SHELLS: TabGroupId = TabGroupId::from_static("shells");

impl std::fmt::Display for TabGroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// The kind of pane a group hosts (§19.10.10: Open groups only accept one).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum PaneKind {
    Shell,
    AgentChat,
    Files,
    Sysmon,
    Viewer,
}

impl PaneKind {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::AgentChat => "agent-chat",
            Self::Files => "files",
            Self::Sysmon => "sysmon",
            Self::Viewer => "viewer",
        }
    }
}

/// Members policy — see §19.10.10.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MembersPolicy {
    /// Built-in fixed members (`files`, `sysmon`). `Ctrl+T` and `Ctrl+W` are
    /// rejected; the tree ships with a stable member list.
    Fixed,
    /// User extendable — but every member must be the same [`PaneKind`].
    Open { max: usize },
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PolicyError {
    #[error("tab group `{0}` is fixed; cannot add / close tabs")]
    GroupIsFixed(TabGroupId),
    #[error("tab group `{group}` is full (max = {max})")]
    Full { group: TabGroupId, max: usize },
    #[error("tab group `{group}` accepts kind `{expected}`, got `{got}`")]
    WrongKind {
        group: TabGroupId,
        expected: &'static str,
        got: &'static str,
    },
    #[error("tab group `{0}` has only one member — refusing to close")]
    LastMember(TabGroupId),
    #[error("tab id `{0:?}` not found in group `{1}`")]
    NotFound(PaneId, TabGroupId),
}

/// Runtime state for a group. The layout tree holds one of these per cell.
#[derive(Debug)]
pub struct TabGroup {
    id: TabGroupId,
    kind: PaneKind,
    policy: MembersPolicy,
    members: Vec<PaneId>,
    active: usize,
}

impl TabGroup {
    /// Construct a group. Empty member vectors are rejected (a group must have
    /// at least one active pane; use a `LayoutNode::Leaf` if you truly want 0).
    pub fn new(
        id: TabGroupId,
        members: Vec<PaneId>,
        policy: MembersPolicy,
        kind: PaneKind,
    ) -> Self {
        assert!(
            !members.is_empty(),
            "TabGroup must have at least one member on construction"
        );
        Self {
            id,
            kind,
            policy,
            members,
            active: 0,
        }
    }

    pub fn id(&self) -> TabGroupId {
        self.id
    }

    pub fn kind(&self) -> PaneKind {
        self.kind
    }

    pub fn policy(&self) -> MembersPolicy {
        self.policy
    }

    pub fn members(&self) -> &[PaneId] {
        &self.members
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active_pane(&self) -> Option<PaneId> {
        self.members.get(self.active).copied()
    }

    /// Advance active to the next member (wrap around).
    pub fn next(&mut self) {
        if self.members.is_empty() {
            return;
        }
        self.active = (self.active + 1) % self.members.len();
    }

    /// Go to the previous member (wrap around).
    pub fn prev(&mut self) {
        if self.members.is_empty() {
            return;
        }
        self.active = (self.active + self.members.len() - 1) % self.members.len();
    }

    /// Jump to the Nth member (0-based, no wrap). Returns `Err(NotFound)` on
    /// out-of-range so callers can surface a status-bar hint.
    pub fn goto(&mut self, index: usize) -> Result<(), PolicyError> {
        if index >= self.members.len() {
            return Err(PolicyError::NotFound(
                *self.members.last().expect("non-empty group"),
                self.id,
            ));
        }
        self.active = index;
        Ok(())
    }

    /// Add a new tab. §19.10.10 rules:
    /// - Fixed groups reject with `GroupIsFixed`.
    /// - Open groups require `kind == self.kind`.
    /// - Cap at `max` for Open groups.
    ///
    /// On success the new member becomes active (mirrors the "new tab jumps to
    /// front" convention from browsers / VS Code).
    pub fn try_add(&mut self, id: PaneId, kind: PaneKind) -> Result<PaneId, PolicyError> {
        match self.policy {
            MembersPolicy::Fixed => Err(PolicyError::GroupIsFixed(self.id)),
            MembersPolicy::Open { max } => {
                if self.members.len() >= max {
                    return Err(PolicyError::Full { group: self.id, max });
                }
                if kind != self.kind {
                    return Err(PolicyError::WrongKind {
                        group: self.id,
                        expected: self.kind.as_str(),
                        got: kind.as_str(),
                    });
                }
                self.members.push(id);
                self.active = self.members.len() - 1;
                Ok(id)
            }
        }
    }

    /// Close a tab by its 0-based index. §19.10.10 rules:
    /// - Fixed groups reject.
    /// - Open groups reject when only one member remains (unless `force`).
    pub fn try_close(&mut self, index: usize, force: bool) -> Result<PaneId, PolicyError> {
        match self.policy {
            MembersPolicy::Fixed => Err(PolicyError::GroupIsFixed(self.id)),
            MembersPolicy::Open { .. } => {
                if !force && self.members.len() == 1 {
                    return Err(PolicyError::LastMember(self.id));
                }
                if index >= self.members.len() {
                    return Err(PolicyError::NotFound(
                        *self.members.last().expect("non-empty group"),
                        self.id,
                    ));
                }
                let removed = self.members.remove(index);
                if !self.members.is_empty() {
                    // First shift active left if the removed index is strictly
                    // before it: the survivor originally at `active` now sits
                    // at `active - 1`. Then clamp against the new length so we
                    // never point past the last member.
                    if index < self.active {
                        self.active = self.active.saturating_sub(1);
                    }
                    self.active = self.active.min(self.members.len() - 1);
                }
                Ok(removed)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid() -> PaneId {
        PaneId::next()
    }

    #[test]
    fn cycle_next_prev_wraps() {
        let a = pid();
        let b = pid();
        let c = pid();
        let mut g = TabGroup::new(
            TabGroupId::from_static("t"),
            vec![a, b, c],
            MembersPolicy::Fixed,
            PaneKind::Sysmon,
        );
        g.next();
        assert_eq!(g.active_pane(), Some(b));
        g.next();
        g.next();
        assert_eq!(g.active_pane(), Some(a));
        g.prev();
        assert_eq!(g.active_pane(), Some(c));
    }

    #[test]
    fn fixed_group_rejects_add_and_close() {
        let a = pid();
        let mut g = TabGroup::new(
            TabGroupId::from_static("files"),
            vec![a],
            MembersPolicy::Fixed,
            PaneKind::Files,
        );
        assert_eq!(
            g.try_add(pid(), PaneKind::Files),
            Err(PolicyError::GroupIsFixed(TabGroupId::from_static("files"))),
        );
        assert_eq!(
            g.try_close(0, false),
            Err(PolicyError::GroupIsFixed(TabGroupId::from_static("files"))),
        );
    }

    #[test]
    fn open_group_enforces_kind_and_max() {
        let a = pid();
        let mut g = TabGroup::new(
            TabGroupId::from_static("shells"),
            vec![a],
            MembersPolicy::Open { max: 2 },
            PaneKind::Shell,
        );
        // Wrong kind → rejected.
        let err = g.try_add(pid(), PaneKind::AgentChat).unwrap_err();
        assert!(matches!(err, PolicyError::WrongKind { .. }));

        // Correct kind → grows and moves active.
        let b = g.try_add(pid(), PaneKind::Shell).unwrap();
        assert_eq!(g.active_pane(), Some(b));

        // Hit the cap.
        let err = g.try_add(pid(), PaneKind::Shell).unwrap_err();
        assert!(matches!(err, PolicyError::Full { .. }));
    }

    #[test]
    fn open_group_refuses_to_close_last() {
        let a = pid();
        let mut g = TabGroup::new(
            TabGroupId::from_static("shells"),
            vec![a],
            MembersPolicy::Open { max: 4 },
            PaneKind::Shell,
        );
        assert!(matches!(
            g.try_close(0, false),
            Err(PolicyError::LastMember(_))
        ));
        // `force` overrides.
        assert!(g.try_close(0, true).is_ok());
    }

    #[test]
    fn close_before_active_pulls_index_back() {
        let a = pid();
        let b = pid();
        let c = pid();
        let mut g = TabGroup::new(
            TabGroupId::from_static("shells"),
            vec![a, b, c],
            MembersPolicy::Open { max: 4 },
            PaneKind::Shell,
        );
        g.goto(2).unwrap();
        assert_eq!(g.active_pane(), Some(c));
        g.try_close(0, false).unwrap();
        // Removing index 0 should keep `c` (originally at 2) visible at index 1.
        assert_eq!(g.active_pane(), Some(c));
    }
}
