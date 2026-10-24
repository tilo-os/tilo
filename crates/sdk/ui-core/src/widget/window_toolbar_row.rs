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

#[path = "window_toolbar_location_label.rs"]
mod location_label;
use location_label::{LocationLabel, edited_word};

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
    /// The views folded into a menu: the chosen one named, the others a
    /// press away.
    ViewsMenu,
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
    /// The view switcher's segments (none: folded into the views menu, or
    /// into the overflow menu).
    pub views: Vec<Rect>,
    /// The views menu, where the row has no room for the switcher
    /// (`Rect::ZERO`: none).
    pub views_menu: Rect,
    /// The actions standing in the row, the first of the declared ones.
    pub actions: Vec<Rect>,
    /// The overflow button (`Rect::ZERO`: every action is in the row).
    pub overflow: Rect,
    /// Which form of its label the location shows
    /// ([`crate::window_toolbar::ToolbarLocation::form`]).
    pub form: usize,
}

/// What the row lays out beside the toolbar: the view switcher's size and
/// the views menu's width, as the theme draws their labels (none before
/// they are measured).
pub(crate) type ViewsSize = Option<ViewsMeasure>;

/// The two forms the views take in the row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ViewsMeasure {
    /// The segmented switcher's width and height.
    pub strip: (f64, f64),
    /// The views menu's width: as wide as its widest label needs, so
    /// choosing another view never moves the row.
    pub menu: f64,
    /// What the location as the views' picker stands beside its label:
    /// its face's padding and chevron (none: the location cannot pick the
    /// views, [`picks_views`]). Laid out, the least width of the picker:
    /// that beside the form the location shows.
    pub location: Option<f64>,
}

/// The location's label and its narrower forms as the theme draws them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LocationForms {
    /// Each form's text width, the label's first.
    pub text: Vec<f64>,
    /// What stands beside the text in the location at rest: its icon, a
    /// field's frame.
    pub chrome: f64,
}

impl LocationForms {
    /// The widest form that stands whole at rest in `room`, else the
    /// narrowest.
    fn fitting(&self, room: f64) -> usize {
        self.text
            .iter()
            .position(|width| width + self.chrome <= room)
            .unwrap_or_else(|| self.text.len().saturating_sub(1))
    }

    /// The room the location at rest keeps for `form` to stand whole: none
    /// where it has no narrower form to choose, or `form` is cut short in
    /// `room` all the same.
    fn least(&self, form: usize, room: f64) -> f64 {
        match self.text.get(form) {
            Some(width) if self.text.len() > 1 && width + self.chrome <= room => {
                width + self.chrome
            }
            _ => 0.0,
        }
    }

    /// The text width of `form` (nothing measured: none).
    fn width(&self, form: usize) -> f64 {
        self.text.get(form).copied().unwrap_or(0.0)
    }
}

/// What the views and the location's forms were measured for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MeasureKey {
    labels: Vec<String>,
    forms: Vec<String>,
    editable: bool,
    picker: bool,
    icon: bool,
    text_scale: u32,
}

/// What stands beside the label's text in the location at rest
/// ([`paint_location`]): a picker's face and chevron where it chooses
/// among places, a field's frame where it is edited, its icon.
fn location_chrome((editable, picker, icon): (bool, bool, bool), theme: &Theme) -> f64 {
    if picker {
        return picker_width(0.0, theme);
    }
    let mut chrome = 0.0;
    if editable {
        let rect = Rect::new(0.0, 0.0, COMFORTABLE_LOCATION_PX, CONTROL_PX);
        chrome += rect.width() - FieldFace::new(rect, "").text_rect(theme).width();
    }
    if icon {
        chrome += LOCATION_ICON_PX + f64::from(theme.tokens.spacing.s2);
    }
    chrome
}

/// Where the views stand in one layout of the row.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ViewsPlace {
    /// The segmented switcher, this wide and tall.
    Strip(f64, f64),
    /// The views menu, this wide.
    Menu(f64),
    /// The location as the views' picker, at least this wide.
    InLocation(f64),
    /// The overflow menu's first rows (or there are no views).
    Overflow,
}

