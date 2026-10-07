use std::env;
use std::io::{self, Read, Write};

use rimeterm_agent_status::{
    AgentEvent, AgentStatusAdapter, ClaudeCodeAdapter, CodexAdapter, OmpAdapter, encode_osc,
};

fn usage() -> ! {
    eprintln!(
        "usage: rimeterm-agent-status emit --agent <omp|claude|codex> --session <id> --cwd <path> --event <name> [--tool <name>] [--activity <text>] [--error] [--denied]"
    );
    std::process::exit(2);
}

fn value(args: &[String], flag: &str) -> String {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| usage())
}

fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("codex-hook") {
        emit_codex_hook();
        return;
    }
    if args.first().map(String::as_str) == Some("claude-hook") {
        emit_claude_hook();
        return;
    }
    if args.first().map(String::as_str) != Some("emit") {
        usage();
    }
    let agent = value(&args, "--agent");
    let session = value(&args, "--session");
    let cwd = value(&args, "--cwd");
    let event_name = value(&args, "--event");
    let event = AgentEvent::from_name(
        &event_name,
        args.windows(2)
            .find(|pair| pair[0] == "--tool")
            .map(|pair| pair[1].clone()),
        args.windows(2)
            .find(|pair| pair[0] == "--activity")
            .map(|pair| pair[1].clone()),
        has(&args, "--error"),
        !has(&args, "--denied"),
    )
    .unwrap_or_else(|| {
        eprintln!("unknown or incomplete Agent event `{event_name}`");
        std::process::exit(2);
    });

    let encoded = match agent.as_str() {
        "omp" => encode_osc(&OmpCode::new(&session, &cwd).apply(event)),
        "claude" => encode_osc(&ClaudeCode::new(&session, &cwd).apply(event)),
        "codex" => encode_osc(&Codex::new(&session, &cwd).apply(event)),
        _ => {
            eprintln!("unsupported realtime Agent `{agent}`");
            std::process::exit(2);
        }
    }
    .unwrap_or_else(|error| {
        eprintln!("encode status: {error}");
        std::process::exit(1);
    });
    let _ = io::stdout().write_all(encoded.as_bytes());
    let _ = io::stdout().flush();
}
fn emit_codex_hook() {
    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        std::process::exit(1);
    }
    let value: serde_json::Value = serde_json::from_str(&input).unwrap_or_else(|_| {
        eprintln!("invalid Codex notify JSON");
        std::process::exit(2);
    });
    let event_name = value
        .get("event")
        .and_then(|v| v.as_str())
        .unwrap_or("agent-turn-complete");
    let session = value
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let cwd = value.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    let event = match event_name {
        "agent-turn-complete" | "turn-complete" => AgentEvent::AgentEnd { error: false },
        "error" | "turn-failed" => AgentEvent::AgentEnd { error: true },
        "approval-requested" | "permission-request" => AgentEvent::ToolApprovalRequested,
        "tool-start" | "tool_execution_start" => AgentEvent::ToolStart {
            tool: value
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or("tool")
                .into(),
            activity: value
                .get("activity")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        },
        "tool-end" | "tool_execution_end" => AgentEvent::ToolEnd { error: false },
        _ => AgentEvent::AgentEnd { error: false },
    };
    let mut adapter = CodexAdapter::new(session, cwd);
    let snapshot = adapter.apply(event);
    let encoded = encode_osc(&snapshot).unwrap_or_else(|error| {
        eprintln!("encode status: {error}");
        std::process::exit(1);
    });
    let _ = io::stdout().write_all(encoded.as_bytes());
    let _ = io::stdout().flush();
}

fn emit_claude_hook() {
    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        std::process::exit(1);
    }
    let value: serde_json::Value = serde_json::from_str(&input).unwrap_or_else(|_| {
        eprintln!("invalid Claude Code hook JSON");
        std::process::exit(2);
    });
    let event_name = value
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("UserPromptSubmit");
    let session = value
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let cwd = value
        .get("cwd")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("workspace").and_then(|v| v.as_str()))
        .unwrap_or("");
    let tool = value
        .get("tool_name")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("toolName").and_then(|v| v.as_str()))
        .map(str::to_string);
    let activity = value
        .get("tool_input")
        .and_then(|v| {
            v.get("command")
                .or_else(|| v.get("description"))
                .or_else(|| v.get("file_path"))
        })
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let event = match event_name {
        "SessionStart" | "UserPromptSubmit" | "SubagentStart" => AgentEvent::TurnStart,
        "PreToolUse" => AgentEvent::ToolStart {
            tool: tool.unwrap_or_else(|| "tool".into()),
            activity,
        },
        "PermissionRequest" => AgentEvent::ToolApprovalRequested,
        "PostToolUse" => AgentEvent::ToolEnd { error: false },
        "PostToolUseFailure" => AgentEvent::ToolEnd { error: true },
        "PreCompact" => AgentEvent::CompactionStart,
        "Stop" | "SubagentStop" => AgentEvent::AgentEnd { error: false },
        "SessionEnd" => AgentEvent::SessionStop,
        _ => AgentEvent::TurnStart,
    };
    let mut adapter = ClaudeCodeAdapter::new(session, cwd);
    let snapshot = adapter.apply(event);
    let encoded = encode_osc(&snapshot).unwrap_or_else(|error| {
        eprintln!("encode status: {error}");
        std::process::exit(1);
    });
    let _ = io::stdout().write_all(encoded.as_bytes());
    let _ = io::stdout().flush();
}

struct OmpCode(OmpAdapter);
impl OmpCode {
    fn new(session: &str, cwd: &str) -> Self {
        Self(OmpAdapter::new(session, cwd))
    }
    fn apply(&mut self, event: AgentEvent) -> rimeterm_agent_status::AgentStatusSnapshot {
        self.0.apply(event)
    }
}
struct ClaudeCode(ClaudeCodeAdapter);
impl ClaudeCode {
    fn new(session: &str, cwd: &str) -> Self {
        Self(ClaudeCodeAdapter::new(session, cwd))
    }
    fn apply(&mut self, event: AgentEvent) -> rimeterm_agent_status::AgentStatusSnapshot {
        self.0.apply(event)
    }
}
struct Codex(CodexAdapter);
impl Codex {
    fn new(session: &str, cwd: &str) -> Self {
        Self(CodexAdapter::new(session, cwd))
    }
    fn apply(&mut self, event: AgentEvent) -> rimeterm_agent_status::AgentStatusSnapshot {
        self.0.apply(event)
    }
}
