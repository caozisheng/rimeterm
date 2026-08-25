# HTML Viewer System Browser Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make every viewer-open entry point launch local `.html` and `.htm` files in the system default browser without opening or mutating the terminal viewer overlay.

**Architecture:** Add an explicit `ViewerOpenTarget` classification result that keeps HTML browser handoff separate from `ViewerKind` and all overlay payload/state machinery. Route the browser target in `App::open_viewer_overlay`, convert the canonical path to an encoded local-file URL, and use the `webbrowser` crate so local HTML is guaranteed to open in a browser rather than an arbitrary file-associated application.

**Tech Stack:** Rust 1.90, ratatui/crossterm, `url`, `webbrowser`, Cargo unit tests.

---

### Task 1: Declare focused browser-handoff dependencies

**Files:**
- Modify: `Cargo.toml:43-110`
- Modify: `crates/rimeterm-tui/Cargo.toml:12-79`
- Modify: `Cargo.lock`

**Step 1: Add workspace dependencies**

Add direct workspace declarations:

```toml
url = "2.5"
webbrowser = { version = "1.2", default-features = false }
```

Do not enable `webbrowser`'s `hardened` feature; it rejects the required `file://` URLs.

**Step 2: Inherit dependencies in rimeterm-tui**

Add:

```toml
url = { workspace = true }
webbrowser = { workspace = true }
```

**Step 3: Resolve the lockfile**

Run:

```bash
cargo check -p rimeterm-tui --locked
```

Expected: FAIL because `webbrowser` is not yet present in `Cargo.lock` and `--locked` forbids lockfile updates.

Then run:

```bash
cargo check -p rimeterm-tui
```

Expected: Cargo adds `webbrowser` and its platform dependencies to `Cargo.lock`; compilation remains GREEN before source usage.

**Step 4: Commit dependency metadata**

```bash
git add Cargo.toml Cargo.lock crates/rimeterm-tui/Cargo.toml
git commit -m "build(viewer): add browser handoff dependencies"
```

### Task 2: Classify HTML as a browser target

**Files:**
- Modify: `crates/rimeterm-tui/src/viewer.rs:51-221`
- Test: `crates/rimeterm-tui/src/viewer.rs:2026-2164`

**Step 1: Write failing classifier tests**

Extend the existing `tests::classify_source` module with tests equivalent to:

```rust
#[test]
fn html_extensions_select_system_browser_case_insensitively() {
    for name in ["index.html", "INDEX.HTML", "report.htm", "REPORT.HTM"] {
        assert_eq!(
            classify_open_target(Path::new(name), regular(1024)).unwrap(),
            Some(ViewerOpenTarget::SystemBrowser(PathBuf::from(name))),
        );
    }
}

#[test]
fn existing_renderable_extensions_select_overlay() {
    for (name, kind) in [
        ("README.md", ViewerKind::Markdown),
        ("logo.png", ViewerKind::Image),
        ("main.rs", ViewerKind::Code),
    ] {
        let target = classify_open_target(Path::new(name), regular(1024))
            .unwrap()
            .expect("supported");
        assert_eq!(
            target,
            ViewerOpenTarget::Overlay(ViewerSource {
                path: PathBuf::from(name),
                kind,
            }),
        );
    }
}
```

Keep or migrate the existing unsupported, non-regular, and size-cap assertions so the new public classifier preserves those contracts.

**Step 2: Run tests to verify RED**

```bash
cargo test -p rimeterm-tui viewer::tests::classify_source --lib
```

Expected: FAIL because `ViewerOpenTarget` and `classify_open_target` do not exist.

**Step 3: Implement the opening decision**

Add:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewerOpenTarget {
    Overlay(ViewerSource),
    SystemBrowser(PathBuf),
}
```

Add `const HTML_EXTS: &[&str] = &["html", "htm"];` and implement:

```rust
pub fn classify_open_target(
    path: &Path,
    meta: SourceMeta,
) -> Result<Option<ViewerOpenTarget>, ClassifyError> {
    if !meta.is_regular_file {
        return Err(ClassifyError::NotRegularFile);
    }
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return Ok(None);
    };
    if HTML_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
        return Ok(Some(ViewerOpenTarget::SystemBrowser(path.to_path_buf())));
    }
    classify_source(path, meta).map(|source| source.map(ViewerOpenTarget::Overlay))
}
```

Avoid duplicating the existing Markdown/image/code cap logic. Preserve `classify_source` for its state-focused tests and internal rendering classification unless migrating every caller makes removal cleaner.

**Step 4: Run focused classifier tests**

```bash
cargo test -p rimeterm-tui viewer::tests::classify_source --lib
```

Expected: PASS, including case-insensitive HTML, overlay kinds, unsupported types, non-regular paths, and all existing byte caps.

**Step 5: Commit classification**

```bash
git add crates/rimeterm-tui/src/viewer.rs
git commit -m "feat(viewer): classify HTML for browser handoff"
```

### Task 3: Route HTML without mutating overlay state

**Files:**
- Modify: `crates/rimeterm-tui/src/app.rs:1940-2018`
- Modify: `crates/rimeterm-tui/src/app.rs:5901-5964`
- Test: `crates/rimeterm-tui/src/app.rs` test module near viewer routing tests

**Step 1: Extract a testable open-target application helper**

Add a helper whose browser dependency is injectable:

```rust
fn apply_viewer_open_target<F>(
    state: &mut ViewerOverlayState,
    target: ViewerOpenTarget,
    return_focus: ReturnFocus,
    open_browser: F,
) -> Result<Option<(ViewerSource, Generation)>, String>
where
    F: FnOnce(&Path) -> Result<(), String>,
