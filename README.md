# rimeterm

**A TUI-native terminal built for coding agents.** Runs inside any modern terminal (Windows Terminal / WezTerm / kitty / iTerm2 / Alacritty) and multiplexes PTY sessions like tmux — with a first-class slot for the agent.


<img width="1897" height="1102" alt="image" src="https://github.com/user-attachments/assets/b43098dc-56ca-4584-be20-46e47dd84253" />


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
