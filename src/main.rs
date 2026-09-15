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
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rimg: {e}");
            return ExitCode::from(2);
        }
    };

    let terminal = term::Terminal::detect();
    if terminal.protocol != Protocol::Kitty && !args.force {
        eprintln!(
            "rimg: this terminal does not support the kitty graphics protocol \
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
        if let Err(e) = tui::run(&args.files, terminal, args.background) {
            eprintln!("rimg: {e}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }

    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut failed = false;
    for path in &args.files {
        if let Err(e) = show(&mut out, path, &budget, args.background) {
            let _ = out.flush();
            eprintln!("rimg: {}: {e}", path.display());
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
    background: [u8; 3],
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let hints = source::Hints {
        max_w: budget.cols * budget.cell.w,
        max_h: budget.rows * budget.cell.h,
    };
    let decoded = source::load(&bytes, path, hints)?.fb;

    let (w, h) = geometry::fit(decoded.width(), decoded.height(), budget);
    let mut fb = framebuffer::resize(&decoded, w, h);
    framebuffer::flatten_onto(&mut fb, background);

    render::kitty::write(out, &fb, render::kitty::next_id())?;

    // the renderer sets C=1, so the cursor is still at the
    // image's top left corner and the next output would
    // land on top of it
    let rows = h.div_ceil(budget.cell.h);
    for _ in 0..rows {
        out.write_all(b"\n")?;
    }
    out.write_all(b"\r")?;
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
            Long("print") => args.print = true,
            Long("probe") => {
                print!("terminal probe:\n{}", term::explain());
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
                println!("rimg {}", env!("CARGO_PKG_VERSION"));
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
usage: rimg [options] <file>...

  -g, --geometry WxH   fit within W columns by H rows
  -U, --upscale        enlarge images smaller than the available space
  -W, --fit-width      use the full width, letting height overflow
      --fit-height     use the full height, letting width overflow
  -b, --background C   composite transparency over color C (#rrggbb)
      --print          write the image to stdout and exit, no viewer
      --force-kitty    emit kitty sequences even if detection says no
      --probe          report what terminal detection sees, then exit
  -h, --help           this text
  -V, --version        version
";
