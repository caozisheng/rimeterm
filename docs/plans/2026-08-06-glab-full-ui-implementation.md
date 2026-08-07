# Glab Full UI Native Embedding Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace RimeTerm's Todo/Notification-only Glab pane with the complete `rcieri/glab-tui` v0.8.3 interface—every upstream tab, table, detail view, filter, overlay, mutation, and diff/review flow—rendered natively inside the bounded RimeTerm pane.

**Architecture:** Vendor the complete upstream source snapshot at commit `c11c244a43d9cc1c71952ab887d09c9bba9476f3` into the existing `crates/glab-tui` workspace crate, then separate the standalone terminal concerns from reusable application state. A new `controller` owns input/event reduction, and `embed::EmbeddedApp` owns one app instance, root-scoped backend tasks, generation filtering, deadlines, and host-action requests. RimeTerm remains the sole terminal, crossterm event-loop, Tokio-runtime, workspace-root, theme, clipboard, browser, and editor owner; `GlabPane` is only the `PaneProvider` adapter.

**Tech Stack:** Rust 2024 / rustc 1.90, ratatui 0.30, crossterm 0.29, Tokio, serde/toml, async-trait, syntect, upstream fake `glab`/`gh` fixtures, RimeTerm pane and memory-state APIs.

**Approved design:** `docs/rimeterm-gitlab-design.md`

**Worktree:** `C:/Users/zisheng/.config/superpowers/worktrees/rimeterm/glab-full-ui`

**Baseline:** `cargo test -p glab-tui` passes 24 tests; `cargo test -p rimeterm-tui glab_pane` passes 5 tests at commit `a2a3d4c`.

---

## Non-negotiable invariants

1. No `glab-tui` binary target, PTY, raw mode, alternate screen, stdout terminal, independent crossterm reader, `process::exit`, or process-wide `set_current_dir` survives the cutover.
2. All `App` mutation and rendering stay on the RimeTerm main thread. Background tasks return typed events only.
3. Every command uses argument arrays and an explicit workspace root/project context. Root switches abort or invalidate old work; stale completions cannot update the new project.
4. Rendering may modify only the supplied `Rect`; undersized panes render a local compact notice and never panic.
5. The full upstream feature set is retained. This is not another Issues/MR/Todos subset.
6. The simplified `GlabSnapshot`, `GlabStatus`, `ProcessBackend`, and Todo-only renderer are deleted after their callers migrate. No compatibility shim or duplicate implementation remains.
7. GitLab/GitHub credentials remain owned by `glab`/`gh`; RimeTerm never reads, stores, or logs tokens.
8. Browser/editor operations cross the library boundary as typed `HostAction` values. Embedded code never suspends or reconfigures the terminal.

---

### Task 1: Import and prove the complete upstream snapshot

**Files:**
- Modify: `crates/glab-tui/Cargo.toml`
- Modify: `crates/glab-tui/UPSTREAM.md`
- Modify: `crates/glab-tui/LICENSE`
- Replace: `crates/glab-tui/src/lib.rs`
- Create from upstream: `crates/glab-tui/src/app.rs`
- Create from upstream: `crates/glab-tui/src/backend/{mod.rs,gh.rs,glab.rs}`
- Create from upstream: `crates/glab-tui/src/domain/*.rs`
- Create from upstream: `crates/glab-tui/src/{cli.rs,config.rs,editor.rs,entity_editor.rs,event.rs,fetch.rs,git_helpers.rs,keybinding.rs,templates.rs}`
- Create from upstream: `crates/glab-tui/src/handlers/{mod.rs,overlays.rs,tabs.rs}`
- Create from upstream: `crates/glab-tui/src/ui/{mod.rs,diff.rs,helpers.rs,modal.rs,overlays.rs,tabs.rs}`
- Create from upstream: `crates/glab-tui/src/utils/{mod.rs,cache.rs,format.rs,ui.rs,update.rs}`
- Create from upstream: `crates/glab-tui/src/themes/*.toml`
- Create from upstream: `crates/glab-tui/tests/e2e/*.rs`
- Create from upstream: `crates/glab-tui/tests/fixtures/*.json`
- Create from upstream: `crates/glab-tui/tests/mocks/{glab,gh}`
- Modify: `crates/glab-tui/tests/metadata.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

**Step 1: Strengthen provenance and completeness tests**

Add tests before importing source:

```rust
#[test]
fn crate_keeps_the_full_upstream_module_surface() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/app.rs",
        "src/backend/glab.rs",
        "src/backend/gh.rs",
        "src/domain/issues.rs",
        "src/domain/mr.rs",
        "src/domain/pipelines.rs",
        "src/domain/notifications.rs",
        "src/handlers/tabs.rs",
        "src/ui/mod.rs",
        "src/ui/diff.rs",
        "tests/fixtures/issues.json",
        "tests/fixtures/mrs.json",
        "tests/fixtures/pipelines.json",
    ] {
        assert!(root.join(relative).is_file(), "missing upstream file {relative}");
    }
}

