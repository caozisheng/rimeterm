//! **Agent Registry** — canonical description of the four coding agents
//! rimeterm knows how to embed. Parallel to the §9.4 tools registry but
//! agents don't have a `cargo install` channel (their upstreams ship via
//! npm / pip / binary release / OS package), so this module only handles
//! **detection + presentation**.
//!
//! Design decision (v0.2 + C14 interjection):
//! - Agents are **not bundled**; rimeterm probes via `which::which`.
//! - The `agents` quadrant starts **empty on first launch**; `Ctrl+T`
//!   inside it opens a picker of detected agents.
//! - Missing agents show up in the picker as disabled with an install
//!   hint (users can toggle "show unavailable" in the picker; C14 default
//!   hides them so the list is short).

use std::path::PathBuf;

use serde::Serialize;

/// One row in the agent registry.
#[derive(Clone, Debug, Serialize)]
pub struct AgentSpec {
    /// Stable id used by `rimectl workspace.pane.open --kind agent:<id>`.
    pub id: &'static str,
    /// Human-facing label shown in the picker.
    pub label: &'static str,
    /// Binary that `which::which` looks up.
    pub binary: &'static str,
    /// argv the picker fires when the user selects this row.
    ///
    /// v0.1 is a single-element vec (`[binary]`); future revisions will add
    /// per-agent flags (`--profile`, `--session-file`, …).
    pub argv: &'static [&'static str],
    /// One-line install hint shown when the binary is missing.
    pub install_hint: &'static str,
}

/// The coding agents rimeterm knows how to embed out of the box.
///
/// **Order is alphabetical (case-insensitive by label)**; the picker
/// walks this slice top-to-bottom. Missing binaries appear greyed with
/// their install hint so the full menu stays visible even before the
/// user has installed any agent.
///
/// **Adding a new agent**: append a new `AgentSpec` here in alphabetical
/// position AND add one `agent_pick_cmd!` macro call in
/// `crates/rimeterm-tui/src/app.rs` (same id literal) so the picker has
/// a command to dispatch. The `registry_alphabetical` test locks the
/// ordering.
pub const AGENT_REGISTRY: &[AgentSpec] = &[
    AgentSpec {
        id: "antigravity",
        label: "Antigravity",
        binary: "antigravity",
        argv: &["antigravity"],
        install_hint: "Install: `curl -fsSL https://antigravity.google/cli/install.sh | bash`",
    },
    AgentSpec {
        id: "claude",
        label: "Claude Code",
        binary: "claude",
        argv: &["claude"],
        install_hint: "Install: `npm i -g @anthropic-ai/claude-code`",
    },
    AgentSpec {
        id: "codebuddy",
        label: "CodeBuddy",
        binary: "codebuddy",
        argv: &["codebuddy"],
        install_hint: "Install: `npm i -g @tencent-ai/codebuddy-code`",
    },
    AgentSpec {
        id: "codex",
        label: "Codex",
        binary: "codex",
        argv: &["codex"],
        install_hint: "Install: `npm i -g @openai/codex-cli`",
    },
    AgentSpec {
        id: "copilot",
        label: "Copilot",
        binary: "copilot",
        argv: &["copilot"],
        install_hint: "Install: `brew install copilot-cli` or `curl -fsSL https://gh.io/copilot-install | bash`",
    },
    AgentSpec {
        id: "cursor",
        label: "Cursor",
        // Cursor ships two binaries — `cursor` is the desktop editor,
        // `cursor-agent` is the CLI agent. We probe the CLI one.
        binary: "cursor-agent",
        argv: &["cursor-agent"],
        install_hint: "Install: `curl https://cursor.com/install -fsS | bash`",
    },
    AgentSpec {
        id: "gemini",
        label: "Gemini CLI",
        binary: "gemini",
        argv: &["gemini"],
        install_hint: "Install: `npm i -g @google/gemini-cli` (superseded by Antigravity CLI)",
    },
    AgentSpec {
        id: "hermes",
        label: "Hermes",
        binary: "hermes",
        argv: &["hermes"],
        install_hint: "Install: https://github.com/NousResearch/hermes-agent",
    },
    AgentSpec {
        id: "kimi",
        label: "Kimi",
        binary: "kimi",
        argv: &["kimi"],
        install_hint: "Install: `npm i -g @moonshot-ai/kimi-code`",
    },
    AgentSpec {
        id: "kiro",
        label: "Kiro CLI",
        binary: "kiro-cli",
        argv: &["kiro-cli"],
        install_hint: "Install: https://kiro.dev/docs/cli/installation",
    },
    AgentSpec {
        id: "omp",
        label: "Oh-My-Pi",
        binary: "omp",
        argv: &["omp"],
        install_hint: "Install: https://github.com/anthropics/oh-my-pi",
    },
    AgentSpec {
        id: "openclaw",
        label: "OpenClaw",
        binary: "openclaw",
        argv: &["openclaw"],
        install_hint: "Install: `curl -fsSL https://openclaw.ai/install.sh | bash`",
    },
    AgentSpec {
        id: "opencode",
        label: "OpenCode",
        binary: "opencode",
        argv: &["opencode"],
        install_hint: "Install: `npm i -g opencode-ai`",
    },
    AgentSpec {
        id: "pi",
        label: "Pi",
        binary: "pi",
        argv: &["pi"],
        install_hint: "Install: https://github.com/inflection-ai/pi",
    },
    AgentSpec {
        id: "qoder",
        label: "Qoder",
        binary: "qoder",
        argv: &["qoder"],
        install_hint: "Install: `curl -fsSL https://qoder.com/install | bash`",
    },
    AgentSpec {
        id: "qwen",
        label: "Qwen Code",
        binary: "qwen",
        argv: &["qwen"],
        install_hint: "Install: `npm i -g @qwen-code/qwen-code`",
    },
];

