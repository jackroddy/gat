//! Markdown events to a block tree.
//
// nothing here deals in pixels, fonts or SVG

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
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
    Code { lang: Option<String>, lines: Vec<String> },
    List { start: Option<u64>, items: Vec<ListItem> },
    Quote { callout: Option<Callout>, blocks: Vec<Block> },
    /// Every footnote definition, in reference order, at the end of the document.
    Footnotes(Vec<Footnote>),
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

/// One footnote definition, numbered by where it was first referenced.
#[derive(Debug, PartialEq)]
pub struct Footnote {
    pub number: usize,
    pub blocks: Vec<Block>,
}

/// One item of a list, and whether it carried a task checkbox.
#[derive(Debug, Default, PartialEq)]
pub struct ListItem {
    /// `None` for an ordinary item, `Some(done)` for `- [ ]` or `- [x]`.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

/// A GitHub alert: the kind named by `> [!NOTE]` and its siblings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Callout {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl Callout {
    /// The word drawn above the quote.
    pub fn label(self) -> &'static str {
        match self {
            Callout::Note => "Note",
            Callout::Tip => "Tip",
            Callout::Important => "Important",
            Callout::Warning => "Warning",
            Callout::Caution => "Caution",
        }
    }
}

/// Inline content, with emphasis already flattened onto each run.
#[derive(Debug, PartialEq)]
pub enum Inline {
    // markdown nests emphasis; bold inside italic is one run
    // that is both, which keeps a style stack out of layout
    Text { text: String, style: Style },

    /// Rendered as its alt text.
    Image { alt: String },

    /// A footnote reference, drawn as its number in brackets.
    Note { number: usize },
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
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);

    // the only GFM extra this asks for is the kind on a
    // blockquote, which is where [!NOTE] arrives
    opts.insert(Options::ENABLE_GFM);

    // front matter is a site generator's metadata rather
    // than part of the document. parsing it is what allows
    // it to be dropped: left off, the fence reads as a rule
    // and the fields as a paragraph
    opts.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    opts.insert(Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS);

    let mut b = Builder::default();
    for event in Parser::new_ext(text, opts) {
        b.event(event);
    }
    b.finish()
}

/// What sort of container is open, for the block stack to close back into.
enum Open {
    Quote(Option<Callout>),
    Note(String),
    List { start: Option<u64>, items: Vec<ListItem> },
    Item { task: Option<bool> },
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
    code_lang: Option<String>,

    /// Set between the front matter fences, where text is discarded.
    meta: bool,

    /// Footnote labels in the order they were first referenced.
    //
    // the number a reader sees is this position, not the
    // label: `[^impl]` and `[^1]` both number from one, in
    // the order the prose reaches them
    notes: Vec<String>,

    /// Definitions collected out of the flow, to be placed at the end.
    defs: Vec<(String, Vec<Block>)>,

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

            Event::Text(_) if self.meta => {}

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

            Event::FootnoteReference(label) => {
                let number = self.note_number(&label);
                self.open_inlines();
                self.inlines.push(Inline::Note { number });
            }

