//! A block tree to a flat display list: positioned rectangles
//! and styled runs, in pixels.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::font;
use super::parse::{Align, Block, Callout, Cell, Doc, Footnote, Inline, ListItem, Style, Table};

/// A colour, as red, green, blue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// A page of drawing instructions, in painter's order.
#[derive(Debug, Default)]
pub struct Page {
    pub w: f32,
    pub h: f32,
    pub items: Vec<Item>,
}

#[derive(Debug)]
pub enum Item {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Rgb,
    },
    Run {
        x: f32,
        baseline: f32,
        text: String,
        size: f32,
        bold: bool,
        italic: bool,
        strike: bool,
        fill: Rgb,
    },
}

/// The bar and label colour of each kind of callout.
pub struct Callouts {
    pub note: Rgb,
    pub tip: Rgb,
    pub important: Rgb,
    pub warning: Rgb,
    pub caution: Rgb,
}

impl Callouts {
    fn of(&self, c: Callout) -> Rgb {
        match c {
            Callout::Note => self.note,
            Callout::Tip => self.tip,
            Callout::Important => self.important,
            Callout::Warning => self.warning,
            Callout::Caution => self.caution,
        }
    }
}

pub struct Theme {
    pub bg: Rgb,
    pub fg: Rgb,
    pub dim: Rgb,
    pub link: Rgb,
    pub code_fg: Rgb,
    pub code_bg: Rgb,
    pub rule: Rgb,
    pub quote_bar: Rgb,
    pub callout: Callouts,
    pub base_size: f32,
    pub heading_scale: [f32; 6],
    pub line_ratio: f32,
    pub margin: f32,
}

impl Theme {
    pub const DARK: Theme = Theme {
        bg: Rgb(0x1e, 0x1e, 0x1e),
        fg: Rgb(0xd4, 0xd4, 0xd4),
        dim: Rgb(0x85, 0x85, 0x85),
        link: Rgb(0x6c, 0xa9, 0xef),
        code_fg: Rgb(0xce, 0x91, 0x78),
        code_bg: Rgb(0x2a, 0x2a, 0x2a),
        rule: Rgb(0x3a, 0x3a, 0x3a),
        quote_bar: Rgb(0x4a, 0x4a, 0x4a),
        callout: Callouts {
            note: Rgb(0x53, 0x9b, 0xf5),
            tip: Rgb(0x57, 0xab, 0x5a),
            important: Rgb(0x98, 0x6e, 0xe2),
            warning: Rgb(0xc6, 0x90, 0x26),
            caution: Rgb(0xe5, 0x53, 0x4b),
        },
        base_size: 16.0,

        // TODO: these three were eyeballed against a terminal,
        //       not derived from anything
        heading_scale: [2.0, 1.6, 1.3, 1.15, 1.0, 1.0],
        line_ratio: 1.45,
        margin: 16.0,
    };
}

/// Lay `doc` out into a page exactly `width` pixels across.
pub fn layout(doc: &Doc, theme: &Theme, width: f32) -> Page {
    let mut c = Cursor {
        theme,
        y: theme.margin,
        items: Vec::new(),
    };
    let content_w = (width - 2.0 * theme.margin).max(1.0);
    c.blocks(&doc.blocks, theme.margin, content_w);

    Page {
        w: width,
        h: c.y + theme.margin,
        items: c.items,
    }
}

struct Cursor<'a> {
    theme: &'a Theme,
    y: f32,
    items: Vec<Item>,
}

