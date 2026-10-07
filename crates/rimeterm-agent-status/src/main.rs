use std::env;
use std::io::{self, Read, Write};

use rimeterm_agent_status::{
    AgentEvent, AgentStatusAdapter, ClaudeCodeAdapter, CodexAdapter, OmpAdapter, OpenCodeAdapter,
    QwenAdapter, encode_osc,
};

/// Best-effort hook mode error: hooks must never block the host agent.
/// A failing status bridge is an observability loss, not a workflow
/// error, so hook entrypoints swallow diagnostics and always exit 0.
fn hook_silent() -> ! {
    std::process::exit(0)
}

fn usage() -> ! {
    eprintln!(
        "usage: rimeterm-agent-status emit --agent <omp|claude|codex> --session <id> --cwd <path> --event <name> [--tool <name>] [--activity <text>] [--error] [--denied]"
    );
    std::process::exit(2)
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
        emit_codex_hook_osc();
        return;
    }
    if args.first().map(String::as_str) == Some("codex-hook-ipc") {
        emit_codex_hook_ipc();
        return;
    }
    if args.first().map(String::as_str) == Some("claude-hook") {
        emit_claude_hook_osc();
        return;
    }
    if args.first().map(String::as_str) == Some("claude-hook-ipc") {
        emit_agent_hook_ipc("claude");
        return;
    }
    if args.first().map(String::as_str) == Some("qwen-hook-ipc") {
        emit_agent_hook_ipc("qwen");
        return;
    }
    if args.first().map(String::as_str) == Some("opencode-hook-ipc") {
        emit_agent_hook_ipc("opencode");
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
        "omp" => encode_osc(&OmpAdapter::new(&session, &cwd).apply(event)),
        "claude" => encode_osc(&ClaudeCodeAdapter::new(&session, &cwd).apply(event)),
        "codex" => encode_osc(&CodexAdapter::new(&session, &cwd).apply(event)),
        "qwen" => encode_osc(&QwenAdapter::new(&session, &cwd).apply(event)),
        "opencode" => encode_osc(&OpenCodeAdapter::new(&session, &cwd).apply(event)),
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
fn parse_claude_hook() -> rimeterm_agent_status::AgentStatusSnapshot {
    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        hook_silent();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&input) else {
        hook_silent();
    };
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
    ClaudeCodeAdapter::new(session, cwd).apply(event)
}

fn parse_codex_hook() -> rimeterm_agent_status::AgentStatusSnapshot {
    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        hook_silent();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&input) else {
        hook_silent();
    };
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
    CodexAdapter::new(session, cwd).apply(event)
}

fn emit_codex_hook_osc() {
    let snapshot = parse_codex_hook();
    let encoded = encode_osc(&snapshot).unwrap_or_else(|error| {
        eprintln!("encode status: {error}");
        std::process::exit(1);
    });
    let _ = io::stdout().write_all(encoded.as_bytes());
    let _ = io::stdout().flush();
}

fn emit_codex_hook_ipc() {
    let snapshot = parse_codex_hook();
    let Ok(payload) = serde_json::to_string(&snapshot) else {
        hook_silent();
    };
    let _ = std::process::Command::new("rimectl")
        .args(["agent.status", "--json", &payload])
        .status();
}

fn emit_claude_hook_osc() {
    let snapshot = parse_claude_hook();
    let encoded = encode_osc(&snapshot).unwrap_or_else(|error| {
        eprintln!("encode status: {error}");
        std::process::exit(1);
    });
    let _ = io::stdout().write_all(encoded.as_bytes());
    let _ = io::stdout().flush();
}

fn emit_agent_hook_ipc(agent: &str) {
    let snapshot = parse_claude_hook();
    // The hook payload identifies the session/cwd; override the agent id
    // to the invoking CLI so IPC binding matches the workspace's first
    // Agent pane.
    let snapshot = rimeterm_agent_status::AgentStatusSnapshot {
        agent: agent.to_string(),
        ..snapshot
    };
    let Ok(payload) = serde_json::to_string(&snapshot) else {
        hook_silent();
    };
    let _ = std::process::Command::new("rimectl")
        .args(["agent.status", "--json", &payload])
        .status();
}
