//! Bundled config assets — Yazi bridge plugin + first-launch seeds
//! (C21.5).
//!
//! Everything under `assets/` in the repo is `include_bytes!`-baked into
//! the rimeterm binary. [`materialize_configs`] writes it out to the
//! managed config dirs (`~/.rimeterm/{yazi,gitui,bottom}/`) with a strict
//! ownership split:
//!
//! - **plugin files** (`yazi/plugins/rimeterm-bridge.yazi/*`) —
//!   rimeterm-owned. On every launch the version marker
//!   `.rimeterm-version` is compared against `CARGO_PKG_VERSION`; if
//!   different, the plugin files are force-rewritten.
//! - **seed files** (`init.lua`, `yazi.toml`, `package.toml`,
//!   `key_bindings.ron`, `theme.ron`, `bottom.toml`) — user-owned.
//!   Written **only when absent**. Users may edit freely; rimeterm
//!   never touches them again unless deleted.
//!
//! Failures are logged and swallowed — a broken filesystem must not
//! prevent rimeterm from starting.

use std::io;
use std::path::Path;

/// Version marker file dropped into each rimeterm-owned dir. Bumped
/// with `CARGO_PKG_VERSION` on every rimeterm release; a mismatch
/// triggers a plugin rewrite.
pub const VERSION_MARKER: &str = ".rimeterm-version";

/// One asset entry — either a plugin file or a seed. Kept generic so
/// the materialize logic doesn't fan out per file.
struct Asset {
    /// Path relative to the tool's config dir. E.g.
    /// `"plugins/rimeterm-bridge.yazi/main.lua"` or `"init.lua"`.
    rel_path: &'static str,
    /// The file contents, baked in at compile time.
    bytes: &'static [u8],
    /// `Managed` → overwrite on version bump. `Seed` → write only if
    /// absent.
    ownership: Ownership,
}

#[derive(Copy, Clone, PartialEq)]
enum Ownership {
    Managed,
    Seed,
}

/// Every asset bundled into rimeterm. Order matters only for
/// determinism in tests; runtime doesn't care.
const ASSETS: &[(ToolBucket, Asset)] = &[
    // yazi — rimeterm-managed plugin (force-overwrite on version bump).
    (
        ToolBucket::Yazi,
        Asset {
            rel_path: "plugins/rimeterm-bridge.yazi/main.lua",
            bytes: include_bytes!("../../../assets/yazi/plugins/rimeterm-bridge.yazi/main.lua"),
            ownership: Ownership::Managed,
        },
    ),
    (
        ToolBucket::Yazi,
        Asset {
            rel_path: "plugins/rimeterm-bridge.yazi/README.md",
            bytes: include_bytes!("../../../assets/yazi/plugins/rimeterm-bridge.yazi/README.md"),
            ownership: Ownership::Managed,
        },
    ),
    (
        ToolBucket::Yazi,
        Asset {
            rel_path: "plugins/rimeterm-bridge.yazi/LICENSE",
            bytes: include_bytes!("../../../assets/yazi/plugins/rimeterm-bridge.yazi/LICENSE"),
            ownership: Ownership::Managed,
        },
    ),
    // yazi — user-owned seeds.
    (
        ToolBucket::Yazi,
        Asset {
            rel_path: "init.lua",
            bytes: include_bytes!("../../../assets/yazi/seeds/init.lua"),
            ownership: Ownership::Seed,
        },
    ),
    (
        ToolBucket::Yazi,
        Asset {
            rel_path: "yazi.toml",
            bytes: include_bytes!("../../../assets/yazi/seeds/yazi.toml"),
            ownership: Ownership::Seed,
        },
    ),
    (
        ToolBucket::Yazi,
        Asset {
            rel_path: "package.toml",
            bytes: include_bytes!("../../../assets/yazi/seeds/package.toml"),
            ownership: Ownership::Seed,
        },
    ),
    // gitui — user-owned seeds only (no bundled plugin).
    (
        ToolBucket::Gitui,
        Asset {
            rel_path: "key_bindings.ron",
            bytes: include_bytes!("../../../assets/gitui/seeds/key_bindings.ron"),
            ownership: Ownership::Seed,
        },
    ),
    (
        ToolBucket::Gitui,
        Asset {
            rel_path: "theme.ron",
            bytes: include_bytes!("../../../assets/gitui/seeds/theme.ron"),
            ownership: Ownership::Seed,
        },
    ),
    // bottom — user-owned seed only.
    (
        ToolBucket::Bottom,
        Asset {
            rel_path: "bottom.toml",
            bytes: include_bytes!("../../../assets/bottom/seeds/bottom.toml"),
            ownership: Ownership::Seed,
        },
    ),
];

