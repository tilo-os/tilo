//! The app's slots in a window's bar ([`crate::window_toolbar`]): back and
//! forward, the location, and the actions, laid out across one row and
//! drawn with the window's own icon button
//! ([`super::window_controls::paint_icon_button`]). At most
//! [`MAX_ACTIONS`](crate::window_toolbar::MAX_ACTIONS) actions stand in the
//! row, fewer when the location would be squeezed; the rest are in the menu
//! of a trailing overflow button. Every button names its outcome in a
//! tooltip ([`ToolbarRow::tooltip_at`]).
//!
//! The location is text at rest, or a breadcrumb of its levels
//! ([`super::breadcrumb::Breadcrumb`]) where the app declared them; a click
//! on a level commits it. An editable location reads as a field, and a
//! click (or [`ToolbarRow::begin_edit`], the bar's Ctrl+L) turns it into a
//! real [`TextInput`] holding the whole value, all of it selected, with a go
//! button at its end ([`AccessoryAction::Submit`]). Enter or the go button
//! commits the typed text to the app ([`ToolbarEvent::Commit`]); Esc puts the
//! location back. While it is edited the app hears the text
//! ([`ToolbarEvent::Edit`]) and its suggestions are listed under the field;
//! the arrow keys move through them and Enter or a click commits one.
//!
//! The overflow menu and the suggestions are the shared
//! [`ContextMenu`], painted in the window's top layer
//! ([`ToolbarRow::paint_popups`]).
//!
//! When the window shows another tab, the row's content cross-fades in place
//! ([`crate::motion::CrossFade`]): the outgoing tab's slots fade out where
//! they stood while the incoming ones fade in.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::Widget;
use super::breadcrumb::Breadcrumb;
use super::context_menu::{ContextMenu, MenuItem};
use super::text_input::{AccessoryAction, FieldFace, TextInput};
use super::window_controls::{ButtonFace, CONTROL_GAP, CONTROL_PX, paint_icon_button};
use crate::icons::Icon;
use crate::motion::CrossFade;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::tokens::{MotionSource, TextRole};
use crate::window_toolbar::{ToolbarEvent, WindowToolbar};

/// Action targets of the slots, as assistive technology and automation
/// address them.
pub const SIDEBAR_TARGET: &str = "window-toolbar-sidebar";
pub const BACK_TARGET: &str = "window-toolbar-back";
pub const FORWARD_TARGET: &str = "window-toolbar-forward";
pub const LOCATION_TARGET: &str = "window-toolbar-location";
pub const ACTION_TARGET_PREFIX: &str = "window-toolbar-action:";
pub const OVERFLOW_TARGET: &str = "window-toolbar-more";
pub const SEGMENT_TARGET_PREFIX: &str = "window-toolbar-level:";

/// Between the navigation buttons and the location, and between the
/// location and the actions.
const SECTION_GAP: f64 = 8.0;
/// The location's leading icon.
const LOCATION_ICON_PX: f64 = 16.0;
/// A location narrower than this is not shown: the bar keeps its buttons.
const MIN_LOCATION_PX: f64 = 48.0;
/// Actions move into the overflow menu, last first, while the location
/// would be narrower than this.
const COMFORTABLE_LOCATION_PX: f64 = 200.0;

/// One slot of the row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Sidebar,
    Back,
    Forward,
    Location,
    Action(usize),
    /// The button whose menu holds the actions the row has no room for.
    Overflow,
}

/// Where each slot of one toolbar sits.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Slots {
    pub sidebar: Rect,
    pub back: Rect,
    pub forward: Rect,
    pub location: Rect,
    /// The actions standing in the row, the first of the declared ones.
    pub actions: Vec<Rect>,
    /// The overflow button (`Rect::ZERO`: every action is in the row).
    pub overflow: Rect,
}

impl Slots {
    /// Lay `toolbar`'s slots out across `row`, centred on its centre line:
    /// navigation leading, actions trailing, the location filling between.
    /// At most [`crate::window_toolbar::MAX_ACTIONS`] actions stand in the
    /// row; while the location would be narrower than
    /// [`COMFORTABLE_LOCATION_PX`] the last of them move into the overflow
    /// menu, as do the declared actions past that many.
    pub(crate) fn lay_out(toolbar: &WindowToolbar, row: Rect) -> Self {
        if row.width() <= 0.0 || row.height() <= 0.0 {
            return Self::default();
        }
        let declared = toolbar.actions.len();
        let mut inline = declared.min(crate::window_toolbar::MAX_ACTIONS);
        loop {
            let slots = Self::with_inline(toolbar, row, inline);
            let room = if toolbar.location.is_some() {
                slots.location.width() >= COMFORTABLE_LOCATION_PX
            } else {
                slots.fits(row)
            };
            if room || inline == 0 {
                return slots;
            }
            inline -= 1;
        }
    }

