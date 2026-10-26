//! What a [`super::VirtualList`] frame shows of its rows, as a later frame
//! compares it ([`crate::scroll_copy::Shown`]).

use vello::kurbo::{Affine, Rect};

/// Where a frame shows the rows: the offset, the list's box, the transform
/// it is drawn with, whether the rows stand beside the bar, and what of the
/// list's own paint (selection, focus, a reorder) the rows do not carry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ListShown {
    pub(super) offset: f64,
    /// How far along the frame stands, as the scroll a copy compares: the
    /// last presented frame's, less how far its rows moved on screen since.
    pub(super) position: f64,
    /// Whether how far the rows moved is known: the first row the last
    /// presented frame showed is still that row.
    pub(super) moved_known: bool,
    pub(super) rect: Rect,
    pub(super) transform: Affine,
    pub(super) beside_bar: bool,
    pub(super) chrome: u64,
    /// Rows were drawn off their places (a list change moving): the frame
    /// is not a copy of another, whatever else matches.
    pub(super) in_motion: bool,
    /// The first row on screen: its index, its key's hash and its top in
    /// the list. The next frame tells how far the rows moved on screen from
    /// where it stands then, which is the scroll only while no row it showed
    /// changed height; a row measured for the first time above it moves the
    /// offset and the row alike.
    pub(super) first_shown: Option<(usize, u64, f64)>,
    /// The bar's track, which stands over the rows without moving.
    pub(super) overlay: Option<Rect>,
}

impl crate::scroll_copy::Shown for ListShown {
    fn scroll(&self) -> vello::kurbo::Vec2 {
        vello::kurbo::Vec2::new(0.0, self.position)
    }

    fn clip(&self) -> Rect {
        self.rect
    }

    fn transform(&self) -> Affine {
        self.transform
    }

    fn same_view(&self, other: &Self) -> bool {
        self.moved_known && self.beside_bar == other.beside_bar && self.chrome == other.chrome
    }

    fn overlays(&self, out: &mut Vec<Rect>) {
        out.extend(self.overlay);
    }
}
