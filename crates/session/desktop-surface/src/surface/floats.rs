//! Windows floating above the desk: where the workspace's record has them,
//! and what a drag that lifts or snaps one reports.
//!
//! The place itself lives in the workspace's arrangement (`float`, next to
//! the window's place in its strip), so every member sees the same window
//! floating in the same spot. The work area a float is anchored to is this
//! device's, which is why a place keeps its corner and distances rather than
//! a rect: a member with a smaller screen finds it inside their display.

use std::collections::HashMap;

use tilo_desktop_model::{CanvasPlace, DesktopTree, FloatPlace, StripId, Workspace, WorkspaceSlot};
use ui::kurbo::{Point, Rect};
use ui::num::{f64_of_i64, i32_of_f64, u32_of_f64};
use ui::widget::{FLOAT_CARD_ROOM, FLOAT_SIZE, FloatAsk, GroupRow, HANDLE_SLOT_PX, Widget};

use super::Shared;

/// Where `at` stands on the work area, as the record's place.
fn place_of(at: Rect) -> CanvasPlace {
    CanvasPlace::new(
        i32_of_f64(at.x0.round()),
        i32_of_f64(at.y0.round()),
        u32_of_f64(at.width().round()).max(1),
        u32_of_f64(at.height().round()).max(1),
    )
}

/// The work area `area` as the model keeps it.
pub(super) fn area_of(area: Rect) -> CanvasPlace {
    place_of(area)
}

/// The rect `at` the record keeps, on the work area `area` .
pub(super) fn rect_of(at: CanvasPlace) -> Rect {
    Rect::new(
        f64_of_i64(i64::from(at.x)),
        f64_of_i64(i64::from(at.y)),
        f64_of_i64(i64::from(at.x).saturating_add(i64::from(at.width))),
        f64_of_i64(i64::from(at.y).saturating_add(i64::from(at.height))),
    )
}

fn workspace_with(tree: &DesktopTree, strip: StripId) -> Option<&Workspace> {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.strips.iter().any(|held| held.id == strip))
}

/// Whether the workspace `strip` is on holds the window `group` floating.
pub(super) fn holds(tree: &DesktopTree, strip: StripId, group: u64) -> bool {
    workspace_with(tree, strip)
        .is_some_and(|workspace| workspace.arrangement.float_of(group).is_some())
}

/// The windows of the strip `strip` the workspace has floating, each at its rect on the work area
/// `area` .
pub(super) fn floats_of(
    tree: &DesktopTree,
    strip: StripId,
    area: CanvasPlace,
) -> HashMap<u64, Rect> {
    let Some(workspace) = workspace_with(tree, strip) else {
        return HashMap::new();
    };
    workspace
        .strips
        .iter()
        .find(|held| held.id == strip)
        .into_iter()
        .flat_map(|held| &held.window_groups)
        .filter_map(|group| {
            let float = workspace.arrangement.float_of(group.id)?;
            Some((group.id, rect_of(float.resolved(area))))
        })
        .collect()
}

/// The workspace `strip` is on, writable, with the slot it takes.
fn slot_with(tree: &DesktopTree, strip: StripId) -> Option<WorkspaceSlot> {
    Some(
        tree.workspaces
            .iter()
            .find(|workspace| workspace.strips.iter().any(|held| held.id == strip))?
            .slot,
    )
}

fn workspace_mut(tree: &mut DesktopTree, slot: WorkspaceSlot) -> Option<&mut Workspace> {
    tree.workspaces
        .iter_mut()
        .find(|workspace| workspace.slot == slot)
}

/// Keep the float `group` of the workspace `strip` is on as `float` , `None` putting it back in its
/// strip. Returns whether the tree changed.
pub(super) fn set(
    shared: &mut Shared,
    strip: StripId,
    group: u64,
    float: Option<FloatPlace>,
) -> bool {
    let Some(slot) = slot_with(&shared.tree, strip) else {
        return false;
    };
    if float.is_some() {
        shared.tree.active_strip.insert(slot, strip);
    }
    let Some(workspace) = workspace_mut(&mut shared.tree, slot) else {
        return false;
    };
    workspace.arrangement.set_float(group, float)
}

/// The window a floating window opened beside: the one before it in its strip, else the one
/// after it, as the strip shares its room with ([`GroupRow::share_with_floating_neighbour`]).
pub(super) fn opened_beside(row: &GroupRow, group: u64) -> Option<u64> {
    let index = row.groups.iter().position(|held| held.id == group)?;
    let standing = |at: usize| {
        row.groups
            .get(at)
            .filter(|held| !row.is_floating(held.id))
            .map(|held| held.id)
    };
    index
        .checked_sub(1)
        .and_then(standing)
        .or_else(|| standing(index.saturating_add(1)))
}