/// Where the views go once the switcher has no room: into the location,
/// as its picker, where it can be one ([`picks_views`]), else into a views
/// menu of their own.
fn folded_views(measured: ViewsMeasure) -> ViewsPlace {
    measured
        .location
        .map_or(ViewsPlace::Menu(measured.menu), ViewsPlace::InLocation)
}

/// Whether `toolbar`'s location can be the views' picker: a plain label,
/// neither edited, nor a picker of its own places, nor a breadcrumb.
fn picks_views(toolbar: &WindowToolbar) -> bool {
    toolbar.location.as_ref().is_some_and(|location| {
        !location.editable
            && location.choices.is_empty()
            && location.segments.is_empty()
            && !location.label.is_empty()
    })
}

/// What one layout keeps in the row: the first `plain` leading actions
/// that do not move through the content, the first `navigating` that do,
/// and the first `inline` actions.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Kept {
    plain: usize,
    navigating: usize,
    inline: usize,
}

impl Kept {
    /// Every leading action, and the first `inline` actions.
    fn all(toolbar: &WindowToolbar, inline: usize) -> Self {
        let navigating = toolbar
            .leading
            .iter()
            .filter(|action| action.navigates)
            .count();
        Self {
            plain: toolbar.leading.len().saturating_sub(navigating),
            navigating,
            inline,
        }
    }

    /// Whether leading action `index` stands in the row.
    fn shows(self, toolbar: &WindowToolbar, index: usize) -> bool {
        let Some(action) = toolbar.leading.get(index) else {
            return false;
        };
        let rank = toolbar
            .leading
            .iter()
            .take(index)
            .filter(|other| other.navigates == action.navigates)
            .count();
        rank < if action.navigates {
            self.navigating
        } else {
            self.plain
        }
    }
}

impl Slots {
    /// Lay `toolbar`'s slots out across `row`, centred on its centre line:
    /// navigation and the leading actions leading, the views and the
    /// actions trailing, the location filling between. At most
    /// [`crate::window_toolbar::MAX_ACTIONS`] actions stand in the row;
    /// while the location would be narrower than
    /// [`COMFORTABLE_LOCATION_PX`] (or, without one, while the buttons would
    /// overlap) the last of them move into the overflow menu, as do the
    /// declared actions past that many and those declared for the menu, and
    /// the view switcher folds into the views menu. Where that leaves the
    /// location narrower than [`MIN_LOCATION_PX`], the rest goes into the
    /// overflow menu in [`Slots::yielding`]'s order, so the location stays
    /// in the row, elided. One action never goes into the menu alone: the
    /// menu's button would take its place and hide it.
    ///
    /// The location shows the widest form of its label that stands whole
    /// beside the navigation, with everything that yields before it in the
    /// menu, and that yields only where that form would not stand whole:
    /// a wider row never shows a narrower form. The location is the views'
    /// picker only where that same form stands whole beside its chevron.
    pub(crate) fn lay_out(
        toolbar: &WindowToolbar,
        row: Rect,
        views: ViewsSize,
        forms: &LocationForms,
    ) -> Self {
        if row.width() <= 0.0 || row.height() <= 0.0 {
            return Self::default();
        }
        // The room the location has beside the navigation alone, everything
        // that yields before it ([`Self::yielding`]) in the menu.
        let navigation = Kept {
            plain: 0,
            inline: 0,
            ..Kept::all(toolbar, 0)
        };
        let room = Self::with_inline(toolbar, row, navigation, ViewsPlace::Overflow)
            .location
            .width();
        let form = forms.fitting(room);
        let least = forms.least(form, room);
        let mut slots = Self::arranged(toolbar, row, views, (forms.width(form), least));
        slots.form = form;
        slots
    }

