//! Realtime Agent status protocol and first-party adapters.

mod adapter;
mod claude_code;
mod codex;
mod omp;
mod opencode;
mod protocol;
mod qwen;
mod validation;

pub use adapter::{AgentEvent, AgentStatusAdapter};
pub use claude_code::ClaudeCodeAdapter;
pub use codex::CodexAdapter;
pub use omp::OmpAdapter;
pub use opencode::OpenCodeAdapter;
pub use protocol::{AgentLifecycle, AgentStatusSnapshot, PROTOCOL_VERSION, encode_osc};
pub use qwen::QwenAdapter;
pub use validation::{
    MAX_PAYLOAD_BYTES, StatusValidationError, validate_json_payload, validate_snapshot,
};

pub fn supports_agent(agent: &str) -> bool {
    matches!(agent, "omp" | "claude" | "codex" | "qwen" | "opencode")
}

pub const OMP_EXTENSION_SOURCE: &str = r#"import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";

export default async (pi: ExtensionAPI) => {
  const sessionId = `${process.pid}-${Date.now()}`;
  let seq = 0;
  const send = (ctx: { cwd?: string }, state: string, extra: Record<string, unknown> = {}) => {
    const payload = {
      version: 1, type: "agent_status", agent: "omp", session_id: sessionId,
      cwd: ctx.cwd ?? process.cwd(), state, seq: ++seq, ...extra,
    };
    process.stdout.write(`\u001b]1337;rimeterm;${JSON.stringify(payload)}\u0007`);
  };
  pi.on("session_start", (_event, ctx) => send(ctx as { cwd?: string }, "idle"));
  pi.on("agent_start", (_event, ctx) => send(ctx as { cwd?: string }, "thinking"));
  pi.on("turn_start", (_event, ctx) => send(ctx as { cwd?: string }, "thinking"));
  pi.on("tool_approval_requested", (_event, ctx) => send(ctx as { cwd?: string }, "waiting_user"));
  pi.on("tool_execution_start", (event, ctx) => send(ctx as { cwd?: string }, "tool_running", { tool: event.toolName }));
  pi.on("tool_execution_end", (event, ctx) => send(ctx as { cwd?: string }, event.isError ? "error" : "thinking"));
  pi.on("session.compacting", (_event, ctx) => send(ctx as { cwd?: string }, "compacting"));
  pi.on("auto_compaction_end", (_event, ctx) => send(ctx as { cwd?: string }, "thinking"));
  pi.on("agent_end", (event, ctx) => send(ctx as { cwd?: string }, event.willContinue ? "thinking" : "success"));
  pi.on("session_stop", (_event, ctx) => send(ctx as { cwd?: string }, "idle"));
};
"#;
