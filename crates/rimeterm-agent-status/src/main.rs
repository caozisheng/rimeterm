use std::env;
use std::io::{self, Write};

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
