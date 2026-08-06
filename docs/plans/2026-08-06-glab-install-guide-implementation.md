# Glab Install Guide Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Show complete, host-specific setup and authentication instructions the first time the native Glab pane encounters a missing backend, missing authentication, or an unrecognized repository, while keeping ready project data unchanged.

**Architecture:** Preserve host context in typed `GlabError` variants after repository detection, then map setup errors to dependency-free static multiline guides inside `glab-tui`. Render guide errors as a wrapped, vertically scrollable `Paragraph`; ordinary command and parse errors remain concise, and `Ready` renders only project data.

**Tech Stack:** Rust 1.90, ratatui, crossterm, built-in Rust tests.

---

### Task 1: Specify guide mapping and backend context

**Files:**
- Modify: `crates/glab-tui/src/lib.rs`

**Step 1: Write failing tests**

1. Add mapping tests requiring `CliMissing`, `NotAuthenticated`, and `NotRepository` to produce setup guides.
2. Assert GitLab guides contain executable Windows (`winget`, Scoop), macOS (`brew`), Linux package/official, Cargo fallback, `glab auth login`, reload, git/origin, token ownership, and no `glab-tui` installation command.
3. Assert GitHub guides contain executable Windows (`winget`, Scoop), macOS (`brew`), Linux package/official page, `gh auth login`, reload, git/origin, and token ownership instructions.
4. Add backend tests proving a missing or unauthenticated `glab`/`gh` retains the detected `ProjectHost`.

**Step 2: Run tests to verify red**

Run:

```sh
cargo test -p glab-tui install_guide
cargo test -p glab-tui process_backend_preserves
```

Expected: compilation or assertion failure because typed host context and guide mapping do not exist.

**Step 3: Implement minimal mapping and context**

1. Add host context to backend `CliMissing` and `NotAuthenticated` errors without exposing stderr credentials.
2. Map setup errors to static multiline guides; use the recognized host when available and a generic GitLab/GitHub explanation when origin cannot be identified.
3. Leave `Parse` and `Command` as ordinary errors.

**Step 4: Run focused tests green**

Run the two focused commands again. Expected: all selected tests pass.

### Task 2: Specify and implement wrapped scrolling render

**Files:**
- Modify: `crates/glab-tui/src/lib.rs`
- Modify only if the adapter requires it: `crates/rimeterm-tui/src/glab_pane.rs`

**Step 1: Write failing tests**

1. Add a ready-render test proving install text is absent.
2. Add a small-pane error-render test proving multiline guide rendering does not panic.
3. Add key and mouse tests requiring setup-guide scroll to advance and return upward.

**Step 2: Run tests to verify red**

Run:

```sh
cargo test -p glab-tui render_
cargo test -p glab-tui guide_scroll
```

Expected: assertions fail because error text is neither wrapped nor scrollable.

**Step 3: Implement minimal rendering**

1. Store a bounded vertical guide scroll offset in `EmbeddedApp` and reset it on refresh or snapshot replacement.
2. Route Up/Down, PageUp/PageDown, Home/End and mouse wheel to guide scrolling only while a setup guide is visible; keep ready list navigation unchanged.
3. Render setup guides with `Paragraph::wrap` and `.scroll`; suppress empty Todos/Notifications headings in error state.

**Step 4: Run focused tests green**

Run the focused render and scroll commands again. Expected: all selected tests pass.

### Task 3: Update the design contract

**Files:**
- Modify: `docs/rimeterm-gitlab-design.md`

1. Expand error handling to require complete, actionable GitLab/GitHub setup instructions for missing CLI, missing authentication, and unrecognized repository.
2. State that RimeTerm never installs the `glab-tui` binary and never reads or saves tokens.
3. Add acceptance coverage for guide mapping, key commands, ready-state exclusion, and wrapped/scrollable small-pane rendering.

### Task 4: Verify and commit

Run from the workspace root:

```sh
cargo test -p glab-tui
cargo test -p rimeterm-tui
cargo test --workspace
cargo fmt --all -- --check
```

Expected: all tests and formatting checks pass.

Commit all scoped changes with a Conventional Commit subject no longer than 72 characters:

```sh
git add crates/glab-tui/src/lib.rs crates/rimeterm-tui/src/glab_pane.rs docs/rimeterm-gitlab-design.md docs/plans/2026-08-06-glab-install-guide-implementation.md
git commit -m "feat(glab): add backend setup guide"
```

Record the commit hash, remove the global worktree from outside it, and confirm the directory no longer exists.
