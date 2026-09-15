//! Markdown as a rasterized source.
//
// height is a function of the width given, so a page is laid
// out at the display width, drawn at 1:1, and cut off

mod font;
mod layout;
mod parse;
mod to_svg;

use resvg::usvg;

use crate::framebuffer::Framebuffer;
use crate::source::{Error, Hints, Loaded, svg};

/// The column count a page is laid out to, whatever pixel width
/// the caller asks for.
const TARGET_COLS: f32 = 88.0;

/// Lower bound on the derived font size, in pixels.
const MIN_SIZE: f32 = 11.0;

/// Upper bound on the derived font size, in pixels.
//
// TODO: both bounds were eyeballed, not measured
const MAX_SIZE: f32 = 40.0;

/// A ceiling on the page, independent of what the caller asked for.
//
// 16 megapixels is 64MB of framebuffer
const MAX_PIXELS: u32 = 16_000_000;

pub fn load(bytes: &[u8], hints: Hints) -> Result<Loaded, Error> {
    let text = String::from_utf8_lossy(bytes);

    let width = hints.max_w.max(1) as f32;
    let theme = theme_for(width);
    let page = layout::layout(&parse::parse(&text), &theme, width);

    // the whole document is drawn, not a screenful: the
    // viewer scrolls by cropping this buffer, and cropping
    // costs a memcpy where drawing again costs a rasterize
    let total_h = drawn_height(page.h, hints);

    let svg_doc = to_svg::emit(&page, theme.bg, 0.0, total_h);
    let tree = usvg::Tree::from_str(&svg_doc, &font::options())
        .map_err(|e| Error::Decode(e.to_string()))?;

    let fb = svg::rasterize(&tree, 1.0)?;
    Ok(Loaded {
        fb: crop(fb, total_h as u32, theme.bg),
        total_h: total_h as u32,
    })
}

/// The theme to lay out with, sized for the width the caller asked for.
fn theme_for(width: f32) -> layout::Theme {
    let base = layout::Theme::DARK;
    let usable = (width - 2.0 * base.margin).max(1.0);

    // derived from a column target rather than fixed in pixels:
    // tui.rs asks for a wider page than main.rs, and one fixed
    // size would wrap the two at different column counts
    let size = (usable / (TARGET_COLS * font::ADVANCE_RATIO)).clamp(MIN_SIZE, MAX_SIZE);
    layout::Theme {
        base_size: size,
        margin: base.margin.max(size * 0.75),
        ..base
    }
}

/// How tall a page to draw, given how tall the content is.
fn drawn_height(content_h: f32, hints: Hints) -> f32 {
    // the one-shot render asks for a screenful and gets one,
    // since geometry::fit would otherwise shrink a long page
    // until the text lost resolution. the viewer asks for the
    // document and crops it as the reader scrolls
    let by_request = hints.max_h.max(1) as f32;
    let by_memory = (MAX_PIXELS / hints.max_w.max(1)) as f32;
    content_h.min(by_request).min(by_memory).max(1.0)
}

/// Cut the page down to `h`, or pad it out if the content fell short.
fn crop(fb: Framebuffer, h: u32, bg: layout::Rgb) -> Framebuffer {
    if fb.height() == h {
        return fb;
    }

    // padded with the page's own background, so the caller does
    // not composite a short document against --background
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
    fn a_page_is_drawn_to_the_height_asked_for() {
        // the one-shot render gets a screenful
        assert_eq!(drawn_height(6000.0, hints(800, 600)), 600.0);
        assert_eq!(drawn_height(200.0, hints(800, 600)), 200.0);
        // the viewer asks for the document and gets all of it
        assert_eq!(drawn_height(6000.0, hints(800, u32::MAX)), 6000.0);
    }

    #[test]
    fn the_pixel_ceiling_overrides_a_generous_caller() {
        let h = drawn_height(f32::MAX, hints(8000, 100_000));
        assert_eq!(h, (MAX_PIXELS / 8000) as f32);
    }

    #[test]
    fn a_long_document_comes_back_whole() {
        // the viewer scrolls by cropping this buffer, so the
        // buffer has to hold every line, not one screenful
        let md = "# Heading\n\n".to_owned() + &"A paragraph of body text. ".repeat(400);
        let got = load(md.as_bytes(), hints(800, u32::MAX)).unwrap();

        assert!(got.total_h > 2400, "test document is too short to matter");
        assert_eq!(
            got.fb.height(),
            got.total_h,
            "the buffer is shorter than the document it reports"
        );
        assert!(
            got.fb.height() > 400,
            "the page was cut to the viewport instead of drawn whole"
        );
    }

    #[test]
    fn a_page_renders_actual_pixels() {
        // a font that fails to load gives a valid but empty page
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
