//! Filesystem / session-transcript enrichment for the [`AgtopPane`].
//!
//! Three responsibilities:
//!
//! 1. [`enabled_plugins`] — parse `~/.claude/settings.json` +
//!    `~/.claude/plugins/installed_plugins.json` to surface the
//!    plugins that are BOTH installed AND enabled. User-global, so
//!    it's the same list for every Claude session on the host.
//! 2. [`skills_for_cwd`] — walk `<cwd>/.claude/skills/*` and
//!    `~/.claude/skills/*`, returning the subdirs that hold a
//!    `SKILL.md` file. Project-local wins on collision.
//! 3. [`ClaudeEnricher`] — read the tail of every
//!    `~/.claude/projects/<encoded-cwd>/<session>.jsonl` and produce
//!    a per-live-pid [`SessionSummary`] with model, tokens, subagents,
//!    tool counts, context-used, and a rolling recent-activity tail.
//!
//! Ported from upstream `agtop`'s `src/{claude,skills,plugins}.rs`
//! (v2.4.24, MIT). Deliberately narrowed to Claude — Codex / Aider /
//! Gemini / Goose enrichers can land later without touching the pane
//! or model types (each vendor becomes another `Enricher` returning
//! the same `HashMap<u32, SessionSummary>`).
//!
//! [`AgtopPane`]: crate::agtop_pane::AgtopPane

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

// ---------------------------------------------------------------------------
// Skills — Claude Code agent skill discovery.
// ---------------------------------------------------------------------------

const SKILL_FILE: &str = "SKILL.md";
/// Cap on skill directories scanned per root. Real installs sit
/// comfortably under 100; the cap keeps a hostile `~/.claude/skills/`
/// with a million junk subdirs from turning per-tick.
const SKILL_SCAN_CAP: usize = 512;

/// Return the sorted, deduped list of skill names visible to a
/// Claude Code session with `cwd`. Project-local first (they win on
/// name collision), then user-global. Silently returns an empty list
/// on any read error — skills are advisory information.
pub fn skills_for_cwd(cwd: &str) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();

    if !cwd.is_empty() && cwd != "?" {
        let root = Path::new(cwd).join(".claude").join("skills");
        collect_skill_names(&root, &mut out);
    }

    if let Some(home) = home_dir() {
        let root = home.join(".claude").join("skills");
        collect_skill_names(&root, &mut out);
    }

    out.into_iter().collect()
}

fn collect_skill_names(root: &Path, out: &mut BTreeSet<String>) {
    let Ok(rd) = fs::read_dir(root) else {
        return;
    };
    let mut scanned = 0usize;
    for ent in rd.flatten() {
        if scanned >= SKILL_SCAN_CAP {
            break;
        }
        scanned += 1;
        // Skip symlinks — a rogue `~/.claude/skills/root -> /` should
        // not send us walking the whole filesystem in search of
        // `SKILL.md`.
        let ft = match ent.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }
        let dir = ent.path();
        if !dir.join(SKILL_FILE).is_file() {
            continue;
        }
        let name = match ent.file_name().to_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        out.insert(name);
    }
}

// ---------------------------------------------------------------------------
// Plugins — Claude Code plugin discovery.
// ---------------------------------------------------------------------------

/// Return the sorted list of plugin display names (part before `@` in
/// `name@marketplace`) that are BOTH installed and enabled in the
/// user's Claude Code settings. Empty on any read / parse error.
pub fn enabled_plugins() -> Vec<String> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let settings = home.join(".claude").join("settings.json");
    let installed = home
        .join(".claude")
        .join("plugins")
        .join("installed_plugins.json");

    let enabled = fs::read_to_string(&settings)
        .ok()
        .map(|s| parse_enabled(&s))
        .unwrap_or_default();
    if enabled.is_empty() {
        return Vec::new();
    }
    let installed = fs::read_to_string(&installed)
        .ok()
        .map(|s| parse_installed(&s))
        .unwrap_or_default();
    if installed.is_empty() {
        return Vec::new();
    }

    let mut out: BTreeSet<String> = BTreeSet::new();
    for full in enabled.intersection(&installed) {
        // Strip `@<marketplace>` suffix for display.
        let name = full
            .split_once('@')
            .map(|(n, _)| n)
            .unwrap_or(full.as_str());
        if !name.is_empty() {
            out.insert(name.to_string());
        }
    }
    out.into_iter().collect()
}

fn parse_enabled(text: &str) -> BTreeSet<String> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return BTreeSet::new();
    };
    let mut out: BTreeSet<String> = BTreeSet::new();
    if let Some(map) = v.get("enabledPlugins").and_then(|m| m.as_object()) {
        for (k, on) in map {
            if on.as_bool() == Some(true) {
                out.insert(k.clone());
            }
        }
    }
    out
}