#[test]
fn manifest_exposes_only_a_library_target() {
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("manifest");
    assert!(manifest.contains("[lib]"));
    assert!(!manifest.contains("[[bin]]"));
}
```

**Step 2: Run the metadata test and observe failure**

Run: `cargo test -p glab-tui --test metadata`

Expected: FAIL because the upstream modules and fixtures are not present.

**Step 3: Import the pinned snapshot mechanically**

Copy source and test assets byte-for-byte from upstream commit `c11c244a43d9cc1c71952ab887d09c9bba9476f3`; do not copy upstream `.github`, installers, Dockerfile, release assets, `Cargo.lock`, or standalone `src/main.rs`. Retain the upstream MIT license verbatim. Record every intentionally excluded file and every RimeTerm-owned adapter file in `UPSTREAM.md` so future syncs are a snapshot comparison rather than archaeology.

**Step 4: Declare the full dependency surface without a binary target**

Add upstream library dependencies—using existing workspace versions where compatible—and required Tokio features. Keep `[lib] name = "glab_tui"`; do not add `[[bin]]`. Add missing root workspace dependency declarations only when at least one other workspace crate can inherit them; otherwise keep them local to `crates/glab-tui/Cargo.toml`.

**Step 5: Establish a temporary library module root**

Expose the imported modules from `lib.rs` under crate-private visibility, with only stable integration types public later through `embed`. Temporarily gate terminal-owned modules/functions behind `#[cfg(test)]` only if required to keep imported upstream tests runnable; do not add a production standalone mode.

**Step 6: Run imported pure tests**

Run: `cargo test -p glab-tui --lib`

Expected: imported app/domain/format/config tests compile and pass; failures caused by standalone `main.rs` references identify extraction work for subsequent tasks, not missing source.

**Step 7: Commit the mechanical snapshot**

```bash
git add Cargo.toml Cargo.lock crates/glab-tui
git commit -m "refactor(glab): import complete upstream snapshot"
```

---

### Task 2: Replace process-global resources with instance state

**Files:**
- Modify: `crates/glab-tui/src/config.rs`
- Modify: `crates/glab-tui/src/app.rs`
- Modify: `crates/glab-tui/src/ui/**/*.rs`
- Modify: `crates/glab-tui/src/backend/gh.rs`
- Test: `crates/glab-tui/src/config.rs`
- Test: `crates/glab-tui/src/app.rs`

**Step 1: Add failing isolation tests**

Create two `AppResources` values with different `Theme`, `Icons`, and GitHub usernames. Render equivalent app fixtures sequentially and assert each buffer uses its own colors/icons. Exercise two backend instances and assert current-user state is not shared.

Run: `cargo test -p glab-tui instance_resources`

Expected: FAIL while UI reads process-global `THEME`/`ICONS` or backend user caches.

**Step 2: Introduce instance resources**

Add:

```rust
#[derive(Clone, Debug)]
pub struct AppResources {
    pub theme: Theme,
    pub icons: Icons,
}
```

Store `AppResources` in `App`. Pass `&AppResources` or the narrow `&Theme` / `&Icons` reference down rendering and formatting calls. Delete production `LazyLock`/`RwLock` theme and icon globals.

**Step 3: Localize backend identity caches**

