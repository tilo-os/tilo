//! How a workspace lays out its windows: in strips, or on a canvas
//! (docs/designs/app-language/canvas.md, workspace-model.md §6).
//!
//! The canvas is another arrangement of the same windows: every window group
//! stays in its strip, and the canvas adds where each group stands on a 2D
//! surface. Places are whole logical pixels in canvas space, shared with the
//! workspace like the rest of its arrangement; how far a person has panned
//! or zoomed is their own lens and is not stored here.
//!
//! Placing never moves what is already there: a new window takes the free
//! space nearest the point it belongs to ([`Arrangement::free_place_near`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::GroupId;

/// Which arrangement a workspace shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArrangementKind {
    /// Rows of windows, one strip at a time.
    #[default]
    Strips,
    /// Windows anywhere on a pannable, zoomable surface.
    Canvas,
}

impl ArrangementKind {
    /// The kind as the shared record and the desktop document store it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strips => "strips",
            Self::Canvas => "canvas",
        }
    }

    /// The stored kind; anything this build does not know reads as strips,
    /// so a newer arrangement never leaves an older device without one.
    #[must_use]
    pub fn parse(stored: &str) -> Self {
        match stored {
            "canvas" => Self::Canvas,
            _ => Self::Strips,
        }
    }
}

/// Where a window group stands on the canvas, in canvas pixels: its top-left
/// corner and its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanvasPlace {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl CanvasPlace {
    #[must_use]
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The right edge, exclusive.
    #[must_use]
    pub fn right(&self) -> i64 {
        i64::from(self.x).saturating_add(i64::from(self.width))
    }

    /// The bottom edge, exclusive.
    #[must_use]
    pub fn bottom(&self) -> i64 {
        i64::from(self.y).saturating_add(i64::from(self.height))
    }

    /// Whether the two overlap, or come closer than `gap` to each other.
    #[must_use]
    pub fn crowds(&self, other: &Self, gap: u32) -> bool {
        let gap = i64::from(gap);
        i64::from(self.x) < other.right().saturating_add(gap)
            && i64::from(other.x) < self.right().saturating_add(gap)
            && i64::from(self.y) < other.bottom().saturating_add(gap)
            && i64::from(other.y) < self.bottom().saturating_add(gap)
    }

    /// Whether the two share any area.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        self.crowds(other, 0)
    }

    /// The point of this place nearest `(x, y)`, and how far that is,
    /// squared: zero inside it.
    #[must_use]
    pub fn distance_sq(&self, (x, y): (i32, i32)) -> i64 {
        let cx = i64::from(x).clamp(i64::from(self.x), self.right());
        let cy = i64::from(y).clamp(i64::from(self.y), self.bottom());
        let dx = i64::from(x).saturating_sub(cx);
        let dy = i64::from(y).saturating_sub(cy);
        dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
    }
}

/// The space kept between two windows on the canvas, in canvas pixels: the
/// design language's `group` rhythm.
pub const CANVAS_GAP: u32 = 24;

/// A workspace's arrangement: which kind it shows and where its groups stand
/// on the canvas. Places are kept while the strips show, so going back to
/// the canvas finds every window where it was left.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arrangement {
    #[serde(default)]
    pub kind: ArrangementKind,
    // Read through the id-key helper: a workspace is stored flattened, where
    // a map's numeric keys arrive as strings.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "crate::serde_ids::deserialize_id_btree_map"
    )]
    pub places: BTreeMap<GroupId, CanvasPlace>,
}