    /// The slots with the first `inline` actions in the row and the rest in
    /// the overflow menu.
    fn with_inline(toolbar: &WindowToolbar, row: Rect, inline: usize) -> Self {
        let mut slots = Self::default();
        let y0 = row.center().y - CONTROL_PX / 2.0;
        let square = |x0: f64| Rect::new(x0, y0, x0 + CONTROL_PX, y0 + CONTROL_PX);
        let mut x0 = row.x0;
        if toolbar.sidebar.is_some() {
            slots.sidebar = square(x0);
            x0 = slots.sidebar.x1 + SECTION_GAP;
        }
        if toolbar.navigation.is_some() {
            slots.back = square(x0);
            slots.forward = square(slots.back.x1 + CONTROL_GAP);
            x0 = slots.forward.x1 + SECTION_GAP;
        }
        let mut x1 = row.x1;
        let mut trailing = false;
        if toolbar.actions.len() > inline {
            slots.overflow = Rect::new(x1 - CONTROL_PX, y0, x1, y0 + CONTROL_PX);
            x1 = slots.overflow.x0 - CONTROL_GAP;
            trailing = true;
        }
        let mut actions = Vec::with_capacity(inline);
        for _ in 0..inline {
            let rect = Rect::new(x1 - CONTROL_PX, y0, x1, y0 + CONTROL_PX);
            x1 = rect.x0 - CONTROL_GAP;
            actions.push(rect);
            trailing = true;
        }
        actions.reverse();
        slots.actions = actions;
        if trailing {
            x1 = x1 + CONTROL_GAP - SECTION_GAP;
        }
        if toolbar.location.is_some() && x1 - x0 >= MIN_LOCATION_PX {
            slots.location = Rect::new(x0, y0, x1, y0 + CONTROL_PX);
        }
        slots
    }

    /// Every button stands inside `row`, clear of the ones before it.
    fn fits(&self, row: Rect) -> bool {
        let lead = self.forward.x1.max(self.sidebar.x1).max(row.x0);
        self.actions
            .iter()
            .chain((self.overflow.area() > 0.0).then_some(&self.overflow))
            .all(|rect| rect.x0 >= lead)
    }

    fn hit(&self, p: Point) -> Option<Slot> {
        if self.sidebar.contains(p) {
            return Some(Slot::Sidebar);
        }
        if self.back.contains(p) {
            return Some(Slot::Back);
        }
        if self.forward.contains(p) {
            return Some(Slot::Forward);
        }
        if let Some(i) = self.actions.iter().position(|r| r.contains(p)) {
            return Some(Slot::Action(i));
        }
        if self.overflow.contains(p) {
            return Some(Slot::Overflow);
        }
        self.location.contains(p).then_some(Slot::Location)
    }
}

/// The slots of the tab the window shows, in one row of its bar.
pub(crate) struct ToolbarRow {
    /// The tab whose slots show, and its toolbar.
    shown: Option<(u64, WindowToolbar)>,
    /// The tab whose slots show (`None`: none), cross-fading on a switch.
    fade: CrossFade<Option<u64>>,
    /// The outgoing tab's slots and where they stood, while they fade out.
    outgoing: Option<(WindowToolbar, Slots)>,
    row: Rect,
    slots: Slots,
    hover: Option<Slot>,
    pressed: Option<Slot>,
    /// The location field, while the location is edited.
    field: Option<TextInput>,
    /// The field's text the app last heard ([`ToolbarEvent::Edit`]).
    heard: String,
    /// The location's levels as a breadcrumb, while it has them, and the
    /// labels it was built from.
    crumbs: Option<(Vec<String>, Breadcrumb)>,
    /// The overflow button's menu, and the declared action each row runs.
    menu: ContextMenu,
    menu_actions: Vec<usize>,
    /// The suggestions under the edited field, and what each commits.
    suggestions: ContextMenu,
    suggestion_values: Vec<String>,
    /// The person closed the list (a press off it): it stays closed until
    /// the app suggests something else.
    suggestions_dismissed: bool,
    /// What the menus keep inside: the window.
    popup_area: Rect,
    events: Vec<(u64, ToolbarEvent)>,
    /// What the edited field holds, for the app's suggestions: drafts, not
    /// things the person did, so they go out apart from [`Self::events`].
    drafts: Vec<(u64, ToolbarEvent)>,
}

impl Default for ToolbarRow {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolbarRow {
    pub(crate) fn new() -> Self {
        Self {
            shown: None,
            fade: CrossFade::new(None),
            outgoing: None,
            row: Rect::ZERO,
            slots: Slots::default(),
            hover: None,
            pressed: None,
            field: None,
            heard: String::new(),
            crumbs: None,
            menu: ContextMenu::new(),
            menu_actions: Vec::new(),
            suggestions: ContextMenu::new(),
            suggestion_values: Vec::new(),
            suggestions_dismissed: false,
            popup_area: Rect::ZERO,
            events: Vec::new(),
            drafts: Vec::new(),
        }
    }

    /// Show `tab`'s slots (`toolbar`), cross-fading from another tab's.
    pub(crate) fn show(
        &mut self,
        tab: u64,
        toolbar: Option<&WindowToolbar>,
        theme_motion: crate::tokens::EffectiveMotion,
        now: Instant,
    ) {
        let next = toolbar.map(|toolbar| (tab, toolbar.clone()));
        let next_tab = next.as_ref().map(|(tab, _)| *tab);
        let previous_tab = self.shown.as_ref().map(|(tab, _)| *tab);
        if next_tab != previous_tab {
            // Another tab: what the person typed belongs to the tab they
            // typed it in, and goes with it.
            self.end_edit();
            self.menu.close();
            self.hover = None;
            self.pressed = None;
            self.outgoing = self
                .shown
                .take()
                .map(|(_, toolbar)| (toolbar, self.slots.clone()));
            self.fade
                .show(next_tab, theme_motion, MotionSource::Pointer, now);
        }
        self.shown = next;
        self.sync_crumbs();
        self.sync_suggestions();
    }