Move GitHub current-user and host-specific mutable caches into `GhBackend`/client instance fields guarded only as required by actual concurrent access. Do not read or mutate process environment in normal request paths.

**Step 4: Run isolation and upstream behavior tests**

Run: `cargo test -p glab-tui instance_resources`

Expected: PASS.

Run: `cargo test -p glab-tui --lib`

Expected: PASS with no behavior loss in config/theme/domain tests.

**Step 5: Commit**

```bash
git add crates/glab-tui/src
git commit -m "refactor(glab): make UI resources instance scoped"
```

---

### Task 3: Make repository and command context explicit

**Files:**
- Create: `crates/glab-tui/src/command.rs`
- Modify: `crates/glab-tui/src/git_helpers.rs`
- Modify: `crates/glab-tui/src/domain/client.rs`
- Modify: `crates/glab-tui/src/backend/{mod.rs,glab.rs,gh.rs}`
- Modify: `crates/glab-tui/src/fetch.rs`
- Modify: `crates/glab-tui/src/utils/cache.rs`
- Test: `crates/glab-tui/src/command.rs`
- Test: `crates/glab-tui/src/git_helpers.rs`

**Step 1: Add failing command-boundary tests**

Define a recording fake runner and assert repository discovery, branch lookup, and one GitLab plus one GitHub API request all carry the same explicit root. Capture `std::env::current_dir()` before and after a root switch and assert it never changes.

```rust
#[async_trait]
trait CommandRunner: Send + Sync {
    async fn output(&self, request: CommandRequest) -> Result<CommandOutput, CommandError>;
}

struct CommandRequest {
    program: OsString,
    args: Vec<OsString>,
    cwd: PathBuf,
}
```

Run: `cargo test -p glab-tui explicit_workspace_root`

Expected: FAIL because upstream helpers derive state from process cwd and construct commands internally.

**Step 2: Inject `CommandRunner` and `ProjectContext`**

Introduce a production Tokio process runner and a test recording runner. Represent detected repository state with an immutable context containing root, host kind, project slug, and current branch. Every `git`, `glab`, and `gh` call must receive `cwd` explicitly and use argument arrays, never shell strings.

**Step 3: Key caches by project context**

Make cache paths derive from explicit project identity/root, not process cwd. Preserve atomic temporary-file + rename writes and offline reads.

**Step 4: Remove cwd mutation and implicit cwd helpers**

Delete all `set_current_dir` calls and zero-argument helpers such as `get_current_branch()`; replace them with `get_current_branch(root, runner)` or values already present in `ProjectContext`.

**Step 5: Run focused tests**

Run: `cargo test -p glab-tui explicit_workspace_root`

Expected: PASS, including unchanged process cwd.

**Step 6: Commit**

```bash
git add crates/glab-tui/src
git commit -m "refactor(glab): inject repository command context"
```

---

### Task 4: Extract the complete key and mouse reducer

**Files:**
- Create: `crates/glab-tui/src/controller.rs`
- Modify: `crates/glab-tui/src/handlers/{mod.rs,overlays.rs,tabs.rs}`
- Modify: `crates/glab-tui/src/app.rs`
- Modify: `crates/glab-tui/src/lib.rs`
- Test: `crates/glab-tui/src/controller.rs`
- Test: `crates/glab-tui/tests/e2e/{keybindings.rs,tabs.rs,combinations.rs}`

**Step 1: Port input contract tests before implementation**

Adapt upstream test scenarios to call a pure `Controller::handle_key`/`handle_mouse` instead of a standalone event loop. Cover at minimum:

- sidebar navigation across all available tabs;
- table selection and detail scrolling;
- global/local search and filters;
- configure, selector, edit, date, save, confirmation, and help overlays;
- diff file tree, hunk navigation, review/comment selection;
- mouse wheel/click routing in overlay, sidebar, content, and detail z-order;
- `q`/Esc semantics returning `ExitRequested` only when no inner overlay consumes the key.

Run: `cargo test -p glab-tui controller_`

Expected: FAIL because reducer logic still lives in upstream `main.rs`.

