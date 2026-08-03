//! Shared key interpretation and action application for standalone and embedded use.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::app::{AddOutcome, App, CalendarTarget, DialogInputMode, Mode, OverlayKind, View};
use crate::keybinds::{KeyBindings, ResolvedKey};
use crate::{cli, clipboard, theme, todo};

/// Features that may perform process-global I/O or mutate host-owned preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerFeatures {
    pub share: bool,
    pub notes: bool,
    pub config: bool,
    pub theme: bool,
    pub clipboard: bool,
}

impl ControllerFeatures {
    pub const STANDALONE: Self = Self {
        share: true,
        notes: true,
        config: true,
        theme: true,
        clipboard: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerOutcome {
    Handled,
    ExitRequested,
}

pub fn next_deadline(app: &App) -> Option<std::time::Instant> {
    match (app.flash_deadline(), app.chord.deadline()) {
        (Some(flash), Some(chord)) => Some(flash.min(chord)),
        (one, other) => one.or(other),
    }
}

pub fn clear_expired(app: &mut App) -> bool {
    let mut changed = false;
    if app.flash_should_clear() {
        app.clear_flash();
        changed = true;
    }
    if app.chord.should_clear() {
        app.chord.clear();
        changed = true;
    }
    changed
}

/// Interpret one key using the same modal dispatcher as the standalone TUI.
pub fn handle_key(
    app: &mut App,
    key: KeyEvent,
    keybinds: &KeyBindings,
    features: ControllerFeatures,
) -> ControllerOutcome {
    // Detect external edits before processing the key. On detection the
    // file is reloaded, the keystroke is consumed (re-press to act on the
    // new state), and the per-mutator checks become no-ops downstream.
    if !app.check_external_changes() {
        return ControllerOutcome::Handled;
    }
    match app.mode {
        Mode::Insert => handle_insert(app, key),
        Mode::Search => handle_search(app, key),
        Mode::Help => handle_help(app, key),
        Mode::Settings => handle_settings(app, key),
        Mode::PromptProject | Mode::PromptContext | Mode::PromptSaveFilter => {
            handle_prompt(app, key)
        }
        Mode::PickProject | Mode::PickContext | Mode::PickSavedFilter => handle_pick(app, key),
        Mode::PickTheme => handle_pick_theme(app, key),
        Mode::CommandPalette => handle_command_palette(app, key, features),
        Mode::Share => handle_share(app, key),
        Mode::Welcome => {
            handle_welcome(app, key);
            if app.should_quit {
                app.should_quit = false;
                return ControllerOutcome::ExitRequested;
            }
        }
        Mode::Normal | Mode::Visual => {
            if handle_normal(app, key, keybinds, features) {
                return ControllerOutcome::ExitRequested;
            }
        }
    }
    ControllerOutcome::Handled
}

/// First-run welcome prompt. `c` creates `./todo.txt` (the App's current
/// `file_path`) and edits it; `s` opens the bundled sample; `q`/`Esc` quits
/// without creating anything. Any other key is ignored so a stray press
/// doesn't silently pick an option.
fn handle_welcome(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('c') => match cli::ensure_file(app.file_path.clone()) {
            Ok(_) => app.mode = Mode::Normal,
            Err(e) => app.flash(format!("could not create {}: {e}", app.file_path.display())),
        },
        KeyCode::Char('s') => match cli::sample_path() {
            Ok(sample) => {
                let done = cli::archive_path(&sample);
                let body = std::fs::read_to_string(&sample).unwrap_or_default();
                app.open_file(sample, done, body);
                app.mode = Mode::Normal;
            }
            Err(e) => app.flash(format!("could not open sample: {e}")),
        },
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        _ => {}
    }
}

/// Share overlay: any key dismisses, returning to Normal. The server
/// keeps running in the background; pressing `s` again re-shows the
/// same QR without rebinding.
fn handle_share(app: &mut App, _key: KeyEvent) {
    app.mode = Mode::Normal;
}

/// What the draft buffer changed (or didn't) in response to a key. Lets
/// callers like search distinguish a text edit (which must re-run the filter)
/// from a cursor move (which must not, otherwise navigating within the search
/// box would reset the visible-list cursor on every arrow press).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DraftEffect {
    Unhandled,
    CursorMoved,
    TextChanged,
}