impl Cursor<'_> {
    fn blocks(&mut self, blocks: &[Block], x: f32, w: f32) {
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 {
                // TODO: this and the other spacing multipliers
                //       in this impl were eyeballed
                self.y += self.theme.base_size * 0.6;
            }
            self.block(b, x, w);
        }
    }

    fn block(&mut self, b: &Block, x: f32, w: f32) {
        match b {
            Block::Heading { level, inlines } => {
                // heading_scale has six entries, one per level
                let size = self.theme.base_size
                    * self.theme.heading_scale[(*level as usize - 1).min(5)];

                // space above a heading, but not at the top of a page
                if self.y > self.theme.margin {
                    self.y += size * 0.4;
                }
                let style = Style {
                    bold: true,
                    ..Style::default()
                };
                self.flow(inlines, x, w, size, style);
            }
            Block::Paragraph(inlines) => {
                self.flow(inlines, x, w, self.theme.base_size, Style::default());
            }
            Block::Code(lines) => self.code(lines, x, w),
            Block::Quote { callout, blocks } => self.quote(*callout, blocks, x, w),
            Block::List { start, items } => self.list(*start, items, x, w),
            Block::Footnotes(notes) => self.footnotes(notes, x, w),
            Block::Table(t) => self.table(t, x, w),
            Block::Rule => {
                let size = self.theme.base_size;
                self.y += size * 0.5;
                self.items.push(Item::Rect {
                    x,
                    y: self.y,
                    w,
                    h: 1.0,
                    fill: self.theme.rule,
                });
                self.y += size * 0.5 + 1.0;
            }
        }
    }

    fn flow(&mut self, inlines: &[Inline], x: f32, w: f32, size: f32, base: Style) {
        let advance = size * font::ADVANCE_RATIO;
        let cols = ((w / advance).floor() as usize).max(1);
        let line_h = size * self.theme.line_ratio;

        for line in wrap(inlines, cols, base) {
            let baseline = self.y + size * font::ASCENT;
            for piece in line {
                self.items.push(Item::Run {
                    x: x + piece.col as f32 * advance,
                    baseline,
                    size,
                    bold: piece.style.bold,
                    italic: piece.style.italic,
                    strike: piece.style.strike,
                    fill: self.colour(&piece.style),
                    text: piece.text,
                });
            }
            self.y += line_h;
        }
    }

    fn colour(&self, style: &Style) -> Rgb {
        if style.code {
            self.theme.code_fg
        } else if style.link {
            self.theme.link
        } else {
            self.theme.fg
        }
    }

    fn code(&mut self, lines: &[String], x: f32, w: f32) {
        let size = self.theme.base_size;
        let advance = size * font::ADVANCE_RATIO;
        let line_h = size * self.theme.line_ratio;
        let pad = size * 0.5;
        let cols = ((w - 2.0 * pad) / advance).floor().max(1.0) as usize;

        // code never soft-wraps: tabs expand to columns, then an
        // over-long line is broken onto further lines
        let drawn: Vec<String> = lines
            .iter()
            .flat_map(|l| hard_split(&expand_tabs(l), cols))
            .collect();

        let h = drawn.len() as f32 * line_h + 2.0 * pad;
        self.items.push(Item::Rect {
            x,
            y: self.y,
            w,
            h,
            fill: self.theme.code_bg,
        });

        let mut baseline = self.y + pad + size * font::ASCENT;
        for line in drawn {
            if !line.is_empty() {
                self.items.push(Item::Run {
                    x: x + pad,
                    baseline,
                    text: line,
                    size,
                    bold: false,
                    italic: false,
                    strike: false,
                    fill: self.theme.code_fg,
                });
            }
            baseline += line_h;
        }
        self.y += h;
    }

    fn quote(&mut self, callout: Option<Callout>, inner: &[Block], x: f32, w: f32) {
        let size = self.theme.base_size;
        let indent = size * font::ADVANCE_RATIO * 2.0;
        let bar = match callout {
            Some(c) => self.theme.callout.of(c),
            None => self.theme.quote_bar,
        };

        // the bar's height is not known until the contents are
        // laid out, so reserve its slot and insert it after
        let at = self.items.len();
        let y0 = self.y;

        if let Some(c) = callout {
            self.items.push(Item::Run {
                x: x + indent,
                baseline: self.y + size * font::ASCENT,
                text: c.label().to_owned(),
                size,
                bold: true,
                italic: false,
                strike: false,
                fill: bar,
            });
            self.y += size * self.theme.line_ratio;
        }

        self.blocks(inner, x + indent, (w - indent).max(1.0));

        self.items.insert(
            at,
            Item::Rect {
                x,
                y: y0,
                w: (size * 0.15).max(2.0),
                h: (self.y - y0).max(1.0),
                fill: bar,
            },
        );
    }

    fn list(&mut self, start: Option<u64>, items: &[ListItem], x: f32, w: f32) {
        let size = self.theme.base_size;

        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.y += size * 0.3;
            }
            // squares, not U+2610 BALLOT BOX: Liberation
            // Mono has no ballot box and no check mark, and
            // a missing glyph draws as nothing at all
            let marker = match (item.task, start) {
                (Some(true), _) => "\u{25a0}".to_owned(),
                (Some(false), _) => "\u{25a1}".to_owned(),
                (None, Some(n)) => format!("{}.", n + i as u64),
                (None, None) => "\u{2022}".to_owned(),
            };
            self.marked(marker, &item.blocks, x, w);
        }
    }

    fn table(&mut self, t: &Table, x: f32, w: f32) {
        let size = self.theme.base_size;
        let advance = size * font::ADVANCE_RATIO;

        let n = t
            .rows
            .iter()
            .map(Vec::len)
            .chain(std::iter::once(t.head.len()))
            .max()
            .unwrap_or(0);
        if n == 0 {
            return;
        }

        let cols = ((w / advance).floor() as usize).max(1);
        let widths = column_widths(t, n, cols);

        // where each column begins, in characters from the
        // table's left edge
        let mut starts = Vec::with_capacity(n);
        let mut at = 0;
        for width in &widths {
            starts.push(at);
            at += width + TABLE_GAP;
        }
        let span = at - TABLE_GAP;

        if !t.head.is_empty() {
            let bold = Style {
                bold: true,
                ..Style::default()
            };
            self.table_row(&t.head, &widths, &starts, &t.align, x, bold);

            self.y += size * 0.2;
            self.items.push(Item::Rect {
                x,
                y: self.y,
                w: span as f32 * advance,
                h: 1.0,
                fill: self.theme.rule,
            });
            self.y += 1.0 + size * 0.2;
        }

        for row in &t.rows {
            self.table_row(row, &widths, &starts, &t.align, x, Style::default());
        }
    }

    /// Draw one row, every cell wrapped to its own column width.
    fn table_row(
        &mut self,
        cells: &[Cell],
        widths: &[usize],
        starts: &[usize],
        align: &[Align],
        x: f32,
        base: Style,
    ) {
        let size = self.theme.base_size;
        let advance = size * font::ADVANCE_RATIO;
        let line_h = size * self.theme.line_ratio;

        let wrapped: Vec<Vec<Vec<Piece>>> = cells
            .iter()
            .enumerate()
            .map(|(i, cell)| wrap(cell, widths.get(i).copied().unwrap_or(1), base))
            .collect();

        // a row is as tall as its deepest cell
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(0);

        for j in 0..height {
            let baseline = self.y + size * font::ASCENT;
            for (i, lines) in wrapped.iter().enumerate() {
                let Some(line) = lines.get(j) else { continue };
                let width = widths.get(i).copied().unwrap_or(1);
                let used = line.last().map_or(0, |p| p.col + p.text.width());
                let pad = match align.get(i).copied().unwrap_or_default() {
                    Align::Left => 0,
                    Align::Center => width.saturating_sub(used) / 2,
                    Align::Right => width.saturating_sub(used),
                };
                for piece in line {
                    self.items.push(Item::Run {
                        x: x + (starts[i] + pad + piece.col) as f32 * advance,
                        baseline,
                        size,
                        bold: piece.style.bold,
                        italic: piece.style.italic,
                        strike: piece.style.strike,
                        fill: self.colour(&piece.style),
                        text: piece.text.clone(),
                    });
                }
            }
            self.y += line_h;
        }
    }

    fn footnotes(&mut self, notes: &[Footnote], x: f32, w: f32) {
        let size = self.theme.base_size;
        for (i, note) in notes.iter().enumerate() {
            if i > 0 {
                self.y += size * 0.3;
            }
            self.marked(format!("{}.", note.number), &note.blocks, x, w);
        }
    }

    /// Draw `blocks` indented past `marker`, with the marker on the first
    /// line's baseline.
    fn marked(&mut self, marker: String, blocks: &[Block], x: f32, w: f32) {
        let size = self.theme.base_size;
        let advance = size * font::ADVANCE_RATIO;
        let indent = (marker.width() + 1) as f32 * advance;

        // the marker sits on the first line's baseline, which
        // exists only once the body has been placed
        let at = self.items.len();
        let y0 = self.y;
        self.blocks(blocks, x + indent, (w - indent).max(1.0));

        self.items.insert(
            at,
            Item::Run {
                x,
                baseline: y0 + size * font::ASCENT,
                text: marker,
                size,
                bold: false,
                italic: false,
                strike: false,
                fill: self.theme.dim,
            },
        );
    }
}