**Step 2: Define controller outcomes**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerOutcome {
    Unchanged,
    Changed,
    ExitRequested,
    Command(CommandIntent),
    HostAction(HostAction),
}
```

`CommandIntent` is typed business work for the async layer; `HostAction` is work only RimeTerm may perform. Neither directly touches terminal state.

**Step 3: Move reducers mechanically**

Move `handle_mouse_event`, key dispatch, overlay-specific reducers, tab selection, and helper parsing from upstream `main.rs` into `controller.rs`/existing `handlers`. Preserve upstream behavior first; consolidate only duplicate code exposed by the move.

**Step 4: Delete standalone event ownership**

Remove crossterm polling, `EventHandler`, process-global `PAUSED`, raw/alternate-screen transitions, stdout terminal aliases, and standalone shutdown paths from production modules. Keep the typed business completion `Event`, renamed if needed to avoid confusion with crossterm input.

**Step 5: Run reducer suites**

Run: `cargo test -p glab-tui controller_`

Expected: PASS.

Run: `cargo test -p glab-tui --lib`

Expected: PASS.

**Step 6: Commit**

```bash
git add crates/glab-tui/src crates/glab-tui/tests
git commit -m "refactor(glab): extract embedded input controller"
```

---

### Task 5: Build generation-safe asynchronous application ownership

**Files:**
- Create: `crates/glab-tui/src/embed.rs`
- Modify: `crates/glab-tui/src/fetch.rs`
- Modify: `crates/glab-tui/src/event.rs`
- Modify: `crates/glab-tui/src/backend/mod.rs`
- Modify: `crates/glab-tui/src/lib.rs`
- Test: `crates/glab-tui/src/embed.rs`

**Step 1: Add failing lifecycle tests**

Using fake command/backend completions, test:

- construction synchronously returns Loading and schedules repository detection;
- a current-generation completion updates the app;
- switching root increments generation and discards late old-root completion;
- same-kind refresh coalesces while in flight;
- hidden panes do not start periodic refreshes but still drain finished tasks;
- becoming visible performs one overdue refresh;
- `Drop`/`shutdown` aborts tracked tasks;
- offline cache remains visible after refresh failure;
- `next_deadline()` is `None` when no timer is needed.

Run: `cargo test -p glab-tui embedded_lifecycle_`

Expected: FAIL because `EmbeddedApp` does not exist.

**Step 2: Implement the narrow API**

Implement the approved contract from `docs/rimeterm-gitlab-design.md`:

```rust
pub struct EmbeddedApp { /* App, Controller, generation, task set, rx, deadlines */ }

