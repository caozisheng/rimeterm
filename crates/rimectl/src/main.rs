//! `rimectl` — CLI client for the local rimeterm IPC.
//!
//! Usage (v1):
//!
//! ```text
//! rimectl <command-id> [--pid <n>] [--json '<args>']
//! rimectl --list-endpoints
//! ```
//!
//! Discovers the running rimeterm server via `${runtime}/rimeterm/<pid>.pid`
//! lockfiles (see [`rimeterm_ipc::endpoint`]), then sends the given
//! `<command-id>` and prints whatever the server responded.

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
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }
    if args.iter().any(|a| a == "--list-endpoints") {
        return list_endpoints().await;
    }

    // Parse `--pid <n>` optionally.
    let mut explicit_pid: Option<u32> = None;
    let mut cmd_args_json: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pid" => {
                let val = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| anyhow!("--pid needs a value"))?;
                explicit_pid = Some(val.parse().context("parse --pid")?);
                args.drain(i..=i + 1);
            }
            "--json" => {
                let val = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| anyhow!("--json needs a value"))?;
                cmd_args_json = Some(val);
                args.drain(i..=i + 1);
            }
            _ => i += 1,
        }
    }

    let cmd_id = args
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("no command specified; try `rimectl app.quit` or `--help`"))?;

    let pid = match explicit_pid {
        Some(p) => p,
        None => discover_latest_pid()
            .await?
            .ok_or_else(|| anyhow!("no running rimeterm found; pass `--pid <n>`"))?,
    };

    let request_args = match cmd_args_json.as_deref() {
        Some(s) => serde_json::from_str(s).context("--json is not valid JSON")?,
        None => serde_json::Value::Null,
    };

    let req = Request {
        cmd: cmd_id,
        args: request_args,
    };
    let resp = send_once(pid, &req).await?;

    let out = serde_json::to_string_pretty(&resp).context("pretty-print response")?;
    println!("{out}");
    if !resp.ok {
        bail!("server returned error");
    }
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

fn print_help() {
    println!(
        "\
rimectl — talk to the running rimeterm

USAGE:
    rimectl <command-id> [--pid <n>] [--json '<args>']
    rimectl --list-endpoints
    rimectl --help

EXAMPLES:
    rimectl app.palette.open
    rimectl workspace.tab.next
    rimectl workspace.layout.reset

DISCOVERY:
    Without --pid, rimectl connects to the most recently-started rimeterm
    on this machine (via lockfile mtime under the local runtime dir).
"
    );
}
