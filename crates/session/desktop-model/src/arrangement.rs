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
use crate::layout::Strip;

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

/// The least a window stands at wherever the workspace keeps its size (on the
/// canvas or floating), in logical pixels: as wide as a window in a strip can
/// get ([`crate::width::WIDTH_FLOOR_PX`]: its tab pill, its controls and a
/// scrollbar), and as tall as its bar with a few lines under it. Every member,
/// person or agent, writes the shared record, so a size below it is never
/// kept: it is raised when written and when read back, and a window laid out
/// from a place smaller than this (one a build without the floor wrote) still
/// stands at it.
pub const WINDOW_MIN_WIDTH: u32 = 160;
/// See [`WINDOW_MIN_WIDTH`].
pub const WINDOW_MIN_HEIGHT: u32 = 120;

/// Where a window group stands on the canvas, in canvas pixels: its top-left
/// corner and its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanvasPlace {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Its height is the most it takes: what it shows is as tall as what it holds up to it (a
    /// thing found where the person clicked, which names no size of its own). A place the person
    /// sized — a rectangle they dragged, an edge they pulled — keeps its height exactly.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hugs: bool,
}

impl CanvasPlace {
    #[must_use]
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            hugs: false,
        }
    }

    /// This place, as tall as what it shows up to its height ([`Self::hugs`]).
    #[must_use]
    pub fn hugging(mut self) -> Self {
        self.hugs = true;
        self
    }

    /// This place as a window's: no smaller than [`WINDOW_MIN_WIDTH`] ×
    /// [`WINDOW_MIN_HEIGHT`], from the same corner. A place that is an area (a
    /// selection, the bounds of several windows) keeps its own size.
    #[must_use]
    pub fn window(self) -> Self {
        Self {
            width: self.width.max(WINDOW_MIN_WIDTH),
            height: self.height.max(WINDOW_MIN_HEIGHT),
            ..self
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
        self.crowds_apart(other, gap, gap)
    }

    /// Whether the two overlap, or come closer than `across` side by side or `down` one over the
    /// other.
    #[must_use]
    pub fn crowds_apart(&self, other: &Self, across: u32, down: u32) -> bool {
        let (across, down) = (i64::from(across), i64::from(down));
        i64::from(self.x) < other.right().saturating_add(across)
            && i64::from(other.x) < self.right().saturating_add(across)
            && i64::from(self.y) < other.bottom().saturating_add(down)
            && i64::from(other.y) < self.bottom().saturating_add(down)
    }

    /// Whether the two stand too close on a canvas: closer than [`CANVAS_GAP`] side by side, or
    /// than [`CANVAS_ROW_GAP`] one over the other, so the caption over the lower one is never
    /// under the upper one.
    #[must_use]
    pub fn crowds_on_canvas(&self, other: &Self) -> bool {
        self.crowds_apart(other, CANVAS_GAP, CANVAS_ROW_GAP)
    }

    /// Whether this place stands wholly inside `area`.
    #[must_use]
    pub fn inside(&self, area: &Self) -> bool {
        self.x >= area.x
            && self.y >= area.y
            && self.right() <= area.right()
            && self.bottom() <= area.bottom()
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

/// The band a place keeps over its top edge on the canvas for its caption and its handle, in
/// canvas pixels: the toolkit's place band (`ui::widget::canvas_place::PLACE_BAND`). A place's
/// point is its view's top, and the band stands above it.
pub const CANVAS_BAND: u32 = 44;

/// The space kept between a place and the one under it on the canvas: the lower one's band and
/// the canvas's gap, so its caption stands clear of the place above it.
pub const CANVAS_ROW_GAP: u32 = CANVAS_GAP + CANVAS_BAND;

/// The window a group the desk opened takes on the canvas when nobody named a place for it:
/// the framed canvas's window, in canvas pixels, which is what the arrangement's own place
/// action defaults to for a place a person types at.
const MISSING_PLACE_WIDTH: u32 = 720;
/// See [`MISSING_PLACE_WIDTH`].
const MISSING_PLACE_HEIGHT: u32 = 520;

/// The corner of the work area a floated window keeps its distance from, so
/// the window lands sensibly on every member's screen size.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum FloatCorner {
    TopLeft,
    TopRight,
    /// The corner a window floats to when the person names none, as cards
    /// do.
    #[default]
    BottomRight,
    BottomLeft,
    /// A corner a newer build stores that this one does not know: the window
    /// still floats, kept at the default corner.
    #[serde(other)]
    Unknown,
}

/// Where a window floats: its corner of the work area, the distance from the
/// two edges meeting there, and its size, all in whole logical pixels.
///
/// Anchoring to a corner rather than to the screen's top-left keeps a float
/// where the person put it on a display of any size: a window 40 px from the
/// bottom-right is 40 px from the bottom-right of everyone's work area.
///
/// Its size is a window's: never under [`WINDOW_MIN_WIDTH`] ×
/// [`WINDOW_MIN_HEIGHT`], however it was made or written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "StoredFloat")]
pub struct FloatPlace {
    #[serde(default)]
    pub corner: FloatCorner,
    /// Distance from the corner's left or right edge.
    pub dx: i32,
    /// Distance from the corner's top or bottom edge.
    pub dy: i32,
    pub width: u32,
    pub height: u32,
}

/// A [`FloatPlace`] as the record holds it, before its size is a window's.
#[derive(Deserialize)]
struct StoredFloat {
    #[serde(default)]
    corner: FloatCorner,
    dx: i32,
    dy: i32,
    width: u32,
    height: u32,
}

impl From<StoredFloat> for FloatPlace {
    /// A float read back is a window's size, whoever wrote a smaller one: a
    /// sliver kept in the record is a window everyone sees again.
    fn from(stored: StoredFloat) -> Self {
        Self::new(
            stored.corner,
            stored.dx,
            stored.dy,
            stored.width,
            stored.height,
        )
    }
}

impl FloatPlace {
    /// A float of `width` × `height`, raised to a window's least size.
    #[must_use]
    pub fn new(corner: FloatCorner, dx: i32, dy: i32, width: u32, height: u32) -> Self {
        Self {
            corner,
            dx,
            dy,
            width: width.max(WINDOW_MIN_WIDTH),
            height: height.max(WINDOW_MIN_HEIGHT),
        }
    }

    /// The float of a window of `width` × `height` let go as `rect` on the
    /// work area `area`: anchored to the corner `rect` lies nearest, at the
    /// distance it was let go from that corner's edges.
    #[must_use]
    pub fn of_rect(area: CanvasPlace, rect: CanvasPlace) -> Self {
        let centre = (
            rect.x
                .saturating_add(i32::try_from(rect.width / 2).unwrap_or(i32::MAX)),
            rect.y
                .saturating_add(i32::try_from(rect.height / 2).unwrap_or(i32::MAX)),
        );
        let right = i32::try_from(area.right()).unwrap_or(i32::MAX);
        let bottom = i32::try_from(area.bottom()).unwrap_or(i32::MAX);
        let corners = [
            FloatCorner::TopLeft,
            FloatCorner::TopRight,
            FloatCorner::BottomRight,
            FloatCorner::BottomLeft,
        ];
        let corner = corners
            .into_iter()
            .min_by_key(|corner| {
                let at = corner_point(*corner, area);
                let dx = i64::from(at.0).saturating_sub(i64::from(centre.0));
                let dy = i64::from(at.1).saturating_sub(i64::from(centre.1));
                (
                    dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)),
                    *corner,
                )
            })
            .unwrap_or_default();
        let dx = if corner.is_right() {
            right.saturating_sub(
                rect.x
                    .saturating_add(i32::try_from(rect.width).unwrap_or(0)),
            )
        } else {
            rect.x.saturating_sub(area.x)
        };
        let dy = if corner.is_bottom() {
            bottom.saturating_sub(
                rect.y
                    .saturating_add(i32::try_from(rect.height).unwrap_or(0)),
            )
        } else {
            rect.y.saturating_sub(area.y)
        };
        Self::new(corner, dx.max(0), dy.max(0), rect.width, rect.height)
    }

    /// The window's rect on the work area `area`: its size, never larger
    /// than the area, at its distance from the corner, kept inside the area.
    #[must_use]
    pub fn resolved(&self, area: CanvasPlace) -> CanvasPlace {
        // A window's least size, unless the work area itself is smaller.
        let width = self.width.max(WINDOW_MIN_WIDTH).min(area.width.max(1));
        let height = self.height.max(WINDOW_MIN_HEIGHT).min(area.height.max(1));
        let room = |total: u32, size: u32| i32::try_from(total.saturating_sub(size)).unwrap_or(0);
        let dx = self.dx.max(0).min(room(area.width, width));
        let dy = self.dy.max(0).min(room(area.height, height));
        let x = if self.corner.is_right() {
            area.x
                .saturating_add(room(area.width, width))
                .saturating_sub(dx)
        } else {
            area.x.saturating_add(dx)
        };
        let y = if self.corner.is_bottom() {
            area.y
                .saturating_add(room(area.height, height))
                .saturating_sub(dy)
        } else {
            area.y.saturating_add(dy)
        };
        CanvasPlace::new(x, y, width, height)
    }

    /// The face of the window nearest its corner, as the corner's distance
    /// from the corner's own edges (0 = flush with them): what a resize or a
    /// drag keeps while the window changes size around that face.
    #[must_use]
    pub fn at(&self, dx: i32, dy: i32) -> Self {
        Self {
            dx: dx.max(0),
            dy: dy.max(0),
            ..*self
        }
    }
}

