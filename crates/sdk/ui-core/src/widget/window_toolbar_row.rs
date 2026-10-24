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
//! they stood while the incoming ones fade in. Slots shown where none stood
//! appear at once.

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

mod access;
mod layout;
#[path = "window_toolbar_location_label.rs"]
mod location_label;
mod paint;
mod strip;
use layout::{LocationForms, MeasureKey, Measured, Slot, Slots, ViewsMeasure, location_chrome};
use location_label::{LocationLabel, edited_word};
#[cfg(test)]
pub(crate) use paint::location_shown;
#[cfg(test)]
use paint::shows_on;
pub(crate) use paint::{LocationLift, PickerText, paint_picker};
use paint::{labelled_face, picker_width};

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
pub const VIEWS_MENU_TARGET: &str = "window-toolbar-views";
pub const NOTICE_TARGET_PREFIX: &str = "window-toolbar-notice:";

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

/// What a row of the open menu (the overflow's, a menu button's, or the
/// location's picker) runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuEntry {
    /// Declared action `n`.
    Action(usize),
    /// Leading action `n`, where the row has no room for it.
    Leading(usize),
    View(usize),
    /// Item `.1` of menu button `.0`.
    Item(usize, usize),
    /// The location's choice `n`, committed.
    Choice(usize),
}

/// A menu row for `action`: its name, icon and state.
fn menu_item(action: &crate::window_toolbar::ToolbarAction) -> MenuItem {
    let mut item = if action.destructive {
        MenuItem::destructive_selection(action.name.clone())
    } else {
        MenuItem::selection(action.name.clone())
    }
    .with_enabled(action.enabled);
    if let Some(icon) = Icon::from_name(&action.icon) {
        item = item.with_icon(icon);
    }
    if action.toggles || action.on {
        item = item.with_checked(action.on);
    }
    item
}

/// The slots of the tab the window shows, in one row of its bar.
pub(crate) struct ToolbarRow {
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
    /// The views' sizes as the theme draws them, and the labels and text
    /// scale they were measured for.
    views_size: Option<Measured>,
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
    /// The window is compact: a notice says itself in its own strip under
    /// the row ([`strip::NoticeStrip`]), not in the location's place.
    compact: bool,
    strip: strip::NoticeStrip,
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
            compact: false,
            strip: strip::NoticeStrip::default(),
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
        // A notice stands in the location's place, unless the location is
        // being edited: the field keeps what the person is typing. In a
        // compact window it has a strip of its own under the row, and the
        // row stands as it does without one.
        let editing =
            self.field.is_some() && self.shown.as_ref().map(|(shown, _)| *shown) == Some(tab);
        let compact = self.compact;
        self.strip.offer(
            toolbar
                .and_then(|toolbar| toolbar.notice.as_ref())
                .filter(|_| compact),
            theme_motion,
        );
        let next = toolbar.map(|toolbar| {
            if editing || compact {
                (
                    tab,
                    WindowToolbar {
                        notice: None,
                        ..toolbar.clone()
                    },
                )
            } else {
                (tab, toolbar.presented())
            }
        });
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
            if previous_tab.is_some() {
                self.fade
                    .show(next_tab, theme_motion, MotionSource::Pointer, now);
            } else {
                // Nothing stood here to fade from: a window's first slots
                // arrive with it, and are at rest at once, so a window that
                // only declared them asks for no frames of its own.
                self.fade = CrossFade::new(next_tab);
            }
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

