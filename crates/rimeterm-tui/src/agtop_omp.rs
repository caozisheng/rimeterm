//! oh-my-pi (`omp`) session enrichment for [`AgtopPane`].
//!
//! oh-my-pi (also known as Pi coding agent, `omp` binary) writes
//! JSONL session transcripts to `~/.omp/agent/sessions/<encoded-cwd>/`
//! — completely different taxonomy from Anthropic Claude Code's
//! `~/.claude/projects/`. Key differences we care about:
//!
//! | Aspect          | Claude Code                          | oh-my-pi                               |
//! |-----------------|--------------------------------------|----------------------------------------|
//! | Session root    | `~/.claude/projects/`                | `~/.omp/agent/sessions/`               |
//! | cwd encoding    | full path, non-alnum → `-`           | home-relative or `--<drive>--…--`      |
//! | Tool-use type   | `"type":"tool_use"`, args in `input` | `"type":"toolCall"`, args in `arguments` |
//! | Usage bucket    | `input_tokens` / `cache_read_input_tokens` | `input` / `cacheRead` / `cacheWrite`   |
//! | Cost            | derived from a price table           | pre-computed by provider (`usage.cost.total`) |
//! | Tool result     | content item `"type":"tool_result"`  | separate message with `role: "toolResult"` |
//!
//! We reuse [`SessionSummary`] and [`SessionState`] from
//! [`crate::agtop_session`] so the pane sees a uniform shape
//! regardless of which vendor produced the row. The worker prefers
//! `cost_provider` over the pricing-table estimate when set — that's
//! how omp rows get authoritative per-turn $ figures without a
//! LiteLLM-scale price catalog.
//!
//! [`AgtopPane`]: crate::agtop_pane::AgtopPane

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agtop_session::{
    ACTIVE_WINDOW_MS, BUSY_WINDOW_MS, LiveAgentRef, RECENT_WINDOW_MS, SessionState, SessionSummary,
    TAIL_BYTES, read_tail, sanitize,
};

const RECENT_ACTIVITY_CAP: usize = 12;
const TOOL_COUNTS_CAP: usize = 8;
const TOOL_HINT_CHARS: usize = 120;

/// Labels the enricher classifies as oh-my-pi sessions. We unified
/// the matcher table to always emit `omp` for both the bare binary
/// and the npm-shim shape (see [`crate::agtop_matchers::builtin`]),
/// but keep the alias list here so a future change that resurrects
/// `oh-my-pi` / `pi` as a distinct label doesn't silently break
/// enrichment.
const OMP_LABELS: &[&str] = &["omp", "oh-my-pi", "pi"];