impl FloatCorner {
    /// Whether the corner is on the work area's right side.
    #[must_use]
    pub fn is_right(self) -> bool {
        matches!(self, Self::TopRight | Self::BottomRight)
    }

    /// Whether the corner is on the work area's bottom side.
    #[must_use]
    pub fn is_bottom(self) -> bool {
        matches!(self, Self::BottomLeft | Self::BottomRight)
    }

    /// Its name where a person or an agent names it: an action's argument and
    /// a shell's reading of the arrangement.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomRight | Self::Unknown => "bottom-right",
            Self::BottomLeft => "bottom-left",
        }
    }

    /// The corner `text` names; `None` for a name this build does not know.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "top-left" => Some(Self::TopLeft),
            "top-right" => Some(Self::TopRight),
            "bottom-right" => Some(Self::BottomRight),
            "bottom-left" => Some(Self::BottomLeft),
            _ => None,
        }
    }
}

/// The point of the work area `area` a corner names.
fn corner_point(corner: FloatCorner, area: CanvasPlace) -> (i32, i32) {
    let right = i32::try_from(area.right()).unwrap_or(i32::MAX);
    let bottom = i32::try_from(area.bottom()).unwrap_or(i32::MAX);
    (
        if corner.is_right() { right } else { area.x },
        if corner.is_bottom() { bottom } else { area.y },
    )
}

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
        deserialize_with = "deserialize_window_places"
    )]
    pub places: BTreeMap<GroupId, CanvasPlace>,
    /// Where each floated window stands: a window the person lifted off the
    /// grid, over it and on every member's desk. The group keeps its strip
    /// and order, so snapping it back returns it exactly where it was.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "crate::serde_ids::deserialize_id_btree_map"
    )]
    pub floats: BTreeMap<GroupId, FloatPlace>,
}

