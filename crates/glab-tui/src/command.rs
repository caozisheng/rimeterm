use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Output;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub stdin: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectContext {
    pub root: PathBuf,
    pub host: crate::backend::BackendKind,
    pub project: String,
    pub branch: Option<String>,
}

impl CommandRequest {
    pub fn new(
        root: &Path,
        program: impl Into<OsString>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: root.to_path_buf(),
            stdin: None,
        }
    }

    pub fn with_stdin(mut self, stdin: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(stdin.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError(pub String);

#[async_trait::async_trait]
pub trait CommandRunner: Send + Sync {
    async fn output(&self, request: CommandRequest) -> Result<CommandOutput, CommandError>;
}

pub(crate) trait SyncCommandRunner: Send + Sync {
    fn output_sync(&self, request: CommandRequest) -> Result<CommandOutput, CommandError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ProcessCommandRunner;

fn convert_output(output: Output) -> CommandOutput {
    CommandOutput {
        status: output.status.code().unwrap_or(-1),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

impl SyncCommandRunner for ProcessCommandRunner {
    fn output_sync(&self, request: CommandRequest) -> Result<CommandOutput, CommandError> {
        let mut command = std::process::Command::new(request.program);
        command.args(request.args).current_dir(request.cwd);
        if request.stdin.is_some() {
            command.stdin(std::process::Stdio::piped());
        }
        let mut child = command
            .spawn()
            .map_err(|error| CommandError(error.to_string()))?;
        if let (Some(stdin), Some(mut pipe)) = (request.stdin, child.stdin.take()) {
            use std::io::Write;
            pipe.write_all(&stdin)
                .map_err(|error| CommandError(error.to_string()))?;
        }
        child
            .wait_with_output()
            .map(convert_output)
            .map_err(|error| CommandError(error.to_string()))
    }
}

#[async_trait::async_trait]
impl CommandRunner for ProcessCommandRunner {
    async fn output(&self, request: CommandRequest) -> Result<CommandOutput, CommandError> {
        let mut command = tokio::process::Command::new(request.program);
        command.args(request.args).current_dir(request.cwd);
        if request.stdin.is_some() {
            command.stdin(std::process::Stdio::piped());
        }
        let mut child = command
            .spawn()
            .map_err(|error| CommandError(error.to_string()))?;
        if let (Some(stdin), Some(mut pipe)) = (request.stdin, child.stdin.take()) {
            use tokio::io::AsyncWriteExt;
            pipe.write_all(&stdin)
                .await
                .map_err(|error| CommandError(error.to_string()))?;
        }
        child
            .wait_with_output()
            .await
            .map(convert_output)
            .map_err(|error| CommandError(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct RecordingRunner {
        requests: Arc<Mutex<Vec<CommandRequest>>>,
    }

    #[async_trait::async_trait]
    impl CommandRunner for RecordingRunner {
        async fn output(&self, request: CommandRequest) -> Result<CommandOutput, CommandError> {
            self.requests.lock().push(request);
            Ok(CommandOutput {
                status: 0,
                stdout: b"feature/demo\n".to_vec(),
                stderr: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn recording_runner_preserves_explicit_workspace_root() {
        let runner = RecordingRunner::default();
        let root = PathBuf::from("C:/fixture/repo");
        runner
            .output(CommandRequest::new(
                &root,
                "git",
                ["branch", "--show-current"],
            ))
            .await
            .unwrap();
        assert_eq!(runner.requests.lock()[0].cwd, root);
    }

    #[test]
    fn process_runner_never_changes_process_current_dir() {
        let before = std::env::current_dir().expect("current dir");
        let request = CommandRequest::new(&PathBuf::from("C:/fixture/repo"), "git", ["--version"]);
        assert_eq!(request.cwd, PathBuf::from("C:/fixture/repo"));
        assert_eq!(std::env::current_dir().expect("current dir"), before);
    }
}