            // the marker arrives inside the item it belongs
            // to, so it is recorded on the open item rather
            // than placed among that item's inlines
            Event::TaskListMarker(done) => {
                if let Some(Open::Item { task }) = self.opens.last_mut() {
                    *task = Some(done);
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.where_ = Some(Inlines::Paragraph),
            Tag::Heading { level, .. } => {
                self.where_ = Some(Inlines::Heading(heading_level(level)));
            }
            Tag::CodeBlock(kind) => {
                self.code = Some(String::new());
                self.code_lang = code_language(&kind);
            }
            Tag::BlockQuote(kind) => {
                self.opens.push(Open::Quote(kind.map(callout)));
                self.levels.push(Vec::new());
            }
            Tag::List(start) => self.opens.push(Open::List {
                start,
                items: Vec::new(),
            }),
            Tag::Item => {
                self.opens.push(Open::Item { task: None });
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
            Tag::MetadataBlock(_) => self.meta = true,

            Tag::Table(align) => {
                self.table = Some(Table {
                    align: align.iter().copied().map(align_of).collect(),
                    ..Table::default()
                });
            }

            // a cell's content arrives as ordinary inlines,
            // so it needs somewhere for them to land
            Tag::TableCell => self.where_ = Some(Inlines::Paragraph),

            // a definition is written wherever the author
            // put it and read at the end, so its blocks are
            // diverted rather than pushed
            Tag::FootnoteDefinition(label) => {
                self.opens.push(Open::Note(label.to_string()));
                self.levels.push(Vec::new());
            }
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
                let lang = self.code_lang.take();
                self.push(Block::Code { lang, lines });
            }
            TagEnd::BlockQuote(_) => {
                let blocks = self.levels.pop().unwrap_or_default();
                let callout = match self.opens.pop() {
                    Some(Open::Quote(c)) => c,
                    _ => None,
                };
                self.push(Block::Quote { callout, blocks });
            }
            TagEnd::Item => {
                self.flush_loose_inlines();
                let blocks = self.levels.pop().unwrap_or_default();
                let task = match self.opens.pop() {
                    Some(Open::Item { task }) => task,
                    _ => None,
                };
                if let Some(Open::List { items, .. }) = self.opens.last_mut() {
                    items.push(ListItem { task, blocks });
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

            TagEnd::MetadataBlock(_) => self.meta = false,

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

            TagEnd::FootnoteDefinition => {
                self.flush_loose_inlines();
                let blocks = self.levels.pop().unwrap_or_default();
                if let Some(Open::Note(label)) = self.opens.pop() {
                    self.defs.push((label, blocks));
                }
            }
            _ => {}
        }
    }

    /// The number `label` is drawn as, assigning the next one if it is new.
    fn note_number(&mut self, label: &str) -> usize {
        let at = self
            .notes
            .iter()
            .position(|n| n == label)
            .unwrap_or_else(|| {
                self.notes.push(label.to_owned());
                self.notes.len() - 1
            });
        at + 1
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

        // a url inside a code span is not a link, and one
        // inside a link already carries the style
        if style.code || style.link {
            self.run(t, style);
            return;
        }

        let linked = Style { link: true, ..style };
        let mut rest = t;
        while let Some((before, url, after)) = split_url(rest) {
            self.run(before, style);
            self.run(url, linked);
            rest = after;
        }
        self.run(rest, style);
    }

    fn run(&mut self, t: &str, style: Style) {
        if t.is_empty() {
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
                Some(Open::Quote(callout)) => self.push(Block::Quote {
                    callout,
                    blocks: inner,
                }),
                _ => {
                    if let Some(level) = self.levels.last_mut() {
                        level.extend(inner);
                    }
                }
            }
        }
        let footnotes = self.footnotes();
        let mut blocks = self.levels.pop().unwrap_or_default();
        if !footnotes.is_empty() {
            blocks.push(Block::Rule);
            blocks.push(Block::Footnotes(footnotes));
        }
        Doc { blocks }
    }

    /// The definitions in reference order, with any never referenced last.
    fn footnotes(&mut self) -> Vec<Footnote> {
        let defs = std::mem::take(&mut self.defs);
        let mut out: Vec<Footnote> = defs
            .into_iter()
            .map(|(label, blocks)| Footnote {
                number: self.note_number(&label),
                blocks,
            })
            .collect();
        out.sort_by_key(|f| f.number);
        out
    }
}

/// The next bare URL in `t`, split into what precedes it, the URL, and
/// what follows.
//
// pulldown-cmark implements commonmark's angle autolinks
// and not gfm's bare ones, so a plain https:// in prose
// arrives as ordinary text and is found here instead
fn split_url(t: &str) -> Option<(&str, &str, &str)> {
    let mut from = 0;
    while let Some(rel) = t[from..].find("http") {
        let at = from + rel;

        // `xhttps://` and `see:https://` are not links; a
        // preceding letter or digit means this is the tail
        // of some longer word
        let edge = t[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());

        if edge && (t[at..].starts_with("http://") || t[at..].starts_with("https://")) {
            let run = &t[at..];
            let run = &run[..run.find(char::is_whitespace).unwrap_or(run.len())];
            let url = trim_url(run);
            if !url.ends_with("://") {
                let end = at + url.len();
                return Some((&t[..at], &t[at..end], &t[end..]));
            }
        }
        from = at + "http".len();
    }
    None
}

/// Drop the sentence punctuation a URL picks up from the prose around it.
fn trim_url(url: &str) -> &str {
    let mut end = url.len();
    while let Some(c) = url[..end].chars().next_back() {
        let drop = match c {
            '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' => true,

            // a closing bracket belongs to the url only if
            // it matches one inside it, which is what keeps
            // the tail of a _(disambiguation) link
            ')' => url[..end].matches(')').count() > url[..end].matches('(').count(),
            ']' => url[..end].matches(']').count() > url[..end].matches('[').count(),
            _ => false,
        };
        if !drop {
            break;
        }
        end -= c.len_utf8();
    }
    &url[..end]
}

fn callout(kind: BlockQuoteKind) -> Callout {
    match kind {
        BlockQuoteKind::Note => Callout::Note,
        BlockQuoteKind::Tip => Callout::Tip,
        BlockQuoteKind::Important => Callout::Important,
        BlockQuoteKind::Warning => Callout::Warning,
        BlockQuoteKind::Caution => Callout::Caution,
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
fn code_language(kind: &CodeBlockKind<'_>) -> Option<String> {
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
            vec![Block::Code {
                lang: Some("rust".into()),
                lines: vec!["fn main() {".into(), "    ok();".into(), "}".into()],
            }]
        );
    }

    #[test]
    fn a_fence_with_no_info_string_names_no_language() {
        let doc = parse("```\nplain\n```\n");
        assert_eq!(
            doc.blocks,
            vec![Block::Code {
                lang: None,
                lines: vec!["plain".into()],
            }]
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
            ListItem {
                task: None,
                blocks: vec![Block::Paragraph(vec![plain("one", Style::default())])],
            }
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
    fn front_matter_is_dropped() {
        // the fences are a rule and the fields a paragraph
        // if the metadata extensions are not enabled
        let doc = parse("---\ntitle: x\ntags: [a, b]\n---\n\n# Heading\n");
        assert_eq!(
            doc.blocks,
            vec![Block::Heading {
                level: 1,
                inlines: vec![plain("Heading", Style::default())],
            }]
        );
    }

    #[test]
    fn toml_front_matter_is_dropped_too() {
        let doc = parse("+++\ntitle = \"x\"\n+++\n\ntext\n");
        assert_eq!(
            doc.blocks,
            vec![Block::Paragraph(vec![plain("text", Style::default())])]
        );
    }

    #[test]
    fn a_task_item_records_whether_it_is_done() {
        let doc = parse("- [x] done\n- [ ] todo\n- plain\n");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("expected a list, got {:?}", doc.blocks[0]);
        };
        let tasks: Vec<_> = items.iter().map(|i| i.task).collect();
        assert_eq!(tasks, vec![Some(true), Some(false), None]);
    }

    #[test]
    fn a_callout_keeps_its_kind() {
        let doc = parse("> [!WARNING]\n> mind the gap\n");
        let Block::Quote { callout, blocks } = &doc.blocks[0] else {
            panic!("expected a quote, got {:?}", doc.blocks[0]);
        };
        assert_eq!(*callout, Some(Callout::Warning));
        assert_eq!(
            *blocks,
            vec![Block::Paragraph(vec![plain(
                "mind the gap",
                Style::default()
            )])]
        );
    }

    #[test]
    fn a_plain_quote_has_no_callout() {
        let doc = parse("> just a quote\n");
        let Block::Quote { callout, .. } = &doc.blocks[0] else {
            panic!("expected a quote");
        };
        assert_eq!(*callout, None);
    }

    #[test]
    fn a_bare_url_is_styled_as_a_link() {
        let linked = Style {
            link: true,
            ..Style::default()
        };
        let doc = parse("see https://example.com/a for more\n");
        assert_eq!(
            doc.blocks,
            vec![Block::Paragraph(vec![
                plain("see ", Style::default()),
                plain("https://example.com/a", linked),
                plain(" for more", Style::default()),
            ])]
        );
    }

    #[test]
    fn a_url_does_not_swallow_the_sentence_punctuation() {
        // a full stop after a url is the prose's, and a
        // closing bracket is the url's only if it opened one
        assert_eq!(
            split_url("at https://example.com/a."),
            Some(("at ", "https://example.com/a", "."))
        );
        assert_eq!(
            split_url("(https://example.com/a)"),
            Some(("(", "https://example.com/a", ")"))
        );
        assert_eq!(
            split_url("https://en.wikipedia.org/wiki/Foo_(bar)"),
            Some(("", "https://en.wikipedia.org/wiki/Foo_(bar)", ""))
        );
    }

    #[test]
    fn a_url_needs_a_boundary_and_a_body() {
        assert_eq!(split_url("xhttps://example.com"), None);
        assert_eq!(split_url("https://"), None);
        assert_eq!(split_url("ftp://example.com"), None);
    }

    #[test]
    fn a_url_in_a_code_span_is_not_a_link() {
        let code = Style {
            code: true,
            ..Style::default()
        };
        let doc = parse("`https://example.com`\n");
        assert_eq!(
            doc.blocks,
            vec![Block::Paragraph(vec![plain("https://example.com", code)])]
        );
    }

    #[test]
    fn a_url_inside_a_real_link_is_not_split() {
        // the link style is already set, so the scan is
        // skipped and the text stays one run
        let linked = Style {
            link: true,
            ..Style::default()
        };
        let doc = parse("[https://example.com](https://example.com)\n");
        assert_eq!(
            doc.blocks,
            vec![Block::Paragraph(vec![plain("https://example.com", linked)])]
        );
    }

    #[test]
    fn footnotes_are_numbered_by_first_reference_and_moved_to_the_end() {
        // the definitions are written in the other order, so
        // this fails if numbering follows the definitions
        let doc = parse("see[^z] and[^a]\n\n[^a]: alpha\n[^z]: zulu\n");

        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected a paragraph, got {:?}", doc.blocks[0]);
        };
        let numbers: Vec<_> = inlines
            .iter()
            .filter_map(|i| match i {
                Inline::Note { number } => Some(*number),
                _ => None,
            })
            .collect();
        assert_eq!(numbers, vec![1, 2]);

        assert_eq!(doc.blocks[1], Block::Rule);
        let Block::Footnotes(notes) = &doc.blocks[2] else {
            panic!("expected footnotes, got {:?}", doc.blocks[2]);
        };
        assert_eq!(notes[0].number, 1);
        assert_eq!(
            notes[0].blocks,
            vec![Block::Paragraph(vec![plain("zulu", Style::default())])]
        );
        assert_eq!(notes[1].number, 2);
    }

    #[test]
    fn a_document_without_footnotes_gains_no_rule() {
        let doc = parse("just text\n");
        assert_eq!(doc.blocks.len(), 1);
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
            vec![Block::Quote {
                callout: None,
                blocks: vec![Block::Paragraph(vec![plain("quoted", Style::default())])],
            }]
        );
    }
}
