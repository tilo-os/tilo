//! Whether a client declared its whole surface opaque.
//!
//! `wl_surface.set_opaque_region` names the pixels a client promises are
//! opaque. When that region covers the whole surface, the alpha channel of
//! the surface's buffer is padding, exactly like an `X` format's: the buffer
//! is drawn ignoring it, and a fullscreen one can be scanned out as its `X`
//! twin (`backend::drm::client_scanout`). Browsers and most GPU toolkits
//! allocate `ARGB8888` swapchains even when every pixel is opaque and say so
//! here; without it they would never be scanned out.
//!
//! Every check errs toward "not opaque": a surface whose size is unsure is
//! measured at its largest, and a region that misses one pixel of it, or is
//! too fragmented to judge cheaply, does not count.

use smithay::reexports::wayland_server::protocol::wl_output::Transform;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::compositor::{RectangleKind, RegionAttributes};
use smithay::wayland::viewporter::ViewportCachedState;

/// A region with more rectangles than this is not judged: no toolkit
/// describes an opaque surface with that many, and the check stays bounded.
const MAX_RECTS: usize = 64;

/// The surface pieces still undecided while a region is walked; past this
/// many the region is too fragmented to judge and does not count.
const MAX_PIECES: usize = 256;

/// A rectangle as half-open edges, wide enough that no edge overflows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl Span {
    fn of(rect: &Rectangle<i32, Logical>) -> Self {
        let x0 = i64::from(rect.loc.x);
        let y0 = i64::from(rect.loc.y);
        Self {
            x0,
            y0,
            x1: x0.saturating_add(i64::from(rect.size.w)),
            y1: y0.saturating_add(i64::from(rect.size.h)),
        }
    }

    fn is_empty(self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    fn intersection(self, other: Self) -> Self {
        Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    fn meets(self, other: Self) -> bool {
        !self.intersection(other).is_empty()
    }

    /// Push the parts of `self` outside `cut`: up to four pieces, none
    /// overlapping.
    fn push_outside(self, cut: Self, out: &mut Vec<Self>) {
        let inner = self.intersection(cut);
        if inner.is_empty() {
            out.push(self);
            return;
        }
        if self.y0 < inner.y0 {
            out.push(Self {
                y1: inner.y0,
                ..self
            });
        }
        if inner.y1 < self.y1 {
            out.push(Self {
                y0: inner.y1,
                ..self
            });
        }
        if self.x0 < inner.x0 {
            out.push(Self {
                x0: self.x0,
                x1: inner.x0,
                ..inner
            });
        }
        if inner.x1 < self.x1 {
            out.push(Self {
                x0: inner.x1,
                x1: self.x1,
                ..inner
            });
        }
    }
}

/// Whether `region`, in surface-local coordinates, covers every pixel of a
/// surface `extent` (width, height) in size. No region covers nothing.
///
/// A region is its rectangles added and subtracted in order, so a pixel is in
/// it when the last rectangle holding it was added. Walking the rectangles
/// from the last, the surface is covered when added rectangles take all of
/// it before any subtracted one meets what is left.
pub fn covers(region: Option<&RegionAttributes>, extent: (i32, i32)) -> bool {
    let Some(region) = region else {
        return false;
    };
    let surface = Span {
        x0: 0,
        y0: 0,
        x1: i64::from(extent.0),
        y1: i64::from(extent.1),
    };
    if surface.is_empty() || region.rects.len() > MAX_RECTS {
        return false;
    }
    let mut undecided = vec![surface];
    let mut rest = Vec::new();
    for (kind, rect) in region.rects.iter().rev() {
        let rect = Span::of(rect);
        match kind {
            RectangleKind::Subtract => {
                if undecided.iter().any(|piece| piece.meets(rect)) {
                    return false;
                }
            }
            RectangleKind::Add => {
                rest.clear();
                for piece in &undecided {
                    piece.push_outside(rect, &mut rest);
                }
                std::mem::swap(&mut undecided, &mut rest);
                if undecided.is_empty() {
                    return true;
                }
                if undecided.len() > MAX_PIECES {
                    return false;
                }
            }
        }
    }
    undecided.is_empty()
}

/// The size, in surface-local coordinates, of a surface showing a `buffer`
/// (width, height) in pixels at `scale` under `transform`, cropped and
/// scaled by `viewport`. Never smaller than the real size: each candidate —
/// the buffer at its scale, the viewport's destination, its source — is
/// rounded up and the largest wins, so [`covers`] can only err toward
/// compositing.
pub fn surface_extent(
    buffer: (u32, u32),
    scale: i32,
    transform: Transform,
    viewport: &ViewportCachedState,
) -> (i32, i32) {
    let scale = u32::try_from(scale)
        .ok()
        .filter(|scale| *scale > 0)
        .unwrap_or(1);
    let scaled = (buffer.0.div_ceil(scale), buffer.1.div_ceil(scale));
    let (width, height) = if matches!(
        transform,
        Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270
    ) {
        (scaled.1, scaled.0)
    } else {
        scaled
    };
    let mut extent = (
        i32::try_from(width).unwrap_or(i32::MAX),
        i32::try_from(height).unwrap_or(i32::MAX),
    );
    if let Some(destination) = viewport.dst {
        extent = (extent.0.max(destination.w), extent.1.max(destination.h));
    }
    if let Some(source) = viewport.src {
        extent = (
            extent.0.max(ceil_to_i32(source.size.w)),
            extent.1.max(ceil_to_i32(source.size.h)),
        );
    }
    extent
}

/// `value` rounded up, saturated to `i32`; a value that is not a number is
/// as large as it gets, so nothing is judged covered by it.
fn ceil_to_i32(value: f64) -> i32 {
    if value.is_nan() {
        return i32::MAX;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "float-to-int `as` saturates; a viewport larger than i32 only makes the surface harder to cover"
    )]
    let rounded = value.ceil() as i32;
    rounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::{Point, Size};

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    fn region(rects: &[(RectangleKind, Rectangle<i32, Logical>)]) -> RegionAttributes {
        RegionAttributes {
            rects: rects.to_vec(),
        }
    }

    const ADD: RectangleKind = RectangleKind::Add;
    const SUB: RectangleKind = RectangleKind::Subtract;
    const SURFACE: (i32, i32) = (1920, 1080);

    #[test]
    fn only_a_region_holding_every_pixel_covers_the_surface() {
        // Phase `the_whole_surface`: what Chromium and GTK send fullscreen.
        assert!(covers(
            Some(&region(&[(ADD, rect(0, 0, 1920, 1080))])),
            SURFACE
        ));
        assert!(
            covers(Some(&region(&[(ADD, rect(-10, -10, 4000, 4000))])), SURFACE),
            "a region larger than the surface covers it"
        );
        // Phase `no_region`: the default is "potentially transparent".
        assert!(!covers(None, SURFACE));
        assert!(!covers(Some(&region(&[])), SURFACE));
        // Phase `pieces_that_add_up`.
        assert!(covers(
            Some(&region(&[
                (ADD, rect(0, 0, 960, 1080)),
                (ADD, rect(960, 0, 960, 1080)),
            ])),
            SURFACE
        ));
        assert!(
            covers(
                Some(&region(&[
                    (ADD, rect(0, 0, 1920, 400)),
                    (ADD, rect(0, 680, 1920, 400)),
                    (ADD, rect(0, 400, 500, 280)),
                    (ADD, rect(1420, 400, 500, 280)),
                    (ADD, rect(500, 400, 920, 280)),
                ])),
                SURFACE
            ),
            "a centre added last leaves a frame on every side for the others"
        );
        assert!(!covers(
            Some(&region(&[
                (ADD, rect(0, 0, 1920, 400)),
                (ADD, rect(0, 680, 1920, 400)),
                (ADD, rect(1420, 400, 500, 280)),
                (ADD, rect(500, 400, 920, 280)),
            ])),
            SURFACE
        ));
        // Phase `one_pixel_short`: a rounded corner or shadow left out.
        assert!(!covers(
            Some(&region(&[(ADD, rect(0, 0, 1920, 1079))])),
            SURFACE
        ));
        assert!(!covers(
            Some(&region(&[
                (ADD, rect(0, 0, 960, 1080)),
                (ADD, rect(961, 0, 959, 1080)),
            ])),
            SURFACE
        ));
        // Phase `a_hole_subtracted`.
        assert!(!covers(
            Some(&region(&[
                (ADD, rect(0, 0, 1920, 1080)),
                (SUB, rect(100, 100, 1, 1)),
            ])),
            SURFACE
        ));
        // Phase `a_hole_filled_again`: the last rectangle holding a pixel wins.
        assert!(covers(
            Some(&region(&[
                (ADD, rect(0, 0, 1920, 1080)),
                (SUB, rect(100, 100, 50, 50)),
                (ADD, rect(100, 100, 50, 50)),
            ])),
            SURFACE
        ));
        // Phase `subtracted_outside_the_surface`.
        assert!(covers(
            Some(&region(&[
                (ADD, rect(0, 0, 1920, 1080)),
                (SUB, rect(1920, 0, 100, 100)),
            ])),
            SURFACE
        ));
        // Phase `an_empty_surface`.
        assert!(!covers(Some(&region(&[(ADD, rect(0, 0, 1, 1))])), (0, 0)));
    }

    #[test]
    fn a_region_too_large_to_judge_does_not_count() {
        let stripes: Vec<_> = (0..=MAX_RECTS)
            .map(|row| {
                let row = i32::try_from(row).expect("a small count");
                (ADD, rect(0, row, 1920, 1))
            })
            .chain([(ADD, rect(0, 0, 1920, 1080))])
            .collect();
        assert!(!covers(Some(&region(&stripes)), SURFACE));
        // Scattered squares leave most of the surface undecided.
        let checks: Vec<_> = (0..MAX_RECTS)
            .map(|index| {
                let index = i32::try_from(index).expect("a small count");
                (
                    ADD,
                    rect(
                        index.saturating_mul(20).saturating_add(5),
                        index.saturating_mul(10).saturating_add(5),
                        2,
                        2,
                    ),
                )
            })
            .collect();
        assert!(!covers(Some(&region(&checks)), SURFACE));
    }

    #[test]
    fn the_surface_is_measured_at_its_largest() {
        let none = ViewportCachedState::default();
        assert_eq!(
            surface_extent((3840, 2160), 2, Transform::Normal, &none),
            (1920, 1080)
        );
        assert_eq!(
            surface_extent((3841, 2161), 2, Transform::Normal, &none),
            (1921, 1081),
            "an odd buffer at scale 2 is rounded up"
        );
        assert_eq!(
            surface_extent((1920, 1080), 1, Transform::_90, &none),
            (1080, 1920),
            "a quarter turn swaps the sides"
        );
        assert_eq!(
            surface_extent((1920, 1080), 0, Transform::Normal, &none),
            (1920, 1080),
            "a nonsensical scale counts as one"
        );
        // Fractional scaling: a 1.5× buffer with its logical destination.
        let destination = ViewportCachedState {
            src: None,
            dst: Some(Size::from((1280, 720))),
        };
        assert_eq!(
            surface_extent((1920, 1080), 1, Transform::Normal, &destination),
            (1920, 1080),
            "the buffer is larger than its destination, and wins"
        );
        let upscaled = ViewportCachedState {
            src: None,
            dst: Some(Size::from((3840, 2160))),
        };
        assert_eq!(
            surface_extent((1920, 1080), 1, Transform::Normal, &upscaled),
            (3840, 2160)
        );
        let cropped = ViewportCachedState {
            src: Some(Rectangle::new(
                Point::from((0.0, 0.0)),
                Size::from((2000.5, 100.0)),
            )),
            dst: None,
        };
        assert_eq!(
            surface_extent((1920, 1080), 1, Transform::Normal, &cropped),
            (2001, 1080)
        );
    }
}
