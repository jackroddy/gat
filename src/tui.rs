use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::framebuffer::{self, Framebuffer};
use crate::geometry::CellSize;
use crate::render::kitty::{self, Placement};
use crate::source;
use crate::term::{self, RawTty, Terminal};

/// How long to block on input before looking for a terminal resize.
//
// polling rather than a SIGWINCH handler: the ioctl costs
// microseconds, and a handler can only safely set a flag
const TICK: Duration = Duration::from_millis(250);

/// How long to wait for the rest of a keypress that arrived in pieces.
//
// a local pty delivers an arrow key's three bytes whole, but
// an ssh session may split them across two reads, and a
// buffer cut after the escape decodes as the escape key
const CONTINUATION: Duration = Duration::from_millis(50);

/// The same wait under an ssh session.
const CONTINUATION_SSH: Duration = Duration::from_millis(200);

/// Transmitted size as a multiple of the viewport, for zoom.
//
// zooming in past the viewport needs real pixels rather
// than an enlargement of the ones already on screen
const ZOOM_HEADROOM: u32 = 2;

// TODO: the zoom range and the key steps below are eyeballed;
//       no measurement or reference is behind them
const MAX_ZOOM: f64 = 32.0;
const MIN_ZOOM: f64 = 1.0;
const PAN_STEP: f64 = 0.2;
const ZOOM_IN: f64 = 1.25;
const ZOOM_OUT: f64 = 0.8;

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
    /// The whole rasterized page, or the whole image.
    //
    // the terminal holds all of it, so scrolling is a source
    // rectangle it already has the pixels for. Handing it a
    // screenful at a time instead made it load an image per
    // keypress, which is what made scrolling drag
    image: Framebuffer,

    /// The id the terminal holds `image` under.
    id: u32,

    kind: source::Kind,

    /// Where the words are, for a document; `None` for a picture.
    index: Option<source::Index>,
}

/// Which part of the image the viewer is looking at.
struct View {
    zoom: f64,

    /// The source pixel column held at the centre of the viewport.
    cx: f64,

    /// The source pixel row held at the centre of the viewport.
    cy: f64,
}

impl View {
    fn reset(shown: &Shown, cells: (u32, u32), cell: CellSize) -> View {
        let mut view = View {
            zoom: 1.0,
            cx: shown.image.width() as f64 / 2.0,
            cy: shown.image.height() as f64 / 2.0,
        };

        // centred, a long document would open in its middle
        if shown.kind == source::Kind::Document {
            view.cy = geom(shown, &view, cells, cell).src_h / 2.0;
        }
        view
    }
}

/// The spinner's frames.
//
// braille cycles read as motion and cost one cell
const SPINNER: [char; 10] = ['\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}',
                             '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280f}'];

/// How long each spinner frame is shown.
//
// TODO: 80ms is eyeballed
const SPIN_TICK: Duration = Duration::from_millis(80);

/// How long a load may take before the spinner appears.
//
// TODO: 120ms is eyeballed
const PATIENCE: Duration = Duration::from_millis(120);

/// A decode running on a worker thread.
struct Loading {
    done: mpsc::Receiver<Result<Shown, String>>,
    began: Instant,
}

enum Key {
    Quit,
    Pan(f64, f64),
    Zoom(f64),
    Reset,
    Next,
    Prev,
    Search,

    /// Step to another search hit, forwards for 1 and back for -1.
    Hit(i32),

    /// Step to another heading, forwards for 1 and back for -1.
    Heading(i32),
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

    // what the status line says instead of the help text
    let mut note: Option<String> = None;

    // Some while a search is being typed, holding it so far
    let mut typing: Option<Vec<u8>> = None;
    let mut query = String::new();

    // lines holding the current query, as indices into the page's own
    let mut hits: Vec<usize> = Vec::new();
    let mut hit = 0usize;

    // shrunk on refusal, never below one viewport

    // the top of the band to render next; 0 for an image
    let mut dirty = true;
    let mut loading: Option<Loading> = None;

