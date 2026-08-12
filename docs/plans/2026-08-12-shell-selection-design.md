# Shell selection design

## Problem

Settings currently emits `SetShell` and replaces the in-memory `App::shell_choice`, but does not persist the selection. Existing PTYs cannot change their child process, so only new shell tabs can observe the choice. The picker also enumerates with `CoreConfig::default()` instead of the loaded application config, which makes custom shell hints invisible.

## Decision

Treat the setting as a shell executable choice, not an external terminal-emulator choice. Hyper, Windows Terminal, WezTerm, and Alacritty are therefore out of scope.

Persist the selected resolved executable path in a small global `shell.toml` under the RimeTerm data directory. Startup loads that path first and uses it when it still exists; otherwise it falls back to the existing `[core].shell_win` or `[core].shell_unix` detection. This avoids rewriting the user's primary TOML, preserves comments, and gives the UI an unambiguous global setting.

The picker receives the loaded config's platform-specific hints and combines them with a broader built-in candidate list. It only displays executables resolvable on the current host and deduplicates normalized resolved paths. Windows probes `pwsh`, `powershell`, `cmd`, `nu`, `bash`, `fish`, `zsh`, `xonsh`, and `elvish`. Unix probes `fish`, `zsh`, `bash`, `nu`, `xonsh`, `elvish`, `dash`, `ksh`, `tcsh`, and `sh`.

## Runtime flow

1. Load regular config.
2. Load the global shell preference.
3. If its path is still a file, classify and use it; otherwise use `detect_default_shell` with the loaded config.
4. Opening Settings enumerates candidates using the same loaded config hints.
5. Selecting a row persists its resolved path, updates `App::shell_choice`, and reports that only new shell tabs are affected.
6. `new_shell_tab_in` passes the updated choice into `spawn_shell`; existing PTYs remain unchanged.

Persistence failures keep the prior active selection and surface a settings hint. A stale preference is ignored without preventing startup.

## Verification

Unit tests cover preference round-trip, stale preference fallback, expanded deterministic candidate names, deduplication, and Settings shell actions. A smoke scenario launches a short-lived command through the selected executable path to prove the selected program reaches PTY spawning.