impl EmbeddedApp {
    pub fn new(options: EmbeddedOptions, runtime: tokio::runtime::Handle) -> Self;
    pub fn handle_key(&mut self, key: KeyEvent) -> EmbeddedOutcome;
    pub fn handle_mouse(&mut self, event: MouseEvent, area: Rect) -> EmbeddedOutcome;
    pub fn poll_background(&mut self, now: Instant) -> EmbeddedOutcome;
    pub fn next_deadline(&self) -> Option<Instant>;
    pub fn set_visible(&mut self, visible: bool) -> EmbeddedOutcome;
    pub fn set_workspace_root(&mut self, root: PathBuf) -> EmbeddedOutcome;
    pub fn reload(&mut self) -> EmbeddedOutcome;
    pub fn snapshot(&self) -> EmbeddedState;
    pub fn restore(&mut self, state: &EmbeddedState);
    pub fn complete_host_action(&mut self, id: u64, result: HostActionResult);
    pub fn shutdown(&mut self);
}
```

Do not export `app_mut()`.

**Step 3: Tag all completions**

Every completion carries generation, request id, project context, and operation kind. Validate generation and project both when applying data and when applying mutation completion.

**Step 4: Track abort handles**

Keep one in-flight refresh per kind. Root changes and shutdown abort all tasks; completion application still rejects stale results to cover unavoidable command races.

**Step 5: Run lifecycle tests**

Run: `cargo test -p glab-tui embedded_lifecycle_`

Expected: PASS.

**Step 6: Commit**

```bash
git add crates/glab-tui/src
git commit -m "feat(glab): add generation-safe embedded app"
```

---

### Task 6: Make the complete upstream renderer pane-bounded

**Files:**
- Modify: `crates/glab-tui/src/ui/mod.rs`
- Modify: `crates/glab-tui/src/ui/{diff.rs,helpers.rs,modal.rs,overlays.rs,tabs.rs}`
- Modify: `crates/glab-tui/src/app.rs`
- Modify: `crates/glab-tui/src/embed.rs`
- Test: `crates/glab-tui/tests/e2e/layout.rs`
- Create: `crates/glab-tui/tests/bounded_render.rs`

**Step 1: Add buffer-boundary tests**

Fill a `TestBackend` buffer with a sentinel, render into an offset inner rect, then assert every cell outside that rect remains unchanged. Repeat for:

- normal three-column layout;
- each internal tab;
- detail hidden and zoomed;
- every overlay kind;
- unified and side-by-side diff;
- setup/loading/error/offline states;
- sizes `0×0`, `1×1`, and below the chosen compact-layout breakpoint.

Run: `cargo test -p glab-tui --test bounded_render`

Expected: FAIL while upstream draw assumes the full terminal frame.

**Step 2: Introduce `draw_in`**

```rust
pub fn draw_in(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    resources: &AppResources,
);
```

All layouts, clearing, modal centering, hit rectangles, and minimum-size calculations use `area` as the root. Do not call `frame.area()` except in test-only full-frame wrappers.

**Step 3: Add adaptive breakpoints**

For narrow RimeTerm panes, collapse detail first, then sidebar; preserve internal tab switching through keys. If there is insufficient space for safe table rendering, display a compact in-pane size notice rather than partial geometry.

**Step 4: Cache absolute hit rectangles**

Store sidebar/content/detail/overlay rectangles in absolute terminal coordinates matching the host's mouse events. Clear obsolete rects on every render mode transition.

**Step 5: Run layout and boundary tests**

Run: `cargo test -p glab-tui --test bounded_render`

Expected: PASS.

Run: `cargo test -p glab-tui --test e2e layout`

Expected: PASS after adapting upstream harness to `draw_in`.

**Step 6: Commit**

```bash
git add crates/glab-tui/src/ui crates/glab-tui/src/app.rs crates/glab-tui/src/embed.rs crates/glab-tui/tests
git commit -m "feat(glab): render complete UI inside pane bounds"
```

---

### Task 7: Preserve setup, offline, and error states around the full app

**Files:**
- Modify: `crates/glab-tui/src/embed.rs`
- Modify: `crates/glab-tui/src/app.rs`
- Modify: `crates/glab-tui/src/ui/mod.rs`
- Modify: `crates/glab-tui/src/git_helpers.rs`
- Modify: `crates/rimeterm-config/src/install_hint.rs`
- Test: `crates/glab-tui/src/embed.rs`
- Test: `crates/rimeterm-config/src/install_hint.rs`

**Step 1: Add failing bootstrap-state tests**

Cover `CliMissing`, `NotAuthenticated`, `NotRepository`, load-from-cache/offline, and ready states for both hosts. Assert:

- setup text names `git` plus the correct host CLI and auth command;
- no instruction installs the `glab-tui` UI binary;
- ready output contains no setup guide;
- errors do not erase already rendered cache;
- setup guide keyboard/mouse scrolling remains bounded and Unicode-safe.

Run: `cargo test -p glab-tui setup_state_`

Expected: FAIL against the newly imported app until bootstrap state is integrated.

**Step 2: Model bootstrap explicitly**

Use an embedded shell state such as `Detecting | Setup(SetupProblem) | Ready(App) | Offline(App, ErrorSummary)`. Do not fake empty upstream tables for setup failures.

**Step 3: Reuse centralized install hints**

Keep OS-specific command guidance in `rimeterm-config::install_hint`; expose structured/plain lines needed by Glab without duplicating package-manager strings in the fork.

**Step 4: Run focused tests**

Run: `cargo test -p rimeterm-config install_hint`

Expected: PASS.

Run: `cargo test -p glab-tui setup_state_`

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/glab-tui crates/rimeterm-config/src/install_hint.rs
git commit -m "feat(glab): preserve setup and offline states"
```

