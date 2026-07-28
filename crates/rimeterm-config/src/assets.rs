//! Bundled config assets — first-launch seeds (C21.5).
//!
//! Historically this module `include_bytes!`-baked the Yazi bridge
//! plugin plus curated Yazi/Gitui/Bottom seed configs into the
//! rimeterm binary and dropped them into `~/.rimeterm/{yazi,gitui,bottom}/`
//! on first launch. The native-file-git refactor retires the yazi and
//! gitui external runtime paths — their bundled files are removed and
//! [`materialize_configs`] shrinks to a no-op stub kept only so
//! `rimeterm-tui` callers (still on the pre-refactor signature) keep
//! compiling. Essentials extraction ([`extract_essentials`],
//! [`copy_rimectl_alongside`]) is unaffected.
//!
//! Failures anywhere in this module are logged and swallowed by
//! callers — a broken filesystem must not prevent rimeterm from
//! starting.

use std::path::Path;

/// Version marker file dropped into each rimeterm-owned dir. Retained
/// as a public constant because downstream doctor / cleanup code
/// still keys off it to detect orphaned pre-refactor sandboxes.
pub const VERSION_MARKER: &str = ".rimeterm-version";

/// Result of a `materialize` call. Kept structured (rather than a bare
/// `()`) so callers keep the "log this on startup" pattern intact even
/// though the report is now always empty.
#[derive(Debug, Default, PartialEq)]
pub struct MaterializeReport {
    pub managed_rewritten: Vec<String>,
    pub seeds_written: Vec<String>,
    pub seeds_kept: Vec<String>,
    pub errors: Vec<String>,
}

/// **No-op stub.** The native-file-git refactor removed every
/// bundled asset (the yazi bridge plugin, chafa/glow previewers, and
/// the yazi/gitui/bottom seed configs). Callers still invoke this on
/// startup for parity with older builds; it returns an empty report
/// so the "log the outcome" call sites stay untouched.
///
/// The `_current_version` argument is kept so re-adding a version-gated
/// asset later doesn't require a signature change across the tree.
pub fn materialize_configs(_current_version: &str) -> MaterializeReport {
    MaterializeReport::default()
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
/// release archive into `~/.rimeterm/bin/` so PATH-based lookups from
/// child processes (agents, plugins) find it — see design doc §5.
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
    fn materialize_configs_is_a_noop_stub() {
        with_home(|root| {
            let report = materialize_configs("1.0.0");
            assert_eq!(report, MaterializeReport::default());
            // No yazi/gitui/bottom sandbox files should be created —
            // materialize is fully retired.
            assert!(!root.join("yazi").exists());
            assert!(!root.join("gitui").exists());
            assert!(!root.join("bottom").exists());
        });
    }

    /// Fake `essentials/` folder for extractor tests. Creates the
    /// canonical binary names plus a `VERSIONS.toml` sibling that
    /// the extractor must skip.
    fn seed_essentials_source(dir: &std::path::Path) -> Vec<String> {
        std::fs::create_dir_all(dir).unwrap();
        let names = if cfg!(windows) {
            vec!["btm.exe"]
        } else {
            vec!["btm"]
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

            // Simulate a rimeterm release with a newer bundled btm:
            // rewrite the source and bump the version.
            let bin_name = if cfg!(windows) { "btm.exe" } else { "btm" };
            std::fs::write(src.join(bin_name), b"#!new bundled btm").unwrap();

            let report = extract_essentials(&src, "1.0.1");
            assert!(report.errors.is_empty());
            assert_eq!(report.extracted.len(), names.len(), "all re-copied");

            let dest = root.join("bin").join(bin_name);
            assert_eq!(
                std::fs::read(&dest).unwrap(),
                b"#!new bundled btm",
                "essentials binary must be overwritten"
            );
        });
    }
}