fn parse_installed(text: &str) -> BTreeSet<String> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return BTreeSet::new();
    };
    let mut out: BTreeSet<String> = BTreeSet::new();
    if let Some(map) = v.get("plugins").and_then(|m| m.as_object()) {
        for k in map.keys() {
            out.insert(k.clone());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Claude session enrichment.
// ---------------------------------------------------------------------------

/// Windows of recency used by the classifier + reader. Match upstream
/// agtop so a session mid-generation reads as `Busy` for the same
/// mid-turn gap window.
pub const RECENT_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
pub const BUSY_WINDOW_MS: u64 = 30 * 1000;
pub const ACTIVE_WINDOW_MS: u64 = 5 * 60 * 1000;
/// Bytes of transcript tail we parse per session. 256 KiB is upstream's
/// pick — leaves plenty of headroom for a Claude session's last
/// hundred-or-so turns while keeping the per-tick I/O bound tight.
pub const TAIL_BYTES: u64 = 256 * 1024;
/// Absolute I/O cap so a malformed / symlinked file can't panic the
/// cast to `i64` on 32-bit targets.
const MAX_TAIL: u64 = 64 * 1024 * 1024;
/// Cap on recent-activity buffer entries surfaced in the popup.
const RECENT_ACTIVITY_CAP: usize = 12;
/// Cap on `tool_counts` entries kept per session.
const TOOL_COUNTS_CAP: usize = 8;
/// Substring cap when previewing a tool-call argument (bash command,
/// file path, task subject). Matches upstream's 120-char clip.
const TOOL_HINT_CHARS: usize = 120;

/// Coarse session state derived from live-pid presence + tail activity
/// age + in-flight tool bookkeeping.
///
/// Mirrors upstream `agtop::Status` so the pane's badges line up 1:1
/// with the upstream binary. See [`crate::agtop_model::AgentStatus`]
/// for the pane-visible enum this maps into.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SessionState {
    /// Live pid + fresh JSONL write (≤30 s) or an in-flight tool.
    #[default]
    Busy,
    /// Live pid + at least one in-flight Task/Agent subagent.
    Spawning,
    /// Live pid + JSONL touched in the last 5 min.
    Active,
    /// Live pid, otherwise quiet.
    Idle,
    /// No live pid, but the session transcript was touched in the
    /// last 24 h. Model probably paused waiting on the user.
    Waiting,
    /// No live pid + explicit `stop_reason: end_turn` (or
    /// `stop_sequence`) in the tail.
    Completed,
    /// No live pid + older than 24 h.
    Stale,
}

/// One enriched session, keyed by live pid on the way back to the
/// pane. Field selection deliberately mirrors upstream
/// `agtop::model::Session` (minus the fields we don't display) so
/// future re-syncs are line-for-line.
#[derive(Clone, Debug, Default)]
pub struct SessionSummary {
    pub session_id: String,
    /// Unix-ms mtime of the JSONL file.
    pub mtime_ms: u64,
    /// Unix-ms timestamp of the JSONL's first record (session start,
    /// which diverges from process start when `--resume` is used).
    pub session_started_ms: u64,
    pub state: SessionState,
    pub stop_reason: Option<String>,
    pub current_tool: Option<String>,
    pub current_task: Option<String>,
    pub in_flight_subagents: Vec<String>,
    pub subagents_count: u32,
    pub recent_activity: Vec<String>,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_cache_read: u64,
    pub tokens_cache_write: u64,
    pub context_used: u64,
    pub model: Option<String>,
    pub tool_counts: Vec<(String, u32)>,
    /// Vendor-emitted USD cost for the session — populated by
    /// enrichers where the transcript carries authoritative per-turn
    /// cost (e.g. oh-my-pi's `usage.cost.total` block). `None` means
    /// "no vendor cost available; fall back to the price table".
    /// Kept separate so a vendor with real cost data doesn't get
    /// overwritten by a `cost_with_cache` estimate.
    pub cost_provider: Option<f64>,
}

impl SessionSummary {
    pub fn tokens_total(&self) -> u64 {
        self.tokens_input.saturating_add(self.tokens_output)
    }
}

/// Reader-side view of a live agent process. Kept tiny (`Copy`) so
/// the worker can build the whole slice inline without cloning
/// strings per pid.
#[derive(Clone, Copy, Debug)]
pub struct LiveAgentRef<'a> {
    pub pid: u32,
    pub cwd: &'a str,
    pub label: &'a str,
    /// Process uptime in seconds — used to pair the freshest live
    /// pid with the freshest JSONL when multiple sessions share a
    /// cwd (parallel `claude` invocations from different terminals).
    pub uptime_sec: u64,
}

