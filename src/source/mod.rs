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
    /// For a flowed source, how far down the document this slice starts.
    //
    // a document is drawn a band at a time rather than whole. Handing a
    // terminal the entire thing means asking it to hold tens of megabytes as
    // one image, which it may simply refuse, and the refusal looks exactly
    // like the feature being broken. So the viewer asks for the part it can
    // see and asks again when it scrolls off the end, and the one-shot render
    // asks for the top and nothing else.
    #[cfg_attr(not(feature = "markdown"), allow(dead_code))]
    pub from_y: u32,
}

/// A decoded source, and how much more of it there is.
pub struct Loaded {
    pub fb: Framebuffer,
    /// For a flowed source, the whole document's height, of which `fb` is one
    /// band. For an image, simply its height.
    pub total_h: u32,
}

/// Whether a source is a picture or a page of text.
//
// the viewer fits a picture to the screen, because seeing all of it at once is
// the point. a document taller than the screen wants the opposite: full width,
// 1:1, and pan to read on. Nothing else distinguishes them once both are
// pixels, so the decoder has to say which it produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
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
//
// the viewer has to know before it asks: a document and a picture want
// different sizes requested of them, so the answer cannot arrive with the
// pixels. sniffing twice is cheap next to decoding once.
pub fn kind(bytes: &[u8], path: &Path) -> Kind {
    match sniff(bytes, path) {
        Format::Markdown => Kind::Document,
        _ => Kind::Image,
    }
}

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
    // the extension is consulted before the content probe below, not after.
    // markdown has no magic number of its own, and a document *about* svg
    // mentions <svg in its first paragraph, which the probe would otherwise
    // read as an svg file
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

/// Extensions that mean markdown. There is no sniffing to fall back on, so an
/// unrecognized text file stays unknown rather than being read as markdown.
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdown", "mkd"];

/// A source that is all there in one piece, which is every source but markdown.
fn whole(fb: Framebuffer) -> Loaded {
    let total_h = fb.height();
    Loaded { fb, total_h }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
