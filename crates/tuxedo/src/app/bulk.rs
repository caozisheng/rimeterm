use super::App;
use super::types::Mode;
use crate::core::{ArchiveOutcome, BulkCompleteOutcome};

impl App {
    /// Bulk-complete every task in the selection that isn't already done.
    /// Clears the selection and exits Visual mode on success. Recurring tasks
    /// also spawn their next instance (handled by the store).
    pub fn complete_selected(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let indices: Vec<usize> = self.selection.iter().collect();
        match self.store.complete_many(&indices) {
            BulkCompleteOutcome::Done { completed, spawned } => {
                self.selection.clear();
                self.mode = Mode::Normal;
                self.flash(if spawned > 0 {
                    format!("completed {completed} (+{spawned} recurring)")
                } else {
                    format!("completed {completed}")
                });
                self.recompute_visible();
                self.clamp_cursor();
            }
            BulkCompleteOutcome::NothingToComplete => {
                self.flash("nothing to complete");
                self.selection.clear();
                self.mode = Mode::Normal;
            }
            BulkCompleteOutcome::Aborted(r) => self.handle_reconcile_abort(r),
            BulkCompleteOutcome::Error(e) => self.flash(format!("complete failed: {e}")),
        }
    }

    /// Bulk-archive every task in the selection: move each row (state
    /// preserved) to `archive.txt`. Powers `dd` in Visual mode.
    pub fn archive_selected(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let indices: Vec<usize> = self.selection.iter().collect();
        match self.store.archive_many(&indices) {
            ArchiveOutcome::Archived { count } => {
                self.selection.clear();
                self.mode = Mode::Normal;
                self.flash(format!("archived {count}"));
                self.recompute_visible();
                self.clamp_cursor();
            }
            ArchiveOutcome::Nothing => {
                self.selection.clear();
                self.mode = Mode::Normal;
            }
            ArchiveOutcome::Aborted(r) => self.handle_reconcile_abort(r),
            ArchiveOutcome::Error(e) => self.flash(format!("archive failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::build_app;

    #[test]
    fn complete_selected_clears_selection_and_flashes() {
        let mut app = build_app("a\nb\nc\n");
        app.selection.toggle(0);
        app.selection.toggle(2);
        app.mode = Mode::Visual;
        app.complete_selected();
        assert!(app.tasks()[0].done);
        assert!(!app.tasks()[1].done);
        assert!(app.tasks()[2].done);
        assert!(app.selection.is_empty());
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.flash_active(), Some("completed 2"));
    }

    #[test]
    fn complete_selected_reports_recurring_spawns() {
        let mut app = build_app("a\nPay rent due:2026-04-15 rec:+1m\nb\nWater plants rec:1w\n");
        app.refresh_today("2026-05-09".into());
        app.selection.toggle(1);
        app.selection.toggle(3);
        app.mode = Mode::Visual;
        app.complete_selected();
        assert_eq!(app.tasks().len(), 6);
        assert_eq!(app.flash_active(), Some("completed 2 (+2 recurring)"));
    }

    #[test]
    fn archive_selected_moves_all_in_selection_to_archive() {
        let mut app = build_app("a\nb\nc\nd\n");
        app.selection.toggle(1);
        app.selection.toggle(3);
        app.mode = Mode::Visual;
        app.archive_selected();
        assert_eq!(app.tasks().len(), 2);
        assert_eq!(app.tasks()[0].raw, "a");
        assert_eq!(app.tasks()[1].raw, "c");
        assert!(app.selection.is_empty());
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.flash_active(), Some("archived 2"));
    }
}
