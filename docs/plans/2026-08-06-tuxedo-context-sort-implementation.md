# Tuxedo Context Sort Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add first-`@context` sorting and grouping to tuxedo's existing sort cycle.

**Architecture:** Extend the current enum-driven pipeline end to end: preference parse/cycle, comparator, visible group key, and list heading. Keep the implementation symmetric with project sorting and reuse all existing filter, completion, navigation, and rendering flows.

**Tech Stack:** Rust 1.90, ratatui, built-in Rust tests, insta snapshots.

---

### Task 1: Add failing contract tests

**Files:**
- Modify: `crates/tuxedo/src/app/prefs.rs`
- Modify: `crates/tuxedo/src/app/visibility.rs`
- Modify: `crates/tuxedo/src/config.rs`
- Modify: `crates/tuxedo/tests/snapshots.rs`

1. Add a cycle test requiring `Project → Context → File`.
2. Add visible-group tests requiring first-context case-insensitive ordering, `ListContext(None)`, completion pinning, and compatibility with an active context filter.
3. Change config round-trip coverage to require `Sort::Context` serialized as `sort = context` and parsed back.
4. Add a `list_grouped_by_context` snapshot scene covering named, missing, multi-context, and completed tasks.
5. Do not run tests in this implementation session; the main session will confirm the expected red state.

### Task 2: Implement the minimal context sort pipeline

**Files:**
- Modify: `crates/tuxedo/src/app/types.rs`
- Modify: `crates/tuxedo/src/app/prefs.rs`
- Modify: `crates/tuxedo/src/core/filter.rs`
- Modify: `crates/tuxedo/src/app/visibility.rs`
- Modify: `crates/tuxedo/src/ui/list.rs`

1. Add `Sort::Context` with `context` display and parsing.
2. Insert it between `Project` and `File` in `Prefs::cycle_sort`.
3. Add a context comparator mirroring the project comparator but reading `contexts.first()`.
4. Add `GroupKey::ListContext` and produce it for pending rows under context sort; keep done rows as `Completed`.
5. Render `@name` / `NO CONTEXT` headings and count keys using `theme.context`.
6. Keep every enum match exhaustive and add no compatibility aliases.

### Task 3: Update snapshots and user documentation

**Files:**
- Create: `crates/tuxedo/tests/snapshots/snapshots__list_grouped_by_context_text.snap`
- Create: `crates/tuxedo/tests/snapshots/snapshots__list_grouped_by_context_styled.snap`
- Modify: `crates/tuxedo/README.md`

1. Record only the new context-grouped snapshot outputs.
2. Update README sort summaries and the `S` key cycle to include project and context in exact cycle order.
3. Do not change unrelated snapshots or documentation.

### Task 4: Main-session verification

Run from the workspace root after implementation:

```sh
cargo test -p tuxedo app::prefs::tests::cycle_sort_includes_context_between_project_and_file
cargo test -p tuxedo app::visibility::tests::list_groups_track_first_context_under_sort_context
cargo test -p tuxedo app::visibility::tests::context_sort_respects_context_filter_and_completed_group
cargo test -p tuxedo config::tests::round_trips
cargo test -p tuxedo --test snapshots list_grouped_by_context
cargo test -p tuxedo
```

Expected: all commands pass with no unreviewed `.snap.new` files. This delegated implementation session must not execute them.
