use std::path::Path;

use crate::framebuffer::Framebuffer;

#[cfg(feature = "markdown")]
mod markdown;
#[cfg(any(feature = "pdf", feature = "pdf-pdfium"))]
mod pdf;
#[cfg(feature = "svg")]
mod svg;

/// What the caller would like the decoder to rasterize at, for the formats
/// that have no inherent pixel size.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(any(feature = "svg", feature = "pdf", feature = "pdf-pdfium")), allow(dead_code))]
pub struct Hints {
    pub max_w: u32,
    pub max_h: u32,

    /// The terminal's character cell.
    //
    // a flowed source sets its type from this: a monospace
    // glyph advances by exactly one cell width, so matching the
    // cell is what makes the body text come out the size of the
    // text around it
    pub cell: crate::geometry::CellSize,
}

/// A decoded source.
pub struct Loaded {
    pub fb: Framebuffer,

    /// Where the words landed, for a document the viewer can search.
    //
    // a rasterized page is pixels, so nothing in it can be
    // found by looking at it again. this is the same text
    // layout already positioned, kept instead of discarded
    pub index: Option<Index>,
}

#[derive(Debug, Default)]
pub struct Index {
    /// One entry per drawn line, top to bottom.
    pub lines: Vec<Line>,
    pub outline: Vec<Heading>,

    /// The page's row height in pixels, which every line top is a
    /// multiple of.
    pub row: f32,
}

#[derive(Debug)]
pub struct Line {
    /// The top of the line, in framebuffer pixels.
    pub y: f32,
    pub text: String,
}

#[derive(Debug)]
pub struct Heading {
    pub level: u8,
    pub text: String,

    /// The top of the heading, in framebuffer pixels.
    pub y: f32,
}

impl Index {
    /// Multiply every position by `k`, for a page the viewer resized.
    pub fn scale(&mut self, k: f32) {
        for l in &mut self.lines {
            l.y *= k;
        }
        for h in &mut self.outline {
            h.y *= k;
        }
        self.row *= k;
    }

    /// The top of every line holding `needle`, in order down the page.
    //
    // ascii case folding only: it is the one mapping that
    // cannot change a string's length, and anything else
    // would need the match's byte offset translated back
    pub fn find(&self, needle: &str) -> Vec<usize> {
        if needle.is_empty() {
            return Vec::new();
        }
        let needle = needle.to_ascii_lowercase();
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.text.to_ascii_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }
}

/// A source as one or more images, stacked top to bottom.
//
// a picture is one piece and always has been. a document can be
// several, because stacking them down the scrollback beats
// handing a terminal a single image it may not take, and beats
// cutting the document off at the fold
pub struct Pieces {
    inner: Inner,
}

enum Inner {
    Whole(Option<Framebuffer>),
    #[cfg(feature = "markdown")]
    Flowed(markdown::Chunks),
}

impl Iterator for Pieces {
    type Item = Result<Framebuffer, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            Inner::Whole(fb) => fb.take().map(Ok),
            #[cfg(feature = "markdown")]
            Inner::Flowed(chunks) => chunks.next(),
        }
    }
}

/// Decode `bytes` as the pieces it should be drawn in.
pub fn pieces(bytes: &[u8], path: &Path, hints: Hints) -> Result<Pieces, Error> {
    #[cfg(feature = "markdown")]
    if matches!(sniff(bytes, path), Format::Markdown) {
        return Ok(Pieces {
            inner: Inner::Flowed(markdown::chunks(bytes, hints)?),
        });
    }
    Ok(Pieces {
        inner: Inner::Whole(Some(load(bytes, path, hints)?.fb)),
    })
}

/// Whether a source is a picture or a page of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A picture, fitted to the screen so that all of it is visible at once.
    Image,
    /// A page of text, drawn at full width and 1:1 and panned to read on.
    Document,
}

#[derive(Debug)]
pub enum Error {
    Unsupported(&'static str),
    Decode(String),
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported(what) => write!(f, "unsupported format: {what}"),
            Error::Decode(msg) => write!(f, "{msg}"),
        }
    }
}

/// What sort of thing this is, without decoding it.
pub fn kind(bytes: &[u8], path: &Path) -> Kind {
    // the size to ask a decoder for depends on the kind, so
    // the kind has to be settled before the pixels arrive
    match sniff(bytes, path) {
        Format::Markdown => Kind::Document,
        _ => Kind::Image,
    }
}

