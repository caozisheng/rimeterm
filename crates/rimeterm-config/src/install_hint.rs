//! **§17 InstallHint** — structured multi-path install hint for external
//! tools whose binaries aren't on `$PATH`.
//!
//! Renders into a single `String` (fits `ExternalToolSpec.install_hint`)
//! that the placeholder pane parses back into distinct lines. Keeping it a
//! string in the config schema means user configs stay declarative TOML;
//! callers who want the structured value build it via this helper and
//! `.to_string()` into the spec.
//!
//! Rendering layout (one line per available path, in preference order):
//! ```text
//! Install one of:
//!   Windows: winget install Clement.bottom
//!   macOS:   brew install bottom
//!   Linux:   sudo apt install bottom
//!   Any:     cargo install --locked bottom
//! Note: needs admin/cap_net_raw on Linux, Npcap on Windows
//! ```
//!
//! Empty paths are skipped so a tool with only a cargo command doesn't
//! render blank "Windows:" rows.

use std::fmt;

/// Multi-path install hint. Each field is `Option<&'static str>` so the
/// zero-value hint (all `None`) renders as an empty string and construction
/// stays a plain struct literal with only the paths that actually work.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstallHint {
    /// `winget install <id>` on Windows 10+.
    pub winget: Option<&'static str>,
    /// `scoop install <bucket>/<name>` on Windows (scoop bucket).
    pub scoop: Option<&'static str>,
    /// `brew install <formula>` on macOS / Linux Homebrew.
    pub brew: Option<&'static str>,
    /// One-line hint for the dominant Linux package manager. Blob-string
    /// (not one per manager) because rimeterm has no way to detect the
    /// distro and the correct answer varies too much (apt / dnf / pacman
    /// / apk). Callers write e.g. `"sudo apt install bottom (Debian);
    /// sudo dnf install bottom (Fedora)"`.
    pub linux: Option<&'static str>,
    /// `cargo install --locked <crate>` — universal fallback.
    pub cargo: Option<&'static str>,
    /// Platform quirks (Npcap requirement, cap_net_raw, image-preview
    /// prerequisite, etc.). Rendered on its own trailing line.
    pub note: Option<&'static str>,
}

impl InstallHint {
    /// True when every install path is empty. Callers can decide to fall
    /// back to a shorter single-line "not on PATH" message.
    pub fn is_empty(&self) -> bool {
        self.winget.is_none()
            && self.scoop.is_none()
            && self.brew.is_none()
            && self.linux.is_none()
            && self.cargo.is_none()
    }
}

impl fmt::Display for InstallHint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            // Empty hint renders as empty string; caller decides fallback.
            return Ok(());
        }
        writeln!(f, "Install one of:")?;
        // Column-aligned labels (7 chars incl colon + space) so the
        // command text lines up regardless of which paths are present.
        // Order: platform-native first, then universal, then note.
        if let Some(cmd) = self.winget {
            writeln!(f, "  Windows: {cmd}")?;
        }
        if let Some(cmd) = self.scoop {
            writeln!(f, "  scoop:   {cmd}")?;
        }
        if let Some(cmd) = self.brew {
            writeln!(f, "  macOS:   {cmd}")?;
        }
        if let Some(cmd) = self.linux {
            writeln!(f, "  Linux:   {cmd}")?;
        }
        if let Some(cmd) = self.cargo {
            writeln!(f, "  Any:     {cmd}")?;
        }
        if let Some(n) = self.note {
            write!(f, "Note: {n}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_hint_renders_empty_string() {
        assert!(InstallHint::default().is_empty());
        assert_eq!(InstallHint::default().to_string(), "");
    }

    #[test]
    fn winget_only_renders_single_row() {
        let h = InstallHint {
            winget: Some("winget install Clement.bottom"),
            ..Default::default()
        };
        let s = h.to_string();
        assert!(s.starts_with("Install one of:\n"));
        assert!(s.contains("  Windows: winget install Clement.bottom"));
        // No other package-manager labels for a winget-only hint.
        assert!(!s.contains("macOS:"));
        assert!(!s.contains("Linux:"));
    }

    #[test]
    fn full_hint_lists_platforms_in_order() {
        let h = InstallHint {
            winget: Some("winget install Clement.bottom"),
            scoop: None,
            brew: Some("brew install bottom"),
            linux: Some("sudo apt install bottom"),
            cargo: Some("cargo install --locked bottom"),
            note: None,
        };
        let s = h.to_string();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "Install one of:");
        assert_eq!(lines[1], "  Windows: winget install Clement.bottom");
        assert_eq!(lines[2], "  macOS:   brew install bottom");
        assert_eq!(lines[3], "  Linux:   sudo apt install bottom");
        assert_eq!(lines[4], "  Any:     cargo install --locked bottom");
        assert_eq!(lines.len(), 5); // no trailing note
    }

    #[test]
    fn note_renders_on_trailing_line_without_final_newline() {
        let h = InstallHint {
            cargo: Some("cargo install --locked trippy"),
            note: Some("needs Npcap on Windows"),
            ..Default::default()
        };
        let s = h.to_string();
        // Trailing note is a `write!` (no newline) so palette / hint bar
        // can suffix a period without a stray blank line.
        assert!(s.ends_with("Note: needs Npcap on Windows"));
    }

    #[test]
    fn cargo_only_is_not_empty() {
        // A tool with only a cargo path (e.g. unpackaged niche crate) is
        // still a valid hint.
        let h = InstallHint {
            cargo: Some("cargo install --locked whatever"),
            ..Default::default()
        };
        assert!(!h.is_empty());
    }
}