/// One-shot oh-my-pi enricher. Silent on any I/O error — enrichment
/// is best-effort. Returns a `pid → summary` map that the worker
/// folds into its per-agent output alongside the Claude enricher's.
pub fn enrich_omp(live: &[LiveAgentRef<'_>], now_ms: u64) -> HashMap<u32, SessionSummary> {
    let Some(sessions_root) = sessions_root() else {
        return HashMap::new();
    };
    if !sessions_root.is_dir() {
        return HashMap::new();
    }
    let Some(home) = home_dir() else {
        return HashMap::new();
    };

    // encoded-cwd → [(pid, uptime)]. Multiple live omp pids can share
    // a cwd; sort by uptime ascending so the freshest pid pairs with
    // the freshest JSONL below. omp v0.74 renamed the encoding and
    // both shapes coexist on disk, so we register EVERY variant per
    // pid — the dir-walk loop will pick up whichever variant the
    // running binary actually wrote.
    let mut encoded_to_pids: HashMap<String, Vec<(u32, u64)>> = HashMap::new();
    for a in live {
        if !OMP_LABELS.iter().any(|l| a.label == *l) {
            continue;
        }
        for enc in encode_cwd_variants(a.cwd, &home) {
            encoded_to_pids
                .entry(enc)
                .or_default()
                .push((a.pid, a.uptime_sec));
        }
    }
    if encoded_to_pids.is_empty() {
        return HashMap::new();
    }
    for v in encoded_to_pids.values_mut() {
        v.sort_by_key(|(_pid, uptime)| *uptime);
    }

    let mut by_pid: HashMap<u32, SessionSummary> = HashMap::new();
    let mut seen_canonical: HashSet<PathBuf> = HashSet::new();

    let Ok(rd) = fs::read_dir(&sessions_root) else {
        return by_pid;
    };
    for ent in rd.flatten() {
        let proj = ent.path();
        if !proj.is_dir() {
            continue;
        }
        let raw_name = ent.file_name().to_string_lossy().into_owned();
        let Some(pids) = encoded_to_pids.get(&raw_name) else {
            continue;
        };

        // Collect + sort JSONLs newest-first.
        let mut jsonls: Vec<(PathBuf, u64)> = Vec::new();
        let Ok(inner) = fs::read_dir(&proj) else {
            continue;
        };
        for f in inner.flatten() {
            let p = f.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(md) = fs::metadata(&p) else { continue };
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            jsonls.push((p, mtime));
        }
        jsonls.sort_by(|a, b| b.1.cmp(&a.1));

        // Pair (i-th freshest pid) → (i-th freshest JSONL).
        for (i, (path, mtime)) in jsonls.iter().enumerate() {
            let Some((pid, _)) = pids.get(i) else { break };
            let canon = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            if !seen_canonical.insert(canon) {
                continue;
            }

            let tail = read_tail(path, TAIL_BYTES);
            let out = analyse(&parse_jsonl(&tail));
            let age_ms = now_ms.saturating_sub(*mtime);
            let state = classify_state(
                true,
                age_ms,
                &out.stop_reason,
                out.in_flight_tasks > 0,
                out.in_flight_tools > 0,
                out.has_agent_activity,
            );

            let summary = SessionSummary {
                session_id: path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                mtime_ms: *mtime,
                session_started_ms: out.session_started_ms,
                state,
                stop_reason: out.stop_reason,
                current_tool: out.current_tool.map(|s| sanitize(&s)),
                current_activity: out.current_activity.map(|s| sanitize(&s)),
                current_task: out.last_task.map(|s| sanitize(&s)),
                in_flight_subagents: out
                    .in_flight_subagents
                    .into_iter()
                    .map(|s| sanitize(&s))
                    .collect(),
                subagents_count: out.in_flight_tasks,
                recent_activity: out
                    .recent_activity
                    .into_iter()
                    .map(|s| sanitize(&s))
                    .collect(),
                tokens_input: out.tokens_input,
                tokens_output: out.tokens_output,
                tokens_cache_read: out.tokens_cache_read,
                tokens_cache_write: out.tokens_cache_write,
                context_used: out.context_used,
                model: out.model.map(|s| sanitize(&s)),
                tool_counts: {
                    let mut v: Vec<(String, u32)> = out.tool_counts.into_iter().collect();
                    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                    v.truncate(TOOL_COUNTS_CAP);
                    v
                },
                // Provider-computed cost is the authoritative source
                // for omp; store it here so the worker can prefer it
                // over the pricing-table estimate.
                cost_provider: if out.cost_provider > 0.0 {
                    Some(out.cost_provider)
                } else {
                    None
                },
            };

            by_pid.entry(*pid).or_insert(summary);
        }
    }

    by_pid
}

fn sessions_root() -> Option<PathBuf> {
    // Honor `OMP_CODING_AGENT_SESSION_DIR` / `OMP_CODING_AGENT_DIR`
    // env overrides the same way `fast-resume`'s omp adapter does.
    if let Some(p) = env_path("OMP_CODING_AGENT_SESSION_DIR") {
        return Some(p);
    }
    let agent_dir = env_path("OMP_CODING_AGENT_DIR")
        .unwrap_or_else(|| home_dir().unwrap_or_default().join(".omp").join("agent"));
    let settings = agent_dir.join("settings.json");
    if let Ok(bytes) = fs::read(&settings) {
        if let Ok(json) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(dir) = json
                .get("sessionDir")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
            {
                return Some(expand_tilde(dir));
            }
        }
    }
    Some(agent_dir.join("sessions"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| expand_tilde(&v))
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_default();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().unwrap_or_default().join(rest);
    }
    #[cfg(windows)]
    if let Some(rest) = path.strip_prefix("~\\") {
        return home_dir().unwrap_or_default().join(rest);
    }
    PathBuf::from(path)
}

fn home_dir() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("RIMETERM_HOME") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    rimeterm_config::paths::user_home_dir()
}

