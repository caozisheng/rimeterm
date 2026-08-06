# Glab Text Interaction Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:test-driven-development to implement this plan task-by-task.

**Goal:** Add pane-local text selection and scrolling to Glab setup guides, and viewport scrolling and row selection to long ready lists.

**Architecture:** `glab-tui::EmbeddedApp` owns rendered guide lines, absolute render rectangles, scroll offsets, selection endpoints, scrollbar drag state, and ready-list viewport state. Input is interpreted against those cached rectangles after render. Clipboard writes remain in `rimeterm-tui::GlabPane`, which drains copy requests from the embedded app and writes them with the existing `arboard` dependency; failures are deliberately silent.

**Tech Stack:** Rust, crossterm events, ratatui widgets and buffers, arboard through the host pane, Cargo tests.

---

### Task 1: Specify guide selection and scrolling

**Files:**
- Modify: `crates/glab-tui/src/lib.rs`

1. Add failing tests for char-cell-safe selection extraction, selection highlight, absolute-coordinate down/drag/up, pending auto-copy, wheel and key clamping, and scrollbar dragging.
2. Run focused `glab-tui` tests and confirm the new API or behavior fails.
3. Add the minimum guide surface state and input/render logic to pass.
4. Re-run focused tests until green.

### Task 2: Specify ready-list viewport interaction

**Files:**
- Modify: `crates/glab-tui/src/lib.rs`

1. Add failing tests for long-list viewport scrolling, scrollbar rendering/dragging, click-to-select, and preservation of Up/Down semantics.
2. Run focused tests and confirm failure.
3. Add the minimum viewport, scrollbar, key, wheel, drag, and click behavior.
4. Re-run focused tests until green.

### Task 3: Delegate host capabilities and clipboard

**Files:**
- Modify: `crates/rimeterm-tui/src/glab_pane.rs`

1. Add failing tests for `has_active_selection`, `scrollbar_dragging`, and `wants_mouse_priority` in guide and ready states.
2. Run focused `rimeterm-tui` Glab pane tests and confirm failure.
3. Delegate capabilities to `EmbeddedApp`; after consumed key/mouse events, drain pending copy text and attempt `arboard` writes without status claims or panics.
4. Re-run focused tests until green.

### Task 4: Verify and commit

**Files:**
- Verify only the files above and this plan document changed.

1. Run `cargo fmt --all -- --check`.
2. Run `cargo test -p glab-tui`.
3. Run `cargo test -p rimeterm-tui`.
4. Run `cargo test --workspace`.
5. Commit with a Conventional Commit message and record the resulting SHA.
