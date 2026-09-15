use resvg::tiny_skia;
use resvg::usvg;

use crate::framebuffer::Framebuffer;
use crate::source::{Error, Hints};

pub fn load(bytes: &[u8], hints: Hints) -> Result<Framebuffer, Error> {
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default())
        .map_err(|e| Error::Decode(e.to_string()))?;

    let size = tree.size();

    // SVG has no pixel size of its own, so rasterize straight at the size
    // it will be displayed at rather than scaling a bitmap afterwards
    let scale = (hints.max_w as f32 / size.width())
        .min(hints.max_h as f32 / size.height())
        // a zero-size tree gives scale 0 and a 0x0 pixmap
        .max(f32::MIN_POSITIVE);

    rasterize(&tree, scale)
}

/// Draw `tree` at `scale` into a straight-alpha framebuffer.
pub fn rasterize(tree: &usvg::Tree, scale: f32) -> Result<Framebuffer, Error> {
    // the scale is the caller's: markdown lays itself out
    // at the display width already, where an svg gets the
    // contain-fit above
    let size = tree.size();
    let (w, h) = (
        (size.width() * scale).round().max(1.0) as u32,
        (size.height() * scale).round().max(1.0) as u32,
    );

    let mut pixmap = tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| Error::Decode(format!("cannot allocate {w}x{h} pixmap")))?;
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    // tiny-skia stores premultiplied alpha; the renderer and the compositor
    // both expect straight alpha
    let mut raw = pixmap.take();
    for px in raw.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                // c_straight = round(c_pre * 255 / a)
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }

    Framebuffer::from_raw(w, h, raw).ok_or_else(|| Error::Decode("short pixmap".into()))
}