/// Which tool's config dir an asset targets.
#[derive(Copy, Clone, PartialEq)]
enum ToolBucket {
    Yazi,
    Gitui,
    Bottom,
}

/// Result of a `materialize` call — what changed on disk. Kept structured
/// so callers (startup logging, tests) can render or assert on it
/// without re-scanning the filesystem.
#[derive(Debug, Default, PartialEq)]
pub struct MaterializeReport {
    pub managed_rewritten: Vec<String>,
    pub seeds_written: Vec<String>,
    pub seeds_kept: Vec<String>,
    pub errors: Vec<String>,
}

/// Materialize all bundled assets under [`crate::paths::home`]. Safe to
/// call on every startup; behavior:
///
/// - **Managed files**: rewritten whenever `.rimeterm-version` in the
///   parent plugin dir doesn't match `current_version`.
/// - **Seed files**: written only if absent — user's edits are never
///   clobbered.
///
/// Individual write failures are captured in the report but never
/// bubbled up — this must not prevent rimeterm from starting.
pub fn materialize_configs(current_version: &str) -> MaterializeReport {
    let mut report = MaterializeReport::default();
    let Some(home) = crate::paths::home() else {
        report
            .errors
            .push("$RIMETERM_HOME not resolvable; skipping config materialize".into());
        return report;
    };

    // Precompute per-bucket parent dirs.
    let dirs = [
        (ToolBucket::Yazi, home.join("yazi")),
        (ToolBucket::Gitui, home.join("gitui")),
        (ToolBucket::Bottom, home.join("bottom")),
    ];
    let dir_for = |b: ToolBucket| -> &Path {
        dirs.iter()
            .find(|(kind, _)| *kind == b)
            .map(|(_, p)| p.as_path())
            .expect("all buckets have a dir")
    };

    // Managed files live in versioned subtrees. Read each subtree's
    // `.rimeterm-version` marker once so we can decide overwrite vs
    // skip cheaply.
    for (bucket, asset) in ASSETS {
        let parent_dir = dir_for(*bucket);
        let dest = parent_dir.join(asset.rel_path);
        let outcome = match asset.ownership {
            Ownership::Seed => write_seed(&dest, asset.bytes),
            Ownership::Managed => write_managed(
                &dest,
                asset.bytes,
                current_version,
                parent_dir,
                asset.rel_path,
            ),
        };
        match outcome {
            Ok(Written::Wrote) => match asset.ownership {
                Ownership::Managed => report.managed_rewritten.push(dest.display().to_string()),
                Ownership::Seed => report.seeds_written.push(dest.display().to_string()),
            },
            Ok(Written::Kept) => match asset.ownership {
                // Managed 'kept' just means "same version, no work" —
                // silent success, no report entry.
                Ownership::Managed => {}
                Ownership::Seed => report.seeds_kept.push(dest.display().to_string()),
            },
            Err(e) => report.errors.push(format!("{}: {e}", dest.display(),)),
        }
    }

    // Drop `.rimeterm-version` markers into every rimeterm-owned
    // subdir we touched (currently just `yazi/plugins/rimeterm-bridge.yazi/`).
    // Doing this once at the end avoids duplicate writes when multiple
    // managed assets share a parent.
    let mut version_dirs: Vec<std::path::PathBuf> = Vec::new();
    for (bucket, asset) in ASSETS {
        if asset.ownership != Ownership::Managed {
            continue;
        }
        let dest = dir_for(*bucket).join(asset.rel_path);
        if let Some(parent) = dest.parent() {
            if !version_dirs.iter().any(|p| p == parent) {
                version_dirs.push(parent.to_path_buf());
            }
        }
    }
    for dir in &version_dirs {
        let marker = dir.join(VERSION_MARKER);
        if let Err(e) = std::fs::write(&marker, current_version.as_bytes()) {
            report.errors.push(format!("{}: {e}", marker.display()));
        }
    }

    report
}

