//! The one font family the markdown renderer draws with, compiled in.
//
// resvg resolves font names through a fontdb, and usvg's default Options
// carries an empty one. Worse, usvg declares fontdb with default-features off,
// which compiles out load_system_fonts, load_font_file and load_fonts_dir
// alike, leaving load_font_data as the only way in. So the bytes have to come
// from here. src/source/pdf/via_hayro.rs does the same thing for the base-14
// PDF fonts, for the same reason.
//
// Shipping the font rather than borrowing the system's also settles the
// advance ratio below. Monospace layout is arithmetic over that one number,
// and a number that describes a font we chose is a fact, whereas one that
// describes whatever `monospace` happened to resolve to is a guess.

use resvg::usvg;

/// Liberation Mono, SIL OFL 1.1. License text sits beside the files.
//
// not JetBrains Mono, whose 600/1000 advance is tidier: its GSUB carries
// `calt`, which is how it implements coding ligatures, and harfrust applies
// calt by default. `->` and `!=` would each shape to a single glyph, so a code
// block would draw narrower than the column arithmetic believes it is.
// Liberation Mono carries only `dlig`, which no shaper applies unasked.
const REGULAR: &[u8] = include_bytes!("../../../assets/fonts/LiberationMono-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../../../assets/fonts/LiberationMono-Bold.ttf");
const ITALIC: &[u8] = include_bytes!("../../../assets/fonts/LiberationMono-Italic.ttf");
const BOLD_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/LiberationMono-BoldItalic.ttf");

/// The family name the faces above register under, and that the generated SVG
/// asks for by name.
pub const FAMILY: &str = "Liberation Mono";

/// Advance width as a fraction of the em, from the font's own hmtx: 1229 over
/// a 2048 unit em. Every column-to-pixel conversion goes through this.
pub const ADVANCE_RATIO: f32 = 1229.0 / 2048.0;

/// Ascent as a fraction of the em, from hhea. Layout places a line's box and
/// then drops the baseline this far into it.
pub const ASCENT: f32 = 1705.0 / 2048.0;

/// Options with the four faces loaded and the family set as the default, so
/// unstyled text still lands on a real font rather than on nothing.
pub fn options() -> usvg::Options<'static> {
    let mut opt = usvg::Options {
        font_family: FAMILY.to_owned(),
        ..usvg::Options::default()
    };
    let db = opt.fontdb_mut();
    for face in [REGULAR, BOLD, ITALIC, BOLD_ITALIC] {
        db.load_font_data(face.to_vec());
    }
    db.set_monospace_family(FAMILY);
    opt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_face_loads() {
        // a face that fails to parse is not an error, it is a silently empty
        // page, so check the db actually took all four
        let opt = options();
        assert_eq!(opt.fontdb.len(), 4);
    }

    #[test]
    fn the_family_resolves() {
        // the generated SVG asks for FAMILY by name; if the name in the font
        // does not match the one we emit, text renders blank
        let opt = options();
        let query = usvg::fontdb::Query {
            families: &[usvg::fontdb::Family::Name(FAMILY)],
            ..Default::default()
        };
        assert!(opt.fontdb.query(&query).is_some(), "{FAMILY} did not resolve");
    }
}
