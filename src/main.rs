//! pkgsync — interactively diff & sync packages between two Arch machines.
//!
//! Stage 8: sources live inside the TUI. You pick a source from a list, the
//! fetch runs on a background thread (so SSH never freezes the UI), and the
//! event loop polls instead of blocking — animating a spinner and draining the
//! worker's result over a channel. Applying changes refreshes in place.

use pkgsync::action::Plan;
use pkgsync::app::{App, InputKind, MenuItem, Mode, Screen};
use pkgsync::diff::{diff, Category, DiffEntry, DiffKind};
use pkgsync::known;
use pkgsync::source::{discover, LocalSource, Source, SourceSpec};
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::{Constraint, Flex, Layout, Position, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, Paragraph},
    DefaultTerminal, Frame,
};
use std::io::Write;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const USAGE: &str = "\
pkgsync — diff this machine's packages against another's, interactively

USAGE:
    pkgsync <target>...   one or more comparison targets, then pick in-app
    pkgsync demo          run with sample data (no machines needed)

Each <target> is either:
    - a path to a .pkgs state file        -> that file
    - a directory                         -> every *.pkgs file inside it
    - anything else                       -> an SSH host (ssh <host> pacman -Qe)

Example:
    pkgsync ~/dotconfigs/state office     # pick from state files or live SSH";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let app = match args.first().map(String::as_str) {
        // No targets: open the interactive menu, seeded with remembered sources
        // (recents + ssh_config hosts).
        None => App::interactive(known::menu_sources()),
        Some("-h") | Some("--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("demo") => App::demo(demo_diff()),
        Some(_) => {
            let specs = discover(&args);
            if specs.is_empty() {
                eprintln!("no comparison targets found in: {}", args.join(" "));
                return ExitCode::from(2);
            }
            App::new(specs)
        }
    };

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, app);
    ratatui::restore();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(terminal: &mut DefaultTerminal, mut app: App) -> std::io::Result<()> {
    // Channel carrying (fetch epoch, result) from worker threads to the UI.
    // The epoch lets the app drop results from cancelled/superseded fetches.
    let (tx, rx) = mpsc::channel::<(u64, Result<Vec<DiffEntry>, String>)>();

    while !app.should_quit {
        terminal.draw(|frame| draw(frame, &mut app))?;

        // Non-blocking input: poll with a timeout so the loop keeps spinning to
        // animate the spinner and to drain the channel while a fetch runs.
        if event::poll(Duration::from_millis(120))? {
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                // Ctrl-C always quits, even mid-typing (where 'q' is just text).
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    break;
                }
                app.handle_key(key.code);
            }
        } else {
            app.tick(); // no input this cycle -> advance the spinner
        }

        // If a fetch was requested, run it on a worker thread. The spec is
        // `Send + 'static`, so it moves into the thread cleanly; the result
        // comes back over the channel.
        if let Some(spec) = app.take_fetch_request() {
            known::record(&spec); // remember it for next time's menu
            let tx = tx.clone();
            let epoch = app.current_epoch();
            thread::spawn(move || {
                let _ = tx.send((epoch, fetch_diff(&spec)));
            });
        }

        // Drain any completed fetches.
        while let Ok((epoch, result)) = rx.try_recv() {
            app.on_fetch_result(epoch, result);
        }

        // Apply confirmed changes: suspend the TUI, run yay, then refresh.
        if app.take_apply_request() {
            let plan = Plan::from_selection(&app.entries, &app.selected);
            if !plan.is_empty() {
                suspend_and_run(terminal, &plan)?;
                app.refresh(); // re-fetch so the diff reflects what changed
            }
        }
    }
    Ok(())
}

/// Run on a worker thread: gather local + remote package lists and diff them.
/// Errors are stringified here because they cross a thread boundary.
fn fetch_diff(spec: &SourceSpec) -> Result<Vec<DiffEntry>, String> {
    let local = LocalSource.fetch().map_err(|e| format!("local: {e}"))?;
    let remote = spec.fetch().map_err(|e| format!("remote: {e}"))?;
    Ok(diff(&local, &remote))
}

