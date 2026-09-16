//! Markdown events to a block tree.
//
// nothing here deals in pixels, fonts or SVG

use pulldown_cmark::{
    Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};

/// A parsed document: blocks in reading order, nested where markdown nests.
#[derive(Debug, Default, PartialEq)]
pub struct Doc {
    pub blocks: Vec<Block>,
}

#[derive(Debug, PartialEq)]
pub enum Block {
    Heading { level: u8, inlines: Vec<Inline> },
    Paragraph(Vec<Inline>),
    /// Already split on newlines, with tabs still in place.
    Code(Vec<String>),
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Quote(Vec<Block>),
    Table(Table),
    Rule,
}

/// The inline content of one table cell.
pub type Cell = Vec<Inline>;

#[derive(Debug, Default, PartialEq)]
pub struct Table {
    /// One entry per column, from the delimiter row.
    pub align: Vec<Align>,
    pub head: Vec<Cell>,
    pub rows: Vec<Vec<Cell>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Inline content, with emphasis already flattened onto each run.
#[derive(Debug, PartialEq)]
pub enum Inline {
    // markdown nests emphasis; bold inside italic is one run
    // that is both, which keeps a style stack out of layout
    Text { text: String, style: Style },

    /// Rendered as its alt text.
    Image { alt: String },
    Break,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: bool,
}

pub fn parse(text: &str) -> Doc {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);

    let mut b = Builder::default();
    for event in Parser::new_ext(text, opts) {
        b.event(event);
    }
    b.finish()
}

/// What sort of container is open, for the block stack to close back into.
enum Open {
    Quote,
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Item,
}

/// Which block the inlines arriving now belong to.
#[derive(Clone, Copy, PartialEq)]
enum Inlines {
    Paragraph,
    Heading(u8),
    /// Between `Start(Image)` and `End(Image)`, where text is alt text.
    ImageAlt,
}

#[derive(Default)]
struct Builder {
    /// One `Vec<Block>` per open container, innermost last. Index 0 is the
    /// document itself and is never popped.
    levels: Vec<Vec<Block>>,
    opens: Vec<Open>,

    inlines: Vec<Inline>,
    where_: Option<Inlines>,
    alt: String,

    // emphasis nests, so these count rather than toggle
    bold: u32,
    italic: u32,
    strike: u32,
    link: u32,

    code: Option<String>,

    table: Option<Table>,

    /// Cells of the row being read, for whichever of head or rows it joins.
    row: Vec<Cell>,
}