    /// The breadcrumb follows the location's levels.
    fn sync_crumbs(&mut self) {
        let labels: Vec<String> = self
            .toolbar()
            .and_then(|toolbar| toolbar.location.as_ref())
            .map(|location| {
                location
                    .segments
                    .iter()
                    .map(|segment| segment.label.clone())
                    .collect()
            })
            .unwrap_or_default();
        if labels.is_empty() {
            self.crumbs = None;
        } else if self
            .crumbs
            .as_ref()
            .is_none_or(|(built, _)| *built != labels)
        {
            self.crumbs = Some((
                labels.clone(),
                Breadcrumb::new(labels).with_role(TextRole::Body),
            ));
        }
    }

    /// The list under the field follows the app's latest suggestions.
    fn sync_suggestions(&mut self) {
        let suggestions = self
            .toolbar()
            .and_then(|toolbar| toolbar.location.as_ref())
            .map(|location| location.suggestions.clone())
            .unwrap_or_default();
        let values: Vec<String> = suggestions.iter().map(|s| s.value.clone()).collect();
        if self.field.is_none() || suggestions.is_empty() {
            self.suggestions.close();
            self.suggestion_values.clear();
            self.suggestions_dismissed = false;
            return;
        }
        if values == self.suggestion_values {
            return;
        }
        self.suggestions_dismissed = false;
        let items = suggestions
            .iter()
            .map(|suggestion| {
                let item = MenuItem::selection(suggestion.label.clone());
                match suggestion.icon.as_deref().and_then(Icon::from_name) {
                    Some(icon) => item.with_icon(icon),
                    None => item,
                }
            })
            .collect();
        self.suggestions.set_items(items);
        self.suggestion_values = values;
        if !self.suggestions.is_open() {
            self.suggestions
                .open_anchored(self.slots.location, self.popup_area);
        }
    }

    /// What the menus keep inside: the window the bar belongs to.
    pub(crate) fn set_popup_area(&mut self, area: Rect) {
        self.popup_area = area;
    }

    /// The toolbar showing, if any.
    pub(crate) fn toolbar(&self) -> Option<&WindowToolbar> {
        self.shown.as_ref().map(|(_, toolbar)| toolbar)
    }

    /// Lay the slots out across `row`.
    pub(crate) fn lay_out(&mut self, row: Rect) {
        self.row = row;
        self.slots = self
            .toolbar()
            .map(|toolbar| Slots::lay_out(toolbar, row))
            .unwrap_or_default();
        let location = self.slots.location;
        if let Some(field) = self.field.as_mut() {
            if location.area() > 0.0 {
                field.apply_layout_external(location);
                if self.suggestions.is_open() {
                    self.suggestions.reanchor(location, self.popup_area);
                }
            } else {
                // No room for the field any more.
                self.end_edit();
            }
        }
        if self.slots.overflow.area() <= 0.0 {
            self.menu.close();
        } else if self.menu.is_open() {
            self.menu.reanchor(self.slots.overflow, self.popup_area);
        }
    }

    /// The field goes, and the suggestions with it.
    fn end_edit(&mut self) {
        self.field = None;
        self.heard.clear();
        self.suggestions.close();
        self.suggestion_values.clear();
        self.suggestions_dismissed = false;
    }

    pub(crate) fn slots(&self) -> &Slots {
        &self.slots
    }

    pub(crate) fn row(&self) -> Rect {
        self.row
    }

    /// Whether the location is being edited.
    pub(crate) fn is_editing(&self) -> bool {
        self.field.is_some()
    }

    /// The breadcrumb level a click at `p` commits, for tests.
    #[cfg(test)]
    pub(crate) fn crumb_at_for_tests(&self, p: Point) -> Option<String> {
        self.crumb_at(p)
    }

    /// The location field's text, while it is edited.
    #[cfg(test)]
    pub(crate) fn field_text(&self) -> Option<&str> {
        self.field.as_ref().map(TextInput::value)
    }

    /// Turn an editable location into a field holding its whole value, all
    /// of it selected. Returns whether it did.
    pub(crate) fn begin_edit(&mut self) -> bool {
        if self.field.is_some() {
            return true;
        }
        let Some(location) = self
            .toolbar()
            .and_then(|toolbar| toolbar.location.as_ref())
            .filter(|location| location.editable)
        else {
            return false;
        };
        if self.slots.location.area() <= 0.0 {
            return false;
        }
        let go_name = if location.go_name.is_empty() {
            crate::i18n::I18nManager::lookup_plain("window-toolbar-go")
        } else {
            location.go_name.clone()
        };
        let mut field = TextInput::local()
            .with_spellcheck(false)
            .with_autocorrect(false)
            .with_initial_value(location.edit_value())
            .with_placeholder(location.placeholder.clone())
            .with_accessible_name(location.name.clone())
            .with_enter_submits(true)
            .with_trailing_accessory(Icon::ArrowRight, &go_name, AccessoryAction::Submit);
        field.apply_layout_external(self.slots.location);
        field.set_focus(true);
        field.select_all();
        field.value().clone_into(&mut self.heard);
        self.field = Some(field);
        self.menu.close();
        // Suggestions the app made for an earlier edit are not for this
        // one: the list opens when the app answers what this edit starts
        // from, or what is typed next.
        self.suggestion_values = self
            .toolbar()
            .and_then(|toolbar| toolbar.location.as_ref())
            .map(|location| {
                location
                    .suggestions
                    .iter()
                    .map(|s| s.value.clone())
                    .collect()
            })
            .unwrap_or_default();
        let text = self.heard.clone();
        self.draft(text);
        true
    }