---

### Task 8: Route browser, editor, clipboard, and exit through host actions

**Files:**
- Modify: `crates/glab-tui/src/embed.rs`
- Modify: `crates/glab-tui/src/controller.rs`
- Modify: `crates/glab-tui/src/editor.rs`
- Modify: `crates/glab-tui/src/backend/{mod.rs,glab.rs,gh.rs}`
- Modify: `crates/glab-tui/src/app.rs`
- Test: `crates/glab-tui/src/embed.rs`

**Step 1: Add failing host-action tests**

Assert browser commands return `HostAction::OpenUrl`, multiline editor requests return `HostAction::EditText`, copy returns a clipboard host action/request, and top-level quit returns `ExitRequested`. Assert none invokes a real browser/editor, changes terminal mode, or terminates the process.

Run: `cargo test -p glab-tui host_action_`

Expected: FAIL while upstream calls external programs directly.

**Step 2: Replace direct side effects**

Move URL construction into backend/domain helpers but return the URL. Replace terminal editor integration with request/response IDs. Preserve pending form state while a host editor action is outstanding; apply only the matching completion.

**Step 3: Make unsupported actions explicit**

Construct `EmbeddedFeatures` with external editor disabled until RimeTerm has a concrete editor workflow. Disabled actions render/return a clear nonfatal status rather than executing a fallback process.

**Step 4: Run host-action tests**

Run: `cargo test -p glab-tui host_action_`

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/glab-tui/src
git commit -m "refactor(glab): route external actions through host"
```

---

### Task 9: Replace the RimeTerm `GlabPane` adapter cleanly

**Files:**
- Modify: `crates/rimeterm-tui/src/glab_pane.rs`
- Modify: `crates/rimeterm-tui/src/app.rs`
- Modify: `crates/rimeterm-tui/src/lib.rs`
- Modify: `crates/rimeterm-core/src/pane.rs`
- Test: `crates/rimeterm-tui/src/glab_pane.rs`
- Test: `crates/rimeterm-tui/src/app.rs`

**Step 1: Add failing pane integration tests**

Test observable contracts:

- construction receives explicit `tokio::runtime::Handle`, root, initial Todos/Notifications tab, and mapped resources;
- render delegates the exact pane rect and does not touch neighboring sentinel cells;
- keys/mouse reach the controller only after host chrome routing;
- `poll_background` returns dirty on applied completion or deadline refresh;
- F5 reloads the active project/tab;
- workspace root changes reach the embedded app once;
- exit activates Files rather than closing RimeTerm;
- clipboard requests use `arboard` at the host boundary;
- browser/editor requests never execute inside `glab-tui`.

Run: `cargo test -p rimeterm-tui glab_pane`

Expected: FAIL against the old Todo-only adapter.

**Step 2: Add visibility to the pane contract**

If the host currently lacks an observable visibility transition, add a default no-op `PaneProvider::set_visible(bool)` and invoke it when tab membership/active selection changes. Implement it in `GlabPane` only; avoid forcing unrelated panes to change.

**Step 3: Rewrite `GlabPane` around the narrow embedded API**

Store only pane id, root, embedded app, and pending host integration state. Remove calls to `GlabSnapshot`, `GlabStatus`, `ProcessBackend`, simplified selection APIs, and other Todo-only types.

**Step 4: Handle outcomes centrally**

Translate `Changed` to redraw, `ExitRequested` to activation of Files through an app-owned pending pane action, and `HostAction` to existing RimeTerm clipboard/browser/editor infrastructure. Never perform I/O from `render`.

**Step 5: Run pane and core tests**

Run: `cargo test -p rimeterm-tui glab_pane`

Expected: PASS.

Run: `cargo test -p rimeterm-core pane`

Expected: PASS.

**Step 6: Commit**

```bash
git add crates/rimeterm-core/src/pane.rs crates/rimeterm-tui/src
git commit -m "feat(glab): embed full UI in native pane"
```

---

### Task 10: Persist stable Glab UI state

**Files:**
- Modify: `crates/rimeterm-config/src/memory_state.rs`
- Modify: `crates/rimeterm-tui/src/glab_pane.rs`
- Modify: `crates/rimeterm-tui/src/app.rs`
- Modify: `crates/glab-tui/src/embed.rs`
- Test: `crates/rimeterm-config/src/memory_state.rs`
- Test: `crates/rimeterm-tui/src/glab_pane.rs`

**Step 1: Add failing round-trip tests**

Round-trip a state containing active internal tab, per-tab cursor, search query, detail visibility/scroll, and column/filter/group/sort choices. Assert overlay, loading, error, in-flight request, auth data, and token-like strings are absent.

Run: `cargo test -p rimeterm-config glab_state_`

Expected: FAIL because Glab currently stores an empty generic `PaneState`.

**Step 2: Add a typed Glab pane state payload**

Use serde-defaulted, backward-compatible fields under the existing pane state envelope. Keep backend/project identity display-only. Unknown/invalid saved tab/column values fall back to upstream defaults rather than failing all UI state loading.

**Step 3: Connect snapshot/restore**

Map config state to `EmbeddedState` in `GlabPane::snapshot_state`/`restore_state`. Restore only after app construction and before first ready render; do not restore transient overlays.

**Step 4: Run persistence tests**

Run: `cargo test -p rimeterm-config glab_state_`

Expected: PASS.

Run: `cargo test -p rimeterm-tui glab_state_`

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/rimeterm-config/src/memory_state.rs crates/rimeterm-tui/src/glab_pane.rs crates/rimeterm-tui/src/app.rs crates/glab-tui/src/embed.rs
git commit -m "feat(glab): persist stable full UI state"
```

