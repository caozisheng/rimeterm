use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use super::Store;
use super::outcome::{ArchiveOutcome, Reconcile, StoreError, UnarchiveOutcome};
use crate::todo::{self, Task};

/// Owns the archived (`archive.txt`) tasks and the lifecycle around loading them
/// off-thread at startup. Fields are `pub(crate)` so the `Store` methods in this
/// file can mutate the archive directly; external callers go through the read
/// methods.
pub struct Archive {
    pub(crate) tasks: Vec<Task>,
    pub(crate) path: PathBuf,
    pub(crate) last_disk: String,
    pub(crate) loader: Option<Receiver<(String, Vec<Task>)>>,
}

fn default_archive_path(todo_path: &Path) -> PathBuf {
    todo_path
        .parent()
        .map(|p| p.join("archive.txt"))
        .unwrap_or_else(|| PathBuf::from("archive.txt"))
}

impl Archive {
    /// Construct an `Archive` for the sibling `archive.txt` of `todo_path` and
    /// spawn a worker thread to read+parse it. The first frame can render
    /// `todo.txt` immediately while the loader runs in the background.
    pub fn spawn(todo_path: &Path) -> Self {
        Self::spawn_at(default_archive_path(todo_path))
    }

    /// Like [`Archive::spawn`] but for an explicit `archive.txt` path (e.g. a
    /// `ARCHIVE_FILE` that isn't a sibling of the todo file).
    pub fn spawn_at(path: PathBuf) -> Self {
        let loader_path = path.clone();
        let (tx, rx) = mpsc::sync_channel::<(String, Vec<Task>)>(1);
        thread::spawn(move || {
            let body = std::fs::read_to_string(&loader_path).unwrap_or_default();
            let parsed = todo::parse_file(&body);
            let _ = tx.send((body, parsed));
        });
        Self {
            tasks: Vec::new(),
            path,
            last_disk: String::new(),
            loader: Some(rx),
        }
    }

    /// Read and parse the sibling `archive.txt` inline (no background thread).
    /// Used by the one-shot CLI, where spawning a loader would be wasteful.
    pub fn load_sync(todo_path: &Path) -> Self {
        Self::load_sync_at(default_archive_path(todo_path))
    }

    /// Like [`Archive::load_sync`] but for an explicit `archive.txt` path.
    pub fn load_sync_at(path: PathBuf) -> Self {
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        let tasks = todo::parse_file(&body);
        Self {
            tasks,
            path,
            last_disk: body,
            loader: None,
        }
    }

    /// Test-only constructor that skips the worker thread and seeds in-memory
    /// state directly.
    #[cfg(test)]
    pub(crate) fn for_test(tasks: Vec<Task>, last_disk: String, path: PathBuf) -> Self {
        Self {
            tasks,
            path,
            last_disk,
            loader: None,
        }
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

/// Internal result of refreshing `archive.txt` before a mutation that writes it.
enum ArchiveRefresh {
    Ready,
    Reloaded,
    Error(std::io::Error),
}

impl Store {
    fn read_archive_body(&self) -> std::io::Result<String> {
        match std::fs::read_to_string(&self.archive.path) {
            Ok(body) => Ok(body),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e),
        }
    }

    fn refresh_archive_for_mutation(&mut self) -> ArchiveRefresh {
        let body = match self.read_archive_body() {
            Ok(b) => b,
            Err(e) => return ArchiveRefresh::Error(e),
        };
        if body != self.archive.last_disk {
            self.archive.tasks = todo::parse_file(&body);
            self.archive.last_disk = body;
            self.archive.loader = None;
            return ArchiveRefresh::Reloaded;
        }
        self.archive.loader = None;
        ArchiveRefresh::Ready
    }

    /// Pump archive state. Returns true when the visible archive changed: the
    /// startup loader landed, or an external edit to `archive.txt` was picked up.
    /// Non-blocking. The caller (TUI) is responsible for any view recompute.
    pub fn poll_archive(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.archive.loader {
            match rx.try_recv() {
                Ok((body, tasks)) => {
                    self.archive.last_disk = body;
                    self.archive.tasks = tasks;
                    self.archive.loader = None;
                    changed = true;
                }
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => {
                    self.archive.loader = None;
                }
            }
        }
        if !changed {
            let read = std::fs::read_to_string(&self.archive.path);
            changed = self.apply_archive_read(read);
        }
        changed
    }

    /// Apply a read result for `archive.txt`. `NotFound` is treated as an empty
    /// archive; any other I/O error preserves in-memory state and returns
    /// `false` rather than wiping the archive.
    pub(crate) fn apply_archive_read(&mut self, read: std::io::Result<String>) -> bool {
        let on_disk = match read {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(_) => return false,
        };
        if on_disk == self.archive.last_disk {
            return false;
        }
        self.archive.tasks = todo::parse_file(&on_disk);
        self.archive.last_disk = on_disk;
        true
    }

    /// Bulk move: shove every currently-completed task into `archive.txt`.
    /// Retained for the CLI `archive` command, which still means
    /// "flush every `x`-marked task at once".
    pub fn archive_completed(&mut self) -> ArchiveOutcome {
        let completed: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.done)
            .map(|(i, _)| i)
            .collect();
        if completed.is_empty() {
            return ArchiveOutcome::Nothing;
        }
        self.archive_many(&completed)
    }

