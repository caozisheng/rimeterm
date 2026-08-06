# Upstream

This crate is a bounded library fork of `glab-tui` at upstream commit
`c11c244a43d9cc1c71952ab887d09c9bba9476f3`.

The upstream MIT license is retained in `LICENSE`. The original terminal
entry point is intentionally excluded: RimeTerm embeds the library through
`EmbeddedApp` and owns terminal setup, input dispatch, rendering, and the
workspace root.
