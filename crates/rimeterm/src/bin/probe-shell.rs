//! `probe-shell` — print the shell that `rimeterm_pty::detect_default_shell`
//! would pick given the default config hints. Non-interactive; safe to run
//! from any harness. Useful for CI + smoke tests.

use rimeterm_config::Config;
use rimeterm_pty::detect_default_shell;

fn main() {
    let cfg = Config::default();
    let hints: &[String] = if cfg!(windows) {
        &cfg.core.shell_win
    } else {
        &cfg.core.shell_unix
    };
    let choice = detect_default_shell(hints);
    println!(
        "hints    = {:?}\nresolved = {} @ {}",
        hints,
        choice.short_name(),
        choice
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".into())
    );
}