/// A single text-editing operation on the draft buffer. Covers the standard
/// keys (insert/backspace/delete/arrows/Home/End) plus the readline/emacs set
/// (Ctrl+A/E/B/F/H/D/W/U/K, Alt+B/F/D). Modeling the keystroke as an action
/// keeps the insert/search/prompt/command-palette contexts in sync — they all
/// route through the same resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditAction {
    Insert(char),
    DeleteBackward,
    DeleteForward,
    DeleteWordBackward,
    DeleteWordForward,
    KillToStart,
    KillToEnd,
    MoveLeft,
    MoveRight,
    MoveHome,
    MoveEnd,
    MoveWordForward,
    MoveWordBackward,
}

impl EditAction {
    fn apply(self, app: &mut App) -> DraftEffect {
        match self {
            EditAction::Insert(c) => {
                app.draft_insert_char(c);
                DraftEffect::TextChanged
            }
            EditAction::DeleteBackward => {
                app.draft_backspace();
                DraftEffect::TextChanged
            }
            EditAction::DeleteForward => {
                app.draft_delete_forward();
                DraftEffect::TextChanged
            }
            EditAction::DeleteWordBackward => {
                app.draft_delete_word_backward();
                DraftEffect::TextChanged
            }
            EditAction::DeleteWordForward => {
                app.draft_delete_word_forward();
                DraftEffect::TextChanged
            }
            EditAction::KillToStart => {
                app.draft_kill_to_start();
                DraftEffect::TextChanged
            }
            EditAction::KillToEnd => {
                app.draft_kill_to_end();
                DraftEffect::TextChanged
            }
            EditAction::MoveLeft => {
                app.draft_left();
                DraftEffect::CursorMoved
            }
            EditAction::MoveRight => {
                app.draft_right();
                DraftEffect::CursorMoved
            }
            EditAction::MoveHome => {
                app.draft_home();
                DraftEffect::CursorMoved
            }
            EditAction::MoveEnd => {
                app.draft_end();
                DraftEffect::CursorMoved
            }
            EditAction::MoveWordForward => {
                app.draft_word_forward();
                DraftEffect::CursorMoved
            }
            EditAction::MoveWordBackward => {
                app.draft_word_backward();
                DraftEffect::CursorMoved
            }
        }
    }
}

/// Map a single keystroke to an `EditAction`, or `None` when the key isn't a
/// text-editing key. A *single* Control or Alt chord is matched first and
/// never falls through to the plain `Char(c)` insert arm, so an unmapped chord
/// (e.g. Ctrl+G) is swallowed rather than typed as a literal control letter —
/// this is what fixes Ctrl+H inserting an 'h' instead of deleting. Ctrl+N/Ctrl+P
/// are deliberately left unmapped: upstream handlers reserve them for popup
/// and list navigation.
///
/// CONTROL **and** ALT together is AltGr, which crossterm reports for printable
/// characters on international layouts (e.g. AltGr+E → `€`). That is text, not a
/// chord, so the chord arms are gated on exactly one modifier being held and
/// AltGr falls through to the `Char(c)` insert arm.
fn resolve_edit_key(key: KeyEvent) -> Option<EditAction> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if ctrl && !alt {
        return match key.code {
            KeyCode::Char('a') => Some(EditAction::MoveHome),
            KeyCode::Char('e') => Some(EditAction::MoveEnd),
            KeyCode::Char('b') => Some(EditAction::MoveLeft),
            KeyCode::Char('f') => Some(EditAction::MoveRight),
            KeyCode::Char('h') => Some(EditAction::DeleteBackward),
            KeyCode::Char('d') => Some(EditAction::DeleteForward),
            KeyCode::Char('w') => Some(EditAction::DeleteWordBackward),
            KeyCode::Char('u') => Some(EditAction::KillToStart),
            KeyCode::Char('k') => Some(EditAction::KillToEnd),
            // Ctrl+Backspace as delete-word is a common modern expectation;
            // terminals that report it this way get it for free.
            KeyCode::Backspace => Some(EditAction::DeleteWordBackward),
            _ => None,
        };
    }
    if alt && !ctrl {
        return match key.code {
            KeyCode::Char('b') => Some(EditAction::MoveWordBackward),
            KeyCode::Char('f') => Some(EditAction::MoveWordForward),
            KeyCode::Char('d') => Some(EditAction::DeleteWordForward),
            // M-DEL is readline's backward-kill-word.
            KeyCode::Backspace => Some(EditAction::DeleteWordBackward),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Backspace => Some(EditAction::DeleteBackward),
        KeyCode::Delete => Some(EditAction::DeleteForward),
        KeyCode::Left => Some(EditAction::MoveLeft),
        KeyCode::Right => Some(EditAction::MoveRight),
        KeyCode::Home => Some(EditAction::MoveHome),
        KeyCode::End => Some(EditAction::MoveEnd),
        KeyCode::Char(c) => Some(EditAction::Insert(c)),
        _ => None,
    }
}