impl Arrangement {
    /// Whether this is the arrangement a workspace has before anyone chose
    /// one: strips, nothing placed.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.kind == ArrangementKind::Strips && self.places.is_empty()
    }

    /// Whether the workspace shows its canvas.
    #[must_use]
    pub fn is_canvas(&self) -> bool {
        self.kind == ArrangementKind::Canvas
    }

    /// Where `group` stands, once placed.
    #[must_use]
    pub fn place_of(&self, group: GroupId) -> Option<CanvasPlace> {
        self.places.get(&group).copied()
    }

    /// The free place of `width` × `height` nearest `anchor` (the corner a
    /// window placed there would have), at least [`CANVAS_GAP`] from every
    /// placed group but `except`. Nothing placed moves. Deterministic: the
    /// same arrangement and anchor give the same place on every device.
    #[must_use]
    pub fn free_place_near(
        &self,
        anchor: (i32, i32),
        width: u32,
        height: u32,
        except: Option<GroupId>,
    ) -> CanvasPlace {
        let others: Vec<CanvasPlace> = self
            .places
            .iter()
            .filter(|(id, _)| Some(**id) != except)
            .map(|(_, place)| *place)
            .collect();
        let free = |place: &CanvasPlace| !others.iter().any(|o| place.crowds(o, CANVAS_GAP));
        let wanted = CanvasPlace::new(anchor.0, anchor.1, width, height);
        if free(&wanted) {
            return wanted;
        }
        // Every free place nearest the anchor touches an obstacle's edge
        // (or the anchor's own line): try each obstacle's four sides, on the
        // anchor's line and flush with the obstacle's edges.
        let gap = i64::from(CANVAS_GAP);
        let (w, h) = (i64::from(width), i64::from(height));
        let mut xs = vec![i64::from(anchor.0)];
        let mut ys = vec![i64::from(anchor.1)];
        for o in &others {
            xs.extend([
                o.right().saturating_add(gap),
                i64::from(o.x).saturating_sub(gap).saturating_sub(w),
                i64::from(o.x),
                o.right().saturating_sub(w),
            ]);
            ys.extend([
                o.bottom().saturating_add(gap),
                i64::from(o.y).saturating_sub(gap).saturating_sub(h),
                i64::from(o.y),
                o.bottom().saturating_sub(h),
            ]);
        }
        let clamp = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
        let mut best: Option<(i64, CanvasPlace)> = None;
        for &x in &xs {
            for &y in &ys {
                let place = CanvasPlace::new(clamp(x), clamp(y), width, height);
                if !free(&place) {
                    continue;
                }
                let dx = x.saturating_sub(i64::from(anchor.0));
                let dy = y.saturating_sub(i64::from(anchor.1));
                let distance = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy));
                if best.is_none_or(|(d, b)| (distance, b.y, b.x) < (d, place.y, place.x)) {
                    best = Some((distance, place));
                }
            }
        }
        best.map_or_else(
            || {
                // Every candidate crowded something (cannot happen with a
                // finite set: right of the rightmost is always free); go there.
                let right = others.iter().map(CanvasPlace::right).max().unwrap_or(0);
                CanvasPlace::new(clamp(right.saturating_add(gap)), anchor.1, width, height)
            },
            |(_, place)| place,
        )
    }

    /// The free place beside `subject` (a window, a selection) for a window
    /// of `width` × `height`: to its trailing side when there is room
    /// there, else the nearest free space.
    #[must_use]
    pub fn free_place_beside(
        &self,
        subject: CanvasPlace,
        width: u32,
        height: u32,
        except: Option<GroupId>,
    ) -> CanvasPlace {
        let gap = i64::from(CANVAS_GAP);
        let x = i32::try_from(subject.right().saturating_add(gap)).unwrap_or(i32::MAX);
        self.free_place_near((x, subject.y), width, height, except)
    }

    /// The groups standing within `radius` canvas pixels of `point`,
    /// nearest first.
    #[must_use]
    pub fn near(&self, point: (i32, i32), radius: u32) -> Vec<GroupId> {
        let reach = i64::from(radius).saturating_mul(i64::from(radius));
        let mut hits: Vec<(i64, GroupId)> = self
            .places
            .iter()
            .map(|(id, place)| (place.distance_sq(point), *id))
            .filter(|(distance, _)| *distance <= reach)
            .collect();
        hits.sort_unstable();
        hits.into_iter().map(|(_, id)| id).collect()
    }

    /// The groups a selection rectangle touches, in id order.
    #[must_use]
    pub fn within(&self, area: CanvasPlace) -> Vec<GroupId> {
        self.places
            .iter()
            .filter(|(_, place)| place.intersects(&area))
            .map(|(id, _)| *id)
            .collect()
    }

    /// The smallest place holding every one of `groups` that is placed.
    #[must_use]
    pub fn bounds_of(&self, groups: &[GroupId]) -> Option<CanvasPlace> {
        let placed: Vec<CanvasPlace> = groups.iter().filter_map(|g| self.place_of(*g)).collect();
        let x0 = placed.iter().map(|p| i64::from(p.x)).min()?;
        let y0 = placed.iter().map(|p| i64::from(p.y)).min()?;
        let x1 = placed.iter().map(CanvasPlace::right).max()?;
        let y1 = placed.iter().map(CanvasPlace::bottom).max()?;
        Some(CanvasPlace::new(
            i32::try_from(x0).ok()?,
            i32::try_from(y0).ok()?,
            u32::try_from(x1.saturating_sub(x0)).ok()?,
            u32::try_from(y1.saturating_sub(y0)).ok()?,
        ))
    }

    /// Forget the places of groups for which `live` is false: a closed
    /// window leaves its space free. Returns whether any went.
    pub fn retain_groups(&mut self, live: impl Fn(GroupId) -> bool) -> bool {
        let before = self.places.len();
        self.places.retain(|id, _| live(*id));
        self.places.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(places: &[(GroupId, CanvasPlace)]) -> Arrangement {
        Arrangement {
            kind: ArrangementKind::Canvas,
            places: places.iter().copied().collect(),
        }
    }

    #[test]
    fn an_empty_canvas_places_a_window_exactly_where_pointed() {
        let place = Arrangement::default().free_place_near((120, -40), 640, 480, None);
        assert_eq!(place, CanvasPlace::new(120, -40, 640, 480));
    }

    #[test]
    fn a_window_in_the_way_is_never_moved_and_never_covered() {
        let board = CanvasPlace::new(0, 0, 800, 600);
        let arrangement = canvas(&[(1, board)]);
        let place = arrangement.free_place_near((100, 100), 640, 480, None);
        assert!(
            !place.crowds(&board, CANVAS_GAP),
            "{place:?} crowds {board:?}"
        );
        assert_eq!(arrangement.place_of(1), Some(board), "nothing moved");
    }

    #[test]
    fn the_free_place_chosen_is_the_nearest_one() {
        // A wide window above the anchor: going below it is nearer than
        // going past its far side.
        let wide = CanvasPlace::new(-1000, 0, 2000, 300);
        let arrangement = canvas(&[(1, wide)]);
        let place = arrangement.free_place_near((0, 100), 400, 300, None);
        assert_eq!((place.x, place.y), (0, 324));
    }

    #[test]
    fn placing_is_deterministic_and_fills_around_a_crowd() {
        let mut arrangement = canvas(&[]);
        let mut placed = Vec::new();
        for id in 1..=6 {
            let place = arrangement.free_place_near((0, 0), 400, 300, None);
            for other in &placed {
                assert!(
                    !place.crowds(other, CANVAS_GAP),
                    "{place:?} crowds {other:?}"
                );
            }
            assert_eq!(place, arrangement.free_place_near((0, 0), 400, 300, None));
            arrangement.places.insert(id, place);
            placed.push(place);
        }
    }

    #[test]
    fn beside_a_window_means_its_trailing_side_when_free() {
        let card = CanvasPlace::new(0, 0, 400, 300);
        let arrangement = canvas(&[(1, card)]);
        let place = arrangement.free_place_beside(card, 300, 200, None);
        assert_eq!((place.x, place.y), (424, 0));
    }

    #[test]
    fn a_window_being_placed_again_ignores_its_own_old_place() {
        let card = CanvasPlace::new(0, 0, 400, 300);
        let arrangement = canvas(&[(1, card)]);
        assert_eq!(
            arrangement.free_place_near((10, 10), 400, 300, Some(1)).x,
            10
        );
    }

    #[test]
    fn nearby_and_selected_windows_are_found() {
        let arrangement = canvas(&[
            (1, CanvasPlace::new(0, 0, 100, 100)),
            (2, CanvasPlace::new(500, 0, 100, 100)),
            (3, CanvasPlace::new(2000, 2000, 100, 100)),
        ]);
        assert_eq!(arrangement.near((150, 50), 600), vec![1, 2]);
        assert_eq!(
            arrangement.within(CanvasPlace::new(50, 50, 500, 10)),
            vec![1, 2]
        );
        assert_eq!(
            arrangement.bounds_of(&[1, 2]),
            Some(CanvasPlace::new(0, 0, 600, 100))
        );
    }

    #[test]
    fn closing_a_window_frees_its_place() {
        let mut arrangement = canvas(&[(1, CanvasPlace::new(0, 0, 10, 10))]);
        assert!(arrangement.retain_groups(|id| id != 1));
        assert!(arrangement.places.is_empty());
    }

    #[test]
    fn a_stored_workspace_keeps_its_canvas() {
        let mut workspace = crate::Workspace::create("Desk");
        workspace.arrangement = canvas(&[(920_001, CanvasPlace::new(40, -8, 400, 300))]);
        let json = serde_json::to_string(&workspace).expect("serialises");
        let back: crate::Workspace = serde_json::from_str(&json).expect("reads back");
        assert_eq!(back.arrangement, workspace.arrangement);
    }

    #[test]
    fn an_unknown_kind_reads_as_strips() {
        assert_eq!(ArrangementKind::parse("canvas"), ArrangementKind::Canvas);
        assert_eq!(ArrangementKind::parse("spiral"), ArrangementKind::Strips);
    }
}
