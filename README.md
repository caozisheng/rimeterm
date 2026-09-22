# rimeterm

**A TUI-native terminal built for coding agents.** Runs inside any modern terminal (Windows Terminal / WezTerm / kitty / iTerm2 / Alacritty) and multiplexes PTY sessions like tmux — with a first-class slot for the agent.

<img width="1908" height="1104" alt="image" src="https://github.com/user-attachments/assets/e3ef898d-c6c5-436f-b688-86c7809f2bf6" />

<img width="1911" height="1105" alt="image" src="https://github.com/user-attachments/assets/9deb36b1-2667-4436-aa91-160440033606" />

## Features

- **Agent-first** — a dedicated agents column auto-detects coding-agent CLIs (`omp`, `codex`, `claude`, `pi`); a persistent session daemon detaches and reattaches sessions.
- **All in-process** — file explorer, git (`gix`), GitLab/GitHub, sysmon, agent monitor, todo, and session search run inside the binary; no external TUIs spawned.
- **tmux-style PTY multiplexing** — scrollback, mouse selection, clipboard, and per-workspace persisted layouts.
- **Multi-workspace tabs** — switch project roots from the workspace strip; each keeps its own layout, tabs, and active agent.
- **Rich `Alt+V` viewer** — tree-sitter code highlighting, images, and mermaid diagrams through kitty / Sixel / iTerm2 graphics protocols.
- **Scriptable via `rimectl`** — every UI command exposed as line-delimited JSON over a named pipe / Unix socket.
- **Cross-pane workflows** — dispatch todo tasks to an agent, resume past sessions via `fr`, track agent CPU / tokens / cost in `agtop`.
- **Desktop pet** — mirrors the main agent's status and current tool intent.
- **Native mouse, 8 themes** — click / drag / scroll everywhere; `Alt+T` cycles palettes.
- **Cross-platform** — Windows (ConPTY, MSI), Debian/Ubuntu `.deb`, macOS `.dmg`, and Termux builds.



## Layout

One screen, four zones on a 2×2 grid. Each zone is a tab group: tabs can be
reordered, hidden, and switched without leaving the workspace. The layout can
also be changed between landscape and vertical modes from the status bar.

```text
┌─────────────────────────────────────────────────────────┬──────────┐
│  files · todo · fr                                      │  agents  │
├─────────────────────────────────────────────────────────┼──────────┤
│  git · glab · sysmon · agtop · pet · models · stock     │  shells  │
│  · zones                                                │          │
└─────────────────────────────────────────────────────────┴──────────┘
```

## Pane Catalog

### Left-top panes

