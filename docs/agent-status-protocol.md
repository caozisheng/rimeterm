# Realtime Agent Status Protocol

Rimeterm workspace tabs consume explicit lifecycle snapshots from Agent adapters. Transcript files, CPU usage, and process heuristics are not used as workspace-status fallbacks.

## Supported adapters

The first implementation ships protocol adapters for:

- Oh-My-Pi (`omp`)
- Claude Code (`claude`)
- Codex (`codex`)

Other Agent binaries in the rimeterm registry still run normally, but their workspace tabs show an empty status slot until an adapter emits this protocol.

## Snapshot format

```json
{
  "version": 1,
  "type": "agent_status",
  "agent": "omp",
  "session_id": "session-123",
  "cwd": "C:\\work\\gridflow",
  "state": "tool_running",
  "tool": "bash",
  "activity": "cargo test",
  "seq": 42
}
```

States: `idle`, `thinking`, `tool_running`, `waiting_user`, `success`, `error`, `interrupted`, `compacting`.

Each message is a complete snapshot. `seq` must increase within one Agent session.

## OSC transport

An Agent running in a rimeterm PTY can emit the JSON through the existing OSC bridge:

```text
ESC ] 1337 ; rimeterm ; <JSON> BEL
```

The originating PTY supplies the PaneId binding automatically.

## rimectl transport


## Hook bridge command

The crate also ships `rimeterm-agent-status`, a small hook-friendly emitter. It maps a lifecycle event to one full snapshot and writes the OSC sequence to stdout:

```powershell
rimeterm-agent-status emit --agent omp --session "$env:OMP_SESSION_ID" --cwd "$PWD" --event tool_start --tool bash --activity "cargo test"
rimeterm-agent-status emit --agent claude --session "$CLAUDE_SESSION_ID" --cwd "$PWD" --event waiting_user
rimeterm-agent-status emit --agent codex --session "$CODEX_SESSION_ID" --cwd "$PWD" --event agent_end
```

Hook systems should forward stdout unchanged to the Agent PTY. The command is intentionally stateless; adapters should provide a stable session id and invoke it for every lifecycle transition.
Hooks that cannot write OSC can use the generic IPC command interface:

```powershell
rimectl agent.status --json '{"version":1,"type":"agent_status","agent":"claude","session_id":"s1","cwd":"C:\\work\\repo","state":"thinking","seq":1}'
```

Clear and inspect records:

```text
rimectl agent.status.clear --json '{"agent":"claude","session_id":"s1"}'
rimectl agent.status.list
```

IPC binding uses the Agent id and canonical workspace cwd. The matching Agent must be the first real tab in the workspace's `agents` group.

## Workspace ownership

Only the first real Agent tab controls a workspace tab's status. Picker placeholders are skipped. Later Agent tabs never contribute, even if the first Agent is unsupported or has no status.

## Workspace symbols

- blank: no observation or `idle`
- `●`: `thinking`, `tool_running`, `compacting`
- `?`: `waiting_user`
- `✓`: `success`
- `!`: `error`, `interrupted`

Every tab reserves one status cell. Non-idle symbols blink on a one-second cycle; the title and hitbox never move.