impl Builder {
    fn event(&mut self, event: Event<'_>) {
        if self.levels.is_empty() {
            self.levels.push(Vec::new());
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),

            // a fenced block arrives as several Text events and
            // must be rejoined before splitting on newlines
            Event::Text(t) if self.code.is_some() => {
                self.code.as_mut().unwrap().push_str(&t);
            }
            Event::Text(t) => self.text(&t, self.style()),
            Event::Code(t) => {
                let mut style = self.style();
                style.code = true;
                self.text(&t, style);
            }

            // the generated SVG goes to a parser, so passing
            // markup through would let a document inject elements
            Event::Html(_) => {}
            Event::InlineHtml(t) => self.text(&t, self.style()),

            // we re-wrap everything, so a soft break is just a space
            Event::SoftBreak => self.text(" ", self.style()),
            Event::HardBreak => {
                self.open_inlines();
                self.inlines.push(Inline::Break);
            }

            Event::Rule => self.push(Block::Rule),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.where_ = Some(Inlines::Paragraph),
            Tag::Heading { level, .. } => {
                self.where_ = Some(Inlines::Heading(heading_level(level)));
            }
            Tag::CodeBlock(_) => self.code = Some(String::new()),
            Tag::BlockQuote(_) => {
                self.opens.push(Open::Quote);
                self.levels.push(Vec::new());
            }
            Tag::List(start) => self.opens.push(Open::List {
                start,
                items: Vec::new(),
            }),
            Tag::Item => {
                self.opens.push(Open::Item);
                self.levels.push(Vec::new());
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { .. } => self.link += 1,
            Tag::Image { .. } => {
                self.where_ = Some(Inlines::ImageAlt);
                self.alt.clear();
            }

            Tag::Table(align) => {
                self.table = Some(Table {
                    align: align.iter().copied().map(align_of).collect(),
                    ..Table::default()
                });
            }

            // a cell's content arrives as ordinary inlines,
            // so it needs somewhere for them to land
            Tag::TableCell => self.where_ = Some(Inlines::Paragraph),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                let inlines = std::mem::take(&mut self.inlines);
                self.where_ = None;
                if !inlines.is_empty() {
                    self.push(Block::Paragraph(inlines));
                }
            }
            TagEnd::Heading(level) => {
                let inlines = std::mem::take(&mut self.inlines);
                self.where_ = None;
                self.push(Block::Heading {
                    level: heading_level(level),
                    inlines,
                });
            }
            TagEnd::CodeBlock => {
                let body = self.code.take().unwrap_or_default();

                // a trailing newline is the fence's, not a blank last line
                let body = body.strip_suffix('\n').unwrap_or(&body);
                let lines = body.split('\n').map(str::to_owned).collect();
                self.push(Block::Code(lines));
            }
            TagEnd::BlockQuote(_) => {
                let inner = self.levels.pop().unwrap_or_default();
                self.opens.pop();
                self.push(Block::Quote(inner));
            }
            TagEnd::Item => {
                self.flush_loose_inlines();
                let item = self.levels.pop().unwrap_or_default();
                self.opens.pop();
                if let Some(Open::List { items, .. }) = self.opens.last_mut() {
                    items.push(item);
                }
            }
            TagEnd::List(_) => {
                if let Some(Open::List { start, items }) = self.opens.pop() {
                    self.push(Block::List { start, items });
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = self.link.saturating_sub(1),
            TagEnd::Image => {
                let alt = std::mem::take(&mut self.alt);
                self.where_ = None;
                self.open_inlines();
                self.inlines.push(Inline::Image { alt });
            }

            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.inlines);
                self.where_ = None;
                self.row.push(cell);
            }

            // the head emits its cells directly, with no row
            // of its own around them
            TagEnd::TableHead => {
                let row = std::mem::take(&mut self.row);
                if let Some(t) = self.table.as_mut() {
                    t.head = row;
                }
            }
            TagEnd::TableRow => {
                let row = std::mem::take(&mut self.row);
                if let Some(t) = self.table.as_mut() {
                    t.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.push(Block::Table(t));
                }
            }
            _ => {}
        }
    }

    fn style(&self) -> Style {
        Style {
            bold: self.bold > 0,
            italic: self.italic > 0,
            code: false,
            strike: self.strike > 0,
            link: self.link > 0,
        }
    }

    fn text(&mut self, t: &str, style: Style) {
        if self.where_ == Some(Inlines::ImageAlt) {
            self.alt.push_str(t);
            return;
        }
        self.open_inlines();

        // runs that agree on style merge into one
        match self.inlines.last_mut() {
            Some(Inline::Text { text, style: s }) if *s == style => text.push_str(t),
            _ => self.inlines.push(Inline::Text {
                text: t.to_owned(),
                style,
            }),
        }
    }

    /// Open an implicit paragraph if no block is taking inlines.
    fn open_inlines(&mut self) {
        // a tight list item emits no paragraph events
        if self.where_.is_none() {
            self.where_ = Some(Inlines::Paragraph);
        }
    }

    fn flush_loose_inlines(&mut self) {
        if !self.inlines.is_empty() {
            let inlines = std::mem::take(&mut self.inlines);
            self.where_ = None;
            self.push(Block::Paragraph(inlines));
        }
    }

    fn push(&mut self, block: Block) {
        if let Some(level) = self.levels.last_mut() {
            level.push(block);
        }
    }

    fn finish(mut self) -> Doc {
        self.flush_loose_inlines();

        // an unterminated container should still render what it held
        while self.levels.len() > 1 {
            let inner = self.levels.pop().unwrap();
            match self.opens.pop() {
                Some(Open::Quote) => self.push(Block::Quote(inner)),
                _ => {
                    if let Some(level) = self.levels.last_mut() {
                        level.extend(inner);
                    }
                }
            }
        }
        Doc {
            blocks: self.levels.pop().unwrap_or_default(),
        }
    }
}

