//! Page-space geometry shared by every part of the API.
//!
//! All coordinates are in **PDF points** in the page's *display* space: origin
//! at the top-left of the visible (crop) box after the page's own `/Rotate`
//! has been applied, x to the right, y downward. That is the space a viewer
//! lays pages out in, so glyph boxes, link rects, annotation rects and search
//! hits can be drawn without knowing anything about PDF user space.

use serde::{Deserialize, Serialize};

/// A point in page display space (points, y down).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle in page display space, stored by its corners
/// (`x0 <= x1`, `y0 <= y1` for every rectangle this crate produces).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    /// A rectangle from any two opposite corners.
    #[must_use]
    pub fn from_corners(a: Point, b: Point) -> Self {
        Self {
            x0: a.x.min(b.x),
            y0: a.y.min(b.y),
            x1: a.x.max(b.x),
            y1: a.y.max(b.y),
        }
    }

    /// A rectangle from its origin and size.
    #[must_use]
    pub fn from_xywh(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self::from_corners(Point::new(x, y), Point::new(x + w, y + h))
    }

    #[must_use]
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    #[must_use]
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    #[must_use]
    pub fn center(&self) -> Point {
        Point::new((self.x0 + self.x1) * 0.5, (self.y0 + self.y1) * 0.5)
    }

    /// Whether the rectangle has no area (a generated glyph without geometry).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !(self.width() > 0.0 && self.height() > 0.0)
    }

    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x0 && p.x <= self.x1 && p.y >= self.y0 && p.y <= self.y1
    }

    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }

    /// The smallest rectangle covering both.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// Length of the vertical overlap with `other` (0 when disjoint).
    #[must_use]
    pub fn vertical_overlap(&self, other: &Self) -> f32 {
        (self.y1.min(other.y1) - self.y0.max(other.y0)).max(0.0)
    }

    /// Euclidean distance from `p` to the rectangle (0 inside).
    #[must_use]
    pub fn distance_to(&self, p: Point) -> f32 {
        let dx = (self.x0 - p.x).max(p.x - self.x1).max(0.0);
        let dy = (self.y0 - p.y).max(p.y - self.y1).max(0.0);
        dx.hypot(dy)
    }

    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.x0.is_finite() && self.y0.is_finite() && self.x1.is_finite() && self.y1.is_finite()
    }
}

/// Display size of a page in points (after the page's own `/Rotate`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

/// How a page's PDF user space (points, y up, as stored in the file) maps to
/// its display space: the visible box in user space plus the page's own
/// clockwise `/Rotate`.
///
/// Records that must survive re-rendering (ink, shapes, markup quads) are
/// stored in user space; drawing converts them with [`Self::to_display`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PageGeometry {
    /// The visible (crop) box in user space: `x0,y0` lower-left, `x1,y1`
    /// upper-right.
    pub user_box: Rect,
    /// The page's own `/Rotate`.
    pub rotation: Rotation,
}

impl PageGeometry {
    /// Size of the page before its own rotation.
    fn unrotated(&self) -> PageSize {
        PageSize {
            width: self.user_box.width(),
            height: self.user_box.height(),
        }
    }

    /// Display size (after the page's own rotation).
    #[must_use]
    pub fn size(&self) -> PageSize {
        self.rotation.apply_size(self.unrotated())
    }

    /// User space → display space.
    #[must_use]
    pub fn to_display(&self, p: Point) -> Point {
        let flipped = Point::new(p.x - self.user_box.x0, self.user_box.y1 - p.y);
        self.rotation.apply_point(self.unrotated(), flipped)
    }

    /// Display space → user space.
    #[must_use]
    pub fn to_user(&self, p: Point) -> Point {
        let flipped = self.rotation.invert_point(self.unrotated(), p);
        Point::new(flipped.x + self.user_box.x0, self.user_box.y1 - flipped.y)
    }

    /// A user-space rectangle (any corner order) in display space.
    #[must_use]
    pub fn rect_to_display(&self, r: Rect) -> Rect {
        Rect::from_corners(
            self.to_display(Point::new(r.x0, r.y0)),
            self.to_display(Point::new(r.x1, r.y1)),
        )
    }

    /// A display-space rectangle in user space, normalised so `x0,y0` is
    /// the lower-left corner.
    #[must_use]
    pub fn rect_to_user(&self, r: Rect) -> Rect {
        Rect::from_corners(
            self.to_user(Point::new(r.x0, r.y0)),
            self.to_user(Point::new(r.x1, r.y1)),
        )
    }
}

/// A clockwise quarter-turn rotation applied on top of the page's own
/// `/Rotate` — how a user rotation record is expressed to the renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Rotation {
    #[default]
    None,
    Cw90,
    Cw180,
    Cw270,
}

