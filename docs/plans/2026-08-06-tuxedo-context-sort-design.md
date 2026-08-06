# Tuxedo Context Sort Design

## Scope

Add `Context` to tuxedo's existing sort cycle, symmetric with `Project`. This changes only sorting/grouping, persisted preference parsing/display, list group headings, documentation, and focused tests. It does not add a view, sidebar behavior, duplicate rows, or rimeterm `TodoPane` behavior.

## Behavior

The cycle becomes `priority → due → project → context → file`. Context sorting uses each task's first parsed `@context`, compares context names case-insensitively, places tasks without a context last, and retains the project sort's priority-then-due tie-breakers. Pending tasks receive `@name` group headings or `NO CONTEXT`; completed tasks remain in the single bottom `COMPLETED` group. Existing filtering, cursor, scrolling, mouse, reload, and archive behavior is unchanged.

## Implementation

Extend the existing `Sort` and `GroupKey` enums exhaustively. Mirror `cmp_project` and the `Sort::Project` visible-group branch with `Task::contexts.first()`. Mirror project list-heading rendering using the theme's context color. Continue serializing the preference through the existing `Display`/`FromStr` config path as `context`.

## Tests and documentation

Add focused unit coverage for context preference parsing/round-trip, cycle order, first-context ordering/grouping, no-context grouping, completion pinning, and filtered results. Add a full-frame context grouping snapshot scene, with reviewed text and styled snapshots. Update only README statements that enumerate available sort modes or the cycle.
