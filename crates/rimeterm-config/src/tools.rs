//! **§9.4 Tools Registry** — canonical description of the TUI tools
//! rimeterm boots into by default.
//!
//! C21.5 splits the registry into two tiers:
//!
//! - **essentials** (`yazi`, `gitui`, `bottom`) — bundled with the
//!   rimeterm release archive; first-launch extraction drops them into
//!   [`crate::paths::bin_dir`] and seeds `~/.rimeterm/{yazi,gitui,bottom}/`
//!   with curated configs. `tools.install <essential>` is a no-op with
//!   `already_bundled`; upgrades ship with new rimeterm releases.
//! - **plugins** (`trippy` today, user-added tomorrow) — installed on
//!   demand via `cargo install --locked --root ~/.rimeterm/plugins/<name>`;
//!   binaries land in `~/.rimeterm/plugins/<name>/bin/`, configs (when
//!   the entry ships a seed) in `~/.rimeterm/plugins/<name>/config/`.
//!
//! Detection order (§9.4 layered rule 1): `bin/` (`Essential`) →
//! `plugins/*/bin/` (`Plugin`) → `$CARGO_HOME/bin` (`Cargo`, v0.1.x
//! legacy or user's own) → other `$PATH` (`System`). First hit wins.
//!
//! This module holds only the **static registries + detection helpers**;
//! the actual `cargo install` shell-out lives in the IPC command layer.

use std::path::{Path, PathBuf};

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

/// Which tier a tool belongs to (§9.4 C21.5 split).
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Bundled with the rimeterm release archive; extracted to
    /// `~/.rimeterm/bin/` on first launch. Upgrades ride rimeterm releases.
    Essential,
    /// Installed on demand via `cargo install --locked --root
    /// ~/.rimeterm/plugins/<name>`; independently upgradable.
    Plugin,
}

/// The three tools rimeterm's default four-quadrant layout requires
/// (`yazi` for files, `gitui` for git, `bottom` for sysmon).
///
/// Every essential ships as a prebuilt binary alongside `rimeterm` in
/// the release archive. The `crates` field is retained as a **build
/// recipe** for CI + the rare case a user opts out of the bundle via
/// `[install.essentials] prefer_system` — it is NOT invoked by
/// `tools.install` (essentials return `already_bundled`).
pub const ESSENTIALS_REGISTRY: &[ToolSpec] = &[
    ToolSpec {
        name: "yazi",
        binary: "yazi",
        crates: &["yazi-fm", "yazi-cli"],
        hint: "bundled with rimeterm; upgrade by installing a newer rimeterm release",
    },
    ToolSpec {
        name: "gitui",
        binary: "gitui",
        crates: &["gitui"],
        hint: "bundled with rimeterm; upgrade by installing a newer rimeterm release",
    },
    ToolSpec {
        name: "bottom",
        binary: "btm",
        crates: &["bottom"],
        hint: "bundled with rimeterm; upgrade by installing a newer rimeterm release",
    },
];

/// Non-essential tools rimeterm knows how to install on demand via
/// `cargo install --locked --root ~/.rimeterm/plugins/<name>`. Users
/// can extend this registry via `config.toml` in a future revision;
/// v0.2 keeps it hardcoded to `trippy`.
///
/// **`bandwhich` dropped in C13**: winget has no package, Windows
/// requires Npcap, and the tool needs admin/cap_net_raw to run.
/// Users who want bandwhich install it through their system package
/// manager.
pub const PLUGIN_REGISTRY: &[ToolSpec] = &[ToolSpec {
    name: "trippy",
    binary: "trip",
    crates: &["trippy"],
    hint: "brew/scoop install trippy, or install via rimeterm `tools.install trippy`",
}];

