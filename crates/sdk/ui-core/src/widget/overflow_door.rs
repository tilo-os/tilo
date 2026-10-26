//! The overflow door: the named place a control goes when the row it belongs
//! to has no room for it.
//!
//! A toolbar, a header row or a bar of the app's own holds commands. Windows
//! change size, text changes size, and a row that simply draws fewer controls
//! at a narrow width silently takes capabilities away: the person sees a
//! toolbar that no longer offers Undo, with nothing saying where it went. The
//! rule is the opposite — a control that does not fit becomes a *named* entry
//! behind the row's door ([`OverflowDoor`], the trailing "More actions"
//! button), so every command is reachable at every width, and the door itself
//! is only there when something is behind it.
//!
//! Two pieces make that up:
//!
//! - [`plan`] — how many of a row's items fit beside the door, given their
//!   widths. It never decides to drop one: what does not fit is counted, not
//!   discarded ([`DoorPlan::hidden`]).
//! - [`OverflowDoor`] — the button and the menu of what the plan put behind
//!   it. `NavShell`'s header row uses the same door for actions that do not
//!   fit ([`crate::widget::NavShell`]), so an app's own row and the shell's
//!   header overflow the same way, into the same face.
//!
//! A composed row usually wants the shared [`Toolbar`](crate::widget::Toolbar)
//! instead, which does its own overflow; this door is for the rows a painted
//! app lays out by hand.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::context_menu::{ContextMenu, MenuItem};
use crate::widget::{AppModel, Button, ModelPath, TaffyTreeExt, Widget};

/// How many of a row's items the row has room for, and how many stand behind
/// the door ([`plan`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DoorPlan {
    shown: usize,
    hidden: usize,
}

impl DoorPlan {
    /// How many items the row shows.
    #[must_use]
    pub fn shown(&self) -> usize {
        self.shown
    }

    /// How many items the door holds. Never dropped, never silently absent:
    /// every item not shown is here, and the list is the menu's order.
    #[must_use]
    pub fn hidden(&self) -> usize {
        self.hidden
    }

    /// Whether anything is behind the door (and so whether it shows at all).
    #[must_use]
    pub fn needs_door(&self) -> bool {
        self.hidden > 0
    }

    /// The first `n` items the row shows, of `total`: the ones a caller
    /// lays out in the row. The rest go in the menu, in the same order.
    #[must_use]
    pub fn shown_items(&self) -> std::ops::Range<usize> {
        0..self.shown
    }
}

/// Which of `items` fit in `width`, with `gap` between them and `door` of the
/// row reserved for the door's own button.
///
/// The door is reserved only when at least one item does not fit — a row that
/// holds them all shows no door, so the row is not narrowed for nothing. When
/// not even the door's button fits beside the items, everything stands behind
/// it: a row too narrow for one control still offers that control, named in
/// the menu, rather than drawing nothing.
///
/// `widths` is the natural width of each item, in the row's order. They are
/// taken in order: a row does not reorder its commands to fit more of them,
/// because the order is what makes them findable.
#[must_use]
pub fn plan(width: f64, widths: impl IntoIterator<Item = f64>, gap: f64, door: f64) -> DoorPlan {
    let widths: Vec<f64> = widths.into_iter().collect();
    let total = widths.len();
    let gap = gap.max(0.0);
    let mut used = 0.0;
    let mut shown = 0;
    for (index, item) in widths.iter().enumerate() {
        let next = if index == 0 {
            *item
        } else {
            used + gap + *item
        };
        if next <= width {
            used = next;
            shown = index.saturating_add(1);
        } else {
            break;
        }
    }
    if shown == total {
        return DoorPlan { shown, hidden: 0 };
    }
    // Something did not fit: the door takes its own place in the row, and the
    // items it holds are the ones after the last that fits beside it.
    let room = (width - door).max(0.0);
    let mut used = 0.0;
    let mut beside = 0;
    for (index, item) in widths.iter().enumerate() {
        let next = if index == 0 {
            *item
        } else {
            used + gap + *item
        };
        if next <= room {
            used = next;
            beside = index.saturating_add(1);
        } else {
            break;
        }
    }
    DoorPlan {
        shown: beside,
        hidden: total.saturating_sub(beside),
    }
}

/// The trailing "More actions" door: a button holding the menu of what did
/// not fit.
///
/// The host lays it out where the row ends and hides it
/// ([`Self::set_shown`]) when nothing is behind it. A press opens the menu of
/// the items it was given ([`Self::open`]); the row the person chose comes
/// back once, by index, in the order the items were given
/// ([`Self::take_choice`]).
pub struct OverflowDoor {
    button: Button,
    menu: ContextMenu,
    requested: Rc<Cell<bool>>,
    chosen: Option<usize>,
    rect: Rect,
    viewport: Rect,
    /// Some items did not fit: the button stands in the row. Otherwise it is
    /// laid out at nothing and the host meets no button.
    shown: bool,
}