/// Outcome of a single asset write.
enum Written {
    Wrote,
    Kept,
}

fn write_seed(dest: &Path, bytes: &[u8]) -> io::Result<Written> {
    if dest.is_file() {
        return Ok(Written::Kept);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, bytes)?;
    Ok(Written::Wrote)
}

fn write_managed(
    dest: &Path,
    bytes: &[u8],
    current_version: &str,
    version_dir: &Path,
    rel_path: &str,
) -> io::Result<Written> {
    // Version marker lives next to the managed asset — for the yazi
    // bridge that's `plugins/rimeterm-bridge.yazi/.rimeterm-version`.
    // We derive the marker dir from `rel_path`'s parent so multiple
    // managed subtrees per bucket stay independent.
    let marker_dir = match Path::new(rel_path).parent() {
        Some(p) => version_dir.join(p),
        None => version_dir.to_path_buf(),
    };
    let marker = marker_dir.join(VERSION_MARKER);
    let existing_ok = std::fs::read_to_string(&marker)
        .map(|s| s.trim() == current_version)
        .unwrap_or(false);
    if existing_ok && dest.is_file() {
        return Ok(Written::Kept);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, bytes)?;
    Ok(Written::Wrote)
}

/// Report from an [`extract_essentials`] run.
#[derive(Debug, Default, PartialEq)]
pub struct EssentialsExtractReport {
    pub extracted: Vec<String>,
    pub skipped_up_to_date: Vec<String>,
    pub missing_source: Vec<String>,
    pub errors: Vec<String>,
    /// `true` when the sibling `essentials/` folder doesn't exist at
    /// all — this is the "dev build via `cargo run`" case. Not an
    /// error; caller may log-and-continue.
    pub source_absent: bool,
}