impl Arrangement {
    /// Whether this is the arrangement a workspace has before anyone chose
    /// one: strips, nothing placed, nothing floating.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.kind == ArrangementKind::Strips && self.places.is_empty() && self.floats.is_empty()
    }

    /// Where `group` stands, once placed.
    #[must_use]
    pub fn place_of(&self, group: GroupId) -> Option<CanvasPlace> {
        self.places.get(&group).copied()
    }

    /// Where `group` floats, while it does.
    #[must_use]
    pub fn float_of(&self, group: GroupId) -> Option<FloatPlace> {
        self.floats.get(&group).copied()
    }

    /// Stand `group` at `(x, y)` on the canvas, keeping its size and every other place where it
    /// is: what a drag that let a place go on the canvas commits, one arranging action for the
    /// whole gesture. Nothing placed moves. Whether the place stood somewhere else.
    pub fn set_position(&mut self, group: GroupId, x: i32, y: i32) -> bool {
        let Some(place) = self.places.get_mut(&group) else {
            return false;
        };
        if place.x == x && place.y == y {
            return false;
        }
        place.x = x;
        place.y = y;
        true
    }

    /// Give `group` the size `width` × `height` on the canvas, keeping its corner where it
    /// stands. Returns whether it changed.
    pub fn set_size(&mut self, group: GroupId, width: u32, height: u32) -> bool {
        let Some(place) = self.places.get_mut(&group) else {
            return false;
        };
        if place.width == width && place.height == height && !place.hugs {
            return false;
        }
        place.width = width;
        place.height = height;
        // Sized by hand, it keeps the size it was given.
        place.hugs = false;
        true
    }

    /// Float `group` where `float` says (`None` snaps it back into the grid).
    /// Returns whether it changed.
    pub fn set_float(&mut self, group: GroupId, float: Option<FloatPlace>) -> bool {
        let float = float.map(|float| {
            FloatPlace::new(float.corner, float.dx, float.dy, float.width, float.height)
        });
        match float {
            Some(float) => self.floats.insert(group, float) != Some(float),
            None => self.floats.remove(&group).is_some(),
        }
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
        let free = |place: &CanvasPlace| !others.iter().any(|o| place.crowds_on_canvas(o));
        let wanted = CanvasPlace::new(anchor.0, anchor.1, width, height);
        if free(&wanted) {
            return wanted;
        }
        // Every free place nearest the anchor touches an obstacle's edge
        // (or the anchor's own line): try each obstacle's four sides, on the
        // anchor's line and flush with the obstacle's edges.
        let gap = i64::from(CANVAS_GAP);
        let row_gap = i64::from(CANVAS_ROW_GAP);
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
                o.bottom().saturating_add(row_gap),
                i64::from(o.y).saturating_sub(row_gap).saturating_sub(h),
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

    /// The free place next to `subject` for a window of `width` × `height`:
    /// to its trailing side at its own line, then below it, then below its
    /// trailing side, and onward in that order down the two columns the anchor
    /// starts — never above it or to its leading side, so a place opened beside
    /// what a person is in stands where they look rather than off the top or
    /// the left of the canvas. Nothing placed moves, and every candidate is
    /// [`CANVAS_GAP`] clear of everything but `except`.
    ///
    /// Deterministic: the same arrangement and anchor give the same place on
    /// every device. A canvas crowded below both columns continues down the
    /// trailing column, where the canvas pans.
    #[must_use]
    pub fn free_place_next_to(
        &self,
        subject: CanvasPlace,
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
        let free = |place: &CanvasPlace| !others.iter().any(|o| place.crowds_on_canvas(o));
        let gap = i64::from(CANVAS_GAP);
        let row_gap = i64::from(CANVAS_ROW_GAP);
        let step_y = i64::from(height).saturating_add(row_gap);
        let right = subject.right().saturating_add(gap);
        let below = subject.bottom().saturating_add(row_gap);
        let clamp = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
        // Past every obstacle's foot both columns are free: the walk stops there.
        let foot = others
            .iter()
            .map(CanvasPlace::bottom)
            .max()
            .unwrap_or(0)
            .saturating_add(row_gap);
        // The trailing side at the anchor's own line first: what a person opening something
        // beside where they are expects to see.
        for (x, y) in [
            (right, i64::from(subject.y)),
            (i64::from(subject.x), below),
            (right, below),
        ] {
            let place = CanvasPlace::new(clamp(x), clamp(y), width, height);
            if free(&place) {
                return place;
            }
        }
        // One row down at a time, along the two columns the anchor starts, until both stand past
        // every obstacle's foot: `step_y` is at least the gap, so the walk always ends.
        let mut drop = step_y;
        while drop <= foot.saturating_sub(below) {
            for (x, y) in [
                (right, i64::from(subject.y).saturating_add(drop)),
                (i64::from(subject.x), below.saturating_add(drop)),
            ] {
                let place = CanvasPlace::new(clamp(x), clamp(y), width, height);
                if free(&place) {
                    return place;
                }
            }
            drop = drop.saturating_add(step_y);
        }
        // Every row the obstacles reach is taken: continue down the trailing column.
        let mut y = below;
        loop {
            let place = CanvasPlace::new(clamp(right), clamp(y), width, height);
            if free(&place) || y == i64::MAX {
                return place;
            }
            y = y.saturating_add(step_y);
        }
    }

    /// Where what an answer shows stands beside its turn `subject`, for a window of `width` ×
    /// `height` (canvas.md §3, §7): in the free spot adjacent to the turn, on its trailing side,
    /// then below it, then on its leading side, then above it. A spot that stands on the screen
    /// (`seen`'s size) together with the turn comes first, made smaller to fit, the one on the
    /// turn's own line first: the camera pans to show both. Failing that, the largest part of the
    /// window that is free beside the turn, never under a window's least size, the spot most
    /// inside what the person sees first: `seen` (the part of the canvas they saw when they
    /// asked), panned the least that shows the turn whole, since the camera keeps the turn. Nothing
    /// placed moves, and every spot is clear of everything but `except`
    /// ([`CanvasPlace::crowds_on_canvas`]). `None` when no spot adjacent to the turn is free: the
    /// caller stands it further along ([`Self::free_place_next_to`]).
    #[must_use]
    pub fn free_place_within(
        &self,
        subject: CanvasPlace,
        (width, height): (u32, u32),
        seen: CanvasPlace,
        except: Option<GroupId>,
    ) -> Option<CanvasPlace> {
        let others: Vec<CanvasPlace> = self
            .places
            .iter()
            .filter(|(id, _)| Some(**id) != except)
            .map(|(_, place)| *place)
            .collect();
        let free = |place: &CanvasPlace| !others.iter().any(|o| place.crowds_on_canvas(o));
        let beside = Beside::new(subject, seen);
        // On the screen with the turn first, shrunk to fit; then as large as is free beside it.
        for on_screen in [true, false] {
            for side in [Side::Trailing, Side::Below, Side::Leading, Side::Above] {
                let Some(asked) = beside.size(side, (width, height), on_screen) else {
                    continue;
                };
                for size in shrinking(asked) {
                    let best = beside
                        .spots(side, size, &others)
                        .filter(|place| free(place) && (!on_screen || beside.fits(place)))
                        .max_by_key(|place| beside.rank(side, place, on_screen));
                    if best.is_some() {
                        return best;
                    }
                }
            }
        }
        None
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
        let held = self.places.len().saturating_add(self.floats.len());
        self.places.retain(|id, _| live(*id));
        self.floats.retain(|id, _| live(*id));
        self.places.len().saturating_add(self.floats.len()) != held
    }

    /// Stand every group `strips` hold that has no place yet, and keep every place that
    /// is there: the framed canvas, the spaces canvas and the traditional desktop draw a
    /// group by its place, so a window the desk opened and nobody placed stands on none of
    /// them, and a desk a person opened an app on looks empty.
    ///
    /// The groups of a strip stand in a row from the strip's corner (`canvas.md` §6), so the
    /// next one goes just past the last one that stands; a strip whose windows none stand yet
    /// starts at the origin, where the first place a person types at also lands. The point an
    /// absent group belongs to is the free space nearest its strip's row, so a window the
    /// person opened is where they look for it. Returns whether any was placed.
    pub fn place_missing(&mut self, strips: &[Strip]) -> bool {
        let gap = i64::from(CANVAS_GAP);
        let step = i64::from(MISSING_PLACE_WIDTH).saturating_add(gap);
        // Where each group that does not stand yet belongs, walking each strip's row from
        // its corner: a strip starts at the origin and each group of it follows the one
        // before, at its right edge, whether that one stands already or is about to.
        let mut wanted: Vec<(GroupId, (i32, i32))> = Vec::new();
        for strip in strips {
            let mut x = 0_i64;
            for group in &strip.window_groups {
                if let Some(place) = self.place_of(group.id) {
                    x = place.right().saturating_add(gap);
                    continue;
                }
                wanted.push((group.id, (clamp_point(x), 0)));
                x = x.saturating_add(step);
            }
        }
        for (group, anchor) in &wanted {
            let place =
                self.free_place_near(*anchor, MISSING_PLACE_WIDTH, MISSING_PLACE_HEIGHT, None);
            self.places.insert(*group, place);
        }
        !wanted.is_empty()
    }
}

