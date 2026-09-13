mod query;

pub use query::Probe as RawTty;

use std::time::Duration;

use crate::geometry::CellSize;

/// The graphics protocol a terminal will accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Kitty,
    None,
}

/// What we managed to learn about the terminal we are writing to.
#[derive(Clone, Copy, Debug)]
pub struct Terminal {
    pub cols: u32,
    pub rows: u32,
    pub cell: CellSize,
    pub protocol: Protocol,
}

impl Terminal {
    pub fn detect() -> Terminal {
        let (cols, rows, pixel_cell) = window_size();
        let env_protocol = protocol_from_env();

        // touching termios at all risks leaving a stray reply in the input
        // queue, where it reaches the shell as if the user had typed it, so
        // open the tty only for a question the environment cannot answer
        let mut probe = (pixel_cell.is_none() || env_protocol.is_none())
            .then(query::Probe::open)
            .flatten();

        let cell = pixel_cell
            .or_else(|| probe.as_mut().and_then(query_cell_size))
            .unwrap_or(CellSize::FALLBACK);
        let protocol =
            env_protocol.unwrap_or_else(|| query_protocol(probe.as_mut()));

        Terminal {
            cols,
            rows,
            cell,
            protocol,
        }
    }
}

/// The terminal's current size in cells, re-read from the kernel.
pub fn current_cells() -> (u32, u32) {
    let (cols, rows, _) = window_size();
    (cols, rows)
}

fn window_size() -> (u32, u32, Option<CellSize>) {
    let ws = [
        rustix::stdio::stdout(),
        rustix::stdio::stderr(),
        rustix::stdio::stdin(),
    ]
    .into_iter()
    .find_map(|fd| rustix::termios::tcgetwinsize(fd).ok());

    // a terminal that reports zero cells is telling us it does not know, the
    // same as the ioctl failing outright; a 1x1 viewport is never the answer
    let ws = ws.filter(|w| w.ws_col > 0 && w.ws_row > 0);
    let Some(ws) = ws else {
        return (80, 24, None);
    };
    let (cols, rows) = (ws.ws_col as u32, ws.ws_row as u32);
    let (xp, yp) = (ws.ws_xpixel as u32, ws.ws_ypixel as u32);

    // a terminal that does not track pixels reports zeroes, and some report
    // a nonsense pair instead; anything that would imply a cell narrower
    // than 2px or shorter than 4px is one of those
    let cell = (xp >= 2 * cols && yp >= 4 * rows).then(|| CellSize {
        w: xp / cols,
        h: yp / rows,
    });
    (cols, rows, cell)
}

fn query_cell_size(probe: &mut query::Probe) -> Option<CellSize> {
    // CSI 16 t asks for the cell size directly; the reply is
    // CSI 6 ; <height> ; <width> t, height first
    let reply = probe.ask(b"\x1b[16t", Duration::from_millis(50), |b| {
        find(b, b"\x1b[6;").is_some_and(|i| b[i..].contains(&b't'))
    });
    let at = find(&reply, b"\x1b[6;")? + 4;
    let tail = &reply[at..];
    let end = tail.iter().position(|&c| c == b't')?;
    let mut parts = std::str::from_utf8(&tail[..end]).ok()?.split(';');
    let h: u32 = parts.next()?.trim().parse().ok()?;
    let w: u32 = parts.next()?.trim().parse().ok()?;
    (w > 0 && h > 0).then_some(CellSize { w, h })
}

/// Identify the protocol from the environment alone, without writing
/// anything to the terminal.
//
// TERM is not reliable on its own: ghostty and kitty are routinely run with
// TERM=xterm-256color so that ssh to a host without their terminfo still
// works, which is exactly when the escape-sequence probe below matters least
// and costs most
fn protocol_from_env() -> Option<Protocol> {
    let term = std::env::var("TERM").unwrap_or_default();
    if term.contains("kitty") || term.contains("ghostty") {
        return Some(Protocol::Kitty);
    }
    match std::env::var("TERM_PROGRAM").unwrap_or_default().as_str() {
        "ghostty" | "kitty" | "WezTerm" => return Some(Protocol::Kitty),
        _ => {}
    }
    let marks = [
        "KITTY_WINDOW_ID",
        "GHOSTTY_RESOURCES_DIR",
        "GHOSTTY_BIN_DIR",
        "WEZTERM_EXECUTABLE",
    ];
    marks
        .iter()
        .any(|v| std::env::var_os(v).is_some())
        .then_some(Protocol::Kitty)
}

fn query_protocol(probe: Option<&mut query::Probe>) -> Protocol {
    let Some(probe) = probe else {
        return Protocol::None;
    };

    // XTVERSION (CSI > q) is not universally answered, so a device status
    // request rides along behind it: its CSI 0 n reply marks the end of the
    // exchange even when the version query drew nothing
    let reply = probe.ask(b"\x1b[>q\x1b[5n", Duration::from_millis(250), |b| {
        find(b, b"\x1b[0").is_some()
    });

    // ghostty answers with "libghostty", so these match as substrings
    for name in [&b"kitty"[..], b"ghostty", b"WezTerm", b"Konsole"] {
        if find(&reply, name).is_some() {
            return Protocol::Kitty;
        }
    }
    Protocol::None
}

/// A human-readable account of what detection saw, for `--probe`.
pub fn explain() -> String {
    use std::fmt::Write;

    let mut s = String::new();
    for v in [
        "TERM",
        "TERM_PROGRAM",
        "KITTY_WINDOW_ID",
        "GHOSTTY_RESOURCES_DIR",
        "GHOSTTY_BIN_DIR",
        "WEZTERM_EXECUTABLE",
    ] {
        let val = std::env::var(v).unwrap_or_else(|_| "<unset>".into());
        let _ = writeln!(s, "  {v:<22} {val}");
    }

    let (cols, rows, pixel_cell) = window_size();
    let _ = writeln!(s, "  ioctl cells            {cols}x{rows}");
    let _ = match pixel_cell {
        Some(c) => writeln!(s, "  ioctl cell pixels      {}x{}", c.w, c.h),
        None => writeln!(s, "  ioctl cell pixels      <not reported>"),
    };

    let _ = match protocol_from_env() {
        Some(p) => writeln!(s, "  protocol from env      {p:?}"),
        None => writeln!(s, "  protocol from env      <undecided, will query>"),
    };

    let t = Terminal::detect();
    let _ = writeln!(s, "  cell size used         {}x{}", t.cell.w, t.cell.h);
    let _ = writeln!(s, "  protocol               {:?}", t.protocol);
    s
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