/// Blank characters between one table column and the next.
const TABLE_GAP: usize = 2;

/// Column widths in characters: each column's natural width, then the widest
/// shrunk one at a time until the row fits `total`.
fn column_widths(t: &Table, n: usize, total: usize) -> Vec<usize> {
    let mut widths = vec![1usize; n];
    for row in std::iter::once(&t.head).chain(t.rows.iter()) {
        for (i, cell) in row.iter().enumerate().take(n) {
            widths[i] = widths[i].max(natural_width(cell));
        }
    }

    // shrink the widest, so a column of prose wraps before a
    // column of short keys is squeezed to nothing
    let avail = total.saturating_sub(TABLE_GAP * (n - 1)).max(n);
    while widths.iter().sum::<usize>() > avail {
        let Some((at, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, w)| **w > 1)
            .max_by_key(|(_, w)| **w)
        else {
            break;
        };
        widths[at] -= 1;
    }
    widths
}

/// The width a cell would take if it were never wrapped.
fn natural_width(cell: &[Inline]) -> usize {
    let mut w = 0;
    let mut first = true;
    for tok in tokens(cell, Style::default()) {
        match tok {
            Tok::Word {
                text, space_before, ..
            } => {
                w += text.width() + usize::from(space_before && !first);
                first = false;
            }
            Tok::Break => w += 1,
        }
    }
    w
}

