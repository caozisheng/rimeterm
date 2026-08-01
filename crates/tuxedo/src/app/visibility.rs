use super::App;
use super::types::{Sort, View};
use crate::core::filter::{self, ListDueBucket};

/// One entry per visible row, parallel to `visible_cache`. Renderers detect
/// group transitions by comparing successive entries; under `Sort::File` every
/// row is `GroupKey::None` so the renderer skips headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupKey {
    None,
    /// Archive view groups everything under one heading.
    Archive,
    /// `Some('A'..='Z')` for a graded priority, `None` for unprioritized.
    ListPriority(Option<char>),
    ListDue(ListDueBucket),
    /// Completed tasks under `Sort::Priority` / `Sort::Due`. Rendered as a
    /// COMPLETED section pinned to the bottom of the live list.
    Completed,
}

impl App {
    /// Indices into the active view's task source after filter + sort, in
    /// display order. The source is `archive().tasks()` in Archive view,
    /// `tasks()` otherwise. Reads the cache populated by `recompute_visible`.
    pub fn visible_indices(&self) -> &[usize] {
        &self.visible_cache
    }

    /// Group key per row, parallel to `visible_indices()`.
    pub fn visible_groups(&self) -> &[GroupKey] {
        &self.visible_groups
    }

    /// Recompute the cached visible-index list and parallel group keys. Call
    /// after any mutation that affects filter/sort/view/tasks/archive.
    pub fn recompute_visible(&mut self) {
        match self.view {
            View::List => self.rebuild_list_cache(),
            View::Archive => self.rebuild_archive_cache(),
        }
    }

    fn rebuild_list_cache(&mut self) {
        let needle = (!self.filter.search.is_empty()).then_some(self.filter.search.as_str());
        let tasks = self.store.tasks();
        let today = self.store.today();

        // Partition into pending vs done so the COMPLETED group can be pinned
        // to the bottom under Priority/Due sorts. File sort keeps disk order.
        let (mut pending, mut done): (Vec<usize>, Vec<usize>) = (0..tasks.len())
            .filter(|&i| {
                filter::list_predicate(
                    &tasks[i],
                    self.prefs.show_future,
                    today,
                    &self.filter,
                    needle,
                )
            })
            .partition(|&i| !tasks[i].done);

        filter::sort_by_prefs(&mut pending, tasks, self.prefs.sort);
        filter::sort_by_prefs(&mut done, tasks, self.prefs.sort);

        let week_start = &self.week_start;
        let (idxs, groups): (Vec<usize>, Vec<GroupKey>) = match self.prefs.sort {
            Sort::File => {
                // File sort preserves disk order — done/pending stay interleaved.
                let mut idxs: Vec<usize> = (0..tasks.len())
                    .filter(|&i| {
                        filter::list_predicate(
                            &tasks[i],
                            self.prefs.show_future,
                            today,
                            &self.filter,
                            needle,
                        )
                    })
                    .collect();
                filter::sort_by_prefs(&mut idxs, tasks, Sort::File);
                let groups = vec![GroupKey::None; idxs.len()];
                (idxs, groups)
            }
            Sort::Priority => {
                let mut idxs = Vec::with_capacity(pending.len() + done.len());
                let mut groups = Vec::with_capacity(pending.len() + done.len());
                for &i in &pending {
                    groups.push(GroupKey::ListPriority(tasks[i].priority));
                    idxs.push(i);
                }
                for &i in &done {
                    groups.push(GroupKey::Completed);
                    idxs.push(i);
                }
                (idxs, groups)
            }
            Sort::Due => {
                let mut idxs = Vec::with_capacity(pending.len() + done.len());
                let mut groups = Vec::with_capacity(pending.len() + done.len());
                for &i in &pending {
                    groups.push(GroupKey::ListDue(filter::due_bucket(
                        &tasks[i], today, week_start,
                    )));
                    idxs.push(i);
                }
                for &i in &done {
                    groups.push(GroupKey::Completed);
                    idxs.push(i);
                }
                (idxs, groups)
            }
        };

        self.visible_groups = groups;
        self.visible_cache = idxs;
    }