/// Apply a standard text-editing key to the draft. Thin wrapper over
/// `resolve_edit_key` + `EditAction::apply`, returning `Unhandled` for keys
/// that aren't text editing so callers can layer their own handling.
fn apply_to_draft(app: &mut App, key: KeyEvent) -> DraftEffect {
    match resolve_edit_key(key) {
        Some(action) => action.apply(app),
        None => DraftEffect::Unhandled,
    }
}

fn handle_insert_normal(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            let outcome = if app.selection.editing().is_some() {
                app.save_edit();
                AddOutcome::Saved
            } else {
                app.add_from_draft()
            };
            if !matches!(outcome, AddOutcome::Parsed) {
                app.mode = Mode::Normal;
                app.draft_clear();
                app.selection.exit_edit();
            }
        }
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.draft_clear();
            app.selection.exit_edit();
        }
        KeyCode::Char('h') | KeyCode::Left => app.draft_left(),
        KeyCode::Char('l') | KeyCode::Right => app.draft_right(),
        KeyCode::Char('w') if app.chord.consume('d') => app.draft_delete_word_forward(),
        KeyCode::Char('w') if app.chord.consume('c') => {
            app.draft_delete_word_forward();
            app.draft.set_input_mode(DialogInputMode::Insert);
        }
        KeyCode::Char('w') => app.draft_word_forward(),
        KeyCode::Char('b') => app.draft_word_backward(),
        KeyCode::Char('e') => app.draft_word_end(),
        KeyCode::Char('d') => app.chord.arm('d'),
        KeyCode::Char('c') => app.chord.arm('c'),
        KeyCode::Char('x') => app.draft_delete_forward(),
        KeyCode::Char('i') => app.draft.set_input_mode(DialogInputMode::Insert),
        KeyCode::Char('a') => {
            app.draft_right();
            app.draft.set_input_mode(DialogInputMode::Insert);
        }
        KeyCode::Char('A') => {
            app.draft_end();
            app.draft.set_input_mode(DialogInputMode::Insert);
        }
        _ => {}
    }
}

fn handle_insert(app: &mut App, key: KeyEvent) {
    if app.draft.input_mode() == DialogInputMode::Normal {
        handle_insert_normal(app, key);
        return;
    }

    // Metadata-picker overlays take precedence. Non-slash overlays fully
    // consume keys until accepted or cancelled; the slash menu intercepts
    // only its navigation keys and lets text editing flow through so the
    // filter text in the buffer keeps growing as the user types.
    let overlay = app.draft.overlay().map(|o| o.kind());
    match overlay {
        Some(OverlayKind::Calendar) => {
            handle_insert_calendar(app, key);
            return;
        }
        Some(OverlayKind::RecurrenceBuilder) => {
            handle_insert_rec_builder(app, key);
            return;
        }
        Some(OverlayKind::PriorityChooser) => {
            handle_insert_priority(app, key);
            return;
        }
        Some(OverlayKind::SlashMenu) => {
            if handle_insert_slash_menu(app, key) {
                return;
            }
            // Fall through — let the key flow into the editor so filter chars
            // can be typed/erased. We re-check the overlay invariants after.
            apply_to_draft(app, key);
            // Backspacing past the `/` closes the menu; typing more chars
            // just narrows the filter.
            app.slash_menu_revalidate();
            return;
        }
        None => {}
    }

    // Autocomplete bindings take precedence — only when the popup is visible.
    // Tab accepts; Enter falls through to save so the popup never swallows the
    // submit keystroke (e.g. when the typed token already matches an existing
    // project/context). Esc with the popup open dismisses the popup but leaves
    // Insert mode intact; a second Esc enters Normal mode (handled below).
    if app.autocomplete_visible() {
        match key.code {
            KeyCode::Tab | KeyCode::Enter => {
                app.autocomplete_accept();
                app.draft.suppress_autocomplete();
                return;
            }
            _ => {
                if handle_autocomplete_keys(app, key) {
                    return;
                }
            }
        }
    }

    match key.code {
        KeyCode::Esc => {
            app.draft.set_input_mode(DialogInputMode::Normal);
        }
        KeyCode::Enter => {
            let outcome = if app.selection.editing().is_some() {
                app.save_edit();
                AddOutcome::Saved
            } else {
                app.add_from_draft()
            };
            // `Parsed` means the NL parser rewrote the draft into canonical
            // todo.txt and is asking the user to confirm — stay in Insert so
            // they can review/edit before a second Enter saves.
            if !matches!(outcome, AddOutcome::Parsed) {
                app.mode = Mode::Normal;
                app.draft_clear();
                app.selection.exit_edit();
            }
        }
        _ => {
            let before = app.draft.text().len();
            let effect = apply_to_draft(app, key);
            // `/` opens the slash menu; `:` after a recognised key
            // (`due` / `t` / `rec`) opens the matching picker directly. Both
            // detections run post-insert so they inspect what actually
            // landed in the buffer.
            if effect == DraftEffect::TextChanged && app.draft.text().len() > before {
                match key.code {
                    KeyCode::Char('/') => app.maybe_open_slash_menu(),
                    KeyCode::Char(':') => app.maybe_open_kv_overlay(),
                    _ => {}
                }
            }
        }
    }
}

