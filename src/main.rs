mod config;
mod framebuffer;
mod geometry;
mod render;
mod source;
mod term;
mod tui;

use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use config::Mode;
use geometry::Budget;
use term::Protocol;

struct Args {
    files: Vec<PathBuf>,
    geometry: Option<(u32, u32)>,
    upscale: bool,
    fill_width: bool,
    fill_height: bool,
    background: [u8; 3],
    force: bool,

    /// `-p` or `-i`, whichever came last.
    mode: Option<Mode>,
    keep: bool,
    max_px: Option<u64>,
}

/// The most pixels an image sent to the terminal may have, unless `--cap`
/// says otherwise.
const MAX_PX: u64 = 1024 * 1024;

/// Where the one-shot renderer's image ids come from.
enum Ids {
    /// The same block every run, so only the latest run's images are stored.
    Block(u32),

    /// Fresh every run, so images earlier runs left stay where they are.
    Fresh,
}

impl Ids {
    fn next(&mut self) -> u32 {
        match self {
            Ids::Block(n) => {
                let id = render::kitty::print_id(*n);
                *n += 1;
                id
            }
            Ids::Fresh => render::kitty::next_id(),
        }
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gat: {e}");
            return ExitCode::from(2);
        }
    };

    // read after the arguments, so --help and --version work
    // with a broken config file
    let config = match config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("gat: {e}");
            return ExitCode::from(2);
        }
    };
    let max_px = args.max_px.or(config.max_px).unwrap_or(MAX_PX);

    // the viewer needs a terminal to draw on and a keyboard to read; without
    // both, a piped or redirected run still gets the one-shot rendering,
    // unless -i asked for the viewer by name
    let tty = rustix::termios::isatty(rustix::stdio::stdout());
    if args.mode == Some(Mode::View) && !tty {
        eprintln!("gat: -i needs a terminal on stdout");
        return ExitCode::from(2);
    }
    let interactive = tty && args.mode.or(config.mode).unwrap_or(Mode::View) == Mode::View;

    let terminal = term::Terminal::detect();
    if terminal.protocol != Protocol::Kitty && !args.force {
        eprintln!(
            "gat: this terminal does not support the kitty graphics protocol \
             (--probe says what detection saw, --force-kitty overrides)"
        );
        return ExitCode::from(1);
    }

    // TODO: the two-cell margin has no recorded origin
    let (cols, rows) = args
        .geometry
        .unwrap_or((terminal.cols.saturating_sub(2).max(1), terminal.rows.saturating_sub(2).max(1)));
    let budget = Budget {
        cols,
        rows,
        cell: terminal.cell,
        upscale: args.upscale,
        fill_width: args.fill_width,
        fill_height: args.fill_height,
    };

    if interactive {
        if let Err(e) = tui::run(&args.files, terminal.cell, max_px, args.background) {
            eprintln!("gat: {e}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }

    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut failed = false;

    // one run's images replace the last one's, so a terminal
    // does not end the day holding every picture ever printed
    // into it. --keep is for a scrollback you want to keep
    let mut ids = if args.keep {
        Ids::Fresh
    } else {
        if let Err(e) = render::kitty::forget_prints(&mut out) {
            eprintln!("gat: {e}");
            return ExitCode::FAILURE;
        }
        Ids::Block(0)
    };

    for path in &args.files {
        if let Err(e) = show(&mut out, path, &budget, max_px, args.background, &mut ids) {
            let _ = out.flush();
            eprintln!("gat: {}: {e}", path.display());
            failed = true;
        }
    }
    if out.flush().is_err() {
        failed = true;
    }

    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn show(
    out: &mut impl Write,
    path: &std::path::Path,
    budget: &Budget,
    max_px: u64,
    background: [u8; 3],
    ids: &mut Ids,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let flowed = source::kind(&bytes, path) == source::Kind::Document;
    let hints = source::Hints {
        max_w: (f64::from(budget.cols) * budget.cell.w) as u32,
        // a document is written into the scrollback in pieces, so
        // it is not cut off at the fold the way a page with an
        // edge of its own would be
        max_h: if flowed {
            u32::MAX
        } else {
            (f64::from(budget.rows) * budget.cell.h) as u32
        },
        cell: budget.cell,
        max_px,
    };

    let pieces = source::pieces(&bytes, path, hints)?;
    let drawn = pieces.per;
    for piece in pieces {
        let decoded = piece?;
        let (dw, dh) = (f64::from(decoded.width()), f64::from(decoded.height()));

        // a picture is fitted to the screen; a document was laid
        // out at the terminal's own text size and fitting it
        // would undo that
        let (mut fb, per) = if flowed {
            (decoded, drawn)
        } else {
            let (w, h) = geometry::fit(
                (dw * drawn).round() as u32,
                (dh * drawn).round() as u32,
                budget,
            );
            let over = geometry::over_cap(f64::from(w), f64::from(h), max_px);
            let (fw, fh) = (
                (f64::from(w) / over).round().max(1.0),
                (f64::from(h) / over).round().max(1.0),
            );

            // a vector source is already drawn under the cap, and
            // a pixel's disagreement in rounding is not worth a
            // resample
            let fb = if (fw - dw).abs() <= 1.0 && (fh - dh).abs() <= 1.0 {
                decoded
            } else {
                framebuffer::resize(&decoded, fw as u32, fh as u32)
            };
            (fb, over)
        };
        framebuffer::flatten_onto(&mut fb, background);

        // the tolerance absorbs a document's rows, which are
        // whole cells but reach here through a float division
        let span = |px: u32, cell: f64| (f64::from(px) * per / cell - 1e-6).ceil().max(1.0) as u32;
        let rows = span(fb.height(), budget.cell.h);

        // a capped image covers the cells it would have covered
        // uncapped, and the terminal scales it up into them
        let cells = (per > 1.0).then(|| (span(fb.width(), budget.cell.w), rows));
        render::kitty::write(out, &fb, ids.next(), cells)?;

        // the renderer sets C=1, so the cursor is still at the
        // image's top left corner and the next output would
        // land on top of it
        for _ in 0..rows {
            out.write_all(b"\n")?;
        }
        out.write_all(b"\r")?;
    }
    Ok(())
}

fn parse_args() -> Result<Option<Args>, lexopt::Error> {
    use lexopt::prelude::*;

    let mut args = Args {
        files: Vec::new(),
        geometry: None,
        upscale: false,
        fill_width: false,
        fill_height: false,
        background: [0, 0, 0],
        force: false,
        mode: None,
        keep: false,
        max_px: None,
    };

    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next()? {
        match arg {
            Short('g') | Long("geometry") => {
                let raw = parser.value()?.string()?;
                let (w, h) = raw
                    .split_once(['x', 'X'])
                    .ok_or_else(|| lexopt::Error::from("geometry wants WxH"))?;
                args.geometry = Some((
                    w.parse().map_err(|_| lexopt::Error::from("bad width"))?,
                    h.parse().map_err(|_| lexopt::Error::from("bad height"))?,
                ));
            }
            Short('U') | Long("upscale") => args.upscale = true,
            Short('W') | Long("fit-width") => args.fill_width = true,
            Long("fit-height") => args.fill_height = true,
            Long("force-kitty") => args.force = true,
            Short('p') | Long("print") => args.mode = Some(Mode::Print),
            Short('i') | Long("interactive") => args.mode = Some(Mode::View),
            Long("cap") => {
                let raw = parser.value()?.string()?;
                args.max_px = Some(config::parse_pixels(&raw).map_err(lexopt::Error::from)?);
            }
            Long("keep") => args.keep = true,
            Long("probe") => {
                print!("terminal probe:\n{}", term::explain());
                let t = term::Terminal::detect();
                print!(
                    "\nhow tall an image this terminal will take:\n{}",
                    term::height_ladder(
                        (f64::from(t.cols) * t.cell.w) as u32,
                        (f64::from(t.rows.saturating_sub(1)) * t.cell.h) as u32,
                    )
                );
                return Ok(None);
            }
            Short('b') | Long("background") => {
                args.background = parse_color(&parser.value()?.string()?)?;
            }
            Short('h') | Long("help") => {
                print!("{HELP}");
                return Ok(None);
            }
            Short('V') | Long("version") => {
                println!("gat {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            Value(v) => args.files.push(PathBuf::from(v)),
            _ => return Err(arg.unexpected()),
        }
    }

    if args.files.is_empty() {
        print!("{HELP}");
        return Ok(None);
    }
    Ok(Some(args))
}

fn parse_color(s: &str) -> Result<[u8; 3], lexopt::Error> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 {
        return Err(lexopt::Error::from("colors look like #rrggbb"));
    }
    let mut out = [0u8; 3];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| lexopt::Error::from("colors look like #rrggbb"))?;
    }
    Ok(out)
}

const HELP: &str = "\
usage: gat [options] <file>...

  -g, --geometry WxH   fit within W columns by H rows
  -U, --upscale        enlarge images smaller than the available space
  -W, --fit-width      use the full width, letting height overflow
      --fit-height     use the full height, letting width overflow
  -b, --background C   composite transparency over color C (#rrggbb)
  -p, --print          write the image to stdout and exit, no viewer
  -i, --interactive    open the viewer, the default unless config says print
      --cap N          send no image of more than N pixels (1048576), as
                       a count or with K or M; a document's width is held
                       to the square root instead
      --keep           leave images from earlier runs in the terminal
      --force-kitty    emit kitty sequences even if detection says no
      --probe          report what terminal detection sees, then exit
  -h, --help           this text
  -V, --version        version

settings: $XDG_CONFIG_HOME/gat/config.toml, or ~/.config/gat/config.toml
  mode = \"print\" | \"view\"   what to do with no -p or -i
  cap = 1048576 | \"2M\"      the --cap default
";

