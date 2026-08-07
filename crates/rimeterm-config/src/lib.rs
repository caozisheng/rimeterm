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
pub mod assets;
pub mod env;
pub mod files_state;
pub mod glab_config;
pub mod install_hint;
pub mod layout_state;
pub mod left_tabs_state;
pub mod memory_state;
pub mod migrate;
pub mod paths;
pub mod session_state;

#[doc(hidden)]
pub mod test_util;
pub mod tools;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Root config type. Everything is optional / defaulted so partial configs load.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub core: CoreConfig,
    pub ui: UiConfig,
    pub agents: AgentsConfig,
    pub files: FilesConfig,
    pub git: GitConfig,
    pub sysmon: SysmonConfig,
    pub mouse: MouseConfig,
    pub viewer: ViewerConfig,
    pub stock: StockConfig,
    pub zones: ZonesConfig,
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

/// Alt+V modal viewer settings (§C22 / C22.6). Currently only the
/// markdown sub-viewer has knobs; code and image viewers get their
/// values from other places (Palette::Default, ratatui-image protocol).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct ViewerConfig {
    pub markdown: ViewerMarkdownConfig,
}

/// Markdown viewer knobs. `theme` is stored as a string here rather
/// than as `rimeterm_markdown::Theme` because doing so would drag
/// `ratatui + syntect + pulldown-cmark` into the config crate's build
/// graph — the App layer parses this string via
/// `crate::viewer_theme::parse_theme` (defined in rimeterm-tui) at
/// startup, falling back to `"default"` on unknown values.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct ViewerMarkdownConfig {
    /// One of: `default`, `dracula`, `solarized_dark`, `solarized_light`,
    /// `nord`, `gruvbox_dark`, `gruvbox_light`, `github_light`.
    pub theme: String,
}

impl Default for ViewerMarkdownConfig {
    fn default() -> Self {
        Self {
            theme: "default".into(),
        }
    }
}

/// Mouse-input policy (§19.14). Governs right-click semantics on the
/// interactive right-column panes (agents / shells).
///
/// All fields are optional in TOML thanks to `#[serde(default)]`; defaults
/// match the shipping behaviour described in §19.14.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct MouseConfig {
    /// When `true` (default), `Down(Right)` on agents / shells panes
    /// pastes the clipboard (matching Windows Terminal / conhost /
    /// iTerm2 defaults). Set to `false` to restore the legacy
    /// "copy-and-clear active selection, no paste" behaviour.
    ///
    /// Quick Look (left-column preview zone) is **read-only** and
    /// always uses copy semantics regardless of this flag —
    /// §19.14.2 invariant 36.
    pub right_click_paste: bool,
    // The pre-native-file-git schema also exposed `quicklook_scrollbar`
    // and `yazi_layout` here; both drove the yazi PTY zone router
    // which retired with the native file/git panes. Legacy configs
    // that still carry those keys are stripped by
    // [`crate::migrate::migrate_pre_native_file_git`] before load so
    // `deny_unknown_fields` on this struct doesn't hard-fail them.
}

impl Default for MouseConfig {
    fn default() -> Self {
        Self {
            right_click_paste: true,
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

/// Files quadrant — the native two-pane file manager (post
/// native-file-git refactor). No more `[[files.tabs]]` array; these
/// are user-tweakable **defaults** consumed by the file-manager panes
/// at startup and mirrored into per-workspace [`FilesState`] the
/// first time a workspace is opened.
///
/// [`FilesState`]: crate::files_state::FilesState
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct FilesConfig {
    /// Default directory shown by the left pane on first open of a
    /// workspace. Interpreted relative to the workspace root when
    /// relative; absolute paths are honoured as-is.
    pub left_dir: PathBuf,
    /// Default directory shown by the right pane on first open.
    pub right_dir: PathBuf,
    /// Whether hidden entries are visible by default.
    pub show_hidden: bool,
    /// Stable sort-mode label (`"name"`, `"modified"`, …).
    pub sort: String,
    /// Whether both panes are visible by default.
    pub dual_pane: bool,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            left_dir: PathBuf::from("."),
            right_dir: PathBuf::from("."),
            show_hidden: false,
            sort: "name".into(),
            dual_pane: true,
        }
    }
}

/// Git integration — the native diff / commit-log viewer that
/// replaces the retired gitui external tool.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct GitConfig {
    /// Master switch for the native git panes. When `false` the git
    /// column is hidden and no `gix` calls run.
    pub enabled: bool,
    /// Cap on the number of commits fetched for the log view. Bounded
    /// so a monorepo with 100k+ commits doesn't stall startup.
    pub commit_limit: u32,
    /// Diff-view layout: `"auto"` (splits by terminal width),
    /// `"unified"` (single column), or `"split"` (two columns).
    pub diff_layout: String,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            commit_limit: 200,
            diff_layout: "auto".into(),
        }
    }
}

/// Sysmon quadrant (`bottom`, `trippy`, …). Fixed tab-group.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct SysmonConfig {
    pub tabs: Vec<ExternalToolSpec>,
}

impl Default for SysmonConfig {
    fn default() -> Self {
        // The `bottom` PTY entry retired in C25 — the Native SysmonPane
        // owns the left-bottom quadrant now, no external process. The
        // `tabs` field survives as an extension slot so a user can wire
        // a bespoke sysmon plugin (e.g. `bandwhich`, `trippy`) via
        // `[[sysmon.tabs]]` in `config.toml`; nothing is seeded by
        // default.
        Self { tabs: Vec::new() }
    }
}