/// Decode `bytes` into a framebuffer, choosing the decoder from the content
/// and, where that settles nothing, from `path`.
pub fn load(bytes: &[u8], path: &Path, #[cfg_attr(not(any(feature = "svg", feature = "pdf", feature = "pdf-pdfium")), allow(unused))] hints: Hints) -> Result<Loaded, Error> {
    match sniff(bytes, path) {
        Format::Raster => image::load_from_memory(bytes)
            .map(|img| whole(img.into_rgba8()))
            .map_err(|e| Error::Decode(e.to_string())),

        #[cfg(feature = "svg")]
        Format::Svg => svg::load(bytes, hints).map(whole),
        #[cfg(not(feature = "svg"))]
        Format::Svg => Err(Error::Unsupported("SVG (rebuild with --features svg)")),

        #[cfg(any(feature = "pdf", feature = "pdf-pdfium"))]
        Format::Pdf => pdf::load(bytes, hints).map(whole),
        #[cfg(not(any(feature = "pdf", feature = "pdf-pdfium")))]
        Format::Pdf => Err(Error::Unsupported("PDF (rebuild with --features pdf)")),

        #[cfg(feature = "markdown")]
        Format::Markdown => markdown::load(bytes, hints),
        #[cfg(not(feature = "markdown"))]
        Format::Markdown => Err(Error::Unsupported(
            "markdown (rebuild with --features markdown)",
        )),

        Format::Unknown => Err(Error::Unsupported("unrecognized file")),
    }
}

enum Format {
    Raster,
    Svg,
    Pdf,
    Markdown,
    Unknown,
}

fn sniff(bytes: &[u8], path: &Path) -> Format {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.starts_with(b"\xff\xd8\xff") {
        return Format::Raster;
    }
    if bytes.starts_with(b"%PDF-") {
        return Format::Pdf;
    }

    // the extension is the only signal for markdown, and it
    // is read before the content probe below: a document
    // *about* svg mentions <svg in its first paragraph,
    // which the probe would take for an svg file
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if MARKDOWN_EXTENSIONS
        .iter()
        .any(|m| ext.eq_ignore_ascii_case(m))
    {
        return Format::Markdown;
    }

    // SVG has no magic number, and the root element can sit behind an XML
    // declaration, a doctype, or comments
    let head = &bytes[..bytes.len().min(1024)];
    if find(head, b"<svg").is_some() {
        return Format::Svg;
    }
    match ext {
        e if e.eq_ignore_ascii_case("svg") => Format::Svg,
        e if e.eq_ignore_ascii_case("pdf") => Format::Pdf,
        _ => Format::Unknown,
    }
}

/// Extensions that mean markdown.
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdown", "mkd"];

/// A `Loaded` for a source that was decoded in full.
fn whole(fb: Framebuffer) -> Loaded {
    Loaded { fb, index: None }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(lines: &[(f32, &str)]) -> Index {
        Index {
            lines: lines
                .iter()
                .map(|(y, text)| Line {
                    y: *y,
                    text: (*text).into(),
                })
                .collect(),
            outline: Vec::new(),
            row: 10.0,
        }
    }

    #[test]
    fn a_search_ignores_ascii_case_and_answers_in_page_order() {
        let ix = index(&[(0.0, "The Quick Fox"), (10.0, "a quick brown"), (20.0, "slow")]);
        assert_eq!(ix.find("quick"), vec![0, 1]);
        assert_eq!(ix.find("QUICK"), vec![0, 1]);
        assert_eq!(ix.find("slow"), vec![2]);
    }

    #[test]
    fn a_search_for_nothing_matches_nothing() {
        let ix = index(&[(0.0, "text")]);
        assert!(ix.find("").is_empty());
        assert!(ix.find("absent").is_empty());
    }

    #[test]
    fn scaling_moves_every_position_by_the_same_factor() {
        let mut ix = index(&[(10.0, "a"), (20.0, "b")]);
        ix.outline.push(Heading {
            level: 1,
            text: "h".into(),
            y: 40.0,
        });
        ix.scale(0.5);
        assert_eq!(ix.lines[0].y, 5.0);
        assert_eq!(ix.lines[1].y, 10.0);
        assert_eq!(ix.outline[0].y, 20.0);
    }
}