---

### Task 11: Map the RimeTerm theme and verify complete interactions

**Files:**
- Modify: `crates/rimeterm-tui/src/glab_pane.rs`
- Modify: `crates/rimeterm-tui/src/app.rs`
- Modify: `crates/glab-tui/src/config.rs`
- Test: `crates/rimeterm-tui/src/glab_pane.rs`
- Test: `crates/glab-tui/tests/e2e/*.rs`

**Step 1: Add failing theme propagation tests**

Assert changing the app-wide RimeTerm theme updates an already-created Glab pane's focused border, normal text, selection, status, modal, and diff colors on the next frame without reconstructing the pane.

Run: `cargo test -p rimeterm-tui glab_theme_`

Expected: FAIL until Glab participates in `set_markdown_theme` propagation.

**Step 2: Implement semantic theme mapping**

Translate RimeTerm's active palette into a full Glab `Theme` instance. Preserve upstream semantic distinctions but do not introduce an independent persistent theme selector inside the embedded pane unless explicitly enabled by `EmbeddedFeatures`.

**Step 3: Adapt all upstream interaction tests to the embedded harness**

Run every imported e2e scenario against `EmbeddedApp`/`TestBackend`, including all tabs, combinations, pagination, workspace handling, filters, configure menus, mutations, diff review, and mouse routing. Eliminate tests that merely inspect source strings; retain behavioral contracts.

Run: `cargo test -p glab-tui --test e2e`

Expected: PASS.

Run: `cargo test -p rimeterm-tui glab_`

Expected: PASS.

**Step 4: Commit**

```bash
git add crates/glab-tui crates/rimeterm-tui/src
git commit -m "feat(glab): integrate theme and full interactions"
```

---

### Task 12: Delete the simplified implementation and prove the clean cutover

**Files:**
- Modify/Delete obsolete content: `crates/glab-tui/src/lib.rs`
- Modify: `crates/rimeterm-tui/src/glab_pane.rs`
- Modify: `docs/rimeterm-gitlab-design.md`
- Modify: `crates/glab-tui/UPSTREAM.md`
- Modify: `ACKNOWLEDGEMENTS.md`
- Test: `crates/glab-tui/tests/metadata.rs`

**Step 1: Add clean-cutover metadata assertions**

Assert there is no binary target and no production source module exposing the old simplified types. Assert the vendored license/provenance and full source inventory remain present.

**Step 2: Remove old code completely**