/// Stock-pane refresh policy. Open-market updates are capped at the configured
/// rate; closed markets use a slow poll so stale quotes can still catch up.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct StockConfig {
    /// Open-market refresh rate in Hz. `1` means one refresh per second.
    pub open_refresh_hz: u16,
    /// Closed-market refresh interval in seconds.
    pub closed_refresh_secs: u64,
    /// Optional HTTP(S) proxy used by akshare requests.
    pub http_proxy: Option<String>,
    /// Optional Tushare token used by akshare's A-share fallback.
    pub tushare_token: Option<String>,
}

impl Default for StockConfig {
    fn default() -> Self {
        Self {
            open_refresh_hz: 1,
            closed_refresh_secs: 60,
            http_proxy: None,
            tushare_token: None,
        }
    }
}

/// Zones-pane knobs: home-zone override, refresh cadence, side-list toggle.
/// Rendered by `rimeterm-tui::zones_pane` on top of the `rimeterm-zones`
/// braille globe.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct ZonesConfig {
    /// Explicit override for the "home" zone marker. `None` (default) calls
    /// [`iana_time_zone::get_timezone`] via `rimeterm-zones`.
    pub home: Option<String>,
    /// Repaint interval in seconds. The subsolar point moves ~0.25°/minute,
    /// so faster is wasted work in a terminal pane.
    pub refresh_secs: u32,
    /// Show the vertical zone list next to the map when the pane is wide
    /// enough (>= 100 cols).
    pub show_side_list: bool,
    /// Default work window used to colour markers Core / Shoulder / Off
    /// when no per-zone window is configured. Format `"HH:MM-HH:MM"`.
    pub default_window: String,
    /// Shoulder minutes flanking the work window. `1` → one hour on each side.
    pub shoulder_hours: u16,
}

impl Default for ZonesConfig {
    fn default() -> Self {
        Self {
            home: None,
            refresh_secs: 60,
            show_side_list: true,
            default_window: "09:00-17:00".to_string(),
            shoulder_hours: 1,
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
    fn default_files_config_matches_native_two_pane_schema() {
        let f = FilesConfig::default();
        assert_eq!(f.left_dir, PathBuf::from("."));
        assert_eq!(f.right_dir, PathBuf::from("."));
        assert!(!f.show_hidden);
        assert_eq!(f.sort, "name");
        assert!(f.dual_pane);
    }

    #[test]
    fn default_git_config_matches_native_schema() {
        let g = GitConfig::default();
        assert!(g.enabled);
        assert_eq!(g.commit_limit, 200);
        assert_eq!(g.diff_layout, "auto");
    }

    #[test]
    fn files_git_partial_toml_only_overrides_named_fields() {
        let cfg: Config =
            toml::from_str("[files]\nshow_hidden = true\n[git]\ncommit_limit = 42\n").unwrap();
        assert!(cfg.files.show_hidden);
        assert_eq!(cfg.files.sort, "name");
        assert_eq!(cfg.git.commit_limit, 42);
        assert!(cfg.git.enabled);
    }

    #[test]
    fn default_sysmon_config_has_no_seeded_tabs() {
        // C25: bottom is now the Native SysmonPane; nothing is seeded.
        // The `tabs` array survives as an extension slot for user-added
        // PTY plugins configured via `[[sysmon.tabs]]`.
        assert!(SysmonConfig::default().tabs.is_empty());
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

    #[test]
    fn default_mouse_config_matches_docs() {
        let m = MouseConfig::default();
        assert!(m.right_click_paste);
    }

    #[test]
    fn mouse_config_partial_toml() {
        let cfg: Config = toml::from_str("[mouse]\nright_click_paste = false\n").unwrap();
        assert!(!cfg.mouse.right_click_paste);
    }

    #[test]
    fn mouse_config_rejects_legacy_zone_keys() {
        // The pre-native-file-git schema exposed `yazi_layout` /
        // `quicklook_scrollbar` here. Configs that still carry them
        // MUST be scrubbed by migrate::migrate_pre_native_file_git
        // before load; a raw parse against MouseConfig hard-fails.
        let err_layout =
            toml::from_str::<Config>("[mouse]\nyazi_layout = [1, 4, 3]\n").unwrap_err();
        assert!(err_layout.to_string().contains("unknown field"));
        let err_scroll =
            toml::from_str::<Config>("[mouse]\nquicklook_scrollbar = true\n").unwrap_err();
        assert!(err_scroll.to_string().contains("unknown field"));
    }

    #[test]
    fn stock_config_defaults_to_one_hz_open_and_slow_closed() {
        let stock = StockConfig::default();
        assert_eq!((stock.open_refresh_hz, stock.closed_refresh_secs), (1, 60));
    }

    #[test]
    fn stock_config_partial_toml_overrides_refresh_rates() {
        let cfg: Config =
            toml::from_str("[stock]\nopen_refresh_hz = 2\nclosed_refresh_secs = 120\n").unwrap();
        assert_eq!(
            (cfg.stock.open_refresh_hz, cfg.stock.closed_refresh_secs),
            (2, 120)
        );
    }
}