    loop {
        if load_wanted {
            load_wanted = false;
            failure = None;

            // replacing the job drops the receiver, so a
            // superseded worker's result is discarded
            loading = Some(spawn_load(files[index].clone(), cells, cell, background));
        }

        if let Some(job) = &loading {
            match job.done.try_recv() {
                Ok(result) => {
                    loading = None;
                    match result {
                        Ok(mut fresh) => {
                            view = View::reset(&fresh, cells, cell);
                            fresh.id = kitty::next_id();
                            let encoded = kitty::encode(&fresh.image)?;
                            kitty::emit(out, &encoded, fresh.id, false, kitty::Quiet::ErrorsOnly)?;

                            // place the new image before dropping the old one,
                            // so the screen never shows the gap between them
                            let previous = shown.replace(fresh);
                            hits.clear();
                            note = None;
                            draw(out, &shown, &view, cells, cell, files, index, &failure, None)?;
                            if let Some(old) = previous {
                                kitty::forget(out, old.id)?;
                            }
                            out.flush()?;
                            dirty = false;
                        }
                        Err(e) => {
                            failure = Some(e);
                            if let Some(old) = shown.take() {
                                kitty::forget(out, old.id)?;
                            }
                            dirty = true;
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    failure = Some("decode failed".into());
                    loading = None;
                    dirty = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        if dirty {
            let status = match &typing {
                Some(buf) => Some(format!("/{}", String::from_utf8_lossy(buf))),
                None => note.clone(),
            };
            draw(
                out,
                &shown,
                &view,
                cells,
                cell,
                files,
                index,
                &failure,
                status.as_deref(),
            )?;
            out.flush()?;
            dirty = false;
        }

        let wait = match &loading {
            Some(job) if job.began.elapsed() >= PATIENCE => {
                status_line(out, cells, &spinner_text(files, index, job.began))?;
                out.flush()?;
                SPIN_TICK
            }
            Some(_) => SPIN_TICK,
            None => TICK,
        };

        let input = read_burst(tty, wait, continuation);
        if input.is_empty() {
            let now = term::current_cells();
            if now != cells {
                cells = now;
                dirty = true;
            }
            continue;
        }

        // a refusal arrives as an APC mixed in with the keys
        if let Some(complaint) = graphics_error(&input) {
            failure = Some(complaint);
            dirty = true;
        }

        // a burst can hold a whole keystroke past the one
        // that ends the search, so the query takes only the
        // bytes up to it and the rest go on as keys
        let mut rest: &[u8] = &input;
        if let Some(buf) = typing.as_mut() {
            let (outcome, taken) = type_into(buf, &input);
            rest = &input[taken..];
            match outcome {
                Typed::More => {}
                Typed::Cancelled => {
                    typing = None;
                    note = None;
                }
                Typed::Done => {
                    let typed = typing.take().unwrap_or_default();
                    query = String::from_utf8_lossy(&typed).into_owned();
                    hits = match shown.as_ref().and_then(|s| s.index.as_ref()) {
                        Some(ix) => ix.find(&query),
                        None => Vec::new(),
                    };
                    hit = 0;
                    note = Some(step(&mut view, &shown, &hits, hit, &query, cells, cell));
                }
            }
            dirty = true;
        }

        for key in keys_from(rest) {
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
                        view = View::reset(s, cells, cell);
                    }
                    note = None;
                    dirty = true;
                }
                Key::Zoom(factor) => {
                    // a document is laid out at the width it is
                    // drawn at, so zooming it returns the same
                    // words larger rather than more of them
                    if !document(&shown) {
                        view.zoom = (view.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
                        dirty = true;
                    }
                }
                Key::Search => {
                    typing = Some(Vec::new());
                    dirty = true;
                }
                Key::Hit(dir) => {
                    if !hits.is_empty() {
                        let n = hits.len();
                        hit = (hit + if dir > 0 { 1 } else { n - 1 }) % n;
                    }
                    note = Some(step(&mut view, &shown, &hits, hit, &query, cells, cell));
                    dirty = true;
                }
                Key::Heading(dir) => {
                    note = Some(heading(&mut view, &shown, dir, cells, cell));
                    dirty = true;
                }
                Key::Pan(dx, dy) => {
                    if let Some(s) = &shown {
                        let g = geom(s, &view, cells, cell);
                        view.cx += dx * g.src_w;

                        // clamped to the document, not the band, so a
                        // scroll past the rendered pixels requests the
                        // next band instead of stopping at a seam
                        view.cy = (view.cy + dy * g.src_h)
                            .clamp(g.src_h / 2.0, (g.doc_h - g.src_h / 2.0).max(g.src_h / 2.0));
                    }
                    dirty = true;
                }
            }
        }
    }
}

/// What a burst of input did to a search being typed.
enum Typed {
    More,
    Done,
    Cancelled,
}

/// Feed `input` into the query in `buf`, stopping at the byte that ends the
/// search. Answers what happened and how much of `input` it consumed.
fn type_into(buf: &mut Vec<u8>, input: &[u8]) -> (Typed, usize) {
    for (i, &b) in input.iter().enumerate() {
        match b {
            b'\r' | b'\n' => return (Typed::Done, i + 1),
            0x1b | 0x03 => return (Typed::Cancelled, i + 1),
            0x7f | 0x08 => {
                // a whole character, not a byte: half of one
                // left behind would show as a replacement and
                // match nothing
                while buf.pop().is_some_and(|b| (0x80..0xc0).contains(&b)) {}
            }

            // anything below space is a control key this does
            // not handle; the rest may be part of a multi-byte
            // character
            b if b >= 0x20 => buf.push(b),
            _ => {}
        }
    }
    (Typed::More, input.len())
}

/// Whether what is on screen is a page of text rather than a picture.
fn document(shown: &Option<Shown>) -> bool {
    shown
        .as_ref()
        .is_some_and(|s| s.kind == source::Kind::Document)
}

/// Move the view to hit `at`, and say what the status line should show.
fn step(
    view: &mut View,
    shown: &Option<Shown>,
    hits: &[usize],
    at: usize,
    query: &str,
    cells: (u32, u32),
    cell: CellSize,
) -> String {
    let Some(s) = shown else {
        return format!("/{query}  nothing loaded");
    };
    let Some(ix) = &s.index else {
        return format!("/{query}  not a document");
    };
    if hits.is_empty() {
        return format!("/{query}  no matches");
    }
    let line = &ix.lines[hits[at]];
    scroll_to(view, s, line.y, cells, cell);
    format!("/{query}  {}/{}  {}", at + 1, hits.len(), line.text.trim())
}

/// Move the view to the next heading in `dir`, and say what to show.
fn heading(
    view: &mut View,
    shown: &Option<Shown>,
    dir: i32,
    cells: (u32, u32),
    cell: CellSize,
) -> String {
    let Some(s) = shown else {
        return "nothing loaded".into();
    };
    let Some(ix) = &s.index else {
        return "not a document".into();
    };

    // measured from the top of the viewport rather than its
    // centre, so the heading already on screen is behind you
    let top = view.cy - geom(s, view, cells, cell).src_h / 2.0;
    let found = if dir > 0 {
        ix.outline.iter().find(|h| f64::from(h.y) > top + 1.0)
    } else {
        ix.outline.iter().rev().find(|h| f64::from(h.y) < top - 1.0)
    };
    match found {
        Some(h) => {
            scroll_to(view, s, h.y, cells, cell);
            format!("{} {}", "#".repeat(h.level as usize), h.text.trim())
        }
        None => "no more headings".into(),
    }
}

/// Scroll so that page row `y` sits near the top of the viewport.
fn scroll_to(view: &mut View, s: &Shown, y: f32, cells: (u32, u32), cell: CellSize) {
    let g = geom(s, view, cells, cell);
    let lo = g.src_h / 2.0;
    let hi = (g.doc_h - g.src_h / 2.0).max(lo);

    // a third down rather than centred, so what follows the
    // line is what fills the screen
    view.cy = (f64::from(y) + g.src_h / 6.0).clamp(lo, hi);
}

/// Decode the source rectangle and destination cell box for the current view.
fn placement(s: &Shown, view: &View, cells: (u32, u32), cell: CellSize) -> Placement {
    let g = geom(s, view, cells, cell);

    // the terminal does the scaling: the source rectangle is
    // clipped to the image and stretched to fill the cell box,
    // so panning and zooming never re-encode a pixel
    Placement {
        src_x: (view.cx - g.src_w / 2.0).clamp(0.0, (g.img_w - g.src_w).max(0.0)) as u32,

        src_y: g.doc_top as u32,
        src_w: g.src_w as u32,
        src_h: g.src_h as u32,
        cols: ((g.shown_w / cell.w as f64).ceil() as u32).clamp(1, cells.0.max(1)),
        rows: ((g.shown_h / cell.h as f64).ceil() as u32)
            .clamp(1, cells.1.saturating_sub(1).max(1)),
    }
}

/// The shared arithmetic behind placing a band and deciding to fetch another.
struct Geom {
    img_w: f64,
    doc_h: f64,
    shown_w: f64,
    shown_h: f64,
    src_w: f64,
    src_h: f64,

    /// Top of the visible window, in document pixels.
    doc_top: f64,
}

fn geom(s: &Shown, view: &View, cells: (u32, u32), cell: CellSize) -> Geom {
    let img_w = s.image.width().max(1) as f64;
    let doc_h = s.image.height().max(1) as f64;
    let (cols, rows) = (cells.0.max(1), cells.1.saturating_sub(1).max(1));
    let view_w = (cols * cell.w) as f64;
    let view_h = (rows * cell.h) as f64;

    let base = match s.kind {
        // width alone: fitting the height too would shrink a
        // long page until the text was unreadable
        source::Kind::Document => (view_w / img_w).min(1.0),

        // fitted whole, and never enlarged past its own
        // pixels, as the one-shot path does without --upscale
        source::Kind::Image => (view_w / img_w).min(view_h / doc_h).min(1.0),
    };
    let scale = (base * view.zoom).max(f64::MIN_POSITIVE);

    let shown_w = (img_w * scale).min(view_w);
    let shown_h = (doc_h * scale).min(view_h);
    let src_w = (shown_w / scale).round().clamp(1.0, img_w);
    let src_h = (shown_h / scale).round().clamp(1.0, doc_h);
    let doc_top = (view.cy - src_h / 2.0).clamp(0.0, (doc_h - src_h).max(0.0));

    Geom {
        img_w,
        doc_h,
        shown_w,
        shown_h,
        src_w,
        src_h,
        doc_top,
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
    note: Option<&str>,
) -> std::io::Result<()> {
    // erase from the cursor down clears text but, per the protocol, must not
    // touch graphics; only a full CSI 2 J would drop the image as well
    out.write_all(b"\x1b[H\x1b[J")?;

    if let Some(s) = shown {
        kitty::place(out, s.id, &placement(s, view, cells, cell))?;
    }

    let name = files[index]
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let place = format!("[{}/{}]", index + 1, files.len());
    let status = match (failure, note) {
        (Some(e), _) => format!("{name}  {e}"),
        (None, Some(n)) => format!("{name}  {n}"),

        // the keys a picture has are not the keys a document
        // has, and listing both leaves neither room to read
        (None, None) if document(shown) => {
            format!("{name}  {place}  hjkl pan  0 top  / find  ][ hit  }}{{ head  np file  q quit")
        }
        (None, None) => format!(
            "{name}  {place}  {:.0}%  hjkl pan  +- zoom  0 reset  np file  q quit",
            view.zoom * 100.0
        ),
    };
    status_line(out, cells, &status)
}

fn load(
    path: &Path,
    cells: (u32, u32),
    cell: CellSize,
    background: [u8; 3],
) -> Result<Shown, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let kind = source::kind(&bytes, path);

    // a document is drawn at the size asked for, so headroom
    // costs four times the pixels for the same page
    let headroom = match kind {
        source::Kind::Document => 1,
        source::Kind::Image => ZOOM_HEADROOM,
    };
    let hints = source::Hints {
        max_w: cells.0 * cell.w * headroom,
        max_h: match kind {
            // the whole document: scrolling places out of this
            // buffer rather than drawing the page again
            source::Kind::Document => u32::MAX,
            source::Kind::Image => cells.1 * cell.h * headroom,
        },
        cell,
    };
    let loaded = source::load(&bytes, path, hints)?;
    let (decoded, mut index) = (loaded.fb, loaded.index);

    // a document is exempt: its height is the document, and a
    // screenful of it would drop the text panning is for
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

    // the index is in layout pixels, and the page may have
    // been shrunk to fit since
    if let Some(ix) = index.as_mut() {
        ix.scale(scale as f32);
    }
    Ok(Shown {
        image: fb,
        id: 0,
        kind,
        index,
    })
}

/// Rasterize `path` on a worker thread.
fn spawn_load(
    path: PathBuf,
    cells: (u32, u32),
    cell: CellSize,
    background: [u8; 3],
) -> Loading {
    let (tx, done) = mpsc::channel();
    std::thread::spawn(move || {
        let result = load(&path, cells, cell, background).map_err(|e| e.to_string());

        // the receiver is gone if the viewer has moved on
        let _ = tx.send(result);
    });
    Loading {
        done,
        began: Instant::now(),
    }
}

/// Which spinner glyph belongs to a wait of `elapsed`.
fn spinner_frame(elapsed: Duration) -> char {
    let step = (elapsed.as_millis() / SPIN_TICK.as_millis()) as usize;
    SPINNER[step % SPINNER.len()]
}

/// The status line shown while a decode runs.
fn spinner_text(files: &[PathBuf], index: usize, began: Instant) -> String {
    let frame = spinner_frame(began.elapsed());
    let name = files[index]
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "{frame} rendering {name}  [{}/{}]  {:.1}s  q quits",
        index + 1,
        files.len(),
        began.elapsed().as_secs_f32(),
    )
}

/// Write `text` on the bottom row, clipped to the width.
fn status_line(out: &mut impl Write, cells: (u32, u32), text: &str) -> std::io::Result<()> {
    write!(out, "\x1b[{};1H\x1b[K", cells.1)?;
    let width = cells.0 as usize;

    // the line carries a filename and a decoder's error
    // string, neither of which gat chose: a name may
    // legally hold ESC, and writing it back would run
    // whatever sequence it spells. is_control covers C1 as
    // well, which an 8-bit CSI would otherwise slip through
    let drawn: String = text.chars().filter(|c| !c.is_control()).take(width).collect();
    out.write_all(drawn.as_bytes())
}

/// The terminal's complaint about a graphics command, if `buf` holds one.
fn graphics_error(buf: &[u8]) -> Option<String> {
    let mut i = 0;

    // the protocol answers in an APC of the form
    // ESC _ G <key>=<value>,... ; <message> ESC \, where the
    // message is OK on success and ENOENT, EINVAL or similar
    while let Some(start) = find(&buf[i..], b"\x1b_G").map(|p| i + p) {
        let Some(end) = find(&buf[start..], b"\x1b\\").map(|p| start + p) else {
            break;
        };
        let body = &buf[start + 3..end];
        if let Some(semi) = body.iter().position(|&c| c == b';') {
            let message = String::from_utf8_lossy(&body[semi + 1..]);
            if !message.is_empty() && message != "OK" {
                return Some(format!("terminal refused the image: {message}"));
            }
        }
        i = end + 2;
    }
    None
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

    // an escape still alone once read_burst has waited is the
    // escape key, not the head of an unfinished sequence
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
                b'A' => keys.push(Key::Pan(0.0, -PAN_STEP)),
                b'B' => keys.push(Key::Pan(0.0, PAN_STEP)),
                b'C' => keys.push(Key::Pan(PAN_STEP, 0.0)),
                b'D' => keys.push(Key::Pan(-PAN_STEP, 0.0)),
                _ => {}
            }
            i = end + 1;
            continue;
        }

