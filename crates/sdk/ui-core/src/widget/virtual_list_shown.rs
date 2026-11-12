//! What a [`super::VirtualList`] frame shows of its rows, as a later frame
//! compares it ([`crate::scroll_copy::Shown`]).

use vello::kurbo::{Affine, Rect};

/// Where a frame shows the rows: the offset, the list's box, the transform
/// it is drawn with, whether the rows stand beside the bar, what of the
/// list's own paint (selection, focus, a reorder) the rows do not carry, and
/// where its rows stand against each other under the headings it paints.
#[derive(Clone, Debug, PartialEq)]
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
    /// The rows near the box, in order: each one's key hash, its top in the
    /// list and its height. A row that took or gave up a heading's band, or
    /// moved because one above it did, stands elsewhere against the others:
    /// the rows still showing are not where the frame on screen drew them.
    pub(super) places: Vec<(u64, f64, f64)>,
    /// The headings drawn, each with how many rows it counts and whether its
    /// group is folded: a heading whose count changed is drawn anew.
    pub(super) headings: Vec<(String, usize, bool)>,
}

impl ListShown {
    /// Whether the rows both frames show stand as they did against each
    /// other, and the headings both draw say what they did. A scroll moves
    /// every row alike, and rows coming into view or leaving it are the
    /// scroll's to draw; anything else that moved a row, or changed a
    /// heading, is a frame no copy of the last one shows.
    fn places_agree(&self, other: &Self) -> bool {
        const SAME: f64 = 0.5;
        let mut shift = None;
        for (key, top, height) in &self.places {
            let Some((_, was, was_height)) = other.places.iter().find(|(at, _, _)| at == key)
            else {
                continue;
            };
            if (height - was_height).abs() > SAME {
                return false;
            }
            let moved = top - was;
            if (moved - *shift.get_or_insert(moved)).abs() > SAME {
                return false;
            }
        }
        self.headings.iter().all(|(heading, count, folded)| {
            other
                .headings
                .iter()
                .find(|(was, _, _)| was == heading)
                .is_none_or(|(_, was_count, was_folded)| was_count == count && was_folded == folded)
        })
    }
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
        self.moved_known
            && self.beside_bar == other.beside_bar
            && self.chrome == other.chrome
            && self.places_agree(other)
    }

    fn overlays(&self, out: &mut Vec<Rect>) {
        out.extend(self.overlay);
    }
}
