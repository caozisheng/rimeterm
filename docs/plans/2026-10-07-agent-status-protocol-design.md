# Agent Status Protocol and Adapter Design

**Date:** 2026-10-07  
**Status:** Approved for implementation  
**Scope:** First-party real-time status support for OMP, Claude Code, and Codex.

## Goal

Replace transcript/CPU-based workspace status inference with an explicit Agent status protocol. Add a standalone `rimeterm-agent-status` crate containing the protocol model and the first OMP, Claude Code, and Codex adapters. Workspace tabs observe only the first valid Agent pane in the workspace's `agents` group.

## Decisions

- Create a new workspace crate: `crates/rimeterm-agent-status`.
- The protocol is owned by rimeterm, not by any one Agent integration.
- First-party adapters: OMP, Claude Code, Codex.
- Transports: PTY OSC 1337 as the primary path and local `rimectl` IPC as the side path.
- No JSONL fallback for workspace status. Agents without a real-time adapter are shown without an observed workspace status.
- No CPU/process fallback for workspace status.
- If an `agents` group has multiple tabs, only the first valid Agent pane in member order owns the workspace status.
- The existing fixed symbol slot and 1 Hz symbol blink remain the presentation layer; colored backgrounds remain removed.

## Protocol model

The crate exposes a serializable full snapshot:

```rust
pub enum AgentLifecycle {
    Idle,
    Thinking,
    ToolRunning,
    WaitingUser,
    Success,
    Error,
    Interrupted,
    Compacting,
}

pub struct AgentStatusSnapshot {
    pub version: u16,
    pub agent: String,
    pub session_id: String,
    pub cwd: String,
    pub state: AgentLifecycle,
    pub tool: Option<String>,
    pub activity: Option<String>,
    pub message: Option<String>,
    pub seq: u64,
}
```

Each message is a complete current-state snapshot, not an incremental event. Receivers reject unsupported versions, stale sequence numbers, oversized payloads, and malformed state values.

## Transport

### OSC 1337

Agents running inside a rimeterm PTY emit:

```text
ESC ] 1337 ; rimeterm ; <JSON> BEL
```

The existing PTY OSC scanner and `SessionOutput::OscRimeterm` path deliver the payload together with the originating `PaneId`.

### rimectl

Agents or hooks that cannot safely write OSC use:

```text
rimectl agent.status --json '<snapshot>'
rimectl agent.status.clear --agent omp --session '<id>'
rimectl agent.status.list
```

The CLI forwards the standard snapshot through the existing local IPC command registry. The CLI does not implement Agent-specific mapping.

## Adapter model

`rimeterm-agent-status` contains one adapter module per supported Agent:

```text
src/
  protocol.rs
  adapter.rs
  omp.rs
  claude_code.rs
  codex.rs
  validation.rs
```

Adapters map Agent-native lifecycle events to `AgentStatusSnapshot`. They do not parse transcripts.

### OMP

Map `rime-omp-pet`/OMP lifecycle events:

- `agent_start`, `turn_start` → `Thinking`
- `tool_execution_start` → `ToolRunning`
- `tool_approval_requested` → `WaitingUser`
- approved tool → `ToolRunning`; denied tool → `Thinking`
- successful tool end → `Thinking`
- failed tool end → `Error`
- compaction start → `Compacting`
- compaction end → `Thinking`
- successful `agent_end` → `Success`
- failed `agent_end` → `Error`
- `session_stop` → `Idle`

### Claude Code

Use its supported hook/event surface to emit equivalent full snapshots. No Claude transcript parsing belongs in the adapter.

### Codex

Use its supported hook/event surface to emit equivalent full snapshots. No Codex transcript parsing belongs in the adapter.

## Rimeterm state ownership

The TUI owns a live status table keyed by:

```text
(PaneId, agent, session_id)
```

Each record stores the latest snapshot, source, sequence, and last-seen time. OSC messages bind directly to their originating PaneId. `rimectl` messages bind by `agent + session_id + cwd`.

Source precedence:

```text
OSC from Pane > rimectl
```

There is no JSONL, CPU, or process-status fallback for workspace status. A missing or stale record means the workspace has no observed status.

## First-Agent workspace rule

For each workspace:

1. Read the `agents` group member order.
2. Skip the Agent picker placeholder.
3. Pick the first real Agent pane as the status owner.
4. Ignore all later Agent panes for workspace-tab status.
5. If the owner closes, the next real Agent becomes owner.
6. If the owner has no supported live status, show the workspace's empty symbol slot; never switch to the second Agent as an implicit fallback.

## Presentation mapping

```text
Idle / no observed status → blank fixed symbol slot
Thinking / ToolRunning / Compacting → ●
WaitingUser → ?
Success → ✓
Error / Interrupted → !
```

The symbol slot always occupies one display cell before the workspace title. Non-idle symbols blink at a 1 Hz cycle. The current workspace retains reverse-video styling; all tabs use plain backgrounds.

## Error handling

- Payload size is bounded before JSON decoding.
- Unknown protocol versions are ignored with debug logging.
- Unknown states are rejected, not guessed.
- Sequence numbers must not decrease for a `(PaneId, agent, session_id)` record.
- Stale status expires to no observed status.
- Pane close removes its status bindings.
- Malformed or unauthorized local IPC requests return a command error.

## Testing boundaries

Permanent tests must cover:

- Protocol serialization/deserialization and validation boundaries.
- Sequence rejection and stale-record cleanup.
- OMP, Claude Code, and Codex event-to-snapshot mappings.
- OSC payload decoding and PaneId binding.
- `rimectl` request routing.
- First-Agent selection with multiple tabs and picker placeholder.
- No fallback to a later Agent tab.
- Workspace symbol mapping, fixed width, and blink phase.

Transcript parsing tests remain only for legacy diagnostic components if still needed; they must not define workspace status behavior.
