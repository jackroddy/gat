//! Colouring for the body of a fenced code block.

use super::layout::Rgb;

/// A run of code that is all one colour.
pub struct Span {
    pub text: String,
    pub fill: Rgb,
}

/// Colour every line of `lines`, falling back to `plain` where the info
/// string names no language this build can parse.
pub fn spans(lang: Option<&str>, lines: &[String], plain: Rgb) -> Vec<Vec<Span>> {
    #[cfg(feature = "syntax")]
    if let Some(coloured) = self::syntax::spans(lang, lines) {
        return coloured;
    }

    let _ = lang;
    lines
        .iter()
        .map(|l| {
            vec![Span {
                text: l.clone(),
                fill: plain,
            }]
        })
        .collect()
}

#[cfg(feature = "syntax")]
mod syntax {
    use std::sync::OnceLock;

    use syntect::easy::HighlightLines;
    use syntect::highlighting::{Color, Theme, ThemeSet};
    use syntect::parsing::SyntaxSet;

    use super::{Rgb, Span};

    /// The theme the colours come from.
    //
    // ocean's background is 2b303b against the page's
    // 2a2a2a, so only its foregrounds are used and the block
    // keeps the tint layout already draws
    const THEME: &str = "base16-ocean.dark";

    pub fn spans(lang: Option<&str>, lines: &[String]) -> Option<Vec<Vec<Span>>> {
        // both dumps cost tens of milliseconds to
        // deserialize, and a page may hold many blocks
        static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
        static THEMES: OnceLock<Option<Theme>> = OnceLock::new();

        let set = SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
        let syntax = set.find_syntax_by_token(lang?)?;
        let theme = THEMES
            .get_or_init(|| ThemeSet::load_defaults().themes.remove(THEME))
            .as_ref()?;

        let mut h = HighlightLines::new(syntax, theme);
        Some(
            lines
                .iter()
                .map(|line| {
                    // a syntax's end-of-line rules do not fire
                    // without the newline, so it is added for
                    // the highlighter and dropped again after
                    let owned = format!("{line}\n");
                    match h.highlight_line(&owned, set) {
                        Ok(ranges) => ranges
                            .iter()
                            .filter_map(|(style, text)| {
                                let text = text.trim_end_matches('\n');
                                (!text.is_empty()).then(|| Span {
                                    text: text.to_owned(),
                                    fill: rgb(style.foreground),
                                })
                            })
                            .collect(),
                        Err(_) => Vec::new(),
                    }
                })
                .collect(),
        )
    }

    fn rgb(c: Color) -> Rgb {
        Rgb(c.r, c.g, c.b)
    }
}