/// All encoded-cwd variants the current + historical `omp` binaries
/// use for a given live process cwd. We MUST return every variant so
/// the enricher can pair a running pid with either an old- or
/// new-style session directory — omp v0.74 renamed the encoding
/// midstream and both shapes coexist in `~/.omp/agent/sessions/`.
///
/// Two variants emitted per cwd:
///
/// 1. **Dash form (legacy)** — separators become `-`; underscores +
///    dots survive. Home-relative → `-Documents-00_code-proj`;
///    Windows non-home → `--C--Program Files-rimeterm--`; POSIX
///    non-home → `--opt-proj--`. This is what `omp` wrote before
///    the sha-hash migration and what still exists on disk for
///    older sessions.
///
/// 2. **Sha form (current)** — `home-{basename}-{sha256(cwd)}` for
///    home-relative paths, where the hash input has all `\` folded
///    to `/` and any trailing separator stripped. Confirmed against
///    real omp v0.74 sessions on Windows. Only emitted for
///    home-relative paths — no non-home evidence in the wild yet;
///    unmatched variants are cheap (they just miss the dir lookup).
///
/// The empty vec means "we can't encode this cwd" — either empty
/// input or (defensively) an all-separator path that trims to
/// nothing.
pub(crate) fn encode_cwd_variants(cwd: &str, home: &Path) -> Vec<String> {
    if cwd.is_empty() {
        return Vec::new();
    }
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    let src = if trimmed.is_empty() { cwd } else { trimmed };
    let home_str = home.to_string_lossy();
    let home_trimmed = home_str.trim_end_matches(['/', '\\']);

    // Home-relative — case-insensitive on Windows because sysinfo can
    // canonicalise to a different case than the user's profile dir.
    let matches_home = if cfg!(windows) {
        src.len() >= home_trimmed.len()
            && src[..home_trimmed.len()].eq_ignore_ascii_case(home_trimmed)
    } else {
        src.starts_with(home_trimmed)
    };

    let mut variants: Vec<String> = Vec::with_capacity(2);

    if matches_home && !home_trimmed.is_empty() {
        // Dash form — home-relative.
        let rel = &src[home_trimmed.len()..];
        let dash: String = rel.chars().map(sep_to_dash).collect();
        if !dash.is_empty() {
            variants.push(dash);
        }

        // Sha form — hash the full cwd with `\` normalised to `/`
        // (omp v0.74 hashes the forward-slash representation). We
        // MUST derive the basename from this same forward-slash form:
        // `std::path::Path::file_name` uses the host OS separator,
        // so on POSIX a Windows-style `C:\…\rimeterm` would look
        // like a single component and the basename would round-trip
        // as the whole path. Trailing separators were already
        // stripped by the `trim_end_matches` at the top of the fn.
        let for_hash: String = src
            .chars()
            .map(|c| if c == '\\' { '/' } else { c })
            .collect();
        let basename: String = for_hash.rsplit('/').next().unwrap_or("").to_string();
        if !basename.is_empty() {
            use sha2::{Digest, Sha256};
            let hex = format!("{:x}", Sha256::digest(for_hash.as_bytes()));
            variants.push(format!("home-{basename}-{hex}"));
        }
        return variants;
    }

    // Outside home — Windows drive-letter path (dash form only,
    // no observed sha-form for non-home paths).
    if src.len() >= 2
        && src.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && src.chars().nth(1) == Some(':')
    {
        let drive = src.chars().next().unwrap();
        let body: String = src[2..].chars().map(sep_to_dash).collect();
        let body = body.trim_start_matches('-');
        variants.push(format!("--{drive}--{body}--"));
        return variants;
    }
    // POSIX outside home — dash form only.
    let body: String = src.chars().map(sep_to_dash).collect();
    let body = body.trim_start_matches('-');
    variants.push(format!("--{body}--"));
    variants
}

fn sep_to_dash(c: char) -> char {
    if c == '/' || c == '\\' { '-' } else { c }
}

// ---------------------------------------------------------------------------
// Analyser — turn a JSONL tail into per-session facts.
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
struct AnalysisOut {
    stop_reason: Option<String>,
    last_task: Option<String>,
    current_tool: Option<String>,
    current_activity: Option<String>,
    current_tool_id: Option<String>,
    in_flight_tasks: u32,
    in_flight_subagents: Vec<String>,
    in_flight_tools: u32,
    recent_activity: Vec<String>,
    tokens_input: u64,
    tokens_output: u64,
    tokens_cache_read: u64,
    tokens_cache_write: u64,
    context_used: u64,
    session_started_ms: u64,
    tool_counts: HashMap<String, u32>,
    model: Option<String>,
    /// Sum of `usage.cost.total` across every assistant record —
    /// the provider's authoritative bill. Zero when the transcript
    /// hasn't produced any assistant turns yet.
    cost_provider: f64,
    has_agent_activity: bool,
}