    /// Put the location back, discarding what was typed.
    pub(crate) fn cancel_edit(&mut self) -> bool {
        let editing = self.field.is_some();
        self.end_edit();
        editing
    }

    fn shown_tab(&self) -> Option<u64> {
        self.shown.as_ref().map(|(tab, _)| *tab)
    }

    fn push(&mut self, event: ToolbarEvent) {
        if let Some(tab) = self.shown_tab() {
            self.events.push((tab, event));
        }
    }

    /// Commit the field's text to the app.
    fn commit(&mut self) {
        if let Some(field) = self.field.as_ref() {
            let text = field.value().trim().to_owned();
            self.end_edit();
            if !text.is_empty() {
                self.push(ToolbarEvent::Commit { text });
            }
        }
    }

    /// Commit suggestion `index`'s value, as choosing it does.
    fn commit_suggestion(&mut self, index: usize) -> bool {
        let Some(value) = self.suggestion_values.get(index).cloned() else {
            return false;
        };
        self.end_edit();
        self.push(ToolbarEvent::Commit { text: value });
        true
    }

    /// Tell the app the field's text when it changed since it last heard.
    fn tell_edit(&mut self) {
        let Some(text) = self.field.as_ref().map(|field| field.value().to_owned()) else {
            return;
        };
        if text != self.heard {
            self.heard.clone_from(&text);
            self.draft(text);
        }
    }

    /// Queue the edited field's text for the app (one per tab: only the
    /// latest matters).
    fn draft(&mut self, text: String) {
        if let Some(tab) = self.shown_tab() {
            self.drafts.retain(|(queued, _)| *queued != tab);
            self.drafts.push((tab, ToolbarEvent::Edit { text }));
        }
    }

    /// What the edited fields held since the last call, by tab
    /// ([`ToolbarEvent::Edit`]).
    pub(crate) fn take_drafts(&mut self) -> Vec<(u64, ToolbarEvent)> {
        std::mem::take(&mut self.drafts)
    }

    /// What the person did since the last call, by tab.
    pub(crate) fn take_events(&mut self) -> Vec<(u64, ToolbarEvent)> {
        std::mem::take(&mut self.events)
    }

    fn enabled(&self, slot: &Slot) -> bool {
        let Some(toolbar) = self.toolbar() else {
            return false;
        };
        match slot {
            Slot::Sidebar => toolbar.sidebar.is_some(),
            Slot::Back => toolbar.navigation.is_some_and(|nav| nav.back),
            Slot::Forward => toolbar.navigation.is_some_and(|nav| nav.forward),
            Slot::Location => toolbar.location.is_some(),
            Slot::Action(i) => toolbar.actions.get(*i).is_some_and(|a| a.enabled),
            Slot::Overflow => self.slots.actions.len() < toolbar.actions.len(),
        }
    }

    /// Open the overflow button's menu: the actions the row has no room
    /// for, by name, each with its icon and state.
    fn open_overflow(&mut self) -> bool {
        let Some(toolbar) = self.toolbar() else {
            return false;
        };
        let shown = self.slots.actions.len();
        let hidden: Vec<usize> = (shown..toolbar.actions.len()).collect();
        if hidden.is_empty() {
            return false;
        }
        let items = hidden
            .iter()
            .filter_map(|i| toolbar.actions.get(*i))
            .map(|action| {
                let mut item =
                    MenuItem::selection(action.name.clone()).with_enabled(action.enabled);
                if let Some(icon) = Icon::from_name(&action.icon) {
                    item = item.with_icon(icon);
                }
                if action.on {
                    item = item.with_checked(true);
                }
                item
            })
            .collect();
        self.menu.set_items(items);
        self.menu.set_align(super::PanelAlign::Trailing);
        self.menu
            .open_anchored(self.slots.overflow, self.popup_area);
        self.menu_actions = hidden;
        true
    }

    /// Run declared action `index`.
    fn run_action(&mut self, index: usize) -> bool {
        let Some(action) = self
            .toolbar()
            .and_then(|toolbar| toolbar.actions.get(index))
            .filter(|action| action.enabled)
        else {
            return false;
        };
        let id = action.id.clone();
        self.push(ToolbarEvent::Action { id });
        true
    }

    /// Run `slot` as a press on it does. Returns whether it acted.
    fn activate(&mut self, slot: &Slot) -> bool {
        if !self.enabled(slot) {
            return false;
        }
        match slot {
            Slot::Sidebar => self.push(ToolbarEvent::ToggleSidebar),
            Slot::Back => self.push(ToolbarEvent::Back),
            Slot::Forward => self.push(ToolbarEvent::Forward),
            Slot::Location => return self.begin_edit(),
            Slot::Action(i) => return self.run_action(*i),
            Slot::Overflow => return self.open_overflow(),
        }
        true
    }

    /// The pointer moved to `p`. Returns whether what shows changed.
    pub(crate) fn pointer_moved(&mut self, p: Point) -> bool {
        let hover = self.slots.hit(p);
        let changed = hover != self.hover;
        self.hover = hover;
        let on_location = self.field.is_none() && self.slots.location.contains(p);
        let crumbs = self
            .crumbs
            .as_mut()
            .is_some_and(|(_, crumbs)| crumbs.hover_at(on_location.then_some(p)));
        changed || crumbs
    }

