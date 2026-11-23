//! Where a place stands when it names no point but something to stand beside: next to that
//! thing's place, in the free space nearest it (`shell.md` §3, "a place next to where it came
//! from"). An agent's `place` and `open` name the conversation of its turn, so what it shows stands
//! beside the conversation that asked (§8.1); a place's `position` names the place it moves next to.

use serde_json::Value as Json;
use tilo_actions::Actor;

use tilo_desktop_model::CanvasPlace;

use super::turns::CanvasRect;
use super::{
    DEFAULT_HEIGHT, DEFAULT_WIDTH, Item, Layout, LayoutAction, LayoutActions, clamp_side, locate,
};

/// What an answer's view stands beside: a turn's place, or the rectangle a turn will take
/// before its view stands.
#[derive(Clone, Copy, Debug)]
pub(super) enum Beside {
    Place(u64),
    Asked(CanvasRect),
}

/// `rect` as a canvas place, its coordinates kept to what a place holds.
pub(super) fn canvas_place(rect: CanvasRect) -> CanvasPlace {
    let at = |value: i64| {
        i32::try_from(value.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or_default()
    };
    let side = |value: i64| u32::try_from(value.clamp(0, i64::from(u32::MAX))).unwrap_or_default();
    CanvasPlace::new(at(rect.x), at(rect.y), side(rect.width), side(rect.height))
}

impl Layout {
    /// The free spot adjacent to `subject` (trailing, then below, then leading, then above) for a
    /// window of the default size, or the part of it that stands on the screen with `subject` or
    /// is free beside it (`free_place_within`), judged against `seen` (the part of the canvas the
    /// person saw when they asked): where an answer's view stands beside its turn, so the camera
    /// pans to show both (canvas.md §3, §7). `None` when no spot adjacent to `subject` is free,
    /// or `subject` stands on no canvas.
    pub(super) fn room_in_view(&self, subject: Beside, seen: CanvasRect) -> Option<CanvasPlace> {
        let seen = canvas_place(seen);
        let size = (clamp_side(DEFAULT_WIDTH), clamp_side(DEFAULT_HEIGHT));
        self.handle.read(|tree| match subject {
            Beside::Place(anchor) => {
                let (workspace, row, group, _) = locate(tree, anchor)?;
                let held = tree.workspaces.get(workspace)?;
                let id = held.strips.get(row)?.window_groups.get(group)?.id;
                let place = held.layout.place_of(id)?;
                held.layout.free_place_within(place, size, seen, None)
            }
            Beside::Asked(rect) => {
                let held = tree.active_workspace()?;
                held.layout
                    .free_place_within(canvas_place(rect), size, seen, None)
            }
        })
    }

    /// The place `beside` names: a place of the layout by its own resource
    /// (`shell:place:<key>`), or the place showing that record, or anything of its root. `None`
    /// for empty text or a resource no place shows.
    pub(super) fn anchor_of(&self, beside: &str) -> Option<u64> {
        let beside = beside.trim();
        if beside.is_empty() {
            return None;
        }
        Item::Place
            .key(beside)
            // ignored-failure: a key that is not a number names no place, and the record's are next
            .and_then(|key| key.parse().ok())
            .filter(|place| self.handle.read(|tree| locate(tree, *place).is_some()))
            .or_else(|| self.place_of(beside))
            .or_else(|| self.place_of_root(beside))
    }

    /// The free canvas point next to the place `anchor`, in its workspace, for a window of the
    /// size of the place `moving` (the default size for a new one): to its trailing side first,
    /// then below (`free_place_next_to`). The place `moving` itself is no obstacle, so a place
    /// moved beside another may stand where it partly stood. `None` when `anchor` stands on no
    /// canvas.
    pub(super) fn point_beside(&self, anchor: u64, moving: Option<u64>) -> Option<(i64, i64)> {
        self.handle.read(|tree| {
            let group_of = |place: u64| {
                let (workspace, row, group, _) = locate(tree, place)?;
                let id = tree
                    .workspaces
                    .get(workspace)?
                    .strips
                    .get(row)?
                    .window_groups
                    .get(group)?
                    .id;
                Some((workspace, id))
            };
            let (workspace, anchor) = group_of(anchor)?;
            let held = tree.workspaces.get(workspace)?;
            let subject = held.layout.place_of(anchor)?;
            let moved = moving
                .and_then(group_of)
                .filter(|(of, _)| *of == workspace)
                .map(|(_, group)| group);
            let (width, height) = moved
                .and_then(|group| held.layout.place_of(group))
                .map_or_else(
                    || (clamp_side(DEFAULT_WIDTH), clamp_side(DEFAULT_HEIGHT)),
                    |own| (own.width, own.height),
                );
            let free = held
                .layout
                .free_place_next_to(subject, width, height, moved);
            Some((i64::from(free.x), i64::from(free.y)))
        })
    }

    /// The free canvas point next to `rect` in the workspace in front, for a new window of the
    /// default size: where a step of a turn stands what it shows beside where the turn will stand,
    /// before its view does. `None` without a workspace in front.
    pub(super) fn point_beside_rect(&self, rect: CanvasRect) -> Option<(i64, i64)> {
        let subject = CanvasPlace {
            width: clamp_side(rect.width),
            height: clamp_side(rect.height),
            ..canvas_place(rect)
        };
        self.handle.read(|tree| {
            let held = tree.active_workspace()?;
            let free = held.layout.free_place_next_to(
                subject,
                clamp_side(DEFAULT_WIDTH),
                clamp_side(DEFAULT_HEIGHT),
                None,
            );
            Some((i64::from(free.x), i64::from(free.y)))
        })
    }
}

impl LayoutActions {
    /// Stand what a `place` shows beside what it names, when it names no point, strip or float
    /// but a resource to stand beside: the system's `open` beside it, which stands it in the free
    /// space next to that resource's place, or brings forward the place already showing it.
    /// Whether it did; `false` leaves the place to stand where it names.
    pub(super) fn placed_beside(
        &self,
        asked: &Actor,
        arguments: &serde_json::Map<String, Json>,
        action: &LayoutAction,
    ) -> Result<bool, tilo_actions::ApiError> {
        let LayoutAction::Place { resource, view, .. } = action else {
            return Ok(false);
        };
        // A turn's conversation, opened from the turn with no point of its own, stands beside the
        // turn (canvas.md §3).
        let beside = match place_beside(arguments) {
            Some(beside) => beside.to_owned(),
            None if !pointed(arguments) => {
                let conversation = self.layout.borrow().conversation_opened_as(resource);
                let Some(conversation) = conversation else {
                    return Ok(false);
                };
                conversation
            }
            None => return Ok(false),
        };
        self.layout
            .borrow_mut()
            .open_place(asked, resource, view, &beside)
            .map_err(|why| tilo_actions::ApiError::Unavailable {
                code: "nowhere_to_place".to_owned(),
                reason: why,
            })?;
        Ok(true)
    }

    /// The point a `position` stands its place at: the `x` and `y` it names, or — naming neither —
    /// the free space beside the place or resource its `beside` names. One that names neither a
    /// point nor anything to stand beside is refused rather than sent to the canvas' origin.
    pub(super) fn position_beside(
        &self,
        arguments: &serde_json::Map<String, Json>,
        place: u64,
        x: &mut i64,
        y: &mut i64,
    ) -> Result<(), tilo_actions::ApiError> {
        let named = |name: &str| arguments.get(name).is_some_and(|value| !value.is_null());
        if named("x") && named("y") {
            return Ok(());
        }
        let beside = arguments
            .get("beside")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|beside| !beside.is_empty())
            .ok_or_else(|| {
                tilo_actions::ApiError::InvalidArguments(
                    "a place stands at an x and a y, or beside something".to_owned(),
                )
            })?;
        let layout = self.layout.borrow();
        let anchor =
            layout
                .anchor_of(beside)
                .ok_or_else(|| tilo_actions::ApiError::Unavailable {
                    code: "nowhere_to_place".to_owned(),
                    reason: format!("nothing on the canvas shows {beside}"),
                })?;
        if anchor == place {
            return Err(tilo_actions::ApiError::InvalidArguments(
                "a place cannot stand beside itself".to_owned(),
            ));
        }
        let (at_x, at_y) = layout.point_beside(anchor, Some(place)).ok_or_else(|| {
            tilo_actions::ApiError::Unavailable {
                code: "nowhere_to_place".to_owned(),
                reason: format!("{beside} stands on no canvas"),
            }
        })?;
        *x = at_x;
        *y = at_y;
        Ok(())
    }
}

/// The resource a `place` names to stand beside, when it names no point, strip, float or row of
/// its own: where it stands is then next to that resource's place.
fn place_beside(arguments: &serde_json::Map<String, Json>) -> Option<&str> {
    if pointed(arguments) {
        return None;
    }
    arguments
        .get("beside")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|beside| !beside.is_empty())
}

/// Whether a `place` names where it stands: a point, a strip, a float, a workspace or a row.
fn pointed(arguments: &serde_json::Map<String, Json>) -> bool {
    ["x", "y", "strip", "float", "workspace", "row"]
        .iter()
        .any(|name| arguments.get(*name).is_some_and(|value| !value.is_null()))
}
