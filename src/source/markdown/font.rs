//! The one font family the markdown renderer draws with, compiled in.
//
// usvg's default Options carries an empty fontdb, and declares
// fontdb with default-features off, compiling out
// load_system_fonts, load_font_file and load_fonts_dir, so
// load_font_data is the only way in

use resvg::usvg;

/// Liberation Mono, SIL OFL 1.1. License text sits beside the files.
//
// not JetBrains Mono: its GSUB carries `calt`, which harfrust
// applies by default, so `->` would shape to one glyph and a
// code block would draw narrower than the column arithmetic
// predicts. Liberation Mono carries only `dlig`
const REGULAR: &[u8] = include_bytes!("../../../assets/fonts/LiberationMono-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../../../assets/fonts/LiberationMono-Bold.ttf");
const ITALIC: &[u8] = include_bytes!("../../../assets/fonts/LiberationMono-Italic.ttf");
const BOLD_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/LiberationMono-BoldItalic.ttf");

/// The family name the faces register under, and that the
/// generated SVG asks for.
pub const FAMILY: &str = "Liberation Mono";

/// Advance width as a fraction of the em, from the font's own
/// hmtx: 1229 over a 2048 unit em.
pub const ADVANCE_RATIO: f32 = 1229.0 / 2048.0;

/// Ascent as a fraction of the em, from hhea.
pub const ASCENT: f32 = 1705.0 / 2048.0;

/// Descent as a fraction of the em, from hhea's descender of -615.
//
// ASCENT + DESCENT is 1.133 em, so the ink of a line is
// taller than its type size and a row has to be at least
// that much taller again before the text can be moved down
// inside it
pub const DESCENT: f32 = 615.0 / 2048.0;

/// Options with the four faces loaded and the family set as the default.
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
        // a face that fails to parse gives a blank page, not an error
        let opt = options();
        assert_eq!(opt.fontdb.len(), 4);
    }

    #[test]
    fn the_family_resolves() {
        // text renders blank if the font's name is not FAMILY
        let opt = options();
        let query = usvg::fontdb::Query {
            families: &[usvg::fontdb::Family::Name(FAMILY)],
            ..Default::default()
        };
        assert!(opt.fontdb.query(&query).is_some(), "{FAMILY} did not resolve");
    }
}
