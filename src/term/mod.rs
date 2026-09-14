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

/// The longest detection will wait for a reply: from a terminal in this
/// machine, and from one at the far end of an ssh connection.
//
// a budget is a ceiling, not a cost. a terminal that answers ends the wait
// the moment its reply lands, so only silence pays the whole thing, and a
// terminal that stays silent through a status report was not going to render
// an image either. that is what makes the remote ceiling affordable: it
// lengthens a run that already fails, and leaves every local run alone
const LOCAL_CEILING: Duration = Duration::from_millis(250);
const REMOTE_CEILING: Duration = Duration::from_millis(1500);

impl Terminal {
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

    // touching termios at all risks leaving a stray reply in the input
    // queue, where it reaches the shell as if the user had typed it, so
    // open the tty only for a question the environment cannot answer
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

    // a terminal that let a status report go unanswered is either absent or
    // behind something that swallows escape sequences, a multiplexer without
    // passthrough being the usual something. graphics escapes would not reach
    // it either, so stop asking rather than spend two more timeouts finding
    // out, and leave the protocol at None so nothing is emitted into the void
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

    let terminal = Terminal {
        cols,
        rows,
        cell,
        protocol,
    };
    (terminal, measured)
}

/// Time one round trip to the terminal, so that the waits which follow are
/// sized to the link instead of to a guess about it.
//
// a device status report is the right yardstick: every terminal back to the
// vt100 answers it, none of them has to think about the answer, and the reply
// is unmistakable. the two queries below are the ones whose answers vary by
// terminal, which makes them the wrong place to learn how far away it is
fn calibrate(probe: &mut query::Probe, ceiling: Duration) -> Option<Duration> {
    let rtt = probe.timed(b"\x1b[5n", ceiling, |b| find(b, b"\x1b[0n").is_some())?;
    // a late reply has to cross the link too, so the window for catching one
    // before the tty goes back to the shell grows with the link
    probe.set_grace(rtt);
    Some(rtt)
}

/// How long to wait for a reply, given a measured round trip.
//
// four round trips of headroom: the link jitters, and a terminal puts more
// work into composing a version string than into a status report. the floor
// is what a local terminal was given outright before there was anything to
// measure, so no terminal on this machine now waits less than it used to
fn reply_budget(rtt: Duration, ceiling: Duration) -> Duration {
    (rtt * 4).clamp(LOCAL_CEILING, ceiling)
}

fn reply_ceiling() -> Duration {
    if over_ssh() { REMOTE_CEILING } else { LOCAL_CEILING }
}

/// Whether this process is at the far end of an ssh connection. sshd sets
/// both of these in the session it spawns and neither leaks into an unrelated
/// local shell, so either one settles it.
//
// the terminal is still the local emulator and it still speaks the graphics
// protocol; all that changed is that every question now costs a network round
// trip, and that none of the variables the emulator sets about itself
// survived the hop. a link ssh cannot be spotted on, mosh or a serial console,
// still gets the local ceiling and may still need --force-kitty
pub fn over_ssh() -> bool {
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
    // XTVERSION (CSI > q) is not universally answered, so a device status
    // request rides along behind it: its CSI 0 n reply marks the end of the
    // exchange even when the version query drew nothing
    let reply = probe.ask(b"\x1b[>q\x1b[5n", budget, |b| {
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
        // sub-millisecond round trips must not scale down into a budget too
        // tight for a terminal that answers in its own good time
        let rtt = Duration::from_micros(200);
        assert_eq!(reply_budget(rtt, LOCAL_CEILING), LOCAL_CEILING);
        assert_eq!(reply_budget(rtt, REMOTE_CEILING), LOCAL_CEILING);
    }

    #[test]
    fn a_slow_link_stretches_the_budget_but_not_past_the_ceiling() {
        // a transatlantic hop: four round trips of headroom, room to spare
        let atlantic = reply_budget(Duration::from_millis(90), REMOTE_CEILING);
        assert_eq!(atlantic, Duration::from_millis(360));

        // and a bad one saturates rather than hanging for a second and a half
        // per query
        let awful = reply_budget(Duration::from_millis(800), REMOTE_CEILING);
        assert_eq!(awful, REMOTE_CEILING);
    }

    #[test]
    fn the_ceiling_only_lifts_for_a_link_worth_waiting_on() {
        // guards the direction of the check: a local run must not inherit the
        // remote ceiling and spend 1.5s discovering a terminal is unsuitable
        assert!(REMOTE_CEILING > LOCAL_CEILING);
    }
}
