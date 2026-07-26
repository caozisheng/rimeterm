//! OS shell integration — "Open with rimeterm here" right-click entry.
//!
//! Windows-only for now. Writes to `HKCU\Software\Classes\Directory\...`
//! so no admin token is needed; on uninstall we `reg delete /f` the
//! subtree. Two verbs land in one call so the entry shows up whether
//! the user right-clicks a folder ITEM in Explorer or the empty
//! BACKGROUND inside a folder — both cases are what people mean by
//! "open this folder in rimeterm".
//!
//! macOS / Linux integrations are stubbed as "not supported" and left
//! for a future skill (Finder Services / .desktop file / `xdg-mime`).
//!
//! The registry writes use the bundled `reg.exe` binary rather than
//! pulling in the `winreg` crate — this saves ~30 crates from the
//! dependency tree for a feature that touches four keys.

#![cfg_attr(not(windows), allow(dead_code))]

/// Human-readable label shown in the Explorer right-click menu.
const MENU_LABEL: &str = "Open with rimeterm here";

/// Registry key name used for both the Directory and
/// Directory\Background verbs. Deliberately capitalized to match the
/// `MenuText` casing users see in Explorer.
const VERB_KEY: &str = "Rimeterm";

/// Two parents that need the verb — right-clicking a folder ICON
/// (`Directory`) and right-clicking the empty area INSIDE a folder
/// (`Directory\Background`) are separate Shell namespaces. Users
/// expect the entry in both places.
const PARENT_KEYS: &[&str] = &[
    r"Software\Classes\Directory\shell",
    r"Software\Classes\Directory\Background\shell",
];

/// Return `Some(true)` when the integration is currently installed,
/// `Some(false)` when it isn't, and `None` on unsupported platforms /
/// probe failure. `reg query` on a missing key exits non-zero but
/// doesn't print to stderr — we only trust the exit status.
pub(crate) fn probe() -> Option<bool> {
    #[cfg(windows)]
    {
        // We consider the integration installed iff BOTH verbs are
        // present. A partial install (e.g. a previous version wrote
        // only Background) is treated as "not installed" so the next
        // Install cleanly re-creates both.
        for parent in PARENT_KEYS {
            let key = format!(r"HKCU\{parent}\{VERB_KEY}");
            match reg_query_exists(&key) {
                Some(true) => continue,
                Some(false) => return Some(false),
                None => return None,
            }
        }
        Some(true)
    }
    #[cfg(not(windows))]
    {
        Some(false)
    }
}

/// Install the Explorer right-click entry. Idempotent: `reg add /f`
/// overwrites existing values so re-running is safe. Returns the
/// exe path that was registered on success (handy for the hint bar).
#[cfg(windows)]
pub(crate) fn install() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))?;
    // Canonicalize to resolve symlinks / `.\..\` noise so re-running
    // rimeterm from a different spawn location doesn't leave stale
    // registry entries pointing at the previous exe. THEN strip the
    // `\\?\` extended-length prefix — canonicalize adds it on
    // Windows, but Explorer's `shell\<verb>\command` parser refuses
    // to launch `\\?\C:\...` (treats it as a broken UNC path and
    // silently drops the click). CreateProcess is fine either way,
    // Explorer is not — see MS docs on "Naming Files, Paths, and
    // Namespaces". `\\?\UNC\server\share\...` (a real UNC extended
    // path) is kept as-is because rewriting THAT to bare `\\server\`
    // is a lossless normalization Explorer accepts.
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let exe_str = strip_extended_prefix(exe.to_string_lossy().as_ref());

    // Explorer substitutes `%V` with the target folder. For the
    // Directory verb `%V` is the clicked folder; for the Background
    // verb it's the folder the user right-clicked INSIDE. `main.rs`
    // then reads argv[1] as the workspace root.
    let command_line = format!("\"{exe_str}\" \"%V\"");

    for parent in PARENT_KEYS {
        let verb_key = format!(r"HKCU\{parent}\{VERB_KEY}");
        let command_key = format!(r"{verb_key}\command");

        // Verb: default value = menu label, `Icon` = exe (Explorer
        // extracts the first icon resource).
        reg_add_default(&verb_key, MENU_LABEL)
            .map_err(|e| format!("write {verb_key}: {e}"))?;
        reg_add_value(&verb_key, "Icon", &exe_str)
            .map_err(|e| format!("write {verb_key}\\Icon: {e}"))?;

        // Command: default value = `"exe" "%V"`.
        reg_add_default(&command_key, &command_line)
            .map_err(|e| format!("write {command_key}: {e}"))?;
    }
    Ok(std::path::PathBuf::from(exe_str))
}