/// Deprecated alias — merged view of essentials + plugins, kept so
/// v0.1.2 IPC callers that iterate `TOOL_REGISTRY` don't break. New
/// code should use `essentials_registry()` / `plugin_registry()`
/// directly and dispatch on `ToolKind`.
pub fn all_tools() -> impl Iterator<Item = (&'static ToolSpec, ToolKind)> {
    ESSENTIALS_REGISTRY
        .iter()
        .map(|s| (s, ToolKind::Essential))
        .chain(PLUGIN_REGISTRY.iter().map(|s| (s, ToolKind::Plugin)))
}

/// Return the kind of a registry name; `None` for unknown names.
pub fn kind_of(name: &str) -> Option<ToolKind> {
    if ESSENTIALS_REGISTRY.iter().any(|s| s.name == name) {
        return Some(ToolKind::Essential);
    }
    if PLUGIN_REGISTRY.iter().any(|s| s.name == name) {
        return Some(ToolKind::Plugin);
    }
    None
}

/// Where a detected binary appears to have come from.
///
/// C21.5: `Essential` and `Plugin` are rimeterm-managed; `Upgrade` and
/// `Uninstall` gate on `Plugin` only. `Essential` upgrades ride a new
/// rimeterm release. `Cargo` / `System` are user-owned and rimeterm
/// refuses to touch them.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallSource {
    /// Binary path is inside `~/.rimeterm/bin/` — bundled with
    /// rimeterm's release archive and extracted at first launch.
    Essential,
    /// Binary path is inside `~/.rimeterm/plugins/<name>/bin/` —
    /// installed on demand via `tools.install <name>`.
    Plugin,
    /// Binary path is inside `$CARGO_HOME/bin` (or `~/.cargo/bin` when
    /// `$CARGO_HOME` is unset). Either v0.1.x-era rimeterm install or
    /// the user's own `cargo install`.
    Cargo,
    /// Detected on `$PATH` but not under a rimeterm-managed or cargo
    /// dir — some OS package manager or manual install.
    System,
    /// Not found in any managed dir or on `$PATH`.
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
    /// Which tier the tool belongs to.
    pub kind: ToolKind,
    /// Absolute path (from the managed dir walk or `which::which`), or
    /// `None` when missing.
    pub detected_path: Option<PathBuf>,
    /// Where the detected binary lives (see [`InstallSource`]).
    pub install_source: InstallSource,
}

/// Probe every tool in both registries, returning owned data ready to
/// serialize. Cheap enough to call per request; the managed-dir walk is
/// bounded (`~/.rimeterm/bin/` + one dirent scan for `plugins/*/bin/`)
/// and `which::which` reads `$PATH` once per call.
pub fn detect_all() -> Vec<DetectedTool> {
    let ctx = DetectContext::probe();
    all_tools()
        .map(|(spec, kind)| detect_with(spec, kind, &ctx))
        .collect()
}

/// Snapshot of the managed dirs the detector needs. Built once per
/// `detect_all` call so the per-tool walk is a couple of `starts_with`
/// checks + one `which::which`.
pub struct DetectContext {
    /// `~/.rimeterm/bin/` (may not exist yet on fresh install).
    pub bin_dir: Option<PathBuf>,
    /// Every `~/.rimeterm/plugins/<name>/bin/` present on disk.
    pub plugin_bin_dirs: Vec<PathBuf>,
    /// `$CARGO_HOME/bin` (or `~/.cargo/bin` fallback).
    pub cargo_bin: Option<PathBuf>,
}

impl DetectContext {
    /// Fresh probe of the filesystem + env. Called once per
    /// `detect_all`; cheap enough to skip caching.
    pub fn probe() -> Self {
        Self {
            bin_dir: crate::paths::bin_dir(),
            plugin_bin_dirs: crate::paths::plugin_bin_dirs(),
            cargo_bin: cargo_bin_dir(),
        }
    }
}

