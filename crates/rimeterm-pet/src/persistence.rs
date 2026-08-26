use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::engine;
use crate::state::PetState;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StoreMode {
    Owner,
    Spectator,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("failed to create pet directory `{path}`: {source}")]
    CreateDirectory { path: PathBuf, source: io::Error },
    #[error("failed to read pet state `{path}`: {source}")]
    ReadState { path: PathBuf, source: io::Error },
    #[error("failed to parse pet state `{path}`: {source}")]
    ParseState {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to serialize pet state: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("failed to write pet state `{path}`: {source}")]
    WriteState { path: PathBuf, source: io::Error },
    #[error("failed to replace pet state `{path}`: {source}")]
    ReplaceState { path: PathBuf, source: io::Error },
}

#[derive(Debug, Serialize, Deserialize)]
struct PetSave {
    schema_version: u32,
    pet: PetState,
}

const SCHEMA_VERSION: u32 = 1;

pub struct PetStore {
    state_path: PathBuf,
    lock_path: PathBuf,
    mode: StoreMode,
    state: PetState,
    lock_owned: bool,
}

impl PetStore {
    pub fn open(
        state_path: &Path,
        lock_path: &Path,
        now: DateTime<Utc>,
    ) -> Result<Self, StoreError> {
        ensure_parent(state_path)?;
        ensure_parent(lock_path)?;
        let lock_owned = acquire_lock(lock_path).map_err(|source| StoreError::CreateDirectory {
            path: lock_path.to_path_buf(),
            source,
        })?;
        let mode = if lock_owned {
            StoreMode::Owner
        } else {
            StoreMode::Spectator
        };
        let state = load_state(state_path, now)?;
        Ok(Self {
            state_path: state_path.to_path_buf(),
            lock_path: lock_path.to_path_buf(),
            mode,
            state,
            lock_owned,
        })
    }

    pub fn mode(&self) -> StoreMode {
        self.mode
    }

    pub fn state(&self) -> &PetState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut PetState {
        &mut self.state
    }

    pub fn reload(&mut self, now: DateTime<Utc>) -> Result<(), StoreError> {
        self.state = load_state(&self.state_path, now)?;
        Ok(())
    }

    pub fn save(&self) -> Result<(), StoreError> {
        if self.mode != StoreMode::Owner {
            return Ok(());
        }
        let save = PetSave {
            schema_version: SCHEMA_VERSION,
            pet: self.state.clone(),
        };
        let body = serde_json::to_vec_pretty(&save)?;
        let temp_path = self.state_path.with_extension("json.tmp");
        fs::write(&temp_path, body).map_err(|source| StoreError::WriteState {
            path: temp_path.clone(),
            source,
        })?;
        replace_file(&temp_path, &self.state_path).map_err(|source| StoreError::ReplaceState {
            path: self.state_path.clone(),
            source,
        })
    }
}

impl Drop for PetStore {
    fn drop(&mut self) {
        if self.lock_owned {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

fn ensure_parent(path: &Path) -> Result<(), StoreError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent).map_err(|source| StoreError::CreateDirectory {
        path: parent.to_path_buf(),
        source,
    })
}

fn acquire_lock(path: &Path) -> io::Result<bool> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => {
            let _ = file.set_len(0);
            fs::write(path, format!("{}\n", std::process::id()))?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let owner_pid = fs::read_to_string(path)
                .ok()
                .and_then(|contents| contents.trim().parse::<u32>().ok());
            if owner_pid.is_some_and(pid_is_alive) {
                return Ok(false);
            }
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => {
                    fs::write(path, format!("{}\n", std::process::id()))?;
                    Ok(true)
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn pid_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 performs an existence/permission check only; it
        // never delivers a signal to the target process.
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        result == 0 || std::io::Error::last_os_error().kind() == io::ErrorKind::PermissionDenied
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .split(',')
                    .nth(1)
                    .is_some_and(|field| field.trim_matches('"').trim() == pid.to_string())
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

fn load_state(path: &Path, now: DateTime<Utc>) -> Result<PetState, StoreError> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(PetState::new_egg(now));
        }
        Err(source) => {
            return Err(StoreError::ReadState {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let mut state = match serde_json::from_str::<PetSave>(&data) {
        Ok(save) if save.schema_version == SCHEMA_VERSION => save.pet,
        Ok(save) => save.pet,
        Err(_) => match serde_json::from_str::<PetState>(&data) {
            Ok(state) => state,
            Err(_) => {
                let backup = path.with_extension(format!("corrupt-{}", now.timestamp_millis()));
                let _ = fs::copy(path, backup);
                return Ok(PetState::new_egg(now));
            }
        },
    };

    if now < state.last_tick {
        state.last_tick = now;
        return Ok(state);
    }
    if now > state.last_tick {
        engine::tick(&mut state, now);
    }
    Ok(state)
}

fn replace_file(temp_path: &Path, path: &Path) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        fs::rename(temp_path, path)
    }
    #[cfg(windows)]
    {
        match fs::rename(temp_path, path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                let backup = path.with_extension(format!("replace-{}", std::process::id()));
                if backup.exists() {
                    fs::remove_file(&backup)?;
                }
                fs::rename(path, &backup)?;
                match fs::rename(temp_path, path) {
                    Ok(()) => {
                        let _ = fs::remove_file(backup);
                        Ok(())
                    }
                    Err(error) => {
                        let _ = fs::rename(&backup, path);
                        Err(error)
                    }
                }
            }
            Err(source) => Err(source),
        }
    }
}
