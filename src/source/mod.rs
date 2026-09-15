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
}

/// A decoded source, and how much more of it there is.
pub struct Loaded {
    pub fb: Framebuffer,
    /// For a flowed source, the whole document's height, of which `fb` is one
    /// band. For an image, simply its height.
    pub total_h: u32,
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
    let total_h = fb.height();
    Loaded { fb, total_h }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
