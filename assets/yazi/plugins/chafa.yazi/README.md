# chafa.yazi

Yazi previewer plugin: renders images in the third column via
[chafa](https://hpjansson.org/chafa/) using coloured Unicode symbols.

Bundled by rimeterm so image Quick Look works on terminals that lack
Sixel / Kitty / iTerm2 protocols (notably Windows Terminal).

## Wiring

`~/.rimeterm/yazi/yazi.toml` (seeded by rimeterm) contains:

```toml
[plugin]
prepend_previewers = [
    { mime = "image/*", run = "chafa" },
]
```

`chafa` must be on the augmented PATH. rimeterm ships it at
`~/.rimeterm/bin/chafa` on every supported platform except macOS —
where terminals almost always have a native image protocol and this
plugin isn't needed.

## License

Apache-2.0. See [LICENSE](./LICENSE).