fn analyse(records: &[Value]) -> AnalysisOut {
    let mut out = AnalysisOut::default();
    let mut tool_call_ids: Vec<String> = Vec::new();
    let mut task_call_ids: Vec<String> = Vec::new();
    let mut task_descr: HashMap<String, String> = HashMap::new();
    let mut completed: HashMap<String, ()> = HashMap::new();

    for r in records {
        if out.session_started_ms == 0 {
            if let Some(ts) = r.get("timestamp").and_then(|v| v.as_str()) {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
                    out.session_started_ms = dt.timestamp_millis().max(0) as u64;
                }
            }
        }
        if r.get("customType").and_then(|v| v.as_str()) == Some("tool_execution_start") {
            if let Some(data) = r.get("data") {
                out.current_tool = data
                    .get("toolName")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                out.current_activity = data
                    .get("intent")
                    .and_then(|v| v.as_str())
                    .map(normalize_activity)
                    .filter(|s| !s.is_empty());
                out.current_tool_id = data
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
        }

        let msg = r.get("message");
        let role = msg
            .and_then(|m| m.get("role"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if role == "assistant" {
            out.has_agent_activity = true;
            if let Some(m) = msg.and_then(|m| m.get("model")).and_then(|v| v.as_str()) {
                out.model = Some(m.to_string());
            }
            if let Some(sr) = msg
                .and_then(|m| m.get("stopReason"))
                .and_then(|v| v.as_str())
            {
                out.stop_reason = Some(sr.to_string());
            }
            if let Some(usage) = msg.and_then(|m| m.get("usage")) {
                // Field names differ from Anthropic-native shape:
                // `input` / `output` / `cacheRead` / `cacheWrite`.
                let it = usage.get("input").and_then(|v| v.as_u64()).unwrap_or(0);
                let ot = usage.get("output").and_then(|v| v.as_u64()).unwrap_or(0);
                let cr = usage.get("cacheRead").and_then(|v| v.as_u64()).unwrap_or(0);
                let cw = usage
                    .get("cacheWrite")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                // Roll cache buckets into `tokens_input` so the shared
                // pane semantics ("tokens_input = raw + cache") stay
                // consistent with the Claude enricher.
                out.tokens_input = out
                    .tokens_input
                    .saturating_add(it.saturating_add(cr).saturating_add(cw));
                out.tokens_output = out.tokens_output.saturating_add(ot);
                out.tokens_cache_read = out.tokens_cache_read.saturating_add(cr);
                out.tokens_cache_write = out.tokens_cache_write.saturating_add(cw);
                out.context_used = it.saturating_add(cr).saturating_add(cw);

                if let Some(cost_total) = usage
                    .get("cost")
                    .and_then(|c| c.get("total"))
                    .and_then(|v| v.as_f64())
                {
                    out.cost_provider += cost_total.max(0.0);
                }
            }

            if let Some(arr) = msg
                .and_then(|m| m.get("content"))
                .and_then(|v| v.as_array())
            {
                for c in arr {
                    let kind = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match kind {
                        "toolCall" => handle_tool_call(
                            c,
                            &mut out,
                            &mut tool_call_ids,
                            &mut task_call_ids,
                            &mut task_descr,
                        ),
                        "text" => {
                            if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                                let trimmed: String =
                                    t.split_whitespace().collect::<Vec<_>>().join(" ");
                                if !trimmed.is_empty() {
                                    let snippet: String =
                                        trimmed.chars().take(TOOL_HINT_CHARS).collect();
                                    out.last_task = Some(snippet.clone());
                                    push_recent(&mut out.recent_activity, format!("› {snippet}"));
                                }
                            }
                        }
                        // Skip "thinking" — chain-of-thought bloats
                        // the preview without adding actionable signal.
                        _ => {}
                    }
                }
            }
        }

        // Tool result — oh-my-pi uses a separate message with
        // `role: "toolResult"`. The tool-call id lives on the result
        // content items as `toolCallId` (camelCase).
        if role == "toolResult" {
            let mut completed_ids = Vec::new();
            if let Some(arr) = msg
                .and_then(|m| m.get("content"))
                .and_then(|v| v.as_array())
            {
                for c in arr {
                    if let Some(id) = c
                        .get("toolCallId")
                        .or_else(|| c.get("toolUseId"))
                        .and_then(|v| v.as_str())
                    {
                        completed_ids.push(id.to_string());
                    }
                }
            }
            if let Some(id) = msg
                .and_then(|m| m.get("toolCallId"))
                .or_else(|| r.get("parentToolCallId"))
                .and_then(|v| v.as_str())
            {
                completed_ids.push(id.to_string());
            }
            for id in &completed_ids {
                completed.insert(id.clone(), ());
            }
            if out
                .current_tool_id
                .as_ref()
                .is_some_and(|current| completed_ids.iter().any(|id| id == current))
            {
                out.current_tool = None;
                out.current_activity = None;
                out.current_tool_id = None;
            }

            let preview = msg
                .and_then(|m| m.get("content"))
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .find_map(|c| c.get("text").and_then(|t| t.as_str()))
                })
                .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            let hint: String = preview.chars().take(TOOL_HINT_CHARS).collect();
            let line = if hint.is_empty() {
                "← (ok)".to_string()
            } else {
                format!("← {hint}")
            };
            push_recent(&mut out.recent_activity, line);
        }
    }

    if out.recent_activity.len() > RECENT_ACTIVITY_CAP {
        let drop = out.recent_activity.len() - RECENT_ACTIVITY_CAP;
        out.recent_activity.drain(0..drop);
    }
    out.in_flight_tasks = task_call_ids
        .iter()
        .filter(|id| !completed.contains_key(*id))
        .count() as u32;
    out.in_flight_subagents = task_call_ids
        .iter()
        .filter(|id| !completed.contains_key(*id))
        .filter_map(|id| task_descr.get(id).cloned())
        .collect();
    out.in_flight_tools = tool_call_ids
        .iter()
        .filter(|id| !completed.contains_key(*id))
        .count() as u32;
    out
}