/// Drop the Win32 extended-length path prefix (`\\?\`) if present,
/// leaving a plain drive-letter or UNC path Explorer can hand to
/// CreateProcess. `\\?\UNC\server\share\file` collapses to
/// `\\server\share\file`.
#[cfg(windows)]
fn strip_extended_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

#[cfg(not(windows))]
pub(crate) fn install() -> Result<std::path::PathBuf, String> {
    Err("right-click integration is Windows-only for now".into())
}

/// Remove the Explorer right-click entry. Idempotent: silently
/// succeeds when the keys don't exist (they might have been removed
/// by hand or never installed).
#[cfg(windows)]
pub(crate) fn uninstall() -> Result<(), String> {
    for parent in PARENT_KEYS {
        let verb_key = format!(r"HKCU\{parent}\{VERB_KEY}");
        // Ignore "key not found" — that's already the desired state.
        // `reg_query_exists` handles the not-found case; only real
        // failures propagate.
        if reg_query_exists(&verb_key) == Some(true)
            && let Err(e) = reg_delete(&verb_key)
        {
            return Err(format!("delete {verb_key}: {e}"));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn uninstall() -> Result<(), String> {
    Err("right-click integration is Windows-only for now".into())
}

// --- reg.exe wrappers ---------------------------------------------------
//
// All three helpers are tiny shell-outs, but they need matching
// argument conventions:
//   - `reg add <key> /f`  → creates a key without a default value.
//   - `reg add <key> /ve /d "..." /f` → sets the (Default) value.
//   - `reg add <key> /v <name> /d "..." /f` → sets a named value.
// We take the "sledgehammer" flag `/f` everywhere so `reg` never
// prompts for confirmation.

#[cfg(windows)]
fn reg_query_exists(key: &str) -> Option<bool> {
    use std::process::Command;
    let output = Command::new("reg")
        .args(["query", key])
        .creation_flags_no_window()
        .output()
        .ok()?;
    // `reg query` prints the key contents on success and exits 0; on
    // missing keys it exits with code 1 and writes "ERROR: The system
    // was unable to find the specified registry key or value." to
    // stderr. Trust the exit code.
    Some(output.status.success())
}

#[cfg(windows)]
fn reg_add_default(key: &str, value: &str) -> std::io::Result<()> {
    reg_run(&["add", key, "/ve", "/d", value, "/f"])
}

#[cfg(windows)]
fn reg_add_value(key: &str, name: &str, value: &str) -> std::io::Result<()> {
    reg_run(&["add", key, "/v", name, "/d", value, "/f"])
}

#[cfg(windows)]
fn reg_delete(key: &str) -> std::io::Result<()> {
    reg_run(&["delete", key, "/f"])
}

#[cfg(windows)]
fn reg_run(args: &[&str]) -> std::io::Result<()> {
    use std::process::Command;
    let output = Command::new("reg")
        .args(args)
        .creation_flags_no_window()
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim();
        Err(std::io::Error::other(if msg.is_empty() {
            format!("reg exited with {}", output.status)
        } else {
            msg.to_string()
        }))
    }
}

/// Suppress the flash-of-console-window that spawning `reg.exe` from
/// a TUI process produces on Windows. `CREATE_NO_WINDOW` is
/// `0x08000000` (see winbase.h). Kept as an extension trait so the
/// call sites stay one-liners.
#[cfg(windows)]
trait CommandNoWindow {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

#[cfg(windows)]
impl CommandNoWindow for std::process::Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW — see Win32 process-creation flags.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        self.creation_flags(CREATE_NO_WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_returns_something() {
        // Contract: probe never panics and always returns a value.
        // On CI without registry access it may return None; on any
        // supported platform it returns Some(_).
        let _ = probe();
    }

    #[cfg(windows)]
    #[test]
    fn strip_extended_prefix_drive_letter() {
        assert_eq!(
            strip_extended_prefix(r"\\?\C:\Users\z\rimeterm.exe"),
            r"C:\Users\z\rimeterm.exe"
        );
    }

    #[cfg(windows)]
    #[test]
    fn strip_extended_prefix_unc() {
        assert_eq!(
            strip_extended_prefix(r"\\?\UNC\server\share\bin\rimeterm.exe"),
            r"\\server\share\bin\rimeterm.exe"
        );
    }

    #[cfg(windows)]
    #[test]
    fn strip_extended_prefix_plain_path_untouched() {
        assert_eq!(
            strip_extended_prefix(r"C:\Program Files\rimeterm\rimeterm.exe"),
            r"C:\Program Files\rimeterm\rimeterm.exe"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn install_uninstall_error_on_non_windows() {
        assert!(install().is_err());
        assert!(uninstall().is_err());
    }
}