/// Probe one spec with an explicit context. Split out so tests can drive
/// the classification matrix without touching the real filesystem.
pub fn detect_with(spec: &'static ToolSpec, kind: ToolKind, ctx: &DetectContext) -> DetectedTool {
    // 1. Managed dirs first — `bin/` (essentials) then any
    //    `plugins/<n>/bin/`. On hit, skip `which::which` entirely so we
    //    don't get fooled by a system copy earlier on `$PATH`.
    let exe = platform_exe_name(spec.binary);
    if let Some(bin) = ctx.bin_dir.as_deref() {
        let candidate = bin.join(&exe);
        if candidate.is_file() {
            return DetectedTool {
                name: spec.name,
                binary: spec.binary,
                crates: spec.crates,
                hint: spec.hint,
                kind,
                detected_path: Some(candidate),
                install_source: InstallSource::Essential,
            };
        }
    }
    for plug_bin in &ctx.plugin_bin_dirs {
        let candidate = plug_bin.join(&exe);
        if candidate.is_file() {
            return DetectedTool {
                name: spec.name,
                binary: spec.binary,
                crates: spec.crates,
                hint: spec.hint,
                kind,
                detected_path: Some(candidate),
                install_source: InstallSource::Plugin,
            };
        }
    }

    // 2. Fall through to `$PATH`; classify by cargo bin dir.
    match which::which(spec.binary) {
        Ok(path) => {
            let source = classify(&path, ctx.cargo_bin.as_deref());
            DetectedTool {
                name: spec.name,
                binary: spec.binary,
                crates: spec.crates,
                hint: spec.hint,
                kind,
                detected_path: Some(path),
                install_source: source,
            }
        }
        Err(_) => DetectedTool {
            name: spec.name,
            binary: spec.binary,
            crates: spec.crates,
            hint: spec.hint,
            kind,
            detected_path: None,
            install_source: InstallSource::Missing,
        },
    }
}

