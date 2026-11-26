//! Windows floating above the shell: where the workspace's record has them,
//! and the press that carries one, which snaps it back into its strip when it lets go there.
//!
//! The place itself lives in the workspace's layout (`float`, next to
//! the window's place in its strip), so every member sees the same window
//! floating in the same spot. The work area a float is anchored to is this
//! device's, which is why a place keeps its corner and distances rather than
//! a rect: a member with a smaller screen finds it inside their display.

use std::collections::HashMap;

use tilo_desktop_model::commands::{DesktopCommand, apply_command};
use tilo_desktop_model::{CanvasPlace, FloatPlace, GroupId, Layout, StripId, TabId, Workspace};
use tilo_ui_base::{num::f64_of_i64, num::i32_of_f64, num::u32_of_f64};
use ui::floating_layer::FLOAT_CARD_SIZE;
use ui::kurbo::{Point, Rect};

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
    /// Where in its strip it goes back, when the person let it go in a gap there: that strip, and
    /// the window it stands before (by the tab in front of it), or `None` for after the last.
    /// `None` puts it back where it stands in its strip.
    pub into: Option<(StripId, Option<TabId>)>,
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
            // The move into a gap was made on the tree when the window was let go there.
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
/// `area` . A float the person carries stands where their hand has it, not where the record keeps
/// it.
pub fn floats_of(shared: &Shared, strip: StripId, area: CanvasPlace) -> HashMap<u64, Rect> {
    let carried = shared.carried.filter(|carried| carried.strip == strip);
    workspace_with(&shared.tree, strip)
        .and_then(|workspace| workspace.strips.iter().find(|held| held.id == strip))
        .into_iter()
        .flat_map(|held| &held.window_groups)
        .filter_map(|group| {
            let float = shared.float_of(strip, group.id)?;
            let at = match carried {
                Some(carried) if carried.group == group.id => carried.at,
                _ => rect_of(float.resolved(area)),
            };
            Some((group.id, at))
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
    shared.asked_floats.push(AskedFloat {
        tab,
        group,
        float,
        into: None,
    });
    shared.changed();
    true
}

/// The place a window opened over the shell as a card keeps in the record: the work area's
/// bottom-right corner, by an inset from its edges.
#[must_use]
pub fn card_place(area: Rect) -> Rect {
    let (width, height) = FLOAT_CARD_SIZE;
    let inset = ui::floating_layer::EDGE_INSET;
    let height = height.min(area.height() - 2.0 * inset).max(1.0);
    Rect::from_origin_size(
        Point::new(area.x1 - inset - width, area.y1 - inset - height),
        (width, height),
    )
}

/// Open the window `group` of `strip` over the shell as a card at the work area `area`'s corner,
/// beside the window it opened next to: that window gives up the room the card stands in
/// ([`tilo_desktop_model::width::share_for_float`]), so the card stands beside what the person was
/// reading rather than over it. The record keeps where the card stands as it opens, so every
/// member finds it there, not only the host that turns the ask into an arranging action. Returns
/// the window it opened beside, which the strip brings into view.
pub fn open_as_card(shared: &mut Shared, strip: StripId, group: u64, area: Rect) -> Option<u64> {
    let beside = share_with_card(shared, strip, group, area);
    let at = card_place(area);
    shared.cards_born.insert(group);
    let place = FloatPlace::of_rect(area_of(area), place_of(at));
    set(shared, strip, group, Some(place));
    if let Some(workspace) = shared
        .tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.strips.iter().any(|held| held.id == strip))
        && workspace.layout.set_float(group, Some(place))
    {
        shared.changed();
    }
    beside
}

/// Give the card of `group` its room beside the window before it on its display, else the one
/// after it: the window it opened next to. Returns that window, when the strip has room for it
/// beside the card.
fn share_with_card(shared: &mut Shared, strip: StripId, group: u64, area: Rect) -> Option<u64> {
    let display = shared.tree.outputs.display_of(group);
    let floating: std::collections::BTreeSet<u64> = workspace_with(&shared.tree, strip)?
        .strips
        .iter()
        .find(|held| held.id == strip)?
        .window_groups
        .iter()
        .filter(|held| holds(shared, strip, held.id))
        .map(|held| held.id)
        .collect();
    let partition = shared.tree.outputs.clone();
    let held = shared
        .tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .find(|held| held.id == strip)?;
    let index = held
        .window_groups
        .iter()
        .position(|held| held.id == group)?;
    let beside = |at: usize| {
        held.window_groups.get(at).is_some_and(|next_to| {
            !next_to.maximized
                && !floating.contains(&next_to.id)
                && partition.display_of(next_to.id) == display
        })
    };
    let neighbour = match index.checked_sub(1) {
        Some(left) if beside(left) => left,
        _ if beside(index.saturating_add(1)) => index.saturating_add(1),
        _ => return None,
    };
    let usable = (area.width() - 2.0 * ui::widget::STRIP_EDGE_GUTTER_PX).max(1.0);
    let (beside_width, card_width) = tilo_desktop_model::width::share_for_float(
        super::shown::float_card_room(),
        usable,
        ui::widget::HANDLE_SLOT_PX,
    )?;
    let mut changed = false;
    for (at, width) in [(neighbour, beside_width), (index, card_width)] {
        if let Some(sized) = held.window_groups.get_mut(at)
            && sized.width != width
        {
            sized.width = width;
            changed = true;
        }
    }
    let id = held.window_groups.get(neighbour).map(|held| held.id);
    if changed {
        shared.changed();
    }
    id
}

/// A floating window the person carries with a press: where the hand took it, where the window
/// stood then and where it stands now, and the strip and work area it floats over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Carried {
    pub strip: StripId,
    pub group: GroupId,
    pub tab: TabId,
    /// Where the press took it, in the surface's coordinates.
    pub grab: Point,
    /// The whole window, its bar and its content, where it stood when the press took it.
    pub from: Rect,
    /// The whole window where the hand has it now.
    pub at: Rect,
    /// The work area of the display it floats on.
    pub area: Rect,
}

