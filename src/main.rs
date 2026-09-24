mod framebuffer;
mod geometry;
mod render;
mod source;
mod term;
mod tui;

use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

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
    print: bool,
    keep: bool,
    cap: u32,
}

/// The most pixels a side of an image sent to the terminal may have, unless
/// `--cap` says otherwise.
const CAP: u32 = 1024;

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

    // the viewer needs a terminal to draw on and a keyboard to read; without
    // both, a piped or redirected run still gets the one-shot rendering
    let interactive = !args.print && rustix::termios::isatty(rustix::stdio::stdout());
    if interactive {
        if let Err(e) = tui::run(&args.files, terminal.cell, args.cap, args.background) {
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
        if let Err(e) = show(&mut out, path, &budget, args.cap, args.background, &mut ids) {
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
    cap: u32,
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
        cap,
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
            let over = geometry::over_cap(f64::from(w), f64::from(h), cap);
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
        print: false,
        keep: false,
        cap: CAP,
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
            Short('p') | Long("print") => args.print = true,
            Long("cap") => {
                args.cap = parser
                    .value()?
                    .string()?
                    .parse()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| lexopt::Error::from("cap wants a pixel count above 0"))?;
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
      --cap N          send no image wider or taller than N pixels (1024);
                       a document is capped on width only
      --keep           leave images from earlier runs in the terminal
      --force-kitty    emit kitty sequences even if detection says no
      --probe          report what terminal detection sees, then exit
  -h, --help           this text
  -V, --version        version
";
