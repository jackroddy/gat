use image::RgbaImage;

pub type Framebuffer = RgbaImage;

/// Scale `fb` to exactly `w` by `h` pixels.
pub fn resize(fb: &Framebuffer, w: u32, h: u32) -> Framebuffer {
    if fb.width() == w && fb.height() == h {
        return fb.clone();
    }
    image::imageops::resize(fb, w, h, image::imageops::FilterType::Lanczos3)
}

/// Composite `fb` over an opaque background, discarding per-pixel alpha.
pub fn flatten_onto(fb: &mut Framebuffer, bg: [u8; 3]) {
    for px in fb.pixels_mut() {
        let a = px.0[3] as u32;
        if a == 255 {
            continue;
        }
        for (c, &b) in px.0[..3].iter_mut().zip(bg.iter()) {
            *c = ((*c as u32 * a + b as u32 * (255 - a)) / 255) as u8;
        }
        px.0[3] = 255;
    }
}
