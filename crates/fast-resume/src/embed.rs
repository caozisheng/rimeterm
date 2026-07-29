use std::path::PathBuf;

use anyhow::Result;

use crate::adapters::adapter_for;
use crate::index::{INDEX_REFRESH_BATCH_SIZE, RefreshSummary, SessionIndex};
use crate::model::Session;
use crate::search::SearchEngine;

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub generation: u64,
    pub query: String,
    pub agent_filter: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub generation: u64,
    pub sessions: Vec<Session>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeTarget {
    pub agent: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
}

#[derive(Clone)]
pub struct EmbeddedEngine {
    search: SearchEngine,
}

impl EmbeddedEngine {
    pub fn open_default() -> Result<Self> {
        Ok(Self {
            search: SearchEngine::open_default()?,
        })
    }

    pub fn search(&self, request: SearchRequest) -> SearchResult {
        let sessions = if request.limit == 0 {
            Ok(Vec::new())
        } else {
            self.search.search_result(
                &request.query,
                request.agent_filter.as_deref(),
                None,
                request.limit,
            )
        };
        match sessions {
            Ok(sessions) => SearchResult {
                generation: request.generation,
                sessions,
                error: None,
            },
            Err(error) => SearchResult {
                generation: request.generation,
                sessions: Vec::new(),
                error: Some(format!("{error:#}")),
            },
        }
    }

    pub fn reload(&mut self) -> Result<()> {
        self.search.reload()
    }

    pub fn refresh(&mut self) -> Result<RefreshSummary> {
        let summary = SessionIndex::open_default()?
            .refresh_incremental_streaming(INDEX_REFRESH_BATCH_SIZE, |_| {})?;
        self.search.reload()?;
        Ok(summary)
    }
}

pub fn search_request(request: SearchRequest) -> SearchResult {
    if request.limit == 0 {
        return SearchResult {
            generation: request.generation,
            sessions: Vec::new(),
            error: None,
        };
    }
    match EmbeddedEngine::open_default() {
        Ok(engine) => engine.search(request),
        Err(error) => SearchResult {
            generation: request.generation,
            sessions: Vec::new(),
            error: Some(format!("{error:#}")),
        },
    }
}

pub fn resume_target(session: &Session) -> Option<ResumeTarget> {
    let adapter = adapter_for(&session.agent)?;
    Some(ResumeTarget {
        agent: session.agent.clone(),
        argv: adapter.resume_command(session, false),
        cwd: PathBuf::from(&session.directory),
    })
}

#[cfg(test)]
mod tests {
    use chrono::Local;

    use super::*;

    #[test]
    fn search_result_preserves_request_generation() {
        let result = search_request(SearchRequest {
            generation: 17,
            query: String::new(),
            agent_filter: None,
            limit: 0,
        });

        assert_eq!(result.generation, 17);
    }

    #[test]
    fn resume_target_uses_adapter_command_and_session_directory() {
        let session = Session::new(
            "session-42",
            "codex",
            "Auth fix",
            "C:/work/api",
            Local::now(),
            "content",
            2,
        );

        let target = resume_target(&session).expect("codex adapter");

        assert_eq!(
            target,
            ResumeTarget {
                agent: "codex".to_string(),
                argv: vec![
                    "codex".to_string(),
                    "resume".to_string(),
                    "session-42".to_string(),
                ],
                cwd: PathBuf::from("C:/work/api"),
            }
        );
    }
}