impl super::DesktopHandle {
    /// A press at `point` (the surface's coordinates) takes the floating window there, by any point
    /// of it: the window follows the press ([`Self::carry_to`]) until it is let go
    /// ([`Self::put_down`]). Whether a floating window stands there to carry.
    pub fn carry_float_at(&self, point: Point) -> bool {
        self.carry_float(point, |shown| shown.window.contains(point))
    }

    /// The floating window showing `tab` , taken by its bar where the press at `grab` (the
    /// surface's coordinates) began: what a person's lift of that bar carries. Whether it floats.
    pub fn carry_float_by_bar(&self, tab: TabId, grab: Point) -> bool {
        self.carry_float(grab, |shown| shown.tab == tab)
    }

    /// The floating window `taken` answers for, carried from `grab`.
    fn carry_float(&self, grab: Point, taken: impl Fn(&super::shown::ShownTab) -> bool) -> bool {
        let mut shared = self.shared.borrow_mut();
        let found = shared
            .shown_tabs
            .0
            .iter()
            .rev()
            .find(|shown| shown.floated && taken(shown));
        let Some(&super::shown::ShownTab {
            tab, group, window, ..
        }) = found
        else {
            return false;
        };
        let Some((strip, area)) = super::shown::section_holding(&shared, group) else {
            return false;
        };
        shared.carried = Some(Carried {
            strip,
            group,
            tab,
            grab,
            from: window,
            at: window,
            area,
        });
        true
    }

    /// The window the person carries now, by the tab in front of it.
    pub fn carried_tab(&self) -> Option<TabId> {
        self.shared.borrow().carried.map(|carried| carried.tab)
    }

    /// Each floating window the shell draws, by its group, and where it stands on the display it
    /// floats over, from that display's top-left corner: where the record keeps it, or where the
    /// person's hand has it while they carry it.
    pub fn floats_drawn(&self) -> Vec<(GroupId, Rect)> {
        let shared = self.shared.borrow();
        shared
            .shown_tabs
            .0
            .iter()
            .filter(|shown| shown.floated)
            .filter_map(|shown| {
                let (_, area) = super::shown::section_holding(&shared, shown.group)?;
                let (left, top, window) = (area.x0, area.y0, shown.window);
                let on_display = Rect::new(
                    window.x0 - left,
                    window.y0 - top,
                    window.x1 - left,
                    window.y1 - top,
                );
                Some((shown.group, on_display))
            })
            .collect()
    }

