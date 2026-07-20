//! Global keymap dispatch.
//!
//! §4.2 of the design doc — the priority ladder is:
//! 1. Menu popover (if open) — see [`crate::menu`].
//! 2. Palette overlay (if open) — see [`crate::palette`].
//! 3. Global bindings registered here (Ctrl+Q, Ctrl+Shift+P, Alt+[/], …).
//! 4. Focused pane's `on_key` fallback.
//!
//! The engine returns [`KeymapOutcome`] instead of running side effects itself
//! so the app main loop can route to the command registry / pane provider it
//! already owns.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rimeterm_core::command::CommandId;

/// What the engine decides for a given key press.
#[derive(Debug, PartialEq)]
pub enum KeymapOutcome {
    /// Fire this command (kernel resolves).
    Run(CommandId),
    /// Nothing bound; caller forwards to the focused pane.
    Passthrough,
    /// Consumed by the engine but no command fires (reserved for M2+ actions).
    Consumed,
}

pub struct Keymap;

impl Keymap {
    /// Global bindings. Kept as a `match` for locality; a later revision moves
    /// these into a config-driven table.
    ///
    /// **Windows Terminal caveat**: `Ctrl+Shift+P` is grabbed by WT for its
    /// own command palette **by default** and never reaches this dispatcher.
    /// Users on WT should either (a) unbind it in WT's Settings → Actions
    /// or (b) use the `F1` alternate we register below. Same story with
    /// `Alt+[` / `Alt+]`: some terminal setups swallow the Alt modifier on
    /// ASCII punctuation, so we also accept `Ctrl+PageUp` / `Ctrl+PageDown`.
    pub fn dispatch(key: KeyEvent) -> KeymapOutcome {
        tracing::trace!(?key, "keymap dispatch");

        // Ctrl+Q: quit.
        if matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return KeymapOutcome::Run("app.quit");
        }
        // Palette:
        //   Ctrl+Shift+P  (design doc primary; blocked by WT by default)
        //   F1            (alternate — no terminal is known to intercept F1)
        if matches!(key.code, KeyCode::Char('p') | KeyCode::Char('P'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
        {
            return KeymapOutcome::Run("app.palette.open");
        }
        if matches!(key.code, KeyCode::F(1)) {
            return KeymapOutcome::Run("app.palette.open");
        }
        // Ctrl+T / Ctrl+W: new / close shell tab (only meaningful when shells
        // group is focused; the command handler will validate policy).
        if matches!(key.code, KeyCode::Char('t') | KeyCode::Char('T'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::SHIFT)
        {
            return KeymapOutcome::Run("workspace.shells.new");
        }
        if matches!(key.code, KeyCode::Char('w') | KeyCode::Char('W'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return KeymapOutcome::Run("workspace.shells.close");
        }
        // Ctrl+Alt+R: enter keyboard Resize mode.
        if matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::ALT)
        {
            return KeymapOutcome::Run("app.resize.toggle");
        }
        // Tab cycling — accept both bindings so at least one works on every
        // terminal setup. Ctrl+PageUp/Down come through even on terminals
        // that swallow the Alt modifier on `[` / `]`.
        if matches!(key.code, KeyCode::PageUp) && key.modifiers.contains(KeyModifiers::CONTROL) {
            return KeymapOutcome::Run("workspace.tab.prev");
        }
        if matches!(key.code, KeyCode::PageDown) && key.modifiers.contains(KeyModifiers::CONTROL) {
            return KeymapOutcome::Run("workspace.tab.next");
        }
        // All Alt-modifier bindings share one block for clarity.
        if key.modifiers.contains(KeyModifiers::ALT) {
            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
            match key.code {
                // Alt+[ / Alt+]: cycle tabs in the focused group.
                KeyCode::Char('[') | KeyCode::Char('{') => {
                    return KeymapOutcome::Run("workspace.tab.prev");
                }
                KeyCode::Char(']') | KeyCode::Char('}') => {
                    return KeymapOutcome::Run("workspace.tab.next");
                }
                // Alt+H/J/K/L: cross-cell focus (§19.4).
                KeyCode::Char('h') | KeyCode::Char('H') => {
                    return KeymapOutcome::Run("workspace.focus.left");
                }
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    return KeymapOutcome::Run("workspace.focus.right");
                }
                KeyCode::Char('k') | KeyCode::Char('K') => {
                    return KeymapOutcome::Run("workspace.focus.up");
                }
                KeyCode::Char('j') | KeyCode::Char('J') => {
                    return KeymapOutcome::Run("workspace.focus.down");
                }
                // Alt+Shift+1..9: jump to Nth tab in focused group.
                KeyCode::Char(c) if shift => {
                    if let Some(idx) = digit_1_to_9(c) {
                        return KeymapOutcome::Run(tab_goto_command_id(idx));
                    }
                }
                // Alt+1..4: quadrant jump (§19.4).
                KeyCode::Char(c) => {
                    if let Some(cmd) = quadrant_command(c) {
                        return KeymapOutcome::Run(cmd);
                    }
                }
                _ => {}
            }
        }
        // F10 / Alt+M: app menu.
        if matches!(key.code, KeyCode::F(10)) {
            return KeymapOutcome::Run("app.menu.toggle");
        }
        if matches!(key.code, KeyCode::Char('m') | KeyCode::Char('M'))
            && key.modifiers.contains(KeyModifiers::ALT)
        {
            return KeymapOutcome::Run("app.menu.toggle");
        }
        KeymapOutcome::Passthrough
    }
}

fn digit_1_to_9(c: char) -> Option<usize> {
    match c {
        '1'..='9' => Some((c as u8 - b'1') as usize),
        _ => None,
    }
}

/// Map Alt+1..4 to the four quadrant focus commands. Returns `None` for other
/// characters so the caller can fall through.
fn quadrant_command(c: char) -> Option<CommandId> {
    Some(match c {
        '1' => "workspace.focus.quadrant.1",
        '2' => "workspace.focus.quadrant.2",
        '3' => "workspace.focus.quadrant.3",
        '4' => "workspace.focus.quadrant.4",
        _ => return None,
    })
}

pub const QUADRANT_COMMANDS: [CommandId; 4] = [
    "workspace.focus.quadrant.1",
    "workspace.focus.quadrant.2",
    "workspace.focus.quadrant.3",
    "workspace.focus.quadrant.4",
];
/// Map an index 0..=8 to a stable command id string. We pre-declare them so
/// the palette can enumerate everything.
const TAB_GOTO_IDS: [&str; 9] = [
    "workspace.tab.goto.1",
    "workspace.tab.goto.2",
    "workspace.tab.goto.3",
    "workspace.tab.goto.4",
    "workspace.tab.goto.5",
    "workspace.tab.goto.6",
    "workspace.tab.goto.7",
    "workspace.tab.goto.8",
    "workspace.tab.goto.9",
];

pub fn tab_goto_command_id(idx: usize) -> CommandId {
    TAB_GOTO_IDS[idx.min(TAB_GOTO_IDS.len() - 1)]
}

pub fn all_tab_goto_ids() -> &'static [CommandId] {
    &TAB_GOTO_IDS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn ctrl_q_quits() {
        assert_eq!(
            Keymap::dispatch(key(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            KeymapOutcome::Run("app.quit")
        );
    }

    #[test]
    fn alt_bracket_cycles_tab() {
        assert_eq!(
            Keymap::dispatch(key(KeyCode::Char(']'), KeyModifiers::ALT)),
            KeymapOutcome::Run("workspace.tab.next"),
        );
        assert_eq!(
            Keymap::dispatch(key(KeyCode::Char('['), KeyModifiers::ALT)),
            KeymapOutcome::Run("workspace.tab.prev"),
        );
    }

    #[test]
    fn f1_opens_palette() {
        assert_eq!(
            Keymap::dispatch(key(KeyCode::F(1), KeyModifiers::NONE)),
            KeymapOutcome::Run("app.palette.open"),
        );
    }

    #[test]
    fn ctrl_shift_p_opens_palette_when_terminal_lets_it_through() {
        assert_eq!(
            Keymap::dispatch(key(
                KeyCode::Char('P'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            KeymapOutcome::Run("app.palette.open"),
        );
    }

    #[test]
    fn ctrl_pageup_pgdn_cycle_tab() {
        assert_eq!(
            Keymap::dispatch(key(KeyCode::PageUp, KeyModifiers::CONTROL)),
            KeymapOutcome::Run("workspace.tab.prev"),
        );
        assert_eq!(
            Keymap::dispatch(key(KeyCode::PageDown, KeyModifiers::CONTROL)),
            KeymapOutcome::Run("workspace.tab.next"),
        );
    }

    #[test]
    fn alt_shift_digits_route_to_goto() {
        assert_eq!(
            Keymap::dispatch(key(
                KeyCode::Char('1'),
                KeyModifiers::ALT | KeyModifiers::SHIFT
            )),
            KeymapOutcome::Run("workspace.tab.goto.1")
        );
        assert_eq!(
            Keymap::dispatch(key(
                KeyCode::Char('9'),
                KeyModifiers::ALT | KeyModifiers::SHIFT
            )),
            KeymapOutcome::Run("workspace.tab.goto.9")
        );
    }

    #[test]
    fn plain_char_passes_through() {
        assert_eq!(
            Keymap::dispatch(key(KeyCode::Char('a'), KeyModifiers::NONE)),
            KeymapOutcome::Passthrough
        );
    }

    #[test]
    fn alt_hjkl_route_to_direction_commands() {
        assert_eq!(
            Keymap::dispatch(key(KeyCode::Char('h'), KeyModifiers::ALT)),
            KeymapOutcome::Run("workspace.focus.left")
        );
        assert_eq!(
            Keymap::dispatch(key(KeyCode::Char('l'), KeyModifiers::ALT)),
            KeymapOutcome::Run("workspace.focus.right")
        );
        assert_eq!(
            Keymap::dispatch(key(KeyCode::Char('k'), KeyModifiers::ALT)),
            KeymapOutcome::Run("workspace.focus.up")
        );
        assert_eq!(
            Keymap::dispatch(key(KeyCode::Char('j'), KeyModifiers::ALT)),
            KeymapOutcome::Run("workspace.focus.down")
        );
    }

    #[test]
    fn alt_1_to_4_jumps_to_quadrant() {
        for (c, expected) in [
            ('1', "workspace.focus.quadrant.1"),
            ('2', "workspace.focus.quadrant.2"),
            ('3', "workspace.focus.quadrant.3"),
            ('4', "workspace.focus.quadrant.4"),
        ] {
            assert_eq!(
                Keymap::dispatch(key(KeyCode::Char(c), KeyModifiers::ALT)),
                KeymapOutcome::Run(expected)
            );
        }
    }

    #[test]
    fn ctrl_alt_r_toggles_resize_mode() {
        assert_eq!(
            Keymap::dispatch(key(
                KeyCode::Char('r'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            KeymapOutcome::Run("app.resize.toggle")
        );
    }
}
