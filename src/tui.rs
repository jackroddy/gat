use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::framebuffer::{self, Framebuffer};
use crate::geometry::CellSize;
use crate::render::kitty::{self, Placement};
use crate::source;
use crate::term::{self, RawTty, Terminal};

/// How long to block on input before looking for a terminal resize.
//
// polling beats installing a SIGWINCH handler here: the ioctl costs
// microseconds and a signal handler cannot safely do anything but set a flag
const TICK: Duration = Duration::from_millis(250);

/// How long to wait for the rest of a keypress that arrived in pieces.
//
// a terminal writes an arrow key as one three-byte burst and a local pty
// delivers it whole, but the network under an ssh session may split it across
// two reads. a buffer cut after the escape byte decodes as the escape key,
// which quits the viewer, so a buffer ending mid sequence is worth waiting on
//
// only a buffer that really does end in an escape pays this, which is either
// a split sequence or the escape key itself. the remote window is the wider
// one because a lost segment costs a round trip to retransmit
const CONTINUATION: Duration = Duration::from_millis(50);
const CONTINUATION_SSH: Duration = Duration::from_millis(200);

/// Headroom transmitted beyond what the viewport shows, so zooming in has
/// real pixels to enlarge rather than a blur.
const ZOOM_HEADROOM: u32 = 2;

const MAX_ZOOM: f64 = 32.0;
const MIN_ZOOM: f64 = 1.0;

pub fn run(
    files: &[PathBuf],
    terminal: Terminal,
    background: [u8; 3],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tty = RawTty::open().ok_or("cannot open /dev/tty")?;
    let _screen = Screen::enter()?;
    let mut out = BufWriter::new(std::io::stdout());
    event_loop(&mut out, &mut tty, files, terminal, background)
}

/// The alternate screen buffer, left on drop.
//
// the spec requires the terminal to clear every image in the alternate screen
// when leaving it, so quitting disposes of the images with no bookkeeping
struct Screen;

impl Screen {
    fn enter() -> std::io::Result<Screen> {
        let mut out = std::io::stdout();
        out.write_all(b"\x1b[?1049h\x1b[?25l")?;
        out.flush()?;
        Ok(Screen)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
    }
}

struct Shown {
    image: Framebuffer,
    id: u32,
    kind: source::Kind,
}

/// Which part of the image the viewer is looking at.
struct View {
    zoom: f64,

    /// The point of the source image held at the centre of the viewport, in
    /// source pixels.
    cx: f64,
    cy: f64,
}

impl View {
    fn reset(image: &Framebuffer, kind: source::Kind, cells: (u32, u32), cell: CellSize) -> View {
        let mut view = View {
            zoom: 1.0,
            cx: image.width() as f64 / 2.0,
            cy: image.height() as f64 / 2.0,
        };
        // a picture opens centred, because the middle is where the subject is.
        // a document opens at the top, because that is where the reading
        // starts, and centring a long one would drop the reader into the
        // middle of it
        if kind == source::Kind::Document {
            let p = placement(image, kind, &view, cells, cell);
            view.cy = p.src_h as f64 / 2.0;
        }
        view
    }
}

enum Key {
    Quit,
    Pan(f64, f64),
    Zoom(f64),
    Reset,
    Next,
    Prev,
}