    /// The hand carrying a floating window moved to `point`: the window follows it, keeping the
    /// hold it was taken by.
    pub fn carry_to(&self, point: Point) {
        let mut shared = self.shared.borrow_mut();
        let Some(carried) = shared.carried.as_mut() else {
            return;
        };
        let (dx, dy) = (point.x - carried.grab.x, point.y - carried.grab.y);
        let from = carried.from;
        let at = Rect::new(from.x0 + dx, from.y0 + dy, from.x1 + dx, from.y1 + dy);
        if at != carried.at {
            carried.at = at;
            shared.wake.wake();
        }
    }

    /// The hand carrying a floating window let it go at `point`. Over the bars of its strip's
    /// windows it snaps back into the strip, as its own window in the gap it was let go in;
    /// anywhere else it floats where it was let go, kept on the display. Either is the person's
    /// arranging action ([`Self::take_asked_floats`]). Whether a window was carried.
    pub fn put_down(&self, point: Point) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(carried) = shared.carried.take() else {
            return false;
        };
        if let Some(before) = gap_at(&shared, &carried, point) {
            snap_into(&mut shared, &carried, before);
            return true;
        }
        let at = ui::floating_layer::keep_float_inside(carried.area, carried.at);
        let place = FloatPlace::of_rect(area_of(carried.area), place_of(at));
        if !set(&mut shared, carried.strip, carried.group, Some(place)) {
            // Let go where it was: the window goes back where the record keeps it.
            shared.wake.wake();
        }
        true
    }
}

/// Where in its strip the carried window lands when let go at `point`: before the first of the
/// strip's other windows whose middle is right of the hand (`Some(Some(group))`), after the last
/// (`Some(None)`), or `None` when the hand is not over the strip's bars.
#[expect(
    clippy::option_option,
    reason = "outer: over the strip or not; inner: before a window or after the last"
)]
fn gap_at(shared: &Shared, carried: &Carried, point: Point) -> Option<Option<GroupId>> {
    let groups: Vec<GroupId> = workspace_with(&shared.tree, carried.strip)?
        .strips
        .iter()
        .find(|held| held.id == carried.strip)?
        .window_groups
        .iter()
        .map(|held| held.id)
        .filter(|id| *id != carried.group)
        .collect();
    let laid: Vec<&super::shown::ShownTab> = groups
        .iter()
        .filter_map(|id| {
            shared
                .shown_tabs
                .0
                .iter()
                .find(|shown| shown.group == *id && !shown.floated)
        })
        .collect();
    let over_bars = laid.iter().any(|shown| {
        let bar_bottom = shown.content.map_or(shown.window.y1, |content| content.y0);
        point.y >= shown.window.y0 && point.y < bar_bottom
    });
    if !over_bars || point.x < carried.area.x0 || point.x >= carried.area.x1 {
        return None;
    }
    Some(
        laid.iter()
            .find(|shown| point.x < shown.window.center().x)
            .map(|shown| shown.group),
    )
}

/// Snap the carried window back into its strip, as its own window before `before` (or last): the
/// tree moves it at once, so the strip shows it there, and the record makes the same move as the
/// person's one arranging action.
fn snap_into(shared: &mut Shared, carried: &Carried, before: Option<GroupId>) {
    let before_tab = before.and_then(|before| {
        workspace_with(&shared.tree, carried.strip)?
            .strips
            .iter()
            .flat_map(|held| &held.window_groups)
            .find(|held| held.id == before)
            .map(|held| held.active_tab)
    });
    let Shared { tree, minter, .. } = shared;
    if let Some(workspace) = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.strips.iter().any(|held| held.id == carried.strip))
    {
        let moved = apply_command(
            workspace,
            &DesktopCommand::MoveGroup {
                group: carried.group,
                strip: carried.strip.0,
                before,
            },
            &mut || Ok(minter.mint()),
            &mut |_, _| Err("a move opens no window".into()),
        );
        if let Err(why) = moved {
            tracing::warn!(why, "the window let go in its strip could not move there");
        }
    }
    shared.tree.set_active_group(carried.strip, carried.group);
    shared.asked_floats.push(AskedFloat {
        tab: carried.tab,
        group: carried.group,
        float: None,
        into: Some((carried.strip, before_tab)),
    });
    shared.changed();
}