/// Slash-menu key handler. Returns `true` when the key was consumed by the
/// menu (navigation, accept, dismiss); `false` when the key should fall
/// through to text editing so filter chars are typed into the buffer.
fn handle_insert_slash_menu(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Up => {
            app.slash_step(false);
            true
        }
        KeyCode::Down => {
            app.slash_step(true);
            true
        }
        KeyCode::Char('n') if ctrl => {
            app.slash_step(true);
            true
        }
        KeyCode::Char('p') if ctrl => {
            app.slash_step(false);
            true
        }
        KeyCode::Tab | KeyCode::Enter => {
            app.slash_accept();
            true
        }
        KeyCode::Esc => {
            app.slash_cancel();
            true
        }
        _ => false,
    }
}

fn handle_insert_calendar(app: &mut App, key: KeyEvent) {
    // In auto-trigger mode (anchor set): digit, dash, and backspace are
    // forwarded to the draft buffer so the user can type the date directly.
    // The calendar grid tracks the typed date as it becomes valid.
    if app.calendar_state().is_some_and(|s| s.anchor.is_some()) {
        let is_date_char = matches!(key.code, KeyCode::Char(c) if c.is_ascii_digit() || c == '-');
        if is_date_char || matches!(key.code, KeyCode::Backspace) {
            apply_to_draft(app, key);
            app.calendar_sync_from_draft();
            return;
        }
    }
    match key.code {
        KeyCode::Char('h') | KeyCode::Left => app.calendar_move(-1, 0),
        KeyCode::Char('l') | KeyCode::Right => app.calendar_move(1, 0),
        KeyCode::Char('k') | KeyCode::Up => app.calendar_move(0, -1),
        KeyCode::Char('j') | KeyCode::Down => app.calendar_move(0, 1),
        KeyCode::Char('t') => app.calendar_set_relative(0),
        KeyCode::Char('T') => app.calendar_set_relative(1),
        KeyCode::Char('w') => app.calendar_set_relative(7),
        KeyCode::Char('m') => app.calendar_add_months(1),
        KeyCode::Char('M') => app.calendar_add_months(-1),
        KeyCode::Char('x') => app.calendar_clear(),
        KeyCode::Enter => app.calendar_accept(),
        KeyCode::Esc => app.calendar_cancel(),
        _ => {}
    }
}

fn handle_insert_rec_builder(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('h') | KeyCode::Left => app.recurrence_focus(-1),
        KeyCode::Char('l') | KeyCode::Right => app.recurrence_focus(1),
        KeyCode::Char('j') | KeyCode::Down => app.recurrence_focus(1),
        KeyCode::Char('k') | KeyCode::Up => app.recurrence_focus(-1),
        // `=` is the unshifted `+` on US keyboards — accept both so users
        // don't have to chord Shift to bump the interval.
        KeyCode::Char('+') | KeyCode::Char('=') => app.recurrence_adjust(1),
        KeyCode::Char('-') | KeyCode::Char('_') => app.recurrence_adjust(-1),
        KeyCode::Enter => app.recurrence_accept(),
        KeyCode::Esc => app.recurrence_cancel(),
        _ => {}
    }
}

fn handle_insert_priority(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.priority_step(true),
        KeyCode::Char('k') | KeyCode::Up => app.priority_step(false),
        KeyCode::Enter => app.priority_accept(),
        KeyCode::Esc => app.priority_cancel(),
        _ => {}
    }
}

fn handle_search(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.draft_clear();
            app.clear_search();
        }
        KeyCode::Enter => {
            app.mode = Mode::Normal;
            app.cursor = 0;
        }
        _ => {
            if apply_to_draft(app, key) == DraftEffect::TextChanged {
                app.set_search(app.draft.text().to_string());
            }
        }
    }
}

fn handle_help(app: &mut App, key: KeyEvent) {
    if matches!(
        key.code,
        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
    ) {
        app.mode = Mode::Normal;
    }
}

