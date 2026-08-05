# Todo Tab Mouse Selection Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add reliable mouse text selection/copy and a draggable scrollbar to the embedded Todo tab.

**Architecture:** Tuxedo owns selection, rendered-body geometry, scrollbar state, and clipboard extraction. `EmbeddedApp` exposes mouse handling and scrollbar ownership. `TodoPane` translates host coordinates and delegates, while `App` already routes drag/up events for panes that report an active scrollbar drag.

**Tech Stack:** Rust 2024, ratatui 0.30, crossterm 0.29, existing OSC 52 clipboard helper, Cargo tests.

---

### Task 1: Add failing pure interaction tests

**Files:**
- Modify: `crates/tuxedo/src/app/selection.rs`
- Modify: `crates/tuxedo/src/ui/mod.rs`
- Modify: `crates/tuxedo/src/embed.rs`

**Step 1: Write failing tests**

Cover normalized selection endpoints, display-column slicing with UTF-8 text, scrollbar offset mapping/clamping, and `EmbeddedApp` mouse forwarding/scrollbar ownership.

**Step 2: Run focused tests**

Run `cargo test -p tuxedo selection -- --nocapture` and the targeted embed tests. Expected: failures for the new APIs.

---

### Task 2: Implement selection state and extraction

**Files:**
- Modify: `crates/tuxedo/src/app/selection.rs`
- Modify: `crates/tuxedo/src/app/mod.rs`
- Modify: `crates/tuxedo/src/controller.rs`

**Step 1: Implement minimal state**

Add a separate display selection with anchor, active endpoint, and active-drag state. Keep the existing task `Selection` untouched. Add methods to begin/update/end/clear, query active state, and extract selected rendered lines by display columns.

**Step 2: Handle copy**

Make `Ctrl+C` copy the display selection first when present, using OSC 52 and the existing flash/error path. Preserve existing keybind resolution when no display selection exists. Right-click with an active display selection copies it and consumes the event.

**Step 3: Run focused tests**

Run the selection and controller tests. Expected: all new and existing tests pass.

---

### Task 3: Render selection and scrollbar geometry

**Files:**
- Modify: `crates/tuxedo/src/ui/list.rs`
- Modify: `crates/tuxedo/src/ui/archive.rs`
- Modify: `crates/tuxedo/src/ui/mod.rs`
- Modify: `crates/tuxedo/src/app/mod.rs`

**Step 1: Publish geometry**

Build the existing display lines, reserve the right-most column only when content exceeds the viewport, and publish body rect, visible text width, total line count, and current scroll offset to App.

**Step 2: Paint selection and scrollbar**

Render selected cells with the theme selection style without changing task row semantics. Render ratatui's `Scrollbar` only when scrollable and use the current offset/viewport to position it.

**Step 3: Run renderer tests**

Run focused Tuxedo tests with the ratatui test backend and existing snapshots where applicable.

---

### Task 4: Route mouse events through the embedded pane

**Files:**
- Modify: `crates/tuxedo/src/embed.rs`
- Modify: `crates/rimeterm-tui/src/todo_pane.rs`

**Step 1: Add embedded mouse API**

Translate absolute host coordinates to the embedded rect, handle left down/drag/up, wheel, and right-click selection copy, and expose `scrollbar_dragging`.

**Step 2: Delegate from TodoPane**

Forward `PaneProvider::on_mouse`, return consumed status, and implement `PaneProvider::scrollbar_dragging` by delegation.

**Step 3: Run focused integration tests**

Run `cargo test -p tuxedo` and `cargo test -p rimeterm-tui todo_pane -- --nocapture`.

---

### Task 5: Verify end-to-end behavior and review

**Files:**
- No new files expected.

**Step 1: Run workspace checks relevant to changed crates**

Run `cargo test -p tuxedo --all-targets` and `cargo test -p rimeterm-tui --all-targets`.

**Step 2: Review diff and behavior**

Check that existing task selection/edit/copy keybindings remain unchanged and that drag/up events cannot leak to unrelated panes. Review for allocation and UTF-8 column correctness.

**Step 3: Run formatter and final smoke test**

Run `cargo fmt --all -- --check`, then focused tests again. Exercise the TUI with a temporary multi-line Todo file if the local terminal runner is available.
