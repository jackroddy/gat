//! Turning a laid-out page into an SVG document.

use std::fmt::Write;

use super::font;
use super::layout::{Item, Page, Rgb};

/// Render the slice of `page` from `from_y` for `h` pixels.
pub fn emit(page: &Page, bg: Rgb, from_y: f32, h: f32) -> String {
    let mut s = String::with_capacity(page.items.len() * 96 + 512);

    // xml:space: usvg collapses whitespace and would strip the
    // indentation off every line of every code block.
    // font-kerning: layout positions each run by multiplying
    // columns by one fixed advance, off which a kern pair moves
    let _ = write!(
        s,
        "<svg xmlns='http://www.w3.org/2000/svg' width='{w}' height='{h}' \
         viewBox='0 0 {w} {h}' xml:space='preserve'>\
         <rect width='100%' height='100%' fill='{bg}'/>\
         <g font-family='{family}' font-kerning='none' \
         transform='translate(0,{shift})'>",
        w = page.w,
        bg = hex(bg),
        family = font::FAMILY,
        shift = -from_y,
    );

    for item in &page.items {
        // the SVG parse and the rasterize then cost what is on
        // screen rather than the length of the document
        if !visible(item, from_y, h) {
            continue;
        }
        match item {
            Item::Rect { x, y, w, h, fill } => {
                let _ = write!(
                    s,
                    "<rect x='{x}' y='{y}' width='{w}' height='{h}' fill='{}'/>",
                    hex(*fill)
                );
            }
            Item::Run {
                x,
                baseline,
                text,
                size,
                bold,
                italic,
                strike,
                fill,
                ..
            } => {
                let _ = write!(s, "<text x='{x}' y='{baseline}' font-size='{size}'");
                if *bold {
                    s.push_str(" font-weight='bold'");
                }
                if *italic {
                    s.push_str(" font-style='italic'");
                }
                if *strike {
                    s.push_str(" text-decoration='line-through'");
                }
                let _ = write!(s, " fill='{}'>{}</text>", hex(*fill), escape(text));
            }
        }
    }

    s.push_str("</g></svg>");
    s
}

/// Whether `item` puts any ink inside the band starting at `from_y`.
fn visible(item: &Item, from_y: f32, h: f32) -> bool {
    let (top, bottom) = match item {
        Item::Rect { y, h, .. } => (*y, y + h),

        // a run is positioned by its baseline; its ink reaches
        // 1.2 em above it and 0.4 em below
        //
        // TODO: both eyeballed, not read from the font metrics
        Item::Run { baseline, size, .. } => (baseline - size * 1.2, baseline + size * 0.4),
    };
    bottom >= from_y && top <= from_y + h
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// Escape `s` for use as XML character data, dropping anything XML 1.0
/// forbids outright.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),

            // XML 1.0 permits almost no C0, and roxmltree
            // rejects a whole document over one stray control
            // character, so these are dropped, not escaped
            c if (c as u32) < 0x20 => {}
            c if ('\u{7f}'..='\u{9f}').contains(&c) => {}
            '\u{fffe}' | '\u{ffff}' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_characters_survive_as_entities() {
        assert_eq!(escape("a < b & c > d"), "a &lt; b &amp; c &gt; d");
        assert_eq!(escape(r#"say "hi""#), "say &quot;hi&quot;");
    }

    #[test]
    fn characters_xml_forbids_are_dropped_not_escaped() {
        // &#12; is as fatal to XML 1.0 as the raw form feed
        assert_eq!(escape("page\u{c}break"), "pagebreak");
        assert_eq!(escape("nul\0byte"), "nulbyte");

        // tab, newline and CR are the C0 characters XML allows
        assert_eq!(escape("a\tb\nc"), "a\tb\nc");
    }

    #[test]
    fn text_is_left_alone_otherwise() {
        assert_eq!(escape("héllo 世界 🦀"), "héllo 世界 🦀");
    }
}