impl Rotation {
    /// Rotation from a number of degrees; any multiple of 90 (negative too)
    /// is accepted, anything else is `None`.
    #[must_use]
    pub fn from_degrees(degrees: i32) -> Option<Self> {
        if degrees % 90 != 0 {
            return None;
        }
        Some(match degrees.rem_euclid(360) {
            90 => Self::Cw90,
            180 => Self::Cw180,
            270 => Self::Cw270,
            _ => Self::None,
        })
    }

    #[must_use]
    pub const fn degrees(self) -> i32 {
        match self {
            Self::None => 0,
            Self::Cw90 => 90,
            Self::Cw180 => 180,
            Self::Cw270 => 270,
        }
    }

    /// Number of clockwise quarter turns (0..=3).
    #[must_use]
    pub const fn quarter_turns(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Cw90 => 1,
            Self::Cw180 => 2,
            Self::Cw270 => 3,
        }
    }

    /// Composition of two rotations.
    #[must_use]
    pub fn then(self, other: Self) -> Self {
        Self::from_degrees(self.degrees().saturating_add(other.degrees())).unwrap_or_default()
    }

    /// Whether width and height swap under this rotation.
    #[must_use]
    pub const fn swaps_axes(self) -> bool {
        matches!(self, Self::Cw90 | Self::Cw270)
    }

    /// The size a page of `size` occupies once rotated.
    #[must_use]
    pub fn apply_size(self, size: PageSize) -> PageSize {
        if self.swaps_axes() {
            PageSize {
                width: size.height,
                height: size.width,
            }
        } else {
            size
        }
    }

    /// Map a point of an unrotated page of `size` into the rotated page.
    #[must_use]
    pub fn apply_point(self, size: PageSize, p: Point) -> Point {
        match self {
            Self::None => p,
            Self::Cw90 => Point::new(size.height - p.y, p.x),
            Self::Cw180 => Point::new(size.width - p.x, size.height - p.y),
            Self::Cw270 => Point::new(p.y, size.width - p.x),
        }
    }

    /// Map a point of the rotated page back onto the unrotated page of `size`.
    #[must_use]
    pub fn invert_point(self, size: PageSize, p: Point) -> Point {
        match self {
            Self::None => p,
            Self::Cw90 => Point::new(p.y, size.height - p.x),
            Self::Cw180 => Point::new(size.width - p.x, size.height - p.y),
            Self::Cw270 => Point::new(size.width - p.y, p.x),
        }
    }

    /// Map a rectangle of an unrotated page of `size` into the rotated page.
    #[must_use]
    pub fn apply_rect(self, size: PageSize, r: Rect) -> Rect {
        Rect::from_corners(
            self.apply_point(size, Point::new(r.x0, r.y0)),
            self.apply_point(size, Point::new(r.x1, r.y1)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_geometry_rotates_and_maps_user_space_to_display_space() {
        {
            let size = PageSize {
                width: 600.0,
                height: 800.0,
            };
            let p = Point::new(10.0, 20.0);
            for r in [
                Rotation::None,
                Rotation::Cw90,
                Rotation::Cw180,
                Rotation::Cw270,
            ] {
                let q = r.apply_point(size, p);
                let back = r.invert_point(size, q);
                assert!(
                    (back.x - p.x).abs() < 1e-3 && (back.y - p.y).abs() < 1e-3,
                    "{r:?}"
                );
                let rotated = r.apply_size(size);
                assert!(q.x >= 0.0 && q.x <= rotated.width && q.y >= 0.0 && q.y <= rotated.height);
            }
            assert_eq!(Rotation::Cw90.then(Rotation::Cw270), Rotation::None);
            assert_eq!(Rotation::from_degrees(-90), Some(Rotation::Cw270));
            assert_eq!(Rotation::from_degrees(45), None);
        }
        {
            // A 600x800 crop box offset by (10, 20), rotated 90° clockwise.
            let g = PageGeometry {
                user_box: Rect {
                    x0: 10.0,
                    y0: 20.0,
                    x1: 610.0,
                    y1: 820.0,
                },
                rotation: Rotation::Cw90,
            };
            assert_eq!(
                g.size(),
                PageSize {
                    width: 800.0,
                    height: 600.0
                }
            );
            let tl = g.to_display(Point::new(10.0, 820.0));
            assert!((tl.x - 800.0).abs() < 1e-3 && tl.y.abs() < 1e-3);
            let p = Point::new(123.0, 456.0);
            let back = g.to_user(g.to_display(p));
            assert!((back.x - p.x).abs() < 1e-3 && (back.y - p.y).abs() < 1e-3);
            let unrotated = PageGeometry {
                rotation: Rotation::None,
                ..g
            };
            let d = unrotated.to_display(Point::new(10.0, 820.0));
            assert!(d.x.abs() < 1e-3 && d.y.abs() < 1e-3);
        }
    }
}
