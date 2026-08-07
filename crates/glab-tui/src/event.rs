#![allow(dead_code)]

use crossterm::event::{KeyEvent, MouseEvent};

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum Event {
    Tick,
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    PipelineJobs(u64, Vec<crate::domain::pipelines::Job>),
    IssuesFetched(Vec<crate::domain::issues::Issue>),
    MrsFetched(Vec<crate::domain::mr::MergeRequest>),
    PipelinesFetched(Vec<crate::domain::pipelines::Pipeline>),
    RunnersFetched(Vec<crate::domain::runners::Runner>),
    ReleasesFetched(Vec<crate::domain::releases::Release>),
    SelectorItemsFetched(Vec<String>),
    RepoAttributesFetched {
        labels: Vec<crate::domain::labels::Label>,
        members: Vec<String>,
    },
    FetchFailed(crate::app::Tab, String),
    DiffFetched {
        mr_iid: u64,
        raw_diff: String,
        comments: Vec<crate::domain::mr::DiscussionNote>,
    },
    DiffFetchFailed(String),
    TodosFetched(Vec<crate::domain::notifications::Notification>),
    JobsTabFetched(u64, Vec<crate::domain::pipelines::Job>),
    CommandStarted(String),
    CommandCompleted(crate::app::Tab, Result<(), String>),
    TerminalCommandLogged {
        timestamp: String,
        command: String,
        status: String,
    },
    MilestonesFetched(Vec<crate::domain::milestones::Milestone>),
    MilestoneIssuesFetched(u64, Vec<crate::domain::issues::Issue>),
    JobTraceFetched(u64, Result<String, String>),
    MilestoneUpdated,
    MilestoneClosed,
    MilestoneReopened,
    MilestoneDeleted,
    ReleaseUpdated,
    ReleaseDeleted,
    IssueDeleted,
    MrDeleted,
    BranchesFetched(Vec<crate::domain::branches::Branch>),
    EnvironmentsFetched(Vec<crate::domain::deployments::Environment>),
    DeploymentsFetched(Vec<crate::domain::deployments::Deployment>),
}
