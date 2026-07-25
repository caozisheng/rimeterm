//! Density-scale spacing tokens used across the TUI layout.
//!
//! The 1/2/3/5/8 progression is a Fibonacci-ish jump set: each step is
//! noticeably bigger than the last, so callers reach for the named slot
//! instead of a raw integer they have to defend against future review.
//!
//! Use sites either consume [`Spacing::cells`] for a `u16` directly
//! (e.g. when subtracting from a viewport height) or rely on the
//! `From<Spacing> for Constraint` impl in `Layout::*` constraint arrays:
//!
//! ```ignore
//! Constraint::Length(Spacing::Md.cells())   // explicit
//! Spacing::Md.into()                        // when the surrounding API takes Into<Constraint>
//! ```

use ratatui::layout::Constraint;

/// Named spacing units, measured in terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spacing {
    /// 1 cell — single-row strips (status bar, separators, footers).
    Xs,
    /// 2 cells — tight padding inside compact widgets.
    Sm,
    /// 3 cells — default block spacing (gutters, popup margins).
    Md,
    /// 5 cells — comfortable section breaks.
    Lg,
    /// 8 cells — generous outer padding for full-screen modals.
    Xl,
}

impl Spacing {
    /// Cell count this slot represents.
    #[must_use]
    pub const fn cells(self) -> u16 {
        match self {
            Spacing::Xs => 1,
            Spacing::Sm => 2,
            Spacing::Md => 3,
            Spacing::Lg => 5,
            Spacing::Xl => 8,
        }
    }
}

impl From<Spacing> for Constraint {
    fn from(s: Spacing) -> Self {
        Constraint::Length(s.cells())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scale must be strictly monotonic — a `Lg` smaller than a `Md`
    /// would break callers' mental model of "step up = more space".
    #[test]
    fn scale_is_strictly_monotonic() {
        let scale = [
            Spacing::Xs,
            Spacing::Sm,
            Spacing::Md,
            Spacing::Lg,
            Spacing::Xl,
        ];
        for pair in scale.windows(2) {
            assert!(
                pair[0].cells() < pair[1].cells(),
                "{:?}={} ≥ {:?}={}",
                pair[0],
                pair[0].cells(),
                pair[1],
                pair[1].cells(),
            );
        }
    }

    #[test]
    fn into_constraint_is_length() {
        let c: Constraint = Spacing::Md.into();
        assert_eq!(c, Constraint::Length(3));
    }
}
