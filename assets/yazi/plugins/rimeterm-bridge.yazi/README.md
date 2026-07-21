# rimeterm-bridge.yazi (internal source)

This directory is the **repo-side source-of-truth** for the Yazi plugin
that rimeterm bundles. End users **do not install this by hand** — it's
baked into the rimeterm binary via `include_bytes!` and materialized to
`~/.rimeterm/yazi/plugins/rimeterm-bridge.yazi/` on first launch.

For end-user setup (which is essentially "nothing"), see
[`docs/yazi-setup.md`](../../../../docs/yazi-setup.md).

## What the plugin does

- Subscribes to Yazi's `hover` DDS topic. Every cursor move fires
  `rimectl osc-emit file.selected <path>` — a fire-and-forget child
  process whose stdout is Yazi's PTY, so the OSC bytes flow straight
  into rimeterm.
- Subscribes to `cd`. Directory changes fire
  `rimectl osc-emit cwd.changed <path>`.
- Why a subprocess and not `io.stdout:write`? Yazi's mlua sandbox
  intercepts direct stdout writes. Spawning a child that inherits the
  PTY is the only reliable path.
- rimeterm's non-destructive OSC scanner (§5.5 of the design doc)
  reads `\x1b]1337;rimeterm;{…}\x07` from the PTY without disturbing
  Yazi's alt-screen. Alacritty sees the same sequence, doesn't
  recognize the `rimeterm;` param, and drops it silently.

## How it reaches users

1. Contributor edits `main.lua` in this directory.
2. `rimeterm-config::assets` module `include_bytes!`-bakes the file at
   build time.
3. Rimeterm binary ships to users; on first launch, the file is
   written to `~/.rimeterm/yazi/plugins/rimeterm-bridge.yazi/main.lua`.
4. On version bumps (a `CARGO_PKG_VERSION` change), the plugin dir is
   force-rewritten — user's `init.lua` and other seeded config stay
   put.

## Prerequisites at runtime

- `rimectl` must be on `PATH` inside Yazi's child env. rimeterm's
  first-launch bootstrap self-copies `rimectl` next to the essentials
  binaries into `~/.rimeterm/bin/`, and `spawn_external` prepends that
  dir to child `PATH`. So this "just works" from a fresh install.

## Modifying the plugin

- Edit `main.lua` here in the repo.
- Bump `CARGO_PKG_VERSION` (or wait for the next rimeterm release) so
  the version-marker check in `assets::materialize_configs` triggers
  a rewrite on user machines.
- Add a regression test in `crates/rimeterm-tui/src/app.rs` — the
  existing `osc_decode_matches_yazi_bridge_payload_shape` test locks
  in the exact JSON schema the plugin emits.

## License

Apache-2.0. See [LICENSE](./LICENSE).
