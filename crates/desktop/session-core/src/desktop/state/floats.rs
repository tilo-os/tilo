//! Windows floating above the shell: where the workspace's record has them,
//! and what a drag that lifts or snaps one reports.
//!
//! The place itself lives in the workspace's layout (`float`, next to
//! the window's place in its strip), so every member sees the same window
//! floating in the same spot. The work area a float is anchored to is this
//! device's, which is why a place keeps its corner and distances rather than
//! a rect: a member with a smaller screen finds it inside their display.

use std::collections::HashMap;

use tilo_desktop_model::{CanvasPlace, FloatPlace, Layout, StripId, TabId, Workspace};
use ui::kurbo::{Point, Rect};
use ui::num::{f64_of_i64, i32_of_f64, u32_of_f64};
use ui::widget::FLOAT_CARD_SIZE;

use super::Shared;

/// Where `at` stands on the work area, as the record's place.
pub fn place_of(at: Rect) -> CanvasPlace {
    CanvasPlace::new(
        i32_of_f64(at.x0.round()),
        i32_of_f64(at.y0.round()),
        u32_of_f64(at.width().round()).max(1),
        u32_of_f64(at.height().round()).max(1),
    )
}

/// The work area `area` as the model keeps it.
pub fn area_of(area: Rect) -> CanvasPlace {
    place_of(area)
}

/// The rect `at` the record keeps, on the work area `area` .
pub fn rect_of(at: CanvasPlace) -> Rect {
    Rect::new(
        f64_of_i64(i64::from(at.x)),
        f64_of_i64(i64::from(at.y)),
        f64_of_i64(i64::from(at.x).saturating_add(i64::from(at.width))),
        f64_of_i64(i64::from(at.y).saturating_add(i64::from(at.height))),
    )
}

pub fn workspace_with(tree: &Layout, strip: StripId) -> Option<&Workspace> {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.strips.iter().any(|held| held.id == strip))
}

/// A window the shell floated or put back in its strip: the person asked for it, or the window
/// opened as a card. The host makes it an arranging action of the workspace's layout
/// ([`super::DesktopHandle::take_asked_floats`]); until then the shell shows the window as asked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AskedFloat {
    /// The window in front of the group, which the action names.
    pub tab: TabId,
    pub group: u64,
    /// Where it floats; `None` puts it back in its strip.
    pub float: Option<FloatPlace>,
}

impl super::DesktopHandle {
    /// The windows the person floated or put back since the last call, in the order asked.
    pub fn take_asked_floats(&self) -> Vec<AskedFloat> {
        std::mem::take(&mut self.shared.borrow_mut().asked_floats)
    }
}

#[cfg(any(test, debug_assertions))]
impl super::DesktopHandle {
    /// Make the floats the shell asked for as the session's layout makes them: the workspace's
    /// record holds each window where it was asked to float.
    pub fn arrange_asked_floats(&self) {
        let asked = self.take_asked_floats();
        let mut shared = self.shared.borrow_mut();
        for asked in asked {
            for workspace in &mut shared.tree.workspaces {
                if workspace.tabs().any(|tab| tab.id == asked.tab) {
                    workspace.layout.set_float(asked.group, asked.float);
                }
            }
        }
        shared.replaced = true;
        shared.changed();
    }
}

impl Shared {
    /// Where the window `group` of the workspace `strip` is on floats: as the person last asked,
    /// else as the workspace holds it.
    fn float_of(&self, strip: StripId, group: u64) -> Option<FloatPlace> {
        match self
            .asked_floats
            .iter()
            .rev()
            .find(|asked| asked.group == group)
        {
            Some(asked) => asked.float,
            None => workspace_with(&self.tree, strip)?.layout.float_of(group),
        }
    }
}

/// Whether the workspace `strip` is on holds the window `group` floating.
pub fn holds(shared: &Shared, strip: StripId, group: u64) -> bool {
    shared.float_of(strip, group).is_some()
}

/// The windows of the strip `strip` the workspace has floating, each at its rect on the work area
/// `area` .
pub fn floats_of(shared: &Shared, strip: StripId, area: CanvasPlace) -> HashMap<u64, Rect> {
    workspace_with(&shared.tree, strip)
        .and_then(|workspace| workspace.strips.iter().find(|held| held.id == strip))
        .into_iter()
        .flat_map(|held| &held.window_groups)
        .filter_map(|group| {
            let float = shared.float_of(strip, group.id)?;
            Some((group.id, rect_of(float.resolved(area))))
        })
        .collect()
}

/// Ask for the float `group` of the workspace `strip` is on to be `float` , `None` putting it back
/// in its strip: an arranging action the host makes. Returns whether that changes where it stands.
pub fn set(shared: &mut Shared, strip: StripId, group: u64, float: Option<FloatPlace>) -> bool {
    let Some(workspace_id) = workspace_with(&shared.tree, strip).map(Workspace::id) else {
        return false;
    };
    let Some(tab) = workspace_with(&shared.tree, strip)
        .and_then(|workspace| workspace.strips.iter().find(|held| held.id == strip))
        .and_then(|held| held.window_groups.iter().find(|held| held.id == group))
        .map(|held| held.active_tab)
    else {
        return false;
    };
    if float.is_some() {
        shared.tree.active_strip.insert(workspace_id, strip);
    }
    if shared.float_of(strip, group) == float {
        return false;
    }
    shared.asked_floats.push(AskedFloat { tab, group, float });
    shared.changed();
    true
}

/// A window floating as a card in the floating layer's column on this device: the layer stacks it
/// with the activities' cards, by the one rule they all stand by, and the shell stands the window
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
pub struct Dock {
    /// The docked windows, by group, each at its card size.
    pub cards: std::collections::BTreeMap<u64, DockedCard>,
    /// Where the layer put each docked window, in shell coordinates.
    pub places: std::collections::BTreeMap<u64, Rect>,
    /// The docked windows folded to their header, with the size they open to again.
    pub collapsed: std::collections::BTreeMap<u64, (f64, f64)>,
    /// The windows that opened here as cards: snapped into their strip and floated again, they go
    /// back to the column as the card they were, not over the shell at a window's size.
    pub born: std::collections::BTreeSet<u64>,
}

/// The place a card docked in the work area `area` keeps in the record: the layer's corner, until
/// the layer gives it its place in the column — and on a device that does not dock it.
#[must_use]
pub fn docked_place(area: Rect) -> Rect {
    let (width, height) = FLOAT_CARD_SIZE;
    let inset = ui::floating_layer::EDGE_INSET;
    let height = height.min(area.height() - 2.0 * inset).max(1.0);
    Rect::from_origin_size(
        Point::new(area.x1 - inset - width, area.y1 - inset - height),
        (width, height),
    )
}

/// Fold the docked card `group` to its header, `header` tall, or open it to its size again.
/// Returns whether the dock changed.
pub fn collapse(dock: &mut Dock, group: u64, header: f64) -> bool {
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
