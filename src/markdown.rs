//! A basic markdown-to-TUI renderer: turns a markdown string into styled
//! `ratatui` lines, rendering the subset that makes sense in a terminal
//! (headings, emphasis, inline code, code blocks, lists, blockquotes, links,
//! rules and simple tables). Anything it can't render (raw HTML, images) is
//! degraded gracefully rather than shown verbatim.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

const LINK: Color = Color::Blue;
const DIM: Color = Color::DarkGray;
const CODE_FG: Color = Color::Indexed(180);
const CODE_BG: Color = Color::Indexed(236);

/// Parse `markdown` and produce styled, unwrapped logical lines.
pub fn render(markdown: &str) -> Vec<Line<'static>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);

    let mut r = Renderer::default();
    for event in Parser::new_ext(markdown, opts) {
        r.event(event);
    }
    r.finish()
}

#[derive(Default)]
struct Renderer {
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    bold: u32,
    italic: u32,
    strike: u32,
    link: bool,
    image: bool,
    heading: Option<u8>,
    /// One entry per open list; `Some(n)` is the next ordered number.
    list: Vec<Option<u64>>,
    blockquote: u32,
    /// Buffer accumulating the current fenced/indented code block.
    code_block: Option<String>,
    /// A list marker to emit once the item's first content line begins.
    pending_marker: Option<String>,
    // Simple table state: rows are buffered until the table ends so columns can
    // be padded to a common width. Each entry is `(is_header, cells)`.
    in_cell: bool,
    cell: String,
    row: Vec<String>,
    table_rows: Vec<(bool, Vec<String>)>,
}

