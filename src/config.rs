use std::path::{Path, PathBuf};

/// Whether gat opens the viewer or prints and exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Print,
    View,
}

/// The settings read from the config file, each `None` where it is unset.
#[derive(Debug, Default, PartialEq)]
pub struct Config {
    pub mode: Option<Mode>,
    pub max_px: Option<u64>,
}

/// The file gat writes where there is none: every setting, commented out at
/// its default.
const DEFAULT: &str = "\
# gat settings. Uncomment a line to change it; a flag on the command line
# wins over this file.

# what a run does with neither -p nor -i: \"view\" or \"print\"
# mode = \"view\"

# the most pixels an image sent to the terminal may have, as a count or as a
# string such as \"2M\". a document's width is held to the square root
# cap = 1048576
";

/// Read the config file, or the defaults where there is none, writing the
/// default file in that case.
pub fn load() -> Result<Config, String> {
    let Some(path) = path() else {
        return Ok(Config::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text).map_err(|(line, e)| format!("{}:{line}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // a home gat cannot write to is no reason to refuse
            // to show a picture, so a failure here is dropped
            if write_default(&path).is_ok() {
                eprintln!("gat: wrote default settings to {}", path.display());
            }
            Ok(Config::default())
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn write_default(path: &Path) -> std::io::Result<()> {
    use std::io::Write as _;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    // create_new, so two runs starting at once cannot both
    // write, and a file that appeared since the read survives
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(DEFAULT.as_bytes())
}

/// Where the config file lives: `$XDG_CONFIG_HOME/gat/config.toml`, falling
/// back to `~/.config`.
fn path() -> Option<PathBuf> {
    // the spec says a relative XDG_CONFIG_HOME is invalid and
    // is to be ignored
    // see: https://specifications.freedesktop.org/basedir-spec/latest/
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(base.join("gat").join("config.toml"))
}

/// Parse the config file's text, or say which line is wrong and why.
fn parse(text: &str) -> Result<Config, (usize, String)> {
    // a flat subset of TOML: `key = value` lines, comments and
    // blank lines, with values that are strings or integers.
    // what it accepts is valid TOML, so a real parser can take
    // over without anyone rewriting their file
    let mut config = Config::default();
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            return Err((n, "tables are not supported; settings go at the top level".into()));
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| (n, format!("expected key = value, found {line:?}")))?;
        let (key, value) = (key.trim(), value.trim());

        match key {
            "mode" => {
                if config.mode.is_some() {
                    return Err((n, "mode is set twice".into()));
                }
                config.mode = Some(match string(value).map_err(|e| (n, e))? {
                    "print" => Mode::Print,
                    "view" => Mode::View,
                    other => return Err((n, format!("mode is \"print\" or \"view\", not {other:?}"))),
                });
            }
            "cap" => {
                if config.max_px.is_some() {
                    return Err((n, "cap is set twice".into()));
                }

                // an integer as TOML writes one, or a string for
                // the suffixed forms --cap takes, since 2M is not
                // a TOML number
                let px = match string(value) {
                    Ok(s) => parse_pixels(s),
                    Err(_) if !value.is_empty()
                        && value.chars().all(|c| c.is_ascii_digit() || c == '_') =>
                    {
                        parse_pixels(&value.replace('_', ""))
                    }
                    Err(_) => Err(format!("cap is an integer, or a string such as \"2M\", not {value}")),
                };
                config.max_px = Some(px.map_err(|e| (n, e))?);
            }
            other => return Err((n, format!("unknown key {other:?}"))),
        }
    }
    Ok(config)
}

/// `line` up to a `#` that is not inside a string.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

/// The contents of a double-quoted TOML string holding no escapes.
fn string(value: &str) -> Result<&str, String> {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .filter(|v| !v.contains(['"', '\\']))
        .ok_or_else(|| format!("expected a quoted string, found {value}"))
}

/// Parse a pixel count: a whole number, or one with a K or M suffix for
/// thousands or millions, as in `2M` or `1.5M`.
pub fn parse_pixels(s: &str) -> Result<u64, String> {
    let bad = || format!("cap wants a pixel count such as 1048576 or 2M, not {s:?}");
    let (num, unit) = match s.char_indices().last() {
        Some((i, 'k' | 'K')) => (&s[..i], 1e3),
        Some((i, 'm' | 'M')) => (&s[..i], 1e6),
        _ => (s, 1.0),
    };
    let n: f64 = num.parse().map_err(|_| bad())?;
    let px = (n * unit).round();
    if !(px >= 1.0 && px < u64::MAX as f64) {
        return Err(bad());
    }
    Ok(px as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pixel_count_takes_a_suffix_or_none() {
        assert_eq!(parse_pixels("1048576").unwrap(), 1_048_576);
        assert_eq!(parse_pixels("2M").unwrap(), 2_000_000);
        assert_eq!(parse_pixels("1.5m").unwrap(), 1_500_000);
        assert_eq!(parse_pixels("500K").unwrap(), 500_000);
        for bad in ["", "0", "M", "-1", "2G", "1e30M", "NaN"] {
            assert!(parse_pixels(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn both_settings_are_read() {
        let text = "# gat\n\nmode = \"print\"  # one-shot\ncap = 2_000_000\n";
        assert_eq!(
            parse(text),
            Ok(Config {
                mode: Some(Mode::Print),
                max_px: Some(2_000_000),
            })
        );
        assert_eq!(parse("cap = \"1.5M\"").unwrap().max_px, Some(1_500_000));
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn a_mistake_names_its_line() {
        for (text, line) in [
            ("mode = \"view\"\ncolour = 3", 2),
            ("\n\nmode = \"alt\"", 3),
            ("mode = view", 1),
            ("cap = 2G", 1),
            ("cap = 2M", 1),
            ("cap = 1e9", 1),
            ("cap = 1\ncap = 2", 2),
            ("[viewer]\nmode = \"view\"", 1),
            ("just words", 1),
        ] {
            let got = parse(text);
            assert!(matches!(got, Err((n, _)) if n == line), "{text:?} gave {got:?}");
        }
    }

    #[test]
    fn the_default_file_sets_nothing_and_uncommented_gives_the_defaults() {
        // commented out, so a later default reaches a user who
        // never edited the file
        assert_eq!(parse(DEFAULT), Ok(Config::default()));

        let live: String = DEFAULT
            .lines()
            .map(|l| l.strip_prefix("# ").filter(|l| l.contains(" = ")).unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            parse(&live),
            Ok(Config {
                mode: Some(Mode::View),
                max_px: Some(crate::MAX_PX),
            })
        );
    }

    #[test]
    fn a_hash_inside_a_string_is_not_a_comment() {
        assert_eq!(strip_comment("mode = \"a#b\" # c"), "mode = \"a#b\" ");
    }
}
