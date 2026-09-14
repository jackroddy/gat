//! Turning a laid-out page into an SVG document.
//
// This is the replaceable half. Layout produces positions and styles in
// pixels; everything that knows about XML, usvg quirks or escaping lives here,
// so swapping resvg for a glyph rasterizer later means rewriting this file and
// nothing else.

use std::fmt::Write;

use super::font;
use super::layout::{Item, Page, Rgb};

/// Render `page` as an SVG document.
pub fn emit(page: &Page, bg: Rgb) -> String {
    let mut s = String::with_capacity(page.items.len() * 96 + 512);

    // xml:space, because usvg implements SVG's whitespace collapsing and would
    // otherwise strip the indentation off every line of every code block.
    // font-kerning, because layout positions each run by multiplying columns
    // by one fixed advance, and a kern pair would put the glyphs somewhere
    // that arithmetic does not predict
    let _ = write!(
        s,
        "<svg xmlns='http://www.w3.org/2000/svg' width='{w}' height='{h}' \
         viewBox='0 0 {w} {h}' xml:space='preserve'>\
         <rect width='100%' height='100%' fill='{bg}'/>\
         <g font-family='{family}' font-kerning='none'>",
        w = page.w,
        h = page.h,
        bg = hex(bg),
        family = font::FAMILY,
    );

    for item in &page.items {
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

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// Escape `s` for use as XML character data, dropping anything XML 1.0
/// forbids outright.
//
// the dropping matters as much as the escaping. XML 1.0 permits almost no C0
// control characters, and roxmltree rejects the whole document over a single
// stray one, so a form feed sitting in a source file would turn into a parse
// error rather than a page with an odd character on it. markdown is written by
// hand and pasted into, so these do turn up
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            // C0 apart from the three above, DEL and the C1 block, and the two
            // noncharacters XML names explicitly
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
        // a form feed has no escape that XML 1.0 accepts; emitting one as
        // &#12; is just as fatal as emitting it raw, so it has to go
        assert_eq!(escape("page\u{c}break"), "pagebreak");
        assert_eq!(escape("nul\0byte"), "nulbyte");
        // tab and newline are the C0 characters XML does allow
        assert_eq!(escape("a\tb\nc"), "a\tb\nc");
    }

    #[test]
    fn text_is_left_alone_otherwise() {
        assert_eq!(escape("héllo 世界 🦀"), "héllo 世界 🦀");
    }
}
