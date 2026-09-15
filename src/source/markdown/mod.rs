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
use crate::source::{Error, Hints, Loaded, svg};

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
// the viewer asks for the whole document so that it has something to scroll
// through, which makes the page as long as the file. Something has to stop
// that, and this is it: the height is cut to respect it before anything is
// allocated. 16 megapixels is 64MB of framebuffer and, at a typical width,
// somewhere north of fifteen thousand pixels of text, which is a long document
// before anyone notices the end is missing.
const MAX_PIXELS: u32 = 16_000_000;

pub fn load(bytes: &[u8], hints: Hints) -> Result<Loaded, Error> {
    // invalid bytes become U+FFFD rather than failing the whole file: markdown
    // is text people edit by hand, and a stray byte should cost one character
    let text = String::from_utf8_lossy(bytes);

    let width = hints.max_w.max(1) as f32;
    let theme = theme_for(width);
    let page = layout::layout(&parse::parse(&text), &theme, width);

    // the document is laid out whole -- it costs a few hundred microseconds --
    // but only the requested band is drawn
    let total_h = page.h.max(1.0);
    let from_y = (hints.from_y as f32).min((total_h - 1.0).max(0.0));
    let band_h = band_height(total_h - from_y, hints);

    let svg_doc = to_svg::emit(&page, theme.bg, from_y, band_h);
    let tree = usvg::Tree::from_str(&svg_doc, &font::options())
        .map_err(|e| Error::Decode(e.to_string()))?;

    let fb = svg::rasterize(&tree, 1.0)?;
    Ok(Loaded {
        fb: crop(fb, band_h as u32, theme.bg),
        total_h: total_h as u32,
    })
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

/// How tall a band to draw, given how much document is left below `from_y`.
//
// never more than was asked for: geometry::fit shrinks by min(w_frac, h_frac),
// so a band taller than the screen would be scaled down and the text would be
// mush. Both callers therefore ask for roughly a screenful, and the viewer
// asks again with a new from_y when it scrolls past the end of one.
fn band_height(remaining: f32, hints: Hints) -> f32 {
    let by_request = hints.max_h.max(1) as f32;
    let by_memory = (MAX_PIXELS / hints.max_w.max(1)) as f32;
    remaining.min(by_request).min(by_memory).max(1.0)
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
            from_y: 0,
        }
    }

    #[test]
    fn a_band_is_never_taller_than_was_asked_for() {
        // a band taller than the screen would be scaled down by geometry::fit
        // and the text would be mush, which is the whole reason for bands
        assert_eq!(band_height(6000.0, hints(800, 600)), 600.0);
        // and the last band of a document is only as tall as what is left
        assert_eq!(band_height(200.0, hints(800, 600)), 200.0);
    }

    #[test]
    fn the_pixel_ceiling_overrides_a_generous_caller() {
        let h = band_height(f32::MAX, hints(8000, 100_000));
        assert_eq!(h, (MAX_PIXELS / 8000) as f32);
    }

    #[test]
    fn a_band_lower_down_the_document_renders_different_text() {
        // the point of from_y: the same file, asked for twice at different
        // offsets, must not come back with the same pixels
        let md = "# Heading\n\n".to_owned() + &"A paragraph of body text. ".repeat(400);
        let top = load(md.as_bytes(), hints(800, 400)).unwrap();
        let down = load(
            md.as_bytes(),
            Hints {
                from_y: 2000,
                ..hints(800, 400)
            },
        )
        .unwrap();

        assert_eq!(top.total_h, down.total_h, "the document did not change");
        assert!(top.total_h > 2400, "test document is too short to band");
        assert_eq!(top.fb.height(), 400);
        assert_ne!(
            top.fb.as_raw(),
            down.fb.as_raw(),
            "two different bands rendered identical pixels"
        );
    }

    #[test]
    fn asking_past_the_end_still_returns_a_band() {
        // clamping rather than erroring: a resize can leave a stale offset
        // pointing past a document that just got shorter
        let got = load(
            b"# short\n",
            Hints {
                from_y: 99_999,
                ..hints(800, 400)
            },
        );
        assert!(got.is_ok(), "an offset past the end should clamp, not fail");
    }

    #[test]
    fn a_page_renders_actual_pixels() {
        // a font that fails to load produces a valid, entirely empty page, so
        // the only honest check is that something was drawn in the foreground
        let md = b"# Heading\n\nSome body text with <angle> & ampersand.\n";
        let fb = load(md, hints(800, 600)).expect("markdown should render").fb;
        assert_eq!(fb.width(), 800);

        let ink = fb
            .pixels()
            .filter(|p| p.0[0] > 0x40 && p.0[1] > 0x40 && p.0[2] > 0x40)
            .count();
        assert!(ink > 200, "page looks blank: only {ink} lit pixels");
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    #[test]
    #[ignore = "timing, not a test"]
    fn where_does_the_time_go() {
        let bytes = std::fs::read("CLAUDE.md").unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let theme = theme_for(2000.0);

        let t = std::time::Instant::now();
        for _ in 0..20 {
            let _ = parse::parse(&text);
        }
        println!("  parse            {:?}", t.elapsed() / 20);

        let doc = parse::parse(&text);
        let t = std::time::Instant::now();
        for _ in 0..20 {
            let _ = layout::layout(&doc, &theme, 2000.0);
        }
        println!("  layout           {:?}", t.elapsed() / 20);

        let page = layout::layout(&doc, &theme, 2000.0);
        println!("  page             {}x{}", page.w, page.h);

        let t = std::time::Instant::now();
        let svg = to_svg::emit(&page, theme.bg, 0.0, page.h);
        println!("  emit svg         {:?}  ({} KB)", t.elapsed(), svg.len() / 1024);

        let t = std::time::Instant::now();
        let tree = usvg::Tree::from_str(&svg, &font::options()).unwrap();
        println!("  usvg parse       {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let fb = svg::rasterize(&tree, 1.0).unwrap();
        println!("  rasterize        {:?}  ({}x{})", t.elapsed(), fb.width(), fb.height());

        let t = std::time::Instant::now();
        let _ = crate::render::kitty::encode(&fb).unwrap();
        println!("  zlib encode      {:?}", t.elapsed());
    }
}
