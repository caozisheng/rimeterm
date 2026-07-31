#![warn(clippy::unwrap_used)]

use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use std::io::Write;

use tuxedo::app::{App, Mode};
use tuxedo::cli;
use tuxedo::config::Config;
use tuxedo::config_watcher;
use tuxedo::controller::{self, ControllerFeatures};
use tuxedo::keybinds::KeyBindings;
use tuxedo::theme;
use tuxedo::ui::hyperlinks;
use tuxedo::{ui, update};

const EVENT_POLL: Duration = Duration::from_millis(250);

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // A recognized subcommand (possibly preceded by `-f`/`--json`) runs the
    // one-shot CLI and exits; otherwise we fall through to the TUI.
    if let Some(code) = tuxedo::cmd::run(&argv)? {
        std::process::exit(code);
    }
    let arg = argv.first().cloned();
    // `start_mode` is `Welcome` only on a true first run (no target and no
    // ./todo.txt); every other entry opens straight into Normal.
    let (path, start_mode) = match arg.as_deref() {
        Some("--help") | Some("-h") => {
            print_usage();
            return Ok(());
        }
        Some("--version") | Some("-V") => {
            println!("tuxedo {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("update") => {
            update::run()?;
            return Ok(());
        }
        Some("--sample") => (cli::sample_path()?, Mode::Normal),
        Some(s) if s.starts_with('-') => {
            eprintln!("tuxedo: unknown option: {s}");
            eprintln!("try `tuxedo --help`");
            std::process::exit(2);
        }
        _ => match cli::resolve_target(arg)? {
            cli::Target::File(p) => (p, Mode::Normal),
            // Open into the welcome prompt backed by an as-yet-uncreated
            // ./todo.txt; `handle_welcome` materializes the file the user picks.
            cli::Target::FirstRun => (std::path::PathBuf::from("todo.txt"), Mode::Welcome),
        },
    };
    // A freshly-created file is empty; otherwise read it. We accept NotFound
    // (race with deletion between resolve_path and now) as "empty file" but
    // refuse to silently swallow other IO errors — an unreadable or non-UTF-8
    // file would otherwise present as an empty editor that, on first save,
    // overwrites the user's data.
    let body = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", path.display()));
        }
    };
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let cfg = Config::load();
    let keybinds = KeyBindings::load();
    // Load user-supplied themes before constructing App, so the theme named
    // in cfg can resolve to a custom theme on the first `Prefs::from_config`.
    let theme_warnings = match theme::themes_dir() {
        Some(dir) => {
            let (user_themes, warnings) = theme::load_user_themes(&dir);
            theme::init(user_themes);
            warnings
        }
        None => {
            theme::init(Vec::new());
            Vec::new()
        }
    };
    let done = cli::done_path(&path);
    let mut app_state = App::new_with_done(path.clone(), done, body, today, cfg);
    app_state.config_path = Config::path();
    app_state.mode = start_mode;
    // Start the config hot-reload watcher.
    let config_rx = app_state
        .config_path
        .as_ref()
        .and_then(|p| config_watcher::spawn(p.clone()));
    // Surface theme-load problems on the first frame. Flash is single-line,
    // so collapse multiple warnings to a count and let the user investigate
    // their themes directory.
    match theme_warnings.len() {
        0 => {}
        1 => app_state.flash(theme_warnings.into_iter().next().expect("len==1")),
        n => app_state.flash(format!(
            "{n} theme(s) skipped — check ~/.config/tuxedo/themes/"
        )),
    }
    if std::env::var_os("TUXEDO_NO_UPDATE_CHECK").is_none() {
        app_state.set_update_check(update::spawn_check());
    }

    let terminal = ratatui::init();
    // Give the window/tab a consistent `tuxedo <path>` title across terminals
    // and operating systems, shortening long paths to fit a fixed budget.
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let title = ui::title::terminal_title(&path, home.as_deref(), ui::title::DEFAULT_BUDGET);
    let _ = crossterm::execute!(io::stdout(), crossterm::terminal::SetTitle(title));
    let result = run(terminal, &mut app_state, &keybinds, config_rx);
    ratatui::restore();
    // Clear the title on exit so the shell retitles on its next prompt rather
    // than leaving `tuxedo …` behind.
    let _ = crossterm::execute!(io::stdout(), crossterm::terminal::SetTitle(""));
    // Print the file path *after* restoring the terminal so the message
    // survives in the user's scrollback rather than being eaten by the
    // alt-screen. Read it back from the app: the welcome prompt may have
    // rebound to the sample. Skip the line if the user quit the welcome
    // prompt without choosing — no file was opened.
    if app_state.mode != Mode::Welcome {
        eprintln!("tuxedo: {}", app_state.file_path.display());
    }
    result
}

