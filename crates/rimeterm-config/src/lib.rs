//! Configuration for rimeterm.
//!
//! Loading order (§9 of the design doc):
//! 1. Repo-scoped  `<repo>/.rimeterm/config.toml`
//! 2. User-scoped  platform config dir + `rimeterm/config.toml`
//! 3. Built-in defaults ([`Config::default`]).
//!
//! v0.1 only surfaces the fields the M0 skeleton needs (shell hint, tick rate,
//! ui theme name). Rest of the schema in the design doc lands as later crates
//! come online.

pub mod agents_state;
pub mod layout_state;
pub mod paths;
pub mod tools;

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Root config type. Everything is optional / defaulted so partial configs load.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub core: CoreConfig,
    pub ui: UiConfig,
    pub agents: AgentsConfig,
    pub files: FilesConfig,
    pub sysmon: SysmonConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct CoreConfig {
    /// Explicit shell command hint per-OS. Order = probe order.
    ///
    /// Kept as `Vec<String>` (not per-OS map) — the binary chooses `win` vs
    /// `unix` at startup and passes only the relevant slice to the PTY host.
    pub shell_win: Vec<String>,
    pub shell_unix: Vec<String>,

    /// Main-loop tick ceiling (Hz). Event-driven redraw, this is only a bound.
    pub tick_hz: u16,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            // pwsh 7 (recommended) → 5.1 → cmd. See §6.2 of the design doc.
            shell_win: vec!["pwsh".into(), "powershell".into(), "cmd".into()],
            shell_unix: vec!["fish".into(), "bash".into(), "sh".into()],
            tick_hz: 60,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct UiConfig {
    pub theme: String,
    pub follow_system_theme: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "rime-cold".into(),
            follow_system_theme: true,
        }
    }
}

/// Configuration of the four default `agents` tab-group members.
///
/// **Design decision (v0.2)**: agent binaries (`omp`, `pi`, `codex`, `claude`)
/// are **not bundled** with rimeterm. Each tab points at an external command
/// resolved via PATH at startup. When the binary is missing the tab shows a
/// placeholder pane with install hints instead of crashing.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentsConfig {
    /// Ordered list of agent tabs to preinstall. First entry = default focus.
    pub tabs: Vec<AgentSpec>,
}

/// **Default (v0.2 + C14):** empty. The `agents` quadrant starts with no
/// tabs; users press `Ctrl+T` inside it to open the picker (see
/// [`rimeterm_pty::agent_registry`]). Anyone who wants pre-spawned agent
/// tabs on every launch can still populate `config.toml`:
///
/// ```toml
/// [[agents.tabs]]
/// id = "codex"
/// label = "Codex CLI"
/// command = ["codex"]
/// install_hint = "npm i -g @openai/codex-cli"
/// ```
impl Default for AgentsConfig {
    fn default() -> Self {
        Self { tabs: Vec::new() }
    }
}

/// One external-tool tab spec — user-editable in `config.toml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalToolSpec {
    /// Tab id. Must be unique within the group.
    pub id: String,
    /// Display label shown in the tab strip.
    pub label: String,
    /// Command to spawn (`argv` — first element is the binary). Resolved via
    /// `which` at startup; on failure the tab shows `install_hint`.
    pub command: Vec<String>,
    /// Optional install pointer displayed when `command[0]` is not on PATH.
    pub install_hint: Option<String>,
}

/// Alias kept for M3 callers. Prefer `ExternalToolSpec`.
pub type AgentSpec = ExternalToolSpec;

/// Files quadrant (`yazi`, `gitui`, …). Fixed tab-group; user can reorder or
/// swap in alternatives via config but rimeterm hardcodes the *group* itself.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct FilesConfig {
    pub tabs: Vec<ExternalToolSpec>,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            tabs: vec![
                ExternalToolSpec {
                    id: "yazi".into(),
                    label: "yazi".into(),
                    command: vec!["yazi".into()],
                    install_hint: Some(
                        "Install yazi: https://yazi-rs.github.io/docs/installation".into(),
                    ),
                },
                ExternalToolSpec {
                    id: "gitui".into(),
                    label: "gitui".into(),
                    command: vec!["gitui".into()],
                    install_hint: Some(
                        "Install gitui: https://github.com/gitui-org/gitui#installation".into(),
                    ),
                },
            ],
        }
    }
}

/// Sysmon quadrant (`bottom`, `bandwhich`, `trippy`, …). Fixed tab-group.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct SysmonConfig {
    pub tabs: Vec<ExternalToolSpec>,
}

impl Default for SysmonConfig {
    fn default() -> Self {
        Self {
            tabs: vec![
                ExternalToolSpec {
                    id: "bottom".into(),
                    label: "bottom".into(),
                    // `bottom` ships as `btm` on all platforms.
                    command: vec!["btm".into()],
                    install_hint: Some(
                        "Install bottom: `cargo install bottom` (or brew / winget)".into(),
                    ),
                },
                ExternalToolSpec {
                    id: "bandwhich".into(),
                    label: "bandwhich".into(),
                    command: vec!["bandwhich".into()],
                    install_hint: Some(
                        "Install bandwhich: `cargo install bandwhich` (needs Npcap on Windows)"
                            .into(),
                    ),
                },
                ExternalToolSpec {
                    id: "trippy".into(),
                    label: "trippy".into(),
                    // `trippy` ships as `trip` on all platforms.
                    command: vec!["trip".into()],
                    install_hint: Some(
                        "Install trippy: `cargo install trippy` (needs Npcap on Windows)".into(),
                    ),
                },
            ],
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("I/O error reading `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML parse error in `{path}`: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
}

impl Config {
    /// Load from an explicit path. Missing file → returns default (not an error).
    pub fn load_or_default(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|source| ConfigError::Parse {
                path: path.display().to_string(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_include_pwsh_first_on_windows() {
        let c = Config::default();
        assert_eq!(c.core.shell_win.first().map(String::as_str), Some("pwsh"));
    }

    #[test]
    fn empty_toml_yields_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.core.tick_hz, 60);
    }

    #[test]
    fn partial_toml_only_overrides_named_fields() {
        let cfg: Config = toml::from_str("[core]\ntick_hz = 30\n").unwrap();
        assert_eq!(cfg.core.tick_hz, 30);
        assert!(!cfg.core.shell_win.is_empty()); // shell defaults preserved
    }

    #[test]
    fn default_files_config_has_yazi_gitui() {
        let ids: Vec<_> = FilesConfig::default()
            .tabs
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert_eq!(ids, vec!["yazi".to_string(), "gitui".to_string()]);
    }

    #[test]
    fn default_sysmon_config_has_five_column_tools() {
        let cmds: Vec<_> = SysmonConfig::default()
            .tabs
            .iter()
            .map(|s| s.command[0].clone())
            .collect();
        assert_eq!(
            cmds,
            vec![
                "btm".to_string(),
                "bandwhich".to_string(),
                "trip".to_string()
            ]
        );
    }

    #[test]
    fn external_tool_spec_round_trips_toml() {
        let toml_str = r#"
id = "yazi"
label = "yazi"
command = ["yazi"]
install_hint = "brew install yazi"
"#;
        let spec: ExternalToolSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.id, "yazi");
        assert_eq!(spec.command, vec!["yazi".to_string()]);
        assert_eq!(spec.install_hint.as_deref(), Some("brew install yazi"));
    }
}
