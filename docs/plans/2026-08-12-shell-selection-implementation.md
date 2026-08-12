# Shell Selection Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make Settings shell selection persist, affect every subsequently created shell tab, and enumerate common installed shells.

**Architecture:** Add a focused global shell-preference type in `rimeterm-config`; keep primary config TOML untouched. Centralize effective-shell resolution in the PTY crate, pass the loaded app config into Settings enumeration, and persist before swapping live application state.

**Tech Stack:** Rust 1.90, serde/TOML, which, portable-pty, ratatui, cargo test.

---

### Task 1: Persist the global shell preference

**Files:**
- Create: `crates/rimeterm-config/src/shell_preference.rs`
- Modify: `crates/rimeterm-config/src/lib.rs`
- Modify: `crates/rimeterm-config/src/paths.rs`

**Steps:**
1. Add failing tests for TOML round-trip and missing-file default.
2. Run `cargo test -p rimeterm-config shell_preference --lib` and confirm RED.
3. Implement `ShellPreference { path: Option<PathBuf> }`, atomic `save_to`, `load_or_default`, and `paths::shell_preference_file()`.
4. Re-run the focused tests and confirm GREEN.

### Task 2: Expand and resolve shell candidates

**Files:**
- Modify: `crates/rimeterm-pty/src/shell_detect.rs`

**Steps:**
1. Add failing tests for candidate ordering and classification of common shell names.
2. Run `cargo test -p rimeterm-pty shell_detect --lib` and confirm RED.
3. Add shared platform candidate constants, path-based classification, preference resolution with stale-path fallback, and normalized path deduplication.
4. Re-run the focused tests and confirm GREEN.

### Task 3: Wire Settings and App to loaded config and persistence

**Files:**
- Modify: `crates/rimeterm-tui/src/settings.rs`
- Modify: `crates/rimeterm-tui/src/app.rs`

**Steps:**
1. Add failing Settings tests proving configured hints are retained and Enter emits the selected executable.
2. Add a focused App helper test proving persistence succeeds before active-shell replacement.
3. Run `cargo test -p rimeterm-tui settings --lib` and confirm RED.
4. Let Settings receive current platform hints from `App::config` instead of `CoreConfig::default()`.
5. Persist `SetShell` to the global preference file before updating `self.shell_choice`; surface failure and retain old state.
6. Update startup shell resolution to prefer a valid saved executable.
7. Re-run focused TUI tests and confirm GREEN.

### Task 4: Verify behavior and quality

**Files:**
- Modify only if verification exposes defects.

**Steps:**
1. Run `cargo fmt --all -- --check`.
2. Run `cargo test -p rimeterm-config -p rimeterm-pty -p rimeterm-tui --lib`.
3. Run `cargo clippy -p rimeterm-config -p rimeterm-pty -p rimeterm-tui --all-targets --locked -- -D warnings`.
4. Run the shell-selection smoke scenario through the PTY/session boundary.
5. Review the complete diff against the approved design; remove stale comments and unused paths.