fn event_loop(
    out: &mut impl Write,
    tty: &mut RawTty,
    files: &[PathBuf],
    terminal: Terminal,
    background: [u8; 3],
) -> Result<(), Box<dyn std::error::Error>> {
    let cell = terminal.cell;
    let continuation = if term::over_ssh() {
        CONTINUATION_SSH
    } else {
        CONTINUATION
    };
    let mut cells = term::current_cells();
    let mut index = 0usize;
    let mut shown: Option<Shown> = None;
    let mut failure: Option<String> = None;
    let mut view = View {
        zoom: 1.0,
        cx: 0.0,
        cy: 0.0,
    };
    let mut load_wanted = true;
    let mut dirty = true;

    loop {
        if load_wanted {
            load_wanted = false;
            failure = None;
            match load(&files[index], cells, cell, background) {
                Ok(mut fresh) => {
                    fresh.id = kitty::next_id();
                    kitty::transmit(out, &fresh.image, fresh.id)?;
                    view = View::reset(&fresh.image, fresh.kind, cells, cell);
                    // place the new image before dropping the old one, so the
                    // screen never shows the gap between them
                    let previous = shown.replace(fresh);
                    draw(out, &shown, &view, cells, cell, files, index, &failure)?;
                    if let Some(old) = previous {
                        kitty::forget(out, old.id)?;
                    }
                    out.flush()?;
                    dirty = false;
                }
                Err(e) => {
                    failure = Some(e.to_string());
                    if let Some(old) = shown.take() {
                        kitty::forget(out, old.id)?;
                    }
                    dirty = true;
                }
            }
        }

        if dirty {
            draw(out, &shown, &view, cells, cell, files, index, &failure)?;
            out.flush()?;
            dirty = false;
        }

        let input = read_burst(tty, TICK, continuation);
        if input.is_empty() {
            let now = term::current_cells();
            if now != cells {
                cells = now;
                dirty = true;
            }
            continue;
        }

        for key in keys_from(&input) {
            match key {
                Key::Quit => return Ok(()),
                Key::Next if index + 1 < files.len() => {
                    index += 1;
                    load_wanted = true;
                }
                Key::Prev if index > 0 => {
                    index -= 1;
                    load_wanted = true;
                }
                Key::Next | Key::Prev => {}
                Key::Reset => {
                    if let Some(s) = &shown {
                        view = View::reset(&s.image, s.kind, cells, cell);
                    }
                    dirty = true;
                }
                Key::Zoom(factor) => {
                    view.zoom = (view.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
                    dirty = true;
                }
                Key::Pan(dx, dy) => {
                    if let Some(s) = &shown {
                        let p = placement(&s.image, s.kind, &view, cells, cell);
                        view.cx += dx * p.src_w as f64;
                        view.cy += dy * p.src_h as f64;
                    }
                    dirty = true;
                }
            }
        }
    }
}

/// Decode the source rectangle and destination cell box for the current view.
//
// the terminal does the scaling: the source rectangle is clipped to the image
// and then stretched to fill the cell box, so panning and zooming cost one
// escape sequence each and never re-encode a pixel
fn placement(
    image: &Framebuffer,
    kind: source::Kind,
    view: &View,
    cells: (u32, u32),
    cell: CellSize,
) -> Placement {
    let (img_w, img_h) = (image.width() as f64, image.height() as f64);
    let (cols, rows) = (cells.0.max(1), cells.1.saturating_sub(1).max(1));
    let view_w = (cols * cell.w) as f64;
    let view_h = (rows * cell.h) as f64;

    // zoom 1.0 shows the whole image, but never enlarges it past its own
    // pixels, which matches what the one-shot path does without --upscale.
    //
    // a document is fitted on width alone. fitting its height too would shrink
    // a long page until the text was unreadable, and would leave nothing to
    // pan to, because the whole thing would already be on screen. width-only
    // is what every pager does: full size, and scroll to read on
    let base = match kind {
        source::Kind::Document => (view_w / img_w).min(1.0),
        source::Kind::Image => (view_w / img_w).min(view_h / img_h).min(1.0),
    };
    let scale = base * view.zoom;

    let shown_w = (img_w * scale).min(view_w);
    let shown_h = (img_h * scale).min(view_h);
    let src_w = (shown_w / scale).round().clamp(1.0, img_w);
    let src_h = (shown_h / scale).round().clamp(1.0, img_h);

    Placement {
        src_x: (view.cx - src_w / 2.0).clamp(0.0, (img_w - src_w).max(0.0)) as u32,
        src_y: (view.cy - src_h / 2.0).clamp(0.0, (img_h - src_h).max(0.0)) as u32,
        src_w: src_w as u32,
        src_h: src_h as u32,
        cols: ((shown_w / cell.w as f64).ceil() as u32).clamp(1, cols),
        rows: ((shown_h / cell.h as f64).ceil() as u32).clamp(1, rows),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    out: &mut impl Write,
    shown: &Option<Shown>,
    view: &View,
    cells: (u32, u32),
    cell: CellSize,
    files: &[PathBuf],
    index: usize,
    failure: &Option<String>,
) -> std::io::Result<()> {
    // erase from the cursor down clears text but, per the protocol, must not
    // touch graphics; only a full CSI 2 J would drop the image as well
    out.write_all(b"\x1b[H\x1b[J")?;

    if let Some(s) = shown {
        kitty::place(out, s.id, &placement(&s.image, s.kind, view, cells, cell))?;
    }

    let name = files[index]
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let status = match failure {
        Some(e) => format!("{name}  {e}"),
        None => format!(
            "{name}  [{}/{}]  {:.0}%  hjkl/arrows pan  +/- zoom  0 reset  n/p file  q quit",
            index + 1,
            files.len(),
            view.zoom * 100.0
        ),
    };
    let width = cells.0 as usize;
    write!(out, "\x1b[{};1H\x1b[K", cells.1)?;
    out.write_all(status.chars().take(width).collect::<String>().as_bytes())
}

fn load(
    path: &Path,
    cells: (u32, u32),
    cell: CellSize,
    background: [u8; 3],
) -> Result<Shown, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let kind = source::kind(&bytes, path);

    // headroom buys real pixels to zoom into, which a photo needs because its
    // detail is already there to be found. a document has no such detail: it
    // is drawn at whatever size it is asked for, and asking for double gives
    // back the same words at double the size, to be displayed at half. That is
    // four times the pixels for an identical-looking page, and on a long
    // document it is the difference between a framebuffer of tens and of
    // hundreds of megabytes
    let headroom = match kind {
        source::Kind::Document => 1,
        source::Kind::Image => ZOOM_HEADROOM,
    };
    let hints = source::Hints {
        max_w: cells.0 * cell.w * headroom,
        max_h: cells.1 * cell.h * headroom,
        // the viewer pans over what it is given, so a flowed source should
        // hand over the whole document rather than one screen of it
        overflow: source::Overflow::Keep,
    };
    let decoded = source::load(&bytes, path, hints)?;

    // transmitting the full source of a large photo would burn the terminal's
    // image quota for detail no zoom level reaches. a document is exempt: its
    // height is the document, and shrinking it to a screenful would throw away
    // the text that panning is for
    let height_limit = match kind {
        source::Kind::Document => f64::INFINITY,
        source::Kind::Image => hints.max_h as f64,
    };
    let scale = (hints.max_w as f64 / decoded.width() as f64)
        .min(height_limit / decoded.height() as f64)
        .min(1.0);
    let mut fb = if scale < 1.0 {
        framebuffer::resize(
            &decoded,
            (decoded.width() as f64 * scale).round().max(1.0) as u32,
            (decoded.height() as f64 * scale).round().max(1.0) as u32,
        )
    } else {
        decoded
    };
    framebuffer::flatten_onto(&mut fb, background);
    Ok(Shown {
        image: fb,
        id: 0,
        kind,
    })
}

/// Read one burst of input, waiting out a keypress delivered in pieces.
fn read_burst(tty: &mut RawTty, first: Duration, rest: Duration) -> Vec<u8> {
    let mut buf = tty.read_available(first);
    while !buf.is_empty() && decode_keys(&buf).1 < buf.len() {
        let more = tty.read_available(rest);
        if more.is_empty() {
            // nothing is in flight after all, so the tail is all there is
            break;
        }
        buf.extend_from_slice(&more);
    }
    buf
}

/// Every key in `buf`, which is taken to be a complete burst.
fn keys_from(buf: &[u8]) -> Vec<Key> {
    let (mut keys, used) = decode_keys(buf);
    // an escape still standing alone once [`read_burst`] has waited is the
    // escape key, not the head of a sequence that has yet to arrive. anything
    // longer left over is a sequence the terminal never finished, and stays
    // dropped
    if buf.len() - used == 1 && buf[used] == 0x1b {
        keys.push(Key::Quit);
    }
    keys
}

/// Decode the keys in `buf`, and report how many bytes were consumed. A
/// trailing escape sequence that is not yet whole is left unconsumed.
fn decode_keys(buf: &[u8]) -> (Vec<Key>, usize) {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        // the terminal answers graphics commands with an APC sequence. q=2 is
        // documented as suppressing failures rather than acknowledgements, so
        // skip anything that arrives rather than read it as typing
        if buf[i..].starts_with(b"\x1b_") {
            match find(&buf[i..], b"\x1b\\") {
                Some(end) => i += end + 2,
                None => break,
            }
            continue;
        }
        if buf[i..].starts_with(b"\x1b[") {
            let Some(end) = buf[i + 2..]
                .iter()
                .position(|b| (0x40..=0x7e).contains(b))
                .map(|p| i + 2 + p)
            else {
                break;
            };
            match buf[end] {
                b'A' => keys.push(Key::Pan(0.0, -0.2)),
                b'B' => keys.push(Key::Pan(0.0, 0.2)),
                b'C' => keys.push(Key::Pan(0.2, 0.0)),
                b'D' => keys.push(Key::Pan(-0.2, 0.0)),
                _ => {}
            }
            i = end + 1;
            continue;
        }
        // an escape with nothing behind it may be the whole keypress or may
        // be the head of a sequence still on the wire; the caller settles it
        if buf[i] == 0x1b && i + 1 == buf.len() {
            break;
        }
        match buf[i] {
            b'q' | 0x1b | 0x03 => keys.push(Key::Quit),
            b'h' => keys.push(Key::Pan(-0.2, 0.0)),
            b'l' => keys.push(Key::Pan(0.2, 0.0)),
            b'k' => keys.push(Key::Pan(0.0, -0.2)),
            b'j' => keys.push(Key::Pan(0.0, 0.2)),
            b'+' | b'=' => keys.push(Key::Zoom(1.25)),
            b'-' | b'_' => keys.push(Key::Zoom(0.8)),
            b'0' => keys.push(Key::Reset),
            b'n' | b' ' => keys.push(Key::Next),
            b'p' => keys.push(Key::Prev),
            _ => {}
        }
        i += 1;
    }
    (keys, i)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: CellSize = CellSize { w: 10, h: 20 };

    fn image(w: u32, h: u32) -> Framebuffer {
        Framebuffer::new(w, h)
    }

    fn view(zoom: f64, cx: f64, cy: f64) -> View {
        View { zoom, cx, cy }
    }

    #[test]
    fn a_document_is_fitted_on_width_and_scrolls() {
        // a picture wants to be seen whole, so it is fitted on both axes and
        // has nowhere to pan at rest. a page of text fitted that way would be
        // shrunk until it was unreadable, and would leave nothing to scroll to
        let page = Framebuffer::new(900, 6000);
        let v = view(1.0, 450.0, 200.0);

        let doc = placement(&page, source::Kind::Document, &v, (100, 30), CELL);
        assert!(
            doc.src_h < 6000,
            "a document showed its whole height at rest, so there is no scroll"
        );

        let pic = placement(&page, source::Kind::Image, &v, (100, 30), CELL);
        assert_eq!(pic.src_h, 6000, "a picture should still be fitted whole");
        assert!(
            doc.src_h < pic.src_h,
            "the document should show less at once than the fitted picture"
        );
    }

    #[test]
    fn a_document_opens_at_its_first_line() {
        // centring a long document drops the reader into the middle of it
        let page = Framebuffer::new(900, 6000);
        let v = View::reset(&page, source::Kind::Document, (100, 30), CELL);
        let p = placement(&page, source::Kind::Document, &v, (100, 30), CELL);
        assert_eq!(p.src_y, 0, "document did not open at the top");
    }

    #[test]
    fn unzoomed_shows_the_whole_image() {
        let img = image(1000, 500);
        let p = placement(&img, source::Kind::Image, &view(1.0, 500.0, 250.0), (80, 25), CELL);
        assert_eq!((p.src_x, p.src_y), (0, 0));
        assert_eq!((p.src_w, p.src_h), (1000, 500));
    }

    #[test]
    fn zooming_in_shrinks_the_source_rectangle() {
        let img = image(1000, 500);
        let wide = placement(&img, source::Kind::Image, &view(1.0, 500.0, 250.0), (80, 25), CELL);
        let close = placement(&img, source::Kind::Image, &view(2.0, 500.0, 250.0), (80, 25), CELL);
        assert!(close.src_w < wide.src_w && close.src_h < wide.src_h);
        // the axis that was already filling the viewport halves exactly; the
        // letterboxed axis shows less than half, because zooming first eats
        // the bars
        assert_eq!(close.src_w, wide.src_w / 2);
        assert!(close.src_h > wide.src_h / 2);
    }

    #[test]
    fn the_source_rectangle_keeps_the_display_box_aspect_ratio() {
        let img = image(1000, 500);
        for zoom in [1.0, 1.5, 2.0, 8.0] {
            let p = placement(&img, source::Kind::Image, &view(zoom, 500.0, 250.0), (80, 25), CELL);
            let src = p.src_w as f64 / p.src_h as f64;
            let dst = (p.cols * CELL.w) as f64 / (p.rows * CELL.h) as f64;
            assert!(
                (src - dst).abs() < 0.12,
                "zoom {zoom}: source {src:.3} vs box {dst:.3}"
            );
        }
    }

    #[test]
    fn the_cell_box_never_exceeds_the_viewport() {
        let img = image(4000, 3000);
        for zoom in [1.0, 2.0, 8.0, 32.0] {
            let p = placement(&img, source::Kind::Image, &view(zoom, 2000.0, 1500.0), (80, 25), CELL);
            assert!(p.cols <= 80, "cols {} at zoom {zoom}", p.cols);
            // one row is held back for the status line
            assert!(p.rows <= 24, "rows {} at zoom {zoom}", p.rows);
        }
    }

    #[test]
    fn panning_past_an_edge_clamps_inside_the_image() {
        let img = image(1000, 500);
        let p = placement(&img, source::Kind::Image, &view(4.0, -9000.0, -9000.0), (80, 25), CELL);
        assert_eq!((p.src_x, p.src_y), (0, 0));

        let q = placement(&img, source::Kind::Image, &view(4.0, 9000.0, 9000.0), (80, 25), CELL);
        assert_eq!(q.src_x + q.src_w, 1000);
        assert_eq!(q.src_y + q.src_h, 500);
    }

    #[test]
    fn a_small_image_is_not_enlarged_at_rest() {
        let img = image(40, 30);
        let p = placement(&img, source::Kind::Image, &view(1.0, 20.0, 15.0), (80, 25), CELL);
        assert_eq!((p.src_w, p.src_h), (40, 30));
        assert_eq!((p.cols, p.rows), (4, 2));
    }

    #[test]
    fn graphics_acknowledgements_are_not_read_as_keys() {
        let keys = keys_from(b"\x1b_Gi=31,I=1;OK\x1b\\");
        assert!(keys.is_empty());

        let mixed = keys_from(b"\x1b_Gi=31;OK\x1b\\q");
        assert!(matches!(mixed.as_slice(), [Key::Quit]));
    }

    #[test]
    fn arrow_keys_pan_and_modifiers_are_swallowed_whole() {
        assert!(matches!(
            keys_from(b"\x1b[A").as_slice(),
            [Key::Pan(0.0, y)] if *y < 0.0
        ));
        // a modified arrow carries parameters before the final byte; the
        // parser must not mistake those digits for commands
        assert!(matches!(
            keys_from(b"\x1b[1;5C").as_slice(),
            [Key::Pan(x, 0.0)] if *x > 0.0
        ));
    }

    #[test]
    fn a_lone_escape_quits() {
        assert!(matches!(keys_from(b"\x1b").as_slice(), [Key::Quit]));
    }

    #[test]
    fn an_arrow_key_split_by_the_network_is_not_a_quit() {
        // the first half of a keypress that ssh delivered in two reads. it
        // must be held back rather than decoded, or panning quits the viewer
        let (keys, used) = decode_keys(b"\x1b");
        assert!(keys.is_empty());
        assert_eq!(used, 0, "the escape has to survive for the next read");

        // and once the rest lands, it pans like any other arrow key
        assert!(matches!(
            keys_from(b"\x1b[A").as_slice(),
            [Key::Pan(0.0, y)] if *y < 0.0
        ));
    }

    #[test]
    fn a_key_before_a_split_sequence_still_registers() {
        // a burst cut mid sequence must not cost the keys ahead of the cut
        let (keys, used) = decode_keys(b"n\x1b[");
        assert!(matches!(keys.as_slice(), [Key::Next]));
        assert_eq!(used, 1);
    }
}
