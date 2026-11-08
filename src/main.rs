//! A small terminal UI for managing the posts of a Zola blog:
//! list, preview (rendered markdown), create, rename, edit (via $EDITOR),
//! delete posts in `content/`, and launch a live `zola serve` preview.
//! Built with the `biv` feature, a local model also reviews, plans or drafts
//! posts.

mod app;
mod assist;
mod date;
mod git;
mod markdown;
mod post;
mod ui;

use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

use app::App;
use post::Result;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let content_dir = post::find_content_dir()?;
    let mut app = App::new(content_dir)?;

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    // Tear down any `zola serve` we started so it does not outlive the tool.
    app.shutdown();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.should_quit {
        app.poll_server();
        app.poll_assist();
        terminal.draw(|f| ui::draw(f, app))?;

        // While the assistant writes, wake often to show its text as it comes;
        // while serving, wake periodically so a server that exits on its own
        // is noticed; otherwise block until the next key.
        let event_ready = if app.is_assisting() {
            event::poll(Duration::from_millis(50))?
        } else if app.is_serving() {
            event::poll(Duration::from_millis(500))?
        } else {
            true
        };
        if event_ready
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.handle_key(key.code, key.modifiers);
        }

        // Editing needs the terminal to itself, so it is deferred out of the
        // key handler and performed here between frames.
        if let Some(path) = app.pending_edit.take() {
            let keep = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string);
            match edit_in_external_editor(terminal, &path) {
                Ok(()) => app.status = format!("edited {}", keep.as_deref().unwrap_or("post")),
                Err(e) => app.status = format!("editor failed: {e}"),
            }
            app.reload(keep.as_deref());
            // If we edited from the preview, pick up the changes on screen.
            app.refresh_view_body();
        }
    }
    Ok(())
}

/// Suspend the TUI, hand the terminal to `$VISUAL`/`$EDITOR` (falling back to a
/// common editor), then restore the alternate screen.
fn edit_in_external_editor(terminal: &mut DefaultTerminal, path: &Path) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| default_editor());

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;

    // Support editors invoked with arguments, e.g. EDITOR="code --wait".
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = Command::new(program).args(parts).arg(path).status();

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;

    let status = status?;
    if !status.success() {
        return Err(format!("{editor} exited with {status}").into());
    }
    Ok(())
}

fn default_editor() -> String {
    for candidate in ["nvim", "vim", "vi", "nano"] {
        if Command::new(candidate).arg("--version").output().is_ok() {
            return candidate.to_string();
        }
    }
    "vi".to_string()
}
