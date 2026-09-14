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

pub fn load(bytes: &[u8], path: &Path, #[cfg_attr(not(any(feature = "svg", feature = "pdf", feature = "pdf-pdfium")), allow(unused))] hints: Hints) -> Result<Framebuffer, Error> {
    match sniff(bytes, path) {
        Format::Raster => image::load_from_memory(bytes)
            .map(|img| img.into_rgba8())
            .map_err(|e| Error::Decode(e.to_string())),

        #[cfg(feature = "svg")]
        Format::Svg => svg::load(bytes, hints),
        #[cfg(not(feature = "svg"))]
        Format::Svg => Err(Error::Unsupported("SVG (rebuild with --features svg)")),

        #[cfg(any(feature = "pdf", feature = "pdf-pdfium"))]
        Format::Pdf => pdf::load(bytes, hints),
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

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
