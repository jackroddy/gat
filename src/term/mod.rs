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

/// The terminal being written to: its size, its cell size, and its protocol.
#[derive(Clone, Copy, Debug)]
pub struct Terminal {
    pub cols: u32,
    pub rows: u32,
    pub cell: CellSize,
    pub protocol: Protocol,
}

/// The longest detection will wait for a reply from a terminal on this
/// machine.
//
// TODO: 250ms has no recorded source
const LOCAL_CEILING: Duration = Duration::from_millis(250);

/// The longest detection will wait for a reply from a terminal at the far end
/// of an ssh connection.
//
// TODO: 1500ms has no recorded source
const REMOTE_CEILING: Duration = Duration::from_millis(1500);

impl Terminal {
    /// The terminal this process is writing to, as far as the environment and
    /// the terminal itself will say.
    pub fn detect() -> Terminal {
        detect_measured().0
    }
}

/// How the round trip to the terminal went, for `--probe` to account for.
enum Measured {
    /// The environment answered every question, so no tty was opened.
    NotNeeded,
    /// Something needed asking, but there was no terminal to ask on.
    NoTty,
    /// The terminal answered a status report this quickly, or not at all.
    Link(Option<Duration>),
}

fn detect_measured() -> (Terminal, Measured) {
    let (cols, rows, pixel_cell) = window_size();
    let env_protocol = protocol_from_env();

    // touching termios at all risks leaving a stray reply
    // in the input queue, where it reaches the shell as if
    // the user had typed it, so open the tty only for a
    // question the environment cannot answer
    //
    // the transport is one of those questions: nothing in the
    // environment says whether this terminal will read a file,
    // and the answer decides whether scrolling costs anything
    let asking = pixel_cell.is_none() || env_protocol.is_none();
    let mut probe = asking.then(query::Probe::open).flatten();

    let ceiling = reply_ceiling();
    let measured = match (asking, probe.as_mut()) {
        (false, _) => Measured::NotNeeded,
        (true, None) => Measured::NoTty,
        (true, Some(p)) => Measured::Link(calibrate(p, ceiling)),
    };
    let rtt = match measured {
        Measured::Link(rtt) => rtt,
        _ => None,
    };

    // a terminal silent through a status report is absent,
    // or behind a multiplexer without passthrough; either
    // way graphics escapes would not reach it, so stop
    // asking and leave the protocol at None
    let mut live = match (probe.as_mut(), rtt) {
        (Some(p), Some(rtt)) => Some((p, reply_budget(rtt, ceiling))),
        _ => None,
    };

    let cell = pixel_cell
        .or_else(|| live.as_mut().and_then(|(p, b)| query_cell_size(p, *b)))
        .unwrap_or(CellSize::FALLBACK);
    let protocol = env_protocol.unwrap_or_else(|| {
        live.as_mut()
            .map_or(Protocol::None, |(p, b)| query_protocol(p, *b))
    });

    // a terminal across a network cannot open a path of ours,
    // so the question is only worth asking locally

    let terminal = Terminal {
        cols,
        rows,
        cell,
        protocol,
    };
    (terminal, measured)
}

/// Time one round trip to the terminal: how long a device status report takes
/// to come back.
fn calibrate(probe: &mut query::Probe, ceiling: Duration) -> Option<Duration> {
    // every terminal answers a device status report; the
    // replies to the two queries below vary by terminal
    let rtt = probe.timed(b"\x1b[5n", ceiling, |b| find(b, b"\x1b[0n").is_some())?;

    // a late reply crosses the link too, so the window for
    // catching one before the tty returns to the shell
    // grows with the link
    probe.set_grace(rtt);
    Some(rtt)
}

/// How long to wait for a reply, given a measured round trip.
fn reply_budget(rtt: Duration, ceiling: Duration) -> Duration {
    // headroom for jitter, and for a terminal that takes
    // longer to compose a version string than a status
    // report
    //
    // TODO: the factor of four is unmeasured
    (rtt * 4).clamp(LOCAL_CEILING, ceiling)
}