fn align_of(a: Alignment) -> Align {
    match a {
        Alignment::Right => Align::Right,
        Alignment::Center => Align::Center,
        Alignment::None | Alignment::Left => Align::Left,
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// The language tag on a fenced code block, if it has one.
#[allow(dead_code)]
fn code_language(kind: &CodeBlockKind<'_>) -> Option<String> {
    // TODO: nothing calls this until code blocks are coloured
    match kind {
        CodeBlockKind::Fenced(lang) if !lang.is_empty() => Some(lang.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str, style: Style) -> Inline {
        Inline::Text {
            text: text.into(),
            style,
        }
    }

    #[test]
    fn headings_carry_their_level() {
        let doc = parse("# One\n\n### Three\n");
        assert_eq!(
            doc.blocks,
            vec![
                Block::Heading {
                    level: 1,
                    inlines: vec![plain("One", Style::default())]
                },
                Block::Heading {
                    level: 3,
                    inlines: vec![plain("Three", Style::default())]
                },
            ]
        );
    }

    #[test]
    fn nested_emphasis_flattens_onto_the_run() {
        let doc = parse("***both***");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected a paragraph, got {:?}", doc.blocks[0]);
        };
        assert_eq!(
            inlines,
            &vec![plain(
                "both",
                Style {
                    bold: true,
                    italic: true,
                    ..Style::default()
                }
            )]
        );
    }

    #[test]
    fn a_soft_break_becomes_a_space_and_merges() {
        let doc = parse("one\ntwo");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected a paragraph");
        };
        assert_eq!(inlines, &vec![plain("one two", Style::default())]);
    }

    #[test]
    fn a_fenced_block_keeps_its_indentation_and_loses_the_fence_newline() {
        let doc = parse("```rust\nfn main() {\n    ok();\n}\n```\n");
        assert_eq!(
            doc.blocks,
            vec![Block::Code(vec![
                "fn main() {".into(),
                "    ok();".into(),
                "}".into(),
            ])]
        );
    }

    #[test]
    fn a_tight_list_still_produces_paragraphs() {
        // tight items emit no Paragraph events at all, so without
        // the implicit open their text would vanish
        let doc = parse("- one\n- two\n");
        let Block::List { start, items } = &doc.blocks[0] else {
            panic!("expected a list, got {:?}", doc.blocks[0]);
        };
        assert_eq!(*start, None);
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0],
            vec![Block::Paragraph(vec![plain("one", Style::default())])]
        );
    }

    #[test]
    fn an_ordered_list_keeps_its_start() {
        let doc = parse("3. three\n4. four\n");
        let Block::List { start, .. } = &doc.blocks[0] else {
            panic!("expected a list");
        };
        assert_eq!(*start, Some(3));
    }

    #[test]
    fn raw_html_never_reaches_the_document_as_markup() {
        // neither path can become an element in the generated
        // SVG: block html is dropped, inline html stays text
        let doc = parse("<script>bad()</script>\n\ntext <b>x</b> more\n");
        let rendered = format!("{:?}", doc);
        assert!(!rendered.contains("Html"), "html leaked into the document");

        let Block::Paragraph(inlines) = doc.blocks.last().unwrap() else {
            panic!("expected a trailing paragraph");
        };
        let joined: String = inlines
            .iter()
            .map(|i| match i {
                Inline::Text { text, .. } => text.as_str(),
                _ => "",
            })
            .collect();
        assert!(joined.contains("<b>"), "inline html should survive as text");
    }

    #[test]
    fn an_image_becomes_its_alt_text() {
        let doc = parse("![a cat](cat.png)");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected a paragraph");
        };
        assert_eq!(
            inlines,
            &vec![Inline::Image {
                alt: "a cat".into()
            }]
        );
    }

    #[test]
    fn a_table_keeps_its_alignments_and_cells() {
        let doc = parse("| a | b | c |\n|:--|:-:|--:|\n| 1 | 2 | 3 |\n");
        let Block::Table(t) = &doc.blocks[0] else {
            panic!("expected a table, got {:?}", doc.blocks[0]);
        };
        assert_eq!(t.align, vec![Align::Left, Align::Center, Align::Right]);
        assert_eq!(t.head.len(), 3);
        assert_eq!(t.head[0], vec![plain("a", Style::default())]);
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0][2], vec![plain("3", Style::default())]);
    }

    #[test]
    fn a_column_with_no_alignment_marker_reads_as_left() {
        let doc = parse("| a |\n|---|\n| 1 |\n");
        let Block::Table(t) = &doc.blocks[0] else {
            panic!("expected a table");
        };
        assert_eq!(t.align, vec![Align::Left]);
    }

    #[test]
    fn a_quote_nests_its_blocks() {
        let doc = parse("> quoted\n");
        assert_eq!(
            doc.blocks,
            vec![Block::Quote(vec![Block::Paragraph(vec![plain(
                "quoted",
                Style::default()
            )])])]
        );
    }
}