    /// Measure the view switcher and the location's forms as `theme` draws
    /// them, and lay the row out again when a size changed. Returns whether
    /// it did.
    pub(crate) fn measure_views(&mut self, theme: &Theme) -> bool {
        let labels: Vec<String> = self
            .toolbar()
            .and_then(|toolbar| toolbar.views.as_ref())
            .map(|views| views.choices.iter().map(|v| v.label.clone()).collect())
            .unwrap_or_default();
        let location = self.toolbar().and_then(|toolbar| toolbar.location.as_ref());
        let forms: Vec<String> = location
            .filter(|location| !location.label.is_empty() && location.segments.is_empty())
            .map(|location| {
                std::iter::once(location.label.clone())
                    .chain(location.shorter.iter().cloned())
                    .collect()
            })
            .unwrap_or_default();
        let leading: Vec<String> = self
            .toolbar()
            .map(|toolbar| {
                toolbar
                    .leading
                    .iter()
                    .map(|action| {
                        if action.labelled {
                            action.name.clone()
                        } else {
                            String::new()
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let key = MeasureKey {
            labels,
            leading,
            forms,
            editable: location.is_some_and(|location| location.editable),
            picker: location
                .is_some_and(|location| !location.editable && !location.choices.is_empty()),
            icon: location.is_some_and(|location| {
                location.icon.as_deref().and_then(Icon::from_name).is_some()
            }),
            notice: self
                .toolbar()
                .and_then(|toolbar| toolbar.notice.as_ref())
                .map(|notice| {
                    notice
                        .actions
                        .iter()
                        .map(|action| action.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
            text_scale: theme.text_scale.to_bits(),
        };
        if key.labels.is_empty() && key.forms.is_empty() && key.leading.iter().all(String::is_empty)
        {
            let had = self.views_size.take().is_some();
            if had {
                self.lay_out(self.row);
            }
            return had;
        }
        if self
            .views_size
            .as_ref()
            .is_some_and(|measured| measured.key == key)
        {
            return false;
        }
        let measured = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            let labels: Vec<&str> = key.labels.iter().map(String::as_str).collect();
            let tabs = SegmentedFace::tab_widths(&labels, VIEWS_SIZE, text, theme);
            // The notice's buttons as text buttons, each its label in it.
            let actions = key
                .notice
                .iter()
                .map(|name| labelled_face(Rect::ZERO, name).intrinsic_width(text, theme))
                .collect();
            let style = theme.text(TextRole::Body);
            let mut width = |label: &str| f64::from(text.measure_style(label, None, &style).0);
            let forms = LocationForms {
                text: key.forms.iter().map(|form| width(form)).collect(),
                chrome: location_chrome((key.editable, key.picker, key.icon), theme),
                actions,
            };
            let widest = labels.iter().map(|label| width(label)).fold(0.0, f64::max);
            let views = (!labels.is_empty()).then_some(ViewsMeasure {
                strip: (
                    tabs.iter().sum::<f64>().ceil(),
                    SegmentedFace::height(VIEWS_SIZE, theme).min(CONTROL_PX),
                ),
                menu: picker_width(widest, theme),
                location: Some(picker_width(0.0, theme)),
            });
            let leading = key
                .leading
                .iter()
                .map(|name| {
                    if name.is_empty() {
                        0.0
                    } else {
                        labelled_face(Rect::ZERO, name).intrinsic_width(text, theme)
                    }
                })
                .collect();
            (views, forms, tabs, leading)
        });
        let (views, forms, tabs, leading) = measured;
        self.views_size = Some(Measured {
            key,
            views,
            forms,
            tabs,
            leading,
        });
        self.lay_out(self.row);
        true
    }

    pub(crate) fn toolbar(&self) -> Option<&WindowToolbar> {
        self.shown.as_ref().map(|(_, toolbar)| toolbar)
    }

    /// Lay the slots out across `row`.
    pub(crate) fn lay_out(&mut self, row: Rect) {
        self.row = row;
        let unmeasured = LocationForms::default();
        let (views, forms, labels) = self
            .views_size
            .as_ref()
            .map_or((None, &unmeasured, &[][..]), |measured| {
                (measured.views, &measured.forms, measured.leading.as_slice())
            });
        self.slots = self
            .toolbar()
            .map(|toolbar| Slots::lay_out(toolbar, row, views, (forms, labels)))
            .unwrap_or_default();
        // The views are tabs, each as wide as its label (as `ViewTabs`
        // packs them), across the strip measured to hold them.
        if let Some(Measured { tabs, .. }) = self.views_size.as_ref()
            && let Some(strip_x0) = self.slots.views.first().map(|tab| tab.x0)
            && tabs.len() == self.slots.views.len()
        {
            let mut x = strip_x0;
            for (tab, width) in self.slots.views.iter_mut().zip(tabs) {
                *tab = Rect::new(x, tab.y0, x + width, tab.y1);
                x = tab.x1;
            }
        }
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
            Some(Slot::ViewsMenu) => self.slots.views_menu,
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

    /// Say a notice in a strip of its own under the row (`compact`), or in
    /// the location's place. Takes hold at the next [`Self::show`].
    pub(crate) fn set_compact(&mut self, compact: bool) {
        self.compact = compact;
    }

    /// How tall the notice's strip stands under the row (none: no strip).
    pub(crate) fn strip_height(&self) -> f64 {
        self.strip.height()
    }

    /// Measure the notice's strip as `theme` draws it across `width`.
    /// Returns whether its height changed.
    pub(crate) fn measure_strip(&mut self, theme: &Theme, width: f64) -> bool {
        self.strip.measure(theme, width)
    }

    /// Stand the notice's strip at `rect`, the room the bar keeps for it.
    pub(crate) fn place_strip(&mut self, rect: Rect) {
        self.strip.place(rect);
    }

    /// Where the notice's strip stands (`Rect::ZERO`: none).
    pub(crate) fn strip_rect(&self) -> Rect {
        self.strip.rect()
    }

    /// The notice up in the strip, where its text stands, whether whole,
    /// and its buttons (none: no strip).
    pub(crate) fn strip_notice(
        &self,
    ) -> Option<(&crate::window_toolbar::ToolbarNotice, Rect, bool, Vec<Rect>)> {
        let notice = self.strip.notice()?;
        Some((
            notice,
            self.strip.text_rect(),
            self.strip.whole(),
            self.strip.buttons(),
        ))
    }

    /// The slot at `p`: one of the strip's buttons, else one of the row's.
    fn hit_slot(&self, p: Point) -> Option<Slot> {
        if let Some(i) = self.strip.buttons().iter().position(|r| r.contains(p)) {
            return Some(Slot::Notice(i));
        }
        self.slots.hit(p)
    }

    /// Whether the notice up stands whole in the location, as measured
    /// (none: no notice is up, or it is not measured yet).
    pub(crate) fn notice_whole(&self) -> Option<bool> {
        self.toolbar()?.notice.as_ref()?;
        let forms = &self.views_size.as_ref()?.forms;
        let whole = forms.text.first()? + forms.chrome;
        Some(self.slots.location.width() + 0.5 >= whole)
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
            Slot::Sidebar => toolbar.sidebar.is_some() && !toolbar.sidebar_unavailable,
            Slot::Back => toolbar.navigation.is_some_and(|nav| nav.back),
            Slot::Forward => toolbar.navigation.is_some_and(|nav| nav.forward),
            Slot::Leading(i) => toolbar.leading.get(*i).is_some_and(|a| a.enabled),
            Slot::Location => toolbar.location.is_some(),
            Slot::View(i) => toolbar
                .views
                .as_ref()
                .is_some_and(|views| *i < views.choices.len()),
            Slot::ViewsMenu => self.slots.views_menu.area() > 0.0,
            Slot::Action(i) => toolbar.actions.get(*i).is_some_and(|a| a.enabled),
            Slot::Notice(i) => self.notice_action(*i).is_some_and(|a| a.enabled),
            Slot::Overflow => self.slots.overflow.area() > 0.0,
        }
    }

    /// Open the overflow button's menu: the view switcher's choices when
    /// the row has no room for it, the chosen one checked, then the leading
    /// actions and the actions the row does not show, by name, each with
    /// its icon and state.
    fn open_overflow(&mut self) -> bool {
        let Some(toolbar) = self.toolbar() else {
            return false;
        };
        let mut items = Vec::new();
        let mut entries = Vec::new();
        if self.views_in_overflow() {
            for (item, entry) in self.view_items() {
                items.push(item);
                entries.push(Some(entry));
            }
        }
        let folded: Vec<usize> = self.slots.folded_leading().collect();
        if !items.is_empty() && !folded.is_empty() {
            items.push(MenuItem::separator());
            entries.push(None);
        }
        for i in folded {
            if let Some(action) = toolbar.leading.get(i) {
                items.push(menu_item(action));
                entries.push(Some(MenuEntry::Leading(i)));
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

    /// Whether the overflow menu lists the views: the row has room neither
    /// for the switcher nor for the views menu.
    fn views_in_overflow(&self) -> bool {
        self.slots.views.is_empty() && self.slots.views_menu.area() <= 0.0
    }

    /// The views as a menu lists them, wherever it opens (the views menu,
    /// the location as their picker, the overflow menu): every view by its
    /// label, in order, the chosen one checked.
    fn view_items(&self) -> Vec<(MenuItem, MenuEntry)> {
        let Some(views) = self.toolbar().and_then(|toolbar| toolbar.views.as_ref()) else {
            return Vec::new();
        };
        views
            .choices
            .iter()
            .enumerate()
            .map(|(i, view)| {
                let item =
                    MenuItem::selection(view.label.clone()).with_checked(view.id == views.chosen);
                (item, MenuEntry::View(i))
            })
            .collect()
    }

    /// Open the views menu under it: every view by its label, the chosen
    /// one checked.
    fn open_views_menu(&mut self) -> bool {
        let (items, entries): (Vec<MenuItem>, Vec<Option<MenuEntry>>) = self
            .view_items()
            .into_iter()
            .map(|(item, entry)| (item, Some(entry)))
            .unzip();
        if items.is_empty() {
            return false;
        }
        let anchor = self.slots.views_menu;
        if anchor.area() <= 0.0 {
            return false;
        }
        self.open_menu(items, entries, Slot::ViewsMenu, anchor);
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

    /// The notice up, in the strip or in the location's place.
    fn notice_up(&self) -> Option<&crate::window_toolbar::ToolbarNotice> {
        self.strip
            .notice()
            .or_else(|| self.toolbar()?.notice.as_ref())
    }

    /// The notice's button `index`, while a notice with it is up.
    fn notice_action(&self, index: usize) -> Option<&crate::window_toolbar::ToolbarAction> {
        self.notice_up()?.actions.get(index)
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
            Some(MenuEntry::Leading(index)) => self.run_leading(index),
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
            Slot::Notice(i) => {
                let Some(id) = self.notice_action(*i).map(|action| action.id.clone()) else {
                    return false;
                };
                self.push(ToolbarEvent::Action { id });
            }
            Slot::Overflow => return self.open_overflow(),
            Slot::ViewsMenu => return self.open_views_menu(),
        }
        true
    }

    /// The pointer moved to `p`. Returns whether what shows changed.
    pub(crate) fn pointer_moved(&mut self, p: Point) -> bool {
        let hover = self.hit_slot(p);
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
        match self.hit_slot(p)? {
            Slot::Sidebar => Some((self.slots.sidebar, lookup(sidebar_key(toolbar)))),
            Slot::Back => Some((self.slots.back, lookup("window-toolbar-back"))),
            Slot::Forward => Some((self.slots.forward, lookup("window-toolbar-forward"))),
            Slot::Leading(i) => {
                let action = toolbar.leading.get(i)?;
                let rect = self.slots.leading.get(i).filter(|rect| rect.area() > 0.0)?;
                Some((*rect, action.name.clone()))
            }
            Slot::Action(i) => {
                let action = toolbar.actions.get(i)?;
                Some((*self.slots.actions.get(i)?, action.name.clone()))
            }
            Slot::Notice(i) => {
                let rect = self
                    .slots
                    .notice
                    .get(i)
                    .copied()
                    .or_else(|| self.strip.buttons().get(i).copied())?;
                Some((rect, self.notice_action(i)?.name.clone()))
            }
            Slot::Overflow => Some((self.slots.overflow, lookup("window-toolbar-more"))),
            // A segment, the views menu and the location say what they are.
            Slot::View(_) | Slot::ViewsMenu | Slot::Location => None,
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

    pub(crate) fn hit(&self, p: Point) -> bool {
        self.hit_slot(p).is_some()
    }

    /// A left press at `p`. Returns whether a slot took it. A press
    /// outside the field while it is edited puts the location back.
    pub(crate) fn press(&mut self, p: Point) -> bool {
        let slot = self.hit_slot(p);
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

    /// Whether the press under way is on one of the app's own slots that
    /// runs as the press ends (its sidebar toggle, a leading action, a mode,
    /// an action without a menu): a press on the app, after which the
    /// keyboard stays with the control in the app that had it. The
    /// location field and the bar's menus take the keyboard themselves.
    pub(crate) fn pressing_app_slot(&self) -> bool {
        match &self.pressed {
            Some(Slot::Sidebar | Slot::Leading(_) | Slot::View(_) | Slot::Notice(_)) => true,
            Some(Slot::Action(i)) => self
                .toolbar()
                .and_then(|toolbar| toolbar.actions.get(*i))
                .is_some_and(|action| action.items.is_empty()),
            _ => false,
        }
    }

    /// The left button came up at `p`: a press released on the slot it
    /// started on runs it.
    pub(crate) fn release(&mut self, p: Point) -> bool {
        let Some(pressed) = self.pressed.take() else {
            return false;
        };
        if self.hit_slot(p).as_ref() == Some(&pressed) {
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
            || self.strip.needs_frame()
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

    /// After an entry of the overflow menu or a menu button's ran (`ran`)
    /// as assistive technology activated it: its open menu closes, as it
    /// does when its row is picked. Returns `ran`.
    fn ran_from_menu(&mut self, ran: bool) -> bool {
        if ran && self.menu.is_open() && !self.menu_open_for(&Slot::Location) {
            self.menu.close();
        }
        ran
    }

    fn menu_open_for(&self, slot: &Slot) -> bool {
        self.menu.is_open() && self.menu_slot.as_ref() == Some(slot)
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

#[cfg(test)]
mod tests;
