# rimeterm

**A TUI-native terminal built for coding agents.** Runs inside any modern terminal (Windows Terminal / WezTerm / kitty / iTerm2 / Alacritty) and multiplexes PTY sessions like tmux — with a first-class slot for the agent.

<img width="1899" height="1108" alt="image" src="https://github.com/user-attachments/assets/c00be4a8-4526-495c-87e5-b730269e43e0" />

## Layout

One screen, four zones in a 2×2 grid:

|                                                                                                                                                                                                                                                                                                                            **Top-left** |                                                                                                                                                                                                             **Top-right** |
| :---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------: | :-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------: |
| · **Files / todo / FR**<br/>· Native dual-pane file explorer<br/>· Embedded [Tuxedo](https://github.com/webstonehq/tuxedo) todo.txt task manager<br/>· Fast Resume session-history fuzzy search + live preview<br/>· `Ctrl+R` resumes the selected session as a new agent tab<br/>· `Alt+V` modal viewer for the cursor'd file (markdown / code / image) | **agents**<br/>· Coding-agent PTY (`omp` / `codex` / `claude` / `pi` / …)<br/>· `Ctrl+Shift+P` picker (`agents.pick.*`)<br/>· Scrollback + inline scrollbar<br/>· Mouse text selection · `Ctrl+Shift+C/V` clipboard |

|                                                                                                                                                                                                                                                                                                                          **Bottom-left** |                                                                                                                                                                                                           **Bottom-right** |
| :---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------: | :-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------: |
| · **git / sysmon / agtop / models**<br/>· Native `gix` snapshot: working-tree changes + commit graph<br/>· Tree-sitter diff highlighting<br/>· In-process **Sysmon** (CPU / memory / disks / network, optional GPU + Docker)<br/>· In-process **agtop** monitors detected agent processes<br/>· **models** browses the [models.dev](https://models.dev) catalog | **Shells**<br/>· Interactive shell PTY tabs |

Every zone is tabbed and hot-swappable. Layout ratios and agent choice persist per workspace; Todo is global for the current user and does not change when the workspace changes.

## What's inside

- **Agent-first** — auto-detects coding-agent CLIs on `$PATH`, picker in the app menu and command palette (`Ctrl+P`).
- **Native files + git + sysmon + models + todo + session search** — zero external processes for the left column; the vendored Tuxedo task manager powers `todo`, and the vendored Fast Resume index/search stack powers `FR`.
- **`rimectl` IPC** — line-delimited JSON over a named pipe (Windows) or Unix socket. Every UI command is scriptable from tests, git hooks, or *other* agents.
- **Native mouse** — click / drag / scroll on tabs, dividers, selections, and shell prompts. Right-click is context-aware.
- **Themes** — 8 curated palettes (`Alt+T` cycles), applied uniformly across chrome and the markdown viewer.
- **Windows first-class** — ConPTY backend, Nerd Font fallback, MSI installer, and an Explorer right-click entry that opens the clicked folder as the workspace root.

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

Grab the archive for your platform from the [latest release] — MSI on Windows, `.deb` on Debian/Ubuntu, `.pkg` on macOS, or a plain `.tar.gz`/`.zip` extractable anywhere. Nothing else ships alongside — a single `rimeterm` + `rimectl` binary is the entire payload.

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