Delete `GlabSnapshot`, `GlabStatus`, simplified `Backend`/`ProcessBackend`, Todo-only `EmbeddedApp` state/render/input, duplicate install guide logic, and obsolete tests. Keep no deprecated aliases or feature gates.

**Step 3: Update documentation and attribution**

Document the completed full-app embedding surface and patch locations in `UPSTREAM.md`; update acknowledgements with upstream URL, commit, MIT attribution, and RimeTerm modifications. Amend the design document only where implementation established a more precise final API; do not rewrite historical decisions.

**Step 4: Verify no forbidden runtime path remains**

Use structural/text searches over `crates/glab-tui/src` for `enable_raw_mode`, `EnterAlternateScreen`, `CrosstermBackend<Stdout>`, `set_current_dir`, `process::exit`, standalone `event::poll/read`, and a `[[bin]]` manifest. Every match must be absent or test documentation only.

**Step 5: Run focused complete verification**

Run: `cargo fmt --all -- --check`

Expected: PASS.

Run: `cargo test -p glab-tui`

Expected: all library, bounded-render, metadata, and embedded e2e suites PASS.

Run: `cargo test -p rimeterm-config glab_`

Expected: PASS.

Run: `cargo test -p rimeterm-tui glab_`

Expected: PASS.

Run: `cargo check --workspace --locked`

Expected: PASS with no new warnings from Glab code.

**Step 6: Commit the cutover**

```bash
git add ACKNOWLEDGEMENTS.md Cargo.toml Cargo.lock crates/glab-tui crates/rimeterm-tui crates/rimeterm-config docs/rimeterm-gitlab-design.md
git commit -m "refactor(glab): remove simplified todo-only view"
```

---

### Task 13: Run the real RimeTerm smoke path and final verification

**Files:**
- Create or modify only if the new observable contract lacks coverage: `crates/rimeterm-tui/tests/glab_full_ui.rs`
- Verify: all files changed by Tasks 1–12

**Step 1: Build deterministic fake CLIs**

Use temporary executable `git`, `glab`, and `gh` fixtures that return the imported JSON payloads for repository detection and each read operation. Record program, arguments, cwd, and process lifetime. Do not contact real GitLab/GitHub or user credentials.

**Step 2: Smoke the actual RimeTerm binary**

Launch `rimeterm` with the temporary fixture repo and PATH. Exercise through the real pane/event path:

1. activate Glab;
2. observe initial Todos/Notifications;
3. visit Issues, MR/PR, Pipelines/Actions, Jobs, Runners, Releases, Milestones, Branches, Environments, and Terminal log;
4. select a row, open/close detail, search/filter/sort/configure columns;
5. open a diff and navigate its file tree/hunks;
6. trigger a mutation up to its confirmation overlay, then cancel;
7. switch workspace root and verify old completion data never appears;
8. hide/show the tab and verify deadline refresh behavior;
9. press F5 and verify current-tab refresh;
10. quit the embedded app and verify focus returns to Files while RimeTerm remains alive.

**Step 3: Inspect process behavior**

Assert no child named `glab-tui`, no PTY allocation for Glab, and only short-lived fixture `git`/`glab`/`gh` processes for business requests. Assert recorded cwd always equals the active fixture root.

**Step 4: Run final test matrix**

Run: `cargo test -p glab-tui`

Run: `cargo test -p rimeterm-config`

Run: `cargo test -p rimeterm-tui`

Run: `cargo test --workspace --locked`

Run: `cargo fmt --all -- --check`

Expected: all commands PASS; existing explicitly ignored tests remain ignored, with no new failures or warnings attributable to the Glab cutover.

**Step 5: Run mandatory code review**

Use `requesting-code-review` and `code-review` against the full branch diff. Resolve every correctness, stale-caller, task-lifecycle, cwd-isolation, rendering-boundary, secret-handling, and license finding; rerun the affected focused test after each correction, then rerun the final matrix once.

**Step 6: Commit any final test-only or review corrections**

Use the smallest applicable Conventional Commit subject, then verify the worktree is clean. Do not squash implementation history until integration strategy is chosen through `finishing-a-development-branch`.