    fn rebuild_archive_cache(&mut self) {
        // Archive view now holds mixed done/pending rows (post-redesign the
        // `dd` action moves any task here, state preserved). Display order
        // mirrors on-disk order so an unarchive is a stable reversal.
        let archive = self.store.archive().tasks();
        let idxs: Vec<usize> = (0..archive.len()).collect();
        let groups = vec![GroupKey::Archive; idxs.len()];
        self.visible_cache = idxs;
        self.visible_groups = groups;
    }

    pub fn cur_abs(&self) -> Option<usize> {
        self.visible_cache.get(self.cursor).copied()
    }

    pub fn clamp_cursor(&mut self) {
        let len = self.visible_cache.len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
    }

    /// Move the cursor to wherever `abs` lives in the current visible list.
    /// Falls back to clamping if `abs` was filtered out.
    pub(super) fn follow_cursor(&mut self, abs: usize) {
        if let Some(pos) = self.visible_cache.iter().position(|&i| i == abs) {
            self.cursor = pos;
        } else {
            self.clamp_cursor();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::build_app;
    use crate::core::filter::ListDueBucket;

    #[test]
    fn search_matches_subsequence() {
        let mut app = build_app("2026-05-01 Call dentist\n2026-05-01 buy milk\n");
        app.filter.search = "cade".into();
        app.recompute_visible();
        assert_eq!(app.visible_indices().len(), 1);
    }

    #[test]
    fn search_matches_body_not_dates() {
        let mut app = build_app("2026-05-01 buy milk\n2026-04-01 something else\n");
        app.filter.search = "2026".into();
        app.recompute_visible();
        assert_eq!(app.visible_indices().len(), 0);
    }

    #[test]
    fn visible_cache_updates_after_mutation() {
        let mut app = build_app("a\nb\nc\n");
        assert_eq!(app.visible_indices().len(), 3);
        app.draft_set("d".into());
        app.add_from_draft();
        assert_eq!(app.visible_indices().len(), 4);
    }

    #[test]
    fn list_cursor_survives_archive_roundtrip() {
        let mut app = build_app("a\nb\nc\nd\ne\n");
        app.cursor = 3;
        app.set_view(View::Archive);
        app.set_view(View::List);
        assert_eq!(app.cursor, 3, "cursor lost on List → Archive → List");
    }

    #[test]
    fn archive_indices_point_into_archive_tasks() {
        let mut app = build_app("a\n");
        let path = app.archive().path().to_path_buf();
        app.store.archive = crate::app::Archive::for_test(
            crate::todo::parse_file(
                "x 2026-05-01 2026-04-01 first\nx 2026-05-02 2026-04-02 second\n",
            ),
            String::new(),
            path,
        );
        app.set_view(View::Archive);
        let idxs = app.visible_indices();
        assert_eq!(idxs.len(), 2);
        for &i in idxs {
            assert!(app.archive().tasks().get(i).is_some());
        }
    }

    #[test]
    fn list_groups_are_none_under_sort_file() {
        let mut app = build_app("(A) a\n(B) b\nc\n");
        app.prefs.sort = Sort::File;
        app.recompute_visible();
        let groups = app.visible_groups();
        assert_eq!(groups.len(), 3);
        for g in groups {
            assert!(matches!(g, GroupKey::None));
        }
    }

    #[test]
    fn list_groups_track_priority_under_sort_priority() {
        let mut app = build_app("(A) a\n(B) b\nc\n(A) a2\n");
        app.prefs.sort = Sort::Priority;
        app.recompute_visible();
        let groups = app.visible_groups();
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0], GroupKey::ListPriority(Some('A')));
        assert_eq!(groups[1], GroupKey::ListPriority(Some('A')));
        assert_eq!(groups[2], GroupKey::ListPriority(Some('B')));
        assert_eq!(groups[3], GroupKey::ListPriority(None));
    }

    #[test]
    fn list_groups_bucket_due_dates_under_sort_due() {
        let raw = "a due:2026-05-04\n\
                   b due:2026-05-06\n\
                   c due:2026-05-08\n\
                   d due:2026-05-15\n\
                   e due:2026-05-25\n\
                   f\n";
        let mut app = build_app(raw);
        app.prefs.sort = Sort::Due;
        app.recompute_visible();
        let groups = app.visible_groups();
        assert_eq!(groups.len(), 6);
        assert_eq!(groups[0], GroupKey::ListDue(ListDueBucket::Overdue));
        assert_eq!(groups[1], GroupKey::ListDue(ListDueBucket::Today));
        assert_eq!(groups[2], GroupKey::ListDue(ListDueBucket::ThisWeek));
        assert_eq!(groups[3], GroupKey::ListDue(ListDueBucket::NextWeek));
        assert_eq!(groups[4], GroupKey::ListDue(ListDueBucket::Later));
        assert_eq!(groups[5], GroupKey::ListDue(ListDueBucket::NoDue));
    }