impl crate::workspace::Workspace {
    /// Every place stands for a window on the desk: one that left it takes its place and its float
    /// with it, so the next window stands in the room it left instead of beside a hole. Called
    /// wherever a group can go — a close, a command, a desk read back from its record.
    pub fn forget_gone_places(&mut self) {
        let live: std::collections::BTreeSet<GroupId> = self
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .map(|group| group.id)
            .collect();
        self.arrangement
            .retain_groups(|group| live.contains(&group));
    }
}

/// The canvas places of a stored arrangement, each a window's size ([`CanvasPlace::window`]).
fn deserialize_window_places<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<GroupId, CanvasPlace>, D::Error> {
    let places: BTreeMap<GroupId, CanvasPlace> =
        crate::serde_ids::deserialize_id_btree_map(deserializer)?;
    Ok(places
        .into_iter()
        .map(|(group, place)| (group, place.window()))
        .collect())
}

/// A side of a turn an answer's view stands on, in the order they are tried
/// ([`Arrangement::free_place_within`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Trailing,
    Below,
    Leading,
    Above,
}

/// The sizes an answer's view is tried at beside its turn, `asked` first: a room too small for it
/// takes the largest part of it that fits, no smaller than a window.
fn shrinking((width, height): (u32, u32)) -> impl Iterator<Item = (u32, u32)> {
    [(4, 4), (3, 4), (4, 3), (3, 3), (2, 3), (3, 2), (2, 2)]
        .into_iter()
        .map(move |(w, h)| {
            (
                (width.saturating_mul(w) / 4).max(WINDOW_MIN_WIDTH),
                (height.saturating_mul(h) / 4).max(WINDOW_MIN_HEIGHT),
            )
        })
}