fn handle_tool_call(
    c: &Value,
    out: &mut AnalysisOut,
    tool_call_ids: &mut Vec<String>,
    task_call_ids: &mut Vec<String>,
    task_descr: &mut HashMap<String, String>,
) {
    let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name.is_empty() {
        return;
    }
    out.current_tool = Some(name.to_string());
    out.current_tool_id = c.get("id").and_then(|v| v.as_str()).map(str::to_string);
    out.current_activity = c
        .get("intent")
        .and_then(|v| v.as_str())
        .or_else(|| {
            c.get("arguments").and_then(|a| {
                a.get("i")
                    .or_else(|| a.get("intent"))
                    .and_then(|v| v.as_str())
            })
        })
        .map(normalize_activity)
        .filter(|s| !s.is_empty());
    *out.tool_counts.entry(name.to_string()).or_insert(0) += 1;

    // Recent-activity preview — pull a short arg hint from `arguments`
    // (oh-my-pi's equivalent of Claude's `input`).
    let arg_hint = c
        .get("arguments")
        .and_then(|a| {
            a.get("command")
                .and_then(|v| v.as_str())
                .or_else(|| a.get("path").and_then(|v| v.as_str()))
                .or_else(|| a.get("file_path").and_then(|v| v.as_str()))
                .or_else(|| a.get("subject").and_then(|v| v.as_str()))
                .or_else(|| a.get("description").and_then(|v| v.as_str()))
                .or_else(|| a.get("intent").and_then(|v| v.as_str()))
                .or_else(|| a.get("pattern").and_then(|v| v.as_str()))
                .or_else(|| a.get("i").and_then(|v| v.as_str()))
        })
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let intent = c.get("intent").and_then(|v| v.as_str()).unwrap_or_default();
    let hint = if arg_hint.is_empty() {
        intent
    } else {
        &arg_hint
    };
    let hint_clip: String = hint.chars().take(TOOL_HINT_CHARS).collect();
    let line = if hint_clip.is_empty() {
        format!("→ {name}")
    } else {
        format!("→ {name}: {hint_clip}")
    };
    push_recent(&mut out.recent_activity, line);

    if let Some(id) = c.get("id").and_then(|v| v.as_str()) {
        tool_call_ids.push(id.to_string());
    }

    // Task-style subagent tools. oh-my-pi commonly uses `task`,
    // `Task`, `Agent`, `dispatch_agent` names.
    let lname = name.to_ascii_lowercase();
    if lname == "task" || lname == "agent" || lname.contains("dispatch") {
        let id_str = c.get("id").and_then(|v| v.as_str()).map(String::from);
        if let Some(id) = &id_str {
            task_call_ids.push(id.clone());
        }
        let subj = c
            .get("arguments")
            .and_then(|a| {
                a.get("subject")
                    .or_else(|| a.get("description"))
                    .or_else(|| a.get("prompt"))
                    .or_else(|| a.get("i"))
                    .and_then(|v| v.as_str())
            })
            .map(|s| s.to_string());
        let subagent_type = c
            .get("arguments")
            .and_then(|a| a.get("subagent_type").or_else(|| a.get("agent")))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "agent".into());
        if let Some(subject) = &subj {
            out.last_task = Some(subject.clone());
        }
        if let Some(id) = id_str {
            let descr = match subj {
                Some(s) => format!("{subagent_type}: {s}"),
                None => subagent_type,
            };
            task_descr.insert(id, descr);
        }
    } else if let Some(subj) = c
        .get("arguments")
        .and_then(|a| a.get("subject"))
        .and_then(|v| v.as_str())
    {
        out.last_task = Some(subj.to_string());
    }
}

