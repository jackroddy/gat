use std::sync::Arc;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::font::FontQuery;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings};

use crate::framebuffer::Framebuffer;
use crate::source::{Error, Hints};

pub fn load(bytes: &[u8], hints: Hints) -> Result<Framebuffer, Error> {
    let pdf = Pdf::new(bytes.to_vec())
        .map_err(|e| Error::Decode(format!("cannot read PDF: {e:?}")))?;
    let pages = pdf.pages();
    let page = pages
        .iter()
        .next()
        .ok_or_else(|| Error::Decode("PDF has no pages".into()))?;

    let (pw, ph) = page.render_dimensions();
    let scale = (hints.max_w as f32 / pw)
        .min(hints.max_h as f32 / ph)
        // a zero-size page gives scale 0 and a 0x0 pixmap
        .max(f32::MIN_POSITIVE);

    let settings = InterpreterSettings {
        // without a resolver, a page that names a font instead of embedding
        // one renders with its text missing. the 14 standard fonts are
        // compiled in via hayro's embed-fonts feature
        font_resolver: Arc::new(|query| match query {
            FontQuery::Standard(font) => Some(font.get_font_data()),
            FontQuery::Fallback(_) => None,
        }),
        ..Default::default()
    };

    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &settings,
        &RenderSettings {
            x_scale: scale,
            y_scale: scale,

            // a page composites over white rather than over
            // the --background color used for images with
            // real alpha
            bg_color: WHITE,
            ..Default::default()
        },
    );

    let (w, h) = (pixmap.width() as u32, pixmap.height() as u32);
    let mut raw = Vec::with_capacity((w * h * 4) as usize);
    for px in pixmap.take_unpremultiplied() {
        raw.extend_from_slice(&[px.r, px.g, px.b, px.a]);
    }
    Framebuffer::from_raw(w, h, raw).ok_or_else(|| Error::Decode("short pixmap".into()))
}

