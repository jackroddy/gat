//! Markdown as a rasterized source.
//
// height is a function of the width given, so a page is laid
// out at the display width, drawn at 1:1, and cut off

mod font;
mod highlight;
mod layout;
mod parse;
mod to_svg;

use resvg::usvg;

use crate::framebuffer::Framebuffer;
use crate::geometry::CellSize;
use crate::source::{Error, Hints, Index, Loaded, svg};

/// The column count a page is laid out to, whatever pixel width
/// the caller asks for.
//
// the page is no wider than this however wide the terminal is.
// a line of prose running the full width of a large window is
// hard to read, and the empty right-hand side costs nothing
// since the page is only as wide as the text it holds
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

/// The largest piece of a page to hand over at once, in pixels.
//
// the one-shot render stacks pieces down the scrollback, so a
// long document costs several images rather than one enormous
// one. Each is transmitted once and then sits in the scrollback,
// so splitting costs the terminal nothing it would not have paid
// for the whole page
const CHUNK_PIXELS: u32 = 4_000_000;

/// A document rendered in pieces, top to bottom.
pub struct Chunks {
    page: layout::Page,
    bg: layout::Rgb,
    total_h: f32,
    chunk_h: f32,
    y: f32,
}

/// Lay `bytes` out and return its pieces, without drawing any.
pub fn chunks(bytes: &[u8], hints: Hints) -> Result<Chunks, Error> {
    let (page, theme, width) = lay_out(bytes, hints);
    let total_h = drawn_height(page.h, width, hints);

    // whole lines only, or a seam slices the glyphs on it
    let line_h = (theme.base_size * theme.line_ratio).max(1.0);
    let rows = ((CHUNK_PIXELS as f32 / width.max(1.0)) / line_h).floor().max(1.0);

    Ok(Chunks {
        page,
        bg: theme.bg,
        total_h,
        chunk_h: rows * line_h,
        y: 0.0,
    })
}

impl Iterator for Chunks {
    type Item = Result<Framebuffer, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.y >= self.total_h {
            return None;
        }
        let h = self.chunk_h.min(self.total_h - self.y);
        let piece = draw(&self.page, self.bg, self.y, h);
        self.y += h;
        Some(piece)
    }
}

/// Parse and lay out, stopping before anything is drawn.
fn lay_out(bytes: &[u8], hints: Hints) -> (layout::Page, layout::Theme, f32) {
    let text = String::from_utf8_lossy(bytes);
    let theme = theme_for(hints.cell);

    // only as wide as the measure needs, so the page is no
    // larger than the text in it
    let measure = TARGET_COLS * theme.base_size * font::ADVANCE_RATIO + 2.0 * theme.margin;
    let width = measure.min(hints.max_w.max(1) as f32).max(1.0);

    let page = layout::layout(&parse::parse(&text), &theme, width);
    (page, theme, width)
}

/// Rasterize the slice of `page` from `from_y` for `h` pixels.
fn draw(page: &layout::Page, bg: layout::Rgb, from_y: f32, h: f32) -> Result<Framebuffer, Error> {
    let svg_doc = to_svg::emit(page, bg, from_y, h);
    let tree = usvg::Tree::from_str(&svg_doc, &font::options())
        .map_err(|e| Error::Decode(e.to_string()))?;
    Ok(crop(svg::rasterize(&tree, 1.0)?, h as u32, bg))
}

pub fn load(bytes: &[u8], hints: Hints) -> Result<Loaded, Error> {
    let (mut page, _, width) = lay_out(bytes, hints);
    let total_h = drawn_height(page.h, width, hints);
    let fb = draw(&page, layout::Theme::DARK.bg, 0.0, total_h)?;
    Ok(Loaded {
        fb,
        index: Some(Index {
            lines: std::mem::take(&mut page.lines),
            outline: std::mem::take(&mut page.outline),
        }),
    })
}