| Pane | Purpose | Main interaction |
|---|---|---|
| **files** | Native dual-pane file explorer; `Alt+V` opens a modal viewer for markdown, code, and images with tree-sitter highlighting. | Navigate, select, preview, and switch tabs. |
| **todo** | Embedded [Tuxedo](https://github.com/webstonehq/tuxedo) todo.txt manager. Global rather than workspace-scoped. | Edit tasks, filter, complete, archive, and dispatch a task to an agent. |
| **fr** | Fast Resume fuzzy search over coding-agent session history. | Search with a live preview; `Ctrl+R` resumes the selected session in a fresh agent tab. |

### Left-bottom panes

| Pane | Purpose | Main interaction |
|---|---|---|
| **git** | In-process `gix` view of working-tree changes, commit history, and diffs with syntax highlighting. | Switch Changes / Commits / Diff / Detail views; scroll and inspect. |
| **glab** | GitLab / GitHub view backed by `glab` / `gh`: issues, MRs/PRs, pipelines, notifications, milestones, and more. | Browse remote project data and refresh the current workspace. |
| **sysmon** | CPU, memory, disks, network, processes, and optional GPU/Docker/cgroup metrics. | `Tab` switches Overview / Processes; filter, sort, and inspect processes. |
| **agtop** | Agent process monitor with CPU, memory, model, tokens, cost, status, session enrichment, and detail popups. | Filter, sort, refresh, and inspect detected coding agents. |
| **pet** | Persistent desktop pet linked to the first agent tab. Its state and activity reflect the main agent, including the current tool intent. | Feed, discipline, clean, toggle lights, give medicine, hatch, and observe agent activity. |
| **models** | Browses the [models.dev](https://models.dev) model catalog. | Search, filter providers, sort models, and inspect context/cost details. |
| **stock** | A-share, HK, and US quote watchlists via [akshare](https://github.com/Cricle/akshare-rs). | Search symbols, switch markets, refresh quotes, and open details. |
| **game** | In-process terminal Pac-Man ported from [tui-game](https://github.com/MXFish/tui-game); high score persists globally. | Arrow keys move, `r` restart, `y`/`n` confirm. |
| **zones** | Braille world map with day/night terminator and a user-curated timezone watchlist. | Add/delete zones, jump home, and inspect local times. |

### Right column

| Zone | Pane | Purpose | Main interaction |
|---|---|---|---|
| Top | **agents** | PTY tabs for coding-agent CLIs such as `omp`, `codex`, `claude`, and `pi`. Available binaries are detected on `$PATH`. | `Ctrl+Shift+P` selects an agent; `Ctrl+P` opens the command palette. |
| Bottom | **shells** | Plain interactive shell PTY tabs. | Run commands with scrollback, selection, clipboard, and mouse support. |

Every PTY pane has scrollback, an inline scrollbar, mouse text selection, and
`Ctrl+Shift+C/V` clipboard actions. Layout ratios, visible tabs, tab order, and
the active agent are persisted per workspace.

### Cross-pane workflows

- **Agent work** — work in an `agents` tab, watch process/resource details in
  `agtop`, and see the main agent's status and current intent reflected in
  `pet`.
- **Task dispatch** — select a Todo task and dispatch its cleaned prompt to a
  selected agent tab; the task remains visible in the global Todo pane.
- **Code review** — preview files from `files`, inspect the working tree in
  `git`, and use `glab` for the corresponding remote issue or merge request.
- **Session recovery** — search old sessions in `fr`, preview their content,
  and resume one into a new agent tab.

## What's inside

- **Zero external processes** — files, git, sysmon, agtop, pet, models, stock, zones, todo, and session search are all in-process. Vendored [Tuxedo](https://github.com/webstonehq/tuxedo) powers `todo`; the vendored Fast Resume index/search stack powers `fr`. Retired external essentials (yazi, gitui, bottom, trippy) leave no shims behind.
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

On Linux the `.deb` is linked against glibc 2.31, so it installs on Ubuntu 20.04+ / Debian 11+ — including WSL images:

```bash
curl -LO https://github.com/caozisheng/rimeterm/releases/latest/download/rimeterm-<version>_amd64.deb
sudo apt install ./rimeterm-<version>_amd64.deb
```

From source (note `--locked`: the committed `Cargo.lock` is currently required — `bisync 0.3.x`, a transitive `gix` dependency, is yanked on crates.io, so fresh resolution fails):

```bash
cargo install --path crates/rimeterm --bin rimeterm --locked
cargo install --path crates/rimectl  --bin rimectl  --locked
```

Then run `rimeterm` from any terminal.

### Android / Termux

One-line install (aarch64 Termux):

```bash
curl -fsSL https://raw.githubusercontent.com/caozisheng/rimeterm/main/scripts/install-termux.sh | sh
```

Downloads the prebuilt `rimeterm` + `rimectl` from the latest release
(cross-compiled against Bionic on CI). Or build from source:

```bash
pkg install rust clang
git clone https://github.com/caozisheng/rimeterm
cd rimeterm
cargo build --release --workspace --bins
cp target/release/rimeterm target/release/rimectl "$PREFIX/bin/"
```

Clipboard operations are unavailable on Android: `arboard` has no Android
backend, so it is only compiled for non-Android targets and the Android build
uses a no-op `clipboard` shim instead.

## More

- Third-party attributions: [`ACKNOWLEDGEMENTS.md`](ACKNOWLEDGEMENTS.md)
- License: Apache-2.0, see [`LICENSE`](LICENSE).

[latest release]: https://github.com/caozisheng/rimeterm/releases/latest
