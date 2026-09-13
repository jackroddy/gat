use pdfium_render::prelude::*;

use crate::framebuffer::Framebuffer;
use crate::source::{Error, Hints};

pub fn load(bytes: &[u8], hints: Hints) -> Result<Framebuffer, Error> {
    let bindings = Pdfium::bind_to_system_library().map_err(|_| {
        Error::Decode(
            "no pdfium library found; put libpdfium on the loader path, or \
             rebuild without --features pdf-pdfium to use the built-in renderer"
                .into(),
        )
    })?;
    let pdfium = Pdfium::new(bindings);

    let doc = pdfium
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| Error::Decode(e.to_string()))?;
    let page = doc
        .pages()
        .first()
        .map_err(|e| Error::Decode(e.to_string()))?;

    // vector input, so rasterize at display size; the bounds are given both
    // ways because the page aspect ratio decides which one binds
    let config = PdfRenderConfig::new()
        .set_target_width(hints.max_w as i32)
        .set_maximum_height(hints.max_h as i32);

    let image = page
        .render_with_config(&config)
        .map_err(|e| Error::Decode(e.to_string()))?
        .as_image()
        .map_err(|e| Error::Decode(e.to_string()))?
        .into_rgba8();
    Ok(image)
}