    /// Move the single task at `abs` into `archive.txt`, preserving its
    /// exact raw line (done or pending). Powers the TUI `dd` action.
    pub fn archive_task(&mut self, abs: usize) -> ArchiveOutcome {
        if abs >= self.tasks.len() {
            return ArchiveOutcome::Nothing;
        }
        self.archive_many(&[abs])
    }

    /// Move every task at `indices` into `archive.txt` atomically.
    /// Out-of-range indices are dropped; duplicates collapsed.
    ///
    /// Contract:
    /// - Preserves each task's raw line (done or pending) — no state rewrites.
    /// - Reconciles the live todo file first; aborts on external change.
    /// - Writes archive first, then rewrites todo. On todo-write failure the
    ///   archive is rolled back so no row is lost.
    pub fn archive_many(&mut self, indices: &[usize]) -> ArchiveOutcome {
        match self.reconcile() {
            Reconcile::Unchanged => {}
            other => return ArchiveOutcome::Aborted(other),
        }
        let mut idxs: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|&i| i < self.tasks.len())
            .collect();
        idxs.sort_unstable();
        idxs.dedup();
        if idxs.is_empty() {
            return ArchiveOutcome::Nothing;
        }
        let to_move: Vec<Task> = idxs.iter().map(|&i| self.tasks[i].clone()).collect();
        // Read fresh so an external edit to archive.txt since startup isn't lost.
        let previous_archive_body = match self.read_archive_body() {
            Ok(b) => b,
            Err(e) => return ArchiveOutcome::Error(StoreError::ArchiveIo(e)),
        };
        let mut combined = previous_archive_body.clone();
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&todo::serialize(&to_move));
        // Write archive.txt before truncating todo.txt so a failed archive can't
        // lose data; if the todo write fails, roll archive.txt back.
        if let Err(e) = todo::write_atomic(&self.archive.path, &combined) {
            return ArchiveOutcome::Error(StoreError::ArchiveIo(e));
        }
        let keep: std::collections::HashSet<usize> = idxs.iter().copied().collect();
        let remaining: Vec<Task> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(i, _)| !keep.contains(i))
            .map(|(_, t)| t.clone())
            .collect();
        let remaining_body = todo::serialize(&remaining);
        if let Err(e) = todo::write_atomic(&self.file_path, &remaining_body) {
            let _ = todo::write_atomic(&self.archive.path, &previous_archive_body);
            return ArchiveOutcome::Error(StoreError::Write(e));
        }
        self.push_history();
        let count = to_move.len();
        self.tasks = remaining;
        self.last_disk = remaining_body;
        self.archive.tasks = todo::parse_file(&combined);
        self.archive.last_disk = combined;
        self.archive.loader = None;
        ArchiveOutcome::Archived { count }
    }

    /// Move an archived task back into the live list. `archive_idx` indexes
    /// `self.archive.tasks()`.
    ///
    /// State is preserved verbatim: a task archived while done comes back done,
    /// a task archived while pending comes back pending. Restoration point is
    /// the tail of the live list — priority/due-based sorts will place it
    /// visually next to its peers.
    pub fn unarchive(&mut self, archive_idx: usize) -> UnarchiveOutcome {
        match self.reconcile() {
            Reconcile::Unchanged => {}
            other => return UnarchiveOutcome::Aborted(other),
        }
        match self.refresh_archive_for_mutation() {
            ArchiveRefresh::Ready => {}
            ArchiveRefresh::Reloaded => return UnarchiveOutcome::ArchiveReloaded,
            ArchiveRefresh::Error(e) => return UnarchiveOutcome::Error(StoreError::ArchiveIo(e)),
        }
        if archive_idx >= self.archive.tasks.len() {
            return UnarchiveOutcome::OutOfRange;
        }
        let task = self.archive.tasks[archive_idx].clone();
        let new_archive: Vec<Task> = self
            .archive
            .tasks
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != archive_idx)
            .map(|(_, t)| t.clone())
            .collect();
        let archive_body = todo::serialize(&new_archive);
        if let Err(e) = todo::write_atomic(&self.archive.path, &archive_body) {
            return UnarchiveOutcome::Error(StoreError::ArchiveIo(e));
        }
        self.archive.tasks = new_archive;
        self.archive.last_disk = archive_body;
        self.push_history();
        self.tasks.push(task);
        if let Err(e) = self.persist() {
            return UnarchiveOutcome::Error(e);
        }
        UnarchiveOutcome::Unarchived
    }

    pub(crate) fn persist(&mut self) -> Result<(), StoreError> {
        let body = todo::serialize(&self.tasks);
        match todo::write_atomic(&self.file_path, &body) {
            Ok(()) => {
                self.last_disk = body;
                Ok(())
            }
            Err(e) => Err(StoreError::Write(e)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::core::Store;
    use crate::core::test_support::build_store;
    use std::time::{Duration, Instant};

    fn dir_for(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-archive-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn archive_writes_archive_file_then_truncates_todo() {
        let dir = dir_for("ok");
        let todo_path = dir.join("todo.txt");
        let raw = "(A) 2026-05-01 keep this +work\n\
                   x 2026-05-05 2026-05-01 archive this +work\n";
        std::fs::write(&todo_path, raw).unwrap();
        let mut store = Store::open_sync(todo_path.clone(), raw.to_string(), "2026-05-06".into());
        assert!(matches!(
            store.archive_completed(),
            ArchiveOutcome::Archived { count: 1 }
        ));
        let archived = std::fs::read_to_string(dir.join("archive.txt")).unwrap();
        assert!(archived.contains("archive this"));
        let todo = std::fs::read_to_string(&todo_path).unwrap();
        assert!(todo.contains("keep this"));
        assert!(!todo.contains("archive this"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_appends_to_existing_archive_file() {
        let dir = dir_for("append");
        let todo_path = dir.join("todo.txt");
        std::fs::write(dir.join("archive.txt"), "x 2026-04-01 2026-03-01 prior\n").unwrap();
        let raw = "x 2026-05-05 2026-05-01 fresh +work\n";
        std::fs::write(&todo_path, raw).unwrap();
        let mut store = Store::open_sync(todo_path, raw.to_string(), "2026-05-06".into());
        store.archive_completed();
        let archived = std::fs::read_to_string(dir.join("archive.txt")).unwrap();
        assert!(archived.contains("prior"));
        assert!(archived.contains("fresh"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_nothing_when_no_completed() {
        let mut store = build_store("a\nb\n");
        assert!(matches!(store.archive_completed(), ArchiveOutcome::Nothing));
    }

    fn wait_archive_loaded(store: &mut Store) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while store.archive.loader.is_some() && Instant::now() < deadline {
            let _ = store.poll_archive();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(store.archive.loader.is_none());
    }

    #[test]
    fn archive_loader_populates_archived_from_archive_file() {
        let dir = dir_for("loader");
        let todo_path = dir.join("todo.txt");
        std::fs::write(
            dir.join("archive.txt"),
            "x 2026-05-01 2026-04-01 first\nx 2026-05-02 2026-04-15 second\n",
        )
        .unwrap();
        std::fs::write(&todo_path, "(A) 2026-05-06 still open\n").unwrap();
        let mut store = Store::new(
            todo_path,
            "(A) 2026-05-06 still open\n".to_string(),
            "2026-05-06".into(),
        );
        wait_archive_loaded(&mut store);
        assert_eq!(store.archive.len(), 2);
        assert!(
            store
                .archive
                .tasks()
                .iter()
                .any(|t| t.raw.contains("first"))
        );
        assert_eq!(store.tasks().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_completed_populates_in_memory_archived() {
        let dir = dir_for("memsync");
        let todo_path = dir.join("todo.txt");
        let raw = "x 2026-05-05 2026-05-01 done one\nx 2026-05-06 2026-05-01 done two\n";
        std::fs::write(&todo_path, raw).unwrap();
        let mut store = Store::new(todo_path, raw.to_string(), "2026-05-06".into());
        store.archive_completed();
        assert_eq!(store.archive.len(), 2);
        let _ = store.poll_archive();
        std::thread::sleep(Duration::from_millis(20));
        let _ = store.poll_archive();
        assert_eq!(store.archive.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poll_archive_detects_external_archive_edit() {
        let dir = dir_for("external");
        let todo_path = dir.join("todo.txt");
        std::fs::write(&todo_path, "(A) 2026-05-06 a\n").unwrap();
        std::fs::write(dir.join("archive.txt"), "").unwrap();
        let mut store = Store::new(
            todo_path,
            "(A) 2026-05-06 a\n".to_string(),
            "2026-05-06".into(),
        );
        wait_archive_loaded(&mut store);
        assert!(store.archive.is_empty());
        std::fs::write(
            dir.join("archive.txt"),
            "x 2026-05-05 2026-05-01 added externally\n",
        )
        .unwrap();
        assert!(store.poll_archive());
        assert_eq!(store.archive.len(), 1);
        assert!(store.archive.tasks()[0].raw.contains("added externally"));
        assert!(!store.poll_archive());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poll_archive_preserves_archived_on_io_error() {
        let mut store = build_store("a\n");
        let path = store.archive.path().to_path_buf();
        store.archive = Archive::for_test(
            todo::parse_file("x 2026-05-01 2026-04-01 prior\n"),
            "x 2026-05-01 2026-04-01 prior\n".to_string(),
            path,
        );
        let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        assert!(!store.apply_archive_read(Err(err)));
        assert_eq!(store.archive.len(), 1);
    }

    #[test]
    fn unarchive_preserves_done_state_after_recurrence_roundtrip() {
        // Complete a recurring task → spawns a live successor and marks the
        // original done. Archive → the done one leaves the live list.
        // Unarchive → the original returns done (state preserved). We must
        // end with exactly one pending row and one done row, and the
        // recurrence successor is not duplicated on the roundtrip.
        let dir = dir_for("rec-roundtrip");
        let todo_path = dir.join("todo.txt");
        let raw = "Water plants due:2026-05-06 rec:1d\n";
        std::fs::write(&todo_path, raw).unwrap();
        let mut store = Store::new(todo_path, raw.to_string(), "2026-05-06".into());
        store.toggle_complete(0);
        assert_eq!(store.tasks().len(), 2);
        store.archive_completed();
        assert_eq!(store.tasks().len(), 1);
        assert!(!store.tasks()[0].done);
        assert_eq!(store.archive.len(), 1);
        store.unarchive(0);
        assert_eq!(store.tasks().len(), 2);
        let done_count = store.tasks().iter().filter(|t| t.done).count();
        let pending_count = store.tasks().iter().filter(|t| !t.done).count();
        assert_eq!(done_count, 1, "unarchive must preserve done state");
        assert_eq!(pending_count, 1, "successor stays untouched by roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_reports_write_failure() {
        let mut store = build_store("a\n");
        let directory_target = std::env::temp_dir().join(format!(
            "tuxedo-directory-target-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&directory_target);
        std::fs::create_dir_all(&directory_target).unwrap();
        store.file_path = directory_target.clone();
        assert!(store.persist().is_err());
        let _ = std::fs::remove_dir_all(directory_target);
    }
}
