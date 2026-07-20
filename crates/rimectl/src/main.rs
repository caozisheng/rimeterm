//! `rimectl` — CLI client for the local rimeterm IPC.
//!
//! Usage (v1):
//!
//! ```text
//! rimectl <command-id> [--pid <n>] [--json '<args>']
//! rimectl --wait <regex> --pane <id> [--timeout-ms N] [--poll-ms N] [--pid <n>]
//! rimectl --list-endpoints
//! ```
//!
//! Discovers the running rimeterm server via `${runtime}/rimeterm/<pid>.pid`
//! lockfiles (see [`rimeterm_ipc::endpoint`]), then sends the given
//! `<command-id>` and prints whatever the server responded.
//!
//! `--wait <regex>` is client-side sugar (C18-A) that expands to
//! `workspace.pane.wait --json '{...}'` so shell scripts can go
//! `write → wait` without hand-writing JSON.

use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use rimeterm_ipc::{Request, discover_latest_pid, send_once};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match real_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rimectl: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn real_main() -> Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match parse_cli(&raw).map_err(|e| anyhow!(e))? {
        CliAction::Help => {
            print_help();
            Ok(())
        }
        CliAction::ListEndpoints => list_endpoints().await,
        CliAction::Wait(w) => run_wait(w).await,
        CliAction::Command(c) => run_command(c).await,
    }
}

/// Structured CLI outcome. Parsing is a pure function so failure modes
/// (missing `--pane`, `--wait` combined with positional cmd, unparseable
/// numeric flag) are unit-testable without an IPC server.
#[derive(Debug, PartialEq)]
enum CliAction {
    Help,
    ListEndpoints,
    Wait(WaitInvocation),
    Command(CommandInvocation),
}

#[derive(Debug, PartialEq)]
struct WaitInvocation {
    pane_id: u64,
    pattern: String,
    /// `None` → server default (5_000 ms).
    timeout_ms: Option<u64>,
    /// `None` → server default (100 ms). Server clamps to [25, 1000].
    poll_ms: Option<u64>,
    pid: Option<u32>,
}

#[derive(Debug, PartialEq)]
struct CommandInvocation {
    cmd: String,
    args_json: Option<String>,
    pid: Option<u32>,
}

