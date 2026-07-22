//! rimeterm binary entrypoint.
//!
//! Boot order (matches §18 first-run + §12 perf budget):
//! 1. Init tracing (stderr, `RIMETERM_LOG` env filter).
//! 2. Resolve workspace root (CWD).
//! 3. Load config: repo `<root>/.rimeterm/config.toml` → user → default.
//! 4. Hand off to [`rimeterm_tui::App::run`].

use std::path::PathBuf;

use anyhow::Result;
use rimeterm_config::Config;
use rimeterm_tui::App;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    init_tracing();

    let workspace_root = std::env::current_dir()?;
    let config = load_config(&workspace_root)?;

    // C21.5: materialize bundled configs (yazi bridge + all seeds) into
    // `~/.rimeterm/{yazi,gitui,bottom}/`. Idempotent — the fingerprint
    // check keeps it a no-op after the first launch of any given
    // version, so there's no need to gate on a "first run" flag.
    let report = rimeterm_config::assets::materialize_configs(env!("CARGO_PKG_VERSION"));
    if !report.errors.is_empty() {
        for err in &report.errors {
            tracing::warn!(error = %err, "config asset materialize hit a snag");
        }
    }
    if !report.managed_rewritten.is_empty() || !report.seeds_written.is_empty() {
        tracing::info!(
            managed = report.managed_rewritten.len(),
            seeds = report.seeds_written.len(),
            "materialized bundled configs"
        );
    }

    // C21.5: extract prebuilt essentials binaries from the release
    // archive's sibling `essentials/` folder into `~/.rimeterm/bin/`.
    // Silent-skip when the folder isn't present (dev builds via
    // `cargo run`, custom repackagings).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let src = parent.join("essentials");
            let ext = rimeterm_config::assets::extract_essentials(&src, env!("CARGO_PKG_VERSION"));
            for err in &ext.errors {
                tracing::warn!(error = %err, "essentials extract hit a snag");
            }
            if !ext.extracted.is_empty() {
                tracing::info!(count = ext.extracted.len(), "extracted essentials binaries");
            } else if ext.source_absent {
                tracing::info!(
                    "no sibling essentials/ folder next to rimeterm binary — \
                     dev build via `cargo run`? run `node bootstrap-essentials.mjs` \
                     to fetch prebuilt yazi/gitui/bottom into target/*/essentials/"
                );
            }

            // C21.5 §5: self-copy `rimectl` next to the essentials so
            // Yazi's bridge (`Command("rimectl")`) reliably finds it via
            // the augmented PATH.
            let rimectl_report = rimeterm_config::assets::copy_rimectl_alongside(parent);
            for err in &rimectl_report.errors {
                tracing::warn!(error = %err, "rimectl self-copy hit a snag");
            }
            if !rimectl_report.extracted.is_empty() {
                tracing::info!("copied rimectl into ~/.rimeterm/bin/");
            }
        }
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let app = App::new(workspace_root, config)?;
        app.run().await
    })
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("RIMETERM_LOG").unwrap_or_else(|_| {
        // `rimeterm=info` alone misses events from the `rimeterm_tui`,
        // `rimeterm_pty`, `rimeterm_config`, `rimeterm_ipc`,
        // `rimeterm_core` crates — tracing directives match target names
        // exactly, not by shared prefix. Enumerate each workspace crate
        // so the default filter actually shows the events the user needs
        // to diagnose bridge / OSC / spawn issues.
        EnvFilter::new(
            "warn,\
             rimeterm=info,\
             rimeterm_tui=info,\
             rimeterm_pty=info,\
             rimeterm_config=info,\
             rimeterm_ipc=info,\
             rimeterm_core=info,\
             rimectl=info",
        )
    });
    // Logs go to stderr so the alt-screen (stdout) is not disturbed.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}

fn load_config(workspace_root: &PathBuf) -> Result<Config> {
    // Repo scope first — falls through to default if the file is absent.
    let repo_path = rimeterm_config::paths::repo_config_file(workspace_root);
    let repo_cfg = Config::load_or_default(&repo_path)?;
    if repo_path.exists() {
        tracing::info!(path = %repo_path.display(), "loaded repo config");
        return Ok(repo_cfg);
    }
    if let Some(user_path) = rimeterm_config::paths::config_file() {
        let user_cfg = Config::load_or_default(&user_path)?;
        if user_path.exists() {
            tracing::info!(path = %user_path.display(), "loaded user config");
        } else {
            tracing::info!("no config file present, using defaults");
        }
        return Ok(user_cfg);
    }
    Ok(Config::default())
}