/// Leave the TUI, run the plan with the real terminal attached (so yay's output
/// and sudo prompt work), wait for the user, then re-enter the TUI.
fn suspend_and_run(terminal: &mut DefaultTerminal, plan: &Plan) -> std::io::Result<()> {
    ratatui::restore();

    let _ = plan.execute(); // failures are printed by execute itself

    print!("\n[pkgsync] press Enter to return…");
    std::io::stdout().flush()?;
    std::io::stdin().read_line(&mut String::new())?;

    *terminal = ratatui::init();
    terminal.clear()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn draw(frame: &mut Frame, app: &mut App) {
    // Clone the screen tag (cheap; only Error carries a String) so we don't
    // hold an immutable borrow of `app` while the arms borrow it mutably.
    match app.screen.clone() {
        Screen::Menu => render_menu(frame, app),
        Screen::Input => render_input(frame, app),
        Screen::Picker => render_picker(frame, app),
        Screen::Loading => render_loading(frame, app),
        Screen::Diff => render_diff(frame, app),
        Screen::Error(message) => render_error(frame, &message),
    }
}

fn render_menu(frame: &mut Frame, app: &mut App) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());

    let items: Vec<ListItem> = app
        .menu_items
        .iter()
        .map(|item| {
            let line = match item {
                // A remembered source: show its label in cyan.
                MenuItem::Source(spec) => Line::from(vec![
                    Span::raw("  "),
                    Span::styled(spec.label(), Style::new().fg(Color::Cyan)),
                ]),
                MenuItem::NewSsh => {
                    Line::from("+ SSH — enter a host / IP".to_string()).dim()
                }
                MenuItem::NewFile => {
                    Line::from("+ Local file — enter a path".to_string()).dim()
                }
            };
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(Block::bordered().title(" pkgsync — choose a source "))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, body, &mut app.menu_state);

    let help = Line::from(vec![
        hotkey("↑↓"),
        Span::raw(" move  "),
        hotkey("Enter"),
        Span::raw(" select  "),
        hotkey("q"),
        Span::raw(" quit"),
    ]);
    frame.render_widget(Paragraph::new(help), footer);
}

fn render_input(frame: &mut Frame, app: &App) {
    let (title, prompt, placeholder) = match app.input_kind {
        InputKind::Ssh => (
            " SSH host ",
            "Hostname or IP address:",
            "e.g. office   or   100.74.x.y",
        ),
        InputKind::File => (
            " local file ",
            "Path to a .pkgs file:",
            "e.g. ~/dev/linux/dotconfigs/state/office.pkgs",
        ),
    };

    let area = centered_rect(frame.area(), 72, 6);

    // Show the live buffer, or a dim placeholder when it's empty.
    let value_line = if app.input.is_empty() {
        Line::from(vec![Span::raw("> "), Span::styled(placeholder, Style::new().dim())])
    } else {
        Line::from(format!("> {}", app.input))
    };
    let lines = vec![
        Line::from(prompt),
        value_line,
        Line::from(""),
        Line::from("Enter connect · Esc back".dim()),
    ];

    let popup = Paragraph::new(lines).block(Block::bordered().title(title));
    frame.render_widget(Clear, area);
    frame.render_widget(popup, area);

    // Place a real terminal cursor right after the typed text. Inside the
    // border (+1,+1); the value is line index 1; "> " is a 2-column prefix.
    let cursor_x = (area.x + 1 + 2 + app.input.chars().count() as u16)
        .min(area.x + area.width.saturating_sub(2));
    let cursor_y = area.y + 2;
    frame.set_cursor_position(Position::new(cursor_x, cursor_y));
}

fn render_picker(frame: &mut Frame, app: &mut App) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());

    let items: Vec<ListItem> = app.specs.iter().map(|s| ListItem::new(s.label())).collect();
    let list = List::new(items)
        .block(Block::bordered().title(" compare against — pick a source "))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, body, &mut app.picker_state);

    let help = Line::from(vec![
        hotkey("↑↓"),
        Span::raw(" move  "),
        hotkey("Enter"),
        Span::raw(" choose  "),
        hotkey("q"),
        Span::raw(" quit"),
    ]);
    frame.render_widget(Paragraph::new(help), footer);
}

