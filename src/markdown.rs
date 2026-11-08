//! A basic markdown-to-TUI renderer: turns a markdown string into styled
//! `ratatui` lines, rendering the subset that makes sense in a terminal
//! (headings, emphasis, inline code, code blocks, lists, blockquotes, links,
//! rules and simple tables). Anything it can't render (raw HTML, images) is
//! degraded gracefully rather than shown verbatim; images can be shown by the
//! caller, through [`render_with`].

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

const LINK: Color = Color::Blue;
const DIM: Color = Color::DarkGray;
const CODE_FG: Color = Color::Indexed(180);
const CODE_BG: Color = Color::Indexed(236);

/// How many rows an image takes when shown: its URL to its height, or `None`
/// to leave only its `[image: alt]` text.
pub type ImageRows<'a> = dyn FnMut(&str) -> Option<u16> + 'a;

/// Where an image goes in the rendered lines: the blank lines reserved for it
/// are `line..line + rows`, for the caller to draw it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAt {
    pub url: String,
    pub line: usize,
    pub rows: u16,
}

/// Parse `markdown` and produce styled, unwrapped logical lines.
pub fn render(markdown: &str) -> Vec<Line<'static>> {
    render_with(markdown, &mut |_| None).0
}

/// As [`render`], with blank lines reserved below the line each image is in
/// (for the images `images` can show; the `[image: alt]` text stays, as a
/// caption), and where they are.
pub fn render_with(
    markdown: &str,
    images: &mut ImageRows<'_>,
) -> (Vec<Line<'static>>, Vec<ImageAt>) {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);

    let mut r = Renderer {
        images: Some(images),
        ..Renderer::default()
    };
    for event in Parser::new_ext(markdown, opts) {
        r.event(event);
    }
    r.finish()
}

#[derive(Default)]
struct Renderer<'a, 'f> {
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    bold: u32,
    italic: u32,
    strike: u32,
    link: bool,
    image: bool,
    /// Sizes images, see [`render_with`].
    images: Option<&'a mut ImageRows<'f>>,
    /// The URL of the image being read.
    image_url: Option<String>,
    /// Images to reserve rows for once the current line ends.
    pending_images: Vec<(String, u16)>,
    /// The images placed so far.
    placed: Vec<ImageAt>,
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

impl Renderer<'_, '_> {
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
            Tag::Image { dest_url, .. } => {
                self.image = true;
                self.image_url = Some(dest_url.into_string());
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
                let url = self.image_url.take();
                // not inside table cells, which are plain text
                if !self.in_cell
                    && let (Some(url), Some(images)) = (url, self.images.as_mut())
                    && let Some(rows) = images(&url)
                    && rows > 0
                {
                    self.pending_images.push((url, rows));
                }
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
        self.place_images();
    }

    /// Reserve the rows of the images of the line just ended.
    fn place_images(&mut self) {
        for (url, rows) in std::mem::take(&mut self.pending_images) {
            let line = self.lines.len();
            self.lines
                .extend(std::iter::repeat_with(Line::default).take(rows as usize));
            self.placed.push(ImageAt { url, line, rows });
        }
    }

    /// Flush a partial line without leaving a trailing empty one.
    fn flush(&mut self) {
        if !self.cur.is_empty() {
            self.newline();
        }
    }

    /// Append a blank separator line, collapsing consecutive blanks.
    fn blank(&mut self) {
        // an image's rows are blank lines, but not a separator
        let after_image = self
            .placed
            .last()
            .is_some_and(|image| image.line + image.rows as usize == self.lines.len());
        if !self.lines.is_empty()
            && (after_image || self.lines.last().map(|l| l.spans.is_empty()) != Some(true))
        {
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

    fn finish(mut self) -> (Vec<Line<'static>>, Vec<ImageAt>) {
        self.flush();
        self.place_images();
        // trailing blank lines go, but not those an image is drawn over
        let keep = self
            .placed
            .last()
            .map_or(0, |image| image.line + image.rows as usize);
        while self.lines.len() > keep && self.lines.last().map(|l| l.spans.is_empty()) == Some(true)
        {
            self.lines.pop();
        }
        if self.lines.is_empty() {
            self.lines.push(Line::from(Span::styled(
                "(empty post)",
                Style::default().fg(DIM),
            )));
        }
        (self.lines, self.placed)
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
    fn rows_are_reserved_below_the_line_of_an_image() {
        let md = "Look: ![a cat](cat.png) here\n\n![remote](https://x/y.png)\n\nafter\n";
        let mut asked = Vec::new();
        let (lines, placed) = render_with(md, &mut |url| {
            asked.push(url.to_string());
            (url == "cat.png").then_some(2)
        });
        assert_eq!(asked, ["cat.png", "https://x/y.png"]);
        let text = plain(&lines);
        let caption = text
            .iter()
            .position(|l| l.contains("[image: a cat]"))
            .unwrap();
        assert_eq!(text[caption], "Look: [image: a cat] here");
        assert_eq!(
            placed,
            [ImageAt {
                url: "cat.png".into(),
                line: caption + 1,
                rows: 2
            }]
        );
        assert_eq!(&text[caption + 1..caption + 3], ["", ""]);
        // an image that cannot be shown keeps its text only
        assert!(text.contains(&"[image: remote]".to_string()));
        assert_eq!(text.last().unwrap(), "after");
        // plain render: the same text, no rows reserved
        assert_eq!(plain(&render(md)).len(), text.len() - 2);
    }

    #[test]
    fn an_image_at_the_end_keeps_its_rows() {
        let (lines, placed) = render_with("![x](x.png)\n", &mut |_| Some(3));
        assert_eq!(placed[0].line, 1);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn images_in_table_cells_stay_text() {
        let md = "| a |\n|---|\n| ![x](x.png) |\n";
        let (_, placed) = render_with(md, &mut |_| Some(1));
        assert!(placed.is_empty());
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
}