    /// [`Self::lay_out`] for a location showing a form `form_w` wide, which
    /// at rest stands whole in a location `least` wide.
    fn arranged(
        toolbar: &WindowToolbar,
        row: Rect,
        views: ViewsSize,
        (form_w, least): (f64, f64),
    ) -> Self {
        let row_actions = toolbar
            .row_actions()
            .min(crate::window_toolbar::MAX_ACTIONS);
        let measured = views
            .filter(|_| {
                toolbar
                    .views
                    .as_ref()
                    .is_some_and(|views| !views.choices.is_empty())
            })
            .map(|m| ViewsMeasure {
                location: m
                    .location
                    .filter(|_| picks_views(toolbar))
                    .map(|chrome| form_w.ceil() + chrome),
                ..m
            });
        // A menu holding one action alone saves no width: its button takes
        // the action's place and hides it.
        let lone = |inline: usize, place: ViewsPlace| {
            row_actions - inline == 1
                && toolbar.actions.len() == row_actions
                && (toolbar.views.is_none() || place != ViewsPlace::Overflow)
        };
        let places = match measured {
            Some(m) => vec![ViewsPlace::Strip(m.strip.0, m.strip.1), folded_views(m)],
            None => vec![ViewsPlace::Overflow],
        };
        let all = Kept::all(toolbar, row_actions);
        for &place in &places {
            let mut inline = row_actions;
            loop {
                let slots = Self::with_inline(toolbar, row, Kept { inline, ..all }, place);
                let room = if toolbar.location.is_some() {
                    slots.location.width() >= COMFORTABLE_LOCATION_PX.max(least)
                } else {
                    slots.fits(row)
                };
                let placed =
                    !matches!(place, ViewsPlace::InLocation(_)) || slots.views_menu.area() > 0.0;
                if room && placed && !lone(inline, place) {
                    return slots;
                }
                if inline == 0 {
                    break;
                }
                inline -= 1;
            }
        }
        let mut last = Self::default();
        for (kept, place) in Self::yielding(toolbar, measured) {
            let inline =
                if kept.plain == all.plain && kept.navigating == all.navigating && lone(0, place) {
                    row_actions
                } else {
                    0
                };
            let slots = Self::with_inline(toolbar, row, Kept { inline, ..kept }, place);
            let located = match place {
                ViewsPlace::InLocation(least) => slots.location.width() >= least,
                _ => {
                    toolbar.location.is_none()
                        || (slots.location.area() > 0.0 && slots.location.width() >= least)
                }
            };
            if slots.fits(row) && located {
                return slots;
            }
            last = slots;
        }
        last
    }

    /// What goes into the overflow menu, in order, where the row is too
    /// narrow for the location beside everything else: the plain leading
    /// actions, the last first; then the views (the views menu's place);
    /// then the leading actions that move through the content, the last
    /// first. The location yields to none of them: it stays, elided.
    fn yielding(toolbar: &WindowToolbar, measured: ViewsSize) -> Vec<(Kept, ViewsPlace)> {
        let all = Kept::all(toolbar, 0);
        let views = measured.map_or(ViewsPlace::Overflow, folded_views);
        let plain = (0..=all.plain)
            .rev()
            .map(|plain| (Kept { plain, ..all }, views));
        let navigating = (0..=all.navigating).rev().map(|navigating| {
            let kept = Kept {
                plain: 0,
                navigating,
                inline: 0,
            };
            (kept, ViewsPlace::Overflow)
        });
        plain.chain(navigating).collect()
    }