/// The theme to lay out with, sized for the width the caller asked for.
fn theme_for(cell: CellSize) -> layout::Theme {
    let base = layout::Theme::DARK;

    // a monospace glyph advances by size * ADVANCE_RATIO, and a
    // cell is exactly one advance wide, so this sets the body
    // text to the width of the terminal's own characters
    // capped at the cell height as well as derived from its
    // width: a line box is cell.h only while the type fits in
    // one row, and the viewer addresses lines by row
    let size = (cell.w as f32 / font::ADVANCE_RATIO)
        .min(cell.h as f32)
        .clamp(MIN_SIZE, MAX_SIZE);

    layout::Theme {
        base_size: size,
        // one text line to one terminal row, so the page lines
        // up with whatever else is on screen
        line_ratio: (cell.h as f32 / size).max(1.0),
        margin: base.margin.max(size * 0.75),
        ..base
    }
}

/// How tall a page to draw, given how tall the content is.
fn drawn_height(content_h: f32, width: f32, hints: Hints) -> f32 {
    // the one-shot render asks for a screenful and gets one,
    // since geometry::fit would otherwise shrink a long page
    // until the text lost resolution. the viewer asks for the
    // document and crops it as the reader scrolls
    let by_request = hints.max_h.max(1) as f32;
    let by_memory = MAX_PIXELS as f32 / width.max(1.0);
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
            cell: CellSize { w: 14, h: 32 },
        }
    }

    #[test]
    fn the_viewer_gets_an_index_that_finds_words_inside_the_page() {
        // what the viewer searches is this index, so a hit
        // has to land inside the pixels it will scroll to
        let md = b"# Heading\n\nfirst line\n\nthe needle is here\n\nlast line\n";
        let loaded = load(md, hints(800, u32::MAX)).expect("should render");
        let ix = loaded.index.expect("a document carries an index");

        let hits = ix.find("NEEDLE");
        assert_eq!(hits.len(), 1, "lines were {:?}", ix.lines);
        let y = ix.lines[hits[0]].y;
        assert!(
            y > 0.0 && y < loaded.fb.height() as f32,
            "hit at {y} is outside a page {} tall",
            loaded.fb.height()
        );

        assert_eq!(ix.outline.len(), 1);
        assert!(ix.outline[0].y < y, "the heading is above the match");
    }

    #[test]
    fn a_body_line_is_exactly_one_terminal_row() {
        // the viewer writes over the page in cells, which only
        // lines up while a line box is the cell it sits on
        for (w, h) in [(7, 14), (9, 18), (10, 20), (14, 32), (8, 30), (20, 20)] {
            let theme = theme_for(CellSize { w, h });
            assert!(
                (theme.row() - h as f32).abs() < 0.01,
                "a {w}x{h} cell gave a row of {}",
                theme.row()
            );
        }
    }

    #[test]
    fn a_page_is_drawn_to_the_height_asked_for() {
        // the one-shot render gets a screenful
        assert_eq!(drawn_height(6000.0, 800.0, hints(800, 600)), 600.0);
        assert_eq!(drawn_height(200.0, 800.0, hints(800, 600)), 200.0);
        // the viewer asks for the document and gets all of it
        assert_eq!(drawn_height(6000.0, 800.0, hints(800, u32::MAX)), 6000.0);
    }

    #[test]
    fn the_pixel_ceiling_overrides_a_generous_caller() {
        // the ceiling is on pixels, so it buys fewer rows the
        // wider the page is
        let wide = drawn_height(f32::MAX, 8000.0, hints(8000, 100_000));
        assert_eq!(wide, MAX_PIXELS as f32 / 8000.0);

        let narrow = drawn_height(f32::MAX, 800.0, hints(8000, 100_000));
        assert!(narrow > wide, "a narrower page should fit more rows");
    }

    #[test]
    fn a_long_document_comes_back_whole() {
        // the viewer scrolls by cropping this buffer, so the
        // buffer has to hold every line, not one screenful
        let md = "# Heading\n\n".to_owned() + &"A paragraph of body text. ".repeat(400);
        let got = load(md.as_bytes(), hints(800, u32::MAX)).unwrap();

        assert!(
            got.fb.height() > 2400,
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
        let theme = theme_for(CellSize { w: 14, h: 32 });

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