/// Parse a flat argv slice into a [`CliAction`]. Kept pure (no IO, no
/// process::exit) so tests can drive the full matrix — help, endpoints,
/// `--wait` sugar, and the generic `<cmd-id>` fall-through — from a
/// `&[String]`.
///
/// Grammar (informal):
///
/// ```text
/// argv = help | list-endpoints | wait-form | cmd-form
/// help          = -h | --help
/// list-endpoints= --list-endpoints
/// wait-form     = --wait <regex> --pane <id>
///                 [--timeout-ms <n>] [--poll-ms <n>] [--pid <n>]
/// cmd-form      = <cmd-id> [--pid <n>] [--json <string>]
/// ```
fn parse_cli(argv: &[String]) -> Result<CliAction, String> {
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(CliAction::Help);
    }
    if argv.iter().any(|a| a == "--list-endpoints") {
        return Ok(CliAction::ListEndpoints);
    }

    // Collect flags first — order-independent — leaving positional args behind.
    let mut positional: Vec<String> = Vec::new();
    let mut pid: Option<u32> = None;
    let mut json: Option<String> = None;
    let mut wait_pattern: Option<String> = None;
    let mut pane: Option<u64> = None;
    let mut timeout_ms: Option<u64> = None;
    let mut poll_ms: Option<u64> = None;

    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        match a {
            "--pid" => {
                pid = Some(
                    take_val(argv, &mut i, "--pid")?
                        .parse()
                        .map_err(|e| format!("--pid needs an unsigned integer: {e}"))?,
                );
            }
            "--json" => {
                json = Some(take_val(argv, &mut i, "--json")?);
            }
            "--wait" => {
                wait_pattern = Some(take_val(argv, &mut i, "--wait")?);
            }
            "--pane" => {
                pane = Some(
                    take_val(argv, &mut i, "--pane")?
                        .parse()
                        .map_err(|e| format!("--pane needs a u64 pane id: {e}"))?,
                );
            }
            "--timeout-ms" => {
                timeout_ms = Some(
                    take_val(argv, &mut i, "--timeout-ms")?
                        .parse()
                        .map_err(|e| format!("--timeout-ms needs a u64: {e}"))?,
                );
            }
            "--poll-ms" => {
                poll_ms = Some(
                    take_val(argv, &mut i, "--poll-ms")?
                        .parse()
                        .map_err(|e| format!("--poll-ms needs a u64: {e}"))?,
                );
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}` (try `rimectl --help`)"));
            }
            _ => {
                positional.push(argv[i].clone());
                i += 1;
            }
        }
    }

    // --- `--wait` sugar path ---
    if let Some(pattern) = wait_pattern {
        if !positional.is_empty() {
            return Err(format!(
                "--wait cannot be combined with a positional command \
                 (found `{}`); --wait maps to workspace.pane.wait",
                positional[0]
            ));
        }
        if json.is_some() {
            return Err(
                "--wait and --json are mutually exclusive; --wait builds the JSON for you".into(),
            );
        }
        let pane_id = pane.ok_or_else(|| "--wait requires --pane <id>".to_string())?;
        return Ok(CliAction::Wait(WaitInvocation {
            pane_id,
            pattern,
            timeout_ms,
            poll_ms,
            pid,
        }));
    }

    // --- `<cmd-id>` fall-through ---
    // Guard: `--pane` / `--timeout-ms` / `--poll-ms` only make sense with --wait.
    if pane.is_some() {
        return Err("--pane only valid with --wait".into());
    }
    if timeout_ms.is_some() {
        return Err("--timeout-ms only valid with --wait".into());
    }
    if poll_ms.is_some() {
        return Err("--poll-ms only valid with --wait".into());
    }

    if positional.is_empty() {
        return Err(
            "no command specified; try `rimectl app.quit`, `rimectl --wait <re> --pane <id>`, \
             or `rimectl --help`"
                .into(),
        );
    }
    if positional.len() > 1 {
        return Err(format!(
            "unexpected extra positional argument `{}` (only one <command-id> allowed)",
            positional[1]
        ));
    }
    Ok(CliAction::Command(CommandInvocation {
        cmd: positional.into_iter().next().unwrap(),
        args_json: json,
        pid,
    }))
}

/// Consume `argv[i+1]` as the value for the flag at `argv[i]`, advancing
/// `i` past both. Errors surface the flag name so the user sees which
/// flag was hanging without a value.
fn take_val(argv: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    let val = argv
        .get(*i + 1)
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))?;
    *i += 2;
    Ok(val)
}

async fn run_command(inv: CommandInvocation) -> Result<()> {
    let pid = resolve_pid(inv.pid).await?;
    let request_args = match inv.args_json.as_deref() {
        Some(s) => serde_json::from_str(s).context("--json is not valid JSON")?,
        None => serde_json::Value::Null,
    };
    let req = Request {
        cmd: inv.cmd,
        args: request_args,
    };
    let resp = send_once(pid, &req).await?;
    print_response(&resp)?;
    if !resp.ok {
        bail!("server returned error");
    }
    Ok(())
}

async fn run_wait(inv: WaitInvocation) -> Result<()> {
    let pid = resolve_pid(inv.pid).await?;
    let args = build_wait_args(&inv);
    let req = Request {
        cmd: "workspace.pane.wait".into(),
        args,
    };
    let resp = send_once(pid, &req).await?;
    print_response(&resp)?;
    if !resp.ok {
        bail!("server returned error");
    }
    // Distinguish "regex matched" vs "timed out". Scripts often want a
    // non-zero exit on timeout so `&&` chains break cleanly.
    let matched = resp
        .result
        .as_ref()
        .and_then(|r| r.get("matched"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !matched {
        bail!("wait timed out before pattern matched");
    }
    Ok(())
}

/// Build the JSON payload for `workspace.pane.wait` from a parsed
/// `--wait` invocation. Split out so the mapping is unit-testable without
/// spawning an IPC server.
fn build_wait_args(inv: &WaitInvocation) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("pane_id".into(), serde_json::json!(inv.pane_id));
    obj.insert("pattern".into(), serde_json::json!(inv.pattern));
    if let Some(t) = inv.timeout_ms {
        obj.insert("timeout_ms".into(), serde_json::json!(t));
    }
    if let Some(p) = inv.poll_ms {
        obj.insert("poll_ms".into(), serde_json::json!(p));
    }
    serde_json::Value::Object(obj)
}

async fn resolve_pid(explicit: Option<u32>) -> Result<u32> {
    match explicit {
        Some(p) => Ok(p),
        None => discover_latest_pid()
            .await?
            .ok_or_else(|| anyhow!("no running rimeterm found; pass `--pid <n>`")),
    }
}

fn print_response(resp: &rimeterm_ipc::Response) -> Result<()> {
    let out = serde_json::to_string_pretty(resp).context("pretty-print response")?;
    println!("{out}");
    Ok(())
}

async fn list_endpoints() -> Result<()> {
    let Some(dir) = rimeterm_ipc::lockfile_dir() else {
        println!("no lockfile dir on this platform");
        return Ok(());
    };
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("no rimeterm servers found at {}", dir.display());
            return Ok(());
        }
        Err(e) => return Err(e).context("read lockfile dir"),
    };
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".pid") else {
            continue;
        };
        let Ok(pid) = stem.parse::<u32>() else {
            continue;
        };
        let ep = rimeterm_ipc::endpoint_display_for_pid(pid).unwrap_or_else(|| "(unknown)".into());
        println!("pid={} endpoint={}", pid, ep);
    }
    Ok(())
}

#[allow(clippy::print_literal)]
fn print_help() {
    // Raw string + `{}` arg so `{...}` inside the help text isn't parsed
    // as a format specifier. Keeps the JSON examples verbatim.
    // `clippy::print_literal` at the fn level suppresses the "just inline
    // the literal" suggestion — inlining re-breaks the brace parse.
    print!(
        "{}",
        r#"rimectl — talk to the running rimeterm

USAGE:
    rimectl <command-id> [--pid <n>] [--json '<args>']
    rimectl --wait <regex> --pane <id> [--timeout-ms N] [--poll-ms N] [--pid <n>]
    rimectl --list-endpoints
    rimectl --help

EXAMPLES:
    rimectl app.palette.open
    rimectl workspace.tab.next
    rimectl workspace.layout.reset

    # Write to a pane and wait for a prompt (sugar for workspace.pane.wait):
    PANE=$(rimectl workspace.pane.open --json '{"kind":"shell"}' | jq -r .result.pane_id)
    rimectl workspace.pane.write --json "{\"pane_id\": $PANE, \"text\": \"cargo test\", \"enter\": true}"
    rimectl --wait 'test result:' --pane $PANE --timeout-ms 60000

SUGAR NOTES:
    `--wait` expands to `workspace.pane.wait --json '{...}'`. Exit code
    is 0 on match, non-zero on timeout — safe for `&&` chains.
    Server-side ranges: timeout_ms ≤ 60000, poll_ms ∈ [25, 1000].

DISCOVERY:
    Without --pid, rimectl connects to the most recently-started rimeterm
    on this machine (via lockfile mtime under the local runtime dir).
"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn help_flag_wins_over_everything() {
        assert_eq!(parse_cli(&argv(&["--help"])), Ok(CliAction::Help));
        assert_eq!(
            parse_cli(&argv(&["workspace.snapshot", "-h"])),
            Ok(CliAction::Help)
        );
    }

    #[test]
    fn list_endpoints_recognized() {
        assert_eq!(
            parse_cli(&argv(&["--list-endpoints"])),
            Ok(CliAction::ListEndpoints)
        );
    }

    #[test]
    fn plain_command_parses() {
        let got = parse_cli(&argv(&["app.quit"])).unwrap();
        assert_eq!(
            got,
            CliAction::Command(CommandInvocation {
                cmd: "app.quit".into(),
                args_json: None,
                pid: None,
            })
        );
    }

    #[test]
    fn plain_command_with_pid_and_json() {
        let got = parse_cli(&argv(&[
            "workspace.pane.close",
            "--pid",
            "1234",
            "--json",
            "{\"pane_id\":42}",
        ]))
        .unwrap();
        assert_eq!(
            got,
            CliAction::Command(CommandInvocation {
                cmd: "workspace.pane.close".into(),
                args_json: Some("{\"pane_id\":42}".into()),
                pid: Some(1234),
            })
        );
    }

    #[test]
    fn flag_order_does_not_matter() {
        // Same as above but with flag before cmd — must yield the same action.
        let got = parse_cli(&argv(&[
            "--pid",
            "1234",
            "--json",
            "{\"pane_id\":42}",
            "workspace.pane.close",
        ]))
        .unwrap();
        assert_eq!(
            got,
            CliAction::Command(CommandInvocation {
                cmd: "workspace.pane.close".into(),
                args_json: Some("{\"pane_id\":42}".into()),
                pid: Some(1234),
            })
        );
    }

    #[test]
    fn missing_command_is_a_helpful_error() {
        let err = parse_cli(&argv(&[])).unwrap_err();
        assert!(err.contains("no command specified"), "unexpected: {err}");
    }

    #[test]
    fn wait_requires_pane() {
        let err = parse_cli(&argv(&["--wait", "prompt>"])).unwrap_err();
        assert!(err.contains("--pane"), "unexpected: {err}");
    }

    #[test]
    fn wait_full_form_parses() {
        let got = parse_cli(&argv(&[
            "--wait",
            "test result:",
            "--pane",
            "7",
            "--timeout-ms",
            "60000",
            "--poll-ms",
            "200",
            "--pid",
            "9999",
        ]))
        .unwrap();
        assert_eq!(
            got,
            CliAction::Wait(WaitInvocation {
                pane_id: 7,
                pattern: "test result:".into(),
                timeout_ms: Some(60000),
                poll_ms: Some(200),
                pid: Some(9999),
            })
        );
    }

    #[test]
    fn wait_minimal_form_leaves_optionals_as_none() {
        let got = parse_cli(&argv(&["--wait", "PS>", "--pane", "3"])).unwrap();
        assert_eq!(
            got,
            CliAction::Wait(WaitInvocation {
                pane_id: 3,
                pattern: "PS>".into(),
                timeout_ms: None,
                poll_ms: None,
                pid: None,
            })
        );
    }

    #[test]
    fn wait_cannot_combine_with_positional_command() {
        let err = parse_cli(&argv(&[
            "--wait",
            "prompt>",
            "--pane",
            "1",
            "workspace.snapshot",
        ]))
        .unwrap_err();
        assert!(
            err.contains("--wait cannot be combined"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn wait_cannot_combine_with_json() {
        let err = parse_cli(&argv(&["--wait", "x", "--pane", "1", "--json", "{}"])).unwrap_err();
        assert!(err.contains("mutually exclusive"), "unexpected: {err}");
    }

    #[test]
    fn wait_flags_outside_wait_are_rejected() {
        // `--pane` alone without `--wait` is a mistake; catch it early
        // instead of silently ignoring like the old parser would.
        let err = parse_cli(&argv(&["workspace.snapshot", "--pane", "1"])).unwrap_err();
        assert!(
            err.contains("--pane only valid with --wait"),
            "unexpected: {err}"
        );

        let err = parse_cli(&argv(&["workspace.snapshot", "--timeout-ms", "1000"])).unwrap_err();
        assert!(
            err.contains("--timeout-ms only valid with --wait"),
            "unexpected: {err}"
        );

        let err = parse_cli(&argv(&["workspace.snapshot", "--poll-ms", "100"])).unwrap_err();
        assert!(
            err.contains("--poll-ms only valid with --wait"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn flag_missing_value_reports_flag_name() {
        for flag in [
            "--wait",
            "--pane",
            "--pid",
            "--json",
            "--timeout-ms",
            "--poll-ms",
        ] {
            let err = parse_cli(&argv(&[flag])).unwrap_err();
            assert!(
                err.contains(flag),
                "flag `{flag}` err didn't mention it: {err}"
            );
        }
    }

    #[test]
    fn unknown_flag_rejected() {
        let err = parse_cli(&argv(&["--nope"])).unwrap_err();
        assert!(err.contains("unknown flag"), "unexpected: {err}");
    }

    #[test]
    fn numeric_flag_bad_value_rejected() {
        let err = parse_cli(&argv(&["--wait", "x", "--pane", "not-a-number"])).unwrap_err();
        assert!(err.contains("--pane"), "unexpected: {err}");
        let err = parse_cli(&argv(&[
            "--wait",
            "x",
            "--pane",
            "1",
            "--timeout-ms",
            "abc",
        ]))
        .unwrap_err();
        assert!(err.contains("--timeout-ms"), "unexpected: {err}");
    }

    #[test]
    fn extra_positional_rejected() {
        let err = parse_cli(&argv(&["workspace.snapshot", "extra"])).unwrap_err();
        assert!(err.contains("extra positional"), "unexpected: {err}");
    }

    // --- build_wait_args mapping ---

    #[test]
    fn wait_args_minimal_omits_optionals() {
        let json = build_wait_args(&WaitInvocation {
            pane_id: 42,
            pattern: "PS>".into(),
            timeout_ms: None,
            poll_ms: None,
            pid: None,
        });
        assert_eq!(json, serde_json::json!({"pane_id": 42, "pattern": "PS>"}));
    }

    #[test]
    fn wait_args_full_includes_all_fields() {
        let json = build_wait_args(&WaitInvocation {
            pane_id: 42,
            pattern: "done".into(),
            timeout_ms: Some(30_000),
            poll_ms: Some(250),
            pid: None,
        });
        assert_eq!(
            json,
            serde_json::json!({
                "pane_id": 42,
                "pattern": "done",
                "timeout_ms": 30_000,
                "poll_ms": 250,
            })
        );
    }
}