    /// The slots with the leading actions and the actions `kept` keeps in
    /// the row and the rest in the overflow menu, and the views at `place`.
    fn with_inline(toolbar: &WindowToolbar, row: Rect, kept: Kept, place: ViewsPlace) -> Self {
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
        let mut folded = false;
        let mut any = false;
        for index in 0..toolbar.leading.len() {
            if kept.shows(toolbar, index) {
                let rect = square(x0);
                x0 = rect.x1 + CONTROL_GAP;
                slots.leading.push(rect);
                any = true;
            } else {
                slots.leading.push(Rect::ZERO);
                folded = true;
            }
        }
        if any {
            x0 = x0 - CONTROL_GAP + SECTION_GAP;
        }
        let inline = kept.inline;
        let mut x1 = row.x1;
        let mut trailing = false;
        let in_menu = toolbar.actions.len() > inline
            || folded
            || (toolbar.views.is_some() && place == ViewsPlace::Overflow);
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
        let choices = toolbar
            .views
            .as_ref()
            .map_or(0, |views| views.choices.len());
        match place {
            ViewsPlace::Strip(width, height) if choices > 0 => {
                let strip = Rect::new(
                    x1 - width,
                    row.center().y - height / 2.0,
                    x1,
                    row.center().y + height / 2.0,
                );
                slots.views = equal_segments(strip, choices);
                x1 = strip.x0 - SECTION_GAP;
            }
            ViewsPlace::Menu(width) if choices > 0 => {
                slots.views_menu = Rect::new(x1 - width, y0, x1, y0 + CONTROL_PX);
                x1 = slots.views_menu.x0 - SECTION_GAP;
            }
            _ => {}
        }
        if toolbar.location.is_some() && x1 - x0 >= MIN_LOCATION_PX {
            slots.location = Rect::new(x0, y0, x1, y0 + CONTROL_PX);
        }
        if let ViewsPlace::InLocation(least) = place
            && choices > 0
            && slots.location.width() >= least
        {
            slots.views_menu = slots.location;
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
            .chain((self.views_menu.area() > 0.0).then_some(&self.views_menu))
            .chain((self.overflow.area() > 0.0).then_some(&self.overflow))
            .all(|rect| rect.x0 >= lead)
    }

    /// Whether the location is the views' picker.
    pub(crate) fn views_in_location(&self) -> bool {
        self.views_menu.area() > 0.0 && self.views_menu == self.location
    }

    /// The leading actions the row has no room for, in their order.
    fn folded_leading(&self) -> impl Iterator<Item = usize> + '_ {
        self.leading
            .iter()
            .enumerate()
            .filter(|(_, rect)| rect.area() <= 0.0)
            .map(|(index, _)| index)
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
        if self.views_menu.contains(p) {
            return Some(Slot::ViewsMenu);
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
    views_size: Option<(MeasureKey, ViewsSize, LocationForms, Vec<f64>)>,
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
        let key = MeasureKey {
            labels,
            forms,
            editable: location.is_some_and(|location| location.editable),
            picker: location
                .is_some_and(|location| !location.editable && !location.choices.is_empty()),
            icon: location.is_some_and(|location| {
                location.icon.as_deref().and_then(Icon::from_name).is_some()
            }),
            text_scale: theme.text_scale.to_bits(),
        };
        if key.labels.is_empty() && key.forms.is_empty() {
            let had = self.views_size.take().is_some();
            if had {
                self.lay_out(self.row);
            }
            return had;
        }
        if self
            .views_size
            .as_ref()
            .is_some_and(|(measured, ..)| *measured == key)
        {
            return false;
        }
        let measured = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            let labels: Vec<&str> = key.labels.iter().map(String::as_str).collect();
            let tabs = SegmentedFace::tab_widths(&labels, VIEWS_SIZE, text, theme);
            let style = theme.text(TextRole::Body);
            let mut width = |label: &str| f64::from(text.measure_style(label, None, &style).0);
            let forms = LocationForms {
                text: key.forms.iter().map(|form| width(form)).collect(),
                chrome: location_chrome((key.editable, key.picker, key.icon), theme),
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
            (views, forms, tabs)
        });
        let (views, forms, tabs) = measured;
        self.views_size = Some((key, views, forms, tabs));
        self.lay_out(self.row);
        true
    }

    pub(crate) fn toolbar(&self) -> Option<&WindowToolbar> {
        self.shown.as_ref().map(|(_, toolbar)| toolbar)
    }

