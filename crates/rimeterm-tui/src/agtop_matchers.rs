//! Regex table that maps a process cmdline to the canonical label of the
//! AI coding agent it represents (e.g. `"claude"`, `"codex"`, `"aider"`).
//!
//! Ported from the [agtop](https://crates.io/crates/agtop) crate by Matt
//! Brassey (MIT-licensed) — specifically `src/matchers.rs` @ v2.4.24. We
//! rebuild the detection intelligence in-process rather than shelling out
//! to the `agtop` binary, so users don't need a separate `cargo install`
//! to get the [`crate::agtop_pane::AgtopPane`] status view.
//!
//! Attribution note: keeping this file as a self-contained port means
//! any future upstream regex tweaks translate one-to-one — bump the
//! version reference above when re-syncing. All logic stays close to
//! upstream to make that re-sync trivial.

use regex::Regex;

/// One built-in regex → label mapping.
pub struct Matcher {
    pub label: &'static str,
    pub re: Regex,
}

/// User-supplied matcher parsed from a `label=regex` spec. Kept as
/// `String` labels (vs `&'static str`) so the caller can hand in
/// runtime-provided values from config / IPC without leaking allocations.
pub struct UserMatcher {
    pub label: String,
    pub re: Regex,
}

/// Build the default matcher table. Order matters: the first regex that
/// matches a cmdline wins, so the scoped-npm-package patterns
/// (`@anthropic-ai/claude-code`) precede the bare-binary ones (`claude`)
/// which they'd otherwise never trigger against.
pub fn builtin() -> Vec<Matcher> {
    // Word-boundary prefix: start of string, forward slash, backslash
    // (Windows paths), or whitespace.
    const P: &str = r"(^|[\s/\\])";
    // Trailing word-boundary: whitespace, end, or a Windows shim
    // suffix (.exe / .cmd / .ps1 / .bat). On Windows, npm-installed
    // CLIs are exposed as `<name>.cmd` shims and the bare exe shows
    // up as `<name>.exe`; without these the cmdline `claude.exe` or
    // `claude.cmd --print` would never match `claude(\s|$)`.
    const E: &str = r"(\.(exe|cmd|ps1|bat))?(\s|$)";
    let m = |label: &'static str, body: &str| Matcher {
        label,
        re: Regex::new(body).expect("builtin regex"),
    };
    let p = |s: &str| format!("{P}{s}");
    vec![
        m("claude", &p(&format!(r"claude(-code)?{E}"))),
        // Scoped npm package paths: forward slash on Linux/macOS, but
        // backslash on Windows (`...\node_modules\@anthropic-ai\claude-code\cli.js`).
        // Same for the other scoped agents below.
        m("claude-code", r"@anthropic-ai[/\\]claude-code"),
        m("codex", &p(&format!(r"codex{E}"))),
        m("openai-codex", r"@openai[/\\]codex"),
        m("aider", &p(r"aider(\s|$|\.)")),
        m("cursor-agent", &p(&format!(r"cursor-agent{E}"))),
        m("gemini", &p(&format!(r"gemini(-cli)?{E}"))),
        m("goose", &p(&format!(r"goose{E}"))),
        m("continue", &p(&format!(r"continue(-cli|-agent)?{E}"))),
        m("opencode", &p(&format!(r"opencode{E}"))),
        m("copilot", r"gh[\s-]copilot|github-copilot-cli"),
        m("cody", &p(&format!(r"cody{E}"))),
        m(
            "amp",
            r"(^|[\s/\\])amp(\.(exe|cmd|ps1|bat))?(\s|$)|@sourcegraph[/\\]amp",
        ),
        m("crush", &p(&format!(r"crush{E}"))),
        m("mods", &p(&format!(r"mods{E}"))),
        m("sgpt", &p(&format!(r"sgpt{E}"))),
        m("llm", &p(&format!(r"llm{E}"))),
        // oh-my-pi ships the bare `omp` binary AND the scoped npm
        // package `@oh-my-pi/pi-coding-agent`. Both surfaces classify
        // as `omp` (same label) so a single logical session doesn't
        // double-count in the pane when the launcher process spawns
        // a child bun/node worker running the shim — the pane's
        // parent/child dedupe collapses the pair to one row.
        m("omp", &p(&format!(r"omp{E}"))),
        m("omp", r"@oh-my-pi[/\\]"),
        m("ollama", &p(r"ollama(\s+(run|chat|serve)|$)")),
        m("fabric", &p(&format!(r"fabric{E}"))),
        m("block-goose", &p(&format!(r"goose-server{E}"))),
    ]
}