/// Copy prebuilt essentials binaries from `<current_exe_dir>/essentials/`
/// into `~/.rimeterm/bin/`. Idempotent — a per-bin
/// `.rimeterm-essentials-version` fingerprint keeps repeat launches
/// O(1).
///
/// Arguments:
/// - `source_dir`: usually `env::current_exe()?.parent()?.join("essentials")`.
/// - `current_version`: normally `CARGO_PKG_VERSION`; bump implies
///   re-copy.
///
/// Contract:
/// - When `source_dir` is absent → returns `source_absent = true` and
///   an otherwise-empty report. Caller MUST NOT treat this as fatal;
///   dev builds and custom repackagings legitimately lack the sibling
///   folder.
/// - When present → every entry ending in `.exe` (Windows) or with no
///   extension (Unix) is copied to `bin_dir`; a fingerprint marker
///   `bin/.rimeterm-essentials-version` records the version so a
///   subsequent same-version launch is a no-op.
/// - Individual copy failures land in `errors` but never abort the
///   whole extraction.
pub fn extract_essentials(source_dir: &Path, current_version: &str) -> EssentialsExtractReport {
    let mut report = EssentialsExtractReport::default();
    if !source_dir.is_dir() {
        report.source_absent = true;
        return report;
    }

    let Some(bin_dir) = crate::paths::bin_dir() else {
        report
            .errors
            .push("$RIMETERM_HOME not resolvable; skipping essentials extract".into());
        return report;
    };

    // Fingerprint short-circuit — if the marker matches, we're done.
    let marker = bin_dir.join(".rimeterm-essentials-version");
    let up_to_date = std::fs::read_to_string(&marker)
        .map(|s| s.trim() == current_version)
        .unwrap_or(false);

    let entries = match std::fs::read_dir(source_dir) {
        Ok(it) => it,
        Err(e) => {
            report.errors.push(format!("{}: {e}", source_dir.display()));
            return report;
        }
    };

    if let Err(e) = std::fs::create_dir_all(&bin_dir) {
        report.errors.push(format!("{}: {e}", bin_dir.display()));
        return report;
    }

    for entry in entries.flatten() {
        let src = entry.path();
        // Skip nested dirs and the VERSIONS.toml manifest — only
        // top-level binaries get copied.
        if !src.is_file() {
            continue;
        }
        let Some(name) = src.file_name() else {
            continue;
        };
        if name == "VERSIONS.toml" {
            continue;
        }
        let dest = bin_dir.join(name);
        if up_to_date && dest.is_file() {
            report.skipped_up_to_date.push(dest.display().to_string());
            continue;
        }
        match std::fs::copy(&src, &dest) {
            Ok(_) => {
                report.extracted.push(dest.display().to_string());
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    // Make sure the copy is executable — `std::fs::copy`
                    // preserves the source's mode on Unix, but a paranoid
                    // chmod +x costs nothing and avoids stray 0644 from
                    // some CI archivers.
                    if let Ok(meta) = std::fs::metadata(&dest) {
                        let mut perms = meta.permissions();
                        perms.set_mode(perms.mode() | 0o111);
                        let _ = std::fs::set_permissions(&dest, perms);
                    }
                }
            }
            Err(e) => {
                report
                    .errors
                    .push(format!("copy {} → {}: {e}", src.display(), dest.display()))
            }
        }
    }

    // Only rewrite the marker if we actually did work — otherwise
    // the file mtime churns needlessly.
    if !report.extracted.is_empty() {
        if let Err(e) = std::fs::write(&marker, current_version.as_bytes()) {
            report.errors.push(format!("{}: {e}", marker.display()));
        }
    }

    report
}