fn normalize_activity(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_recent(buf: &mut Vec<String>, line: String) {
    if buf.last().map(|s| s == &line).unwrap_or(false) {
        return;
    }
    buf.push(line);
}

fn parse_jsonl(text: &str) -> Vec<Value> {
    text.split('\n')
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

fn classify_state(
    is_live: bool,
    age_ms: u64,
    stop_reason: &Option<String>,
    has_in_flight_task: bool,
    has_in_flight_tool: bool,
    has_agent_activity: bool,
) -> SessionState {
    let completed = matches!(
        stop_reason.as_deref(),
        Some("endTurn") | Some("stopSequence") | Some("end_turn") | Some("stop_sequence")
    );
    if completed && !has_in_flight_task && !has_in_flight_tool {
        return SessionState::Completed;
    }
    if is_live && has_in_flight_task {
        return SessionState::Spawning;
    }
    if is_live && ((has_agent_activity && age_ms < BUSY_WINDOW_MS) || has_in_flight_tool) {
        return SessionState::Busy;
    }
    if is_live && has_agent_activity && age_ms < ACTIVE_WINDOW_MS {
        return SessionState::Active;
    }
    if is_live {
        return SessionState::Idle;
    }
    if completed {
        return SessionState::Completed;
    }
    if age_ms < RECENT_WINDOW_MS {
        return SessionState::Waiting;
    }
    SessionState::Stale
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Helper: assert `expected` is one of the encoded variants for
    /// `cwd`. Every existing dash-form test is stated as "the dash
    /// variant must be produced" so we don't lock the SHA form into
    /// asserting a specific hash from every fixture.
    fn assert_variant_contains(cwd: &str, home: &Path, expected: &str) {
        let vs = encode_cwd_variants(cwd, home);
        assert!(
            vs.iter().any(|v| v == expected),
            "expected variant {expected:?} in {vs:?} for cwd {cwd:?}"
        );
    }

    #[test]
    fn encode_cwd_windows_home_relative_dash_form() {
        let home = PathBuf::from(r"C:\Users\zisheng");
        assert_variant_contains(
            r"C:\Users\zisheng\Documents\cao\00_code\github\rimeterm",
            &home,
            "-Documents-cao-00_code-github-rimeterm",
        );
        // Trailing separator stripped.
        assert_variant_contains(
            r"C:\Users\zisheng\Documents\cao\00_code\github\rimeterm\",
            &home,
            "-Documents-cao-00_code-github-rimeterm",
        );
    }

    #[cfg(windows)]
    #[test]
    fn encode_cwd_windows_case_insensitive_home() {
        let home = PathBuf::from(r"C:\Users\zisheng");
        assert_variant_contains(r"c:\users\ZISHENG\Documents\proj", &home, "-Documents-proj");
    }

    #[test]
    fn encode_cwd_windows_outside_home_dash_only() {
        let home = PathBuf::from(r"C:\Users\zisheng");
        let a = encode_cwd_variants(r"C:\Program Files\rimeterm", &home);
        assert_eq!(a, vec!["--C--Program Files-rimeterm--"]);
        let b = encode_cwd_variants(r"C:\tmp", &home);
        assert_eq!(b, vec!["--C--tmp--"]);
    }

    #[test]
    fn encode_cwd_posix_home_relative_dash_form() {
        let home = PathBuf::from("/home/u");
        assert_variant_contains("/home/u/proj", &home, "-proj");
        assert_variant_contains("/home/u/00_code/foo", &home, "-00_code-foo");
    }

    #[test]
    fn encode_cwd_posix_outside_home_dash_only() {
        let home = PathBuf::from("/home/u");
        let v = encode_cwd_variants("/opt/proj", &home);
        assert_eq!(v, vec!["--opt-proj--"]);
    }

    #[test]
    fn encode_cwd_preserves_underscores_and_dots() {
        // Divergence from Claude Code — omp keeps `_` and `.` intact.
        // That's WHY we need a separate encoder.
        let home = PathBuf::from(r"C:\Users\z");
        assert_variant_contains(r"C:\Users\z\_download", &home, "-_download");
        assert_variant_contains(r"C:\Users\z\foo.bar", &home, "-foo.bar");
    }

    #[test]
    fn encode_cwd_empty_returns_empty() {
        assert!(encode_cwd_variants("", &PathBuf::from("/home/u")).is_empty());
    }

    /// Regression: omp v0.74 writes home-relative sessions to a
    /// `home-{basename}-{sha256(cwd_forward_slash)}` directory. Before
    /// we recognised this shape the pane silently rendered TOKENS as
    /// `-` because the enricher couldn't pair any live pid with the
    /// on-disk JSONL.
    #[test]
    fn encode_cwd_windows_home_relative_sha_form() {
        let home = PathBuf::from(r"C:\Users\zisheng");
        let cwd = r"C:\Users\zisheng\Documents\cao\00_code\github\rimeterm";
        // Hash computed against the real omp session dir observed in
        // the wild — locks the format so a future refactor can't
        // silently break enrichment.
        assert_variant_contains(
            cwd,
            &home,
            "home-rimeterm-b76f888c81ade2f60d31c07f9eda21611d7164c496173f9cd647b8eb1f7b707c",
        );
    }

    #[test]
    fn encode_cwd_posix_home_relative_sha_form() {
        let home = PathBuf::from("/home/u");
        let vs = encode_cwd_variants("/home/u/proj", &home);
        // Sha variant is `home-{basename}-<64-hex>`.
        let sha = vs
            .iter()
            .find(|v| v.starts_with("home-proj-"))
            .unwrap_or_else(|| panic!("sha variant missing from {vs:?}"));
        assert_eq!(sha.len(), "home-proj-".len() + 64);
        assert!(
            sha["home-proj-".len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
            "sha suffix must be hex: {sha}"
        );
    }

    #[test]
    fn analyse_extracts_omp_usage_and_cost() {
        let record = serde_json::json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-7",
                "content": [{"type": "text", "text": "hello"}],
                "usage": {
                    "input": 100,
                    "output": 50,
                    "cacheRead": 5000,
                    "cacheWrite": 200,
                    "totalTokens": 5350,
                    "cost": {"total": 0.087}
                },
                "stopReason": "endTurn"
            }
        });
        let out = analyse(&[record]);
        assert_eq!(out.model.as_deref(), Some("claude-opus-4-7"));
        // input rollup includes cache buckets: 100 + 5000 + 200.
        assert_eq!(out.tokens_input, 5300);
        assert_eq!(out.tokens_output, 50);
        assert_eq!(out.tokens_cache_read, 5000);
        assert_eq!(out.tokens_cache_write, 200);
        assert!((out.cost_provider - 0.087).abs() < 1e-9);
        assert_eq!(out.stop_reason.as_deref(), Some("endTurn"));
    }

    #[test]
    fn analyse_tracks_in_flight_tool_calls() {
        let call = serde_json::json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-7",
                "content": [{
                    "type": "toolCall",
                    "id": "toolu_1",
                    "name": "read",
                    "arguments": {"path": "/tmp/foo"}
                }]
            }
        });
        let out = analyse(&[call]);
        assert_eq!(out.in_flight_tools, 1);
        assert_eq!(out.current_tool.as_deref(), Some("read"));
    }

    #[test]
    fn analyse_exposes_current_tool_intent_as_activity() {
        let call = serde_json::json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "id": "toolu_tidy",
                    "name": "bash",
                    "intent": "Checking Tidy availability",
                    "arguments": {"command": "tidy -version"}
                }]
            }
        });

        let out = analyse(&[call]);

        assert_eq!(
            out.current_activity.as_deref(),
            Some("Checking Tidy availability")
        );
    }

    #[test]
    fn analyse_completes_tool_on_result() {
        let call = serde_json::json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "id": "toolu_9",
                    "name": "read",
                    "arguments": {"path": "/tmp/foo"}
                }]
            }
        });
        let result = serde_json::json!({
            "type": "message",
            "message": {
                "role": "toolResult",
                "toolCallId": "toolu_9",
                "content": [{"type": "text", "text": "ok"}]
            }
        });
        let out = analyse(&[call, result]);
        assert_eq!(out.in_flight_tools, 0);
        assert!(out.current_tool.is_none());
        assert!(out.current_activity.is_none());
    }

    #[test]
    fn omp_tool_result_clears_matching_started_activity() {
        let start = serde_json::json!({
            "type": "custom",
            "customType": "tool_execution_start",
            "data": {
                "toolCallId": "call_tidy",
                "toolName": "bash",
                "intent": "Checking Tidy availability"
            }
        });
        let result = serde_json::json!({
            "type": "message",
            "message": {
                "role": "toolResult",
                "toolCallId": "call_tidy",
                "toolName": "bash",
                "content": [{"type": "text", "text": "not found"}]
            }
        });

        let out = analyse(&[start, result]);

        assert!(out.current_tool.is_none());
        assert!(out.current_activity.is_none());
    }

    #[test]
    fn analyse_extracts_intent_from_omp_tool_execution_start() {
        let start = serde_json::json!({
            "type": "custom",
            "customType": "tool_execution_start",
            "data": {
                "toolCallId": "call_tidy",
                "toolName": "bash",
                "intent": "Checking Tidy availability"
            }
        });
        let call = serde_json::json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "id": "call_tidy",
                    "name": "bash",
                    "arguments": {
                        "i": "Checking Tidy availability",
                        "command": "tidy -version"
                    }
                }]
            }
        });

        let out = analyse(&[start, call]);

        assert_eq!(out.in_flight_tools, 1);
        assert_eq!(out.current_tool.as_deref(), Some("bash"));
        assert_eq!(
            out.current_activity.as_deref(),
            Some("Checking Tidy availability")
        );
    }

    #[test]
    fn classify_state_maps_endturn_to_completed() {
        assert_eq!(
            classify_state(false, 60_000, &Some("endTurn".into()), false, false, true),
            SessionState::Completed
        );
        assert_eq!(
            classify_state(
                false,
                60_000,
                &Some("stopSequence".into()),
                false,
                false,
                true
            ),
            SessionState::Completed
        );
    }

    /// End-to-end enricher smoke: stand up an oh-my-pi session tree
    /// under a temp home, feed a live `omp` pid whose cwd would
    /// encode to the fixture's project dir, and assert cost + tokens
    /// come back through `enrich_omp`.
    ///
    /// This is the test that catches "encoder rule diverges from what
    /// omp actually writes to disk" — exactly the bug that made
    /// tokens/cost show as `—` for real omp sessions.
    #[test]
    fn enrich_omp_end_to_end_populates_tokens_model_and_cost() {
        let _guard = rimeterm_config::test_util::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let prev_home = std::env::var("RIMETERM_HOME").ok();
        // SAFETY: serialized by ENV_LOCK and restored below.
        unsafe {
            std::env::set_var("RIMETERM_HOME", dir.path());
        }

        // Home-relative cwd — the common case (bun launched from the
        // shell in a user-owned project).
        let cwd_str = if cfg!(windows) {
            let tp = dir.path().to_string_lossy().into_owned();
            format!(r"{tp}\Documents\00_code\proj")
        } else {
            let tp = dir.path().to_string_lossy().into_owned();
            format!("{tp}/Documents/00_code/proj")
        };
        // Fixture uses the dash variant so the pairing exercises the
        // legacy-encoding branch of `encode_cwd_variants`; the sha
        // variant is covered by dedicated encoder-level tests above.
        let encoded = encode_cwd_variants(&cwd_str, dir.path())
            .into_iter()
            .next()
            .expect("home-relative cwd must yield at least one variant");
        let proj_dir = dir
            .path()
            .join(".omp")
            .join("agent")
            .join("sessions")
            .join(&encoded);
        std::fs::create_dir_all(&proj_dir).unwrap();

        let session_id = "2026-08-03T09-00-00-000Z_abc";
        let jsonl_path = proj_dir.join(format!("{session_id}.jsonl"));
        let record = serde_json::json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-7",
                "content": [{"type": "text", "text": "hi"}],
                "usage": {
                    "input": 200,
                    "output": 80,
                    "cacheRead": 1000,
                    "cacheWrite": 50,
                    "cost": {"total": 0.42}
                }
            }
        });
        std::fs::write(
            &jsonl_path,
            format!("{}\n", serde_json::to_string(&record).unwrap()),
        )
        .unwrap();

        let live = LiveAgentRef {
            pid: 5757,
            cwd: cwd_str.as_str(),
            label: "omp",
            uptime_sec: 60,
        };
        let out = enrich_omp(&[live], 0);

        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("RIMETERM_HOME", v),
                None => std::env::remove_var("RIMETERM_HOME"),
            }
        }

        let summary = out
            .get(&5757)
            .expect("enricher should have paired the omp pid with the fixture session");
        assert_eq!(summary.model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(summary.tokens_input, 1250); // 200 + 1000 + 50
        assert_eq!(summary.tokens_output, 80);
        assert_eq!(summary.tokens_cache_read, 1000);
        assert_eq!(summary.tokens_cache_write, 50);
        assert_eq!(summary.cost_provider, Some(0.42));
    }

    /// Regression: an omp v0.74 install writes home-relative sessions
    /// to the `home-<basename>-<sha256>` directory shape. This test
    /// stands up a fixture with ONLY the sha-form directory (no dash
    /// dir) and confirms the enricher still pairs the live pid with
    /// the JSONL — the very failure mode that made TOKENS render as
    /// `-` for real omp sessions.
    #[test]
    fn enrich_omp_end_to_end_paired_via_sha_directory() {
        let _guard = rimeterm_config::test_util::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let prev_home = std::env::var("RIMETERM_HOME").ok();
        // SAFETY: serialized by ENV_LOCK and restored below.
        unsafe {
            std::env::set_var("RIMETERM_HOME", dir.path());
        }

        let cwd_str = if cfg!(windows) {
            let tp = dir.path().to_string_lossy().into_owned();
            format!(r"{tp}\Documents\proj")
        } else {
            let tp = dir.path().to_string_lossy().into_owned();
            format!("{tp}/Documents/proj")
        };
        // Grab the SHA variant specifically — skip the dash variant so
        // we prove the enricher can find the session using only the
        // new-style directory name.
        let sha_variant = encode_cwd_variants(&cwd_str, dir.path())
            .into_iter()
            .find(|v| v.starts_with("home-proj-"))
            .expect("sha variant must be emitted for home-relative cwd");
        let proj_dir = dir
            .path()
            .join(".omp")
            .join("agent")
            .join("sessions")
            .join(&sha_variant);
        std::fs::create_dir_all(&proj_dir).unwrap();

        let jsonl_path = proj_dir.join("2026-08-04T04-00-00-000Z_xyz.jsonl");
        let record = serde_json::json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-7",
                "content": [{"type": "text", "text": "hi"}],
                "usage": {
                    "input": 300,
                    "output": 40,
                    "cacheRead": 2000,
                    "cacheWrite": 10,
                    "cost": {"total": 0.11}
                }
            }
        });
        std::fs::write(
            &jsonl_path,
            format!("{}\n", serde_json::to_string(&record).unwrap()),
        )
        .unwrap();

        let live = LiveAgentRef {
            pid: 7373,
            cwd: cwd_str.as_str(),
            label: "omp",
            uptime_sec: 12,
        };
        let out = enrich_omp(&[live], 0);

        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("RIMETERM_HOME", v),
                None => std::env::remove_var("RIMETERM_HOME"),
            }
        }

        let summary = out.get(&7373).expect(
            "enricher must pair the omp pid via the sha-encoded directory (new omp v0.74 layout)",
        );
        assert_eq!(summary.model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(summary.tokens_input, 2310); // 300 + 2000 + 10
        assert_eq!(summary.tokens_output, 40);
        assert_eq!(summary.tokens_cache_read, 2000);
        assert_eq!(summary.tokens_cache_write, 10);
        assert_eq!(summary.cost_provider, Some(0.11));
    }

    #[test]
    fn classify_state_keeps_empty_live_session_idle() {
        assert_eq!(
            classify_state(true, 1_000, &None, false, false, false),
            SessionState::Idle
        );
    }
    #[test]
    fn classify_state_marks_live_end_turn_as_completed() {
        assert_eq!(
            classify_state(true, 60_000, &Some("endTurn".into()), false, false, true),
            SessionState::Completed
        );
    }
}
