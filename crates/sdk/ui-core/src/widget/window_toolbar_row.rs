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
//! ([`ToolbarEvent::Edit`]) and its suggestions are listed under the field,
//! as wide as it, in the shared [`SuggestionList`]: each row its icon, its
//! title and the line under it. The arrow keys move through them; Enter, a
//! click or a tap commits one. This list is the only one: the app draws no
//! suggestions of its own while the bar hosts its location.
//!
//! A location that is one of a few places (its app declared
//! [`LocationChoice`](crate::window_toolbar::LocationChoice)s) reads as a
//! picker: its label with a chevron, lifting under the pointer like the
//! bar's buttons. A click, a tap or the location's accessibility action opens
//! the choices in the shared [`ContextMenu`] under it, the current one
//! checked; choosing one commits its value ([`ToolbarEvent::Commit`]).
//!
//! The overflow menu is the shared [`ContextMenu`] too. Both are painted in
//! the window's top layer ([`ToolbarRow::paint_popups`]).
//!
//! When the window shows another tab, the row's content cross-fades in place
//! ([`crate::motion::CrossFade`]): the outgoing tab's slots fade out where
//! they stood while the incoming ones fade in.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::Widget;
use super::autocomplete::{Suggestion, SuggestionList};
use super::breadcrumb::Breadcrumb;
use super::context_menu::{ContextMenu, MenuItem};
use super::text_input::{AccessoryAction, FieldFace, TextInput};
use super::toggle_group::{SegmentedFace, ToggleGroup};
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
pub const SUGGESTION_TARGET_PREFIX: &str = "window-toolbar-suggestion:";
pub const CHOICE_TARGET_PREFIX: &str = "window-toolbar-choice:";
pub const LEADING_TARGET_PREFIX: &str = "window-toolbar-leading:";
pub const VIEW_TARGET_PREFIX: &str = "window-toolbar-view:";

/// The size of the view switcher's segments: a strip no taller than the
/// bar's buttons.
const VIEWS_SIZE: super::ControlSize = super::ControlSize::Small;

/// Between the navigation buttons and the location, and between the
/// location and the actions.
const SECTION_GAP: f64 = crate::tokens::Spacing::BASE.s2 as f64;
/// The location's leading icon: a control glyph.
const LOCATION_ICON_PX: f64 = crate::tokens::Metrics::BASE.icon_control as f64;
/// A location narrower than this is not shown: the bar keeps its buttons.
const MIN_LOCATION_PX: f64 = crate::tokens::Metrics::BASE.location_min_width as f64;
/// Actions move into the overflow menu, last first, while the location
/// would be narrower than this.
const COMFORTABLE_LOCATION_PX: f64 = crate::tokens::Metrics::BASE.location_comfortable_width as f64;

/// One slot of the row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Sidebar,
    Back,
    Forward,
    /// One of the actions after the navigation.
    Leading(usize),
    Location,
    /// One choice of the view switcher.
    View(usize),
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
    /// The leading actions, after the navigation.
    pub leading: Vec<Rect>,
    pub location: Rect,
    /// The view switcher's segments (none: its choices are in the overflow
    /// menu).
    pub views: Vec<Rect>,
    /// The actions standing in the row, the first of the declared ones.
    pub actions: Vec<Rect>,
    /// The overflow button (`Rect::ZERO`: every action is in the row).
    pub overflow: Rect,
}

/// What the row lays out beside the toolbar: the view switcher's size, as
/// the theme draws its labels (none before it is measured).
pub(crate) type ViewsSize = Option<(f64, f64)>;

impl Slots {
    /// Lay `toolbar`'s slots out across `row`, centred on its centre line:
    /// navigation and the leading actions leading, the view switcher and the
    /// actions trailing, the location filling between. At most
    /// [`crate::window_toolbar::MAX_ACTIONS`] actions stand in the row;
    /// while the location would be narrower than
    /// [`COMFORTABLE_LOCATION_PX`] (or, without one, while the buttons would
    /// overlap) the last of them move into the overflow menu, then the view
    /// switcher's choices, as do the declared actions past that many and
    /// those declared for the menu.
    pub(crate) fn lay_out(toolbar: &WindowToolbar, row: Rect, views: ViewsSize) -> Self {
        if row.width() <= 0.0 || row.height() <= 0.0 {
            return Self::default();
        }
        let row_actions = toolbar
            .row_actions()
            .min(crate::window_toolbar::MAX_ACTIONS);
        let views = views.filter(|_| toolbar.views.is_some());
        // The switcher in the row first, then in the menu.
        let tries = if views.is_some() { 2 } else { 1 };
        for views in [views, None].into_iter().take(tries) {
            let mut inline = row_actions;
            loop {
                let slots = Self::with_inline(toolbar, row, inline, views);
                let room = if toolbar.location.is_some() {
                    slots.location.width() >= COMFORTABLE_LOCATION_PX
                } else {
                    slots.fits(row)
                };
                if room {
                    return slots;
                }
                if inline == 0 {
                    break;
                }
                inline -= 1;
            }
        }
        Self::with_inline(toolbar, row, 0, None)
    }