impl Renderer {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => self.inline_code(&t),
            Event::SoftBreak => self.soft_break(),
            Event::HardBreak => self.newline(),
            Event::Rule => self.rule(),
            Event::TaskListMarker(done) => {
                let mark = if done { "[x] " } else { "[ ] " };
                self.push(mark.to_string(), Style::default().fg(Color::Green));
            }
            // Raw HTML, math and footnotes have no sensible terminal rendering.
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.flush();
                let lvl = level as u8;
                self.heading = Some(lvl);
                let hashes = format!("{} ", "#".repeat(lvl as usize));
                self.push(hashes, self.style());
            }
            Tag::BlockQuote(_) => self.blockquote += 1,
            Tag::CodeBlock(_) => {
                self.flush();
                self.code_block = Some(String::new());
            }
            Tag::List(start) => {
                self.flush();
                self.list.push(start);
            }
            Tag::Item => {
                self.flush();
                let depth = self.list.len().saturating_sub(1);
                let indent = "  ".repeat(depth);
                let marker = match self.list.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                self.pending_marker = Some(format!("{indent}{marker}"));
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { .. } => self.link = true,
            Tag::Image { .. } => {
                self.image = true;
                self.push("[image: ".to_string(), Style::default().fg(DIM));
            }
            Tag::Table(_) => {
                self.flush();
                self.table_rows.clear();
            }
            Tag::TableHead | Tag::TableRow => self.row.clear(),
            Tag::TableCell => {
                self.in_cell = true;
                self.cell.clear();
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.newline();
                if self.list.is_empty() && self.blockquote == 0 {
                    self.blank();
                }
            }
            TagEnd::Heading(_) => {
                self.heading = None;
                self.newline();
                self.blank();
            }
            TagEnd::BlockQuote(_) => {
                self.blockquote = self.blockquote.saturating_sub(1);
                self.blank();
            }
            TagEnd::CodeBlock => self.emit_code_block(),
            TagEnd::List(_) => {
                self.list.pop();
                if self.list.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Item => self.newline(),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = false,
            TagEnd::Image => {
                self.push("]".to_string(), Style::default().fg(DIM));
                self.image = false;
            }
            TagEnd::TableCell => {
                self.in_cell = false;
                self.row
                    .push(std::mem::take(&mut self.cell).trim().to_string());
            }
            TagEnd::TableHead => self.table_rows.push((true, std::mem::take(&mut self.row))),
            TagEnd::TableRow => self.table_rows.push((false, std::mem::take(&mut self.row))),
            TagEnd::Table => {
                self.emit_table();
                self.blank();
            }
            _ => {}
        }
    }

    /// Inline styling accumulated from the open emphasis/heading/link tags.
    fn style(&self) -> Style {
        let mut s = Style::default();
        if let Some(level) = self.heading {
            s = s.fg(heading_color(level)).add_modifier(Modifier::BOLD);
        }
        if self.bold > 0 {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.strike > 0 {
            s = s.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.link {
            s = s.fg(LINK).add_modifier(Modifier::UNDERLINED);
        }
        if self.image {
            s = s.fg(DIM);
        }
        s
    }

    fn text(&mut self, text: &str) {
        if let Some(buf) = &mut self.code_block {
            buf.push_str(text);
            return;
        }
        if self.in_cell {
            self.cell.push_str(text);
            return;
        }
        self.push(text.to_string(), self.style());
    }

    fn inline_code(&mut self, text: &str) {
        if self.in_cell {
            self.cell.push_str(text);
            return;
        }
        self.push(text.to_string(), Style::default().fg(CODE_FG).bg(CODE_BG));
    }

    fn soft_break(&mut self) {
        if self.code_block.is_some() {
            return;
        }
        if self.in_cell {
            self.cell.push(' ');
            return;
        }
        // Join wrapped source lines into one logical line so the viewer can
        // reflow them to the terminal width.
        if !self.cur.is_empty() {
            self.cur.push(Span::raw(" "));
        }
    }

    /// Push a span, emitting any pending blockquote bars / list marker first.
    fn push(&mut self, text: String, style: Style) {
        if self.cur.is_empty() {
            for _ in 0..self.blockquote {
                self.cur.push(Span::styled("▏ ", Style::default().fg(DIM)));
            }
            if let Some(marker) = self.pending_marker.take() {
                self.cur
                    .push(Span::styled(marker, Style::default().fg(Color::Cyan)));
            }
        }
        self.cur.push(Span::styled(text, style));
    }

    fn newline(&mut self) {
        let spans = std::mem::take(&mut self.cur);
        self.lines.push(Line::from(spans));
    }

    /// Flush a partial line without leaving a trailing empty one.
    fn flush(&mut self) {
        if !self.cur.is_empty() {
            self.newline();
        }
    }

    /// Append a blank separator line, collapsing consecutive blanks.
    fn blank(&mut self) {
        if self.lines.last().map(|l| l.spans.is_empty()) != Some(true) && !self.lines.is_empty() {
            self.lines.push(Line::default());
        }
    }

    fn rule(&mut self) {
        self.flush();
        self.lines.push(Line::from(Span::styled(
            "─".repeat(50),
            Style::default().fg(DIM),
        )));
        self.blank();
    }

    fn emit_code_block(&mut self) {
        let Some(buf) = self.code_block.take() else {
            return;
        };
        let body = buf.strip_suffix('\n').unwrap_or(&buf);
        for line in body.split('\n') {
            self.lines.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(CODE_FG).bg(CODE_BG),
            )));
        }
        self.blank();
    }

    /// Emit the buffered table, padding every cell to its column's width so the
    /// ` │ ` separators line up, with a `─┼─` rule under the header row.
    fn emit_table(&mut self) {
        let rows = std::mem::take(&mut self.table_rows);
        let ncols = rows.iter().map(|(_, c)| c.len()).max().unwrap_or(0);
        if ncols == 0 {
            return;
        }
        let mut widths = vec![0usize; ncols];
        for (_, cells) in &rows {
            for (i, cell) in cells.iter().enumerate() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }

        for (header, cells) in &rows {
            let cell_style = if *header {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Cyan)
            } else {
                Style::default()
            };
            let mut spans = Vec::with_capacity(ncols * 2);
            for (i, width) in widths.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" │ ", Style::default().fg(DIM)));
                }
                let text = cells.get(i).map(String::as_str).unwrap_or("");
                // Don't pad the final column, to avoid trailing whitespace.
                let cell = if i + 1 < ncols {
                    pad(text, *width)
                } else {
                    text.to_string()
                };
                spans.push(Span::styled(cell, cell_style));
            }
            self.lines.push(Line::from(spans));

            if *header {
                let rule = widths
                    .iter()
                    .map(|w| "─".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("─┼─");
                self.lines
                    .push(Line::from(Span::styled(rule, Style::default().fg(DIM))));
            }
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        while self.lines.last().map(|l| l.spans.is_empty()) == Some(true) {
            self.lines.pop();
        }
        if self.lines.is_empty() {
            self.lines.push(Line::from(Span::styled(
                "(empty post)",
                Style::default().fg(DIM),
            )));
        }
        self.lines
    }
}