    pub(crate) fn pointer_left(&mut self) -> bool {
        let changed = self.hover.is_some() || self.pressed.is_some();
        self.hover = None;
        self.pressed = None;
        if let Some((_, crumbs)) = self.crumbs.as_mut() {
            crumbs.hover_at(None);
        }
        changed
    }

    /// What the button at `p` does, for its tooltip: the slot and its name.
    pub(crate) fn tooltip_at(&self, p: Point) -> Option<(Rect, String)> {
        let toolbar = self.toolbar()?;
        let lookup = crate::i18n::I18nManager::lookup_plain;
        match self.slots.hit(p)? {
            Slot::Sidebar => Some((self.slots.sidebar, lookup(sidebar_key(toolbar)))),
            Slot::Back => Some((self.slots.back, lookup("window-toolbar-back"))),
            Slot::Forward => Some((self.slots.forward, lookup("window-toolbar-forward"))),
            Slot::Action(i) => {
                let action = toolbar.actions.get(i)?;
                Some((*self.slots.actions.get(i)?, action.name.clone()))
            }
            Slot::Overflow => Some((self.slots.overflow, lookup("window-toolbar-more"))),
            Slot::Location => None,
        }
    }

    /// Whether `p` is on the location or the suggestions under it: a press
    /// there keeps the edit.
    pub(crate) fn location_hit(&self, p: Point) -> bool {
        self.slots.location.contains(p)
            || (self.suggestions.is_open() && self.suggestions.contains([p.x, p.y]))
    }

    /// Whether one of the row's menus is open.
    pub(crate) fn popup_open(&self) -> bool {
        self.menu.is_open() || self.suggestions.is_open()
    }