fn reply_ceiling() -> Duration {
    // a ceiling is paid in full only by a terminal that
    // stays silent, and one that answers no status report
    // would not render an image either, so the remote
    // ceiling only lengthens a run that already fails
    if over_ssh() { REMOTE_CEILING } else { LOCAL_CEILING }
}

/// Whether this process is at the far end of an ssh connection.
pub fn over_ssh() -> bool {
    // sshd sets both in the session it spawns, and neither
    // leaks into an unrelated local shell, so either one
    // settles it
    std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some()
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

    // zero cells means the terminal has no size to report,
    // the same as the ioctl failing outright
    let ws = ws.filter(|w| w.ws_col > 0 && w.ws_row > 0);
    let Some(ws) = ws else {
        // TODO: the 80x24 fallback has no recorded source
        return (80, 24, None);
    };
    let (cols, rows) = (ws.ws_col as u32, ws.ws_row as u32);
    let (xp, yp) = (ws.ws_xpixel as u32, ws.ws_ypixel as u32);

    // a terminal that does not track pixels reports zeroes, and some report
    // a nonsense pair instead; anything that would imply a cell narrower
    // than 2px or shorter than 4px is one of those
    //
    // TODO: the 2px and 4px floors have no recorded source
    let cell = (xp >= 2 * cols && yp >= 4 * rows).then(|| CellSize {
        w: xp / cols,
        h: yp / rows,
    });
    (cols, rows, cell)
}

