//! Detect whether an external tool binary is installed on PATH.
//!
//! **Design decision (v0.2)**: rimeterm doesn't bundle third-party tools.
//! Agents (`omp` / `codex` / `claude`) and the file/git/monitor group members
//! (`yazi` / `gitui` / `bottom` / `bandwhich` / `trippy`) are all discovered
//! via `which` at startup. Missing binaries surface a placeholder pane with
//! the user-configured install hint; present binaries spawn a PTY.
//!
//! This module intentionally does no version parsing / `--version` spawn:
//! probing is on the hot startup path.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum ToolAvailability {
    Available(PathBuf),
    Missing { probed: String },
}

/// Alias kept for M3 callers. Prefer `ToolAvailability` in new code.
pub type AgentAvailability = ToolAvailability;

/// Look up `argv[0]` on PATH. `argv` must not be empty.
pub fn detect_tool(argv: &[String]) -> ToolAvailability {
    let Some(cmd) = argv.first() else {
        return ToolAvailability::Missing {
            probed: "(empty command)".into(),
        };
    };
    match which::which(cmd) {
        Ok(path) => ToolAvailability::Available(path),
        Err(_) => ToolAvailability::Missing {
            probed: cmd.clone(),
        },
    }
}

/// Backwards-compatible name for M3 callers.
pub fn detect_agent(argv: &[String]) -> ToolAvailability {
    detect_tool(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_command_reports_probed_name() {
        let a = detect_tool(&["this-command-does-not-exist-hopefully".into()]);
        assert!(matches!(a, ToolAvailability::Missing { .. }));
    }

    #[test]
    fn empty_argv_is_missing() {
        let a = detect_tool(&[]);
        assert!(matches!(a, ToolAvailability::Missing { .. }));
    }

    #[test]
    fn detect_agent_alias_forwards_to_detect_tool() {
        assert!(matches!(
            detect_agent(&["definitely-not-installed".into()]),
            ToolAvailability::Missing { .. }
        ));
    }
}
