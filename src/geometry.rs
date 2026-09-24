/// The pixel dimensions of one terminal character cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellSize {
    // fractional: a cell drawn coarser than the terminal keeps
    // its aspect only if its width may fall between pixels
    pub w: f64,
    pub h: f64,
}

impl CellSize {
    /// The guess used when the terminal reports no pixel size.
    //
    // 9x18 is timg's fallback: close to the common terminal
    // default, and it keeps the 1:2 cell ratio the fit math
    // assumes
    pub const FALLBACK: CellSize = CellSize { w: 9.0, h: 18.0 };

    /// This cell drawn at least `by` times coarser, and how many terminal
    /// pixels one pixel of it covers.
    #[cfg_attr(not(feature = "markdown"), allow(dead_code))]
    pub fn coarser(self, by: f64) -> (CellSize, f64) {
        // a whole number of pixels tall, so a page's rows start
        // on pixel boundaries and bands can be cut between them.
        // rounded down, so the cell shrinks by at least `by`
        // and a page stays inside its cap. the width follows at
        // the same ratio
        let h = (self.h / by.max(1.0)).floor().max(1.0);
        let per = self.h / h;
        (CellSize { w: self.w / per, h }, per)
    }
}

/// The space available to an image, and the scaling options the fit applies.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub cols: u32,
    pub rows: u32,
    pub cell: CellSize,
    pub upscale: bool,
    pub fill_width: bool,
    pub fill_height: bool,
}

/// Compute how many times too large `w` by `h` is to fit within `cap` on
/// both sides: 1 where it already fits.
pub fn over_cap(w: f64, h: f64, cap: u32) -> f64 {
    let cap = f64::from(cap.max(1));
    (w / cap).max(h / cap).max(1.0)
}

/// Compute the pixel size to render at: the largest scaling of `img_w` by
/// `img_h` that respects `budget` and preserves the aspect ratio.
pub fn fit(img_w: u32, img_h: u32, budget: &Budget) -> (u32, u32) {
    let avail_w = (f64::from(budget.cols) * budget.cell.w).max(1.0);
    let avail_h = (f64::from(budget.rows) * budget.cell.h).max(1.0);
    let (iw, ih) = (img_w.max(1) as f64, img_h.max(1) as f64);

    let w_frac = avail_w / iw;
    let h_frac = avail_h / ih;

    if !budget.upscale
        && !budget.fill_width
        && !budget.fill_height
        && w_frac >= 1.0
        && h_frac >= 1.0
    {
        return (img_w, img_h);
    }

    // the kitty protocol addresses real pixels, so there is
    // no cell aspect ratio to correct for
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
            cell: CellSize { w: 10.0, h: 20.0 },
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
    fn a_coarser_cell_keeps_its_aspect_and_whole_rows() {
        for (w, h) in [(9.0, 18.0), (10.0, 21.0), (7.0, 15.0)] {
            for by in [1.3, 2.0, 3.7] {
                let (small, per) = CellSize { w, h }.coarser(by);
                assert_eq!(small.h, small.h.round(), "{w}x{h} by {by} gave {small:?}");
                assert!((small.w / small.h - w / h).abs() < 1e-9, "{w}x{h} by {by} gave {small:?}");
                assert!(per >= by, "{w}x{h} by {by} shrank only {per}");
            }
        }
        assert_eq!(CellSize::FALLBACK.coarser(1.0), (CellSize::FALLBACK, 1.0));
    }

    #[test]
    fn the_cap_shrinks_by_the_longer_side_and_never_grows() {
        assert_eq!(over_cap(800.0, 600.0, 1024), 1.0);
        assert_eq!(over_cap(2048.0, 600.0, 1024), 2.0);
        assert_eq!(over_cap(600.0, 4096.0, 1024), 4.0);
    }

    #[test]
    fn aspect_ratio_survives_the_round_trip() {
        let (w, h) = fit(1920, 1080, &budget(80, 24));
        let err = (w as f64 / h as f64) - (1920.0 / 1080.0);
        assert!(err.abs() < 0.01, "got {w}x{h}");
    }
}
