//! Low-latency activity polling for live coding-agent sessions.
//!
//! The regular agent snapshot intentionally remains slow and comprehensive.
//! This module only reads bounded transcript tails and extracts the current
//! tool plus its human-readable intent on a dedicated worker thread.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const ACTIVITY_TAIL_BYTES: u64 = 32 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivityAgent {
    pub pid: u32,
    pub label: String,
    pub cwd: String,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivityState {
    pub current_tool: String,
    pub current_activity: String,
}

impl ActivityState {
    fn new(tool: impl Into<String>, activity: impl Into<String>) -> Self {
        Self {
            current_tool: tool.into(),
            current_activity: activity.into(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ActivitySnapshot {
    by_pid: HashMap<u32, ActivityState>,
}

struct ActivityCandidate {
    session_id: String,
    mtime_ms: u64,
    state: Option<ActivityState>,
}

fn select_current_activity<I>(candidates: I, session_id: Option<&str>) -> Option<ActivityState>
where
    I: IntoIterator<Item = ActivityCandidate>,
{
    let candidates = candidates.into_iter().collect::<Vec<_>>();
    if let Some(session_id) = session_id {
        return candidates
            .into_iter()
            .find(|candidate| candidate.session_id == session_id)
            .and_then(|candidate| candidate.state);
    }
    candidates
        .into_iter()
        .max_by_key(|candidate| candidate.mtime_ms)
        .and_then(|candidate| candidate.state)
}

enum Request {
    Poll { agents: Vec<ActivityAgent> },
}

pub struct ActivityMonitor {
    request_tx: Sender<Request>,
    response_rx: Receiver<ActivitySnapshot>,
    agents: Vec<ActivityAgent>,
    latest: ActivitySnapshot,
    last_request: Instant,
}

impl ActivityMonitor {
    pub fn new() -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        thread::Builder::new()
            .name("rimeterm-agent-activity".into())
            .spawn(move || run(request_rx, response_tx))
            .expect("spawn activity worker");
        Self {
            request_tx,
            response_rx,
            agents: Vec::new(),
            latest: ActivitySnapshot::default(),
            last_request: Instant::now() - POLL_INTERVAL,
        }
    }

    pub fn poll(&mut self, agents: &[ActivityAgent]) -> bool {
        if self.agents != agents {
            self.agents = agents.to_vec();
            self.latest = ActivitySnapshot::default();
        }
        if self.last_request.elapsed() >= POLL_INTERVAL {
            let _ = self.request_tx.send(Request::Poll {
                agents: self.agents.clone(),
            });
            self.last_request = Instant::now();
        }
        let mut changed = false;
        while let Ok(snapshot) = self.response_rx.try_recv() {
            if snapshot.by_pid != self.latest.by_pid {
                self.latest = snapshot;
                changed = true;
            }
        }
        changed
    }

    pub fn activity_for(&self, pid: u32) -> Option<&ActivityState> {
        self.latest.by_pid.get(&pid)
    }
}

impl Default for ActivityMonitor {
    fn default() -> Self {
        Self::new()
    }
}

fn run(request_rx: Receiver<Request>, response_tx: Sender<ActivitySnapshot>) {
    while let Ok(Request::Poll { agents }) = request_rx.recv() {
        let mut snapshot = ActivitySnapshot::default();
        for agent in agents {
            let state = match agent.label.as_str() {
                "omp" | "oh-my-pi" | "pi" => find_omp_activity(&agent),
                "claude" | "claude-code" => find_claude_activity(&agent),
                "codex" | "openai-codex" => find_codex_activity(&agent),
                _ => None,
            };
            if let Some(state) = state {
                snapshot.by_pid.insert(agent.pid, state);
            }
        }
        if response_tx.send(snapshot).is_err() {
            break;
        }
    }
}

fn find_omp_activity(agent: &ActivityAgent) -> Option<ActivityState> {
    let root = omp_sessions_root()?;
    let home = user_home()?;
    let mut candidates = Vec::new();
    for variant in crate::agtop_omp::encode_cwd_variants(&agent.cwd, &home) {
        let dir = root.join(variant);
        for path in jsonl_files(&dir) {
            let records = parse_jsonl(&read_tail(&path));
            candidates.push(ActivityCandidate {
                session_id: path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                mtime_ms: file_mtime_ms(&path),
                state: parse_omp_activity(&records),
            });
        }
    }
    select_current_activity(candidates, agent.session_id.as_deref())
}

fn find_claude_activity(agent: &ActivityAgent) -> Option<ActivityState> {
    let home = user_home()?;
    let dir = home
        .join(".claude")
        .join("projects")
        .join(crate::agtop_session::encode_cwd(&agent.cwd));
    let mut candidates = Vec::new();
    for path in jsonl_files(&dir) {
        let records = parse_jsonl(&read_tail(&path));
        candidates.push(ActivityCandidate {
            session_id: path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            mtime_ms: file_mtime_ms(&path),
            state: parse_claude_activity(&records),
        });
    }
    select_current_activity(candidates, agent.session_id.as_deref())
}

fn find_codex_activity(agent: &ActivityAgent) -> Option<ActivityState> {
    let home = user_home()?;
    let root = home.join(".codex").join("sessions");
    let mut candidates = Vec::new();
    for path in jsonl_files_recursive(&root) {
        let metadata = parse_jsonl(&read_head(&path));
        if !codex_metadata_matches(&metadata, &agent.cwd) {
            continue;
        }
        let records = parse_jsonl(&read_tail(&path));
        candidates.push(ActivityCandidate {
            session_id: path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            mtime_ms: file_mtime_ms(&path),
            state: parse_codex_activity(&records),
        });
    }
    select_current_activity(candidates, agent.session_id.as_deref())
}

fn codex_metadata_matches(records: &[Value], cwd: &str) -> bool {
    records.iter().any(|record| {
        record.get("type").and_then(Value::as_str) == Some("session_meta")
            && record
                .pointer("/payload/cwd")
                .and_then(Value::as_str)
                .is_some_and(|value| value == cwd)
    })
}

fn omp_sessions_root() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("OMP_CODING_AGENT_SESSION_DIR")
        && !path.trim().is_empty()
    {
        return Some(expand_home(&path));
    }
    let home = user_home()?;
    let agent_dir = std::env::var("OMP_CODING_AGENT_DIR")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .map(|path| expand_home(&path))
        .unwrap_or_else(|| home.join(".omp").join("agent"));
    let settings = agent_dir.join("settings.json");
    if let Ok(bytes) = fs::read(settings)
        && let Ok(json) = serde_json::from_slice::<Value>(&bytes)
        && let Some(path) = json.get("sessionDir").and_then(Value::as_str)
        && !path.trim().is_empty()
    {
        return Some(expand_home(path));
    }
    Some(agent_dir.join("sessions"))
}

fn user_home() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("RIMETERM_HOME")
        && !path.is_empty()
    {
        return Some(PathBuf::from(path));
    }
    rimeterm_config::paths::user_home_dir()
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return user_home().unwrap_or_default();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return user_home().unwrap_or_default().join(rest);
    }
    #[cfg(windows)]
    if let Some(rest) = path.strip_prefix("~\\") {
        return user_home().unwrap_or_default().join(rest);
    }
    PathBuf::from(path)
}

fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .collect()
}

fn jsonl_files_recursive(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(jsonl_files_recursive(&path));
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
    out
}

fn file_mtime_ms(path: &Path) -> u64 {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn read_head(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let mut bytes = Vec::with_capacity(ACTIVITY_TAIL_BYTES as usize);
    let _ = file
        .by_ref()
        .take(ACTIVITY_TAIL_BYTES)
        .read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn read_tail(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let Ok(metadata) = file.metadata() else {
        return String::new();
    };
    let amount = ACTIVITY_TAIL_BYTES.min(metadata.len());
    if file.seek(SeekFrom::End(-(amount as i64))).is_err() {
        return String::new();
    }
    let mut bytes = Vec::with_capacity(amount as usize);
    let _ = file.take(amount).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn parse_jsonl(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn activity_from_value(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(normalize)
        .filter(|text| !text.is_empty())
}

pub(crate) fn parse_omp_activity(records: &[Value]) -> Option<ActivityState> {
    let mut current: Option<(String, ActivityState)> = None;
    for record in records {
        if record.get("customType").and_then(Value::as_str) == Some("tool_execution_start") {
            if let Some(data) = record.get("data")
                && let (Some(id), Some(tool)) = (
                    data.get("toolCallId").and_then(Value::as_str),
                    data.get("toolName").and_then(Value::as_str),
                )
            {
                let intent = activity_from_value(data.get("intent"));
                if let Some(intent) = intent {
                    current = Some((id.to_string(), ActivityState::new(tool, intent)));
                }
            }
        }
        if let Some(message) = record.get("message") {
            if message.get("role").and_then(Value::as_str) == Some("assistant")
                && let Some(content) = message.get("content").and_then(Value::as_array)
            {
                for item in content {
                    if item.get("type").and_then(Value::as_str) != Some("toolCall") {
                        continue;
                    }
                    let Some(id) = item.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(tool) = item.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let intent = activity_from_value(item.get("intent")).or_else(|| {
                        item.get("arguments").and_then(|args| {
                            activity_from_value(args.get("i").or_else(|| args.get("intent")))
                        })
                    });
                    if let Some(intent) = intent {
                        current = Some((id.to_string(), ActivityState::new(tool, intent)));
                    }
                }
            }
            if message.get("role").and_then(Value::as_str) == Some("toolResult") {
                let result_id = message
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .or_else(|| record.get("parentToolCallId").and_then(Value::as_str))
                    .or_else(|| {
                        message
                            .get("content")
                            .and_then(Value::as_array)
                            .and_then(|items| {
                                items.iter().find_map(|item| {
                                    item.get("toolCallId")
                                        .or_else(|| item.get("toolUseId"))
                                        .and_then(Value::as_str)
                                })
                            })
                    });
                if current
                    .as_ref()
                    .is_some_and(|(id, _)| Some(id.as_str()) == result_id)
                {
                    current = None;
                }
            }
        }
    }
    current.map(|(_, state)| state)
}

pub(crate) fn parse_claude_activity(records: &[Value]) -> Option<ActivityState> {
    let mut current: Option<(String, ActivityState)> = None;
    for record in records {
        let Some(message) = record.get("message") else {
            continue;
        };
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for item in content {
            match item.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let Some(id) = item.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(tool) = item.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let intent = item
                        .get("input")
                        .and_then(|input| {
                            input
                                .get("description")
                                .or_else(|| input.get("i"))
                                .or_else(|| input.get("command"))
                                .or_else(|| input.get("path"))
                        })
                        .and_then(|value| value.as_str())
                        .map(normalize)
                        .filter(|text| !text.is_empty());
                    if let Some(intent) = intent {
                        current = Some((id.to_string(), ActivityState::new(tool, intent)));
                    }
                }
                Some("tool_result") => {
                    let result_id = item.get("tool_use_id").and_then(Value::as_str);
                    if current
                        .as_ref()
                        .is_some_and(|(id, _)| Some(id.as_str()) == result_id)
                    {
                        current = None;
                    }
                }
                _ => {}
            }
        }
    }
    current.map(|(_, state)| state)
}

pub(crate) fn parse_codex_activity(records: &[Value]) -> Option<ActivityState> {
    let mut current: Option<(String, ActivityState)> = None;
    for record in records {
        if record.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(payload) = record.get("payload") else {
            continue;
        };
        match payload.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let Some(id) = payload.get("call_id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(name) = payload.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let activity = payload
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|args| serde_json::from_str::<Value>(args).ok())
                    .and_then(|args| {
                        ["description", "command", "cmd"]
                            .into_iter()
                            .find_map(|key| args.get(key).and_then(Value::as_str).map(normalize))
                    })
                    .filter(|text| !text.is_empty());
                if let Some(activity) = activity {
                    current = Some((id.to_string(), ActivityState::new(name, activity)));
                }
            }
            Some("function_call_output") => {
                let result_id = payload.get("call_id").and_then(Value::as_str);
                if current
                    .as_ref()
                    .is_some_and(|(id, _)| Some(id.as_str()) == result_id)
                {
                    current = None;
                }
            }
            _ => {}
        }
    }
    current.map(|(_, state)| state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omp_parser_uses_latest_in_flight_intent() {
        let records = vec![
            serde_json::json!({"type":"custom","customType":"tool_execution_start","data":{"toolCallId":"call_1","toolName":"bash","intent":"Checking Tidy availability"}}),
            serde_json::json!({"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_1","name":"bash","arguments":{"i":"Checking Tidy availability"}}]}}),
        ];
        assert_eq!(
            parse_omp_activity(&records),
            Some(ActivityState::new("bash", "Checking Tidy availability"))
        );
    }

    #[test]
    fn claude_parser_uses_description_and_matching_result_clears_it() {
        let start = serde_json::json!({"message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"description":"Running focused tests"}}]}});
        assert_eq!(
            parse_claude_activity(std::slice::from_ref(&start)),
            Some(ActivityState::new("Bash", "Running focused tests"))
        );
        let result = serde_json::json!({"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}});
        assert_eq!(parse_claude_activity(&[start, result]), None);
    }

    #[test]
    fn codex_parser_uses_function_call_name_and_arguments() {
        let records = vec![
            serde_json::json!({"type":"response_item","payload":{"type":"function_call","call_id":"call_1","name":"shell_command","arguments":"{\"command\":\"cargo test\"}"}}),
        ];
        assert_eq!(
            parse_codex_activity(&records),
            Some(ActivityState::new("shell_command", "cargo test"))
        );
    }

    #[test]
    fn omp_parser_clears_activity_from_result_content_id() {
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
                "content": [{"type": "text", "toolCallId": "call_tidy"}]
            }
        });

        assert_eq!(parse_omp_activity(&[start, result]), None);
    }

    #[test]
    fn codex_metadata_match_survives_activity_tail_boundary() {
        let records = vec![serde_json::json!({
            "type": "session_meta",
            "payload": {"cwd": "C:\\work"}
        })];
        assert!(codex_metadata_matches(&records, r"C:\work"));
    }

    #[test]
    fn tail_reader_keeps_valid_utf8_after_multibyte_boundary() {
        let dir = tempfile::tempdir().expect("create temp directory");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "x".repeat(ACTIVITY_TAIL_BYTES as usize) + "\n✓")
            .expect("write fixture");
        assert!(read_tail(&path).contains('✓'));
    }

    #[test]
    fn current_session_without_activity_does_not_fall_back_to_older_session() {
        let old = ActivityCandidate {
            session_id: "old-session".into(),
            mtime_ms: 10,
            state: Some(ActivityState::new("bash", "再次构建0.2.5")),
        };
        let current = ActivityCandidate {
            session_id: "current-session".into(),
            mtime_ms: 20,
            state: None,
        };

        assert_eq!(
            select_current_activity([old, current], Some("current-session")),
            None
        );
    }
}
