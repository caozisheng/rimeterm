# glow.yazi

Yazi previewer plugin: renders Markdown files in the third column via
[glow](https://github.com/charmbracelet/glow) with heading, list, code
block, and link formatting.

Bundled by rimeterm so Markdown Quick Look reads as prose rather than
raw syntax-highlighted source.

## Wiring

`~/.rimeterm/yazi/yazi.toml` (seeded by rimeterm) contains:

```toml
[plugin]
prepend_previewers = [
    { name = "*.md",       run = "glow" },
    { name = "*.markdown", run = "glow" },
]
```

`glow` must be on the augmented PATH. rimeterm ships it at
`~/.rimeterm/bin/glow` on every supported platform.

## License

Apache-2.0. See [LICENSE](./LICENSE).
