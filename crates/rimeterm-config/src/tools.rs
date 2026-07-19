//! **§9.4 Tools Registry** — canonical description of the TUI five-piece
//! set (`yazi`, `gitui`, `bottom`, `bandwhich`, `trippy`) as
//! **crates.io-installable** external tools.
//!
//! Design principle 7 (see `docs/rimeterm-overall-design.md`):
//! - **detection is layered**: user's `$PATH` beats anything rimeterm knows;
//!   we probe with `which::which` and take whatever we find.
//! - **install is opt-in and non-invasive**: `cargo install --locked <crate>`
//!   drops the binary into `$CARGO_HOME/bin`; we NEVER write into system
//!   directories, NEVER touch sudo, NEVER fork upstream.
//! - **not detected** panes get an install hint, not a stub.
//!
//! This module holds only the **static registry + detection helpers**; the
//! actual `cargo install` shell-out lives in the IPC command layer.

use std::path::PathBuf;

use serde::Serialize;

/// One row in the registry — how to probe, which crate to install, and a
/// human-readable install hint (surfaced by the placeholder pane).
#[derive(Clone, Debug, Serialize)]
pub struct ToolSpec {
    /// Stable public name used by `rimectl tools.*` commands.
    pub name: &'static str,
    /// Binary the tool ships as (what `which::which` looks up).
    pub binary: &'static str,
    /// crates.io package name(s) passed to `cargo install --locked`.
    /// Some tools ship several binaries from the same crate (yazi = fm+cli).
    pub crates: &'static [&'static str],
    /// One-line human hint shown in the placeholder pane when the binary is
    /// missing. Includes both system-package-manager and cargo suggestions.
    pub hint: &'static str,
}

/// The five canonical PTY-plugin tools rimeterm boots into by default.
///
/// Order matters — `tools.list` returns them in this order so scripts get a
/// stable index.
pub const TOOL_REGISTRY: &[ToolSpec] = &[
    ToolSpec {
        name: "yazi",
        binary: "yazi",
        crates: &["yazi-fm", "yazi-cli"],
        hint: "brew/scoop/apt install yazi, or `cargo install --locked yazi-fm yazi-cli`",
    },
    ToolSpec {
        name: "gitui",
        binary: "gitui",
        crates: &["gitui"],
        hint: "brew/scoop install gitui, or `cargo install --locked gitui`",
    },
    ToolSpec {
        name: "bottom",
        binary: "btm",
        crates: &["bottom"],
        hint: "brew/scoop install bottom, or `cargo install --locked bottom`",
    },
    ToolSpec {
        name: "bandwhich",
        binary: "bandwhich",
        crates: &["bandwhich"],
        hint: "brew/scoop install bandwhich (needs admin/cap_net_raw), or `cargo install --locked bandwhich`",
    },
    ToolSpec {
        name: "trippy",
        binary: "trip",
        crates: &["trippy"],
        hint: "brew/scoop install trippy, or `cargo install --locked trippy`",
    },
];

/// Where a detected binary appears to have come from.
///
/// Rimeterm only tries to `upgrade` / `uninstall` tools it can prove were
/// installed via `cargo install` (path lands under `$CARGO_HOME/bin`);
/// system-installed binaries defer to the OS package manager.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallSource {
    /// Binary path is inside `$CARGO_HOME/bin` (or `~/.cargo/bin` when
    /// `$CARGO_HOME` is unset).
    Cargo,
    /// Detected on `$PATH` but not under a cargo dir — some OS package
    /// manager or manual install. Not upgradable through rimeterm.
    System,
    /// `which::which` returned nothing.
    Missing,
}

/// Result of probing one tool. Serializes straight into the `tools.list`
/// response body.
#[derive(Clone, Debug, Serialize)]
pub struct DetectedTool {
    pub name: &'static str,
    pub binary: &'static str,
    pub crates: &'static [&'static str],
    pub hint: &'static str,
    /// Absolute path from `which::which`, or `None` when missing.
    pub detected_path: Option<PathBuf>,
    /// Where the detected binary lives (see [`InstallSource`]).
    pub install_source: InstallSource,
}