fn render_loading(frame: &mut Frame, app: &App) {
    let lines = vec![
        Line::from(format!(
            "{}  fetching from {} …",
            app.spinner_frame(),
            app.loading_label()
        )),
        Line::from(""),
        Line::from("SSH can take a few seconds · Esc to cancel".dim()),
    ];
    let area = centered_rect(frame.area(), 64, 5);
    let popup = Paragraph::new(lines)
        .centered()
        .block(Block::bordered().title(" loading "));
    frame.render_widget(Clear, area);
    frame.render_widget(popup, area);
}

fn render_error(frame: &mut Frame, message: &str) {
    let lines = vec![
        Line::from("fetch failed".bold().red()),
        Line::from(""),
        Line::from(message.to_string()),
        Line::from(""),
        Line::from("press any key for the source list · q to quit".dim()),
    ];
    let area = centered_rect(frame.area(), 70, lines.len() as u16 + 2);
    let popup = Paragraph::new(lines)
        .block(Block::bordered().title(" error ").border_style(Style::new().fg(Color::Red)));
    frame.render_widget(Clear, area);
    frame.render_widget(popup, area);
}

fn render_diff(frame: &mut Frame, app: &mut App) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let [list_area, detail_area] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);

    render_list(frame, list_area, app);
    render_detail(frame, detail_area, app);
    render_diff_footer(frame, footer, app);

    if app.mode == Mode::Confirm {
        render_confirm(frame, app);
    }
}

fn render_list(frame: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .visible()
        .iter()
        .map(|entry| diff_item(entry, app.is_selected(&entry.name)))
        .collect();

    let title = summary_title(app);
    let list = List::new(items)
        .block(Block::bordered().title(title))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("› ");

    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn diff_item(entry: &DiffEntry, selected: bool) -> ListItem<'static> {
    let (symbol, action, color, detail) = match &entry.kind {
        DiffKind::Missing { remote_version } => {
            ("+", "install", Color::Green, format!("remote {remote_version}"))
        }
        DiffKind::Extra { local_version } => {
            ("-", "remove", Color::Red, format!("local {local_version}"))
        }
        DiffKind::VersionSkew { local_version, remote_version } => (
            "~",
            "upgrade",
            Color::Yellow,
            format!("{local_version} -> {remote_version}"),
        ),
    };

    let checkbox = if selected { "[x] " } else { "[ ] " };
    let line = Line::from(vec![
        Span::raw(checkbox),
        Span::styled(format!("{symbol} "), Style::new().fg(color)),
        Span::styled(format!("{action:<8}"), Style::new().fg(color).bold()),
        Span::raw(format!("{:<22}", entry.name)),
        Span::raw(detail).dim(),
    ]);
    ListItem::new(line)
}

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let lines = match app.selected_entry() {
        Some(entry) => detail_lines(entry, app.is_selected(&entry.name)),
        None => vec![Line::from("— nothing here —".dim())],
    };
    let detail = Paragraph::new(lines).block(Block::bordered().title(" detail "));
    frame.render_widget(detail, area);
}

fn detail_lines(entry: &DiffEntry, selected: bool) -> Vec<Line<'static>> {
    let (action, color, versions): (&str, Color, Vec<Line>) = match &entry.kind {
        DiffKind::Missing { remote_version } => (
            "install",
            Color::Green,
            vec![Line::from(format!("remote version : {remote_version}"))],
        ),
        DiffKind::Extra { local_version } => (
            "remove",
            Color::Red,
            vec![Line::from(format!("local version  : {local_version}"))],
        ),
        DiffKind::VersionSkew { local_version, remote_version } => (
            "upgrade",
            Color::Yellow,
            vec![
                Line::from(format!("local version  : {local_version}")),
                Line::from(format!("remote version : {remote_version}")),
            ],
        ),
    };

    let mut lines = vec![
        Line::from(entry.name.clone().bold()),
        Line::from(Span::styled(action, Style::new().fg(color).bold())),
        Line::from(""),
    ];
    lines.extend(versions);
    lines.push(Line::from(""));
    lines.push(Line::from(if selected {
        "✓ selected for action".green()
    } else {
        "· not selected".dim()
    }));
    lines
}

