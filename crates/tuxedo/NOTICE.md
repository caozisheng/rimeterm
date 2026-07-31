# tuxedo fork notice

This directory is an in-tree fork of [webstonehq/tuxedo](https://github.com/webstonehq/tuxedo), imported from commit `8c990c0e1f57462115c0d2dffdfffb3f0b63b7db` on 2026-07-31.

Upstream is licensed under the MIT License; see `LICENSE` in this directory.

rimeterm-specific changes add a bounded, non-terminal embedding API around the upstream todo.txt state machine, renderer, and input dispatcher. The standalone `tuxedo` binary and CLI remain available.