    #[test]
    fn future_absolute_threshold_hidden_by_default() {
        let mut app = build_app("future task t:2030-01-01\nvisible task\n");
        assert_eq!(app.visible_indices().len(), 1);
        assert_eq!(app.tasks()[app.visible_indices()[0]].raw, "visible task");
        app.prefs.show_future = true;
        app.recompute_visible();
        assert_eq!(app.visible_indices().len(), 2);
    }

    #[test]
    fn relative_threshold_anchors_on_due() {
        let mut app = build_app("Pay rent due:2026-05-15 t:-3d\n");
        assert_eq!(app.visible_indices().len(), 0);
        app.prefs.show_future = true;
        app.recompute_visible();
        assert_eq!(app.visible_indices().len(), 1);
    }

    #[test]
    fn refresh_today_unhides_tasks_when_date_advances() {
        let mut app = build_app("future task t:2026-05-07\nvisible task\n");
        assert_eq!(app.visible_indices().len(), 1);
        let changed = app.refresh_today("2026-05-07".into());
        assert!(changed);
        assert_eq!(app.today(), "2026-05-07");
        assert_eq!(app.visible_indices().len(), 2);
    }

    #[test]
    fn refresh_today_is_noop_when_date_unchanged() {
        let mut app = build_app("a\n");
        let changed = app.refresh_today("2026-05-06".into());
        assert!(!changed);
        assert_eq!(app.today(), "2026-05-06");
    }

    #[test]
    fn archive_visible_groups_all_use_archive_key() {
        let mut app = build_app("a\n");
        let path = app.archive().path().to_path_buf();
        app.store.archive = crate::app::Archive::for_test(
            crate::todo::parse_file(
                "x 2026-04-01 2026-03-01 older\nx 2026-05-02 2026-04-02 newer\n",
            ),
            String::new(),
            path,
        );
        app.set_view(View::Archive);
        let groups = app.visible_groups();
        assert_eq!(groups.len(), 2);
        assert!(matches!(groups[0], GroupKey::Archive));
        assert!(matches!(groups[1], GroupKey::Archive));
    }

    #[test]
    fn completed_tasks_sink_under_priority_sort() {
        // Done rows always visible; they anchor at the bottom under a
        // COMPLETED group regardless of the priority they had before.
        let mut app = build_app("(A) top\nmiddle\nx 2026-05-05 done-hi\n");
        app.prefs.sort = Sort::Priority;
        app.recompute_visible();
        let groups = app.visible_groups();
        let idxs = app.visible_indices();
        assert_eq!(groups.len(), 3);
        assert!(matches!(groups[0], GroupKey::ListPriority(Some('A'))));
        assert!(matches!(groups[1], GroupKey::ListPriority(None)));
        assert!(matches!(groups[2], GroupKey::Completed));
        assert!(app.tasks()[idxs[2]].done);
    }

    #[test]
    fn completed_tasks_sink_under_due_sort() {
        let mut app = build_app("a due:2026-05-04\nx 2026-05-05 done\n");
        app.prefs.sort = Sort::Due;
        app.recompute_visible();
        let groups = app.visible_groups();
        assert_eq!(groups.len(), 2);
        assert!(matches!(groups[0], GroupKey::ListDue(_)));
        assert!(matches!(groups[1], GroupKey::Completed));
    }

    #[test]
    fn file_sort_keeps_done_interleaved() {
        let mut app = build_app("a\nx 2026-05-05 done\nb\n");
        app.prefs.sort = Sort::File;
        app.recompute_visible();
        let idxs = app.visible_indices();
        assert_eq!(idxs, &[0, 1, 2]);
        for g in app.visible_groups() {
            assert!(matches!(g, GroupKey::None));
        }
    }
}