fn handle_settings(app: &mut App, key: KeyEvent) {
    if matches!(
        key.code,
        KeyCode::Esc | KeyCode::Char(',') | KeyCode::Char('q')
    ) {
        app.mode = Mode::Normal;
    }
}

fn handle_pick(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.pick_step(true),
        KeyCode::Char('k') | KeyCode::Up => app.pick_step(false),
        KeyCode::Enter => app.pick_accept(),
        KeyCode::Esc => app.pick_cancel(),
        _ => {}
    }
}

fn handle_pick_theme(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.pick_theme_step(true),
        KeyCode::Char('k') | KeyCode::Up => app.pick_theme_step(false),
        KeyCode::Enter => app.pick_theme_accept(),
        KeyCode::Esc => app.pick_theme_cancel(),
        _ => {}
    }
}

fn handle_command_palette(app: &mut App, key: KeyEvent, features: ControllerFeatures) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // List navigation. Plain j/k must type into the search box — the user
    // might be searching for "jump" — so navigation goes via arrows or
    // Ctrl-N/Ctrl-P (matches the autocomplete popup in handle_insert).
    match key.code {
        KeyCode::Esc => {
            app.mode = app.command_palette.take_prior();
            app.draft_clear();
            return;
        }
        KeyCode::Enter => {
            let chosen = app.command_palette.current_action();
            // Restore the prior mode (Normal or Visual) *before* dispatching
            // so visual-aware actions (ToggleComplete, Delete, ToggleSelected)
            // see the selection. The dispatched action may then set its own
            // mode (BeginAdd → Insert, etc.); we don't stomp it after.
            app.mode = app.command_palette.take_prior();
            app.draft_clear();
            if let Some(action) = chosen {
                let _ = apply_action(app, action, features);
            }
            return;
        }
        KeyCode::Down => {
            app.command_palette.step(1);
            return;
        }
        KeyCode::Up => {
            app.command_palette.step(-1);
            return;
        }
        KeyCode::Char('n') if ctrl => {
            app.command_palette.step(1);
            return;
        }
        KeyCode::Char('p') if ctrl => {
            app.command_palette.step(-1);
            return;
        }
        _ => {}
    }
    if apply_to_draft(app, key) == DraftEffect::TextChanged {
        // `refresh` resets the cursor when the needle actually changes; a
        // same-needle call (e.g. typed-and-deleted character) is a no-op.
        app.command_palette.refresh(app.draft.text());
    }
}

fn handle_autocomplete_keys(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Up => {
            app.autocomplete_step(false);
            true
        }
        KeyCode::Down => {
            app.autocomplete_step(true);
            true
        }
        KeyCode::Char('n') if ctrl => {
            app.autocomplete_step(true);
            true
        }
        KeyCode::Char('p') if ctrl => {
            app.autocomplete_step(false);
            true
        }
        KeyCode::Esc => {
            app.draft.suppress_autocomplete();
            true
        }
        _ => false,
    }
}

fn handle_prompt(app: &mut App, key: KeyEvent) {
    if app.autocomplete_visible() {
        match key.code {
            KeyCode::Tab => {
                app.autocomplete_accept();
                return;
            }
            _ => {
                if handle_autocomplete_keys(app, key) {
                    return;
                }
            }
        }
    }

    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.draft_clear();
        }
        KeyCode::Enter => {
            let prev_mode = app.mode;
            let value = app.draft.text().to_string();
            app.draft_clear();
            app.mode = Mode::Normal;
            match prev_mode {
                Mode::PromptProject => app.add_project_to_current(&value),
                Mode::PromptContext => app.toggle_context_on_current(&value),
                Mode::PromptSaveFilter => app.save_current_filter_as(&value),
                _ => {}
            }
        }
        _ => {
            apply_to_draft(app, key);
        }
    }
}

// `Action` lives in `tuxedo::action` (see `src/action.rs`). Keeping it in the
// library lets the command palette enumerate every variant without pulling
// main.rs into the dependency graph.