/// Probe every spec in [`TOOL_REGISTRY`], returning owned data ready to
/// serialize. Cheap enough to call per request; `which::which` reads
/// `$PATH` once per call.
pub fn detect_all() -> Vec<DetectedTool> {
    let cargo_bin = cargo_bin_dir();
    TOOL_REGISTRY
        .iter()
        .map(|spec| detect_one(spec, cargo_bin.as_deref()))
        .collect()
}

/// Probe one spec. Split out so tests can drive individual entries.
pub fn detect_one(spec: &'static ToolSpec, cargo_bin: Option<&std::path::Path>) -> DetectedTool {
    match which::which(spec.binary) {
        Ok(path) => {
            let source = classify(&path, cargo_bin);
            DetectedTool {
                name: spec.name,
                binary: spec.binary,
                crates: spec.crates,
                hint: spec.hint,
                detected_path: Some(path),
                install_source: source,
            }
        }
        Err(_) => DetectedTool {
            name: spec.name,
            binary: spec.binary,
            crates: spec.crates,
            hint: spec.hint,
            detected_path: None,
            install_source: InstallSource::Missing,
        },
    }
}

/// Resolve `$CARGO_HOME/bin` or fall back to `<HOME>/.cargo/bin`. Returns
/// `None` when neither can be determined (rare — headless CI without HOME).
pub fn cargo_bin_dir() -> Option<PathBuf> {
    if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        return Some(PathBuf::from(cargo_home).join("bin"));
    }
    directories::UserDirs::new().map(|u| u.home_dir().join(".cargo").join("bin"))
}

fn classify(binary_path: &std::path::Path, cargo_bin: Option<&std::path::Path>) -> InstallSource {
    match cargo_bin {
        Some(bin) if binary_path.starts_with(bin) => InstallSource::Cargo,
        _ => InstallSource::System,
    }
}

/// Look up a tool spec by name. Returns `None` for unknown names — the IPC
/// layer relies on this to reject arbitrary strings before shelling out.
pub fn find(name: &str) -> Option<&'static ToolSpec> {
    TOOL_REGISTRY.iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_all_five_tools() {
        // Deliberately hard-coded — if this fails, someone shipped a change
        // that either dropped a tool or added a new one without updating
        // this assertion (and, presumably, the design doc §9.4 table).
        let names: Vec<&str> = TOOL_REGISTRY.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            vec!["yazi", "gitui", "bottom", "bandwhich", "trippy"]
        );
    }

    #[test]
    fn find_hits_and_misses() {
        assert!(find("yazi").is_some());
        assert!(find("bottom").is_some());
        assert!(find("nope").is_none());
        assert!(find("").is_none());
    }

    #[test]
    fn each_spec_has_at_least_one_crate() {
        for spec in TOOL_REGISTRY {
            assert!(
                !spec.crates.is_empty(),
                "tool `{}` must declare at least one crates.io package",
                spec.name
            );
        }
    }

    #[test]
    fn each_spec_has_nonempty_hint() {
        for spec in TOOL_REGISTRY {
            assert!(
                !spec.hint.trim().is_empty(),
                "tool `{}` must have a non-empty install hint",
                spec.name
            );
        }
    }

    #[test]
    fn classify_recognizes_cargo_bin() {
        let cargo = std::path::Path::new("/home/u/.cargo/bin");
        let inside = std::path::Path::new("/home/u/.cargo/bin/gitui");
        let outside = std::path::Path::new("/usr/local/bin/gitui");
        assert_eq!(classify(inside, Some(cargo)), InstallSource::Cargo);
        assert_eq!(classify(outside, Some(cargo)), InstallSource::System);
        assert_eq!(classify(outside, None), InstallSource::System);
    }
}