fn print_usage() {
    println!("usage: tuxedo [FILE]                 launch the TUI");
    println!("       tuxedo <command> [args]       run a one-shot command");
    println!("       tuxedo update");
    println!();
    println!("Without FILE or a command, opens ./todo.txt if present; otherwise");
    println!("prompts to create ./todo.txt here or open a sample todo.txt, in");
    println!("the interactive TUI.");
    println!();
    println!("Inside the TUI, press `s` to expose a phone-friendly capture");
    println!("endpoint on your LAN and show a QR code for it. Captures land");
    println!("in a sibling inbox.txt that the TUI merges on the next poll.");
    println!();
    println!("Commands (task numbers are 1-based file lines, as shown by `list`):");
    println!("  add, a TEXT...            add a task (natural-language dates supported)");
    println!("  append, app N TEXT...     append text to task N");
    println!("  prepend, prep N TEXT...   prepend text to task N");
    println!("  replace N TEXT...         replace task N");
    println!("  pri, p N PRIORITY         set priority A-Z on task N");
    println!("  depri, dp N...            remove priority from task N");
    println!("  done, do N...             mark task N complete");
    println!("  del, rm N [TERM]          delete task N (prompts; -f to force), or remove TERM");
    println!("  archive                   move completed tasks to done.txt");
    println!("  list, ls [TERM...]        list tasks (TERM: +project @context or text)");
    println!("  listall, lsa [TERM...]    list todo.txt and done.txt");
    println!("  listpri, lsp [PRIORITY]   list prioritized tasks");
    println!("  listproj, lsprj           list +projects");
    println!("  listcon, lsc              list @contexts");
    println!("  update                    print instructions for upgrading tuxedo");
    println!();
    println!("Options:");
    println!("  -f, --force      skip confirmation prompts (e.g. for del)");
    println!("      --json       machine-readable output for the commands above");
    println!("  -h, --help       show this message and exit");
    println!("  -V, --version    print version and exit");
    println!("      --sample     open the sample todo.txt in the TUI");
    println!();
    println!("Environment:");
    println!("  TODO_DIR     directory holding todo.txt / done.txt");
    println!("  TODO_FILE    path to the todo file (default $TODO_DIR/todo.txt)");
    println!("  DONE_FILE    path to the archive file (default sibling done.txt)");
}

fn run(
    mut terminal: DefaultTerminal,
    app: &mut App,
    keybinds: &KeyBindings,
    config_rx: Option<mpsc::Receiver<()>>,
) -> Result<()> {
    let mut dirty = true;
    while !app.should_quit {
        // Pick up midnight rollover so threshold-hidden tasks reveal
        // themselves without requiring an app restart.
        if app.refresh_today(chrono::Local::now().format("%Y-%m-%d").to_string()) {
            dirty = true;
        }
        // Drain the startup archive loader (and pick up external edits to
        // done.txt). Non-blocking: the first frame can render todo.txt
        // before the archive read completes.
        if app.poll_archive() {
            dirty = true;
        }
        // Pick up the update-check result so the status-bar indicator can
        // appear without waiting for a keystroke.
        if app.poll_update_check() {
            dirty = true;
        }
        // Poll the config hot-reload watcher. On signal, reload strictly
        // and apply the new prefs. On parse failure the old config stays
        // intact and a warning is flashed.
        if poll_config_reload(app, &config_rx) {
            dirty = true;
        }
        if dirty {
            // Extract URL runs from the completed frame before the borrow on
            // terminal ends, then write the OSC 8 overlay directly to the
            // backend writer. Doing this here (rather than inside `ui::draw`)
            // keeps cell symbols byte-identical to a plain render, so
            // ratatui's diff width calculation doesn't skip cells past the
            // URL — see `ui::hyperlinks` for the full explanation.
            let runs = {
                let frame = terminal.draw(|f| ui::draw(f, app))?;
                hyperlinks::collect(frame.buffer)
            };
            if !runs.is_empty() {
                let backend = terminal.backend_mut();
                hyperlinks::emit_overlay(backend, &runs)?;
                backend.flush()?;
            }
            dirty = false;
        }
        let timeout = next_timeout(app);
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if controller::handle_key(app, key, keybinds, ControllerFeatures::STANDALONE)
                        == controller::ControllerOutcome::ExitRequested
                    {
                        app.should_quit = true;
                    }
                    if let Some(path) = app.take_pending_editor_path() {
                        open_path_in_editor(&path)?;
                    }
                    dirty = true;
                }
                // A terminal resize must trigger an immediate redraw;
                // otherwise the screen stays stale until the next keystroke.
                Event::Resize(_, _) => {
                    dirty = true;
                }
                _ => {}
            }
        } else if !app.check_external_changes() {
            // Idle tick — file changed under us; reload was performed.
            dirty = true;
        }
        if controller::clear_expired(app) {
            dirty = true;
        }
    }
    Ok(())
}

/// Poll the config watcher channel. On signal, reload config strictly and
/// apply it to the app. Returns `true` when a reload was attempted (whether
/// successful or not) so the caller can trigger a redraw.
fn poll_config_reload(app: &mut App, rx: &Option<mpsc::Receiver<()>>) -> bool {
    let rx = match rx {
        Some(r) => r,
        None => return false,
    };
    match rx.try_recv() {
        Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {}
        Err(mpsc::TryRecvError::Empty) => return false,
    }
    let Some(ref path) = app.config_path else {
        return true;
    };
    match Config::load_strict(path) {
        Ok(new_cfg) => {
            app.reload_config(new_cfg);
            app.flash("config reloaded");
            true
        }
        Err(e) => {
            app.flash(format!("config reload failed: {e}"));
            true
        }
    }
}

fn open_path_in_editor(path: &std::path::Path) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "nvim".to_string());
    ratatui::restore();
    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("failed to launch editor `{editor}`"));
    ratatui::crossterm::terminal::enable_raw_mode()?;
    ratatui::crossterm::execute!(
        io::stdout(),
        ratatui::crossterm::terminal::EnterAlternateScreen
    )?;
    match status {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

fn next_timeout(app: &App) -> Duration {
    controller::next_deadline(app)
        .map(|deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .min(EVENT_POLL)
        })
        .unwrap_or(EVENT_POLL)
}