/// Map a single keystroke to an `Action`. Returns `None` when the keystroke
/// is the *first* press of a chord (e.g. `g` of `gg`) or unknown — in both
/// cases there is no immediate behavior to apply.
///
/// Mutates the chord state because chord progress is part of interpreting
/// the key, not a separate concern.
fn resolve_normal_key(app: &mut App, key: KeyEvent, keybinds: &KeyBindings) -> Option<Action> {
    match keybinds.resolve_normal(key, &mut app.chord) {
        Some(ResolvedKey::Action(action)) => return Some(action),
        Some(ResolvedKey::Pending) => return None,
        None => {}
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl {
        return match key.code {
            KeyCode::Char('d') => Some(Action::HalfPageDown),
            KeyCode::Char('u') => Some(Action::HalfPageUp),
            KeyCode::Char('p') => Some(Action::OpenCommandPalette),
            _ => None,
        };
    }
    Some(match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('j') | KeyCode::Down => Action::CursorDown,
        KeyCode::Char('k') | KeyCode::Up => Action::CursorUp,
        KeyCode::Char('G') => Action::CursorBottom,
        // First 'g' arms the chord; second 'g' fires CursorTop.
        KeyCode::Char('g') if app.chord.toggle('g') => Action::CursorTop,
        KeyCode::Char('n') => Action::BeginAdd,
        KeyCode::Char('r') => Action::Reschedule,
        KeyCode::Char('a') => Action::ToggleArchiveView,
        KeyCode::Char('l') => Action::GoList,
        KeyCode::Char('e') => Action::BeginEdit,
        KeyCode::Char('i') => Action::BeginEditInsert,
        KeyCode::Char('o') => Action::OpenNote,
        KeyCode::Char('O') => Action::CreateOrOpenNote,
        KeyCode::Char('x') => Action::ToggleComplete,
        // 'dd' chord. First press arms; second fires.
        KeyCode::Char('d') if app.chord.toggle('d') => Action::Delete,
        // 'yy' chord copies the whole line; 'yb' (after 'y' is armed) copies
        // the body only. Plain 'y' just arms the leader.
        KeyCode::Char('y') if app.chord.toggle('y') => Action::CopyLine,
        KeyCode::Char('b') if app.chord.consume('y') => Action::CopyBody,
        KeyCode::Char('p') => {
            // After 'f' arms, 'fp' opens the project picker. Otherwise plain
            // 'p' cycles priority.
            if app.chord.consume('f') {
                Action::PickProject
            } else {
                Action::CyclePriority
            }
        }
        KeyCode::Char('c') => {
            if app.chord.consume('f') {
                Action::PickContext
            } else {
                Action::BeginPromptContext
            }
        }
        KeyCode::Char('/') => Action::BeginSearch,
        KeyCode::Char('?') => Action::OpenHelp,
        KeyCode::Char(',') => Action::OpenSettings,
        KeyCode::Char(':') => Action::OpenCommandPalette,
        KeyCode::Char('u') => Action::Undo,
        KeyCode::Char('v') => Action::ToggleVisual,
        KeyCode::Char(' ') => Action::ToggleSelected,
        // First 'f' arms the leader; a second 'f' (`ff`) opens the saved-
        // search picker. Mirrors the `fp`/`fc` pattern below.
        KeyCode::Char('f') => {
            if app.chord.consume('f') {
                Action::PickSavedFilter
            } else {
                Action::ArmF
            }
        }
        KeyCode::Char('s') => {
            // `fs` saves the active search; plain 's' opens the share QR.
            if app.chord.consume('f') {
                Action::SaveCurrentFilter
            } else {
                Action::OpenShare
            }
        }
        KeyCode::Char('S') => Action::CycleSort,
        KeyCode::Char('+') => Action::BeginPromptProject,
        KeyCode::Char('[') => Action::ToggleLeftPane,
        KeyCode::Char(']') => Action::ToggleRightPane,
        KeyCode::Char('T') => Action::OpenThemePicker,
        KeyCode::Char('D') => Action::CycleDensity,
        KeyCode::Char('L') => Action::ToggleLineNum,
        KeyCode::Char('F') => Action::ToggleShowFuture,
        KeyCode::Esc => Action::EscapeStack,
        KeyCode::Char('W') => Action::ChangeWeekStart,
        _ => return None,
    })
}