/// A turn's place, and what the person sees once it is in view: what an answer's view is placed
/// against ([`Arrangement::free_place_within`]). Canvas pixels, edges exclusive.
struct Beside {
    turn: CanvasPlace,
    /// The view the camera shows the turn in: what the person saw, panned the least that shows
    /// the turn whole with a gap at each edge.
    view: (i64, i64, i64, i64),
    /// How much the screen holds with a gap clear of each edge, its width then its height.
    span: (i64, i64),
}

impl Beside {
    fn new(turn: CanvasPlace, seen: CanvasPlace) -> Self {
        let gap = i64::from(CANVAS_GAP);
        let (x0, y0, x1, y1) = on_screen(&turn);
        let (width, height) = (i64::from(seen.width), i64::from(seen.height));
        // The least pan along one axis that holds `low..high`; one longer than the view shows its
        // start, as the camera does.
        let along = |start: i64, length: i64, low: i64, high: i64| {
            let (low, high) = (low.saturating_sub(gap), high.saturating_add(gap));
            if high.saturating_sub(low) > length || low < start {
                low
            } else if high > start.saturating_add(length) {
                high.saturating_sub(length)
            } else {
                start
            }
        };
        let x = along(i64::from(seen.x), width, x0, x1);
        let y = along(i64::from(seen.y), height, y0, y1);
        Self {
            turn,
            view: (x, y, x.saturating_add(width), y.saturating_add(height)),
            span: (
                width.saturating_sub(gap.saturating_mul(2)),
                height.saturating_sub(gap.saturating_mul(2)),
            ),
        }
    }

    /// The most an answer's view of `asked` size takes on `side`: `asked` itself, or with
    /// `on_screen` the part of it that stands on the screen beside the turn. `None` when not even
    /// a window's least size does.
    fn size(&self, side: Side, (width, height): (u32, u32), on_screen: bool) -> Option<(u32, u32)> {
        if !on_screen {
            return Some((width, height));
        }
        let (band, gap, row_gap) = (
            i64::from(CANVAS_BAND),
            i64::from(CANVAS_GAP),
            i64::from(CANVAS_ROW_GAP),
        );
        let (turn_width, turn_height) = (i64::from(self.turn.width), i64::from(self.turn.height));
        let (most_width, most_height) = match side {
            Side::Trailing | Side::Leading => (
                self.span.0.saturating_sub(turn_width).saturating_sub(gap),
                self.span.1.saturating_sub(band),
            ),
            Side::Below | Side::Above => (
                self.span.0,
                self.span
                    .1
                    .saturating_sub(band)
                    .saturating_sub(turn_height)
                    .saturating_sub(row_gap),
            ),
        };
        let width = i64::from(width).min(most_width);
        let height = i64::from(height).min(most_height);
        let least = width >= i64::from(WINDOW_MIN_WIDTH) && height >= i64::from(WINDOW_MIN_HEIGHT);
        least.then(|| {
            (
                u32::try_from(width).unwrap_or(WINDOW_MIN_WIDTH),
                u32::try_from(height).unwrap_or(WINDOW_MIN_HEIGHT),
            )
        })
    }

    /// The spots of `width` × `height` adjacent to the turn on `side`: standing against that edge
    /// and sharing some of its length, lined up with the turn's ends, the view's edges and each
    /// place's edges, where the free spots nearest each lie.
    fn spots<'a>(
        &'a self,
        side: Side,
        (width, height): (u32, u32),
        others: &'a [CanvasPlace],
    ) -> impl Iterator<Item = CanvasPlace> + 'a {
        let (gap, row_gap, band) = (
            i64::from(CANVAS_GAP),
            i64::from(CANVAS_ROW_GAP),
            i64::from(CANVAS_BAND),
        );
        let (w, h) = (i64::from(width), i64::from(height));
        let turn = self.turn;
        let (view_left, view_top, view_right, view_bottom) = self.view;
        let across = matches!(side, Side::Trailing | Side::Leading);
        // Where it stands against the edge, and where along the edge it may slide.
        let at = match side {
            Side::Trailing => turn.right().saturating_add(gap),
            Side::Leading => i64::from(turn.x).saturating_sub(gap).saturating_sub(w),
            Side::Below => turn.bottom().saturating_add(row_gap),
            Side::Above => i64::from(turn.y).saturating_sub(row_gap).saturating_sub(h),
        };
        let (start, end, length) = if across {
            (i64::from(turn.y), turn.bottom(), h)
        } else {
            (i64::from(turn.x), turn.right(), w)
        };
        let mut along = if across {
            vec![
                start,
                end.saturating_sub(length),
                view_top.saturating_add(band),
                view_bottom.saturating_sub(length),
            ]
        } else {
            vec![
                start,
                end.saturating_sub(length),
                view_left,
                view_right.saturating_sub(length),
            ]
        };
        for o in others {
            along.extend(if across {
                [
                    o.bottom().saturating_add(row_gap),
                    i64::from(o.y)
                        .saturating_sub(row_gap)
                        .saturating_sub(length),
                ]
            } else {
                [
                    o.right().saturating_add(gap),
                    i64::from(o.x).saturating_sub(gap).saturating_sub(length),
                ]
            });
        }
        along
            .into_iter()
            .filter(move |slide| *slide < end && slide.saturating_add(length) > start)
            .map(move |slide| {
                let (x, y) = if across { (at, slide) } else { (slide, at) };
                CanvasPlace::new(clamp_point(x), clamp_point(y), width, height)
            })
    }

    /// Whether `place` and the turn stand on the screen together.
    fn fits(&self, place: &CanvasPlace) -> bool {
        let (ax0, ay0, ax1, ay1) = on_screen(&self.turn);
        let (bx0, by0, bx1, by1) = on_screen(place);
        ax1.max(bx1).saturating_sub(ax0.min(bx0)) <= self.span.0
            && ay1.max(by1).saturating_sub(ay0.min(by0)) <= self.span.1
    }

    /// How `place` ranks among the spots on `side`, higher first: how much of it the person sees
    /// with the turn in view, then how near the turn's own line it stands, then the topmost and
    /// leftmost, so the same canvas gives the same spot on every device. Spots that stand on the
    /// screen together with the turn (`on_screen`) are all seen whole once the camera pans to
    /// both, so only the turn's line ranks them.
    fn rank(
        &self,
        side: Side,
        place: &CanvasPlace,
        on_screen_with_turn: bool,
    ) -> (i64, i64, i64, i64) {
        let (x0, y0, x1, y1) = on_screen(place);
        let (vx0, vy0, vx1, vy1) = self.view;
        let shown = if on_screen_with_turn {
            0
        } else {
            x1.min(vx1)
                .saturating_sub(x0.max(vx0))
                .max(0)
                .saturating_mul(y1.min(vy1).saturating_sub(y0.max(vy0)).max(0))
        };
        let off_line = match side {
            Side::Trailing | Side::Leading => {
                i64::from(place.y).saturating_sub(i64::from(self.turn.y))
            }
            Side::Below | Side::Above => i64::from(place.x).saturating_sub(i64::from(self.turn.x)),
        };
        (
            shown,
            off_line.saturating_abs().saturating_neg(),
            i64::from(place.y).saturating_neg(),
            i64::from(place.x).saturating_neg(),
        )
    }
}

