use std::io::{self, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use flate2::Compression;
use flate2::write::ZlibEncoder;

use crate::framebuffer::Framebuffer;

// the protocol caps a chunk's base64 payload at 4096 characters; 3072 raw
// bytes encode to exactly that, and being a multiple of 3 it also keeps
// padding out of every chunk but the last
const RAW_CHUNK: usize = 4096 / 4 * 3;

/// The placement slot every interactive redraw reuses.
//
// an image id and placement id together name a placement, and re-sending one
// with both the same replaces it in place. the spec calls this out as the way
// to move or resize a placement without flicker, which is why the viewer
// never deletes before drawing
const PLACEMENT: u32 = 1;

/// Where to take pixels from a stored image, and how large to draw them.
pub struct Placement {
    /// Left edge of the source rectangle, in image pixels.
    pub src_x: u32,

    /// Top edge of the source rectangle, in image pixels.
    pub src_y: u32,

    /// Width of the source rectangle, in image pixels.
    pub src_w: u32,

    /// Height of the source rectangle, in image pixels.
    pub src_h: u32,

    /// Where the image sits relative to the terminal's text.
    //
    // negative puts it under the text layer, which is what
    // lets the viewer write over the page without touching a
    // pixel of it
    pub z: i32,

    /// Width of the destination box, in terminal cells.
    pub cols: u32,

    /// Height of the destination box, in terminal cells.
    pub rows: u32,
}

/// Write `fb` as a Kitty graphics command that displays it at the cursor,
/// scaled into `cells` columns by rows where given.
pub fn write(
    out: &mut impl Write,
    fb: &Framebuffer,
    id: u32,
    cells: Option<(u32, u32)>,
) -> io::Result<()> {
    send(out, fb, id, true, cells)
}

/// A framebuffer compressed and ready to be written out.
pub struct Encoded {
    /// Width of the image, in pixels.
    pub w: u32,

    /// Height of the image, in pixels.
    pub h: u32,

    // zlib, compressed on a worker thread
    payload: Vec<u8>,
}

/// Compress `fb` into the payload a transmission carries.
pub fn encode(fb: &Framebuffer) -> io::Result<Encoded> {
    let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
    z.write_all(fb.as_raw())?;
    Ok(Encoded {
        w: fb.width(),
        h: fb.height(),
        payload: z.finish()?,
    })
}

fn send(
    out: &mut impl Write,
    fb: &Framebuffer,
    id: u32,
    display: bool,
    cells: Option<(u32, u32)>,
) -> io::Result<()> {
    // nothing reads the tty after the one-shot path writes,
    // so an unread reply would land at the user's shell
    emit(out, &encode(fb)?, id, display, cells, Quiet::Fully)
}

/// How much the terminal reports back about a transmission.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Quiet {
    /// q=2: no reply at all.
    //
    // this suppresses failures as well as acknowledgements,
    // so a refused image leaves an empty screen and no reason
    // for it; only use it where nothing will read the reply
    Fully,

    /// q=1: failures only.
    ErrorsOnly,
}

impl Quiet {
    fn code(self) -> u8 {
        match self {
            Quiet::Fully => 2,
            Quiet::ErrorsOnly => 1,
        }
    }
}

/// Write an already-compressed image as the protocol's chunked escapes.
pub fn emit(
    out: &mut impl Write,
    img: &Encoded,
    id: u32,
    display: bool,
    cells: Option<(u32, u32)>,
    quiet: Quiet,
) -> io::Result<()> {
    let q = quiet.code();
    let Encoded { w, h, payload } = img;

    // C=1 stops the terminal advancing the cursor itself. its
    // default, C=0, moves right by the placement's columns and down
    // by its rows, and the spec leaves the result undefined once
    // that runs past the edge of the screen. the caller places the
    // cursor instead, which it can do deterministically
    let action = match (display, cells) {
        (false, _) => "t".to_string(),
        (true, None) => "T,C=1".to_string(),

        // both given, the terminal letterboxes rather than
        // stretching, so rounding up to whole cells cannot
        // distort the picture
        (true, Some((c, r))) => format!("T,C=1,c={c},r={r}"),
    };

    let mut chunks = payload.chunks(RAW_CHUNK).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            write!(
                out,
                "\x1b_Ga={action},i={id},q={q},f=32,o=z,s={w},v={h},m={more};"
            )?;
            first = false;
        } else {
            write!(out, "\x1b_Gq={q},m={more};")?;
        }
        out.write_all(B64.encode(chunk).as_bytes())?;
        out.write_all(b"\x1b\\")?;
    }
    Ok(())
}

/// Draw part of the image stored under `id` at the cursor, scaled into a box
/// of `p.cols` by `p.rows` cells.
pub fn place(out: &mut impl Write, id: u32, p: &Placement) -> io::Result<()> {
    write!(
        out,
        "\x1b_Ga=p,i={id},p={PLACEMENT},q=2,C=1,x={x},y={y},w={w},h={h},c={c},r={r},z={z};\x1b\\",
        x = p.src_x,
        y = p.src_y,
        w = p.src_w,
        h = p.src_h,
        c = p.cols,
        r = p.rows,
        z = p.z,
    )
}

/// The block of image ids the one-shot renderer draws from.
//
// high and recognisable in a trace, and far from the low ids
// a client that has not thought about ids will pick: the
// block is deleted wholesale, so anything else inside it goes
// too
const PRINT_BASE: u32 = 0xC0DE_0000;

/// How many ids that block holds.
//
// one run's images all have to fit, and a run is one id per
// picture plus at most five per document, since a page is
// capped at MAX_PIXELS and cut at CHUNK_PIXELS. so this is a
// file count, and a terminal runs out of memory for the
// pixels long before a million files run it out of ids
const PRINT_SPAN: u32 = 1 << 20;