fn apply_action(app: &mut App, action: Action, features: ControllerFeatures) -> bool {
    // Archive view is read-only with one exception: `dd` restores the row at
    // the cursor to the live list (state preserved). `x` is deliberately a
    // no-op here — there's no complete/uncomplete concept in the archive.
    // Every other mutating action flashes a hint and aborts. Navigation,
    // view-switch, theme/density/layout toggles, and overlays (help/settings)
    // fall through to the normal handler below.
    if app.view() == View::Archive {
        match action {
            Action::Delete => {
                if let Some(idx) = app.cur_abs() {
                    app.unarchive(idx);
                }
                return false;
            }
            Action::ToggleComplete => {
                app.flash("x disabled in archive · dd restores");
                return false;
            }
            Action::BeginAdd
            | Action::BeginEdit
            | Action::BeginEditInsert
            | Action::CyclePriority
            | Action::ToggleVisual
            | Action::ToggleSelected
            | Action::BeginSearch
            | Action::BeginPromptProject
            | Action::BeginPromptContext
            | Action::PickProject
            | Action::PickContext
            | Action::PickSavedFilter
            | Action::SaveCurrentFilter
            | Action::CycleSort
            | Action::ToggleShowFuture
            | Action::Undo => {
                app.flash("read-only in archive");
                return false;
            }
            _ => {}
        }
    }
    let len = app.visible_indices().len();
    match action {
        Action::Quit => return true,
        Action::CursorDown => {
            if len > 0 {
                app.cursor = (app.cursor + 1).min(len - 1);
            }
        }
        Action::CursorUp => app.cursor = app.cursor.saturating_sub(1),
        Action::CursorTop => app.cursor = 0,
        Action::CursorBottom => {
            if len > 0 {
                app.cursor = len - 1;
            }
        }
        Action::HalfPageDown => {
            app.cursor = (app.cursor + 10).min(len.saturating_sub(1));
        }
        Action::HalfPageUp => app.cursor = app.cursor.saturating_sub(10),
        Action::BeginAdd => {
            app.mode = Mode::Insert;
            app.draft_clear();
            app.selection.exit_edit();
        }
        Action::BeginEdit => {
            if let Some(abs) = app.cur_abs()
                && let Some(raw) = app.task_raw(abs)
            {
                app.selection.enter_edit(abs);
                app.draft_set(raw);
                app.mode = Mode::Insert;
            }
        }
        Action::BeginEditInsert => {
            if let Some(abs) = app.cur_abs()
                && let Some(raw) = app.task_raw(abs)
            {
                app.selection.enter_edit(abs);
                app.draft_set_insert(raw);
                app.mode = Mode::Insert;
            }
        }
        Action::ToggleComplete => {
            if app.mode == Mode::Visual && !app.selection.is_empty() {
                app.complete_selected();
            } else if let Some(abs) = app.cur_abs() {
                app.toggle_complete(abs);
            }
        }
        Action::Delete => {
            if app.mode == Mode::Visual && !app.selection.is_empty() {
                app.archive_selected();
            } else if let Some(abs) = app.cur_abs() {
                app.archive_at(abs);
            }
        }
        Action::CyclePriority => {
            if let Some(abs) = app.cur_abs() {
                app.cycle_priority(abs);
            }
        }
        Action::BeginSearch => {
            app.mode = Mode::Search;
            app.draft_clear();
            app.clear_search();
        }
        Action::OpenHelp => app.mode = Mode::Help,
        Action::OpenSettings if features.config => app.mode = Mode::Settings,
        Action::OpenSettings => app.flash("settings unavailable when embedded"),
        Action::OpenCommandPalette => {
            // Snapshot the current mode (Normal or Visual) so cancel/run
            // can restore it — otherwise opening the palette from Visual
            // and cancelling silently exits Visual.
            let prior = app.mode;
            app.command_palette.open(prior);
            app.mode = Mode::CommandPalette;
            app.draft_clear();
        }
        Action::Undo => app.undo(),
        Action::ToggleVisual => {
            app.mode = if app.mode == Mode::Visual {
                Mode::Normal
            } else {
                Mode::Visual
            };
        }
        Action::ToggleSelected => {
            if app.mode == Mode::Visual
                && let Some(abs) = app.cur_abs()
            {
                app.selection.toggle(abs);
            }
        }
        Action::GoList => app.set_view(View::List),
        Action::ToggleArchiveView => {
            let next = if app.view() == View::Archive {
                View::List
            } else {
                View::Archive
            };
            app.set_view(next);
        }
        Action::ArmF => app.chord.arm('f'),
        Action::PickProject => app.enter_pick_project(),
        Action::PickContext => app.enter_pick_context(),
        Action::PickSavedFilter if features.config => app.enter_pick_saved(),
        Action::PickSavedFilter => app.flash("saved filters unavailable when embedded"),
        Action::SaveCurrentFilter if !features.config => {
            app.flash("saved filters unavailable when embedded")
        }
        Action::SaveCurrentFilter => {
            if app.filter().search.is_empty() {
                app.flash("no active search to save");
            } else {
                app.mode = Mode::PromptSaveFilter;
                app.draft_clear();
            }
        }
        // Sort is a view preference (like cursor position) so it stays
        // available even in the embedded pane where broader config writes
        // are gated off. Persistence still flows through the shared XDG
        // config when `config` is on, so standalone Tuxedo picks it up.
        Action::CycleSort => {
            let msg = app.prefs.cycle_sort();
            app.flash(msg);
            app.recompute_visible();
            if features.config {
                app.save_prefs();
            }
        }
        Action::BeginPromptProject => {
            app.mode = Mode::PromptProject;
            app.draft_clear();
        }
        Action::BeginPromptContext => {
            app.mode = Mode::PromptContext;
            app.draft_clear();
        }
        Action::ToggleLeftPane if !features.config => {
            app.flash("layout settings unavailable when embedded")
        }
        Action::ToggleLeftPane => {
            app.prefs.toggle_left();
            app.save_prefs();
        }
        Action::ToggleRightPane if !features.config => {
            app.flash("layout settings unavailable when embedded")
        }
        Action::ToggleRightPane => {
            app.prefs.toggle_right();
            app.save_prefs();
        }
        Action::CycleTheme if features.theme => app.cycle_theme(),
        Action::CycleTheme => app.flash("theme is controlled by the host"),
        Action::CycleDensity if !features.config => {
            app.flash("density settings unavailable when embedded")
        }
        Action::CycleDensity => app.cycle_density(),
        Action::ToggleLineNum if !features.config => {
            app.flash("layout settings unavailable when embedded")
        }
        Action::ToggleLineNum => {
            app.prefs.toggle_line_num();
            app.save_prefs();
        }
        Action::ToggleShowFuture if !features.config => {
            app.flash("display settings unavailable when embedded")
        }
        Action::ToggleShowFuture => {
            app.prefs.toggle_show_future();
            app.cursor = 0;
            app.recompute_visible();
            app.save_prefs();
        }
        Action::CopyLine if features.clipboard => copy_current_task(app, false),
        Action::CopyBody if features.clipboard => copy_current_task(app, true),
        Action::CopyLine | Action::CopyBody => app.flash("clipboard unavailable when embedded"),
        Action::OpenNote if features.notes => app.open_note_for_current(),
        Action::OpenNote => app.flash("notes unavailable when embedded"),
        Action::CreateOrOpenNote if features.notes => app.create_or_open_note_for_current(),
        Action::CreateOrOpenNote => app.flash("notes unavailable when embedded"),
        Action::OpenShare if features.share => match app.ensure_share_started() {
            Ok(_) => {
                app.mode = Mode::Share;
            }
            Err(e) => app.flash(format!("share unavailable: {e}")),
        },
        Action::OpenShare => app.flash("share unavailable when embedded"),
        Action::OpenThemePicker if features.theme => {
            if theme::all().len() <= 1 {
                app.flash("only one theme");
            } else {
                app.enter_pick_theme();
            }
        }
        Action::OpenThemePicker => app.flash("theme is controlled by the host"),
        Action::EscapeStack => {
            let has_pc = app.filter().project.is_some() || app.filter().context.is_some();
            let has_search = !app.filter().search.is_empty();
            if has_pc {
                app.set_project_filter(None);
                app.set_context_filter(None);
            } else if has_search {
                app.draft_clear();
                app.clear_search();
            } else if !app.selection.is_empty() {
                app.selection.clear();
            } else if app.mode == Mode::Visual {
                app.mode = Mode::Normal;
            } else if app.view() != View::List {
                app.set_view(View::List);
            }
        }
        // Opens to insert mode just like i/e but with the calendar open and the cursor on the calendar
        // If there is a due date, the cursor begins on the current due date
        // If there is no due date, the cursor begins on today
        // Enter/escape takes the user back to insert mode on the task
        Action::Reschedule => {
            if let Some(abs) = app.cur_abs()
                && let Some(raw) = app.task_raw(abs)
            {
                app.selection.enter_edit(abs);
                app.draft_set_insert(raw);
                app.mode = Mode::Insert;
                app.open_calendar(CalendarTarget::Due);
            }
        }
        Action::ChangeWeekStart if features.config => {
            app.toggle_week_start_date();
            app.recompute_visible();
        }
        Action::ChangeWeekStart => app.flash("calendar settings unavailable when embedded"),
    }
    false
}

fn handle_normal(
    app: &mut App,
    key: KeyEvent,
    keybinds: &KeyBindings,
    features: ControllerFeatures,
) -> bool {
    let exit = resolve_normal_key(app, key, keybinds)
        .is_some_and(|action| apply_action(app, action, features));
    app.clamp_cursor();
    exit
}

fn copy_current_task(app: &mut App, body_only: bool) {
    let Some(raw) = app.cur_task().map(|t| t.raw.clone()) else {
        return;
    };
    let payload = if body_only {
        todo::body_only(&raw)
    } else {
        raw
    };
    match clipboard::copy(&payload) {
        Ok(()) => app.flash(if body_only { "copied (body)" } else { "copied" }),
        Err(e) => app.flash(format!("copy failed: {e}")),
    }
}