```

Contract:

- `SystemBrowser(path)`: call `open_browser(&path)` and return `Ok(None)` without reading or changing `state`.
- `Overlay(source)`: call `state.open_snapshot(source.clone(), return_focus)` and return `Ok(Some((source, generation)))` so `App` can spawn the existing loader.

Do not move worker spawning into the helper.

**Step 2: Write failing state-invariant tests**

Add tests proving:

```rust
#[test]
fn html_handoff_keeps_closed_overlay_generation_and_focus_unchanged() { /* ... */ }

#[test]
fn html_handoff_keeps_existing_overlay_snapshot_unchanged() { /* ... */ }

#[test]
fn html_handoff_propagates_browser_failure_without_state_change() { /* ... */ }
```

The fake browser closure should record the exact `Path` and return either `Ok(())` or `Err("browser unavailable".into())`. Assert the snapshot, status, generation, and `return_focus` match their pre-call values.

**Step 3: Run tests to verify RED**

```bash
cargo test -p rimeterm-tui html_handoff --lib
```

Expected: FAIL because the helper/routing behavior is absent.

**Step 4: Implement HTML browser launch boundary**

Add a focused launcher:

```rust
fn open_html_in_browser(path: &Path) -> Result<(), String> {
    let path = std::fs::canonicalize(path).map_err(|err| err.to_string())?;
    let url = url::Url::from_file_path(&path)
        .map_err(|_| format!("cannot build browser URL from {}", path.display()))?;
    webbrowser::open(url.as_str()).map_err(|err| err.to_string())
}
```

Keep `spawn_external` unchanged for generic `viewer.open-with-system` and `viewer.reveal`; those commands intentionally use file associations/file-manager behavior, while HTML promises an actual browser.

Update `App::open_viewer_overlay` to call `viewer::classify_open_target` and branch:

- browser target: execute the injected production launcher, set `viewer: open in browser → <path>` on success or `viewer: open in browser failed: <reason>` on failure, request redraw, and return;
- overlay target: preserve the current deduplication, `open_snapshot`, `spawn_blocking`, completion, and redraw flow.

**Step 5: Run focused routing tests**

```bash
cargo test -p rimeterm-tui html_handoff --lib
cargo test -p rimeterm-tui viewer::tests::classify_source --lib
```

Expected: PASS. No browser process should be launched by unit tests.

**Step 6: Commit routing**

```bash
git add crates/rimeterm-tui/src/app.rs
git commit -m "feat(viewer): open HTML in system browser"
```

### Task 4: Verify encoded local-file URLs

**Files:**
- Modify: `crates/rimeterm-tui/src/app.rs` browser URL helper and tests

**Step 1: Split URL construction from process launch**

Extract:

```rust
fn html_file_url(path: &Path) -> Result<url::Url, String>
```

It must canonicalize the file and call `url::Url::from_file_path`; `open_html_in_browser` delegates to it.

**Step 2: Add real-path encoding tests**

Using the existing `tempfile` dev-dependency, create an HTML file whose name contains spaces, non-ASCII text, `#`, and `%`. Assert:

- scheme is `file`;
- `url.to_file_path()` round-trips to the canonical path;
- the URL string contains percent encoding for syntax-significant characters and does not treat `#` as a fragment.

Do not assert the whole URL string; Windows drive-letter and Unix root syntax differ.

**Step 3: Run the encoding tests**

```bash
cargo test -p rimeterm-tui html_file_url --lib
```

Expected: PASS on Windows, macOS, and Linux without launching a browser.

**Step 4: Commit URL coverage**

```bash
git add crates/rimeterm-tui/src/app.rs
git commit -m "test(viewer): cover HTML file URL encoding"
```

### Task 5: Verify behavior and quality

**Files:**
- Modify only if verification exposes a defect.

**Step 1: Format and run focused tests**

```bash
cargo fmt --all -- --check
cargo test -p rimeterm-tui viewer::tests::classify_source --lib
cargo test -p rimeterm-tui html_handoff --lib
cargo test -p rimeterm-tui html_file_url --lib
```

Expected: all commands PASS.

**Step 2: Run the changed crate test suite**

```bash
cargo test -p rimeterm-tui --lib --locked
```

Expected: PASS.

**Step 3: Run lint diagnostics**

```bash
cargo clippy -p rimeterm-tui --all-targets --locked -- -D warnings
```

Expected: PASS with no warnings.

**Step 4: Perform the actual Windows smoke scenario**

Create or select a local HTML fixture with relative assets, launch rimeterm, focus it in the native file manager, and press `Alt+V`.

Observe:

- the configured default browser opens the local page;
- no viewer overlay appears;
- rimeterm focus/pane state remains unchanged;
- a successful browser-handoff hint appears;
- Markdown, image, and code files still open in the existing overlay.

This is the required behavioral proof; unit tests alone do not prove OS browser registration or launch behavior.

**Step 5: Review and commit any verification fixes**

Run the mandatory code review against the complete diff. Fix every critical/high finding, rerun the affected command, then commit only necessary fixes with a Conventional Commit subject no longer than 72 characters.