        // the caller settles what a trailing escape means
        if buf[i] == 0x1b && i + 1 == buf.len() {
            break;
        }
        match buf[i] {
            b'q' | 0x1b | 0x03 => keys.push(Key::Quit),
            b'h' => keys.push(Key::Pan(-PAN_STEP, 0.0)),
            b'l' => keys.push(Key::Pan(PAN_STEP, 0.0)),
            b'k' => keys.push(Key::Pan(0.0, -PAN_STEP)),
            b'j' => keys.push(Key::Pan(0.0, PAN_STEP)),
            b'+' | b'=' => keys.push(Key::Zoom(ZOOM_IN)),
            b'-' | b'_' => keys.push(Key::Zoom(ZOOM_OUT)),
            b'0' => keys.push(Key::Reset),
            b'n' | b' ' => keys.push(Key::Next),
            b'p' => keys.push(Key::Prev),
            b'/' => keys.push(Key::Search),
            b']' => keys.push(Key::Hit(1)),
            b'[' => keys.push(Key::Hit(-1)),
            b'}' => keys.push(Key::Heading(1)),
            b'{' => keys.push(Key::Heading(-1)),
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

    /// What `status_line` drew, with its own positioning prefix removed.
    fn drawn(text: &str) -> String {
        let mut out = Vec::new();
        status_line(&mut out, (80, 24), text).unwrap();
        let s = String::from_utf8(out).unwrap();
        s.strip_prefix("\x1b[24;1H\x1b[K").unwrap().to_owned()
    }

    #[test]
    fn a_search_takes_only_the_bytes_up_to_the_return() {
        // read_burst hands over whatever arrived together, so
        // a key pressed just after return rides along in the
        // same input and must not join the query
        let mut buf = Vec::new();
        let (outcome, taken) = type_into(&mut buf, b"ab\rc");
        assert!(matches!(outcome, Typed::Done));
        assert_eq!(taken, 3);
        assert_eq!(buf, b"ab");
    }

    #[test]
    fn a_cancelled_search_also_leaves_the_rest_of_the_burst() {
        let mut buf = Vec::new();
        let (outcome, taken) = type_into(&mut buf, b"ab\x1bq");
        assert!(matches!(outcome, Typed::Cancelled));
        assert_eq!(taken, 3);
    }

    #[test]
    fn a_search_accumulates_across_bursts() {
        let mut buf = Vec::new();
        let (outcome, taken) = type_into(&mut buf, b"ab");
        assert!(matches!(outcome, Typed::More));
        assert_eq!(taken, 2);
        type_into(&mut buf, b"cd");
        assert_eq!(buf, b"abcd");
    }

    #[test]
    fn backspace_removes_a_whole_character_and_stops_at_empty() {
        let mut buf = "aé".as_bytes().to_vec();
        assert_eq!(buf.len(), 3, "é is two bytes");

        type_into(&mut buf, b"\x7f");
        assert_eq!(String::from_utf8(buf.clone()).unwrap(), "a");

        // twice more than there is to delete
        type_into(&mut buf, b"\x7f\x7f");
        assert!(buf.is_empty());
    }

    /// The status line `draw` ended up writing.
    fn status_of(kind: source::Kind, name: &str) -> String {
        let mut out = Vec::new();
        let files = [PathBuf::from(name)];
        draw(
            &mut out,
            &Some(shown(image(400, 4000), kind)),
            &view(1.0, 200.0, 300.0),
            (100, 24),
            CELL,
            &files,
            0,
            &None,
            None,
        )
        .unwrap();
        let all = String::from_utf8_lossy(&out).into_owned();
        all.rsplit("\x1b[K").next().unwrap_or_default().to_owned()
    }

    #[test]
    fn a_document_is_offered_no_zoom_and_the_keys_it_has() {
        let s = status_of(source::Kind::Document, "notes.md");
        assert!(!s.contains("zoom"), "{s}");
        assert!(!s.contains('%'), "a document is always drawn 1:1: {s}");
        assert!(s.contains("/ find"), "{s}");
        assert!(s.contains("head"), "{s}");
    }

    #[test]
    fn a_picture_keeps_zoom_and_is_not_offered_the_document_keys() {
        let s = status_of(source::Kind::Image, "photo.png");
        assert!(s.contains("+- zoom"), "{s}");
        assert!(s.contains("100%"), "{s}");
        assert!(!s.contains("find"), "searching a picture means nothing: {s}");
    }

    #[test]
    fn a_filename_cannot_write_escape_sequences_to_the_terminal() {
        // a name holding ESC is legal on disk, and reaches
        // here from a glob rather than from anything typed
        assert_eq!(drawn("evil\x1b[2Jname.md"), "evil[2Jname.md");
        assert_eq!(drawn("bel\x07and\x00nul"), "belandnul");

        // C1 CSI is a single byte and opens a sequence too
        assert_eq!(drawn("eight\u{9b}bit"), "eightbit");
    }

    #[test]
    fn the_status_line_keeps_ordinary_text_and_the_width_limit() {
        assert_eq!(drawn("notes.md  [1/3]"), "notes.md  [1/3]");
        assert_eq!(drawn(&"x".repeat(200)).len(), 80);
    }

    #[test]
    fn a_refused_image_is_reported_rather_than_skipped() {
        let refusal = b"\x1b_Gi=31;EINVAL:image too large\x1b\\";
        let got = graphics_error(refusal).expect("refusal was not noticed");
        assert!(got.contains("EINVAL"), "{got}");
        assert!(got.contains("too large"), "{got}");
    }

    #[test]
    fn an_acknowledgement_is_not_an_error() {
        assert_eq!(graphics_error(b"\x1b_Gi=31,I=1;OK\x1b\\"), None);
        assert_eq!(graphics_error(b"hello"), None);

        // a half-arrived reply must not be read as a complaint either
        assert_eq!(graphics_error(b"\x1b_Gi=31;EINV"), None);
    }

    #[test]
    fn a_refusal_is_found_even_mixed_in_with_typing() {
        let mixed = b"j\x1b_Gi=7;ENOMEM\x1b\\k";
        assert!(graphics_error(mixed).is_some());
        assert_eq!(keys_from(mixed).len(), 2);
    }

    #[test]
    fn the_spinner_cycles_and_never_leaves_the_frame_list() {
        assert_eq!(spinner_frame(Duration::ZERO), SPINNER[0]);
        assert_eq!(spinner_frame(SPIN_TICK), SPINNER[1]);
        assert_eq!(spinner_frame(SPIN_TICK * SPINNER.len() as u32), SPINNER[0]);
        assert_eq!(spinner_frame(Duration::from_secs(3600)), SPINNER[0]);
    }

    #[test]
    fn nothing_is_said_about_a_wait_too_short_to_notice() {
        // long enough to sit out a decode that finishes in a
        // frame or two, short enough to announce a slow one
        assert!(PATIENCE >= SPIN_TICK);
        assert!(PATIENCE < Duration::from_millis(500));
    }

    fn shown(image: Framebuffer, kind: source::Kind) -> Shown {
        Shown {
            image,
            id: 1,
            kind,
            index: None,
        }
    }

    #[test]
    fn a_document_is_fitted_on_width_and_scrolls() {
        // an image is fitted on both axes and has nowhere to
        // pan at rest; a page fitted that way is unreadable
        let page = Framebuffer::new(900, 6000);
        let v = view(1.0, 450.0, 200.0);

        let doc = placement(&shown(page.clone(), source::Kind::Document), &v, (100, 30), CELL);
        assert!(
            doc.src_h < 6000,
            "a document showed its whole height at rest, so there is no scroll"
        );

        let pic = placement(&shown(page.clone(), source::Kind::Image), &v, (100, 30), CELL);
        assert_eq!(pic.src_h, 6000, "a picture should still be fitted whole");
        assert!(
            doc.src_h < pic.src_h,
            "the document should show less at once than the fitted picture"
        );
    }

    #[test]
    fn a_document_opens_at_its_first_line() {
        let page = Framebuffer::new(900, 6000);
        let s = shown(page, source::Kind::Document);
        let v = View::reset(&s, (100, 30), CELL);
        let p = placement(&s, &v, (100, 30), CELL);
        assert_eq!(p.src_y, 0, "document did not open at the top");
    }

    #[test]
    fn unzoomed_shows_the_whole_image() {
        let img = image(1000, 500);
        let p = placement(&shown(img.clone(), source::Kind::Image), &view(1.0, 500.0, 250.0), (80, 25), CELL);
        assert_eq!((p.src_x, p.src_y), (0, 0));
        assert_eq!((p.src_w, p.src_h), (1000, 500));
    }

    #[test]
    fn zooming_in_shrinks_the_source_rectangle() {
        let img = image(1000, 500);
        let wide = placement(&shown(img.clone(), source::Kind::Image), &view(1.0, 500.0, 250.0), (80, 25), CELL);
        let close = placement(&shown(img.clone(), source::Kind::Image), &view(2.0, 500.0, 250.0), (80, 25), CELL);
        assert!(close.src_w < wide.src_w && close.src_h < wide.src_h);
        // the axis that was already filling the viewport
        // halves exactly; the letterboxed axis shows less
        // than half, because zooming first removes the bars
        assert_eq!(close.src_w, wide.src_w / 2);
        assert!(close.src_h > wide.src_h / 2);
    }

    #[test]
    fn the_source_rectangle_keeps_the_display_box_aspect_ratio() {
        let img = image(1000, 500);
        for zoom in [1.0, 1.5, 2.0, 8.0] {
            let p = placement(&shown(img.clone(), source::Kind::Image), &view(zoom, 500.0, 250.0), (80, 25), CELL);
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
            let p = placement(&shown(img.clone(), source::Kind::Image), &view(zoom, 2000.0, 1500.0), (80, 25), CELL);
            assert!(p.cols <= 80, "cols {} at zoom {zoom}", p.cols);
            // one row is held back for the status line
            assert!(p.rows <= 24, "rows {} at zoom {zoom}", p.rows);
        }
    }

    #[test]
    fn panning_past_an_edge_clamps_inside_the_image() {
        let img = image(1000, 500);
        let p = placement(&shown(img.clone(), source::Kind::Image), &view(4.0, -9000.0, -9000.0), (80, 25), CELL);
        assert_eq!((p.src_x, p.src_y), (0, 0));

        let q = placement(&shown(img.clone(), source::Kind::Image), &view(4.0, 9000.0, 9000.0), (80, 25), CELL);
        assert_eq!(q.src_x + q.src_w, 1000);
        assert_eq!(q.src_y + q.src_h, 500);
    }

    #[test]
    fn a_small_image_is_not_enlarged_at_rest() {
        let img = image(40, 30);
        let p = placement(&shown(img.clone(), source::Kind::Image), &view(1.0, 20.0, 15.0), (80, 25), CELL);
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
        // the first half of an arrow key that ssh delivered
        // in two reads
        let (keys, used) = decode_keys(b"\x1b");
        assert!(keys.is_empty());
        assert_eq!(used, 0, "the escape has to survive for the next read");

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
