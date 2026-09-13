/// The pixel dimensions of one terminal character cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellSize {
    pub w: u32,
    pub h: u32,
}

impl CellSize {
    /// The guess used when the terminal reports no pixel size.
    //
    // 9x18 is timg's fallback: close to the common terminal
    // default, and it keeps the 1:2 cell ratio the fit math
    // assumes
    pub const FALLBACK: CellSize = CellSize { w: 9, h: 18 };
}

/// How much room an image may occupy, and what liberties the fit may take.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub cols: u32,
    pub rows: u32,
    pub cell: CellSize,
    pub upscale: bool,
    pub fill_width: bool,
    pub fill_height: bool,
}

/// Compute the pixel size to render at: the largest scaling of `img_w` by
/// `img_h` that respects `budget` and preserves the aspect ratio.
pub fn fit(img_w: u32, img_h: u32, budget: &Budget) -> (u32, u32) {
    let avail_w = (budget.cols * budget.cell.w).max(1) as f64;
    let avail_h = (budget.rows * budget.cell.h).max(1) as f64;
    let (iw, ih) = (img_w.max(1) as f64, img_h.max(1) as f64);

    let w_frac = avail_w / iw;
    let h_frac = avail_h / ih;

    // the pixel-direct protocols address real pixels, so unlike the
    // block-drawing modes there is no cell aspect ratio to correct for

    if !budget.upscale
        && !budget.fill_width
        && !budget.fill_height
        && w_frac >= 1.0
        && h_frac >= 1.0
    {
        return (img_w, img_h);
    }

    let frac = match (budget.fill_width, budget.fill_height) {
        (true, true) => w_frac.max(h_frac),
        (true, false) => w_frac,
        (false, true) => h_frac,
        (false, false) => w_frac.min(h_frac),
    };

    (
        ((iw * frac).round() as u32).max(1),
        ((ih * frac).round() as u32).max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(cols: u32, rows: u32) -> Budget {
        Budget {
            cols,
            rows,
            cell: CellSize { w: 10, h: 20 },
            upscale: false,
            fill_width: false,
            fill_height: false,
        }
    }

    #[test]
    fn small_image_is_left_alone() {
        assert_eq!(fit(50, 40, &budget(80, 24)), (50, 40));
    }

    #[test]
    fn large_image_is_letterboxed_by_the_tighter_axis() {
        // 800x480 available; the image is 4x too wide and 2x too tall,
        // so width limits first
        assert_eq!(fit(3200, 960, &budget(80, 24)), (800, 240));
    }

    #[test]
    fn upscale_grows_a_small_image_to_the_tighter_axis() {
        let mut b = budget(80, 24);
        b.upscale = true;
        assert_eq!(fit(80, 48, &b), (800, 480));
    }

    #[test]
    fn fill_width_lets_height_overflow() {
        let mut b = budget(80, 24);
        b.fill_width = true;
        assert_eq!(fit(400, 400, &b), (800, 800));
    }

    #[test]
    fn aspect_ratio_survives_the_round_trip() {
        let (w, h) = fit(1920, 1080, &budget(80, 24));
        let err = (w as f64 / h as f64) - (1920.0 / 1080.0);
        assert!(err.abs() < 0.01, "got {w}x{h}");
    }
}