    /// Hand a pointer event to an open menu (the overflow's, or the
    /// suggestions'): a pick runs, a press off it closes it. Returns
    /// whether the menu took the event.
    pub(crate) fn popup_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let pointer = matches!(
            event,
            WindowEvent::MouseInput { .. }
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_)
        );
        if self.menu.is_open() {
            let mut handled = false;
            self.menu.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    handled: &mut handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if let Some(row) = self.menu.take_selection() {
                if let Some(index) = self.menu_actions.get(row).copied() {
                    self.run_action(index);
                }
                self.menu.close();
            }
            // The menu is modal while it is up: the keyboard is its, and so
            // is the press that closes it.
            if handled || !pointer {
                *ctx.handled = true;
                return true;
            }
            return false;
        }
        if self.suggestions.is_open() && pointer {
            let over = match event {
                WindowEvent::CursorMoved { position, .. } => {
                    let p = ctx.logical(position);
                    self.suggestions.contains([p.x, p.y])
                }
                _ => true,
            };
            let mut handled = false;
            self.suggestions.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    handled: &mut handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if let Some(row) = self.suggestions.take_selection() {
                self.commit_suggestion(row);
                *ctx.handled = true;
                return true;
            }
            if !self.suggestions.is_open() {
                // A press off the list closed it; the press goes on, to the
                // field or to whatever ends the edit.
                self.suggestions_dismissed = true;
                return false;
            }
            if handled && over {
                *ctx.handled = true;
                return true;
            }
        }
        false
    }

    /// Whether `p` is on one of the slots.
    pub(crate) fn hit(&self, p: Point) -> bool {
        self.slots.hit(p).is_some()
    }

    /// A left press at `p`. Returns whether a slot took it. A press
    /// outside the field while it is edited puts the location back.
    pub(crate) fn press(&mut self, p: Point) -> bool {
        let slot = self.slots.hit(p);
        if self.field.is_some() && slot != Some(Slot::Location) {
            self.end_edit();
        }
        match slot {
            Some(Slot::Location) if self.field.is_none() => {
                // A click on one of the location's levels goes there.
                if let Some(value) = self.crumb_at(p) {
                    self.push(ToolbarEvent::Commit { text: value });
                    return true;
                }
                // Anywhere else on it starts editing at once: the field
                // takes the press's place, with everything selected.
                self.begin_edit()
            }
            Some(slot) if slot != Slot::Location => {
                self.pressed = Some(slot);
                true
            }
            // The edited field takes its own presses; off the slots, none.
            _ => false,
        }
    }

    /// The left button came up at `p`: a press released on the slot it
    /// started on runs it.
    pub(crate) fn release(&mut self, p: Point) -> bool {
        let Some(pressed) = self.pressed.take() else {
            return false;
        };
        if self.slots.hit(p).as_ref() == Some(&pressed) {
            self.activate(&pressed);
        }
        true
    }

    /// Hand an event to the location field while it is edited. Enter and
    /// the go button commit, Esc puts the location back. Returns whether the
    /// field took it.
    pub(crate) fn field_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        if self.field.is_none() {
            return false;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.end_edit();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp)
                    if self.suggestions.is_open() =>
                {
                    let mut handled = false;
                    self.suggestions.handle_event(
                        event,
                        &mut crate::widget::EventCtx {
                            handled: &mut handled,
                            ..ctx.reborrow(ctx.path, ctx.node_id)
                        },
                    );
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) if self.suggestions.is_open() => {
                    if let Some(row) = self.suggestions.hovered()
                        && self.commit_suggestion(row)
                    {
                        *ctx.handled = true;
                        return true;
                    }
                }
                _ => {}
            }
        }
        let Some(field) = self.field.as_mut() else {
            return false;
        };
        let mut handled = false;
        field.handle_event(
            event,
            &mut crate::widget::EventCtx {
                handled: &mut handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if field.take_submit() {
            self.commit();
            handled = true;
        } else {
            self.tell_edit();
        }
        // The field keeps the keyboard while it is edited: no key reaches
        // the tabs behind it.
        let key = matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        );
        if handled || key {
            *ctx.handled = true;
        }
        handled || key
    }

    /// Whether a frame must follow: the cross-fade, the field's caret, or
    /// a menu arriving or leaving.
    pub(crate) fn needs_animation_frame(&self, now: Instant) -> bool {
        self.fade.is_moving(now)
            || self
                .field
                .as_ref()
                .is_some_and(Widget::needs_animation_frame)
            || self.menu.needs_animation_frame()
            || self.suggestions.needs_animation_frame()
    }

    /// The level of the location's breadcrumb at `p`, as its value: an
    /// ancestor, not the current level.
    fn crumb_at(&self, p: Point) -> Option<String> {
        let (_, crumbs) = self.crumbs.as_ref()?;
        let index = crumbs.ancestor_at(p)?;
        self.toolbar()?
            .location
            .as_ref()?
            .segments
            .get(index)
            .map(|segment| segment.value.clone())
    }

    /// Paint the row's menus, in the window's top layer.
    pub(crate) fn paint_popups(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        Widget::draw_overlay(&mut self.menu, scene, transform, tr, theme, presence);
        Widget::draw_overlay(&mut self.suggestions, scene, transform, tr, theme, presence);
    }

    /// Paint the row.
    pub(crate) fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let now = crate::time::now();
        if let Some((outgoing, opacity)) = self.fade.outgoing(now)
            && outgoing.is_some()
            && let Some((toolbar, slots)) = self.outgoing.as_ref()
        {
            layered(scene, transform, self.row, opacity, |scene| {
                paint_slots(
                    scene,
                    transform,
                    (tr, theme, presence),
                    toolbar,
                    slots,
                    (None, None),
                );
            });
        } else if !self.fade.is_moving(now) {
            self.outgoing = None;
        }
        let incoming = self.fade.incoming(now);
        let Some((_, toolbar)) = self.shown.as_ref() else {
            return;
        };
        let interaction = Interaction {
            hover: self.hover.clone(),
            pressed: self.pressed.clone(),
            editing: self.field.is_some(),
            menu_open: self.menu.is_open(),
        };
        let row = self.row;
        let slots = &self.slots;
        let field = self.field.as_mut();
        let crumbs = self.crumbs.as_mut().map(|(_, crumbs)| crumbs);
        layered(scene, transform, row, incoming, |scene| {
            paint_slots(
                scene,
                transform,
                (tr, theme, presence),
                toolbar,
                slots,
                (Some(&interaction), crumbs),
            );
            if let Some(field) = field {
                Widget::draw(field, scene, transform, tr, theme, presence);
            }
        });
    }

    /// The slots as assistive technology and automation reach them.
    pub(crate) fn accessibility(&self) -> Vec<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let Some(toolbar) = self.toolbar() else {
            return Vec::new();
        };
        let button = |name: String, target: String, rect: Rect, enabled: bool| {
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_action_target(target)
                .with_bounds(rect)
                .with_state(AccessState {
                    focusable: enabled,
                    disabled: !enabled,
                    ..Default::default()
                })
        };
        let mut nodes = Vec::new();
        if let Some(open) = toolbar.sidebar {
            let mut toggle = button(
                crate::i18n::I18nManager::lookup_plain(sidebar_key(toolbar)),
                SIDEBAR_TARGET.into(),
                self.slots.sidebar,
                true,
            );
            toggle.role = Role::ToggleButton;
            toggle.state.checked = Some(open);
            nodes.push(toggle);
        }
        if let Some(nav) = toolbar.navigation {
            nodes.push(button(
                crate::i18n::I18nManager::lookup_plain("window-toolbar-back"),
                BACK_TARGET.into(),
                self.slots.back,
                nav.back,
            ));
            nodes.push(button(
                crate::i18n::I18nManager::lookup_plain("window-toolbar-forward"),
                FORWARD_TARGET.into(),
                self.slots.forward,
                nav.forward,
            ));
        }
        if let Some(location) = toolbar.location.as_ref()
            && self.slots.location.area() > 0.0
        {
            if let Some(field) = self.field.as_ref().and_then(Widget::accessibility) {
                nodes.push(field);
            } else {
                let name = if location.name.is_empty() {
                    location.label.clone()
                } else {
                    location.name.clone()
                };
                let mut node = AccessNode::new(if location.editable {
                    Role::Button
                } else {
                    Role::StaticText
                })
                .with_name(name)
                .with_optional_value(Some(location.edit_value().to_owned()))
                .with_bounds(self.slots.location);
                if location.editable {
                    node = node
                        .with_action_target(LOCATION_TARGET)
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        });
                }
                nodes.push(node);
            }
        }
        // The location's levels: each ancestor goes there.
        if let Some(location) = toolbar.location.as_ref()
            && self.field.is_none()
            && self.slots.location.area() > 0.0
        {
            let ancestors = location.segments.len().saturating_sub(1);
            for (i, segment) in location.segments.iter().take(ancestors).enumerate() {
                nodes.push(button(
                    segment.label.clone(),
                    format!("{SEGMENT_TARGET_PREFIX}{i}"),
                    self.slots.location,
                    true,
                ));
            }
        }
        // Every action, in the row or in the overflow menu: assistive
        // technology and automation reach all of them the same way.
        for (i, action) in toolbar.actions.iter().enumerate() {
            let rect = self
                .slots
                .actions
                .get(i)
                .copied()
                .unwrap_or(self.slots.overflow);
            let mut node = button(
                action.name.clone(),
                format!("{ACTION_TARGET_PREFIX}{}", action.id),
                rect,
                action.enabled,
            );
            if action.on {
                node.role = Role::ToggleButton;
                node.state.checked = Some(true);
            }
            nodes.push(node);
        }
        if self.slots.overflow.area() > 0.0 {
            let mut more = button(
                crate::i18n::I18nManager::lookup_plain("window-toolbar-more"),
                OVERFLOW_TARGET.into(),
                self.slots.overflow,
                true,
            );
            more.state.expanded = Some(self.menu.is_open());
            nodes.push(more);
        }
        nodes
    }

    /// Run the slot `target` names, as assistive technology activates it.
    pub(crate) fn perform(&mut self, target: &str) -> bool {
        let slot = match target {
            SIDEBAR_TARGET => Slot::Sidebar,
            BACK_TARGET => Slot::Back,
            FORWARD_TARGET => Slot::Forward,
            LOCATION_TARGET => Slot::Location,
            OVERFLOW_TARGET => Slot::Overflow,
            _ => {
                if let Some(level) = target
                    .strip_prefix(SEGMENT_TARGET_PREFIX)
                    .and_then(|level| level.parse::<usize>().ok())
                {
                    let Some(value) = self
                        .toolbar()
                        .and_then(|toolbar| toolbar.location.as_ref())
                        .and_then(|location| location.segments.get(level))
                        .map(|segment| segment.value.clone())
                    else {
                        return false;
                    };
                    self.push(ToolbarEvent::Commit { text: value });
                    return true;
                }
                let Some(id) = target.strip_prefix(ACTION_TARGET_PREFIX) else {
                    return false;
                };
                let Some(i) = self
                    .toolbar()
                    .and_then(|toolbar| toolbar.actions.iter().position(|a| a.id == id))
                else {
                    return false;
                };
                // An action in the overflow menu runs as one in the row does.
                return self.run_action(i);
            }
        };
        self.activate(&slot)
    }
}

