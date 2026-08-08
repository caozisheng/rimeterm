# rimeterm

**A TUI-native terminal built for coding agents.** Runs inside any modern terminal (Windows Terminal / WezTerm / kitty / iTerm2 / Alacritty) and multiplexes PTY sessions like tmux — with a first-class slot for the agent.

<img width="1899" height="1108" alt="image" src="https://github.com/user-attachments/assets/c00be4a8-4526-495c-87e5-b730269e43e0" />

## Layout

One screen, four zones on a 2×2 grid. Every zone is a tab strip you can reorder, hide, or hot-swap.

```
┌─────────────────────────────────────────────────────────┬──────────┐
│  files · todo · fr                                      │  agents  │
├─────────────────────────────────────────────────────────┼──────────┤
│  git · glab · sysmon · agtop · models · stock · zones   │  shells  │
└─────────────────────────────────────────────────────────┴──────────┘
```

### Left column

- **files** — native dual-pane file explorer. `Alt+V` opens the cursor'd file in a modal viewer (markdown / code / image, tree-sitter highlighted).
- **todo** — embedded [Tuxedo](https://github.com/webstonehq/tuxedo) todo.txt manager. Global, not per-workspace — see [Global Todo](#global-todo).
- **fr** — Fast Resume fuzzy search over coding-agent session history with live preview. `Ctrl+R` resumes the selected session in a fresh agent tab.
- **git** — in-process `gix` snapshot: working-tree changes + serie-style commit graph, tree-sitter diff highlighting.
- **glab** — in-process GitLab / GitHub view backed by the `glab` / `gh` CLI: issues, MRs/PRs, pipelines, notifications, milestones, and more.
- **sysmon** — CPU / memory / disks / network. Optional GPU (NVML) and Docker (bollard) behind feature flags.
- **agtop** — process monitor for detected coding-agent CLIs; session enrichment + chip header + detail popup.
- **models** — browses the [models.dev](https://models.dev) catalog.
- **stock** — A-share / HK / US quote lists via [akshare](https://github.com/Cricle/akshare-rs).
- **zones** — world map + user-curated timezone watchlist.

### Right column

- **agents** — PTY tabs for coding-agent CLIs (`omp` / `codex` / `claude` / `pi` / …). Auto-detected on `$PATH`; `Ctrl+Shift+P` picks one, `Ctrl+P` opens the command palette.
- **shells** — plain interactive shell PTY tabs.

Every PTY pane has scrollback + inline scrollbar, mouse text selection, and `Ctrl+Shift+C/V` clipboard. Layout ratios and agent choice persist per workspace.

## What's inside

- **Zero external processes** — files, git, sysmon, agtop, models, stock, zones, todo, and session search are all in-process. Vendored [Tuxedo](https://github.com/webstonehq/tuxedo) powers `todo`; the vendored Fast Resume index/search stack powers `fr`. Retired external essentials (yazi, gitui, bottom, trippy) leave no shims behind.
- **`rimectl` IPC** — line-delimited JSON over a named pipe (Windows) or Unix socket. Every UI command is scriptable from tests, git hooks, or *other* agents.
- **Native mouse** — click / drag / scroll on tabs, dividers, selections, and shell prompts. Right-click is context-aware.
- **Themes** — 8 curated palettes (`Alt+T` cycles), applied uniformly across chrome and the markdown viewer.
- **Upgrade check** — silent background probe against GitHub Releases on startup; a red `⚠ 有新版本 vX.Y.Z` chip in the hint bar's bottom-right opens the Upgrade modal on click. Silent when offline.
- **Windows first-class** — ConPTY backend, Nerd Font fallback, MSI installer with SHA-256-verified download, and an Explorer right-click entry that opens the clicked folder as the workspace root.

## Workspace semantics

The Files pane's current root determines the active workspace:

1. Starting at the Files root, rimeterm walks upward.
2. The nearest directory containing a `.git` directory or `.git` file is the workspace root.
3. If no ancestor contains `.git`, the Files root itself is the workspace root.

The Git pane and newly opened agent sessions follow this resolved workspace. The Todo pane does not: it always uses the same user-level files.

## Global Todo

The top-left **todo** tab stores standard todo.txt data at:

```text
~/.rimeterm/tuxedo/todo.txt
~/.rimeterm/tuxedo/done.txt
```

If `RIMETERM_HOME` is set, both files live under `$RIMETERM_HOME/tuxedo/`.
Use standard `+project` and `@context` tags to organize tasks. Rimeterm never
derives Todo projects from the active workspace. Tuxedo handles atomic writes,
external-edit detection, completion, filtering, recurrence, and archival.

## Install

Grab the installer for your platform from the [latest release] — MSI on Windows, `.deb` on Debian/Ubuntu, or a `.dmg` on macOS (Apple Silicon). Nothing else ships alongside — a single `rimeterm` + `rimectl` binary is the entire payload.

From source:

```bash
cargo install --path crates/rimeterm --bin rimeterm
cargo install --path crates/rimectl  --bin rimectl
```

Then run `rimeterm` from any terminal.

## More

- Third-party attributions: [`ACKNOWLEDGEMENTS.md`](ACKNOWLEDGEMENTS.md)
- License: Apache-2.0, see [`LICENSE`](LICENSE).

[latest release]: https://github.com/caozisheng/rimeterm/releases/latest