/// One-shot Claude enricher. Scans `~/.claude/projects/**/*.jsonl`,
/// pairs live pids to session files, and returns a `pid → summary`
/// map. Silent on any I/O error — enrichment is best-effort.
pub fn enrich_claude(live: &[LiveAgentRef<'_>], now_ms: u64) -> HashMap<u32, SessionSummary> {
    let Some(root) = claude_projects_root() else {
        return HashMap::new();
    };
    if !root.is_dir() {
        return HashMap::new();
    }

    // Build the encoded-cwd → [(pid, uptime)] map. A single cwd can
    // host multiple live `claude` pids (parallel sessions in the
    // same project from different terminals), so we keep them all
    // and sort by uptime ascending so the freshest pid gets paired
    // with the freshest JSONL below.
    let mut encoded_to_pids: HashMap<String, Vec<(u32, u64)>> = HashMap::new();
    for a in live {
        if a.label != "claude" && a.label != "claude-code" {
            continue;
        }
        let enc = encode_cwd(a.cwd);
        if !enc.is_empty() {
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

    let Ok(rd) = fs::read_dir(&root) else {
        return by_pid;
    };
    for ent in rd.flatten() {
        let proj = ent.path();
        if !proj.is_dir() {
            continue;
        }
        let raw_name = ent.file_name().to_string_lossy().into_owned();
        let Some(pids) = encoded_to_pids.get(&raw_name) else {
            // No live pid points at this project dir. Skip — we
            // don't render session-only rows in the pane (unlike
            // upstream agtop's TUI, which shows completed / stale
            // sessions too).
            continue;
        };

        // Collect + sort jsonls newest-first.
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

        // Pair (i-th freshest pid) → (i-th freshest jsonl).
        for (i, (path, mtime)) in jsonls.iter().enumerate() {
            let Some((pid, _)) = pids.get(i) else { break };
            // Cross-root dedupe (in case future WSL bridging adds a
            // second view of the same file).
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
                // Claude transcripts don't carry provider-computed
                // cost — the pricing table + cost_with_cache handle
                // it downstream in the worker.
                cost_provider: None,
            };

            by_pid.entry(*pid).or_insert(summary);
        }
    }

    by_pid
}

fn claude_projects_root() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Forward-encode a live-process cwd into the shape Claude Code
/// uses under `~/.claude/projects/`. Claude Code's rule (from
/// observed on-disk encodings): **every non-alphanumeric character
/// becomes `-`**. That includes path separators (`/`, `\`), Windows
/// drive-letter colon, dots, underscores, and any other punctuation.
///
/// Examples:
/// - POSIX  `/home/u/proj`                    → `-home-u-proj`
/// - POSIX  `/home/u/00_code/foo.bar`          → `-home-u-00-code-foo-bar`
/// - Windows `C:\Users\u\proj`                → `C--Users-u-proj`
/// - Windows `C:\Users\u\00_code\.claude`      → `C--Users-u-00-code--claude`
///
/// Encoding is lossy — `/home/u/foo-bar` and `/home/u/foo/bar` both
/// produce `-home-u-foo-bar`, so we always encode-forward from the
/// live cwd rather than decode-reverse from a dir name.
///
/// The upstream `agtop::src/claude.rs::encode_cwd` (v2.4.24) only
/// replaces path separators, but that misses every real Claude
/// project whose cwd contains a dot or underscore. This matches
/// what Claude Code actually writes to disk.
pub(crate) fn encode_cwd(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    // sysinfo hands back trailing separators for many Windows
    // processes; strip them so `C:\workspace\proj\` doesn't produce
    // `C--workspace-proj-` and miss the encoded name Claude wrote.
    // Preserve bare-root paths (`/`, `C:\`) by keeping the original
    // when the trim would empty the string.
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    let src = if trimmed.is_empty() { cwd } else { trimmed };
    let mut out = String::with_capacity(src.len());
    for ch in src.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out
}

pub(crate) fn read_tail(path: &Path, bytes: u64) -> String {
    let Ok(mut f) = File::open(path) else {
        return String::new();
    };
    let Ok(md) = f.metadata() else {
        return String::new();
    };
    let len = md.len();
    if len == 0 {
        return String::new();
    }
    let take = bytes.min(len).min(MAX_TAIL);
    if f.seek(SeekFrom::End(-(take as i64))).is_err() {
        return String::new();
    }
    let mut buf = String::with_capacity(take as usize);
    let _ = f.take(take).read_to_string(&mut buf);
    buf
}

fn parse_jsonl(text: &str) -> Vec<Value> {
    // The tail almost always starts mid-line — the first partial
    // line is dropped by the `is_empty` / parse-failure filter, so
    // we don't have to explicitly slice past the first newline.
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
) -> SessionState {
    if is_live && has_in_flight_task {
        return SessionState::Spawning;
    }
    if is_live && (age_ms < BUSY_WINDOW_MS || has_in_flight_tool) {
        return SessionState::Busy;
    }
    if is_live && age_ms < ACTIVE_WINDOW_MS {
        return SessionState::Active;
    }
    if is_live {
        return SessionState::Idle;
    }
    if matches!(
        stop_reason.as_deref(),
        Some("end_turn") | Some("stop_sequence")
    ) {
        return SessionState::Completed;
    }
    if age_ms < RECENT_WINDOW_MS {
        return SessionState::Waiting;
    }
    SessionState::Stale
}

// ---------------------------------------------------------------------------
// Tail analyser — pull structured facts out of the JSONL tail.
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
struct AnalysisOut {
    stop_reason: Option<String>,
    last_task: Option<String>,
    current_tool: Option<String>,
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
}

fn push_recent(buf: &mut Vec<String>, line: String) {
    // Cheap dedup — skip consecutive duplicates so retries don't
    // spam out the preview window.
    if buf.last().map(|s| s == &line).unwrap_or(false) {
        return;
    }
    buf.push(line);
}

fn analyse(records: &[Value]) -> AnalysisOut {
    let mut out = AnalysisOut::default();
    let mut task_use_ids: Vec<String> = Vec::new();
    let mut tool_use_ids: Vec<String> = Vec::new();
    let mut task_descr: HashMap<String, String> = HashMap::new();
    let mut completed: HashMap<String, ()> = HashMap::new();

    for r in records {
        // First timestamp wins as session start.
        if out.session_started_ms == 0 {
            if let Some(ts) = r.get("timestamp").and_then(|v| v.as_str()) {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
                    out.session_started_ms = dt.timestamp_millis().max(0) as u64;
                }
            }
        }

        // Stop reason (top-level and nested under `message`).
        if let Some(sr) = r.get("stop_reason").and_then(|v| v.as_str()) {
            out.stop_reason = Some(sr.to_string());
        } else if let Some(sr) = r
            .get("message")
            .and_then(|m| m.get("stop_reason"))
            .and_then(|v| v.as_str())
        {
            out.stop_reason = Some(sr.to_string());
        }

        // Token usage — Anthropic attaches a `usage` block to each
        // assistant message. Track the three input buckets separately
        // so the cost calc doesn't over-bill cache hits.
        if let Some(usage) = r.get("message").and_then(|m| m.get("usage")) {
            let it = usage
                .get("input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let ot = usage
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cr = usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cc = usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            out.tokens_input = out
                .tokens_input
                .saturating_add(it.saturating_add(cr).saturating_add(cc));
            out.tokens_output = out.tokens_output.saturating_add(ot);
            out.tokens_cache_read = out.tokens_cache_read.saturating_add(cr);
            out.tokens_cache_write = out.tokens_cache_write.saturating_add(cc);
            // The MOST RECENT usage block is the current context fill
            // (prompt size on the next request). Records iterate
            // oldest → newest so the last write wins.
            out.context_used = it.saturating_add(cr).saturating_add(cc);
        }

        // Model — most recent assistant message wins.
        if let Some(m) = r
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(|v| v.as_str())
        {
            out.model = Some(m.to_string());
        }

        let content_holder = r
            .get("message")
            .and_then(|m| m.get("content"))
            .cloned()
            .or_else(|| r.get("content").cloned());
        let Some(content) = content_holder else {
            continue;
        };
        let Some(arr) = content.as_array() else {
            continue;
        };
        for c in arr {
            let kind = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                "tool_use" => handle_tool_use(
                    c,
                    &mut out,
                    &mut task_use_ids,
                    &mut tool_use_ids,
                    &mut task_descr,
                ),
                "tool_result" => handle_tool_result(c, &mut out, &mut completed),
                "text" => handle_text(r, c, &mut out),
                _ => {}
            }
        }

        // Toolformer-style top-level subject.
        if let Some(subj) = r
            .get("toolUseResult")
            .and_then(|tu| tu.get("subject"))
            .and_then(|v| v.as_str())
        {
            out.last_task = Some(subj.to_string());
        }
    }

    if out.recent_activity.len() > RECENT_ACTIVITY_CAP {
        let drop = out.recent_activity.len() - RECENT_ACTIVITY_CAP;
        out.recent_activity.drain(0..drop);
    }
    out.in_flight_tasks = task_use_ids
        .iter()
        .filter(|id| !completed.contains_key(*id))
        .count() as u32;
    out.in_flight_subagents = task_use_ids
        .iter()
        .filter(|id| !completed.contains_key(*id))
        .filter_map(|id| task_descr.get(id).cloned())
        .collect();
    out.in_flight_tools = tool_use_ids
        .iter()
        .filter(|id| !completed.contains_key(*id))
        .count() as u32;
    out
}

fn handle_tool_use(
    c: &Value,
    out: &mut AnalysisOut,
    task_use_ids: &mut Vec<String>,
    tool_use_ids: &mut Vec<String>,
    task_descr: &mut HashMap<String, String>,
) {
    let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name.is_empty() {
        return;
    }
    out.current_tool = Some(name.to_string());
    *out.tool_counts.entry(name.to_string()).or_insert(0) += 1;

    // Recent-activity preview.
    let arg_hint = c
        .get("input")
        .and_then(|i| {
            i.get("command")
                .and_then(|v| v.as_str())
                .or_else(|| i.get("file_path").and_then(|v| v.as_str()))
                .or_else(|| i.get("subject").and_then(|v| v.as_str()))
                .or_else(|| i.get("description").and_then(|v| v.as_str()))
                .or_else(|| i.get("path").and_then(|v| v.as_str()))
        })
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let hint: String = arg_hint.chars().take(TOOL_HINT_CHARS).collect();
    let line = if hint.is_empty() {
        format!("→ {name}")
    } else {
        format!("→ {name}: {hint}")
    };
    push_recent(&mut out.recent_activity, line);

    // Track every tool_use id so generic in-flight (Bash / Edit /
    // Read / …) is computable.
    if let Some(id) = c.get("id").and_then(|v| v.as_str()) {
        tool_use_ids.push(id.to_string());
    }

    // Task / Agent subagent bookkeeping.
    if name == "Task" || name == "Agent" {
        let id_str = c.get("id").and_then(|v| v.as_str()).map(String::from);
        if let Some(id) = &id_str {
            task_use_ids.push(id.clone());
        }
        let mut subj_opt = None::<String>;
        let mut kind_opt = None::<String>;
        if let Some(input) = c.get("input") {
            if let Some(s) = input
                .get("subject")
                .or_else(|| input.get("description"))
                .and_then(|v| v.as_str())
            {
                out.last_task = Some(s.to_string());
                subj_opt = Some(s.to_string());
            }
            if let Some(k) = input.get("subagent_type").and_then(|v| v.as_str()) {
                kind_opt = Some(k.to_string());
            }
        }
        if let Some(id) = id_str {
            let kind = kind_opt.unwrap_or_else(|| "agent".into());
            let descr = match subj_opt {
                Some(s) => format!("{kind}: {s}"),
                None => kind,
            };
            task_descr.insert(id, descr);
        }
    } else if name == "TodoWrite" {
        if let Some(todos) = c
            .get("input")
            .and_then(|i| i.get("todos"))
            .and_then(|v| v.as_array())
        {
            if let Some(in_prog) = todos
                .iter()
                .find(|t| t.get("status").and_then(|v| v.as_str()) == Some("in_progress"))
            {
                if let Some(t) = in_prog.get("content").and_then(|v| v.as_str()) {
                    out.last_task = Some(t.to_string());
                }
            }
        }
    } else if let Some(subj) = c
        .get("input")
        .and_then(|i| i.get("subject"))
        .and_then(|v| v.as_str())
    {
        out.last_task = Some(subj.to_string());
    }
}

fn handle_tool_result(c: &Value, out: &mut AnalysisOut, completed: &mut HashMap<String, ()>) {
    if let Some(id) = c.get("tool_use_id").and_then(|v| v.as_str()) {
        completed.insert(id.to_string(), ());
    }
    // The tool the previous `current_tool` write pointed at is now
    // done — clear so the pane doesn't render a stale "in Bash" hint.
    out.current_tool = None;

    let preview = c
        .get("content")
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                return Some(s.to_string());
            }
            if let Some(arr) = v.as_array() {
                for x in arr {
                    if let Some(s) = x.get("text").and_then(|t| t.as_str()) {
                        return Some(s.to_string());
                    }
                }
            }
            None
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

fn handle_text(r: &Value, c: &Value, out: &mut AnalysisOut) {
    // Only assistant text — user text bloats the preview with
    // whatever the operator just typed.
    if r.get("type").and_then(|v| v.as_str()) != Some("assistant") {
        return;
    }
    let Some(t) = c.get("text").and_then(|v| v.as_str()) else {
        return;
    };
    let trimmed: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() {
        return;
    }
    let snippet: String = trimmed.chars().take(TOOL_HINT_CHARS).collect();
    out.last_task = Some(snippet.clone());
    push_recent(&mut out.recent_activity, format!("› {snippet}"));
}

/// Strip ASCII control chars (< 0x20, plus 0x7F DEL) EXCEPT tab (0x09)
/// which fits legibly in a table cell. Prevents a hostile transcript
/// with an embedded CSI sequence from smearing the pane's styling.
pub(crate) fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\t' {
                ' '
            } else if (c as u32) < 0x20 || c == '\u{7f}' {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Home-dir resolution — respects `RIMETERM_HOME` for tests via the
// upstream `rimeterm-config` helper (which already handles Windows /
// macOS / Linux user dirs).
// ---------------------------------------------------------------------------

fn home_dir() -> Option<PathBuf> {
    // Tests + power-users can point the enricher at an alternate
    // tree via `RIMETERM_HOME`. Matches the escape hatch
    // `rimeterm-config::paths::home()` already offers for its own
    // dot-dir; we reuse the name so users don't need two env vars.
    if let Ok(v) = std::env::var("RIMETERM_HOME") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    rimeterm_config::paths::user_home_dir()
}

// ---------------------------------------------------------------------------
// Dangerous-flag detection.
// ---------------------------------------------------------------------------

/// Substrings that mark an agent invocation as running with elevated
/// / unsafe permissions. Ported verbatim from upstream so the flag's
/// meaning stays consistent across the two binaries.
const DANGEROUS_FLAGS: &[&str] = &[
    "--dangerously-skip-permissions",
    "--no-permissions",
    "--allow-dangerous",
    "--yolo",
];

/// If `cmdline` contains a dangerous flag, return the specific
/// substring that triggered — surfaced in the detail popup so the
/// user sees what's actually in play (e.g. `--yolo` vs
/// `--dangerously-skip-permissions`).
pub fn dangerous_flag(cmdline: &str) -> Option<&'static str> {
    if cmdline.is_empty() {
        return None;
    }
    for flag in DANGEROUS_FLAGS {
        if cmdline.contains(flag) {
            return Some(*flag);
        }
    }
    // Sudo-prefixed launches count as elevated even without an
    // agent-specific flag. Cheap prefix check on the trimmed
    // cmdline; false positives (e.g. `/usr/bin/sudo-alike` shim)
    // would be surprising but harmless.
    let head = cmdline.trim_start();
    if head.starts_with("sudo claude")
        || head.starts_with("sudo codex")
        || head.starts_with("sudo /usr/bin/claude")
        || head.starts_with("sudo /usr/bin/codex")
    {
        return Some("sudo");
    }
    None
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn encode_cwd_posix() {
        assert_eq!(encode_cwd("/home/u/proj"), "-home-u-proj");
        assert_eq!(encode_cwd("/"), "-");
        // Non-alphanumeric chars (dots, underscores) also become `-`
        // — matches what Claude Code actually writes to disk.
        assert_eq!(
            encode_cwd("/home/u/00_code/foo.bar"),
            "-home-u-00-code-foo-bar"
        );
        // Dotfile prefix produces a double `-` (leading `-` from `/`,
        // second `-` from the `.` in `.rimedeck`).
        assert_eq!(encode_cwd("/home/u/.rimedeck"), "-home-u--rimedeck");
    }

    #[test]
    fn encode_cwd_windows() {
        assert_eq!(encode_cwd(r"C:\Users\u\proj"), "C--Users-u-proj");
        // Trailing separator gets stripped so we match the on-disk
        // encoding Claude wrote.
        assert_eq!(encode_cwd(r"C:\workspace\proj\"), "C--workspace-proj");
        // Forward slashes on a Windows path (as some tools normalise
        // to) still work.
        assert_eq!(encode_cwd(r"C:/Users/u"), "C--Users-u");
        // Real-world path with an underscore + hyphen mix — mirrors
        // the rimeterm workspace itself (`00_code`, `.claude`).
        assert_eq!(
            encode_cwd(r"C:\Users\zisheng\Documents\cao\00_code\github\rimeterm"),
            "C--Users-zisheng-Documents-cao-00-code-github-rimeterm"
        );
        // Dot in a segment (`.claude`) contributes an extra `-`.
        assert_eq!(
            encode_cwd(r"C:\Users\u\00_code\.claude"),
            "C--Users-u-00-code--claude"
        );
    }

    #[test]
    fn encode_cwd_empty() {
        assert_eq!(encode_cwd(""), "");
    }

    #[test]
    fn dangerous_flag_detects_yolo() {
        assert_eq!(dangerous_flag("claude --yolo"), Some("--yolo"));
        assert_eq!(
            dangerous_flag("claude --dangerously-skip-permissions"),
            Some("--dangerously-skip-permissions")
        );
        assert_eq!(
            dangerous_flag("claude --no-permissions"),
            Some("--no-permissions")
        );
        assert_eq!(dangerous_flag("sudo claude --resume"), Some("sudo"));
        assert_eq!(dangerous_flag("claude --resume"), None);
        assert_eq!(dangerous_flag(""), None);
    }

    #[test]
    fn sanitize_strips_control_chars() {
        // ESC (0x1b) → space, matching every other control char so
        // a rogue CSI can't smear the pane's styling.
        assert_eq!(sanitize("hello\x1b[31mworld"), "hello [31mworld");
        assert_eq!(sanitize("with\ttab"), "with tab");
        // Trailing / leading whitespace goes.
        assert_eq!(sanitize("  padded  "), "padded");
    }

    #[test]
    fn parse_enabled_reads_plugin_map() {
        let json = r#"{"enabledPlugins":{"caveman@main":true,"disabled@x":false}}"#;
        let e = parse_enabled(json);
        assert!(e.contains("caveman@main"));
        assert!(!e.contains("disabled@x"));
    }

    #[test]
    fn parse_installed_reads_plugin_names() {
        let json = r#"{"plugins":{"caveman@main":[{}],"other@y":[]}}"#;
        let i = parse_installed(json);
        assert!(i.contains("caveman@main"));
        assert!(i.contains("other@y"));
    }

    #[test]
    fn parse_enabled_handles_junk_gracefully() {
        assert!(parse_enabled("not json at all").is_empty());
        assert!(parse_enabled(r#"{"other":true}"#).is_empty());
    }

    #[test]
    fn skills_scan_finds_skill_md() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join(".claude").join("skills");
        fs::create_dir_all(root.join("sk-one")).unwrap();
        fs::create_dir_all(root.join("sk-two")).unwrap();
        // Bare dir without SKILL.md — should NOT be surfaced.
        fs::create_dir_all(root.join("no-skill-file")).unwrap();
        fs::write(root.join("sk-one").join("SKILL.md"), "hi").unwrap();
        fs::write(root.join("sk-two").join("SKILL.md"), "hi").unwrap();

        let mut out = BTreeSet::new();
        collect_skill_names(&root, &mut out);
        let v: Vec<String> = out.into_iter().collect();
        assert_eq!(v, vec!["sk-one".to_string(), "sk-two".to_string()]);
    }

    #[test]
    fn classify_state_covers_axes() {
        // Live + in-flight subagent → Spawning.
        assert_eq!(
            classify_state(true, 0, &None, true, false),
            SessionState::Spawning
        );
        // Live + fresh mtime → Busy.
        assert_eq!(
            classify_state(true, 1_000, &None, false, false),
            SessionState::Busy
        );
        // Live + in-flight tool but no fresh mtime → Busy still.
        assert_eq!(
            classify_state(true, 60_000, &None, false, true),
            SessionState::Busy
        );
        // Live + minute-old → Active.
        assert_eq!(
            classify_state(true, 60_000, &None, false, false),
            SessionState::Active
        );
        // Live + quiet → Idle.
        assert_eq!(
            classify_state(true, 6 * 60 * 1000, &None, false, false),
            SessionState::Idle
        );
        // Dead + end_turn → Completed.
        assert_eq!(
            classify_state(false, 60_000, &Some("end_turn".into()), false, false),
            SessionState::Completed
        );
        // Dead + fresh → Waiting.
        assert_eq!(
            classify_state(false, 60_000, &None, false, false),
            SessionState::Waiting
        );
        // Dead + old → Stale.
        assert_eq!(
            classify_state(false, 2 * RECENT_WINDOW_MS, &None, false, false),
            SessionState::Stale
        );
    }

    #[test]
    fn analyse_extracts_tokens_and_model() {
        let jsonl = vec![serde_json::json!({
            "type": "assistant",
            "message": {
                "model": "claude-sonnet-4-7",
                "usage": {
                    "input_tokens": 1000,
                    "output_tokens": 50,
                    "cache_read_input_tokens": 300,
                    "cache_creation_input_tokens": 100
                },
                "content": [{"type": "text", "text": "hello world"}]
            }
        })];
        let out = analyse(&jsonl);
        assert_eq!(out.model.as_deref(), Some("claude-sonnet-4-7"));
        // total input includes cache splits: 1000 + 300 + 100 = 1400
        assert_eq!(out.tokens_input, 1400);
        assert_eq!(out.tokens_output, 50);
        assert_eq!(out.tokens_cache_read, 300);
        assert_eq!(out.tokens_cache_write, 100);
        assert_eq!(out.context_used, 1400);
    }

    #[test]
    fn analyse_tracks_in_flight_subagents() {
        let jsonl = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {"subject": "review auth", "subagent_type": "code-reviewer"}
                    }]
                }
            }),
            // No matching tool_result → still in flight.
        ];
        let out = analyse(&jsonl);
        assert_eq!(out.in_flight_tasks, 1);
        assert_eq!(
            out.in_flight_subagents,
            vec!["code-reviewer: review auth".to_string()]
        );
    }

    #[test]
    fn analyse_completes_task_on_result() {
        let jsonl = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {"subject": "review auth"}
                    }]
                }
            }),
            serde_json::json!({
                "type": "user",
                "message": {
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "content": "done"
                    }]
                }
            }),
        ];
        let out = analyse(&jsonl);
        assert_eq!(out.in_flight_tasks, 0);
        assert!(out.in_flight_subagents.is_empty());
    }

    #[test]
    fn analyse_current_tool_cleared_after_result() {
        let jsonl = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use", "id": "b-1", "name": "Bash",
                        "input": {"command": "ls"}
                    }]
                }
            }),
            serde_json::json!({
                "type": "user",
                "message": {
                    "content": [{
                        "type": "tool_result", "tool_use_id": "b-1", "content": "a b c"
                    }]
                }
            }),
        ];
        let out = analyse(&jsonl);
        assert!(out.current_tool.is_none());
        // Bash still counted in tool_counts even though it's no
        // longer in flight.
        assert_eq!(out.tool_counts.get("Bash").copied(), Some(1));
    }

    #[test]
    fn analyse_recent_activity_capped() {
        let mut records = Vec::new();
        // 20 assistant `text` events — should truncate to 12.
        for i in 0..20 {
            records.push(serde_json::json!({
                "type": "assistant",
                "message": {"content": [{"type": "text", "text": format!("line {i}")}]}
            }));
        }
        let out = analyse(&records);
        assert_eq!(out.recent_activity.len(), RECENT_ACTIVITY_CAP);
        // Newest wins — last two lines survive.
        assert!(out.recent_activity.last().unwrap().contains("line 19"));
    }

    #[test]
    fn tail_of_short_file_returns_whole_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("short.jsonl");
        fs::write(&path, "hello world").unwrap();
        assert_eq!(read_tail(&path, TAIL_BYTES), "hello world");
    }

    #[test]
    fn tail_of_missing_file_returns_empty() {
        let path = Path::new("/tmp/this/does/not/exist/xxx.jsonl");
        assert!(read_tail(path, TAIL_BYTES).is_empty());
    }

    /// End-to-end enricher smoke: stand up a `~/.claude/projects/`
    /// tree under a temp dir, point `RIMETERM_HOME` at it, feed a
    /// live `claude` pid whose cwd would encode to the fixture's
    /// project dir, and assert the tokens + model come back through
    /// `enrich_claude`.
    ///
    /// This is the test that would have caught the "encoder only
    /// handles path separators" bug that made every real Claude
    /// session show up as `tokens: —` on Windows.
    #[test]
    fn enrich_claude_end_to_end_populates_tokens_and_model() {
        let _guard = rimeterm_config::test_util::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        // `user_home_dir()` respects `RIMETERM_HOME` via
        // `rimeterm-config::paths` — we shadow $HOME for the test so
        // the enricher scans our fixture tree instead of the real
        // one.
        let prev_home = std::env::var("RIMETERM_HOME").ok();
        // SAFETY: serialized by ENV_LOCK and restored below.
        unsafe {
            std::env::set_var("RIMETERM_HOME", dir.path());
        }

        // The cwd we'll pretend a `claude` pid is running from.
        // Contains dots + underscores so the encoder's
        // "everything-non-alphanumeric → -" rule gets exercised end
        // to end.
        let cwd_str = if cfg!(windows) {
            r"C:\Users\u\00_code\.claude-test".to_string()
        } else {
            "/home/u/00_code/.claude-test".to_string()
        };
        let encoded = encode_cwd(&cwd_str);
        let proj_dir = dir.path().join(".claude").join("projects").join(&encoded);
        std::fs::create_dir_all(&proj_dir).unwrap();

        // Minimal JSONL — one assistant record with a usage block
        // and a model. Enough for `analyse` to fill tokens_input,
        // tokens_output, and model.
        let session_id = "abcd-1234";
        let jsonl_path = proj_dir.join(format!("{session_id}.jsonl"));
        let record = serde_json::json!({
            "type": "assistant",
            "message": {
                "model": "claude-sonnet-4-7",
                "usage": {
                    "input_tokens": 1000,
                    "output_tokens": 250,
                    "cache_read_input_tokens": 200,
                    "cache_creation_input_tokens": 50
                },
                "content": [{"type": "text", "text": "hi"}]
            }
        });
        std::fs::write(
            &jsonl_path,
            format!("{}\n", serde_json::to_string(&record).unwrap()),
        )
        .unwrap();

        let live = LiveAgentRef {
            pid: 4242,
            cwd: cwd_str.as_str(),
            label: "claude",
            uptime_sec: 60,
        };
        let out = enrich_claude(&[live], 0);

        // Restore env before assertions so a failure doesn't leak.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("RIMETERM_HOME", v),
                None => std::env::remove_var("RIMETERM_HOME"),
            }
        }

        let summary = out
            .get(&4242)
            .expect("enricher should have paired the pid with the fixture session");
        assert_eq!(summary.model.as_deref(), Some("claude-sonnet-4-7"));
        // input includes cache_read + cache_creation.
        assert_eq!(summary.tokens_input, 1250);
        assert_eq!(summary.tokens_output, 250);
        assert_eq!(summary.tokens_cache_read, 200);
        assert_eq!(summary.tokens_cache_write, 50);
        assert_eq!(summary.session_id, session_id);
    }
}