/// The name of the sidebar toggle in its present state.
fn sidebar_key(toolbar: &WindowToolbar) -> &'static str {
    if toolbar.sidebar == Some(true) {
        "window-toolbar-hide-sidebar"
    } else {
        "window-toolbar-show-sidebar"
    }
}

/// What the person is doing with the row, for its paint.
struct Interaction {
    hover: Option<Slot>,
    pressed: Option<Slot>,
    editing: bool,
    /// The overflow menu is open: its button stays lifted.
    menu_open: bool,
}

/// Paint `paint` at `opacity`, in a layer only while it is part-way.
fn layered(
    scene: &mut Scene,
    transform: Affine,
    clip: Rect,
    opacity: f32,
    paint: impl FnOnce(&mut Scene),
) {
    if opacity <= 0.0 {
        return;
    }
    let part = opacity < 1.0;
    if part {
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            opacity,
            transform,
            &clip.inflate(2.0, 2.0),
        );
    }
    paint(scene);
    if part {
        scene.pop_layer();
    }
}

fn paint_slots(
    scene: &mut Scene,
    transform: Affine,
    (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    toolbar: &WindowToolbar,
    slots: &Slots,
    (interaction, crumbs): (Option<&Interaction>, Option<&mut Breadcrumb>),
) {
    let face = |slot: Slot, enabled: bool, on: bool| ButtonFace {
        hovered: interaction.is_some_and(|i| i.hover.as_ref() == Some(&slot)),
        pressed: interaction.is_some_and(|i| i.pressed.as_ref() == Some(&slot)),
        on,
        enabled,
    };
    if toolbar.sidebar.is_some() {
        paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (slots.sidebar, Icon::PanelLeft),
            // Open or closed, the sidebar shows its state itself; the
            // button stays plain like the bar's other navigation.
            face(Slot::Sidebar, true, false),
        );
    }
    if let Some(nav) = toolbar.navigation {
        paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (slots.back, Icon::ArrowLeft),
            face(Slot::Back, nav.back, false),
        );
        paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (slots.forward, Icon::ArrowRight),
            face(Slot::Forward, nav.forward, false),
        );
    }
    for (i, (action, rect)) in toolbar.actions.iter().zip(&slots.actions).enumerate() {
        let Some(icon) = Icon::from_name(&action.icon) else {
            continue;
        };
        paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (*rect, icon),
            face(Slot::Action(i), action.enabled, action.on),
        );
    }
    if slots.overflow.area() > 0.0 {
        let open = interaction.is_some_and(|i| i.menu_open);
        let mut overflow = face(Slot::Overflow, true, false);
        overflow.hovered |= open;
        paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (slots.overflow, Icon::MoreHorizontal),
            overflow,
        );
    }
    let editing = interaction.is_some_and(|i| i.editing);
    if let Some(location) = toolbar.location.as_ref()
        && slots.location.area() > 0.0
        && !editing
    {
        let hovered = interaction.is_some_and(|i| i.hover == Some(Slot::Location));
        paint_location(
            scene,
            transform,
            (tr, theme, presence),
            (location, crumbs),
            slots.location,
            hovered,
        );
    }
}

