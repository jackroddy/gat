//! A block tree to a flat display list.
//
// This is the seam the design turns on. Everything above is markdown and
// everything below is SVG, but this file is neither: it produces positioned
// rectangles and styled runs in pixels. Replacing the SVG backend later means
// rewriting to_svg.rs and leaving this and its tests alone.
//
// Wrapping works in whole columns and converts to pixels only when a run is
// emitted. That is not an optimisation, it is what makes the tests readable:
// "this wraps at 40 columns" is exact, where a float-pixel assertion is a
// guess with a tolerance attached.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::font;
use super::parse::{Block, Doc, Inline, Style};

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

pub struct Theme {
    pub bg: Rgb,
    pub fg: Rgb,
    pub dim: Rgb,
    pub link: Rgb,
    pub code_fg: Rgb,
    pub code_bg: Rgb,
    pub rule: Rgb,
    pub quote_bar: Rgb,
    /// Body text size in pixels; headings scale off it.
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
        base_size: 16.0,
        heading_scale: [2.0, 1.6, 1.3, 1.15, 1.0, 1.0],
        line_ratio: 1.45,
        margin: 16.0,
    };
}

/// Lay `doc` out into a page exactly `width` pixels across.
//
// the height falls out of the content; the caller decides how much of it to
// actually draw.
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
                self.y += self.theme.base_size * 0.6;
            }
            self.block(b, x, w);
        }
    }

    fn block(&mut self, b: &Block, x: f32, w: f32) {
        match b {
            Block::Heading { level, inlines } => {
                let size = self.theme.base_size
                    * self.theme.heading_scale[(*level as usize - 1).min(5)];
                // a heading needs air above it, but not at the top of a page
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
            Block::Quote(inner) => self.quote(inner, x, w),
            Block::List { start, items } => self.list(*start, items, x, w),
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

    /// Wrap `inlines` into `w` pixels at `size` and emit the runs.
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

        // code never soft-wraps; tabs become columns first, then anything
        // still too long is cut at the edge rather than silently lost
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

    fn quote(&mut self, inner: &[Block], x: f32, w: f32) {
        let size = self.theme.base_size;
        let indent = size * font::ADVANCE_RATIO * 2.0;
        // the bar's height is not known until the contents are laid out, so
        // remember where it belongs and insert it once they are
        let at = self.items.len();
        let y0 = self.y;

        self.blocks(inner, x + indent, (w - indent).max(1.0));

        self.items.insert(
            at,
            Item::Rect {
                x,
                y: y0,
                w: (size * 0.15).max(2.0),
                h: (self.y - y0).max(1.0),
                fill: self.theme.quote_bar,
            },
        );
    }

    fn list(&mut self, start: Option<u64>, items: &[Vec<Block>], x: f32, w: f32) {
        let size = self.theme.base_size;
        let advance = size * font::ADVANCE_RATIO;

        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.y += size * 0.3;
            }
            let marker = match start {
                Some(n) => format!("{}.", n + i as u64),
                None => "\u{2022}".to_owned(),
            };
            let indent = (marker.width() + 1) as f32 * advance;

            // the marker sits on the first line's baseline, which only exists
            // once that line has been placed, so draw the body first and
            // measure back to it
            let at = self.items.len();
            let y0 = self.y;
            self.blocks(item, x + indent, (w - indent).max(1.0));

            let baseline = y0 + size * font::ASCENT;
            self.items.insert(
                at,
                Item::Run {
                    x,
                    baseline,
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

        // only the first chunk of a split word inherits the incoming space;
        // the rest are continuations of it
        let mut spaced = space_before;
        for chunk in hard_split(&text, cols) {
            let width = chunk.width();
            let gap = usize::from(spaced && col > 0);
            if col > 0 && col + gap + width > cols {
                lines.push(std::mem::take(&mut line));
                col = 0;
            }
            // a space never survives to the start of a line
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

/// Append `text` at column `at`, joining the previous piece when the style
/// matches so a sentence is one run rather than one run per word.
fn place(line: &mut Vec<Piece>, at: usize, text: &str, style: Style, spaced: bool) {
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

/// One word, or a forced line break.
enum Tok {
    Word {
        text: String,
        style: Style,
        /// Whether whitespace actually separated this word from the last one.
        //
        // this cannot be inferred from "is there anything to the left", which
        // is the tempting shortcut. markdown splits `**bold**, more` into a
        // bold run and a plain run beginning with a comma, and inserting a
        // space between them because both are non-empty puts a gap before
        // every piece of punctuation that follows emphasis
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
                // a run that is nothing but whitespace still separates its
                // neighbours, so the gap it leaves has to outlive it
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

/// Break `s` into chunks of at most `cols` display columns, never splitting a
/// `char`. A string that already fits comes back whole.
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

/// Tabs to the next multiple of four. SVG has no tab, so this has to happen
/// before the text is measured or drawn.
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
        // "plain again" is two words at one style and must be a single run
        assert_eq!(line[2].text, "plain again");
    }

    #[test]
    fn punctuation_after_emphasis_does_not_gain_a_space() {
        // markdown hands "**bold**, then" over as two runs, the second of
        // which starts with the comma. Spacing them apart because both are
        // non-empty is the bug this guards
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
