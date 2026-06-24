//! pkgsync — interactively diff & sync packages between two Arch machines.
//!
//! Stage 5: a stateful, interactive TUI. The `App` (in `app.rs`) owns all
//! state; here we just render it and feed key presses into it. Navigate with
//! ↑/↓ (or j/k), tick rows with Tab/Space, filter with a/i/u/r, quit with q.

use pkgsync::app::App;
use pkgsync::diff::{diff, Category, DiffEntry, DiffKind};
use pkgsync::source::{fetch_with_fallback, FileSource, LocalSource, SshSource, Source};
use ratatui::{
    crossterm::event::{self, Event, KeyEventKind},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, List, ListItem, Paragraph},
    DefaultTerminal, Frame,
};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
pkgsync — diff this machine's packages against another's

USAGE:
    pkgsync <remote> [fallback-file]   compare local vs a remote
    pkgsync demo                       run with sample data (no machines needed)

<remote> is either a path to a .pkgs state file, or an SSH host.
If <remote> is an SSH host, you can pass a state file as a fallback for when
the host is unreachable.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Build the diff BEFORE entering the alternate screen, so any error prints
    // cleanly to the normal terminal instead of being wiped by TUI teardown.
    let entries = match load_entries(&args) {
        Ok(entries) => entries,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, App::new(entries));
    ratatui::restore();

    if let Err(error) = result {
        eprintln!("error: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Decide where packages come from based on CLI args, then compute the diff.
/// Returns a user-facing error string on any failure.
fn load_entries(args: &[String]) -> Result<Vec<DiffEntry>, String> {
    match args.first().map(String::as_str) {
        None => Err(USAGE.to_string()),
        Some("demo") => Ok(demo_diff()),
        Some(remote_arg) => {
            let local = LocalSource
                .fetch()
                .map_err(|e| format!("reading local packages: {e}"))?;

            // A path that exists -> file source; otherwise treat it as an SSH host.
            let remote = if Path::new(remote_arg).is_file() {
                FileSource::new(remote_arg)
                    .fetch()
                    .map_err(|e| format!("reading remote: {e}"))?
            } else {
                let ssh = SshSource::new(remote_arg);
                match args.get(1) {
                    // SSH with a state-file fallback.
                    Some(fallback) => {
                        let (packages, _origin) =
                            fetch_with_fallback(&ssh, &FileSource::new(fallback))
                                .map_err(|e| format!("reading remote (ssh+fallback): {e}"))?;
                        packages
                    }
                    None => ssh.fetch().map_err(|e| format!("reading remote (ssh): {e}"))?,
                }
            };

            Ok(diff(&local, &remote))
        }
    }
}

fn run(terminal: &mut DefaultTerminal, mut app: App) -> std::io::Result<()> {
    // The loop now runs until the App says to quit, instead of matching a key
    // inline. All key meaning lives in `app.handle_key`.
    while !app.should_quit {
        terminal.draw(|frame| draw(frame, &mut app))?;

        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.handle_key(key.code);
        }
    }
    Ok(())
}

/// Paint one frame from the current `App` state. Takes `&mut App` because the
/// list highlight is a *stateful* widget — it reads and updates `list_state`.
fn draw(frame: &mut Frame, app: &mut App) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let [list_area, detail_area] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);

    render_list(frame, list_area, app);
    render_detail(frame, detail_area, app);
    render_footer(frame, footer, app);
}

fn render_list(frame: &mut Frame, area: Rect, app: &mut App) {
    // Build owned `ListItem`s first. This borrows `app` immutably, but the
    // borrow ends with this statement (the items own their strings), freeing us
    // to take a *mutable* borrow of `app.list_state` below.
    let items: Vec<ListItem> = app
        .visible()
        .iter()
        .map(|entry| diff_item(entry, app.is_selected(&entry.name)))
        .collect();

    let title = summary_title(app);
    let list = List::new(items)
        .block(Block::bordered().title(title))
        // The highlight style is applied to whichever row `list_state` points
        // at; the symbol is drawn in the left margin of that row.
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("› ");

    // `render_stateful_widget` is the key call: it hands the widget our
    // `ListState`, which is how the moving cursor and scrolling work.
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

/// One styled row: `[x] <sym> <action>  <name>          <detail>`.
fn diff_item(entry: &DiffEntry, selected: bool) -> ListItem<'static> {
    let (symbol, action, color, detail) = match &entry.kind {
        DiffKind::Missing { remote_version } => {
            ("+", "install", Color::Green, format!("remote {remote_version}"))
        }
        DiffKind::Extra { local_version } => {
            ("-", "remove", Color::Red, format!("local {local_version}"))
        }
        DiffKind::VersionSkew {
            local_version,
            remote_version,
        } => (
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

/// The right pane: live detail of the highlighted entry.
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
        DiffKind::VersionSkew {
            local_version,
            remote_version,
        } => (
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

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let key = |k: &'static str| Span::styled(format!(" {k} "), Style::new().fg(Color::Black).bg(Color::Gray));
    let help = Line::from(vec![
        key("↑↓"),
        Span::raw(" move  "),
        key("Tab"),
        Span::raw(" select  "),
        key("a/i/u/r"),
        Span::raw(" filter  "),
        key("q"),
        Span::raw(" quit   "),
        Span::styled(
            format!("[filter: {}]  [selected: {}]", app.filter.label(), app.selected.len()),
            Style::new().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(help), area);
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

/// Sample data for `pkgsync demo` — lets you see the UI without a second
/// machine reachable.
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
