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
use ui::widget::{FLOAT_CARD_SIZE, FloatAsk, GroupRow, Widget};

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

/// A window floating as a card in the floating layer's column on this device: the layer stacks it
/// with the activities' cards, by the one rule they all stand by, and the desk stands the window
/// where the layer put it ([`super::DesktopHandle::dock_at`]).
#[derive(Clone, Debug, PartialEq)]
pub struct DockedCard {
    /// The window, by its group id.
    pub group: u64,
    /// The size it stands at: the card, or its header alone while collapsed.
    pub size: (f64, f64),
}

/// The windows this device docks in the layer's column, and where the layer last put each. Docking
/// is this device's: the record keeps the window floating, and where on another device's display
/// it stands is that device's column.
#[derive(Default)]
pub(super) struct Dock {
    /// The docked windows, by group, each at its card size.
    pub(super) cards: std::collections::BTreeMap<u64, DockedCard>,
    /// Where the layer put each docked window, in desk coordinates.
    pub(super) slots: std::collections::BTreeMap<u64, Rect>,
    /// The docked windows folded to their header, with the size they open to again.
    pub(super) collapsed: std::collections::BTreeMap<u64, (f64, f64)>,
}

/// The place a card docked in the work area `area` keeps in the record: the layer's corner, until
/// the layer gives it its place in the column — and on a device that does not dock it.
#[must_use]
fn docked_place(area: Rect) -> Rect {
    let (width, height) = FLOAT_CARD_SIZE;
    let inset = ui::floating_layer::EDGE_INSET;
    let height = height.min(area.height() - 2.0 * inset).max(1.0);
    Rect::from_origin_size(
        Point::new(area.x1 - inset - width, area.y1 - inset - height),
        (width, height),
    )
}

impl super::DesktopSurface {
    /// Dock the float `group` of `strip` in the floating layer's column, and return the window it
    /// opened beside: the strip shows that one, clear of the column.
    pub(super) fn dock(&self, strip: StripId, group: u64) -> Option<u64> {
        let row = self.rows.get(&strip)?;
        let sender = opened_beside(row, group);
        let area = area_of(row.get_rect());
        let at = docked_place(row.get_rect());
        let mut shared = self.shared.borrow_mut();
        set(
            &mut shared,
            strip,
            group,
            Some(FloatPlace::of_rect(area, place_of(at))),
        );
        shared.dock.cards.insert(
            group,
            DockedCard {
                group,
                size: (at.width(), at.height()),
            },
        );
        sender
    }
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

/// Fold the docked card `group` to its header, `header` tall, or open it to its size again.
/// Returns whether the dock changed.
fn collapse(dock: &mut Dock, group: u64, header: f64) -> bool {
    let Some(card) = dock.cards.get_mut(&group) else {
        return false;
    };
    if let Some(open) = dock.collapsed.remove(&group) {
        card.size = open;
    } else {
        dock.collapsed.insert(group, card.size);
        card.size = (card.size.0, header);
    }
    true
}

/// The float `row` reports let go, kept in the workspace's record: a window let go off the strip
/// floats at the hand; one let go over it snaps back (the row has already moved it to its gap).
/// Returns whether the tree changed.
pub(super) fn carry_out(
    shared: &mut Shared,
    strip: StripId,
    row: &GroupRow,
    ask: FloatAsk,
) -> bool {
    let area = area_of(row.get_rect());
    let group = match ask {
        // A card folds to its header in the column, and opens from it again.
        FloatAsk::Collapse { group } => {
            // A card folded to its header stands as tall as its window's bar.
            let header = row
                .groups
                .iter()
                .find(|held| held.id == group)
                .map_or(0.0, |held| held.bar_rect().height());
            return collapse(&mut shared.dock, group, header);
        }
        // A card the person carried or snapped is theirs to place: it leaves the column.
        FloatAsk::Float { group, .. } | FloatAsk::Snap { group } => group,
    };
    shared.dock.cards.remove(&group);
    shared.dock.slots.remove(&group);
    shared.dock.collapsed.remove(&group);
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
        FloatAsk::Collapse { group } => group,
    };
    // The window the hand let go of is the one the person works in: it takes the keyboard, floating
    // or back in its strip.
    shared.tree.set_active_group(strip, group);
    true
}
