//! pkgsync — interactively diff & sync packages between two Arch machines.
//!
//! Stage 1: the bare event loop. Right now it just draws a placeholder screen
//! and quits on `q`. Everything else (parsing packages, diffing, selecting,
//! applying) gets layered on top of this same loop in later stages.

use ratatui::{
    // ratatui re-exports crossterm, so we use *its* copy — that guarantees the
    // crossterm version always matches what ratatui was built against.
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    widgets::{Block, Paragraph},
    DefaultTerminal, Frame,
};

fn main() -> std::io::Result<()> {
    // `ratatui::init()` does three things for us:
    //   1. switches the terminal into "raw mode" (keystrokes come to us
    //      directly instead of being line-buffered by the shell),
    //   2. enters the "alternate screen" (a separate buffer, so your real
    //      shell history is untouched when we exit),
    //   3. returns a `DefaultTerminal` we draw through.
    let mut terminal = ratatui::init();

    // Run the app, but DON'T `?` straight away — we want to restore the
    // terminal first no matter what, otherwise a crash leaves the user's shell
    // in raw mode (invisible cursor, no echo). So we capture the result, undo
    // the terminal setup, then return it.
    let result = run(&mut terminal);

    // Undo init(): leave alternate screen, disable raw mode, show cursor.
    ratatui::restore();

    result
}

/// The main loop. ratatui is *immediate mode*: there are no persistent widget
/// objects. Every iteration we (a) describe the entire UI from scratch in
/// `draw`, then (b) block waiting for one input event and react to it.
fn run(terminal: &mut DefaultTerminal) -> std::io::Result<()> {
    loop {
        // Hand ratatui a closure that paints one frame. It diffs our described
        // frame against what's currently on screen and only writes the cells
        // that actually changed — that's why redrawing "everything" is cheap.
        terminal.draw(draw)?;

        // Block until the next terminal event (key, resize, mouse, ...).
        if let Event::Key(key) = event::read()? {
            // On some platforms a key press emits both Press and Release
            // events. We only act on Press so actions don't fire twice.
            if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') {
                return Ok(());
            }
        }
    }
}

/// Describe the whole UI for one frame. In later stages this grows into the
/// real layout (diff list, detail pane, status bar); for now it's a centered
/// placeholder inside a bordered box.
fn draw(frame: &mut Frame) {
    let block = Block::bordered().title(" pkgsync ");
    let text = Paragraph::new("pkgsync — stage 1 scaffold\n\npress q to quit").block(block);

    // `frame.area()` is the full terminal rectangle. We render the paragraph
    // (with its border block) to fill it.
    frame.render_widget(text, frame.area());
}
