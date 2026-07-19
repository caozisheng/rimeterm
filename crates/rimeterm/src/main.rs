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

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let app = App::new(workspace_root, config)?;
        app.run().await
    })
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("RIMETERM_LOG")
        .unwrap_or_else(|_| EnvFilter::new("warn,rimeterm=info"));
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