/// The location at rest: its icon, then its label, elided at the end. An
/// editable one stands in a field's frame, so it reads as one to click.
fn paint_location(
    scene: &mut Scene,
    transform: Affine,
    (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    (location, crumbs): (
        &crate::window_toolbar::ToolbarLocation,
        Option<&mut Breadcrumb>,
    ),
    rect: Rect,
    hovered: bool,
) {
    let mut text_rect = rect;
    if location.editable {
        let face = FieldFace::new(rect, "").hovered(hovered);
        face.paint_chrome(scene, transform, theme);
        text_rect = face.text_rect(theme);
    }
    if let Some(icon) = location.icon.as_deref().and_then(Icon::from_name) {
        let c = Point::new(text_rect.x0 + LOCATION_ICON_PX / 2.0, rect.center().y);
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(c, (LOCATION_ICON_PX, LOCATION_ICON_PX)),
            icon,
            theme.tokens.colors.text_dim,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_rect.x0 += LOCATION_ICON_PX + f64::from(theme.tokens.spacing.s2);
    }
    // A location with levels shows them as a breadcrumb.
    if let Some(crumbs) = crumbs
        && !location.segments.is_empty()
    {
        if text_rect.width() > 0.0 {
            crumbs.set_rect(Rect::new(text_rect.x0, rect.y0, text_rect.x1, rect.y1));
            Widget::draw(crumbs, scene, transform, tr, theme, presence);
        }
        return;
    }
    let (shown, ink) = if location.label.is_empty() {
        (
            location.placeholder.as_str(),
            theme.tokens.colors.text_tertiary,
        )
    } else {
        (location.label.as_str(), theme.tokens.colors.text_primary)
    };
    if shown.is_empty() || text_rect.width() <= 0.0 {
        return;
    }
    let style = theme.text(TextRole::Body);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a location's width is a bounded on-screen size, far inside f32"
    )]
    let width = text_rect.width() as f32;
    let line = tr.elide_to_width_style(shown, width, &style);
    let (_, h) = tr.measure_style(&line, None, &style);
    let y = rect.center().y - f64::from(h) / 2.0;
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine transform composition over f64 screen coordinates"
    )]
    let origin = transform * Affine::translate((text_rect.x0, y));
    tr.draw_style(scene, &line, ink, origin, None, &style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_toolbar::{ToolbarAction, ToolbarLocation, ToolbarNavigation};

    fn browser() -> WindowToolbar {
        WindowToolbar {
            sidebar: None,
            navigation: Some(ToolbarNavigation {
                back: true,
                forward: false,
            }),
            location: Some(ToolbarLocation {
                label: "duckduckgo.com".into(),
                value: "https://duckduckgo.com/".into(),
                editable: true,
                name: "Address and search".into(),
                ..ToolbarLocation::default()
            }),
            actions: vec![ToolbarAction {
                id: "reload".into(),
                icon: "refresh".into(),
                name: "Reload".into(),
                enabled: true,
                on: false,
            }],
        }
    }

    fn row() -> ToolbarRow {
        let mut row = ToolbarRow::new();
        row.show(
            7,
            Some(&browser()),
            Theme::dark().motion(),
            crate::time::now(),
        );
        row.lay_out(Rect::new(40.0, 0.0, 640.0, 40.0));
        row
    }

    #[test]
    fn navigation_leads_actions_trail_and_the_location_fills_between() {
        let row = row();
        let slots = row.slots();
        assert!(slots.back.x1 < slots.forward.x0);
        assert!(slots.forward.x1 < slots.location.x0);
        assert!(slots.location.x1 < slots.actions[0].x0);
        assert!((slots.actions[0].x1 - 640.0).abs() < f64::EPSILON);
        assert!(slots.location.width() > 400.0);
    }

    #[test]
    fn back_runs_only_where_it_can_and_forward_not_at_the_end() {
        let mut row = row();
        let back = row.slots().back.center();
        assert!(row.press(back));
        assert!(row.release(back));
        let forward = row.slots().forward.center();
        assert!(row.press(forward));
        assert!(row.release(forward));
        let reload = row.slots().actions[0].center();
        assert!(row.press(reload));
        assert!(row.release(reload));
        assert_eq!(
            row.take_events(),
            vec![
                (7, ToolbarEvent::Back),
                (
                    7,
                    ToolbarEvent::Action {
                        id: "reload".into()
                    }
                )
            ],
            "forward cannot go"
        );
    }

    #[test]
    fn a_click_on_the_location_edits_the_whole_address_and_esc_puts_it_back() {
        let mut row = row();
        let location = row.slots().location.center();
        assert!(row.press(location));
        assert!(row.is_editing());
        assert_eq!(row.field_text(), Some("https://duckduckgo.com/"));
        assert!(row.cancel_edit());
        assert!(!row.is_editing());
        assert!(row.take_events().is_empty(), "nothing was committed");
    }

    #[test]
    fn another_tab_cross_fades_and_drops_what_was_typed_in_this_one() {
        let mut row = row();
        assert!(row.begin_edit());
        let now = crate::time::now();
        row.show(8, None, Theme::dark().motion(), now);
        assert!(!row.is_editing());
        assert!(row.toolbar().is_none());
        assert!(row.needs_animation_frame(now), "the old slots fade out");
    }
}
