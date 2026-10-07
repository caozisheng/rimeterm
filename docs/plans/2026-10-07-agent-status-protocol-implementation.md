# Realtime Agent Status Protocol Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a standalone `rimeterm-agent-status` crate and connect first-party OMP, Claude Code, and Codex realtime status snapshots to workspace tabs without JSONL fallback.

**Architecture:** The new crate owns protocol types, validation, sequence handling, and three Agent adapters. `rimeterm-tui` owns transport ingress, PaneId/session binding, first-Agent selection, stale status cleanup, and workspace-tab projection. PTY OSC 1337 is the primary transport; existing local IPC/rimectl is the secondary transport. Unsupported or stale status produces no observed workspace state.

**Tech Stack:** Rust 1.90, serde/serde_json, existing `rimeterm-pty` OSC scanner, existing `rimeterm-ipc` command registry, ratatui top-bar renderer, OMP/Claude/Codex hook adapters.

---

### Task 1: Create the protocol crate and workspace wiring

**Files:**
- Create: `crates/rimeterm-agent-status/Cargo.toml`
- Create: `crates/rimeterm-agent-status/src/lib.rs`
- Create: `crates/rimeterm-agent-status/src/protocol.rs`
- Create: `crates/rimeterm-agent-status/src/validation.rs`
- Modify: `Cargo.toml`
- Modify: `crates/rimeterm-tui/Cargo.toml`

**Steps:**
1. Add the crate to the workspace and declare only the necessary serde/serde_json dependencies.
2. Define `AgentLifecycle`, `AgentStatusSnapshot`, protocol constants, and source-independent validation errors.
3. Implement JSON round-trip and payload-size validation.
4. Add tests for supported states, unknown state rejection, version rejection, empty session IDs, and maximum payload size.
5. Run `cargo test -p rimeterm-agent-status` and confirm the new crate is independently usable.

### Task 2: Implement the three adapter modules

**Files:**
- Create: `crates/rimeterm-agent-status/src/adapter.rs`
- Create: `crates/rimeterm-agent-status/src/omp.rs`
- Create: `crates/rimeterm-agent-status/src/claude_code.rs`
- Create: `crates/rimeterm-agent-status/src/codex.rs`
- Test: the adapter modules' unit tests

**Steps:**
1. Define a common adapter trait/event input and sequence-producing snapshot builder.
2. Implement OMP lifecycle event mapping from the `rime-omp-pet` event vocabulary.
3. Implement Claude Code hook event mapping without transcript parsing.
4. Implement Codex hook event mapping without transcript parsing.
5. Test every state transition named in the approved design, including approval denial, tool failure, compaction, success, interruption, and idle/session stop.
6. Run `cargo test -p rimeterm-agent-status`.

### Task 3: Decode status snapshots from OSC 1337

**Files:**
- Modify: `crates/rimeterm-tui/src/app.rs`
- Modify: existing OSC decode helper location in `crates/rimeterm-tui/src/app.rs` or its current module
- Modify: `crates/rimeterm-tui/Cargo.toml` if the dependency is not inherited correctly
- Test: existing OSC decoder tests plus new status payload tests

**Steps:**
1. Add `rimeterm-agent-status` to the TUI dependencies.
2. Extend the decoded OSC result with an `AgentStatus` variant carrying the validated snapshot.
3. Preserve existing OSC behavior and malformed-payload diagnostics.
4. Route the originating `PaneId` and snapshot into App-owned status storage.
5. Add tests for valid status payloads, unsupported versions, oversized payloads, unknown states, and malformed JSON.
6. Run targeted OSC tests.

### Task 4: Add App-owned live status table and stale handling

**Files:**
- Modify: `crates/rimeterm-tui/src/app.rs`
- Modify: `crates/rimeterm-tui/src/workspace.rs` only if status cleanup needs bundle metadata
- Test: App/status helper unit tests

**Steps:**
1. Add a status record keyed by `(PaneId, agent, session_id)` containing snapshot, source, sequence, and last-seen time.
2. Accept a snapshot only when its sequence is not older than the stored record.
3. Implement stale expiry to “no observed status”; do not call AgentMonitor, CPU, or JSONL as fallback.
4. Remove pane status records when Agent panes close or workspace bundles are released.
5. Add pure tests for sequence ordering, stale expiry, pane cleanup, and source precedence (OSC over rimectl).
6. Run the focused status-store tests.

### Task 5: Implement `rimectl agent.status` ingress

**Files:**
- Modify: `crates/rimeterm-tui/src/app.rs` command registration
- Modify: `crates/rimectl/src/main.rs` only if dedicated CLI sugar is required by the existing command grammar
- Modify: protocol/IPC tests as needed

**Steps:**
1. Register `agent.status`, `agent.status.clear`, and `agent.status.list` in the existing command registry.
2. Validate JSON through the new crate before queuing the status mutation.
3. Route status snapshots without a PaneId through `(agent, session_id, cwd)` binding.
4. Return deterministic JSON responses for accepted, rejected, stale, and unknown-session cases.
5. Add command/IPC tests; keep the generic `rimectl <command-id> --json` path compatible.
6. Run `cargo test -p rimeterm-ipc -p rimeterm-tui` targeted tests.

### Task 6: Bind status to the first valid Agent tab only

**Files:**
- Modify: `crates/rimeterm-tui/src/app.rs`
- Modify: `crates/rimeterm-tui/src/workspace_activity.rs`
- Test: workspace status-owner tests

**Steps:**
1. Add a pure helper that scans an agents group member list, skips the picker placeholder, and returns the first real Agent pane.
2. Ensure later Agent panes never contribute to workspace status.
3. Make owner replacement deterministic after the first pane closes.
4. Make an owner without a realtime status render the empty symbol slot rather than falling back to another pane.
5. Add tests for one Agent, multiple Agents, picker-only, owner close, unsupported owner, and later Agent activity isolation.
6. Run the workspace-owner tests.

### Task 7: Connect realtime state to the existing symbol-only tab renderer

**Files:**
- Modify: `crates/rimeterm-tui/src/app.rs`
- Modify: `crates/rimeterm-tui/src/workspace_activity.rs`
- Modify: `crates/rimeterm-tui/src/top_bar.rs` only where the existing state projection needs the new observed state
- Modify: `crates/rimeterm-tui/tests/top_bar_snapshot.rs` if snapshot inputs change

**Steps:**
1. Map `AgentLifecycle` to the existing workspace symbol states.
2. Preserve the fixed one-cell symbol slot and 1 Hz blink phase.
3. Ensure no JSONL/CPU/AgentMonitor fallback can set workspace status.
4. Add tests proving realtime `tool_running`, `waiting_user`, `success`, and `error` states render the correct symbol and that absent status stays blank.
5. Run targeted top-bar and workspace-state tests.

### Task 8: Document adapters, hook contracts, and verification

**Files:**
- Modify: `README.md`
- Create or modify: a user-facing Agent status protocol document under `docs/`
- Modify: `docs/plans/2026-10-07-agent-status-protocol-design.md` only if implementation decisions materially change

**Steps:**
1. Document supported realtime adapters: OMP, Claude Code, Codex.
2. Document that other registered Agents may run but do not produce workspace status until an adapter exists.
3. Document OSC and `rimectl` hook examples.
4. Run `cargo test -p rimeterm-agent-status -p rimeterm-tui` and the relevant IPC tests.
5. Run `cargo fmt --all -- --check` and `git diff --check`.
6. Perform a TUI smoke run that exercises one supported Agent status path and one unsupported Agent path; report any environment limitation.

No branch creation is required; implementation remains on the user-requested local `main` branch.