/// Stand the window `group` of the strip `strip` over the desk beside the window `sender` it
/// opened next to, where that window stands on screen now: after it where the card fits on the
/// desk, else before it, over a window the person was not using, whichever covers less of the one
/// they were reading. The strip shares its room with the card ([`GroupRow::share_with_floating_neighbour`]),
/// so a window that has the desk to itself leaves the card room after it. Returns whether the tree
/// changed.
pub(super) fn stand_beside(
    shared: &mut Shared,
    strip: StripId,
    row: &GroupRow,
    (group, sender): (u64, u64),
    area: CanvasPlace,
) -> bool {
    let Some(beside) = row
        .groups
        .iter()
        .find(|held| held.id == sender)
        .map(Widget::get_rect)
        .filter(|rect| rect.width() > 0.0)
    else {
        return false;
    };
    let gap = HANDLE_SLOT_PX + FLOAT_CARD_ROOM;
    let card = |x: f64| {
        row.keep_float_inside(Rect::from_origin_size(Point::new(x, beside.y0), FLOAT_SIZE))
    };
    let after = card(beside.x1 + gap);
    let before = card(beside.x0 - gap - FLOAT_SIZE.0);
    let covered = |card: Rect| (card.x1.min(beside.x1) - card.x0.max(beside.x0)).max(0.0);
    let at = if covered(before) < covered(after) {
        before
    } else {
        after
    };
    set(
        shared,
        strip,
        group,
        Some(FloatPlace::of_rect(area, place_of(at))),
    )
}

impl super::DesktopSurface {
    /// Stand the float `group` beside the window it opened next to, as the strip has it now, and
    /// again once the reveal brings that window into view whole. Returns that window.
    pub(super) fn open_beside(&mut self, strip: StripId, group: u64) -> Option<u64> {
        let row = self.rows.get(&strip)?;
        let sender = opened_beside(row, group)?;
        let area = area_of(row.get_rect());
        stand_beside(
            &mut self.shared.borrow_mut(),
            strip,
            row,
            (group, sender),
            area,
        );
        self.standing = Some((strip, group, sender));
        Some(sender)
    }
}

/// The float `standing` waits for, beside the window the reveal just brought into view on `row`:
/// it stands beside it where it is on screen now. Returns whether the tree changed.
pub(super) fn stand_revealed(
    standing: &mut Option<(StripId, u64, u64)>,
    shared: &std::cell::RefCell<Shared>,
    strip: StripId,
    row: &GroupRow,
) -> bool {
    let Some((_, float, sender)) = standing.take_if(|(waits, ..)| *waits == strip) else {
        return false;
    };
    let area = area_of(row.get_rect());
    stand_beside(&mut shared.borrow_mut(), strip, row, (float, sender), area)
}

/// Lift the window `group` above the desk where it stands: a chord or a
/// menu row asked, so it takes the floating size over the middle of the
/// place it had in its strip. Returns whether the tree changed.
pub(super) fn lift(
    shared: &mut Shared,
    strip: StripId,
    row: &GroupRow,
    group: u64,
    area: CanvasPlace,
) -> bool {
    let Some(rect) = row
        .groups
        .iter()
        .find(|held| held.id == group)
        .map(Widget::get_rect)
    else {
        return false;
    };
    let at = row.float_rect_at(rect.center());
    set(
        shared,
        strip,
        group,
        Some(FloatPlace::of_rect(area, place_of(at))),
    )
}

/// The float `row` reports let go, kept in the workspace's record: a window let go off the strip
/// floats at the hand; one let go over it snaps back (the row has already moved it to its gap).
/// Returns whether the tree changed.
pub(super) fn carry_out(
    shared: &mut Shared,
    strip: StripId,
    ask: FloatAsk,
    area: CanvasPlace,
) -> bool {
    let group = match ask {
        FloatAsk::Float { group, at } => {
            if !set(
                shared,
                strip,
                group,
                Some(FloatPlace::of_rect(area, place_of(at))),
            ) {
                return false;
            }
            group
        }
        FloatAsk::Snap { group } => {
            if !set(shared, strip, group, None) {
                return false;
            }
            group
        }
    };
    // The window the hand let go of is the one the person works in: it takes the keyboard, floating
    // or back in its strip.
    shared.tree.set_active_group(strip, group);
    true
}