/// One styled fragment on a line, positioned in columns from the left margin.
#[derive(Debug, PartialEq)]
struct Piece {
    col: usize,
    text: String,
    style: Style,
}

/// Greedy line breaking over a flattened token stream.
fn wrap(inlines: &[Inline], cols: usize, base: Style) -> Vec<Vec<Piece>> {
    let mut lines: Vec<Vec<Piece>> = Vec::new();
    let mut line: Vec<Piece> = Vec::new();
    let mut col = 0usize;

    for token in tokens(inlines, base) {
        let Tok::Word {
            text,
            style,
            space_before,
        } = token
        else {
            lines.push(std::mem::take(&mut line));
            col = 0;
            continue;
        };

        // the incoming space goes before the first chunk only;
        // the rest are continuations of the same word
        let mut spaced = space_before;
        for chunk in hard_split(&text, cols) {
            let width = chunk.width();
            let gap = usize::from(spaced && col > 0);
            if col > 0 && col + gap + width > cols {
                lines.push(std::mem::take(&mut line));
                col = 0;
            }

            // recomputed because the wrap above may have reset
            // col to 0, where a space never survives
            let gap = usize::from(spaced && col > 0);
            let at = col + gap;
            place(&mut line, at, &chunk, style, gap == 1);
            col = at + width;
            spaced = false;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Append `text` at column `at`, joining the previous piece when the
/// style matches.
fn place(line: &mut Vec<Piece>, at: usize, text: &str, style: Style, spaced: bool) {
    // one run per sentence rather than one per word
    if let Some(p) = line.last_mut()
        && p.style == style
        && p.col + p.text.width() + usize::from(spaced) == at
    {
        if spaced {
            p.text.push(' ');
        }
        p.text.push_str(text);
        return;
    }
    line.push(Piece {
        col: at,
        text: text.to_owned(),
        style,
    });
}

enum Tok {
    Word {
        text: String,
        style: Style,
        /// Whether whitespace actually separated this word from the last one.
        //
        // markdown splits `**bold**, more` into a bold run and a
        // plain run beginning with a comma, so inferring a space
        // from "is anything to the left" spaces off punctuation
        space_before: bool,
    },
    Break,
}

/// Flatten inlines to words, remembering where the whitespace was.
fn tokens(inlines: &[Inline], base: Style) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut gap = false;

    for inline in inlines {
        match inline {
            Inline::Break => {
                out.push(Tok::Break);
                gap = false;
            }
            Inline::Image { alt } => {
                out.push(Tok::Word {
                    text: format!("[{alt}]"),
                    style: Style {
                        italic: true,
                        ..base
                    },
                    space_before: gap,
                });
                gap = false;
            }
            // the link colour, because a reference points
            // somewhere and Style carries no dim of its own
            Inline::Note { number } => {
                out.push(Tok::Word {
                    text: format!("[{number}]"),
                    style: Style { link: true, ..base },
                    space_before: gap,
                });
                gap = false;
            }
            Inline::Text { text, style } => {
                let style = Style {
                    bold: style.bold || base.bold,
                    italic: style.italic || base.italic,
                    ..*style
                };
                let mut first = true;
                for word in text.split_whitespace() {
                    out.push(Tok::Word {
                        text: word.to_owned(),
                        style,
                        space_before: if first {
                            gap || text.starts_with(char::is_whitespace)
                        } else {
                            true
                        },
                    });
                    first = false;
                }

                // a run of only whitespace still separates its
                // neighbours, so gap stays set for the next one
                gap = if first {
                    gap || !text.is_empty()
                } else {
                    text.ends_with(char::is_whitespace)
                };
            }
        }
    }
    out
}

/// Break `s` into chunks of at most `cols` display columns, never
/// splitting a `char`.
fn hard_split(s: &str, cols: usize) -> Vec<String> {
    if s.width() <= cols {
        return vec![s.to_owned()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > cols && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            w = 0;
        }
        cur.push(c);
        w += cw;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Tabs expanded to the next multiple of four.
fn expand_tabs(s: &str) -> String {
    if !s.contains('\t') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 8);
    let mut col = 0usize;
    for c in s.chars() {
        if c == '\t' {
            let n = 4 - col % 4;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += c.width().unwrap_or(0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(text: &str) -> Cell {
        vec![Inline::Text {
            text: text.into(),
            style: Style::default(),
        }]
    }

    #[test]
    fn a_table_that_fits_keeps_every_column_at_its_natural_width() {
        let t = Table {
            align: vec![Align::Left; 2],
            head: vec![cell("key"), cell("value")],
            rows: vec![vec![cell("a"), cell("bb")]],
        };
        assert_eq!(column_widths(&t, 2, 80), vec![3, 5]);
    }

    #[test]
    fn a_table_too_wide_shrinks_its_widest_column_first() {
        // 6 + 2 gap + 40 is 48, so eight characters have to
        // come off, and all of them off the prose column
        let t = Table {
            align: vec![Align::Left; 2],
            head: vec![cell("symbol"), cell(&"x".repeat(40))],
            rows: vec![],
        };
        assert_eq!(column_widths(&t, 2, 40), vec![6, 32]);
    }

    #[test]
    fn a_table_far_too_narrow_stops_at_one_character_a_column() {
        let t = Table {
            align: vec![Align::Left; 3],
            head: vec![cell("aaaa"), cell("bbbb"), cell("cccc")],
            rows: vec![],
        };
        assert_eq!(column_widths(&t, 3, 4), vec![1, 1, 1]);
    }

    #[test]
    fn a_cells_natural_width_counts_the_spaces_between_its_words() {
        assert_eq!(natural_width(&cell("one two")), 7);
        assert_eq!(natural_width(&cell("")), 0);
    }
    use crate::source::markdown::parse;

    fn text(s: &str) -> Vec<Inline> {
        vec![Inline::Text {
            text: s.into(),
            style: Style::default(),
        }]
    }

    fn lines_of(inlines: &[Inline], cols: usize) -> Vec<String> {
        wrap(inlines, cols, Style::default())
            .iter()
            .map(|line| {
                line.iter()
                    .map(|p| p.text.as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    #[test]
    fn wrapping_breaks_at_the_column_and_not_before() {
        // "aaa bbb ccc" is 11 columns; at 7 it takes two lines, at 11 one
        let t = text("aaa bbb ccc");
        assert_eq!(lines_of(&t, 11), vec!["aaa bbb ccc"]);
        assert_eq!(lines_of(&t, 7), vec!["aaa bbb", "ccc"]);
        assert_eq!(lines_of(&t, 3), vec!["aaa", "bbb", "ccc"]);
    }

    #[test]
    fn a_line_never_starts_with_the_space_it_broke_on() {
        let t = text("aaa bbb");
        let wrapped = wrap(&t, 3, Style::default());
        assert_eq!(wrapped[1][0].col, 0, "second line was indented by a space");
        assert_eq!(wrapped[1][0].text, "bbb");
    }

    #[test]
    fn a_word_longer_than_the_line_is_split_rather_than_lost() {
        assert_eq!(lines_of(&text("abcdefghij"), 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn east_asian_text_counts_two_columns_per_character() {
        // the whole column model is display width, not chars: four CJK
        // characters fill eight columns, so they wrap at four per line
        assert_eq!(lines_of(&text("世界世界"), 8), vec!["世界世界"]);
        assert_eq!(lines_of(&text("世界世界"), 4), vec!["世界", "世界"]);
    }

    #[test]
    fn a_style_change_starts_a_new_run_but_matching_styles_merge() {
        let inlines = vec![
            Inline::Text {
                text: "plain ".into(),
                style: Style::default(),
            },
            Inline::Text {
                text: "loud".into(),
                style: Style {
                    bold: true,
                    ..Style::default()
                },
            },
            Inline::Text {
                text: " plain again".into(),
                style: Style::default(),
            },
        ];
        let line = &wrap(&inlines, 40, Style::default())[0];
        assert_eq!(line.len(), 3, "runs did not split on style: {line:?}");
        assert_eq!(line[0].col, 0);
        assert_eq!(line[1].text, "loud");
        assert_eq!(line[1].col, 6);
        assert_eq!(line[2].text, "plain again");
    }

    #[test]
    fn punctuation_after_emphasis_does_not_gain_a_space() {
        // markdown hands "**bold**, then" over as two runs, the
        // second starting with the comma; spacing them apart is
        // the bug guarded here
        let inlines = vec![
            Inline::Text {
                text: "bold".into(),
                style: Style {
                    bold: true,
                    ..Style::default()
                },
            },
            Inline::Text {
                text: ", then".into(),
                style: Style::default(),
            },
        ];
        let line = &wrap(&inlines, 40, Style::default())[0];
        assert_eq!(line[0].col, 0);
        assert_eq!(line[1].text, ", then");
        assert_eq!(line[1].col, 4, "comma was pushed off the word it follows");
    }

    #[test]
    fn a_space_between_runs_is_still_honoured() {
        let inlines = vec![
            Inline::Text {
                text: "bold".into(),
                style: Style {
                    bold: true,
                    ..Style::default()
                },
            },
            Inline::Text {
                text: " and more".into(),
                style: Style::default(),
            },
        ];
        let line = &wrap(&inlines, 40, Style::default())[0];
        assert_eq!(line[1].text, "and more");
        assert_eq!(line[1].col, 5, "the real space between runs was dropped");
    }

    #[test]
    fn tabs_expand_to_the_next_stop_of_four() {
        assert_eq!(expand_tabs("a\tb"), "a   b");
        assert_eq!(expand_tabs("abcd\te"), "abcd    e");
        assert_eq!(expand_tabs("\tx"), "    x");
    }

    #[test]
    fn a_heading_is_taller_than_body_text() {
        let doc = parse::parse("# Big\n\nsmall\n");
        let page = layout(&doc, &Theme::DARK, 800.0);
        let sizes: Vec<f32> = page
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Run { size, .. } => Some(*size),
                _ => None,
            })
            .collect();
        assert_eq!(sizes.len(), 2);
        assert!(sizes[0] > sizes[1], "heading did not scale: {sizes:?}");
    }

    #[test]
    fn a_taller_document_reports_a_taller_page() {
        let short = layout(&parse::parse("one\n"), &Theme::DARK, 800.0);
        let long = layout(
            &parse::parse("one\n\ntwo\n\nthree\n\nfour\n"),
            &Theme::DARK,
            800.0,
        );
        assert!(long.h > short.h, "{} !> {}", long.h, short.h);
    }

    #[test]
    fn a_code_block_draws_its_background_before_its_text() {
        // painter's order: a rect emitted after the runs would hide them
        let doc = parse::parse("```\nfn main() {}\n```\n");
        let page = layout(&doc, &Theme::DARK, 800.0);
        let first_rect = page
            .items
            .iter()
            .position(|i| matches!(i, Item::Rect { .. }));
        let first_run = page.items.iter().position(|i| matches!(i, Item::Run { .. }));
        assert!(first_rect < first_run, "background would cover the code");
    }

    #[test]
    fn a_quote_bar_spans_everything_inside_it() {
        let doc = parse::parse("> one\n>\n> two\n");
        let page = layout(&doc, &Theme::DARK, 800.0);
        let Some(Item::Rect { y, h, .. }) = page.items.first() else {
            panic!("expected the bar first, got {:?}", page.items.first());
        };
        let lowest = page
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Run { baseline, .. } => Some(*baseline),
                _ => None,
            })
            .fold(f32::MIN, f32::max);
        assert!(y + h >= lowest, "bar stops short of the quoted text");
    }
}
