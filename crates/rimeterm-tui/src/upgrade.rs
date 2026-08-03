//! Cross-platform online-upgrade modal state and rendering.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::updater::AvailableRelease;

#[derive(Clone, Debug)]
pub enum WorkerEvent {
    CheckFinished {
        generation: u64,
        result: Result<Option<AvailableRelease>, String>,
    },
    Progress {
        generation: u64,
        downloaded: u64,
        total: u64,
    },
    DownloadFinished {
        generation: u64,
        result: Result<PathBuf, String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpgradeAction {
    Check {
        generation: u64,
    },
    Download {
        generation: u64,
        release: AvailableRelease,
    },
    Close,
}

#[derive(Clone, Debug)]
enum Phase {
    Idle,
    Checking,
    UpToDate,
    Available(AvailableRelease),
    Downloading {
        release: AvailableRelease,
        downloaded: u64,
        total: u64,
    },
    ReadyToInstall(PathBuf),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradePhase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    ReadyToInstall,
    Failed,
}

#[derive(Clone, Debug)]
pub struct UpgradeState {
    pub open: bool,
    generation: u64,
    phase: Phase,
}

impl Default for UpgradeState {
    fn default() -> Self {
        Self {
            open: false,
            generation: 0,
            phase: Phase::Idle,
        }
    }
}

impl UpgradeState {
    pub fn phase(&self) -> UpgradePhase {
        match self.phase {
            Phase::Idle => UpgradePhase::Idle,
            Phase::Checking => UpgradePhase::Checking,
            Phase::UpToDate => UpgradePhase::UpToDate,
            Phase::Available(_) => UpgradePhase::Available,
            Phase::Downloading { .. } => UpgradePhase::Downloading,
            Phase::ReadyToInstall(_) => UpgradePhase::ReadyToInstall,
            Phase::Failed(_) => UpgradePhase::Failed,
        }
    }

    pub fn open_and_check(&mut self) -> u64 {
        self.open = true;
        match self.phase {
            Phase::Downloading { .. } | Phase::ReadyToInstall(_) => self.generation,
            _ => self.start_check(),
        }
    }

    pub fn start_check(&mut self) -> u64 {
        self.open = true;
        self.generation = self.generation.wrapping_add(1);
        self.phase = Phase::Checking;
        self.generation
    }

    /// Background variant of [`start_check`]: bumps the generation and
    /// moves to `Checking`, but leaves `open` alone so the caller can
    /// probe GitHub Releases at startup without popping the modal in
    /// the user's face. The generation still gates the resulting
    /// [`WorkerEvent::CheckFinished`] through the same [`apply`]
    /// pipeline, so if the user hits Menu → Upgrade before the
    /// background result lands, [`open_and_check`] takes over cleanly
    /// (its own `start_check` advances the generation past this one,
    /// so the stale silent response gets filtered out).
    pub fn start_silent_check(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.phase = Phase::Checking;
        self.generation
    }

    /// Current generation counter — exposed so the App can guard its
    /// own snapshot of the latest available release against stale
    /// worker events (only mirror after an [`apply`] whose event
    /// generation matches).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Latest known release when we're parked on `Phase::Available`.
    /// Any other phase — Checking, UpToDate, Downloading,
    /// ReadyToInstall, Failed — returns `None`. Used by the hint-bar
    /// update chip so it can render the target version alongside the
    /// "⚠ Update available" chip.
    pub fn available_release(&self) -> Option<&AvailableRelease> {
        match &self.phase {
            Phase::Available(release) => Some(release),
            _ => None,
        }
    }

    pub fn apply(&mut self, event: WorkerEvent) {
        let event_generation = match &event {
            WorkerEvent::CheckFinished { generation, .. }
            | WorkerEvent::Progress { generation, .. }
            | WorkerEvent::DownloadFinished { generation, .. } => *generation,
        };
        if event_generation != self.generation {
            return;
        }
        match event {
            WorkerEvent::CheckFinished { result, .. } => {
                self.phase = match result {
                    Ok(Some(release)) => Phase::Available(release),
                    Ok(None) => Phase::UpToDate,
                    Err(error) => Phase::Failed(error),
                };
            }
            WorkerEvent::Progress {
                downloaded, total, ..
            } => {
                if let Phase::Downloading {
                    release,
                    downloaded: current,
                    total: expected,
                } = &mut self.phase
                {
                    *current = downloaded;
                    *expected = total;
                    let _ = release;
                }
            }
            WorkerEvent::DownloadFinished { result, .. } => {
                self.phase = match result {
                    Ok(path) => Phase::ReadyToInstall(path),
                    Err(error) => Phase::Failed(error),
                };
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<UpgradeAction> {
        if !self.open {
            return None;
        }
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            self.open = false;
            return Some(UpgradeAction::Close);
        }
        if !matches!(key.code, KeyCode::Enter | KeyCode::Char('r')) {
            return None;
        }
        match &self.phase {
            Phase::Available(release) => {
                let total = release.windows_installer.as_ref()?.msi.size;
                let release = release.clone();
                self.generation = self.generation.wrapping_add(1);
                self.phase = Phase::Downloading {
                    release: release.clone(),
                    downloaded: 0,
                    total,
                };
                Some(UpgradeAction::Download {
                    generation: self.generation,
                    release,
                })
            }
            Phase::Failed(_) | Phase::UpToDate => Some(UpgradeAction::Check {
                generation: self.start_check(),
            }),
            _ => None,
        }
    }

    pub fn ready_installer_path(&self) -> Option<PathBuf> {
        match &self.phase {
            Phase::ReadyToInstall(path) => Some(path.clone()),
            _ => None,
        }
    }

    pub fn popup_rect(area: Rect) -> Rect {
        let width = area
            .width
            .saturating_mul(4)
            .saturating_div(5)
            .clamp(44, 100);
        let height = area
            .height
            .saturating_mul(4)
            .saturating_div(5)
            .clamp(12, 32);
        Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width.min(area.width),
            height.min(area.height),
        )
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if !self.open {
            return;
        }
        let popup = Self::popup_rect(area);
        Clear.render(popup, buf);
        let block = Block::default().title(" Upgrade ").borders(Borders::ALL);
        let inner = block.inner(popup);
        block.render(popup, buf);
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    "Current version: ",
                    Style::default().add_modifier(Modifier::DIM),
                ),
                Span::raw(env!("CARGO_PKG_VERSION")),
            ]),
            Line::raw(""),
        ];
        match &self.phase {
            Phase::Idle | Phase::Checking => lines.push(Line::styled(
                "Checking GitHub Releases…",
                Style::default().fg(Color::LightCyan),
            )),
            Phase::UpToDate => {
                lines.push(Line::styled(
                    "rimeterm is up to date.",
                    Style::default().fg(Color::Green),
                ));
                lines.push(Line::raw(""));
                lines.push(Line::raw("Enter/R: check again · Esc: close"));
            }
            Phase::Available(release) => {
                lines.push(Line::styled(
                    format!("Version {} is available", release.version),
                    Style::default()
                        .fg(Color::LightGreen)
                        .add_modifier(Modifier::BOLD),
                ));
                if let Some(date) = &release.published_at {
                    lines.push(Line::raw(format!("Published: {date}")));
                }
                if let Some(installer) = &release.windows_installer {
                    lines.push(Line::raw(format!(
                        "Installer: {} bytes",
                        installer.msi.size
                    )));
                }
                lines.push(Line::raw(format!("Release: {}", release.html_url)));
                lines.push(Line::raw(""));
                lines.extend(
                    release
                        .notes
                        .lines()
                        .take(inner.height.saturating_sub(10) as usize)
                        .map(Line::raw),
                );
                lines.push(Line::raw(""));
                let action = if release.windows_installer.is_some() {
                    "Enter: download and install · Esc: close"
                } else {
                    "Update information only · Esc: close"
                };
                lines.push(Line::styled(action, Style::default().fg(Color::Yellow)));
            }
            Phase::Downloading {
                downloaded, total, ..
            } => {
                let percent = if *total == 0 {
                    0
                } else {
                    downloaded.saturating_mul(100) / total
                };
                lines.push(Line::styled(
                    "Downloading verified MSI…",
                    Style::default().fg(Color::LightCyan),
                ));
                lines.push(Line::raw(format!(
                    "{downloaded} / {total} bytes ({percent}%)"
                )));
                lines.push(Line::raw(""));
                lines.push(Line::raw("Esc hides this window; download continues."));
            }
            Phase::ReadyToInstall(path) => {
                lines.push(Line::styled(
                    "Installer verified.",
                    Style::default().fg(Color::Green),
                ));
                lines.push(Line::raw(path.display().to_string()));
                lines.push(Line::raw("Launching Windows Installer…"));
            }
            Phase::Failed(error) => {
                lines.push(Line::styled(
                    "Upgrade failed",
                    Style::default()
                        .fg(Color::LightRed)
                        .add_modifier(Modifier::BOLD),
                ));
                lines.push(Line::raw(""));
                lines.extend(error.lines().map(Line::raw));
                lines.push(Line::raw(""));
                lines.push(Line::raw("Enter/R: retry · Esc: close"));
            }
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::{AvailableRelease, ReleaseAsset, WindowsInstaller};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use semver::Version;

    fn release() -> AvailableRelease {
        AvailableRelease {
            version: Version::parse("0.3.0").unwrap(),
            notes: "new feature".into(),
            html_url: "https://example.invalid/release".into(),
            published_at: Some("2026-07-29T00:00:00Z".into()),
            windows_installer: Some(WindowsInstaller {
                msi: ReleaseAsset {
                    name: "rimeterm-0.3.0-x86_64.msi".into(),
                    browser_download_url: "https://example.invalid/rimeterm.msi".into(),
                    size: 100,
                    digest: None,
                },
                checksums: ReleaseAsset {
                    name: "SHA256SUMS".into(),
                    browser_download_url: "https://example.invalid/SHA256SUMS".into(),
                    size: 64,
                    digest: None,
                },
            }),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn open_starts_a_new_check_generation() {
        let mut state = UpgradeState::default();

        let generation = state.open_and_check();

        assert!(state.open);
        assert_eq!(generation, 1);
        assert!(matches!(state.phase(), UpgradePhase::Checking));
    }

    #[test]
    fn available_release_enter_starts_download() {
        let mut state = UpgradeState::default();
        let generation = state.open_and_check();
        state.apply(WorkerEvent::CheckFinished {
            generation,
            result: Ok(Some(release())),
        });

        let action = state.handle_key(key(KeyCode::Enter));

        assert!(matches!(
            action,
            Some(UpgradeAction::Download { generation: 2, .. })
        ));
        assert!(matches!(state.phase(), UpgradePhase::Downloading));
    }

    #[test]
    fn information_only_release_enter_does_not_start_download() {
        let mut info = release();
        info.windows_installer = None;
        let mut state = UpgradeState::default();
        let generation = state.open_and_check();
        state.apply(WorkerEvent::CheckFinished {
            generation,
            result: Ok(Some(info)),
        });

        assert_eq!(state.handle_key(key(KeyCode::Enter)), None);
        assert!(matches!(state.phase(), UpgradePhase::Available));
    }

    #[test]
    fn failed_check_enter_retries_with_new_generation() {
        let mut state = UpgradeState::default();
        let generation = state.open_and_check();
        state.apply(WorkerEvent::CheckFinished {
            generation,
            result: Err("offline".into()),
        });

        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            Some(UpgradeAction::Check { generation: 2 })
        );
        assert!(matches!(state.phase(), UpgradePhase::Checking));
    }

    #[test]
    fn stale_worker_event_cannot_replace_newer_attempt() {
        let mut state = UpgradeState::default();
        let old = state.open_and_check();
        let new = state.start_check();

        state.apply(WorkerEvent::CheckFinished {
            generation: old,
            result: Ok(Some(release())),
        });

        assert_eq!(new, 2);
        assert!(matches!(state.phase(), UpgradePhase::Checking));
    }

    #[test]
    fn closing_during_download_hides_without_cancelling_state() {
        let mut state = UpgradeState::default();
        let generation = state.open_and_check();
        state.apply(WorkerEvent::CheckFinished {
            generation,
            result: Ok(Some(release())),
        });
        let _ = state.handle_key(key(KeyCode::Enter));

        assert_eq!(
            state.handle_key(key(KeyCode::Esc)),
            Some(UpgradeAction::Close)
        );
        assert!(!state.open);
        assert!(matches!(state.phase(), UpgradePhase::Downloading));
    }

    #[test]
    fn verified_download_becomes_ready_to_install() {
        let mut state = UpgradeState::default();
        let generation = state.open_and_check();
        state.apply(WorkerEvent::CheckFinished {
            generation,
            result: Ok(Some(release())),
        });
        let action = state.handle_key(key(KeyCode::Enter)).unwrap();
        let UpgradeAction::Download { generation, .. } = action else {
            panic!("expected download action")
        };
        let path = std::path::PathBuf::from(r"C:\Temp\rimeterm.msi");

        state.apply(WorkerEvent::DownloadFinished {
            generation,
            result: Ok(path.clone()),
        });

        assert_eq!(state.ready_installer_path(), Some(path));
        assert!(matches!(state.phase(), UpgradePhase::ReadyToInstall));
    }

    #[test]
    fn silent_check_does_not_open_the_overlay() {
        let mut state = UpgradeState::default();

        let generation = state.start_silent_check();

        assert_eq!(generation, 1);
        assert!(!state.open, "silent check must NOT open the overlay");
        assert!(matches!(state.phase(), UpgradePhase::Checking));
        assert!(state.available_release().is_none());
    }

    #[test]
    fn silent_check_result_populates_available_release() {
        let mut state = UpgradeState::default();
        let generation = state.start_silent_check();

        state.apply(WorkerEvent::CheckFinished {
            generation,
            result: Ok(Some(release())),
        });

        assert!(!state.open);
        assert!(matches!(state.phase(), UpgradePhase::Available));
        assert_eq!(
            state.available_release().map(|r| r.version.to_string()),
            Some("0.3.0".to_string())
        );
    }

    #[test]
    fn user_open_after_silent_available_advances_generation_past_silent() {
        // Silent check has landed on Available; hitting Menu → Upgrade
        // must start a fresh interactive check (users expect the
        // overlay to reflect a live probe, not a cached result).
        let mut state = UpgradeState::default();
        let silent_gen = state.start_silent_check();
        state.apply(WorkerEvent::CheckFinished {
            generation: silent_gen,
            result: Ok(Some(release())),
        });

        let interactive_gen = state.open_and_check();

        assert!(state.open);
        assert!(interactive_gen > silent_gen);
        assert!(matches!(state.phase(), UpgradePhase::Checking));
    }
}
