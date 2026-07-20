//! Command registry.
//!
//! §2.4 of the design doc: a flat `<namespace>.<verb>` command space that the
//! command palette, keymap engine, app menu, and `rimectl` all draw from. The
//! kernel MUST NOT hardcode who provides a command — every entry is registered
//! at runtime.

use std::collections::HashMap;
use std::sync::Arc;

use thiserror::Error;

/// Stable identifier such as `app.settings` or `workspace.tab.close`.
///
/// We keep this as `&'static str` because commands are registered from
/// compile-time string literals in practice, and `PartialEq` on `&str` is a
/// cheap pointer/short-slice compare.
pub type CommandId = &'static str;

/// Signal-only closure form (v0.1..M6): no args, no result, no failure. Used
/// by every command that just flips an atomic flag drained by the app main
/// loop.
pub type CommandFn = Arc<dyn Fn() + Send + Sync + 'static>;

/// New-style closure form (M7+): takes JSON args, returns a JSON result or a
/// human-readable error. Callers wanting the old zero-arg shape can wrap a
/// [`CommandFn`] via [`Command::signal`].
pub type CommandFnV2 =
    Arc<dyn Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync + 'static>;

#[derive(Clone)]
pub struct Command {
    pub id: CommandId,
    pub title: &'static str,
    /// Optional short description used by the palette footer.
    pub description: Option<&'static str>,
    /// Body executed when the command fires. Accepts JSON args and returns a
    /// JSON result / error.
    pub run: CommandFnV2,
}

impl Command {
    /// Convenience constructor for the common "flip a flag" style: takes a
    /// zero-arg [`CommandFn`], ignores whatever args come in, always returns
    /// `Value::Null`.
    pub fn signal(
        id: CommandId,
        title: &'static str,
        description: Option<&'static str>,
        f: CommandFn,
    ) -> Self {
        let run: CommandFnV2 = Arc::new(move |_args: &serde_json::Value| {
            (f)();
            Ok(serde_json::Value::Null)
        });
        Self {
            id,
            title,
            description,
            run,
        }
    }
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("command `{0}` is not registered")]
    NotFound(CommandId),
    #[error("command `{0}` already exists")]
    Duplicate(CommandId),
    #[error("command `{id}` failed: {msg}")]
    Failed { id: CommandId, msg: String },
}

/// Registry the kernel owns. Not internally locked — assume single-owner
/// (usually the kernel main task) or wrap in `Mutex` at the call site.
#[derive(Default)]
pub struct CommandRegistry {
    map: HashMap<CommandId, Command>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, cmd: Command) -> Result<(), CommandError> {
        if self.map.contains_key(cmd.id) {
            return Err(CommandError::Duplicate(cmd.id));
        }
        self.map.insert(cmd.id, cmd);
        Ok(())
    }

    /// Zero-arg convenience: run with `Value::Null` and discard the result.
    pub fn run(&self, id: CommandId) -> Result<(), CommandError> {
        self.run_with(id, &serde_json::Value::Null).map(|_| ())
    }

    /// Full form: run with args, return the JSON result.
    pub fn run_with(
        &self,
        id: CommandId,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, CommandError> {
        let cmd = self.map.get(id).ok_or(CommandError::NotFound(id))?;
        (cmd.run)(args).map_err(|msg| CommandError::Failed { id, msg })
    }

    pub fn get(&self, id: CommandId) -> Option<&Command> {
        self.map.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.map.values()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_command_ignores_args_and_flips_side_effect() {
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f = flag.clone();
        let cmd = Command::signal(
            "test.flip",
            "test flip",
            None,
            Arc::new(move || {
                f.store(true, std::sync::atomic::Ordering::Relaxed);
            }),
        );
        let mut reg = CommandRegistry::new();
        reg.register(cmd).unwrap();
        let out = reg
            .run_with("test.flip", &serde_json::json!({"ignored": 1}))
            .unwrap();
        assert_eq!(out, serde_json::Value::Null);
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn v2_command_reads_args_and_returns_value() {
        let mut reg = CommandRegistry::new();
        reg.register(Command {
            id: "test.double",
            title: "double",
            description: None,
            run: Arc::new(|args: &serde_json::Value| {
                let n = args
                    .get("n")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| "missing n".to_string())?;
                Ok(serde_json::json!({"doubled": n * 2}))
            }),
        })
        .unwrap();
        let out = reg
            .run_with("test.double", &serde_json::json!({"n": 21}))
            .unwrap();
        assert_eq!(out, serde_json::json!({"doubled": 42}));
    }

    #[test]
    fn v2_command_failure_maps_to_command_error() {
        let mut reg = CommandRegistry::new();
        reg.register(Command {
            id: "test.fail",
            title: "fail",
            description: None,
            run: Arc::new(|_| Err("nope".into())),
        })
        .unwrap();
        let err = reg
            .run_with("test.fail", &serde_json::Value::Null)
            .unwrap_err();
        assert!(matches!(err, CommandError::Failed { .. }));
    }

    #[test]
    fn duplicate_registration_rejected() {
        let mut reg = CommandRegistry::new();
        let make = || Command::signal("dup", "x", None, Arc::new(|| {}));
        reg.register(make()).unwrap();
        let err = reg.register(make()).unwrap_err();
        assert!(matches!(err, CommandError::Duplicate("dup")));
    }
}
