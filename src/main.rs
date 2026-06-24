//! pkgsync — interactively diff & sync packages between two Arch machines.
//!
//! Stage 4: render a (hard-coded) diff in a real two-pane layout with a
//! color-coded list. Still static — no selection or actions yet — so we can
//! focus on ratatui's layout + widget + styling model. The sample data gets
//! replaced by real sources in a later stage.

use pkgsync::diff::{DiffEntry, DiffKind};
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, List, ListItem, Paragraph},
    DefaultTerminal, Frame,
};

fn main() -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal) -> std::io::Result<()> {
    // Hard-coded sample diff so we have something to render. In Stage 6 this
    // comes from parsing real `pacman -Qe` output of both machines.
    let entries = sample_diff();

    loop {
        // `draw` needs the data, so we wrap it in a closure that captures
        // `entries` and forwards the `Frame`.
        terminal.draw(|frame| draw(frame, &entries))?;

        // A "let-chain" (2024 edition): combine the `if let` pattern match with
        // extra boolean conditions in one `if`, no nesting required.
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && key.code == KeyCode::Char('q')
        {
            return Ok(());
        }
    }
}

/// Paint one frame: a header/list pane on the left, a detail pane on the right,
/// and a one-line footer.
fn draw(frame: &mut Frame, entries: &[DiffEntry]) {
    // Split the screen vertically into a main body and a 1-row footer.
    // `Layout::vertical(...).areas(area)` returns a fixed-size array we can
    // destructure directly — cleaner than indexing into a slice.
    let [body, footer] = Layout::vertical([
        Constraint::Min(0),    // body takes all remaining space
        Constraint::Length(1), // footer is exactly one row
    ])
    .areas(frame.area());

    // Split the body horizontally: list on the left, detail on the right.
    let [list_area, detail_area] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);

    render_list(frame, list_area, entries);
    render_detail(frame, detail_area, entries);
    render_footer(frame, footer);
}

/// The left pane: every diff entry as a colored line inside a titled box.
fn render_list(frame: &mut Frame, area: ratatui::layout::Rect, entries: &[DiffEntry]) {
    let items: Vec<ListItem> = entries.iter().map(diff_item).collect();

    let title = summary_title(entries);
    let list = List::new(items).block(Block::bordered().title(title));

    frame.render_widget(list, area);
}

/// Turn one diff entry into a styled list row:
/// `<sym> <action>  <name>            <version detail>`, colored by category.
fn diff_item(entry: &DiffEntry) -> ListItem<'static> {
    // Each category gets a symbol, an action verb, a color, and a version blurb.
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

    // A `Line` is a sequence of `Span`s, each with its own styling. We pad with
    // `{:<width}` so columns line up regardless of name length.
    let line = Line::from(vec![
        Span::styled(format!("{symbol} "), Style::new().fg(color)),
        Span::styled(format!("{action:<8}"), Style::new().fg(color).bold()),
        Span::raw(format!("{:<24}", entry.name)),
        Span::raw(detail).dim(),
    ]);

    ListItem::new(line)
}

/// The right pane. For now (no selection yet) it just shows a legend and the
/// per-category counts. Stage 5 turns this into a live detail view of the
/// currently highlighted package.
fn render_detail(frame: &mut Frame, area: ratatui::layout::Rect, entries: &[DiffEntry]) {
    let (install, upgrade, remove) = counts(entries);

    let lines = vec![
        Line::from("Legend".bold()),
        Line::from(vec![Span::styled("+ install", Style::new().fg(Color::Green))]),
        Line::from(vec![Span::styled("~ upgrade", Style::new().fg(Color::Yellow))]),
        Line::from(vec![Span::styled("- remove", Style::new().fg(Color::Red))]),
        Line::from(""),
        Line::from(format!("{install} to install")),
        Line::from(format!("{upgrade} to upgrade")),
        Line::from(format!("{remove} to remove")),
        Line::from(""),
        Line::from("(selection & per-package".dim()),
        Line::from(" detail land in stage 5)".dim()),
    ];

    let detail = Paragraph::new(lines).block(Block::bordered().title(" detail "));
    frame.render_widget(detail, area);
}

/// The bottom help line.
fn render_footer(frame: &mut Frame, area: ratatui::layout::Rect) {
    let help = Line::from(vec![
        Span::styled(" q ", Style::new().fg(Color::Black).bg(Color::Gray)),
        Span::raw(" quit"),
    ]);
    frame.render_widget(Paragraph::new(help), area);
}

/// Build the list's title with a quick breakdown of how many of each category.
fn summary_title(entries: &[DiffEntry]) -> String {
    let (install, upgrade, remove) = counts(entries);
    format!(" diff — {install} install · {upgrade} upgrade · {remove} remove ")
}

/// Count entries per category in a single pass.
fn counts(entries: &[DiffEntry]) -> (usize, usize, usize) {
    use pkgsync::diff::Category;
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

/// Placeholder data until we wire up real package sources.
fn sample_diff() -> Vec<DiffEntry> {
    vec![
        DiffEntry {
            name: "btop".to_string(),
            kind: DiffKind::Missing {
                remote_version: "1.4.0-1".to_string(),
            },
        },
        DiffEntry {
            name: "discord".to_string(),
            kind: DiffKind::Extra {
                local_version: "0.0.49-1".to_string(),
            },
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
            kind: DiffKind::Missing {
                remote_version: "0.10.2-1".to_string(),
            },
        },
        DiffEntry {
            name: "ripgrep".to_string(),
            kind: DiffKind::Missing {
                remote_version: "14.1.0-1".to_string(),
            },
        },
    ]
}