fn query_cell_size(probe: &mut query::Probe, budget: Duration) -> Option<CellSize> {
    // CSI 16 t asks for the cell size directly; the reply is
    // CSI 6 ; <height> ; <width> t, height first
    let reply = probe.ask(b"\x1b[16t", budget, |b| {
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

fn query_protocol(probe: &mut query::Probe, budget: Duration) -> Protocol {
    // XTVERSION (CSI > q) is not universally answered, so a
    // device status request follows it: the CSI 0 n reply
    // marks the end of the exchange even when the version
    // query drew nothing
    let reply = probe.ask(b"\x1b[>q\x1b[5n", budget, |b| {
        find(b, b"\x1b[0").is_some()
    });

    // ghostty answers with "libghostty", so these match as substrings
    //
    // TODO: Konsole's place in this list is unverified here
    for name in [&b"kitty"[..], b"ghostty", b"WezTerm", b"Konsole"] {
        if find(&reply, name).is_some() {
            return Protocol::Kitty;
        }
    }
    Protocol::None
}

/// Store and place an image at a range of heights, and report
/// what the terminal said to each.
//
// this is the viewer's own sequence: store the pixels inline,
// then place a screenful of them with a source rectangle. The
// test images are one colour, so they deflate to almost nothing
// and the ladder costs little despite the sizes it names.
//
// the replies are read through a Probe rather than left on the
// tty, because an unread refusal reaches the shell and turns up
// at the next prompt
pub fn height_ladder(width: u32, view_h: u32) -> String {
    use std::fmt::Write as _;
    use std::io::Write as _;

    let mut s = String::new();
    let Some(mut probe) = query::Probe::open() else {
        return "  no terminal to ask\n".into();
    };
    let budget = Duration::from_millis(400);

    for (n, h) in [8192u32, 8500, 9000, 9500, 10000, 11000, 12000]
        .into_iter()
        .enumerate()
    {
        let id = 700 + n as u32;
        let mb = (width as u64 * h as u64 * 4) as f64 / 1e6;

        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        let row: Vec<u8> = (0..width).flat_map(|_| [0xd0u8, 0x40, 0xa0, 0xff]).collect();
        for _ in 0..h {
            let _ = z.write_all(&row);
        }
        let Ok(payload) = z.finish() else { continue };
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &payload);

        let stored = chunked(&mut probe, id, width, h, &b64, budget);
        let place =
            format!("\x1b_Ga=p,i={id},x=0,y=0,w={width},h={view_h},c=24,r=1,q=1;\x1b\\");
        let placed = said(&mut probe, &place, budget);
        let _ = writeln!(
            s,
            "  {width} x {h:<6} {mb:>5.0} MB  store: {stored:<26} place: {placed}"
        );
    }
    s
}

/// Send one image as the protocol's chunked escapes and report
/// what came back.
fn chunked(
    probe: &mut query::Probe,
    id: u32,
    w: u32,
    h: u32,
    b64: &str,
    budget: Duration,
) -> String {
    let mut chunks = b64.as_bytes().chunks(4096).peekable();
    let mut first = true;
    let mut last = String::from("accepted (no complaint)");
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        let head = if first {
            format!("\x1b_Ga=t,i={id},q=1,f=32,o=z,s={w},v={h},m={more};")
        } else {
            format!("\x1b_Gq=1,m={more};")
        };
        first = false;
        let escape = format!("{head}{}\x1b\\", String::from_utf8_lossy(chunk));

        // only the last chunk can draw a complaint worth waiting for
        let wait = if more == 0 {
            budget
        } else {
            Duration::from_millis(1)
        };
        let reply = said(probe, &escape, wait);
        if more == 0 {
            last = reply;
        }
    }
    last
}

/// Send `escape` and report the terminal's reply, or silence.
//
// silence is the good answer under q=1: successes are suppressed
// and only failures come back
fn said(probe: &mut query::Probe, escape: &str, budget: Duration) -> String {
    let reply = probe.ask(escape.as_bytes(), budget, |b| find(b, b"\x1b\\").is_some());
    if reply.is_empty() {
        return "accepted (no complaint)".into();
    }
    String::from_utf8_lossy(&reply)
        .trim_matches(|c: char| c.is_control() || c == '\\')
        .to_string()
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
        "SSH_TTY",
        "SSH_CONNECTION",
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

    let ceiling = reply_ceiling();
    let (t, m) = detect_measured();
    let _ = match m {
        Measured::NotNeeded => writeln!(
            s,
            "  link round trip        <not measured, environment sufficed>"
        ),
        Measured::NoTty => writeln!(
            s,
            "  link round trip        <not measured, no terminal to ask>"
        ),
        Measured::Link(Some(rtt)) => writeln!(
            s,
            "  link round trip        {}ms (replies given {}ms)",
            rtt.as_millis(),
            reply_budget(rtt, ceiling).as_millis(),
        ),
        Measured::Link(None) => writeln!(
            s,
            "  link round trip        <silent for {}ms, gave up>",
            ceiling.as_millis(),
        ),
    };

    let _ = writeln!(s, "  cell size used         {}x{}", t.cell.w, t.cell.h);
    let _ = writeln!(s, "  protocol               {:?}", t.protocol);
    s
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_terminals_keep_the_budget_they_always_had() {
        // a sub-millisecond round trip must not scale the
        // budget below the local ceiling
        let rtt = Duration::from_micros(200);
        assert_eq!(reply_budget(rtt, LOCAL_CEILING), LOCAL_CEILING);
        assert_eq!(reply_budget(rtt, REMOTE_CEILING), LOCAL_CEILING);
    }

    #[test]
    fn a_slow_link_stretches_the_budget_but_not_past_the_ceiling() {
        // 90ms round trip: 4 * 90ms, under the remote ceiling
        let atlantic = reply_budget(Duration::from_millis(90), REMOTE_CEILING);
        assert_eq!(atlantic, Duration::from_millis(360));

        // 800ms round trip: 4 * 800ms clamps to the ceiling
        let awful = reply_budget(Duration::from_millis(800), REMOTE_CEILING);
        assert_eq!(awful, REMOTE_CEILING);
    }

    #[test]
    fn the_ceiling_only_lifts_for_a_link_worth_waiting_on() {
        // reply_ceiling only ever widens the wait, and only
        // over ssh
        assert!(REMOTE_CEILING > LOCAL_CEILING);
    }
}
