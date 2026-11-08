//! All rendering. Mostly pure functions of `&App`; the preview additionally
//! clamps its scroll offset against the wrapped content height during draw.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table, TableState, Wrap,
    },
};

use crate::app::{App, ConfirmAction, InputKind, Mode};
use crate::date::{self, Date};
use crate::git::GitState;
use crate::markdown;

const ACCENT: Color = Color::Cyan;

pub fn draw(frame: &mut Frame, app: &mut App) {
    // The markdown preview is a full-screen mode of its own.
    if matches!(app.mode, Mode::View(_)) {
        draw_view(frame, app);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(1),    // table
            Constraint::Length(4), // footer (keys + status)
        ])
        .split(frame.area());

    draw_header(frame, app, chunks[0]);
    draw_list(frame, app, chunks[1]);
    draw_footer(frame, app, chunks[2]);

    match &app.mode {
        Mode::Input(_) => draw_input(frame, app),
        Mode::Confirm(_) => draw_confirm(frame, app),
        Mode::DatePicker(_) => draw_datepicker(frame, app),
        Mode::Help => draw_help(frame),
        Mode::List | Mode::View(_) => {}
    }
}

fn draw_view(frame: &mut Frame, app: &mut App) {
    let Mode::View(view) = &mut app.mode else {
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(1),    // rendered body
            Constraint::Length(3), // footer
        ])
        .split(frame.area());

    let lines = markdown::render(&view.body);

    let inner_w = chunks[1].width.saturating_sub(2);
    let inner_h = chunks[1].height.saturating_sub(2);
    let total = markdown::visual_row_count(&lines, inner_w);
    let max_scroll = total.saturating_sub(inner_h);
    if view.scroll > max_scroll {
        view.scroll = max_scroll;
    }
    let percent = if max_scroll == 0 {
        100
    } else {
        (view.scroll as u32 * 100 / max_scroll as u32) as u16
    };

    // Header: title + filename + scroll position.
    let header = Line::from(vec![
        Span::styled(
            format!(" 📖 {} ", view.title),
            Style::default().fg(ACCENT).bold(),
        ),
        Span::styled(
            format!("· {} ", view.filename),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let header_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(format!(" {percent}% "))
        .title_alignment(Alignment::Right);
    frame.render_widget(Paragraph::new(header).block(header_block), chunks[0]);

    // Body.
    let body = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Preview "))
        .wrap(Wrap { trim: false })
        .scroll((view.scroll, 0));
    frame.render_widget(body, chunks[1]);

    // Scrollbar on the body's right edge.
    let mut sb_state = ScrollbarState::new(total as usize)
        .viewport_content_length(inner_h as usize)
        .position(view.scroll as usize);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓")),
        chunks[1],
        &mut sb_state,
    );

    // Footer.
    let keys = Line::from(vec![
        key("↑/↓"),
        Span::raw(" scroll  "),
        key("PgUp/PgDn"),
        Span::raw(" page  "),
        key("g/G"),
        Span::raw(" top/end  "),
        key("e"),
        Span::raw(" edit  "),
        key("q/Esc"),
        Span::raw(" back"),
    ]);
    frame.render_widget(
        Paragraph::new(keys).block(Block::default().borders(Borders::ALL)),
        chunks[2],
    );
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let drafts = app.posts.iter().filter(|p| p.draft).count();
    let mut spans = vec![
        Span::styled(" 📝 Zola Blog Manager ", Style::default().fg(ACCENT).bold()),
        Span::styled(
            format!("· {} posts ({} draft) ", app.posts.len(), drafts),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if let Some(url) = app.server_url() {
        spans.push(Span::styled(
            format!("· ● serving {url} "),
            Style::default().fg(Color::Green).bold(),
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(app.content_dir.display().to_string());
    frame.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Posts ");

    if app.posts.is_empty() {
        let msg = Paragraph::new("No posts yet — press 'n' to create one.")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center)
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let show_git = app.is_git_repo();

    let rows = app.posts.iter().map(|p| {
        let flag = if p.draft {
            Cell::from("● draft").style(Style::default().fg(Color::Yellow))
        } else {
            Cell::from("published").style(Style::default().fg(Color::Green))
        };
        let mut cells = vec![
            Cell::from(p.date.clone()).style(Style::default().fg(Color::DarkGray)),
            flag,
        ];
        if show_git {
            cells.push(git_cell(p.git));
        }
        cells.push(Cell::from(p.title.clone()));
        Row::new(cells)
    });

    let mut widths = vec![Constraint::Length(12), Constraint::Length(11)];
    let mut headers = vec!["Date", "Status"];
    if show_git {
        widths.push(Constraint::Length(12));
        headers.push("Git");
    }
    widths.push(Constraint::Min(20));
    headers.push("Title");

    let table = Table::new(rows, widths)
        .header(Row::new(headers).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
        .block(block)
        .row_highlight_style(
            Style::default()
                .bg(Color::Indexed(238))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = TableState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

/// The git-status cell for a post (empty when the state is unknown).
fn git_cell(state: Option<GitState>) -> Cell<'static> {
    match state {
        Some(GitState::Tracked) => Cell::from("✓ tracked").style(Style::default().fg(Color::Green)),
        Some(GitState::Modified) => {
            Cell::from("● modified").style(Style::default().fg(Color::Yellow))
        }
        Some(GitState::Untracked) => {
            Cell::from("✚ untracked").style(Style::default().fg(Color::Red))
        }
        None => Cell::from(""),
    }
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let serve_label = if app.is_serving() {
        " stop  "
    } else {
        " serve  "
    };
    let mut spans = vec![
        key("↑/↓"),
        Span::raw(" move  "),
        key("⏎"),
        Span::raw(" view  "),
        key("e"),
        Span::raw(" edit  "),
        key("p"),
        Span::raw(" pub  "),
        key("t"),
        Span::raw(" date  "),
        key("n"),
        Span::raw(" new  "),
        key("r"),
        Span::raw(" rename  "),
        key("d"),
        Span::raw(" del  "),
    ];
    if app.is_git_repo() {
        spans.extend([
            key("c"),
            Span::raw(" commit  "),
            key("P"),
            Span::raw(" deploy  "),
        ]);
    }
    spans.extend([
        key("s"),
        Span::raw(serve_label),
        key("?"),
        Span::raw(" help  "),
        key("q"),
        Span::raw(" quit"),
    ]);
    let keys = Line::from(spans);
    let status = Line::from(Span::styled(
        format!(" {}", app.status),
        Style::default().fg(Color::Green),
    ));
    let para = Paragraph::new(vec![keys, status]).block(Block::default().borders(Borders::ALL));
    frame.render_widget(para, area);
}

fn key(k: &str) -> Span<'_> {
    Span::styled(k, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
}

fn draw_input(frame: &mut Frame, app: &App) {
    let Mode::Input(input) = &app.mode else {
        return;
    };
    let area = centered_rect(60, 30, frame.area());
    frame.render_widget(Clear, area);

    let hint = match input.kind {
        InputKind::NewTitle => "A YYYY-MM-DD-slug.md draft will be created.",
        InputKind::Rename => "Edit the filename (.md added automatically).",
        InputKind::Commit => "The selected post is staged and committed.",
    };
    let body = vec![
        Line::from(Span::styled(
            format!("{} ", input.buffer),
            Style::default().fg(Color::White).bg(Color::Indexed(236)),
        ))
        .style(Style::default()),
        Line::from(""),
        Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled(
            "⏎ confirm   Esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(format!(" {} ", input.prompt));
    frame.render_widget(
        Paragraph::new(body).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_confirm(frame: &mut Frame, app: &App) {
    let Mode::Confirm(confirm) = &app.mode else {
        return;
    };
    let area = centered_rect(60, 45, frame.area());
    frame.render_widget(Clear, area);

    let (border, title, body) = match &confirm.action {
        ConfirmAction::DeletePost { filename, .. } => (
            Color::Red,
            " Confirm delete ",
            vec![
                Line::from(Span::styled(
                    "⚠  Delete this post permanently?",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    filename.clone(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "This cannot be undone.",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                confirm_prompt("delete"),
            ],
        ),
        ConfirmAction::Push => (
            ACCENT,
            " Confirm deploy ",
            vec![
                Line::from(Span::styled(
                    "Deploy — push commits to the remote?",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Runs `git push`, publishing your local commits.",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                confirm_prompt("push"),
            ],
        ),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(title);
    frame.render_widget(
        Paragraph::new(body)
            .alignment(Alignment::Center)
            .block(block),
        area,
    );
}

/// The `y <verb> / n cancel` line shared by confirmation dialogs.
fn confirm_prompt(verb: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "y",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {verb}    ")),
        Span::styled(
            "n",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("/Esc cancel"),
    ])
}

fn draw_datepicker(frame: &mut Frame, app: &App) {
    let Mode::DatePicker(state) = &app.mode else {
        return;
    };
    let area = centered_rect(52, 64, frame.area());
    frame.render_widget(Clear, area);

    let sel = state.date;
    let today = Date::today();

    let mut lines: Vec<Line> = Vec::new();

    // Headline: the selected date, then the month/year being viewed.
    lines.push(Line::from(Span::styled(
        format!("{} {}", date::weekday_short(sel.weekday()), sel.to_iso()),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        format!("‹ {} {} ›", date::month_name(sel.month), sel.year),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    // Weekday header (Monday-first), aligned with the grid below it.
    const WEEKDAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
    let mut header = vec![Span::raw(" ")];
    for wd in WEEKDAYS {
        header.push(Span::styled(
            format!("{wd} "),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(header));

    // Grid: blanks up to the first weekday of the month, then each day.
    let first_weekday = Date::new(sel.year, sel.month, 1)
        .map(Date::weekday)
        .unwrap_or(0);
    let mut cells: Vec<Option<u32>> = vec![None; first_weekday as usize];
    cells.extend((1..=sel.days_in_month()).map(Some));
    while !cells.len().is_multiple_of(7) {
        cells.push(None);
    }
    for week in cells.chunks(7) {
        let mut spans = vec![Span::raw(" ")];
        for cell in week {
            match cell {
                Some(d) => {
                    let style = if *d == sel.day {
                        Style::default()
                            .bg(ACCENT)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD)
                    } else if today.year == sel.year && today.month == sel.month && *d == today.day
                    {
                        Style::default()
                            .fg(ACCENT)
                            .add_modifier(Modifier::UNDERLINED)
                    } else {
                        Style::default()
                    };
                    spans.push(Span::styled(format!("{d:>2}"), style));
                    spans.push(Span::raw(" "));
                }
                None => spans.push(Span::raw("   ")),
            }
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "←→ day  ↑↓ week  PgUp/Dn month",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        "[ ] year  t today  ⏎ set  Esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(format!(" Set date · {} ", state.filename));
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(block),
        area,
    );
}

fn draw_help(frame: &mut Frame) {
    let area = centered_rect(60, 60, frame.area());
    frame.render_widget(Clear, area);

    let rows = [
        ("↑ / k", "move selection up"),
        ("↓ / j", "move selection down"),
        ("g / G", "jump to first / last"),
        ("⏎ / v", "preview post (rendered markdown)"),
        ("e", "edit post in $EDITOR"),
        ("p", "toggle draft / published"),
        ("t", "change the post's date (calendar picker)"),
        ("n", "create a new draft post"),
        ("r", "rename the selected post's file"),
        ("d", "delete the selected post (asks to confirm)"),
        ("c", "commit the selected post (git)"),
        ("P", "deploy: push commits to the remote (git)"),
        ("s", "start/stop `zola serve` + open in browser"),
        ("?", "toggle this help"),
        ("q / Esc", "quit"),
    ];
    let mut lines = vec![Line::from(Span::styled(
        "Keybindings",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))];
    lines.push(Line::from(""));
    for (k, desc) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<10}"), Style::default().fg(ACCENT)),
            Span::raw(desc),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  press any key to close",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Help ");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// A rectangle centered inside `area`, sized as a percentage of it.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::draw;
    use crate::app::{App, InputKind, InputState, Mode};
    use ratatui::{Terminal, backend::TestBackend};

    fn scratch_with_post() -> (std::path::PathBuf, App) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        // Atomic counter (not a timestamp) so parallel tests never collide.
        let dir = std::env::temp_dir().join(format!(
            "blog-tool-render-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::post::create(&dir, "Combining Rust and Haskell").unwrap();
        let app = App::new(dir.clone()).unwrap();
        (dir, app)
    }

    fn render(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf.cell((x, y)).unwrap().symbol());
            }
        }
        text
    }

    #[test]
    fn list_view_shows_header_and_post() {
        let (dir, mut app) = scratch_with_post();
        let text = render(&mut app);
        assert!(text.contains("Zola Blog Manager"), "missing header");
        assert!(
            text.contains("Combining Rust and Haskell"),
            "missing post title"
        );
        assert!(text.contains("draft"), "missing draft flag");
        assert!(text.contains("quit"), "missing footer keys");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn confirm_overlay_shows_warning() {
        let (dir, mut app) = scratch_with_post();
        app.handle_key(
            ratatui::crossterm::event::KeyCode::Char('d'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        );
        let text = render(&mut app);
        assert!(
            text.contains("Delete this post permanently"),
            "missing warning"
        );
        assert!(
            text.contains("cannot be undone"),
            "missing irreversibility notice"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn input_overlay_shows_typed_buffer() {
        let (dir, mut app) = scratch_with_post();
        app.mode = Mode::Input(InputState {
            kind: InputKind::NewTitle,
            prompt: "New post title".into(),
            buffer: "Hello There".into(),
        });
        let text = render(&mut app);
        assert!(text.contains("New post title"), "missing prompt");
        assert!(text.contains("Hello There"), "missing typed buffer");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn date_picker_renders_calendar() {
        let (dir, mut app) = scratch_with_post();
        app.handle_key(
            ratatui::crossterm::event::KeyCode::Char('t'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        );
        let text = render(&mut app);
        assert!(text.contains("Set date"), "calendar title not shown");
        assert!(
            text.contains("Mo") && text.contains("Su"),
            "weekday header not shown"
        );
        // The current month name should appear in the subtitle.
        let month = crate::date::month_name(crate::date::Date::today().month);
        assert!(text.contains(month), "month name {month} not shown");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn preview_renders_markdown_body() {
        let (dir, mut app) = scratch_with_post();
        // Enter view via the same key the list uses, then inject a body.
        app.handle_key(
            ratatui::crossterm::event::KeyCode::Enter,
            ratatui::crossterm::event::KeyModifiers::NONE,
        );
        if let Mode::View(v) = &mut app.mode {
            v.body = "# Big Heading\n\nsome **bold** words\n\n- bullet item\n".into();
        } else {
            panic!("Enter did not open the preview");
        }
        let text = render(&mut app);
        assert!(text.contains("Preview"), "missing preview frame");
        assert!(text.contains("Big Heading"), "heading not rendered");
        assert!(text.contains("bold words"), "paragraph not rendered");
        assert!(text.contains("bullet item"), "list item not rendered");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_column_shows_when_repo() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "blog-tool-uigit-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .output()
            .unwrap();
        let content = root.join("content");
        std::fs::create_dir_all(&content).unwrap();
        crate::post::create(&content, "Some Git Post").unwrap();

        let mut app = App::new(content).unwrap();
        assert!(app.is_git_repo(), "temp dir should be detected as a repo");
        let text = render(&mut app);
        assert!(text.contains("Git"), "missing Git column header");
        // A freshly created, uncommitted post is untracked.
        assert!(
            text.contains("untracked"),
            "new post should show as untracked"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn no_git_column_without_repo() {
        // scratch_with_post creates a plain (non-git) temp dir.
        let (dir, mut app) = scratch_with_post();
        assert!(!app.is_git_repo());
        let text = render(&mut app);
        assert!(!text.contains("untracked"), "should not show git states");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