fn render_diff_footer(frame: &mut Frame, area: Rect, app: &App) {
    let help = Line::from(vec![
        hotkey("↑↓"),
        Span::raw(" move  "),
        hotkey("Tab"),
        Span::raw(" select  "),
        hotkey("a/i/u/r"),
        Span::raw(" filter  "),
        hotkey("Enter"),
        Span::raw(" apply  "),
        hotkey("R"),
        Span::raw(" reload  "),
        hotkey("Esc"),
        Span::raw(" sources  "),
        hotkey("q"),
        Span::raw(" quit  "),
        Span::styled(
            format!("[{}] [sel {}]", app.filter.label(), app.selected.len()),
            Style::new().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(help), area);
}

fn render_confirm(frame: &mut Frame, app: &App) {
    let plan = Plan::from_selection(&app.entries, &app.selected);

    let mut lines = vec![
        Line::from(format!("About to act on {} package(s):", plan.total()).bold()),
        Line::from(""),
    ];
    for cmd in plan.commands() {
        lines.push(Line::from(vec![
            Span::raw("  $ "),
            Span::styled(cmd.display(), Style::new().fg(Color::Cyan)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" y ", Style::new().fg(Color::Black).bg(Color::Green)),
        Span::raw(" apply    "),
        Span::styled(" n ", Style::new().fg(Color::Black).bg(Color::Red)),
        Span::raw(" cancel"),
    ]));

    let area = centered_rect(frame.area(), 70, lines.len() as u16 + 2);
    let popup = Paragraph::new(lines)
        .block(Block::bordered().title(" confirm ").border_style(Style::new().fg(Color::Yellow)));
    frame.render_widget(Clear, area);
    frame.render_widget(popup, area);
}

/// A small key-hint chip used in the footers.
fn hotkey(k: &'static str) -> Span<'static> {
    Span::styled(format!(" {k} "), Style::new().fg(Color::Black).bg(Color::Gray))
}

fn summary_title(app: &App) -> String {
    let (install, upgrade, remove) = counts(&app.entries);
    format!(" diff — {install} install · {upgrade} upgrade · {remove} remove ")
}

fn counts(entries: &[DiffEntry]) -> (usize, usize, usize) {
    let mut install = 0;
    let mut upgrade = 0;
    let mut remove = 0;
    for e in entries {
        match e.kind.category() {
            Category::Install => install += 1,
            Category::Upgrade => upgrade += 1,
            Category::Remove => remove += 1,
        }
    }
    (install, upgrade, remove)
}

/// A rectangle centered in `area`, `percent_x` wide and `height` rows tall.
fn centered_rect(area: Rect, percent_x: u16, height: u16) -> Rect {
    let [horizontal] = Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .areas(area);
    let [centered] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(horizontal);
    centered
}

/// Sample data for `pkgsync demo`.
fn demo_diff() -> Vec<DiffEntry> {
    vec![
        DiffEntry {
            name: "btop".to_string(),
            kind: DiffKind::Missing { remote_version: "1.4.0-1".to_string() },
        },
        DiffEntry {
            name: "discord".to_string(),
            kind: DiffKind::Extra { local_version: "0.0.49-1".to_string() },
        },
        DiffEntry {
            name: "hyprland".to_string(),
            kind: DiffKind::VersionSkew {
                local_version: "0.45.0-1".to_string(),
                remote_version: "0.46.2-1".to_string(),
            },
        },
        DiffEntry {
            name: "neovim".to_string(),
            kind: DiffKind::Missing { remote_version: "0.10.2-1".to_string() },
        },
        DiffEntry {
            name: "ripgrep".to_string(),
            kind: DiffKind::Missing { remote_version: "14.1.0-1".to_string() },
        },
    ]
}