/// Parse a slice of `label=regex` specs (from CLI `-m` flags, config,
/// or IPC) into runnable matchers. Bogus rows (empty label / pattern,
/// invalid regex, oversized DFA) are skipped silently — a hostile
/// `--match` shouldn't take the whole pane down. The 1 MB `size_limit`
/// / `dfa_size_limit` combo comes from upstream agtop and caps
/// pathological patterns before they OOM the process.
pub fn parse_user_matchers(extra: &[String]) -> Vec<UserMatcher> {
    let mut out = Vec::new();
    for spec in extra {
        if let Some((label, pat)) = spec.split_once('=') {
            let label = label.trim().to_string();
            let pat = pat.trim();
            if label.is_empty() || pat.is_empty() {
                continue;
            }
            let built = regex::RegexBuilder::new(pat)
                .size_limit(1_000_000)
                .dfa_size_limit(1_000_000)
                .build();
            if let Ok(re) = built {
                out.push(UserMatcher { label, re });
            }
        }
    }
    out
}

/// Return the label of the first matcher that hits `cmdline`, or `None`
/// when the cmdline doesn't look like a known agent.
///
/// **ReDoS defense**: caps the regex-scanned prefix at 16 KiB. Real
/// agent cmdlines fit comfortably under 1 KiB; a hostile co-tenant
/// process with megabyte-scale argv combined with a pathological
/// user-supplied `-m` regex could otherwise spike CPU per tick.
pub fn classify<'a>(
    cmdline: &str,
    builtins: &'a [Matcher],
    user: &'a [UserMatcher],
) -> Option<&'a str> {
    if cmdline.is_empty() {
        return None;
    }
    const MAX_MATCH_BYTES: usize = 16 * 1024;
    let trimmed = if cmdline.len() > MAX_MATCH_BYTES {
        // Slice to the closest valid utf-8 boundary at or below the
        // cap so `regex` doesn't see a half-byte sequence.
        let mut end = MAX_MATCH_BYTES;
        while end > 0 && !cmdline.is_char_boundary(end) {
            end -= 1;
        }
        &cmdline[..end]
    } else {
        cmdline
    };
    for m in builtins {
        if m.re.is_match(trimmed) {
            return Some(m.label);
        }
    }
    for m in user {
        if m.re.is_match(trimmed) {
            return Some(m.label.as_str());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_agents() {
        let b = builtin();
        let u: Vec<UserMatcher> = vec![];
        assert_eq!(classify("/usr/bin/claude --resume", &b, &u), Some("claude"));
        assert_eq!(
            classify("node /opt/codex/bin/codex chat", &b, &u),
            Some("codex")
        );
        assert_eq!(classify("python -m aider --no-git", &b, &u), Some("aider"));
        assert_eq!(
            classify("/usr/bin/cursor-agent --watch", &b, &u),
            Some("cursor-agent")
        );
        assert_eq!(classify("/usr/bin/bash", &b, &u), None);
    }

    /// oh-my-pi ships as the bare `omp` binary AND the scoped npm
    /// package `@oh-my-pi/pi-coding-agent`. Both surfaces MUST
    /// classify — and both MUST produce the SAME `omp` label so a
    /// single logical session doesn't split into a `omp` row + an
    /// `oh-my-pi` row when the launcher spawns a bun/node worker.
    #[test]
    fn oh_my_pi_variants() {
        let b = builtin();
        let u: Vec<UserMatcher> = vec![];
        assert_eq!(classify("/usr/local/bin/omp", &b, &u), Some("omp"));
        assert_eq!(classify("omp --resume", &b, &u), Some("omp"));
        assert_eq!(
            classify(r"C:\Users\jake\AppData\Roaming\npm\omp.cmd chat", &b, &u),
            Some("omp")
        );
        assert_eq!(classify(r"C:\bin\omp.exe", &b, &u), Some("omp"));
        // The scoped-npm-package shape ALSO classifies as `omp` now
        // — this is the divergence-from-upstream fix that stops the
        // pane double-counting a single logical session.
        assert_eq!(
            classify(
                r"node C:\Users\jake\AppData\Roaming\npm\node_modules\@oh-my-pi\pi-coding-agent\dist\cli.js",
                &b,
                &u
            ),
            Some("omp")
        );
        assert_eq!(
            classify(
                "/usr/bin/node /opt/nvm/lib/node_modules/@oh-my-pi/pi-coding-agent/dist/cli.js",
                &b,
                &u
            ),
            Some("omp")
        );
        // Live shape from a real bun-launched session (this is what
        // was breaking on the user's Windows box).
        assert_eq!(
            classify(
                r"bun C:\Users\jake\.bun\..\node_modules\@oh-my-pi\pi-coding-agent\dist\cli.js",
                &b,
                &u
            ),
            Some("omp")
        );
    }

    #[test]
    fn user_matchers() {
        let b = builtin();
        let u = parse_user_matchers(&["myagent=python.*my_agent\\.py".to_string()]);
        assert_eq!(
            classify("python /home/x/my_agent.py --foo", &b, &u),
            Some("myagent")
        );
        // Builtin wins on its own pattern even when user matchers are set.
        assert_eq!(classify("/usr/bin/claude", &b, &u), Some("claude"));
    }

    #[test]
    fn empty_cmdline_returns_none() {
        let b = builtin();
        let u: Vec<UserMatcher> = vec![];
        assert_eq!(classify("", &b, &u), None);
    }

    #[test]
    fn user_matcher_ignores_bad_specs() {
        // Empty label, empty pattern, missing `=`, and an invalid
        // regex must all silently drop — a hostile config shouldn't
        // hijack the classifier.
        let u = parse_user_matchers(&[
            "".into(),
            "=foo".into(),
            "foo=".into(),
            "no-eq".into(),
            "bad=(".into(),
        ]);
        assert!(u.is_empty());
    }

    /// Windows paths use backslash separators and CLI shims expose the
    /// tool as `<name>.cmd` / `<name>.exe`. Pre-2.4.x upstream these
    /// were silent misses and produced an empty pane on Windows even
    /// when Claude/Codex were running — lock the regression in on our
    /// port too.
    #[test]
    fn windows_npm_global_paths() {
        let b = builtin();
        let u: Vec<UserMatcher> = vec![];
        assert_eq!(
            classify(
                r"C:\Program Files\nodejs\node.exe C:\Users\jake\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js",
                &b,
                &u
            ),
            Some("claude-code")
        );
        assert_eq!(
            classify(
                r"node.exe C:\Users\jake\AppData\Roaming\npm\node_modules\@openai\codex\dist\cli.js chat",
                &b,
                &u
            ),
            Some("openai-codex")
        );
        assert_eq!(
            classify(
                r"node C:\Users\jake\AppData\Roaming\npm\node_modules\@sourcegraph\amp\bin\amp.js",
                &b,
                &u
            ),
            Some("amp")
        );
    }

    #[test]
    fn windows_cmd_and_exe_shims() {
        let b = builtin();
        let u: Vec<UserMatcher> = vec![];
        assert_eq!(
            classify(
                r"C:\Users\jake\AppData\Roaming\npm\claude.cmd --print",
                &b,
                &u
            ),
            Some("claude")
        );
        assert_eq!(
            classify(r"C:\Users\jake\AppData\Roaming\npm\codex.cmd chat", &b, &u),
            Some("codex")
        );
        assert_eq!(classify(r"C:\bin\claude.exe", &b, &u), Some("claude"));
        assert_eq!(
            classify(r"C:\bin\gemini.exe --interactive", &b, &u),
            Some("gemini")
        );
        assert_eq!(classify(r"goose.exe session", &b, &u), Some("goose"));
    }

    #[test]
    fn oversized_cmdline_does_not_panic() {
        // 20 KiB cmdline exceeds the 16 KiB cap; the classifier must
        // still return a decision without slicing across a UTF-8
        // boundary.  Include a multi-byte glyph near the cap to force
        // the boundary-adjustment branch to fire.
        let b = builtin();
        let u: Vec<UserMatcher> = vec![];
        let filler = "字".repeat(6000); // ~18 KiB in UTF-8
        let cmdline = format!("/usr/bin/claude {}", filler);
        assert_eq!(classify(&cmdline, &b, &u), Some("claude"));
    }
}
