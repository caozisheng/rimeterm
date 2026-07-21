//! Cross-crate test helpers. Hidden from public docs but reachable
//! from downstream test modules that also mutate process env.
//!
//! Every test that mutates process env (`RIMETERM_HOME`, `PATH`,
//! `APPDATA`, …) MUST take [`ENV_LOCK`] before touching it. Rust's
//! test harness runs tests threaded by default and `std::env::set_var`
//! is process-wide; without a shared lock, sibling modules (paths /
//! env / assets in this crate, spawn/detection tests in
//! `rimeterm-tui`) will stomp each other's env mid-flight and flake.

/// Serializes every env-touching test in this crate. Poison-safe:
/// callers unwrap-or-inner so one panicking test doesn't wedge the rest.
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