/// What `place` takes on the screen: its view and the band over it, as its leading, top,
/// trailing and bottom edges.
fn on_screen(place: &CanvasPlace) -> (i64, i64, i64, i64) {
    (
        i64::from(place.x),
        i64::from(place.y).saturating_sub(i64::from(CANVAS_BAND)),
        place.right(),
        place.bottom(),
    )
}

/// A canvas coordinate as a place keeps it.
fn clamp_point(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(places: &[(GroupId, CanvasPlace)]) -> Arrangement {
        Arrangement {
            kind: ArrangementKind::Canvas,
            places: places.iter().copied().collect(),
            floats: BTreeMap::new(),
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
        assert_eq!((place.x, place.y), (0, 368));
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
    fn next_to_a_window_walks_right_then_down_and_never_above_it() {
        // The conversation at the origin: the first place opened beside it stands at its right on
        // its own line, the second below it, the third below its right — and none above it or to
        // its left, where a person would not look for what they asked for.
        let conversation = CanvasPlace::new(0, 0, 720, 520);
        let mut arrangement = canvas(&[(1, conversation)]);
        let opened: Vec<CanvasPlace> = (2..=7)
            .map(|id| {
                let place = arrangement.free_place_next_to(conversation, 720, 520, None);
                assert!(!place.crowds(&conversation, CANVAS_GAP), "{place:?}");
                assert!(
                    place.y >= conversation.y,
                    "never above the anchor: {place:?}"
                );
                assert!(
                    place.x >= conversation.x,
                    "never to the anchor's leading side: {place:?}"
                );
                for other in &opened_places(&arrangement) {
                    assert!(
                        !place.crowds(other, CANVAS_GAP),
                        "{place:?} crowds {other:?}"
                    );
                }
                arrangement.places.insert(id, place);
                place
            })
            .collect();
        assert_eq!(
            (opened[0].x, opened[0].y),
            (744, 0),
            "the first stands beside the conversation, at its line"
        );
        assert_eq!((opened[1].x, opened[1].y), (0, 588), "the second below it");
        assert_eq!(
            (opened[2].x, opened[2].y),
            (744, 588),
            "the third under its right"
        );
        let places: Vec<CanvasPlace> = opened_places(&arrangement);
        assert_eq!(places.len(), 7, "every place stands clear of the rest");
    }

    /// A place under another keeps its caption's band clear of the one above (canvas.md §7,
    /// "Captions never hidden"): one over the other they stand the band and the gap apart, side by
    /// side the gap.
    #[test]
    fn a_place_below_another_keeps_its_caption_clear() {
        let above = CanvasPlace::new(0, 0, 400, 300);
        let next = canvas(&[(1, above)]).free_place_next_to(above, 400, 300, None);
        assert_eq!((next.x, next.y), (424, 0), "side by side the gap is enough");
        let crowded = canvas(&[(1, above), (2, CanvasPlace::new(424, 0, 400, 300))]);
        let below = crowded.free_place_next_to(above, 400, 300, None);
        assert_eq!(
            (below.x, below.y),
            (0, 300 + 44 + 24),
            "below, the band and the gap stand between them"
        );
        let near = canvas(&[(1, above)]).free_place_near((0, 310), 400, 300, None);
        assert!(
            !near.crowds_on_canvas(&above),
            "{near:?} keeps its caption clear of {above:?}"
        );
        assert!(
            CanvasPlace::new(0, 330, 400, 300).crowds_on_canvas(&above),
            "a place whose band stands under the one above crowds it"
        );
    }

    /// Whether `a` and `b`, with the bands over them, stand together on a screen of `seen`'s size.
    fn on_one_screen(a: CanvasPlace, b: CanvasPlace, seen: CanvasPlace) -> bool {
        let band = i64::from(CANVAS_BAND);
        let left = i64::from(a.x.min(b.x));
        let top = i64::from(a.y.min(b.y)).saturating_sub(band);
        a.right().max(b.right()).saturating_sub(left) <= i64::from(seen.width)
            && a.bottom().max(b.bottom()).saturating_sub(top) <= i64::from(seen.height)
    }

    /// Beside a turn, an answer's view stands in the free spot adjacent to it, trailing side
    /// first, at the turn's line: in view when the turn is, and past the edge of what the person
    /// sees when that is where the free spot beside the turn is (the camera pans to both), made
    /// smaller so the turn and it stand on the screen together. Nothing adjacent free: nothing.
    #[test]
    fn an_answer_stands_adjacent_to_its_turn() {
        let seen = CanvasPlace::new(0, 0, 1280, 740);
        let turn = CanvasPlace::new(100, 100, 520, 480);
        let place = canvas(&[(1, turn)])
            .free_place_within(turn, (600, 400), seen, None)
            .expect("room beside the turn");
        assert_eq!(
            (place.x, place.y, place.width, place.height),
            (644, 100, 600, 400),
            "the trailing side, at its line, at its size"
        );
        assert!(place.inside(&seen) && !place.crowds_on_canvas(&turn));
        // The turn near the trailing edge: the answer still stands on its trailing side, past
        // what the person saw, and the two fit on one screen for the camera to show.
        let right = CanvasPlace::new(700, 100, 520, 480);
        let place = canvas(&[(1, right)])
            .free_place_within(right, (600, 400), seen, None)
            .expect("room on the trailing side");
        assert_eq!((place.x, place.y), (1244, 100), "trailing, at its line");
        assert!(
            on_one_screen(right, place, seen),
            "{place:?} with {right:?}"
        );
        // A turn too wide for the whole size beside it on one screen: a smaller part of it.
        let wide = CanvasPlace::new(0, 60, 700, 480);
        let place = canvas(&[(1, wide)])
            .free_place_within(wide, (720, 520), seen, None)
            .expect("a smaller room beside it");
        assert_eq!((place.x, place.y), (724, 60), "trailing, at its line");
        assert!(place.width < 720, "made smaller to fit: {place:?}");
        assert!(on_one_screen(wide, place, seen), "{place:?} with {wide:?}");
        // Every side taken: the caller stands it further along.
        let small = CanvasPlace::new(0, 0, 400, 300);
        let ring = canvas(&[
            (1, small),
            (2, CanvasPlace::new(-5000, -5000, 4976, 10000)),
            (3, CanvasPlace::new(448, -5000, 5000, 10000)),
            (4, CanvasPlace::new(-5000, -5000, 10000, 4932)),
            (5, CanvasPlace::new(-5000, 400, 10000, 5000)),
        ]);
        assert_eq!(ring.free_place_within(small, (720, 520), seen, None), None);
    }

    /// A turn asked by a click near the bottom of what the person sees, after another turn and
    /// its answer took the top of the view: the answer stands on the turn's trailing side at
    /// its line, never in the far corner of the view by the first turn, and the turn and it fit
    /// on one screen for the camera to pan to.
    #[test]
    fn an_answer_to_a_turn_near_the_bottom_of_the_view_stands_beside_it() {
        let seen = CanvasPlace::new(0, 0, 1280, 760);
        let first = CanvasPlace::new(26, 38, 358, 240);
        let notes = CanvasPlace::new(408, 44, 720, 520);
        let turn = CanvasPlace::new(640, 662, 520, 480);
        let place = canvas(&[(1, first), (2, notes), (3, turn)])
            .free_place_within(turn, (720, 520), seen, None)
            .expect("room beside the turn");
        assert_eq!(
            (i64::from(place.x), place.y),
            (turn.right().saturating_add(i64::from(CANVAS_GAP)), turn.y),
            "the trailing side, at its line: {place:?}"
        );
        assert!(on_one_screen(turn, place, seen), "{place:?} with {turn:?}");
        for other in [first, notes, turn] {
            assert!(
                !place.crowds_on_canvas(&other),
                "{place:?} clear of {other:?}"
            );
        }
    }

    /// The places standing in `arrangement`, for saying one crowds none.
    fn opened_places(arrangement: &Arrangement) -> Vec<CanvasPlace> {
        arrangement.places.values().copied().collect()
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

    /// A window's place, on the canvas or floating, is never smaller than a window: an agent's
    /// command or a record that asked for a sliver gets a window, and a sliver some older build
    /// left still lays out a window wide.
    #[test]
    fn a_window_is_never_kept_or_laid_out_a_sliver() {
        let area = CanvasPlace::new(0, 32, 1280, 768);
        let sliver = FloatPlace {
            corner: FloatCorner::TopLeft,
            dx: 24,
            dy: 0,
            width: 2,
            height: 2,
        };
        assert_eq!(
            sliver.resolved(area),
            CanvasPlace::new(24, 32, WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT),
            "laid out from the corner it keeps, a window wide"
        );
        let made = FloatPlace::new(FloatCorner::TopLeft, 24, 0, 2, 2);
        assert_eq!(
            (made.width, made.height),
            (WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT)
        );
        let read: FloatPlace =
            serde_json::from_str(r#"{"corner":"top_left","dx":24,"dy":0,"width":2,"height":2}"#)
                .unwrap();
        assert_eq!(read, made, "a stored sliver reads back a window");
        let mut arrangement = Arrangement::default();
        arrangement.set_float(7, Some(sliver));
        assert_eq!(arrangement.float_of(7), Some(made));
        // On a work area smaller than a window, the window fills the area.
        assert_eq!(
            made.resolved(CanvasPlace::new(0, 0, 100, 80)),
            CanvasPlace::new(0, 0, 100, 80)
        );

        let stored = r#"{"kind":"Canvas","places":{"7":{"x":-40,"y":12,"width":2,"height":0}}}"#;
        let arrangement: Arrangement = serde_json::from_str(stored).unwrap();
        assert_eq!(
            arrangement.place_of(7),
            Some(CanvasPlace::new(
                -40,
                12,
                WINDOW_MIN_WIDTH,
                WINDOW_MIN_HEIGHT
            ))
        );
        assert!(
            (f64::from(WINDOW_MIN_WIDTH) - crate::width::WIDTH_FLOOR_PX).abs() < f64::EPSILON,
            "a float or a canvas window is as narrow as a strip's window can get, no narrower"
        );
    }

    #[test]
    fn closing_a_window_frees_its_place() {
        let mut arrangement = canvas(&[(1, CanvasPlace::new(0, 0, 10, 10))]);
        assert!(arrangement.retain_groups(|id| id != 1));
        assert!(arrangement.places.is_empty());
    }

    #[test]
    fn a_place_let_go_elsewhere_on_the_canvas_keeps_its_size_and_moves_nothing_else() {
        // A drag that ended on the canvas: the place stands where it was let go, at the size it
        // had, and every other place stays exactly where it was.
        let mut arrangement = canvas(&[
            (1, CanvasPlace::new(0, 0, 400, 300)),
            (2, CanvasPlace::new(500, 0, 400, 300)),
        ]);
        assert!(arrangement.set_position(2, 900, 640));
        assert_eq!(
            arrangement.place_of(2),
            Some(CanvasPlace::new(900, 640, 400, 300)),
            "the place stands where it was let go, at its own size"
        );
        assert_eq!(
            arrangement.place_of(1),
            Some(CanvasPlace::new(0, 0, 400, 300)),
            "nothing else moved"
        );
        assert!(
            !arrangement.set_position(2, 900, 640),
            "a place already there is not a change"
        );
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
    fn a_workspace_stored_before_floats_reads_with_none() {
        let json = r#"{"kind":"Strips","places":{}}"#;
        let arrangement: Arrangement = serde_json::from_str(json).expect("reads back");
        assert!(arrangement.floats.is_empty());
    }

    #[test]
    fn a_float_is_kept_from_its_corner_and_resolves_on_any_screen() {
        let float = FloatPlace::new(FloatCorner::BottomRight, 40, 24, 640, 480);
        let small = CanvasPlace::new(1280, 0, 1280, 720);
        assert_eq!(float.resolved(small), CanvasPlace::new(1880, 216, 640, 480));
        let large = CanvasPlace::new(0, 0, 2560, 1440);
        assert_eq!(float.resolved(large), CanvasPlace::new(1880, 936, 640, 480));
        // A window larger than the work area fills it rather than hanging off.
        let huge = FloatPlace::new(FloatCorner::TopLeft, 10, 10, 4000, 3000);
        assert_eq!(huge.resolved(small), CanvasPlace::new(1280, 0, 1280, 720));
        let back: FloatPlace =
            serde_json::from_str(&serde_json::to_string(&float).unwrap()).unwrap();
        assert_eq!(back, float);
    }

    #[test]
    fn dropping_a_window_anchors_it_to_the_corner_nearest_where_it_was_let_go() {
        let area = CanvasPlace::new(0, 0, 1600, 900);
        let bottom_right = FloatPlace::of_rect(area, CanvasPlace::new(900, 380, 640, 480));
        assert_eq!(bottom_right.corner, FloatCorner::BottomRight);
        assert_eq!((bottom_right.dx, bottom_right.dy), (60, 40));
        assert_eq!(
            bottom_right.resolved(area),
            CanvasPlace::new(900, 380, 640, 480),
            "the window stays exactly where it was let go"
        );
        let top_left = FloatPlace::of_rect(area, CanvasPlace::new(30, 20, 400, 300));
        assert_eq!(top_left.corner, FloatCorner::TopLeft);
        assert_eq!((top_left.dx, top_left.dy), (30, 20));
    }

    #[test]
    fn floating_and_snapping_back_moves_nothing_else() {
        let place = CanvasPlace::new(40, 40, 400, 300);
        let mut arrangement = canvas(&[(1, place)]);
        let float = FloatPlace::new(FloatCorner::TopRight, 10, 10, 640, 480);
        assert!(arrangement.set_float(1, Some(float)));
        assert!(!arrangement.set_float(1, Some(float)), "nothing changed");
        assert_eq!(arrangement.float_of(1), Some(float));
        assert_eq!(
            arrangement.place_of(1),
            Some(place),
            "its canvas place is kept"
        );
        assert!(arrangement.set_float(1, None));
        assert!(!arrangement.set_float(1, None));
        assert_eq!(arrangement.place_of(1), Some(place), "snap back is exact");
    }

    #[test]
    fn an_unknown_kind_reads_as_strips() {
        assert_eq!(ArrangementKind::parse("canvas"), ArrangementKind::Canvas);
        assert_eq!(ArrangementKind::parse("spiral"), ArrangementKind::Strips);
    }
}