impl OverflowDoor {
    /// A shut door with `id` on its button, named "More actions".
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self::with_label(id, "More actions")
    }

    /// [`Self::new`] with the name assistive technology reads, and the
    /// tooltip and overflow row show.
    #[must_use]
    pub fn with_label(id: &str, label: &str) -> Self {
        let requested = Rc::new(Cell::new(false));
        let button = Button::icon_only(id, Icon::MoreHorizontal, label).on_view({
            let requested = Rc::clone(&requested);
            move || requested.set(true)
        });
        Self {
            button,
            menu: ContextMenu::new(),
            requested,
            chosen: None,
            rect: Rect::ZERO,
            viewport: Rect::ZERO,
            shown: false,
        }
    }

    /// Whether anything is behind the door: it shows, and takes its place in
    /// the row. `false` closes it and lays the button out at nothing.
    pub fn set_shown(&mut self, shown: bool) {
        self.shown = shown;
        if !shown {
            self.menu.close();
        }
    }

    /// Where the door's button stands from the last layout.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// The button was pressed since the last call.
    pub fn take_open_request(&self) -> bool {
        self.requested.replace(false)
    }

    /// Open the menu with `items` — one per control that did not fit, in the
    /// row's order — anchored at the button, inside `viewport`.
    pub fn open(&mut self, items: Vec<MenuItem>, viewport: Rect) {
        self.menu = ContextMenu::new();
        for item in items {
            self.menu.push(item);
        }
        self.viewport = viewport;
        self.menu.open_anchored(self.rect, viewport);
    }

    /// Put the menu away.
    pub fn close(&mut self) {
        self.menu.close();
    }

    /// Whether the menu is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// The row the person chose in the menu, once.
    pub fn take_choice(&mut self) -> Option<usize> {
        self.chosen.take()
    }

    /// The menu's labels, in order, while it is open: what the person can
    /// reach behind the door.
    #[must_use]
    pub fn menu_labels(&self) -> Vec<String> {
        if !self.menu.is_open() {
            return Vec::new();
        }
        self.menu
            .items()
            .iter()
            .filter_map(|item| item.label().map(str::to_owned))
            .collect()
    }

    /// Choose `row`, as a click on it does. For a test (a capture, a drive)
    /// that drives the door without a pointer.
    pub fn choose(&mut self, row: usize) {
        self.menu.close();
        self.chosen = Some(row);
    }
}

impl Widget for OverflowDoor {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.button.build_layout(taffy, theme, node_map);
        taffy.widget_node(Style::default(), &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Some(child) = taffy.children(node).ok().and_then(|c| c.first().copied()) {
            self.button
                .apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
        if self.menu.is_open() {
            self.menu.reanchor(self.rect, self.viewport);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.menu.is_open() {
            self.menu.layout_within(text, theme);
        }
        self.button.draw(scene, transform, text, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.menu
            .draw_overlay(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.menu.is_open() {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
            if let Some(row) = self.menu.take_selection() {
                self.chosen = Some(row);
                *ctx.handled = true;
            }
            return;
        }
        let child = ctx
            .taffy
            .children(ctx.node_id)
            .ok()
            .and_then(|c| c.first().copied());
        if let Some(child) = child {
            let path = ctx.path;
            self.button
                .handle_event(event, &mut ctx.reborrow(path, child));
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.button.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.button.update(state, path)
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.menu.modal_capture()
    }

    fn traps_focus(&self) -> bool {
        self.menu.is_open()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_open()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.shown {
            vec![&self.button]
        } else {
            Vec::new()
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.shown {
            visitor(&self.button);
        }
    }

    /// Laid out also while hidden: the row that hides the door still has to
    /// know where the next control after it would stand.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&self.button);
        true
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.shown {
            vec![&mut self.button]
        } else {
            Vec::new()
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.shown && self.button.is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.shown && self.button.layout_needs_measure()
    }

    fn name(&self) -> &'static str {
        "OverflowDoor"
    }

    /// Its button's, while it shows, and every frame its menu is open.
    fn needs_redraw(&self) -> bool {
        self.menu.is_open() || (self.shown && Widget::needs_redraw(&self.button))
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_that_holds_them_all_shows_no_door() {
        let plan = plan(400.0, [40.0, 40.0, 40.0], 8.0, 40.0);
        assert_eq!(plan.shown(), 3);
        assert_eq!(plan.hidden(), 0);
        assert!(!plan.needs_door(), "nothing is behind it");
        assert_eq!(plan.shown_items(), 0..3);
    }

    #[test]
    fn what_does_not_fit_is_named_behind_the_door_not_dropped() {
        // Four 40 px controls and their gaps want 184 px; a 170 px row has
        // room for two of them beside the door's own 40 px, so the last two
        // move behind it.
        let plan = plan(170.0, [40.0, 40.0, 40.0, 40.0], 8.0, 40.0);
        assert_eq!(plan.shown(), 2, "two fit beside the door's place");
        assert_eq!(plan.hidden(), 2);
        assert!(plan.needs_door());
        assert_eq!(
            plan.shown() + plan.hidden(),
            4,
            "every control is either in the row or behind the door"
        );
    }

    #[test]
    fn a_row_too_narrow_for_one_control_still_offers_it() {
        let plan = plan(30.0, [40.0, 40.0], 8.0, 40.0);
        assert_eq!(plan.shown(), 0);
        assert_eq!(plan.hidden(), 2, "both stand behind the door");
        assert!(plan.needs_door());
    }

    #[test]
    fn the_door_holds_the_ones_after_the_last_that_fits() {
        // 40 + 8 + 40 + 8 + 40 = 136 fits in 200 - 40 = 160; the fourth does not.
        let plan = plan(200.0, [40.0, 40.0, 40.0, 60.0], 8.0, 40.0);
        assert_eq!(plan.shown(), 3);
        assert_eq!(plan.hidden(), 1);
    }

    #[test]
    fn an_empty_row_has_no_door() {
        let plan = plan(0.0, [], 8.0, 40.0);
        assert_eq!(plan.shown(), 0);
        assert_eq!(plan.hidden(), 0);
        assert!(!plan.needs_door());
    }
}