/// Serialised state of one agent — result of probing [`AGENT_REGISTRY`].
#[derive(Clone, Debug, Serialize)]
pub struct DetectedAgent {
    pub id: &'static str,
    pub label: &'static str,
    pub binary: &'static str,
    pub argv: &'static [&'static str],
    pub install_hint: &'static str,
    /// `None` when `which::which` failed.
    pub detected_path: Option<PathBuf>,
}

impl DetectedAgent {
    pub fn is_available(&self) -> bool {
        self.detected_path.is_some()
    }
}

/// Probe every entry in [`AGENT_REGISTRY`]. Cheap — one `which` per row.
pub fn detect_all() -> Vec<DetectedAgent> {
    AGENT_REGISTRY.iter().map(detect_one).collect()
}

/// Probe a single spec. Split out so tests can drive individual rows.
pub fn detect_one(spec: &'static AgentSpec) -> DetectedAgent {
    let detected_path = which::which(spec.binary).ok();
    DetectedAgent {
        id: spec.id,
        label: spec.label,
        binary: spec.binary,
        argv: spec.argv,
        install_hint: spec.install_hint,
        detected_path,
    }
}

/// Look up a spec by id. `None` for unknown ids — the IPC / picker layer
/// uses this to reject arbitrary strings before spawning anything.
pub fn find(id: &str) -> Option<&'static AgentSpec> {
    AGENT_REGISTRY.iter().find(|s| s.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_sixteen_agents_in_alphabetical_order() {
        // Locks BOTH the count and the case-insensitive label ordering
        // so a new agent can't silently break the picker's sort. If
        // adding an agent, insert its `AgentSpec` in the right slot AND
        // append its id here.
        let ids: Vec<&str> = AGENT_REGISTRY.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![
                "antigravity",
                "claude",
                "codebuddy",
                "codex",
                "copilot",
                "cursor",
                "gemini",
                "hermes",
                "kimi",
                "kiro",
                "omp",
                "openclaw",
                "opencode",
                "pi",
                "qoder",
                "qwen",
            ]
        );
    }

    #[test]
    fn registry_labels_are_case_insensitively_alphabetical() {
        // Guard the human-facing ordering separately: labels don't
        // always sort the same as ids (e.g. "Kiro CLI" vs id "kiro",
        // or "Oh-My-Pi" vs id "omp"), so a pure id sort could still
        // yield an out-of-order picker.
        let labels: Vec<String> = AGENT_REGISTRY
            .iter()
            .map(|s| s.label.to_lowercase())
            .collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted, "AGENT_REGISTRY labels must be alphabetical");
    }

    #[test]
    fn registry_ids_are_unique() {
        // Duplicate ids would make `find` ambiguous and let one row
        // shadow another silently in the picker.
        let mut ids: Vec<&str> = AGENT_REGISTRY.iter().map(|s| s.id).collect();
        let dupes_would_show_here = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            dupes_would_show_here,
            "AGENT_REGISTRY ids must be unique"
        );
    }

    #[test]
    fn each_agent_has_nonempty_hint_and_argv() {
        for spec in AGENT_REGISTRY {
            assert!(
                !spec.install_hint.trim().is_empty(),
                "agent `{}` needs an install hint",
                spec.id
            );
            assert!(
                !spec.argv.is_empty(),
                "agent `{}` needs at least one argv element",
                spec.id
            );
            assert_eq!(
                spec.argv[0], spec.binary,
                "agent `{}` argv[0] should match binary",
                spec.id
            );
        }
    }

    #[test]
    fn find_hits_and_misses() {
        assert!(find("omp").is_some());
        assert!(find("codex").is_some());
        assert!(find("nope").is_none());
        assert!(find("").is_none());
    }

    #[test]
    fn detect_one_missing_reports_none_path() {
        // Fake spec so tests don't depend on host binaries.
        static BOGUS: AgentSpec = AgentSpec {
            id: "bogus",
            label: "Bogus",
            binary: "this-binary-definitely-does-not-exist-xyzzy",
            argv: &["this-binary-definitely-does-not-exist-xyzzy"],
            install_hint: "n/a",
        };
        let d = detect_one(&BOGUS);
        assert!(!d.is_available());
        assert!(d.detected_path.is_none());
    }
}
