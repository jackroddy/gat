//! Markdown as a rasterized source.
//
// Unlike the other decoders, markdown has no size of its own in either
// direction: its height is a function of the width it is given. So it is laid
// out at the display width and drawn at 1:1, and anything past the display
// height is cut off rather than scaled away. See the note on `clip` below for
// why scaling is not an option.

mod font;
mod layout;
mod parse;
mod to_svg;

use resvg::usvg;

use crate::framebuffer::Framebuffer;
use crate::source::{Error, Hints, svg};

/// How many columns of text a page aims to be wide, whatever pixel width the
/// caller asks for.
//
// the font size is derived from this rather than fixed, because the two
// callers ask for different widths: main.rs passes the viewport, tui.rs passes
// it multiplied by ZOOM_HEADROOM. A fixed pixel size would wrap to a different
// column count in the viewer than under --print, so the same document would
// reflow when you opened it. Deriving size from a column target instead means
// the viewer just gets the same page drawn with more pixels in it.
const TARGET_COLS: f32 = 88.0;

/// Bounds on the derived size, for a terminal that is very narrow or very wide.
const MIN_SIZE: f32 = 11.0;
const MAX_SIZE: f32 = 40.0;

/// A ceiling on the page, independent of what the caller asked for.
//
// max_w by max_h with the viewer's headroom on a 4K screen is well over a
// hundred megabytes of RGBA. The height is cut to respect this before anything
// is allocated.
const MAX_PIXELS: u32 = 32_000_000;

pub fn load(bytes: &[u8], hints: Hints) -> Result<Framebuffer, Error> {
    // invalid bytes become U+FFFD rather than failing the whole file: markdown
    // is text people edit by hand, and a stray byte should cost one character
    let text = String::from_utf8_lossy(bytes);

    let width = hints.max_w.max(1) as f32;
    let theme = theme_for(width);
    let page = layout::layout(&parse::parse(&text), &theme, width);

    let svg_doc = to_svg::emit(&page, theme.bg);
    let tree = usvg::Tree::from_str(&svg_doc, &font::options())
        .map_err(|e| Error::Decode(e.to_string()))?;

    let fb = svg::rasterize(&tree, 1.0)?;
    Ok(crop(fb, clip(page.h, hints) as u32, theme.bg))
}

/// The theme to lay out with, sized for the width the caller asked for.
//
// the font size is derived rather than fixed because the two callers ask for
// different widths: main.rs passes the viewport, tui.rs passes it multiplied by
// ZOOM_HEADROOM. A fixed pixel size would wrap to a different column count in
// the viewer than under --print, so the same document would reflow on opening
// it. Deriving from a column target instead means the viewer gets the same
// page with more pixels in it.
fn theme_for(width: f32) -> layout::Theme {
    let base = layout::Theme::DARK;
    let usable = (width - 2.0 * base.margin).max(1.0);
    let size = (usable / (TARGET_COLS * font::ADVANCE_RATIO)).clamp(MIN_SIZE, MAX_SIZE);
    layout::Theme {
        base_size: size,
        margin: base.margin.max(size * 0.75),
        ..base
    }
}

/// How tall to actually draw, given how tall the content wants to be.
//
// truncation rather than scaling, which looks like the wrong answer until you
// follow what the caller does with the result. geometry::fit shrinks by
// min(w_frac, h_frac), so handing back a six thousand pixel page for an eight
// hundred pixel viewport would get it scaled to an eighth of its size and the
// text would be mush. The viewer does the same at zoom 1.0. Cutting instead
// leaves fit with nothing to do and the text lands at 1:1, which is the same
// bargain PDF makes by rendering only the first page.
fn clip(content_h: f32, hints: Hints) -> f32 {
    let by_request = hints.max_h.max(1) as f32;
    let by_memory = (MAX_PIXELS / hints.max_w.max(1)) as f32;
    content_h.min(by_request).min(by_memory).max(1.0)
}

/// Cut the page down to `h`, or pad it out if the content fell short.
//
// resvg already clipped anything past the viewBox, so this is the cheap half:
// take the rows we want and keep the page's own background underneath, rather
// than letting the caller composite a long document against --background.
fn crop(fb: Framebuffer, h: u32, bg: layout::Rgb) -> Framebuffer {
    if fb.height() == h {
        return fb;
    }
    let mut out = Framebuffer::from_pixel(fb.width(), h, image::Rgba([bg.0, bg.1, bg.2, 255]));
    image::imageops::replace(&mut out, &fb, 0, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(w: u32, h: u32) -> Hints {
        Hints {
            max_w: w,
            max_h: h,
        }
    }

    #[test]
    fn a_long_document_is_cut_rather_than_shrunk() {
        // the whole point: content taller than the viewport comes back at the
        // viewport's height, so geometry::fit has nothing left to scale
        assert_eq!(clip(6000.0, hints(800, 600)), 600.0);
        // and a short one keeps its own height
        assert_eq!(clip(200.0, hints(800, 600)), 200.0);
    }

    #[test]
    fn the_pixel_ceiling_overrides_a_generous_caller() {
        // a 4K viewport with the viewer's headroom asks for far more than we
        // are willing to allocate
        let h = clip(f32::MAX, hints(8000, 100_000));
        assert_eq!(h, (MAX_PIXELS / 8000) as f32);
    }

    #[test]
    fn a_page_renders_actual_pixels() {
        // a font that fails to load produces a valid, entirely empty page, so
        // the only honest check is that something was drawn in the foreground
        let md = b"# Heading\n\nSome body text with <angle> & ampersand.\n";
        let fb = load(md, hints(800, 600)).expect("markdown should render");
        assert_eq!(fb.width(), 800);

        let ink = fb
            .pixels()
            .filter(|p| p.0[0] > 0x40 && p.0[1] > 0x40 && p.0[2] > 0x40)
            .count();
        assert!(ink > 200, "page looks blank: only {ink} lit pixels");
    }
}