/// Copy the `rimectl` binary sitting alongside `rimeterm` in the
/// release archive into `~/.rimeterm/bin/` so Yazi's bridge plugin
/// (and any other child process using PATH lookup) can reliably find
/// it — see design doc §5.
///
/// Idempotent: skips when the destination already exists with the
/// same size + mtime as the source (cheap proxy for "unchanged").
/// Errors are collected in the returned report; nothing is fatal.
pub fn copy_rimectl_alongside(source_dir: &Path) -> EssentialsExtractReport {
    let mut report = EssentialsExtractReport::default();
    let exe_name = if cfg!(windows) {
        "rimectl.exe"
    } else {
        "rimectl"
    };
    let src = source_dir.join(exe_name);
    if !src.is_file() {
        report.source_absent = true;
        return report;
    }
    let Some(bin_dir) = crate::paths::bin_dir() else {
        report
            .errors
            .push("$RIMETERM_HOME not resolvable; skipping rimectl copy".into());
        return report;
    };
    if let Err(e) = std::fs::create_dir_all(&bin_dir) {
        report.errors.push(format!("{}: {e}", bin_dir.display()));
        return report;
    }
    let dest = bin_dir.join(exe_name);

    // Skip when the two files look identical. Byte-level compare is
    // overkill for a bootstrap copy; size + mtime is enough.
    if let (Ok(sm), Ok(dm)) = (std::fs::metadata(&src), std::fs::metadata(&dest)) {
        if sm.len() == dm.len() && sm.modified().ok() == dm.modified().ok() {
            report.skipped_up_to_date.push(dest.display().to_string());
            return report;
        }
    }

    match std::fs::copy(&src, &dest) {
        Ok(_) => {
            report.extracted.push(dest.display().to_string());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&dest) {
                    let mut perms = meta.permissions();
                    perms.set_mode(perms.mode() | 0o111);
                    let _ = std::fs::set_permissions(&dest, perms);
                }
            }
        }
        Err(e) => report
            .errors
            .push(format!("copy {} → {}: {e}", src.display(), dest.display())),
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_util::ENV_LOCK;

    fn with_home<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("RIMETERM_HOME").ok();
        let mut root = std::env::temp_dir();
        let stamp = format!(
            "rimeterm-assets-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        root.push(stamp);
        std::fs::create_dir_all(&root).expect("mkdir test home");
        unsafe { std::env::set_var("RIMETERM_HOME", &root) };
        f(&root);
        let _ = std::fs::remove_dir_all(&root);
        match prev {
            Some(v) => unsafe { std::env::set_var("RIMETERM_HOME", v) },
            None => unsafe { std::env::remove_var("RIMETERM_HOME") },
        }
    }

    #[test]
    fn first_launch_writes_everything() {
        with_home(|root| {
            let report = materialize_configs("1.0.0");
            assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
            assert!(report.seeds_kept.is_empty());
            // 3 managed (main.lua/README.md/LICENSE) + 6 seeds.
            assert_eq!(report.managed_rewritten.len(), 3);
            assert_eq!(report.seeds_written.len(), 6);

            // Every file must exist on disk.
            assert!(
                root.join("yazi/plugins/rimeterm-bridge.yazi/main.lua")
                    .is_file()
            );
            assert!(root.join("yazi/init.lua").is_file());
            assert!(root.join("gitui/key_bindings.ron").is_file());
            assert!(root.join("bottom/bottom.toml").is_file());

            // Version marker was written.
            let marker = root.join("yazi/plugins/rimeterm-bridge.yazi/.rimeterm-version");
            assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "1.0.0");
        });
    }

    #[test]
    fn second_call_same_version_is_idempotent() {
        with_home(|root| {
            let _ = materialize_configs("1.0.0");
            // User edits a seed.
            let seed = root.join("yazi/init.lua");
            std::fs::write(&seed, b"-- user edit\n").unwrap();

            let report = materialize_configs("1.0.0");
            assert!(report.errors.is_empty());
            assert!(report.managed_rewritten.is_empty(), "managed must stick");
            assert!(report.seeds_written.is_empty(), "seeds must stick");
            assert_eq!(report.seeds_kept.len(), 6);

            // User's edit is preserved.
            assert_eq!(std::fs::read_to_string(&seed).unwrap(), "-- user edit\n");
        });
    }

    #[test]
    fn version_bump_rewrites_managed_but_keeps_seeds() {
        with_home(|root| {
            let _ = materialize_configs("1.0.0");
            let seed = root.join("yazi/init.lua");
            std::fs::write(&seed, b"-- user edit\n").unwrap();
            // User tampers with the managed plugin main.lua — this
            // simulates a broken hand-edit. Version bump must clobber
            // it, not preserve it.
            let plugin = root.join("yazi/plugins/rimeterm-bridge.yazi/main.lua");
            std::fs::write(&plugin, b"-- tampered\n").unwrap();

            let report = materialize_configs("1.0.1");
            assert!(report.errors.is_empty());
            assert_eq!(report.managed_rewritten.len(), 3, "plugin dir rewritten");
            assert_eq!(report.seeds_kept.len(), 6, "all seeds kept");

            // Plugin was restored to bundled bytes.
            let restored = std::fs::read(&plugin).unwrap();
            assert!(
                !restored.starts_with(b"-- tampered"),
                "managed asset must be overwritten, not preserved"
            );
            // User edit still there.
            assert_eq!(std::fs::read_to_string(&seed).unwrap(), "-- user edit\n");

            // Marker updated.
            let marker = root.join("yazi/plugins/rimeterm-bridge.yazi/.rimeterm-version");
            assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "1.0.1");
        });
    }

    #[test]
    fn seed_deletion_re_creates_on_next_launch() {
        with_home(|root| {
            let _ = materialize_configs("1.0.0");
            let seed = root.join("yazi/init.lua");
            std::fs::remove_file(&seed).unwrap();

            let report = materialize_configs("1.0.0");
            assert!(report.errors.is_empty());
            assert!(
                report.seeds_written.iter().any(|p| p.contains("init.lua")),
                "deleted seed must be re-seeded"
            );
            assert!(seed.is_file());
        });
    }

    /// Fake `essentials/` folder for extractor tests. Creates the
    /// three canonical binary names plus a `VERSIONS.toml` sibling that
    /// the extractor must skip.
    fn seed_essentials_source(dir: &std::path::Path) -> Vec<String> {
        std::fs::create_dir_all(dir).unwrap();
        let names = if cfg!(windows) {
            vec!["yazi.exe", "ya.exe", "gitui.exe", "btm.exe"]
        } else {
            vec!["yazi", "ya", "gitui", "btm"]
        };
        for n in &names {
            std::fs::write(dir.join(n), format!("#!fake {n}").as_bytes()).unwrap();
        }
        std::fs::write(dir.join("VERSIONS.toml"), b"# pins\n").unwrap();
        names.into_iter().map(String::from).collect()
    }

    #[test]
    fn extract_absent_source_reports_source_absent() {
        with_home(|root| {
            let src = root.join("absent-essentials");
            let report = extract_essentials(&src, "1.0.0");
            assert!(report.source_absent);
            assert!(report.extracted.is_empty());
            assert!(report.errors.is_empty());
        });
    }

    #[test]
    fn extract_first_launch_copies_all_binaries() {
        with_home(|root| {
            let src = root.join("release-essentials");
            let names = seed_essentials_source(&src);

            let report = extract_essentials(&src, "1.0.0");
            assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
            assert!(!report.source_absent);
            assert_eq!(report.extracted.len(), names.len());
            assert!(report.skipped_up_to_date.is_empty());

            let bin = root.join("bin");
            for n in &names {
                assert!(bin.join(n).is_file(), "missing extracted: {n}");
            }
            // Marker written.
            assert_eq!(
                std::fs::read_to_string(bin.join(".rimeterm-essentials-version"))
                    .unwrap()
                    .trim(),
                "1.0.0"
            );
            // VERSIONS.toml must NOT be copied into bin/.
            assert!(!bin.join("VERSIONS.toml").exists());
        });
    }

    #[test]
    fn extract_second_call_same_version_is_noop() {
        with_home(|root| {
            let src = root.join("release-essentials");
            let names = seed_essentials_source(&src);
            let _ = extract_essentials(&src, "1.0.0");

            let report = extract_essentials(&src, "1.0.0");
            assert!(report.errors.is_empty());
            assert!(report.extracted.is_empty(), "no re-copy on same version");
            assert_eq!(report.skipped_up_to_date.len(), names.len());
        });
    }

    #[test]
    fn extract_version_bump_re_copies() {
        with_home(|root| {
            let src = root.join("release-essentials");
            let names = seed_essentials_source(&src);
            let _ = extract_essentials(&src, "1.0.0");

            // Simulate a rimeterm release with a newer bundled yazi:
            // rewrite the source and bump the version.
            let bin_name = if cfg!(windows) { "yazi.exe" } else { "yazi" };
            std::fs::write(src.join(bin_name), b"#!new bundled yazi").unwrap();

            let report = extract_essentials(&src, "1.0.1");
            assert!(report.errors.is_empty());
            assert_eq!(report.extracted.len(), names.len(), "all re-copied");

            let dest = root.join("bin").join(bin_name);
            assert_eq!(
                std::fs::read(&dest).unwrap(),
                b"#!new bundled yazi",
                "essentials binary must be overwritten"
            );
        });
    }
}
