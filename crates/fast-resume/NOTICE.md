# fast-resume fork notice

This directory is an in-tree fork of [angristan/fast-resume](https://github.com/angristan/fast-resume), imported from commit `66e42cfd34bca4800161098d3b302a35a52ce69b` on 2026-07-29.

Upstream is licensed under the MIT License; see `LICENSE` in this directory.

rimeterm-specific changes add a non-terminal embedding API so the upstream session adapters, Tantivy index, query parser, and resume-command generation can be hosted by a native rimeterm pane. The standalone `fr` binary remains available.

The fork pins `rusqlite` 0.39 instead of upstream 0.40 because rimeterm's
Rust 1.90 toolchain cannot compile `libsqlite3-sys` 0.38's unstable
`cfg_select!` build-script usage. No fast-resume API or SQLite behavior is
changed by this compatibility pin.