/// Left-pad `s` with spaces to `width` display columns (approximated by char
/// count, matching the wrap estimator).
fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - len))
}

fn heading_color(level: u8) -> Color {
    match level {
        1 => Color::Cyan,
        2 => Color::Green,
        3 => Color::Yellow,
        _ => Color::Magenta,
    }
}

/// Estimate how many terminal rows the logical `lines` occupy once word-wrapped
/// to `width`. Used to bound scrolling and size the scrollbar; it mirrors
/// ratatui's word-wrap closely enough for that purpose.
pub fn visual_row_count(lines: &[Line], width: u16) -> u16 {
    if width == 0 {
        return lines.len().min(u16::MAX as usize) as u16;
    }
    let w = width as usize;
    let mut total: usize = 0;
    for line in lines {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        total += wrapped_rows(&text, w);
    }
    total.min(u16::MAX as usize) as u16
}

fn wrapped_rows(text: &str, width: usize) -> usize {
    let mut rows = 1usize;
    let mut col = 0usize;
    for word in text.split(' ') {
        let wl = word.chars().count();
        if col == 0 {
            place_first(&mut rows, &mut col, wl, width);
        } else if col + 1 + wl <= width {
            col += 1 + wl;
        } else {
            rows += 1;
            place_first(&mut rows, &mut col, wl, width);
        }
    }
    rows
}

/// Place a word at the start of a line, hard-splitting it if it is wider than
/// the whole line.
fn place_first(rows: &mut usize, col: &mut usize, word_len: usize, width: usize) {
    if word_len <= width {
        *col = word_len;
    } else {
        *rows += (word_len - 1) / width;
        *col = word_len % width;
        if *col == 0 {
            *col = width;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn renders_headings_and_emphasis() {
        let lines = render("# Title\n\nsome **bold** and *italic* text\n");
        let text = plain(&lines);
        assert!(text.iter().any(|l| l == "# Title"));
        assert!(
            text.iter()
                .any(|l| l.contains("bold") && l.contains("italic"))
        );
    }

    #[test]
    fn renders_list_and_code_block() {
        let md = "- one\n- two\n\n```\ncode line\n```\n";
        let text = plain(&render(md));
        assert!(text.iter().any(|l| l.contains("• one")));
        assert!(text.iter().any(|l| l.contains("• two")));
        assert!(text.iter().any(|l| l.contains("code line")));
    }

    #[test]
    fn ordered_list_numbers() {
        let text = plain(&render("1. first\n2. second\n"));
        assert!(text.iter().any(|l| l.contains("1. first")));
        assert!(text.iter().any(|l| l.contains("2. second")));
    }

    #[test]
    fn table_columns_are_aligned() {
        let md = "| Name | Val |\n| --- | --- |\n| a | 1 |\n| bbbb | 22 |\n";
        let text = plain(&render(md));
        let pipe = |s: &&String| s.chars().position(|c| c == '│');

        let header = text.iter().find(|l| l.contains("Name")).unwrap();
        let rule = text.iter().find(|l| l.contains('┼')).unwrap();
        let row_a = text.iter().find(|l| l.starts_with("a ")).unwrap();
        let row_b = text.iter().find(|l| l.starts_with("bbbb")).unwrap();

        // Every row's separator, and the header rule's cross, sit at one column.
        let col = pipe(&header);
        assert!(col.is_some());
        assert_eq!(pipe(&row_a), col);
        assert_eq!(pipe(&row_b), col);
        assert_eq!(rule.chars().position(|c| c == '┼'), col);
    }

    #[test]
    fn raw_html_comment_is_skipped() {
        let text = plain(&render("before\n\n<!-- more -->\n\nafter"));
        assert!(text.iter().all(|l| !l.contains("more")));
        assert!(text.iter().any(|l| l == "before"));
        assert!(text.iter().any(|l| l == "after"));
    }

    #[test]
    fn wrapped_rows_counts_wrapping() {
        assert_eq!(wrapped_rows("", 10), 1);
        assert_eq!(wrapped_rows("short", 10), 1);
        assert_eq!(wrapped_rows("one two three four", 8), 3); // "one two" / "three" / "four"
        assert_eq!(wrapped_rows("abcdefghij", 5), 2); // hard split
    }
}
