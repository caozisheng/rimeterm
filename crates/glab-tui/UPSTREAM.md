# Upstream

This crate is a bounded library fork of `glab-tui` at upstream commit
`c11c244a43d9cc1c71952ab887d09c9bba9476f3`.

The upstream MIT license is retained in `LICENSE` from the pinned snapshot.

Imported snapshot content:

- `src/app.rs`
- `src/backend/{mod.rs,gh.rs,glab.rs}`
- `src/domain/*.rs`
- `src/{cli.rs,config.rs,editor.rs,entity_editor.rs,event.rs,fetch.rs,git_helpers.rs,keybinding.rs,templates.rs}`
- `src/handlers/{mod.rs,overlays.rs,tabs.rs}`
- `src/ui/{mod.rs,diff.rs,helpers.rs,modal.rs,overlays.rs,tabs.rs}`
- `src/utils/{mod.rs,cache.rs,format.rs,ui.rs,update.rs}`
- `src/themes/*.toml`
- `tests/e2e/*.rs`, `tests/fixtures/*.json`, and `tests/mocks/{glab,gh}`

Intentionally excluded upstream content:

- `src/main.rs` and the `[[bin]]` target. RimeTerm embeds this crate as a
  library and owns terminal setup, input dispatch, rendering, process cwd, and
  workspace-root selection.
- Repository-only upstream assets such as `.github`, installer/release
  packaging, Docker files, `README.md`, and upstream `Cargo.lock`.

Temporary Task 1 library boundary:

- `src/lib.rs` keeps the existing simplified public API (`EmbeddedApp`,
  `GlabSnapshot`, `GlabStatus`, `ProcessBackend`, and related helpers) so
  `rimeterm-tui` continues to compile until the planned clean cutover.
- Full upstream modules are wired into the library crate behind crate-private
  modules, except `handlers`/`utils` remain public where the imported snapshot
  already expects that shape.
- `AppTerminal` is a private compatibility type alias for imported upstream
  handlers/editor paths that still reference the standalone terminal boundary.
  It does not add a binary target or restore upstream PTY/raw-mode ownership;
  those side-effecting paths are scheduled for later extraction.