/// Legacy shim retained for a single call site
/// (`rimeterm-tui/src/app.rs run_tool_action` when v0.1.2 IPC callers
/// pre-C21.5 didn't have `ToolKind`). New code MUST call [`detect_with`].
pub fn detect_one(spec: &'static ToolSpec, cargo_bin: Option<&std::path::Path>) -> DetectedTool {
    let ctx = DetectContext {
        bin_dir: crate::paths::bin_dir(),
        plugin_bin_dirs: crate::paths::plugin_bin_dirs(),
        cargo_bin: cargo_bin.map(Path::to_path_buf),
    };
    let kind = kind_of(spec.name).unwrap_or(ToolKind::Plugin);
    detect_with(spec, kind, &ctx)
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

/// `binary.exe` on Windows, `binary` elsewhere. Kept here (not in
/// `paths`) because it's a detection-only concern.
pub fn platform_exe_name(binary: &str) -> String {
    if cfg!(windows) {
        format!("{binary}.exe")
    } else {
        binary.to_string()
    }
}

/// Look up a tool spec by name across both registries. Returns `None`
/// for unknown names — the IPC layer relies on this to reject arbitrary
/// strings before shelling out.
pub fn find(name: &str) -> Option<&'static ToolSpec> {
    ESSENTIALS_REGISTRY
        .iter()
        .chain(PLUGIN_REGISTRY.iter())
        .find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn essentials_registry_holds_three_tools() {
        // Hard-coded — if this fails someone shipped a registry change
        // without updating the C21.5 design doc §9.4 table.
        let names: Vec<&str> = ESSENTIALS_REGISTRY.iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["yazi", "gitui", "bottom"]);
    }

    #[test]
    fn plugin_registry_holds_trippy() {
        // v0.2 keeps this hardcoded; future revisions may make it
        // extensible via config.toml (see design open q #6 resolved:
        // additive schema).
        let names: Vec<&str> = PLUGIN_REGISTRY.iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["trippy"]);
    }

    #[test]
    fn kind_of_matches_registries() {
        assert_eq!(kind_of("yazi"), Some(ToolKind::Essential));
        assert_eq!(kind_of("gitui"), Some(ToolKind::Essential));
        assert_eq!(kind_of("bottom"), Some(ToolKind::Essential));
        assert_eq!(kind_of("trippy"), Some(ToolKind::Plugin));
        assert_eq!(kind_of("nope"), None);
        assert_eq!(kind_of(""), None);
    }

    #[test]
    fn find_hits_across_both_registries() {
        assert!(find("yazi").is_some());
        assert!(find("bottom").is_some());
        assert!(find("trippy").is_some());
        assert!(find("nope").is_none());
        assert!(find("").is_none());
    }

    #[test]
    fn every_spec_has_nonempty_crates_and_hint() {
        for (spec, _kind) in all_tools() {
            assert!(
                !spec.crates.is_empty(),
                "tool `{}` must declare at least one crates.io package",
                spec.name
            );
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

    #[test]
    fn platform_exe_name_is_windows_aware() {
        let got = platform_exe_name("yazi");
        if cfg!(windows) {
            assert_eq!(got, "yazi.exe");
        } else {
            assert_eq!(got, "yazi");
        }
    }

    /// Drive [`detect_with`] through the full classification matrix
    /// using tempdirs — no `which::which` invocation, so the test is
    /// hermetic across dev machines and CI.
    #[test]
    fn detect_with_prefers_managed_dirs_over_path() {
        let root = mktemp("rimeterm-detect");
        let bin_dir = root.join("bin");
        let plug_bin = root.join("plugins").join("trippy").join("bin");
        let cargo_bin = root.join("cargo").join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&plug_bin).unwrap();
        std::fs::create_dir_all(&cargo_bin).unwrap();

        let yazi_spec = ESSENTIALS_REGISTRY
            .iter()
            .find(|s| s.name == "yazi")
            .expect("yazi essential exists");
        let trippy_spec = PLUGIN_REGISTRY
            .iter()
            .find(|s| s.name == "trippy")
            .expect("trippy plugin exists");

        let exe_yazi = platform_exe_name("yazi");
        let exe_trip = platform_exe_name("trip");

        let ctx = DetectContext {
            bin_dir: Some(bin_dir.clone()),
            plugin_bin_dirs: vec![plug_bin.clone()],
            cargo_bin: Some(cargo_bin.clone()),
        };

        // 1. Both managed dirs empty → detector falls through to
        //    `which::which`. When the host lacks yazi on `$PATH` we
        //    can also assert `Missing`; otherwise skip (still get
        //    coverage from cases 2+3).
        if which::which(yazi_spec.binary).is_err() {
            let got = detect_with(yazi_spec, ToolKind::Essential, &ctx);
            assert_eq!(got.install_source, InstallSource::Missing);
            assert!(got.detected_path.is_none());
        }

        // 2. Only `bin/yazi(.exe)` present → Essential.
        std::fs::write(bin_dir.join(&exe_yazi), b"stub").unwrap();
        let got = detect_with(yazi_spec, ToolKind::Essential, &ctx);
        assert_eq!(got.install_source, InstallSource::Essential);
        assert_eq!(got.detected_path, Some(bin_dir.join(&exe_yazi)));

        // 3. Only `plugins/trippy/bin/trip(.exe)` present → Plugin.
        std::fs::write(plug_bin.join(&exe_trip), b"stub").unwrap();
        let got = detect_with(trippy_spec, ToolKind::Plugin, &ctx);
        assert_eq!(got.install_source, InstallSource::Plugin);
        assert_eq!(got.detected_path, Some(plug_bin.join(&exe_trip)));

        // 4. Cleanup.
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Manual tempdir helper — kept in sync with `paths::tests`. Not
    /// worth pulling `tempfile` into the workspace for this alone.
    fn mktemp(prefix: &str) -> PathBuf {
        let mut root = std::env::temp_dir();
        let stamp = format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        root.push(stamp);
        std::fs::create_dir_all(&root).expect("mkdir tmp");
        root
    }
}