    /// Lay the slots out across `row`.
    pub(crate) fn lay_out(&mut self, row: Rect) {
        self.row = row;
        let (views, forms) = self
            .views_size
            .as_ref()
            .map_or((None, None), |(_, views, forms, _)| (*views, Some(forms)));
        let unmeasured = LocationForms::default();
        self.slots = self
            .toolbar()
            .map(|toolbar| Slots::lay_out(toolbar, row, views, forms.unwrap_or(&unmeasured)))
            .unwrap_or_default();
        // The views are tabs, each as wide as its label (as `ViewTabs`
        // packs them), across the strip measured to hold them.
        if let Some((.., tabs)) = self.views_size.as_ref()
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
            Slot::Overflow => return self.open_overflow(),
            Slot::ViewsMenu => return self.open_views_menu(),
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
                let rect = self.slots.leading.get(i).filter(|rect| rect.area() > 0.0)?;
                Some((*rect, action.name.clone()))
            }
            Slot::Action(i) => {
                let action = toolbar.actions.get(i)?;
                Some((*self.slots.actions.get(i)?, action.name.clone()))
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

    /// Whether the press under way is on one of the app's own slots that
    /// runs as the press ends (its sidebar toggle, a leading action, a mode,
    /// an action without a menu): a press on the app, after which the
    /// keyboard stays with the control in the app that had it. The
    /// location field and the bar's menus take the keyboard themselves.
    pub(crate) fn pressing_app_slot(&self) -> bool {
        match &self.pressed {
            Some(Slot::Sidebar | Slot::Leading(_) | Slot::View(_)) => true,
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
                !toolbar.sidebar_unavailable,
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
        for (action, rect) in toolbar
            .leading
            .iter()
            .zip(&self.slots.leading)
            .filter(|(_, rect)| rect.area() > 0.0)
        {
            let mut node = button(
                action.name.clone(),
                format!("{LEADING_TARGET_PREFIX}{}", action.id),
                *rect,
                action.enabled,
            );
            if action.toggles || action.on {
                node.role = Role::ToggleButton;
                node.state.checked = Some(action.on);
            }
            nodes.push(node);
        }
        if let Some(location) = toolbar.location.as_ref()
            && self.slots.location.area() > 0.0
            && !self.slots.views_in_location()
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
                if location.edited {
                    node = node.with_description(edited_word());
                }
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
        // The views the row shows as tabs, the chosen one selected; without room
        // they are the overflow menu's.
        if let Some(views) = toolbar.views.as_ref() {
            for (view, rect) in views.choices.iter().zip(&self.slots.views) {
                let mut node = button(
                    view.label.clone(),
                    format!("{VIEW_TARGET_PREFIX}{}", view.id),
                    *rect,
                    true,
                );
                node.role = Role::Tab;
                node.state.selected = view.id == views.chosen;
                if !views.name.is_empty() {
                    node = node.with_description(views.name.clone());
                }
                nodes.push(node);
            }
            // Folded, the views are a menu that names the chosen one; its
            // items run while it is closed, as the overflow menu's do.
            if self.slots.views_menu.area() > 0.0 {
                let chosen = views.chosen_index().and_then(|i| views.choices.get(i));
                let views_name = if views.name.is_empty() {
                    crate::i18n::I18nManager::lookup_plain("window-toolbar-views")
                } else {
                    views.name.clone()
                };
                // As the views' picker the location is named by what it
                // shows, its period, and says it picks the view.
                let in_location = toolbar
                    .location
                    .as_ref()
                    .filter(|_| self.slots.views_in_location());
                let name = in_location.map_or_else(|| views_name.clone(), |l| l.label.clone());
                let mut menu = button(
                    name.clone(),
                    VIEWS_MENU_TARGET.into(),
                    self.slots.views_menu,
                    true,
                )
                .with_optional_value(chosen.map(|view| view.label.clone()));
                if in_location.is_some() {
                    menu = menu.with_description(views_name);
                }
                menu.state.expanded = Some(self.menu_open_for(&Slot::ViewsMenu));
                menu = menu.with_children(vec![self.menu_node(
                    name,
                    &Slot::ViewsMenu,
                    self.slots.views_menu,
                    (0..views.choices.len()).map(MenuEntry::View),
                )]);
                nodes.push(menu);
            }
        }
        // The actions the row shows; a menu button's items are its menu's.
        for ((i, action), rect) in toolbar.actions.iter().enumerate().zip(&self.slots.actions) {
            let mut node = button(
                action.name.clone(),
                format!("{ACTION_TARGET_PREFIX}{}", action.id),
                *rect,
                action.enabled,
            );
            if action.toggles || action.on {
                node.role = Role::ToggleButton;
                node.state.checked = Some(action.on);
            }
            if !action.items.is_empty() {
                let slot = Slot::Action(i);
                node.state.expanded = Some(self.menu_open_for(&slot));
                let entries = (0..action.items.len()).map(|j| MenuEntry::Item(i, j));
                node = node.with_children(vec![self.menu_node(
                    action.name.clone(),
                    &slot,
                    *rect,
                    entries,
                )]);
            }
            nodes.push(node);
        }
        // What the row has no room for is the overflow menu's. Its items
        // are there for assistive technology and automation to run even
        // while it is closed, hidden from a reader until it opens, each
        // where its row is drawn while it is open.
        if self.slots.overflow.area() > 0.0 {
            let name = crate::i18n::I18nManager::lookup_plain("window-toolbar-more");
            let mut more = button(
                name.clone(),
                OVERFLOW_TARGET.into(),
                self.slots.overflow,
                true,
            );
            more.state.expanded = Some(self.menu_open_for(&Slot::Overflow));
            let entries = self.overflow_entries();
            if !entries.is_empty() {
                more = more.with_children(vec![self.menu_node(
                    name,
                    &Slot::Overflow,
                    self.slots.overflow,
                    entries.into_iter(),
                )]);
            }
            nodes.push(more);
        }
        nodes
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

    /// What the overflow menu lists, in its order: the views the row has
    /// no room for, then the leading actions and the actions it does not
    /// show, a menu button's items in its place.
    fn overflow_entries(&self) -> Vec<MenuEntry> {
        let Some(toolbar) = self.toolbar() else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        if self.views_in_overflow()
            && let Some(views) = toolbar.views.as_ref()
        {
            entries.extend((0..views.choices.len()).map(MenuEntry::View));
        }
        entries.extend(self.slots.folded_leading().map(MenuEntry::Leading));
        for (i, action) in toolbar
            .actions
            .iter()
            .enumerate()
            .skip(self.slots.actions.len())
        {
            if action.items.is_empty() {
                entries.push(MenuEntry::Action(i));
            } else {
                entries.extend((0..action.items.len()).map(|j| MenuEntry::Item(i, j)));
            }
        }
        entries
    }

    /// `slot`'s menu, named `name`, as assistive technology reaches it:
    /// one item per entry, each running what its row runs. While the menu
    /// is open each item is where its row is drawn; while it is closed the
    /// items are hidden from a reader and stand at `anchor`, the control
    /// that opens the menu.
    fn menu_node(
        &self,
        name: String,
        slot: &Slot,
        anchor: Rect,
        entries: impl Iterator<Item = MenuEntry>,
    ) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
        let open = self.menu_open_for(slot);
        let entries: Vec<MenuEntry> = entries.collect();
        let count = entries.len();
        let items = entries
            .iter()
            .enumerate()
            .filter_map(|(position, entry)| {
                let (name, target, enabled, checked) = self.entry_semantics(*entry)?;
                let rect = open
                    .then(|| {
                        self.menu_entries
                            .iter()
                            .position(|row| *row == Some(*entry))
                            .and_then(|row| self.menu.item_rect(row))
                    })
                    .flatten()
                    .unwrap_or(anchor);
                Some(
                    AccessNode::new(Role::MenuItem)
                        .with_name(name)
                        .with_action_target(target)
                        .with_bounds(rect)
                        .with_position(position.saturating_add(1), count)
                        .with_state(AccessState {
                            focusable: enabled && open,
                            disabled: !enabled,
                            checked,
                            hidden: !open,
                            ..Default::default()
                        }),
                )
            })
            .collect();
        AccessNode::new(Role::Menu)
            .with_name(name)
            .with_children(items)
            .with_state(AccessState {
                hidden: !open,
                ..Default::default()
            })
    }

    /// A menu entry's name, action target, whether it runs, and its check.
    fn entry_semantics(&self, entry: MenuEntry) -> Option<(String, String, bool, Option<bool>)> {
        let toolbar = self.toolbar()?;
        let action = |action: &crate::window_toolbar::ToolbarAction| {
            (
                action.name.clone(),
                format!("{ACTION_TARGET_PREFIX}{}", action.id),
                action.enabled,
                (action.toggles || action.on).then_some(action.on),
            )
        };
        match entry {
            MenuEntry::Action(i) => toolbar.actions.get(i).map(action),
            MenuEntry::Leading(i) => toolbar.leading.get(i).map(|leading| {
                (
                    leading.name.clone(),
                    format!("{LEADING_TARGET_PREFIX}{}", leading.id),
                    leading.enabled,
                    (leading.toggles || leading.on).then_some(leading.on),
                )
            }),
            MenuEntry::Item(i, j) => toolbar
                .actions
                .get(i)
                .and_then(|menu| menu.items.get(j))
                .map(action),
            MenuEntry::View(i) => {
                let views = toolbar.views.as_ref()?;
                let view = views.choices.get(i)?;
                Some((
                    view.label.clone(),
                    format!("{VIEW_TARGET_PREFIX}{}", view.id),
                    true,
                    Some(view.id == views.chosen),
                ))
            }
            MenuEntry::Choice(_) => None,
        }
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
            VIEWS_MENU_TARGET => Slot::ViewsMenu,
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
                    let chosen = self.choose_view(i);
                    return self.ran_from_menu(chosen);
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
                    let opens = toolbar.actions.get(i).is_some_and(|a| !a.items.is_empty());
                    let ran = self.activate(&Slot::Action(i));
                    return if opens { ran } else { self.ran_from_menu(ran) };
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
                let ran = self.run_item(i, j);
                return self.ran_from_menu(ran);
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

/// Whether `slot` is drawn on: the sidebar's toggle while the sidebar is
/// open, a toggle while it is on (a panel's, while the panel is open).
fn shows_on(toolbar: &WindowToolbar, slot: &Slot) -> bool {
    match slot {
        Slot::Sidebar => toolbar.sidebar == Some(true),
        Slot::Leading(i) => toolbar.leading.get(*i).is_some_and(|action| action.on),
        Slot::Action(i) => toolbar.actions.get(*i).is_some_and(|action| action.on),
        _ => false,
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
        destructive: false,
    };
    if toolbar.sidebar.is_some() {
        paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (slots.sidebar, Icon::PanelLeft),
            face(
                Slot::Sidebar,
                !toolbar.sidebar_unavailable,
                shows_on(toolbar, &Slot::Sidebar),
            ),
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
        let Some(icon) = Icon::from_name(&action.icon).filter(|_| rect.area() > 0.0) else {
            continue;
        };
        paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (*rect, icon),
            face(
                Slot::Leading(i),
                action.enabled,
                shows_on(toolbar, &Slot::Leading(i)),
            ),
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
        // Switching the window's view is tabs, the look of every view
        // switcher (`ViewTabs`), never a segmented control.
        .style(super::toggle_group::SegmentedStyle::Tabs)
        .control_size(VIEWS_SIZE)
        .hovered(hovered);
        ToggleGroup::paint(scene, transform, tr, theme, &face);
    }
    if let Some(views) = toolbar.views.as_ref()
        && slots.views_menu.area() > 0.0
    {
        let open = interaction.is_some_and(|i| i.menu_open == Some(Slot::ViewsMenu));
        let lift = LocationLift {
            hovered: interaction.is_some_and(|i| i.hover == Some(Slot::ViewsMenu)),
            pressed: open || interaction.is_some_and(|i| i.pressed == Some(Slot::ViewsMenu)),
            focused: false,
        };
        // As the views' picker the location names its period, not the view.
        let picker = match toolbar.location.as_ref() {
            Some(location) if slots.views_in_location() => {
                let (shown, ink) = location_shown(location, slots.form, theme);
                PickerText::new(shown, ink).edited(location.edited)
            }
            _ => {
                let chosen = views.chosen_index().and_then(|i| views.choices.get(i));
                let label = chosen.map_or(views.name.as_str(), |view| view.label.as_str());
                PickerText::new(label, theme.tokens.colors.text_primary)
            }
        };
        paint_picker(
            scene,
            transform,
            (tr, theme),
            picker,
            slots.views_menu,
            lift,
        );
    }
    for (i, (action, rect)) in toolbar.actions.iter().zip(&slots.actions).enumerate() {
        let Some(icon) = Icon::from_name(&action.icon) else {
            continue;
        };
        paint_icon_button(scene, transform, tr, theme, (*rect, icon), {
            let mut lifted = face(
                Slot::Action(i),
                action.enabled,
                shows_on(toolbar, &Slot::Action(i)),
            );
            lifted.hovered |= interaction.is_some_and(|it| it.menu_open == Some(Slot::Action(i)));
            lifted.destructive = action.destructive;
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
    if slots.location.area() <= 0.0 || editing || slots.views_in_location() {
        return None;
    }
    let lifted = LocationLift {
        hovered: interaction.is_some_and(|i| i.hover == Some(Slot::Location)),
        pressed: interaction.is_some_and(|i| {
            i.pressed == Some(Slot::Location) || i.menu_open == Some(Slot::Location)
        }),
        focused: false,
    };
    paint_location(
        scene,
        transform,
        (tr, theme, presence),
        (location, crumbs),
        (slots.location, slots.form),
        lifted,
    )
}

/// How the pointer stands on a picker: the location, or the views menu.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LocationLift {
    pub(crate) hovered: bool,
    /// Pressed, or its menu is open.
    pub(crate) pressed: bool,
    /// Holds keyboard focus with the ring shown.
    pub(crate) focused: bool,
}

/// The location at rest: its icon, then its label in form `form`
/// ([`Slots::form`]), elided at the end. An
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
    (rect, form): (Rect, usize),
    lift: LocationLift,
) -> Option<Rect> {
    if !location.editable && !location.choices.is_empty() {
        let (shown, ink) = location_shown(location, form, theme);
        return Some(paint_picker(
            scene,
            transform,
            (tr, theme),
            PickerText::new(shown, ink).edited(location.edited),
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
    let (shown, ink) = location_shown(location, form, theme);
    if shown.is_empty() || text_rect.width() <= 0.0 {
        return None;
    }
    let label = LocationLabel::measure(tr, theme, shown, location.edited, text_rect.width());
    let y = rect.center().y - label.height / 2.0;
    label.paint(scene, transform, (tr, theme), (text_rect.x0, y), ink);
    None
}

/// What the location says, in its ink: form `form` of its label
/// ([`crate::window_toolbar::ToolbarLocation::form`]), else its
/// placeholder.
pub(crate) fn location_shown<'a>(
    location: &'a crate::window_toolbar::ToolbarLocation,
    form: usize,
    theme: &Theme,
) -> (&'a str, vello::peniko::Color) {
    if location.label.is_empty() {
        return (
            location.placeholder.as_str(),
            theme.tokens.colors.text_tertiary,
        );
    }
    (location.form(form), theme.tokens.colors.text_primary)
}

/// How wide a picker ([`paint_picker`]) whose label is `label_w` wide
/// stands.
pub(crate) fn picker_width(label_w: f64, theme: &Theme) -> f64 {
    let pad = f64::from(theme.tokens.spacing.s2);
    let gap = f64::from(theme.tokens.spacing.s1);
    pad * 2.0 + label_w.ceil() + gap + LOCATION_ICON_PX
}

/// What a picker ([`paint_picker`]) says: its label in its ink, and
/// whether the document it names has unsaved edits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PickerText<'a> {
    shown: &'a str,
    ink: vello::peniko::Color,
    edited: bool,
}

impl<'a> PickerText<'a> {
    pub(crate) fn new(shown: &'a str, ink: vello::peniko::Color) -> Self {
        Self {
            shown,
            ink,
            edited: false,
        }
    }

    /// The document it names has unsaved edits: the edited word follows.
    pub(crate) fn edited(mut self, edited: bool) -> Self {
        self.edited = edited;
        self
    }
}

/// A label that opens a menu of choices (a location with choices, the
/// folded views): the label (elided at the end) and a trailing chevron, on
/// the plain button face the bar's buttons lift with, as wide as they need
/// and never wider than `rect`. Returns the face's rect, which the menu
/// hangs from.
pub(crate) fn paint_picker(
    scene: &mut Scene,
    transform: Affine,
    (tr, theme): (&mut TextRenderer, &Theme),
    text: PickerText<'_>,
    rect: Rect,
    lift: LocationLift,
) -> Rect {
    let pad = f64::from(theme.tokens.spacing.s2);
    let gap = f64::from(theme.tokens.spacing.s1);
    let room = (rect.width() - pad * 2.0 - gap - LOCATION_ICON_PX).max(0.0);
    let label = LocationLabel::measure(tr, theme, text.shown, text.edited, room);
    let label_w = label.width.min(room);
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
        .pressed(lift.pressed)
        .focused(lift.focused);
    crate::widget::Button::paint(scene, transform, tr, theme, &face);
    let y = rect.center().y - label.height / 2.0;
    label.paint(
        scene,
        transform,
        (tr, theme),
        (face_rect.x0 + pad, y),
        text.ink,
    );
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
mod tests;
