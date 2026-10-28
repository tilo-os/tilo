//! A window held as the NV12 frame its encoder takes, kept current by
//! converting only what changed.

/// A rectangle of a frame in its pixels: x, y, width, height.
pub type Region = (u32, u32, u32, u32);

/// BT.709 limited-range luma and chroma of an RGB pixel, in the integer
/// form encoders use.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    reason = "8-bit samples times coefficients below 256 stay far inside i32, and each value is \
              clamped into its 8-bit range before the cast"
)]
fn yuv([red, green, blue]: [u8; 3]) -> (u8, u8, u8) {
    let (red, green, blue) = (i32::from(red), i32::from(green), i32::from(blue));
    let luma = (47 * red + 157 * green + 16 * blue + 4096 + 128) >> 8;
    let cb = (-26 * red - 86 * green + 112 * blue + 32768 + 128) >> 8;
    let cr = (112 * red - 102 * green - 10 * blue + 32768 + 128) >> 8;
    (
        luma.clamp(16, 235) as u8,
        cb.clamp(16, 240) as u8,
        cr.clamp(16, 240) as u8,
    )
}

/// An even-sized NV12 frame converted from RGBA frames `stride` pixels wide.
pub struct Nv12Frame {
    width: u32,
    height: u32,
    stride: u32,
    bytes: Vec<u8>,
    /// Nothing has been converted yet, so the next frame is converted whole.
    blank: bool,
}

impl Nv12Frame {
    /// A frame of the top-left `width`×`height` (both even) of RGBA frames
    /// `stride` pixels wide.
    pub fn new(width: u32, height: u32, stride: u32) -> Self {
        let len = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(3)
            / 2;
        Self {
            width,
            height,
            stride,
            bytes: vec![0; len],
            blank: true,
        }
    }

    /// The NV12 bytes: the Y plane, then interleaved Cb and Cr.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Bring the frame up to `rgba`, which differs from the frame last
    /// converted only within `changed`; `None` converts all of it.
    pub fn update(&mut self, rgba: &[u8], changed: Option<&[Region]>) {
        let whole = [(0, 0, self.width, self.height)];
        let regions = match changed {
            Some(regions) if !self.blank => regions,
            _ => &whole[..],
        };
        for region in regions {
            self.convert(rgba, *region);
        }
        self.blank = false;
    }

    /// Convert `region` of `rgba`, widened to whole 2×2 chroma blocks and
    /// clipped to the frame.
    fn convert(&mut self, rgba: &[u8], (left, top, width, height): Region) {
        let x0 = (left & !1).min(self.width);
        let y0 = (top & !1).min(self.height);
        let x1 = left.saturating_add(width).saturating_add(1).min(self.width) & !1;
        let y1 = top
            .saturating_add(height)
            .saturating_add(1)
            .min(self.height)
            & !1;
        let width = self.width as usize;
        let stride = (self.stride as usize).saturating_mul(4);
        let (luma, chroma) = self
            .bytes
            .split_at_mut(width.saturating_mul(self.height as usize));
        for row in (y0..y1).step_by(2) {
            let row = row as usize;
            let upper = row.saturating_mul(stride);
            let lower = upper.saturating_add(stride);
            for column in (x0..x1).step_by(2) {
                let column = column as usize;
                let (mut cb, mut cr) = (0_u32, 0_u32);
                for (line, source) in [(row, upper), (row.saturating_add(1), lower)] {
                    for dx in 0..2 {
                        let at = source.saturating_add(column.saturating_add(dx).saturating_mul(4));
                        let Some(&[red, green, blue, _]) = rgba.get(at..at.saturating_add(4))
                        else {
                            continue;
                        };
                        let (luma_value, blue_difference, red_difference) = yuv([red, green, blue]);
                        if let Some(sample) = luma.get_mut(
                            line.saturating_mul(width)
                                .saturating_add(column)
                                .saturating_add(dx),
                        ) {
                            *sample = luma_value;
                        }
                        cb = cb.saturating_add(u32::from(blue_difference));
                        cr = cr.saturating_add(u32::from(red_difference));
                    }
                }
                let at = (row / 2).saturating_mul(width).saturating_add(column);
                if let Some([cb_out, cr_out]) = chroma.get_mut(at..at.saturating_add(2)) {
                    *cb_out = u8::try_from(cb.saturating_add(2) / 4).unwrap_or(u8::MAX);
                    *cr_out = u8::try_from(cr.saturating_add(2) / 4).unwrap_or(u8::MAX);
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test frames are a few pixels, indexed where the test put them"
)]
mod tests {
    use super::*;

    fn rgba(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        pixel.repeat((width * height) as usize)
    }

    #[test]
    fn white_black_and_grey_land_on_their_limited_range_values() {
        for (pixel, expected) in [
            ([255, 255, 255, 255], (235, 128, 128)),
            ([0, 0, 0, 255], (16, 128, 128)),
            ([128, 128, 128, 255], (126, 128, 128)),
        ] {
            let mut frame = Nv12Frame::new(4, 2, 4);
            frame.update(&rgba(4, 2, pixel), None);
            assert_eq!(frame.bytes()[0], expected.0);
            assert_eq!(frame.bytes()[8], expected.1);
            assert_eq!(frame.bytes()[9], expected.2);
        }
    }

    #[test]
    fn only_the_changed_region_is_converted_after_the_first_frame() {
        let mut frame = Nv12Frame::new(8, 4, 8);
        frame.update(&rgba(8, 4, [0, 0, 0, 255]), Some(&[(0, 0, 1, 1)]));
        assert!(
            frame.bytes()[..32].iter().all(|y| *y == 16),
            "the first frame is converted whole whatever it says changed"
        );

        let white = rgba(8, 4, [255, 255, 255, 255]);
        frame.update(&white, Some(&[(5, 1, 1, 1)]));
        let changed = |x: usize, y: usize| frame.bytes()[y * 8 + x] == 235;
        assert!(changed(4, 0) && changed(5, 1), "its 2×2 block is converted");
        assert!(!changed(0, 0) && !changed(6, 2), "nothing else is");

        frame.update(&white, None);
        assert!(frame.bytes()[..32].iter().all(|y| *y == 235));
    }

    #[test]
    fn an_odd_sized_source_converts_its_even_part() {
        let mut frame = Nv12Frame::new(2, 2, 3);
        let mut source = rgba(3, 3, [0, 0, 0, 255]);
        source[..4].copy_from_slice(&[255, 255, 255, 255]);
        frame.update(&source, None);
        assert_eq!(&frame.bytes()[..4], &[235, 16, 16, 16]);
    }
}