// id 0 means unspecified to the terminal, and a block running
// off the end of a u32 would wrap into it
const _: () = assert!(PRINT_BASE > 0);
const _: () = assert!(PRINT_BASE.checked_add(PRINT_SPAN).is_some());

/// The id for the one-shot renderer's `n`th image of a run.
pub fn print_id(n: u32) -> u32 {
    PRINT_BASE + n % PRINT_SPAN
}

/// Drop every image the one-shot renderer left behind, freeing the pixels of
/// any the scrollback no longer refers to.
//
// d=R is the range form, and the capital frees the data
// rather than only dropping the placements
pub fn forget_prints(out: &mut impl Write) -> io::Result<()> {
    let last = PRINT_BASE + PRINT_SPAN - 1;
    write!(out, "\x1b_Ga=d,d=R,x={PRINT_BASE},y={last},q=2;\x1b\\")
}

/// Drop the image stored under `id` and free its pixel data.
pub fn forget(out: &mut impl Write, id: u32) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")
}

/// A fresh image id, distinct from those any other run of this program is
/// likely to have used.
pub fn next_id() -> u32 {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static BASE: OnceLock<u32> = OnceLock::new();
    static N: AtomicU32 = AtomicU32::new(0);

    // kitty stores a transmitted image under its id and
    // re-renders every placement referring to that id when the
    // data behind it changes, so a counter restarting at 1 each
    // run repaints the images earlier runs left on screen, at
    // the new image's size
    //
    // seconds alone collide between two runs inside the same
    // second and the pid alone repeats within a boot, so mix
    // both
    //
    // TODO: where do the rotation distances 11 and 19 come from?
    let base = *BASE.get_or_init(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        (now.as_secs() as u32).rotate_left(11)
            ^ now.subsec_nanos()
            ^ std::process::id().rotate_left(19)
    });

    // id 0 means "unspecified" to the terminal
    match base.wrapping_add(N.fetch_add(1, Ordering::Relaxed)) {
        0 => 1,
        id => id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload_of(bytes: &[u8]) -> Vec<u8> {
        let mut b64 = Vec::new();
        let mut rest = bytes;
        while let Some(start) = rest.windows(3).position(|w| w == b"\x1b_G") {
            let body = &rest[start + 3..];
            let semi = body.iter().position(|&c| c == b';').unwrap();
            let end = body.windows(2).position(|w| w == b"\x1b\\").unwrap();
            b64.extend_from_slice(&body[semi + 1..end]);
            rest = &body[end + 2..];
        }
        let z = B64.decode(&b64).unwrap();
        let mut out = Vec::new();
        std::io::copy(&mut flate2::read::ZlibDecoder::new(&z[..]), &mut out).unwrap();
        out
    }

    #[test]
    fn payload_round_trips_to_the_original_pixels() {
        // 40x40 is large enough to need several chunks once
        // the gradient makes the payload incompressible
        let fb = Framebuffer::from_fn(40, 40, |x, y| {
            image::Rgba([x as u8, y as u8, (x * y) as u8, 255])
        });
        let mut out = Vec::new();
        write(&mut out, &fb, 7, None).unwrap();
        assert_eq!(payload_of(&out), *fb.as_raw());
    }

    #[test]
    fn a_print_id_stays_inside_the_block_it_is_deleted_with() {
        // the block is deleted wholesale at the start of a
        // run, so an id outside it would survive and leak
        for n in [0, 1, 999, PRINT_SPAN - 1, PRINT_SPAN, PRINT_SPAN + 7, u32::MAX] {
            let id = print_id(n);
            assert!(
                (PRINT_BASE..PRINT_BASE + PRINT_SPAN).contains(&id),
                "image {n} took id {id:#x}, outside the block"
            );
        }
    }

    #[test]
    fn the_block_delete_names_the_whole_block() {
        let mut out = Vec::new();
        forget_prints(&mut out).unwrap();
        let lo = print_id(0);
        let hi = print_id(PRINT_SPAN - 1);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("\x1b_Ga=d,d=R,x={lo},y={hi},q=2;\x1b\\")
        );
    }

    #[test]
    fn ids_are_distinct_and_never_zero() {
        let ids: Vec<u32> = (0..64).map(|_| next_id()).collect();
        assert!(ids.iter().all(|&i| i != 0));
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "ids repeated within one run");
    }

    #[test]
    fn header_declares_the_image_size_and_id() {
        let fb = Framebuffer::new(3, 5);
        let mut out = Vec::new();
        write(&mut out, &fb, 42, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.starts_with("\x1b_Ga=T,C=1,i=42,q=2,f=32,o=z,s=3,v=5,m=0;"),
            "{}",
            &text[..44]
        );
    }

    #[test]
    fn a_cell_box_rides_on_the_display_header() {
        let fb = Framebuffer::new(3, 5);
        let mut out = Vec::new();
        write(&mut out, &fb, 42, Some((6, 2))).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.starts_with("\x1b_Ga=T,C=1,c=6,r=2,i=42,"), "{text:?}");
    }

    #[test]
    fn every_chunk_but_the_last_is_flagged_and_full() {
        let fb = Framebuffer::from_fn(200, 200, |x, y| {
            image::Rgba([(x ^ y) as u8, (x * 7) as u8, (y * 13) as u8, 255])
        });
        let mut out = Vec::new();
        write(&mut out, &fb, 1, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        let flags: Vec<&str> = text.match_indices("m=").map(|(i, _)| &text[i + 2..i + 3]).collect();
        assert!(flags.len() > 1, "test image did not chunk");
        assert!(flags[..flags.len() - 1].iter().all(|f| *f == "1"));
        assert_eq!(flags[flags.len() - 1], "0");
    }
}