    /// The slots with the first `inline` actions in the row and the rest in
    /// the overflow menu, and the view switcher in the row at `views`' size
    /// (none: in the menu).
    fn with_inline(toolbar: &WindowToolbar, row: Rect, inline: usize, views: ViewsSize) -> Self {
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
        if !toolbar.leading.is_empty() {
            for _ in &toolbar.leading {
                let rect = square(x0);
                x0 = rect.x1 + CONTROL_GAP;
                slots.leading.push(rect);
            }
            x0 = x0 - CONTROL_GAP + SECTION_GAP;
        }
        let mut x1 = row.x1;
        let mut trailing = false;
        let in_menu =
            toolbar.actions.len() > inline || (toolbar.views.is_some() && views.is_none());
        if in_menu {
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
        if let (Some((width, height)), Some(choices)) = (
            views,
            toolbar.views.as_ref().map(|views| views.choices.len()),
        ) && choices > 0
        {
            let strip = Rect::new(
                x1 - width,
                row.center().y - height / 2.0,
                x1,
                row.center().y + height / 2.0,
            );
            slots.views = equal_segments(strip, choices);
            x1 = strip.x0 - SECTION_GAP;
        }
        if toolbar.location.is_some() && x1 - x0 >= MIN_LOCATION_PX {
            slots.location = Rect::new(x0, y0, x1, y0 + CONTROL_PX);
        }
        slots
    }

    /// Every button stands inside `row`, clear of the ones before it.
    fn fits(&self, row: Rect) -> bool {
        let lead = self
            .leading
            .iter()
            .map(|rect| rect.x1)
            .fold(self.forward.x1.max(self.sidebar.x1).max(row.x0), f64::max);
        self.actions
            .iter()
            .chain(self.views.first())
            .chain((self.overflow.area() > 0.0).then_some(&self.overflow))
            .all(|rect| rect.x0 >= lead)
    }

    /// The view switcher's whole strip (`Rect::ZERO`: in the menu).
    #[cfg(test)]
    pub(crate) fn views_strip(&self) -> Rect {
        match (self.views.first(), self.views.last()) {
            (Some(first), Some(last)) => first.union(*last),
            _ => Rect::ZERO,
        }
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
        if let Some(i) = self.leading.iter().position(|r| r.contains(p)) {
            return Some(Slot::Leading(i));
        }
        if let Some(i) = self.views.iter().position(|r| r.contains(p)) {
            return Some(Slot::View(i));
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

/// `strip` split into `n` equal segments, as a segmented control of
/// labels that all fit splits itself.
fn equal_segments(strip: Rect, n: usize) -> Vec<Rect> {
    let Some(count) = u32::try_from(n).ok().filter(|n| *n > 0) else {
        return Vec::new();
    };
    let share = strip.width() / f64::from(count);
    (0..count)
        .map(|i| {
            let x0 = strip.x0 + share * f64::from(i);
            let x1 = if i.saturating_add(1) == count {
                strip.x1
            } else {
                x0 + share
            };
            Rect::new(x0, strip.y0, x1, strip.y1)
        })
        .collect()
}

/// What a row of the open menu (the overflow's, a menu button's, or the
/// location's picker) runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuEntry {
    /// Declared action `n`.
    Action(usize),
    /// View `n` of the switcher.
    View(usize),
    /// Item `.1` of menu button `.0`.
    Item(usize, usize),
    /// The location's choice `n`, committed.
    Choice(usize),
}

/// A menu row for `action`: its name, icon and state.
fn menu_item(action: &crate::window_toolbar::ToolbarAction) -> MenuItem {
    let mut item = MenuItem::selection(action.name.clone()).with_enabled(action.enabled);
    if let Some(icon) = Icon::from_name(&action.icon) {
        item = item.with_icon(icon);
    }
    if action.on {
        item = item.with_checked(true);
    }
    item
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
    /// The overflow button's menu or the location's picker, and what each
    /// of its rows does (`None`: a separator).
    menu: ContextMenu,
    menu_entries: Vec<Option<MenuEntry>>,
    /// Where a location with choices painted its picker last: what its menu
    /// hangs from (`Rect::ZERO`: not painted yet, the whole slot).
    choice_trigger: Rect,
    /// The button the open menu hangs from: the overflow button, a menu
    /// button, or the location's picker.
    menu_slot: Option<Slot>,
    /// The view switcher's size as the theme draws it, and the labels and
    /// text scale it was measured for.
    views_size: Option<((Vec<String>, u32), (f64, f64))>,
    /// The suggestions under the edited field, and what each commits.
    suggestions: SuggestionList,
    suggestion_values: Vec<String>,
    /// The person closed the list (a press off it): it stays closed until
    /// the app suggests something else.
    suggestions_dismissed: bool,
    /// Where the pointer last was, for the press that follows.
    pointer: Point,
    /// The suggestion a touch went down on: lifting on it commits it.
    touched: Option<(u64, usize)>,
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
            menu_entries: Vec::new(),
            choice_trigger: Rect::ZERO,
            menu_slot: None,
            views_size: None,
            suggestions: SuggestionList::new(),
            suggestion_values: Vec::new(),
            suggestions_dismissed: false,
            pointer: Point::ZERO,
            touched: None,
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
            self.abandon_edit();
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
            self.suggestions.clear();
            self.suggestion_values.clear();
            self.suggestions_dismissed = false;
            return;
        }
        if values == self.suggestion_values {
            return;
        }
        self.suggestions_dismissed = false;
        let rows = suggestions
            .iter()
            .map(|suggestion| {
                let mut row =
                    Suggestion::labelled(suggestion.value.clone(), suggestion.label.clone());
                if let Some(icon) = suggestion.icon.as_deref().and_then(Icon::from_name) {
                    row = row.with_icon(icon);
                }
                if !suggestion.detail.is_empty() {
                    row = row.with_subtitle(suggestion.detail.clone());
                }
                row
            })
            .collect();
        // A new answer is a new list: nothing in it is chosen yet, so Enter
        // opens what was typed until an arrow chooses a row.
        self.suggestions.set_suggestions(rows);
        self.suggestions.set_highlight(None);
        self.suggestion_values = values;
        self.place_suggestions();
    }

    /// The list hangs under the location, as wide as it, inside the window.
    fn place_suggestions(&mut self) {
        self.suggestions.set_anchor(self.slots.location);
        if self.popup_area.area() > 0.0 {
            self.suggestions.set_viewport(self.popup_area);
        }
    }

    /// Whether the suggestions are listed under the edited field.
    fn suggestions_open(&self) -> bool {
        self.field.is_some() && !self.suggestions_dismissed && self.suggestions.has_content()
    }

    /// The person closed the list: it stays closed until the app suggests
    /// something else.
    fn dismiss_suggestions(&mut self) {
        self.suggestions_dismissed = true;
        self.suggestions.set_highlight(None);
        self.touched = None;
    }

    /// What the menus keep inside.
    pub(crate) fn popup_area(&self) -> Rect {
        self.popup_area
    }

    /// What the menus keep inside: the window the bar belongs to.
    pub(crate) fn set_popup_area(&mut self, area: Rect) {
        self.popup_area = area;
        self.place_suggestions();
    }

    /// Measure the view switcher as `theme` draws its labels, and lay the
    /// row out again when its size changed. Returns whether it did.
    pub(crate) fn measure_views(&mut self, theme: &Theme) -> bool {
        let labels: Vec<String> = self
            .toolbar()
            .and_then(|toolbar| toolbar.views.as_ref())
            .map(|views| views.choices.iter().map(|v| v.label.clone()).collect())
            .unwrap_or_default();
        if labels.is_empty() {
            let had = self.views_size.take().is_some();
            if had {
                self.lay_out(self.row);
            }
            return had;
        }
        let key = (labels, theme.text_scale.to_bits());
        if self
            .views_size
            .as_ref()
            .is_some_and(|(measured, _)| *measured == key)
        {
            return false;
        }
        let size = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            let labels: Vec<&str> = key.0.iter().map(String::as_str).collect();
            (
                SegmentedFace::intrinsic_width(&labels, VIEWS_SIZE, text, theme),
                SegmentedFace::height(VIEWS_SIZE, theme).min(CONTROL_PX),
            )
        });
        self.views_size = Some((key, size));
        self.lay_out(self.row);
        true
    }

    /// The toolbar showing, if any.
    pub(crate) fn toolbar(&self) -> Option<&WindowToolbar> {
        self.shown.as_ref().map(|(_, toolbar)| toolbar)
    }

    /// Lay the slots out across `row`.
    pub(crate) fn lay_out(&mut self, row: Rect) {
        self.row = row;
        let views = self.views_size.as_ref().map(|(_, size)| *size);
        self.slots = self
            .toolbar()
            .map(|toolbar| Slots::lay_out(toolbar, row, views))
            .unwrap_or_default();
        let location = self.slots.location;
        if let Some(field) = self.field.as_mut() {
            if location.area() > 0.0 {
                field.apply_layout_external(location);
                self.place_suggestions();
            } else {
                // No room for the field any more.
                self.abandon_edit();
            }
        }
        // The open menu follows what it hangs from, and goes with it.
        let anchor = match self.menu_slot {
            Some(Slot::Action(i)) => self.slots.actions.get(i).copied().unwrap_or(Rect::ZERO),
            Some(Slot::Location) => self.choice_anchor(),
            _ => self.slots.overflow,
        };
        if anchor.area() <= 0.0 {
            self.menu.close();
        } else if self.menu.is_open() {
            self.menu.reanchor(anchor, self.popup_area);
        }
    }

    /// Whether the location is one of a few places, picked from a menu.
    fn has_choices(&self) -> bool {
        self.toolbar()
            .and_then(|toolbar| toolbar.location.as_ref())
            .is_some_and(|location| !location.editable && !location.choices.is_empty())
    }

    /// Whether the open menu is the location's picker.
    pub(crate) fn choices_open(&self) -> bool {
        self.menu.is_open() && self.menu_slot == Some(Slot::Location)
    }

    /// The labels of the open menu's rows, for tests.
    #[cfg(test)]
    pub(crate) fn menu_labels(&self) -> Vec<String> {
        if !self.menu.is_open() {
            return Vec::new();
        }
        self.menu
            .items()
            .iter()
            .filter_map(MenuItem::label)
            .map(str::to_owned)
            .collect()
    }

    /// What the location's picker hangs from: the label and chevron it
    /// painted, inside the location's slot.
    fn choice_anchor(&self) -> Rect {
        let slot = self.slots.location;
        if slot.area() <= 0.0 {
            return Rect::ZERO;
        }
        let trigger = self.choice_trigger.intersect(slot);
        if trigger.width() > 0.0 && trigger.height() > 0.0 {
            trigger
        } else {
            slot
        }
    }

    /// Open the location's picker under it: every choice by its label, the
    /// current one checked, separators where the app grouped them.
    fn open_choices(&mut self) -> bool {
        let Some(location) = self
            .toolbar()
            .and_then(|toolbar| toolbar.location.as_ref())
            .filter(|location| !location.editable && !location.choices.is_empty())
        else {
            return false;
        };
        let mut items = Vec::with_capacity(location.choices.len() + 2);
        let mut rows = Vec::with_capacity(location.choices.len() + 2);
        for (index, choice) in location.choices.iter().enumerate() {
            if choice.separated && index > 0 {
                items.push(MenuItem::separator());
                rows.push(None);
            }
            items.push(MenuItem::selection(choice.label.clone()).with_checked(choice.current));
            rows.push(Some(MenuEntry::Choice(index)));
        }
        let anchor = self.choice_anchor();
        if anchor.area() <= 0.0 {
            return false;
        }
        self.menu.set_items(items);
        self.menu.set_align(super::PanelAlign::Leading);
        self.menu.open_anchored(anchor, self.popup_area);
        self.menu_entries = rows;
        self.menu_slot = Some(Slot::Location);
        true
    }

    /// Commit the location's choice `index`, as choosing it does.
    fn commit_choice(&mut self, index: usize) -> bool {
        let Some(value) = self
            .toolbar()
            .and_then(|toolbar| toolbar.location.as_ref())
            .filter(|location| !location.editable)
            .and_then(|location| location.choices.get(index))
            .map(|choice| choice.value.clone())
        else {
            return false;
        };
        self.menu.close();
        self.push(ToolbarEvent::Commit { text: value });
        true
    }

    /// The field goes, and the suggestions with it.
    fn end_edit(&mut self) {
        self.field = None;
        self.heard.clear();
        self.suggestions.clear();
        self.suggestion_values.clear();
        self.suggestions_dismissed = false;
        self.touched = None;
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
        self.abandon_edit();
        editing
    }

    /// End the edit without committing it: the field holds the location
    /// again, and the app hears so ([`ToolbarEvent::Edit`] with the
    /// location's value), as it heard each change, so nothing it does for
    /// typed text goes on for text no longer there.
    fn abandon_edit(&mut self) {
        if self.field.is_none() {
            return;
        }
        let location = self
            .toolbar()
            .and_then(|toolbar| toolbar.location.as_ref())
            .map(|location| location.value.clone());
        self.end_edit();
        if let Some(text) = location {
            self.draft(text);
        }
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
            Slot::Leading(i) => toolbar.leading.get(*i).is_some_and(|a| a.enabled),
            Slot::Location => toolbar.location.is_some(),
            Slot::View(i) => toolbar
                .views
                .as_ref()
                .is_some_and(|views| *i < views.choices.len()),
            Slot::Action(i) => toolbar.actions.get(*i).is_some_and(|a| a.enabled),
            Slot::Overflow => self.slots.overflow.area() > 0.0,
        }
    }

    /// Open the overflow button's menu: the view switcher's choices when
    /// the row has no room for it, the chosen one checked, then the actions
    /// the row does not show, by name, each with its icon and state.
    fn open_overflow(&mut self) -> bool {
        let Some(toolbar) = self.toolbar() else {
            return false;
        };
        let mut items = Vec::new();
        let mut entries = Vec::new();
        if self.slots.views.is_empty()
            && let Some(views) = toolbar.views.as_ref()
        {
            for (i, view) in views.choices.iter().enumerate() {
                items.push(
                    MenuItem::selection(view.label.clone()).with_checked(view.id == views.chosen),
                );
                entries.push(Some(MenuEntry::View(i)));
            }
        }
        let shown = self.slots.actions.len();
        let hidden = shown..toolbar.actions.len();
        if !items.is_empty() && !hidden.is_empty() {
            items.push(MenuItem::separator());
            entries.push(None);
        }
        for i in hidden {
            let Some(action) = toolbar.actions.get(i) else {
                continue;
            };
            if action.items.is_empty() {
                items.push(menu_item(action));
                entries.push(Some(MenuEntry::Action(i)));
                continue;
            }
            // A menu button's items follow its name.
            if !items.is_empty() {
                items.push(MenuItem::separator());
                entries.push(None);
            }
            items.push(MenuItem::caption(action.name.clone()));
            entries.push(None);
            for (j, item) in action.items.iter().enumerate() {
                items.push(menu_item(item));
                entries.push(Some(MenuEntry::Item(i, j)));
            }
        }
        if items.is_empty() {
            return false;
        }
        self.open_menu(items, entries, Slot::Overflow, self.slots.overflow);
        true
    }

    /// Open menu button `index`'s menu under it: its items, by name, each
    /// with its icon and state.
    fn open_action_menu(&mut self, index: usize) -> bool {
        let Some(action) = self
            .toolbar()
            .and_then(|toolbar| toolbar.actions.get(index))
        else {
            return false;
        };
        let items = action.items.iter().map(menu_item).collect();
        let entries = (0..action.items.len())
            .map(|j| Some(MenuEntry::Item(index, j)))
            .collect();
        let Some(anchor) = self.slots.actions.get(index).copied() else {
            return false;
        };
        self.open_menu(items, entries, Slot::Action(index), anchor);
        true
    }

    fn open_menu(
        &mut self,
        items: Vec<MenuItem>,
        entries: Vec<Option<MenuEntry>>,
        slot: Slot,
        anchor: Rect,
    ) {
        self.menu.set_items(items);
        self.menu.set_align(super::PanelAlign::Trailing);
        self.menu.open_anchored(anchor, self.popup_area);
        self.menu_entries = entries;
        self.menu_slot = Some(slot);
    }

    /// Run item `item` of menu button `index`.
    fn run_item(&mut self, index: usize, item: usize) -> bool {
        let Some(chosen) = self
            .toolbar()
            .and_then(|toolbar| toolbar.actions.get(index))
            .and_then(|action| action.items.get(item))
            .filter(|chosen| chosen.enabled)
        else {
            return false;
        };
        let id = chosen.id.clone();
        self.push(ToolbarEvent::Action { id });
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

    /// Run leading action `index`.
    fn run_leading(&mut self, index: usize) -> bool {
        let Some(action) = self
            .toolbar()
            .and_then(|toolbar| toolbar.leading.get(index))
            .filter(|action| action.enabled)
        else {
            return false;
        };
        let id = action.id.clone();
        self.push(ToolbarEvent::Action { id });
        true
    }

    /// Choose view `index`: the app hears its id.
    fn choose_view(&mut self, index: usize) -> bool {
        let Some(view) = self
            .toolbar()
            .and_then(|toolbar| toolbar.views.as_ref())
            .and_then(|views| views.choices.get(index))
        else {
            return false;
        };
        let id = view.id.clone();
        self.push(ToolbarEvent::Action { id });
        true
    }

    /// Run what open menu row `row` stands for.
    fn run_menu_entry(&mut self, row: usize) -> bool {
        match self.menu_entries.get(row).copied().flatten() {
            Some(MenuEntry::Action(index)) => self.run_action(index),
            Some(MenuEntry::View(index)) => self.choose_view(index),
            Some(MenuEntry::Item(index, item)) => self.run_item(index, item),
            Some(MenuEntry::Choice(index)) => self.commit_choice(index),
            None => false,
        }
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
            Slot::Location if self.has_choices() => return self.open_choices(),
            Slot::Leading(i) => return self.run_leading(*i),
            Slot::Location => return self.begin_edit(),
            Slot::View(i) => return self.choose_view(*i),
            Slot::Action(i)
                if self
                    .toolbar()
                    .and_then(|toolbar| toolbar.actions.get(*i))
                    .is_some_and(|action| !action.items.is_empty()) =>
            {
                return self.open_action_menu(*i);
            }
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
            Slot::Leading(i) => {
                let action = toolbar.leading.get(i)?;
                Some((*self.slots.leading.get(i)?, action.name.clone()))
            }
            Slot::Action(i) => {
                let action = toolbar.actions.get(i)?;
                Some((*self.slots.actions.get(i)?, action.name.clone()))
            }
            Slot::Overflow => Some((self.slots.overflow, lookup("window-toolbar-more"))),
            // A segment and the location say what they are.
            Slot::View(_) | Slot::Location => None,
        }
    }

    /// Whether `p` is on the location or the suggestions under it: a press
    /// there keeps the edit.
    pub(crate) fn location_hit(&self, p: Point) -> bool {
        self.slots.location.contains(p)
            || self
                .suggestions_rect()
                .is_some_and(|panel| panel.contains(p))
    }

    /// Where each suggestion under the edited field is, best first, while
    /// the list is open.
    pub(crate) fn suggestion_rects(&self) -> Vec<Rect> {
        if !self.suggestions_open() {
            return Vec::new();
        }
        (0..self.suggestion_values.len())
            .map_while(|index| self.suggestions.row_rect(index))
            .collect()
    }

    /// The list's panel, while it is open.
    pub(crate) fn suggestions_rect(&self) -> Option<Rect> {
        self.suggestions_open()
            .then(|| self.suggestions.rect())
            .flatten()
    }

    /// Whether one of the row's menus is open.
    pub(crate) fn popup_open(&self) -> bool {
        self.menu.is_open() || self.suggestions_open()
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
                self.run_menu_entry(row);
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
        if self.suggestions_open() && pointer {
            return self.suggestions_pointer(event, ctx);
        }
        false
    }

    /// A pointer or touch event while the suggestions are listed: hovering
    /// lifts a row, a click or a tap commits it, the wheel scrolls the list,
    /// and a press off it closes the list and goes on (to the field, or to
    /// whatever ends the edit). Returns whether the list took it.
    fn suggestions_pointer(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::{MouseButton, MouseScrollDelta, TouchPhase};
        let over = |row: &Self, p: Point| row.suggestions_rect().is_some_and(|r| r.contains(p));
        let took = match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = ctx.logical(position);
                self.suggestions.hover_at(self.pointer);
                over(self, self.pointer)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(index) = self.suggestions.row_at(self.pointer) {
                    self.commit_suggestion(index)
                } else if over(self, self.pointer) {
                    // The panel's inset: no row, but not off the list.
                    true
                } else {
                    self.dismiss_suggestions();
                    false
                }
            }
            WindowEvent::MouseInput { .. } => over(self, self.pointer),
            WindowEvent::MouseWheel { delta, .. } if over(self, self.pointer) => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(p) => p.y / 40.0,
                };
                // Wheel up shows earlier rows.
                if lines.abs() >= 0.5 {
                    self.suggestions.scroll_by(if lines < 0.0 { 1 } else { -1 });
                }
                true
            }
            WindowEvent::Touch(touch) => {
                let p = ctx.logical(touch.location);
                match touch.phase {
                    TouchPhase::Started => {
                        if let Some(index) = self.suggestions.row_at(p) {
                            self.touched = Some((touch.id, index));
                            self.suggestions.set_highlight(Some(index));
                            true
                        } else if over(self, p) {
                            true
                        } else {
                            self.dismiss_suggestions();
                            false
                        }
                    }
                    TouchPhase::Moved => self.touched.is_some_and(|(id, _)| id == touch.id),
                    TouchPhase::Ended => match self.touched.take() {
                        Some((id, index)) if id == touch.id => {
                            // A tap lifts where it went down; a drag off
                            // the row lets it go.
                            if self.suggestions.row_at(p) == Some(index) {
                                self.commit_suggestion(index);
                            } else {
                                self.suggestions.set_highlight(None);
                            }
                            true
                        }
                        other => {
                            self.touched = other;
                            over(self, p)
                        }
                    },
                    TouchPhase::Cancelled => {
                        if self.touched.take().is_some() {
                            self.suggestions.set_highlight(None);
                        }
                        false
                    }
                }
            }
            _ => false,
        };
        if took {
            *ctx.handled = true;
        }
        took
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
            self.abandon_edit();
        }
        match slot {
            Some(Slot::Location) if self.field.is_none() => {
                // A click on one of the location's levels goes there.
                if let Some(value) = self.crumb_at(p) {
                    self.push(ToolbarEvent::Commit { text: value });
                    return true;
                }
                // One of a few places: the picker opens as the press ends,
                // as the bar's menu buttons do.
                if self.has_choices() {
                    self.pressed = Some(Slot::Location);
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
                    self.abandon_edit();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(named @ (NamedKey::ArrowDown | NamedKey::ArrowUp))
                    if self.suggestions_open() =>
                {
                    self.suggestions
                        .move_highlight(*named == NamedKey::ArrowDown);
                    *ctx.handled = true;
                    return true;
                }
                // Enter commits the chosen row; with none chosen, the field
                // commits what was typed.
                Key::Named(NamedKey::Enter) if self.suggestions_open() => {
                    if let Some(row) = self.suggestions.highlight()
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
        if self.suggestions_open() {
            self.suggestions.paint(scene, transform, tr, theme);
        }
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
        self.measure_views(theme);
        let now = crate::time::now();
        if let Some((outgoing, opacity)) = self.fade.outgoing(now)
            && outgoing.is_some()
            && let Some((toolbar, slots)) = self.outgoing.as_ref()
        {
            layered(scene, transform, self.row, opacity, |scene| {
                let _ = paint_slots(
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
            menu_open: self
                .menu
                .is_open()
                .then(|| self.menu_slot.clone())
                .flatten(),
        };
        let row = self.row;
        let slots = &self.slots;
        let field = self.field.as_mut();
        let crumbs = self.crumbs.as_mut().map(|(_, crumbs)| crumbs);
        let mut trigger = None;
        layered(scene, transform, row, incoming, |scene| {
            trigger = paint_slots(
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
        if let Some(trigger) = trigger {
            self.choice_trigger = trigger;
        }
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
        for (action, rect) in toolbar.leading.iter().zip(&self.slots.leading) {
            nodes.push(button(
                action.name.clone(),
                format!("{LEADING_TARGET_PREFIX}{}", action.id),
                *rect,
                action.enabled,
            ));
        }
        if let Some(location) = toolbar.location.as_ref()
            && self.slots.location.area() > 0.0
        {
            if let Some(field) = self.field.as_ref().and_then(Widget::accessibility) {
                nodes.push(field);
                nodes.extend(self.suggestion_nodes());
            } else {
                let name = if location.name.is_empty() {
                    location.label.clone()
                } else {
                    location.name.clone()
                };
                let picker = !location.editable && !location.choices.is_empty();
                let mut node = AccessNode::new(if location.editable || picker {
                    Role::Button
                } else {
                    Role::StaticText
                })
                .with_name(name)
                .with_optional_value(Some(location.edit_value().to_owned()))
                .with_bounds(self.slots.location);
                if location.editable || picker {
                    node = node
                        .with_action_target(LOCATION_TARGET)
                        .with_state(AccessState {
                            focusable: true,
                            expanded: picker.then(|| self.choices_open()),
                            ..Default::default()
                        });
                }
                if picker && self.choices_open() {
                    // The open picker's rows: each one goes there.
                    let count = location.choices.len();
                    node = node.with_children(
                        location
                            .choices
                            .iter()
                            .enumerate()
                            .map(|(index, choice)| {
                                AccessNode::new(Role::MenuItem)
                                    .with_name(choice.label.clone())
                                    .with_action_target(format!("{CHOICE_TARGET_PREFIX}{index}"))
                                    .with_position(index.saturating_add(1), count)
                                    .with_state(AccessState {
                                        focusable: true,
                                        checked: Some(choice.current),
                                        ..Default::default()
                                    })
                            })
                            .collect(),
                    );
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
        // Every view, in the row or in the overflow menu, the chosen one
        // checked.
        if let Some(views) = toolbar.views.as_ref() {
            for (i, view) in views.choices.iter().enumerate() {
                let rect = self
                    .slots
                    .views
                    .get(i)
                    .copied()
                    .unwrap_or(self.slots.overflow);
                let mut node = button(
                    view.label.clone(),
                    format!("{VIEW_TARGET_PREFIX}{}", view.id),
                    rect,
                    true,
                );
                node.role = Role::RadioButton;
                node.state.checked = Some(view.id == views.chosen);
                if !views.name.is_empty() {
                    node = node.with_description(views.name.clone());
                }
                nodes.push(node);
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
            if !action.items.is_empty() {
                node.state.expanded =
                    Some(self.menu.is_open() && self.menu_slot == Some(Slot::Action(i)));
            }
            nodes.push(node);
            for item in &action.items {
                let mut node = button(
                    item.name.clone(),
                    format!("{ACTION_TARGET_PREFIX}{}", item.id),
                    rect,
                    item.enabled,
                );
                if item.on {
                    node.role = Role::ToggleButton;
                    node.state.checked = Some(true);
                }
                nodes.push(node);
            }
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

    /// The listed suggestions as assistive technology reaches them: each
    /// one an item that commits it, its line under the title its
    /// description.
    fn suggestion_nodes(&self) -> Vec<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if !self.suggestions_open() {
            return Vec::new();
        }
        let rows = self.suggestions.suggestions();
        let list = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let rect = self.suggestions.row_rect(index)?;
                let mut node = AccessNode::new(Role::ListItem)
                    .with_name(&row.label)
                    .with_action_target(format!("{SUGGESTION_TARGET_PREFIX}{index}"))
                    .with_bounds(rect)
                    .with_position(index.saturating_add(1), rows.len())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.suggestions.highlight() == Some(index)),
                    );
                if let Some(subtitle) = &row.subtitle {
                    node = node.with_description(subtitle);
                }
                Some(node)
            })
            .collect();
        let mut node = AccessNode::new(Role::List)
            .with_name(crate::i18n::I18nManager::lookup_plain(
                "window-toolbar-suggestions",
            ))
            .with_children(list);
        if let Some(panel) = self.suggestions.rect() {
            node = node.with_bounds(panel);
        }
        vec![node]
    }

    /// Run the slot `target` names, as assistive technology activates it.
    pub(crate) fn perform(&mut self, target: &str) -> bool {
        if let Some(index) = target
            .strip_prefix(SUGGESTION_TARGET_PREFIX)
            .and_then(|index| index.parse::<usize>().ok())
        {
            return self.suggestions_open() && self.commit_suggestion(index);
        }
        if let Some(index) = target
            .strip_prefix(CHOICE_TARGET_PREFIX)
            .and_then(|index| index.parse::<usize>().ok())
        {
            return self.commit_choice(index);
        }
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
                if let Some(id) = target.strip_prefix(LEADING_TARGET_PREFIX) {
                    let Some(i) = self
                        .toolbar()
                        .and_then(|toolbar| toolbar.leading.iter().position(|a| a.id == id))
                    else {
                        return false;
                    };
                    return self.run_leading(i);
                }
                if let Some(id) = target.strip_prefix(VIEW_TARGET_PREFIX) {
                    let Some(i) = self
                        .toolbar()
                        .and_then(|toolbar| toolbar.views.as_ref())
                        .and_then(|views| views.choices.iter().position(|v| v.id == id))
                    else {
                        return false;
                    };
                    // A view in the overflow menu is chosen as one in the row is.
                    return self.choose_view(i);
                }
                let Some(id) = target.strip_prefix(ACTION_TARGET_PREFIX) else {
                    return false;
                };
                let Some(toolbar) = self.toolbar() else {
                    return false;
                };
                if let Some(i) = toolbar.actions.iter().position(|a| a.id == id) {
                    // An action in the overflow menu runs as one in the row
                    // does; a menu button opens its menu.
                    return self.activate(&Slot::Action(i));
                }
                let Some((i, j)) = toolbar.actions.iter().enumerate().find_map(|(i, action)| {
                    action
                        .items
                        .iter()
                        .position(|item| item.id == id)
                        .map(|j| (i, j))
                }) else {
                    return false;
                };
                // A menu button's item runs without its menu open.
                return self.run_item(i, j);
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
    /// The button whose menu is open (the location, for its picker): it
    /// stays lifted.
    menu_open: Option<Slot>,
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

/// Paint `toolbar`'s slots. Returns where a location with choices painted
/// its picker, which its menu hangs from.
fn paint_slots(
    scene: &mut Scene,
    transform: Affine,
    (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    toolbar: &WindowToolbar,
    slots: &Slots,
    (interaction, crumbs): (Option<&Interaction>, Option<&mut Breadcrumb>),
) -> Option<Rect> {
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
    for (i, (action, rect)) in toolbar.leading.iter().zip(&slots.leading).enumerate() {
        let Some(icon) = Icon::from_name(&action.icon) else {
            continue;
        };
        paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (*rect, icon),
            face(Slot::Leading(i), action.enabled, action.on),
        );
    }
    if let Some(views) = toolbar.views.as_ref()
        && !slots.views.is_empty()
    {
        let labels: Vec<&str> = views.choices.iter().map(|v| v.label.as_str()).collect();
        let hovered = interaction.and_then(|i| match &i.hover {
            Some(Slot::View(index)) => Some(*index),
            _ => None,
        });
        let face = SegmentedFace::new(
            &slots.views,
            &labels,
            views
                .chosen_index()
                .unwrap_or(super::toggle_group::NONE_CHOSEN),
        )
        .control_size(VIEWS_SIZE)
        .hovered(hovered);
        ToggleGroup::paint(scene, transform, tr, theme, &face);
    }
    for (i, (action, rect)) in toolbar.actions.iter().zip(&slots.actions).enumerate() {
        let Some(icon) = Icon::from_name(&action.icon) else {
            continue;
        };
        paint_icon_button(scene, transform, tr, theme, (*rect, icon), {
            let mut lifted = face(Slot::Action(i), action.enabled, action.on);
            lifted.hovered |= interaction.is_some_and(|it| it.menu_open == Some(Slot::Action(i)));
            lifted
        });
    }
    if slots.overflow.area() > 0.0 {
        let open = interaction.is_some_and(|i| i.menu_open == Some(Slot::Overflow));
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
    let location = toolbar.location.as_ref()?;
    if slots.location.area() <= 0.0 || editing {
        return None;
    }
    let lifted = LocationLift {
        hovered: interaction.is_some_and(|i| i.hover == Some(Slot::Location)),
        pressed: interaction.is_some_and(|i| {
            i.pressed == Some(Slot::Location) || i.menu_open == Some(Slot::Location)
        }),
    };
    paint_location(
        scene,
        transform,
        (tr, theme, presence),
        (location, crumbs),
        slots.location,
        lifted,
    )
}

/// How the pointer stands on the location.
#[derive(Clone, Copy, Debug, Default)]
struct LocationLift {
    hovered: bool,
    /// Pressed, or its picker is open.
    pressed: bool,
}

/// The location at rest: its icon, then its label, elided at the end. An
/// editable one stands in a field's frame, so it reads as one to click; one
/// with choices reads as a picker, its label and a chevron on the bar's
/// plain button face, and returns where it painted that.
fn paint_location(
    scene: &mut Scene,
    transform: Affine,
    (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    (location, crumbs): (
        &crate::window_toolbar::ToolbarLocation,
        Option<&mut Breadcrumb>,
    ),
    rect: Rect,
    lift: LocationLift,
) -> Option<Rect> {
    if !location.editable && !location.choices.is_empty() {
        return Some(paint_location_picker(
            scene,
            transform,
            (tr, theme),
            location,
            rect,
            lift,
        ));
    }
    let mut text_rect = rect;
    if location.editable {
        let face = FieldFace::new(rect, "").hovered(lift.hovered);
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
        return None;
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
        return None;
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
    None
}

/// A location with choices: its label (elided at the end) and a trailing
/// chevron, on the plain button face the bar's buttons lift with, as wide as
/// they need and never wider than the slot. Returns the face's rect.
fn paint_location_picker(
    scene: &mut Scene,
    transform: Affine,
    (tr, theme): (&mut TextRenderer, &Theme),
    location: &crate::window_toolbar::ToolbarLocation,
    rect: Rect,
    lift: LocationLift,
) -> Rect {
    let pad = f64::from(theme.tokens.spacing.s2);
    let gap = f64::from(theme.tokens.spacing.s1);
    let style = theme.text(TextRole::Body);
    let (shown, ink) = if location.label.is_empty() {
        (
            location.placeholder.as_str(),
            theme.tokens.colors.text_tertiary,
        )
    } else {
        (location.label.as_str(), theme.tokens.colors.text_primary)
    };
    let room = (rect.width() - pad * 2.0 - gap - LOCATION_ICON_PX).max(0.0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a location's width is a bounded on-screen size, far inside f32"
    )]
    let line = tr.elide_to_width_style(shown, room as f32, &style);
    let (w, h) = tr.measure_style(&line, None, &style);
    let label_w = f64::from(w).min(room);
    let face_rect = Rect::new(
        rect.x0,
        rect.y0,
        (rect.x0 + pad * 2.0 + label_w + gap + LOCATION_ICON_PX).min(rect.x1),
        rect.y1,
    );
    let face = crate::widget::ButtonFace::new(face_rect, "")
        .role(crate::widget::ButtonRole::Plain)
        .control_size(crate::widget::ControlSize::Small)
        .hovered(lift.hovered)
        .pressed(lift.pressed);
    crate::widget::Button::paint(scene, transform, tr, theme, &face);
    if !line.is_empty() {
        let y = rect.center().y - f64::from(h) / 2.0;
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine transform composition over f64 screen coordinates"
        )]
        let origin = transform * Affine::translate((face_rect.x0 + pad, y));
        tr.draw_style(scene, &line, ink, origin, None, &style);
    }
    let chevron_x = face_rect.x0 + pad + label_w + gap;
    crate::icons::draw_icon(
        scene,
        transform,
        Rect::new(
            chevron_x,
            rect.center().y - LOCATION_ICON_PX / 2.0,
            chevron_x + LOCATION_ICON_PX,
            rect.center().y + LOCATION_ICON_PX / 2.0,
        ),
        Icon::ChevronDown,
        theme.tokens.colors.text_dim,
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    face_rect
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
                menu: false,
                items: Vec::new(),
            }],
            ..WindowToolbar::default()
        }
    }

    fn row() -> ToolbarRow {
        let mut row = ToolbarRow::new();
        row.show(
            7,
            Some(&browser()),
            Theme::dark().motion(),
            crate::motion::frame_now(),
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
        assert_eq!(
            row.take_drafts(),
            vec![(
                7,
                ToolbarEvent::Edit {
                    text: "https://duckduckgo.com/".into()
                }
            )],
            "the app hears its location put back"
        );
    }

    /// A terminal's slots: where its shell runs, one of a few places.
    fn terminal() -> WindowToolbar {
        use crate::window_toolbar::LocationChoice;
        let choice = |label: &str, value: &str, current: bool, separated: bool| LocationChoice {
            label: label.into(),
            value: value.into(),
            current,
            separated,
        };
        WindowToolbar {
            location: Some(ToolbarLocation {
                label: "Your shell on this device".into(),
                choices: vec![
                    choice("Your shell on this device", "target:host", true, false),
                    choice("work", "target:environment:work", false, false),
                    choice("Manage environments…", "manage", false, true),
                ],
                ..ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        }
    }

    #[test]
    fn a_location_with_choices_opens_its_picker_and_commits_the_one_chosen() {
        let mut row = ToolbarRow::new();
        row.show(
            9,
            Some(&terminal()),
            Theme::dark().motion(),
            crate::motion::frame_now(),
        );
        row.set_popup_area(Rect::new(0.0, 0.0, 800.0, 600.0));
        row.lay_out(Rect::new(40.0, 0.0, 640.0, 40.0));
        let location = row.slots().location.center();
        assert!(row.press(location));
        assert!(!row.is_editing(), "a picker never turns into a field");
        assert!(!row.choices_open(), "it opens as the press ends");
        assert!(row.release(location));
        assert!(row.choices_open());
        assert_eq!(
            row.menu_labels(),
            vec![
                "Your shell on this device".to_owned(),
                "work".to_owned(),
                "Manage environments…".to_owned()
            ]
        );
        let picker = row
            .accessibility()
            .into_iter()
            .find(|node| node.action_target.as_deref() == Some(LOCATION_TARGET))
            .expect("the picker reaches assistive technology");
        assert_eq!(picker.state.expanded, Some(true));
        assert_eq!(picker.children.len(), 3);
        assert_eq!(picker.children[0].state.checked, Some(true));
        assert!(row.perform(&format!("{CHOICE_TARGET_PREFIX}1")));
        assert!(!row.choices_open(), "choosing closes the picker");
        assert_eq!(
            row.take_events(),
            vec![(
                9,
                ToolbarEvent::Commit {
                    text: "target:environment:work".into()
                }
            )]
        );
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

    /// A calendar's slots: the sidebar, previous, next and today, the
    /// period as the location, the view switcher, New event and an import
    /// for the menu.
    fn calendar() -> WindowToolbar {
        use crate::window_toolbar::{ToolbarView, ToolbarViews};
        let view = |id: &str, label: &str| ToolbarView {
            id: id.into(),
            label: label.into(),
        };
        WindowToolbar {
            sidebar: Some(true),
            leading: vec![
                ToolbarAction::new("previous", "chevron-left", "Previous week"),
                ToolbarAction::new("next", "chevron-right", "Next week"),
                ToolbarAction::new("today", "calendar", "Today"),
            ],
            location: Some(ToolbarLocation {
                label: "Oct 19 – 25, 2026".into(),
                ..ToolbarLocation::default()
            }),
            views: Some(ToolbarViews {
                name: "View".into(),
                choices: vec![
                    view("month", "Month"),
                    view("week", "Week"),
                    view("day", "Day"),
                ],
                chosen: "week".into(),
            }),
            actions: vec![
                ToolbarAction::new("import", "download", "Import").in_menu(),
                ToolbarAction::new("new-event", "plus", "New event"),
            ],
            ..WindowToolbar::default()
        }
        .bounded()
    }

    fn calendar_row(width: f64) -> ToolbarRow {
        let mut row = ToolbarRow::new();
        row.show(
            3,
            Some(&calendar()),
            Theme::dark().motion(),
            crate::motion::frame_now(),
        );
        row.lay_out(Rect::new(0.0, 0.0, width, 40.0));
        assert!(
            row.measure_views(&Theme::dark()),
            "the switcher is measured"
        );
        row
    }

    #[test]
    fn leading_actions_follow_the_navigation_and_views_stand_before_the_actions() {
        let row = calendar_row(1000.0);
        let slots = row.slots();
        assert_eq!(slots.leading.len(), 3);
        assert!(slots.sidebar.x1 < slots.leading[0].x0);
        assert!(slots.leading[2].x1 < slots.location.x0);
        assert_eq!(slots.views.len(), 3, "the switcher has room in the row");
        assert!(slots.location.x1 < slots.views_strip().x0);
        assert!(slots.views_strip().x1 < slots.actions[0].x0);
        assert_eq!(slots.actions.len(), 1, "Import is the menu's only");
        assert!(slots.overflow.area() > 0.0, "the menu holds Import");
        let widths: Vec<f64> = slots.views.iter().map(Rect::width).collect();
        assert!(
            widths.iter().all(|w| (w - widths[0]).abs() < 0.5),
            "{widths:?}"
        );
    }

    #[test]
    fn a_view_is_chosen_in_the_row_or_from_the_menu_when_the_row_has_no_room() {
        let mut row = calendar_row(1000.0);
        let month = row.slots().views[0].center();
        assert!(row.press(month));
        assert!(row.release(month));
        let today = row.slots().leading[2].center();
        assert!(row.press(today));
        assert!(row.release(today));
        assert_eq!(
            row.take_events(),
            vec![
                (3, ToolbarEvent::Action { id: "month".into() }),
                (3, ToolbarEvent::Action { id: "today".into() }),
            ]
        );

        let mut narrow = calendar_row(420.0);
        assert!(
            narrow.slots().views.is_empty(),
            "no room: the views go in the menu"
        );
        assert!(narrow.slots().overflow.area() > 0.0);
        assert!(narrow.open_overflow());
        assert_eq!(
            narrow.menu_entries.first().copied().flatten(),
            Some(MenuEntry::View(0)),
            "the menu starts with the views"
        );
        assert!(narrow.perform(&format!("{VIEW_TARGET_PREFIX}day")));
        assert!(narrow.perform(&format!("{LEADING_TARGET_PREFIX}previous")));
        assert_eq!(
            narrow.take_events(),
            vec![
                (3, ToolbarEvent::Action { id: "day".into() }),
                (
                    3,
                    ToolbarEvent::Action {
                        id: "previous".into()
                    }
                ),
            ]
        );
        let nodes = narrow.accessibility();
        let week = nodes
            .iter()
            .find(|node| node.name.as_deref() == Some("Week"))
            .expect("every view reaches assistive technology");
        assert_eq!(week.state.checked, Some(true));
    }

    #[test]
    fn a_menu_button_lists_its_items_and_the_overflow_menu_carries_them() {
        let toolbar = WindowToolbar {
            actions: vec![
                ToolbarAction::new("format", "type", "Format").with_items(vec![
                    ToolbarAction::new("bold", "bold", "Bold").on(true),
                    ToolbarAction::new("italic", "italic", "Italic"),
                ]),
                ToolbarAction::new("export", "upload", "Export").in_menu(),
            ],
            ..WindowToolbar::default()
        }
        .bounded();
        let mut row = ToolbarRow::new();
        row.show(
            5,
            Some(&toolbar),
            Theme::dark().motion(),
            crate::motion::frame_now(),
        );
        row.lay_out(Rect::new(0.0, 0.0, 800.0, 40.0));
        let format = row.slots().actions[0].center();
        assert!(row.press(format) && row.release(format));
        assert!(
            row.take_events().is_empty(),
            "a menu button sends nothing itself"
        );
        assert!(row.popup_open(), "its menu lists the items");
        assert_eq!(row.menu_entries.len(), 2);
        assert!(row.run_menu_entry(1));
        assert_eq!(
            row.take_events(),
            vec![(
                5,
                ToolbarEvent::Action {
                    id: "italic".into()
                }
            )]
        );
        assert!(
            row.perform(&format!("{ACTION_TARGET_PREFIX}bold")),
            "AT runs an item"
        );
        assert_eq!(
            row.take_events(),
            vec![(5, ToolbarEvent::Action { id: "bold".into() })]
        );

        let mut narrow = ToolbarRow::new();
        let crowded = WindowToolbar {
            location: Some(ToolbarLocation {
                label: "Cover letter".into(),
                ..ToolbarLocation::default()
            }),
            ..toolbar.clone()
        };
        narrow.show(
            5,
            Some(&crowded),
            Theme::dark().motion(),
            crate::motion::frame_now(),
        );
        narrow.lay_out(Rect::new(0.0, 0.0, 240.0, 40.0));
        assert!(
            narrow.slots().actions.is_empty(),
            "no room: Format goes in the menu"
        );
        assert!(narrow.open_overflow());
        assert!(
            narrow
                .menu_entries
                .iter()
                .flatten()
                .any(|entry| *entry == MenuEntry::Item(0, 1)),
            "the overflow menu carries Format's items"
        );
    }
}
