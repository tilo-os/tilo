//! Search in a window, in place.
//!
//! The window's bar carries one search control right after the window's
//! place. Clicking it, or Ctrl+F (⌘F) in the window, expands it into a field
//! where it stands, and what is typed acts on the window's app: a document
//! highlights every match with "3 of 12" and steps through them (Enter,
//! Shift+Enter, F3), a list narrows to what matches ("12 of 40"). The search
//! widens step by step: this view, then the whole app ("Search Notes", or
//! Ctrl+F again), then everywhere — both hand the query to the platform
//! search ([`SearchHandoff`]), which stays the place for launching and for
//! everything.
//!
//! The app draws no field and keeps no find state of its own: it provides
//! the [`Findable`] document or the [`Filterable`](crate::widget::Filterable) list it already has, and
//! the window reaches them through its tile ([`SearchableWindow`] for an app
//! that runs inside its own runtime). An app in a process of its own is
//! reached over its shell connection instead: the search sends it the query,
//! the steps and the close ([`WindowSearch::take_remote_events`]) and shows
//! the count it answers with, and the app's runtime runs the same
//! [`SearchDriver`] on its widgets. An app with neither opens the platform
//! search narrowed to itself, as the control always did.
//!
//! An app whose views are queries over its resources offers more than a
//! list: what is typed turns the window's view into a query view of the
//! app's resources ([`Queryable`], [`InPlace::Query`]). The field then
//! searches the whole app in place ("Search Music"), so it offers no
//! widening to the app; Enter or Down hands the keyboard to the results, and
//! Escape returns the window to the view it showed before.
//!
//! On a document the open search carries one more control after the steps:
//! the find's options (`options`), a panel under the field with Match case
//! and Whole word, Regular expression or Text / Hex where the document
//! offers them ([`FindAbilities`]), and for a document that can replace, the
//! replacement with Replace and Replace all. The replace chord (Ctrl+H,
//! ⌘⌥F) opens the search there. Each of them is a semantic action too, so
//! assistive technology and agents set the options and replace without the
//! panel.

mod options;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::Icon;
use crate::motion::{Disclose, Presence};
use crate::platform::PlatformConventions;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{EffectiveMotion, MotionSource};
use crate::widget::find_bar::{FindAbilities, FindOptions, FindSession, Findable, with_findable};
use crate::widget::search_field::{FilterCount, apply_filter, with_filterable};
use crate::widget::{Button, ButtonFace, ButtonRole, ControlSize, SearchField, Widget};
use crate::window_toolbar::SearchOpening;
use options::{Choice, OptionsPanel};

/// The side of the resting control, the window bar's control size.
const CONTROL_PX: f64 = super::window_controls::CONTROL_PX;
/// The narrowest field, in control heights: below this the widening buttons
/// give way first.
fn field_min(theme: &Theme) -> f64 {
    6.0 * ControlSize::Small.height(theme)
}

/// What a window's search acts on in place, as its app offers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InPlace {
    /// Nothing to search in place: the control opens the platform search.
    #[default]
    Nothing,
    /// A document ([`Findable`]): matches are highlighted and stepped through.
    Document,
    /// A list ([`Filterable`](crate::widget::Filterable)): it narrows to what matches.
    List,
    /// The app's views ([`Queryable`]): the window shows a query view of the
    /// app's resources.
    Query,
}

/// An app whose views are queries over its resources: the window's search
/// turns the view shown into a query view, and closing it returns to the
/// view before.
pub trait Queryable {
    /// The search opened on the window: the person focused the field and
    /// may type next (an app warms its remote sources here).
    fn query_opened(&mut self) {}

    /// What is typed is now `text` (empty when nothing is yet).
    fn set_query_text(&mut self, text: &str);

    /// Enter or Down in the field: the keyboard goes to the results.
    fn enter_results(&mut self) {}

    /// The search closed: show the view from before it opened, as it was.
    fn query_closed(&mut self);

    /// What the field's counter shows, if anything.
    fn query_count(&self) -> Option<FilterCount> {
        None
    }
}

/// Run `visit` on the first [`Queryable`] in `widget`'s subtree; whether
/// there was one.
pub fn with_queryable(widget: &mut dyn Widget, visit: &mut dyn FnMut(&mut dyn Queryable)) -> bool {
    if let Some(target) = crate::widget::capability::capability_mut::<dyn Queryable>(&mut *widget) {
        visit(target);
        return true;
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| with_queryable(child, visit))
}

impl InPlace {
    /// What `root`'s widgets offer: views that are queries win, since such an
    /// app searches all of itself in place; then a document over a list,
    /// since a window showing both (a sidebar of notes and the open note) is
    /// read in its document.
    pub fn of(root: &mut dyn Widget) -> Self {
        if with_queryable(root, &mut |_| ()) {
            Self::Query
        } else if with_findable(root, &mut |_| ()).is_some() {
            Self::Document
        } else if with_filterable(root, &mut |_| ()) {
            Self::List
        } else {
            Self::Nothing
        }
    }
}

/// How far beyond the window a search goes when the person widens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchReach {
    /// The whole app the window shows.
    App,
    /// Everything: the platform search unscoped.
    Everywhere,
}

/// A search the window hands to the platform search: the tab it came from,
/// how far it reaches and what was typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHandoff {
    pub tab: u64,
    pub reach: SearchReach,
    pub query: String,
}

/// The find engine behind a window's search, run where the app's widgets
/// are: by the window for an app in its process, by the app's own runtime
/// for an app in a process of its own. It finds or narrows when the query
/// changed, follows the content when it changed under the search, steps, and
/// stops when the search closed.
#[derive(Debug, Default)]
pub struct SearchDriver {
    in_place: InPlace,
    session: FindSession,
    /// What the document's find offers beyond matching, as last driven.
    abilities: FindAbilities,
    /// The query a list was last narrowed to.
    applied: Option<String>,
    count: Option<FilterCount>,
    /// A find the document ran on its own that the search took over, until
    /// the field it drives from takes its query ([`Self::take_adopted`]).
    adopted: Option<(String, FindOptions)>,
    /// What of the document the search's options cover, in its coordinates
    /// ([`Self::set_cover`]), and what the document was last told (it
    /// starts out covered by nothing).
    cover: Option<Rect>,
    cover_told: Option<Rect>,
}

impl SearchDriver {
    /// Start a search on what the app offers: the query runs afresh on what
    /// it shows now.
    pub fn open(&mut self, in_place: InPlace) {
        self.in_place = in_place;
        self.forget();
    }

    /// What the search acts on; [`InPlace::Nothing`] while none is open.
    #[must_use]
    pub fn in_place(&self) -> InPlace {
        self.in_place
    }

    /// How many matches a document's search found.
    #[must_use]
    pub fn matches(&self) -> usize {
        self.session.matches()
    }

    /// What the document's find offers beyond matching, as last driven.
    #[must_use]
    pub fn abilities(&self) -> FindAbilities {
        self.abilities
    }

    /// `(current, total)` in a document, 1-based; `None` with no match.
    #[must_use]
    pub fn position(&self) -> Option<(usize, usize)> {
        self.session.position()
    }

    /// What the field's counter shows: the current match of all of them in
    /// a document, the rows shown of all rows in a list; none before
    /// anything is typed.
    #[must_use]
    pub fn count(&self) -> Option<FilterCount> {
        self.count
    }

    /// Forget what was found without telling the app (it went away).
    pub fn forget(&mut self) {
        self.session.forget();
        self.abilities = FindAbilities::default();
        self.applied = None;
        self.count = None;
        self.adopted = None;
        self.cover_told = None;
    }

    /// What of the document the search's options cover now, in the app's
    /// coordinates (`None`: nothing). The next drive tells the document,
    /// which shows the current match clear of it.
    pub fn set_cover(&mut self, cover: Option<Rect>) {
        self.cover = cover;
    }

    /// The query and options of a find the document ran on its own
    /// ([`Findable::take_own_find`]) that the last drive took over: the
    /// field shows that query and drives with it from now on.
    pub fn take_adopted(&mut self) -> Option<(String, FindOptions)> {
        self.adopted.take()
    }

    /// Carry `query` under `options` to the app's `root`, step when asked,
    /// or stop when `finishing` (after which the driver is closed). Returns
    /// whether anything the app shows changed.
    pub fn drive(
        &mut self,
        root: &mut dyn Widget,
        query: &str,
        options: FindOptions,
        step: Option<bool>,
        finishing: bool,
    ) -> bool {
        let mut touched = false;
        match self.in_place {
            InPlace::Nothing => {}
            InPlace::Document => {
                let session = &mut self.session;
                let abilities = &mut self.abilities;
                let adopted = &mut self.adopted;
                let cover = self.cover.filter(|_| !finishing);
                let cover_moved = self.cover_told != cover;
                self.cover_told = cover;
                let mut shown = query.to_owned();
                with_findable(root, &mut |target: &mut dyn Findable| {
                    *abilities = FindAbilities::of(target);
                    let options = abilities.allow(options);
                    if cover_moved {
                        // Told before anything is revealed, so what this
                        // drive reveals stands clear of it.
                        target.set_find_cover(cover);
                    }
                    if finishing {
                        session.clear(target);
                        touched = true;
                        return;
                    }
                    let mut revealed = false;
                    if let Some(own) = target.take_own_find() {
                        // The document went to a match on its own: the
                        // search runs its query from that match on.
                        let options = abilities.allow(own.options);
                        session.search(target, &own.query, options, Some(own.current));
                        shown.clone_from(&own.query);
                        *adopted = Some((own.query, options));
                        touched = true;
                        revealed = true;
                    } else if session.is_stale(query, options) {
                        session.search(target, query, options, Some(0));
                        touched = true;
                        revealed = true;
                    } else if session.content_moved(target) {
                        session.refresh(target, query, options);
                        touched = true;
                    }
                    if let Some(forward) = step {
                        session.step(target, forward);
                        touched = true;
                        revealed = true;
                    }
                    if cover_moved && !revealed {
                        // The options opened over the current match, or
                        // closed: it is shown again, clear of them.
                        touched |= session.reveal_current(target);
                    }
                    if let Some((total, current)) = target.take_engine_results() {
                        session.set_results(total, current);
                        touched = true;
                    }
                });
                self.count = (!shown.is_empty() && !finishing).then(|| {
                    let (current, total) = session.position().unwrap_or((0, session.matches()));
                    FilterCount::new(current, total)
                });
            }
            InPlace::List => {
                if finishing {
                    let _ = apply_filter(root, "");
                    self.applied = None;
                    self.count = None;
                    touched = true;
                } else if self.applied.as_deref() != Some(query) {
                    let count = apply_filter(root, query);
                    self.count = count.filter(|_| !query.is_empty());
                    self.applied = Some(query.to_owned());
                    touched = true;
                }
            }
            InPlace::Query => {
                let opening = self.applied.is_none();
                let changed = self.applied.as_deref() != Some(query);
                let mut count = None;
                with_queryable(root, &mut |target| {
                    if finishing {
                        target.query_closed();
                        return;
                    }
                    if opening {
                        target.query_opened();
                    }
                    if changed {
                        target.set_query_text(query);
                    }
                    if step == Some(true) {
                        target.enter_results();
                    }
                    count = target.query_count();
                });
                if finishing {
                    self.applied = None;
                    self.count = None;
                    touched = true;
                } else {
                    touched |= changed || step.is_some();
                    self.applied = Some(query.to_owned());
                    self.count = count.filter(|_| !query.is_empty());
                }
            }
        }
        if finishing {
            self.in_place = InPlace::Nothing;
            self.forget();
        }
        touched
    }

    /// Replace the current match of the document's search for `query` under
    /// `options` with `with`, or every match (`all`), and find again,
    /// keeping the place. Returns whether the document changed.
    pub fn replace(
        &mut self,
        root: &mut dyn Widget,
        query: &str,
        options: FindOptions,
        with: &str,
        all: bool,
    ) -> bool {
        if self.in_place != InPlace::Document || query.is_empty() {
            return false;
        }
        let session = &mut self.session;
        let mut changed = false;
        with_findable(root, &mut |target: &mut dyn Findable| {
            if !target.supports_replace() {
                return;
            }
            let options = FindAbilities::of(target).allow(options);
            if session.is_stale(query, options) {
                session.search(target, query, options, Some(0));
            }
            changed = session.replace(target, query, options, with, all);
        });
        let (current, total) = session.position().unwrap_or((0, session.matches()));
        self.count = Some(FilterCount::new(current, total));
        changed
    }
}

/// An app that runs inside a runtime of its own (an embedded app) and lets
/// the window's search reach its widgets.
pub trait SearchableWindow {
    /// Run `visit` on the app's root widget, where the search finds the
    /// view's document or list. `visit` says whether it changed anything the
    /// app shows, so the app lays out and draws again only then; returns
    /// what `visit` said.
    fn visit_search_root(&mut self, visit: &mut dyn FnMut(&mut dyn Widget) -> bool) -> bool;
}

/// What the person asked of the window's search, for the window to carry
/// out: it knows the tab and its app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowSearchRequest {
    /// Open the search: in place where the app offers it, else the platform
    /// search narrowed to the app.
    Open,
    /// Close it: the app stops highlighting or narrowing.
    Close,
    Widen(SearchReach),
}

/// A clickable part of the open search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Previous,
    Next,
    /// The find's options and the replacement ([`options`]).
    Options,
    App,
    Everywhere,
    Close,
}

impl Part {
    fn target(self) -> &'static str {
        match self {
            Self::Previous => "window-search-previous",
            Self::Next => "window-search-next",
            Self::Options => "window-search-options",
            Self::App => "window-search-app",
            Self::Everywhere => "window-search-everywhere",
            Self::Close => "window-search-close",
        }
    }
}

/// A toolkit message in the host's locale.
fn tr(key: &str) -> String {
    crate::i18n::widget_format(key, &[])
}

/// What a key press asks of the window's search on `platform`: Find
/// (Ctrl+F, ⌘F), or Find and Replace (Ctrl+H, ⌘⌥F). Ctrl+Shift+H stays
/// History's.
#[must_use]
pub fn search_chord(
    event: &WindowEvent,
    modifiers: winit::keyboard::ModifiersState,
    platform: PlatformConventions,
) -> Option<SearchOpening> {
    let WindowEvent::KeyboardInput {
        event:
            KeyEvent {
                logical_key,
                physical_key,
                state: ElementState::Pressed,
                ..
            },
        ..
    } = event
    else {
        return None;
    };
    if !platform.command_modifier_down(modifiers) || modifiers.shift_key() {
        return None;
    }
    let mac = platform.platform() == crate::platform::Platform::MacOs;
    let letter = |letter: &str| matches!(logical_key, Key::Character(text) if text.eq_ignore_ascii_case(letter));
    let alt = modifiers.alt_key();
    // Option changes the character a Mac key types (⌥F is ƒ): the key itself
    // names the chord there.
    if mac
        && alt
        && (*physical_key == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyF)
            || letter("f"))
    {
        return Some(SearchOpening::Replace);
    }
    if alt {
        return None;
    }
    if !mac && letter("h") {
        return Some(SearchOpening::Replace);
    }
    letter("f").then_some(SearchOpening::Find)
}

/// The window bar's search: a control that expands into a field in place.
pub struct WindowSearch {
    presence: Presence,
    field: SearchField,
    /// The resting control, where the field grows from.
    anchor: Rect,
    /// The furthest the open search may reach: rightwards to the window's
    /// own controls, or (trailing) leftwards to an app's location.
    limit: f64,
    /// The furthest the search reaches back when the query would otherwise
    /// be narrower than it can be typed in: over the location and the
    /// app's other slots, or over the tabs it follows.
    cramped: f64,
    /// Where the app's coordinates start in the search's: what the open
    /// options cover of the app is told in the app's.
    frame_origin: Point,
    /// What of the app in its own process the options were last said to
    /// cover (it starts out covered by nothing).
    cover_sent: Option<Rect>,
    /// The control ends an app's slots in the bar, so the open search grows
    /// back over them, on the bar's ground.
    trailing: bool,
    ground: Option<vello::peniko::Color>,
    /// The field as drawn this frame, and the open search's other parts.
    field_rect: Rect,
    parts: Vec<(Part, Rect)>,
    /// Whether the parts after the field show (they fade in as it opens).
    parts_opacity: f32,
    hover: Option<Part>,
    rest_hover: bool,
    last_cursor: Point,
    /// The field holds the keyboard: typing goes to the query, not the app.
    keyboard: bool,
    /// What the window's app offers to search in place.
    in_place: InPlace,
    /// The app's name, which "Search Notes" names.
    app_name: String,
    driver: SearchDriver,
    /// The app runs in a process of its own: the search is carried to it as
    /// events ([`Self::take_remote_events`]) instead of driven here.
    remote: bool,
    /// The query the app in its own process was last sent.
    sent: Option<String>,
    /// A step through a document's matches asked for since the last drive.
    step: Option<bool>,
    /// The find's options as the person set them; they carry over from one
    /// opening to the next. A mode the document does not offer runs as text
    /// ([`FindAbilities::allow`]).
    options: FindOptions,
    /// What the document's find offers beyond matching.
    abilities: FindAbilities,
    /// The options and the replacement, under the field.
    panel: OptionsPanel,
    /// The replacement field holds the search's keyboard, not the query.
    replacing: bool,
    /// The replace chord asked for the replacement before the document said
    /// whether it can replace: it gets the keyboard once it says so.
    replace_wanted: bool,
    /// A replace asked for since the last drive: every match (`true`), or
    /// the current one.
    replace: Option<bool>,
    /// The options the app in its own process was last sent.
    options_sent: Option<FindOptions>,
    /// The last find the app in its own process took over that the field
    /// shows ([`Self::set_remote_adopted`]).
    adopted_serial: Option<u64>,
    /// The search closed and the app has not yet been told.
    finishing: bool,
    requests: Vec<WindowSearchRequest>,
    motion: EffectiveMotion,
    platform: PlatformConventions,
}

impl Default for WindowSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowSearch {
    #[must_use]
    pub fn new() -> Self {
        Self {
            presence: Presence::closed(),
            // One close control: the field clears with the keyboard.
            field: SearchField::new()
                .with_placeholder(&tr("window-search-placeholder"))
                .with_count_slot()
                .without_clear()
                .with_id("window-search-query")
                .control_size(ControlSize::Small),
            anchor: Rect::ZERO,
            limit: 0.0,
            cramped: 0.0,
            frame_origin: Point::ZERO,
            cover_sent: None,
            trailing: false,
            ground: None,
            field_rect: Rect::ZERO,
            parts: Vec::new(),
            parts_opacity: 0.0,
            hover: None,
            rest_hover: false,
            last_cursor: Point::ZERO,
            keyboard: false,
            in_place: InPlace::Nothing,
            app_name: String::new(),
            driver: SearchDriver::default(),
            remote: false,
            sent: None,
            step: None,
            options: FindOptions::default(),
            abilities: FindAbilities::default(),
            panel: OptionsPanel::new(),
            replacing: false,
            replace_wanted: false,
            replace: None,
            options_sent: None,
            adopted_serial: None,
            finishing: false,
            requests: Vec::new(),
            motion: EffectiveMotion::default(),
            platform: PlatformConventions::default(),
        }
    }

    /// Whether the search is open (or opening), whatever is drawn.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.presence.is_open()
    }

    #[must_use]
    pub fn has_keyboard(&self) -> bool {
        self.is_open() && self.keyboard
    }

    /// What the search acts on in place while open.
    #[must_use]
    pub fn in_place(&self) -> InPlace {
        self.in_place
    }

    /// Where the caret stands in the query, in bytes, while the field holds
    /// the keyboard.
    #[must_use]
    pub fn caret_byte(&self) -> Option<usize> {
        self.has_keyboard()
            .then(|| self.field.caret_byte())
            .flatten()
    }

    /// The query typed so far.
    #[must_use]
    pub fn query(&self) -> &str {
        self.field.value()
    }

    /// The replacement typed so far.
    #[must_use]
    pub fn replacement(&self) -> &str {
        self.panel.replace_field.value()
    }

    /// The find's options, as the document runs them.
    #[must_use]
    pub fn options(&self) -> FindOptions {
        self.abilities.allow(self.options)
    }

    /// What the document's find offers beyond matching.
    #[must_use]
    pub fn abilities(&self) -> FindAbilities {
        self.abilities
    }

    /// Whether the options panel is open.
    #[must_use]
    pub fn options_open(&self) -> bool {
        self.is_open() && self.panel.is_open()
    }

    /// The open options panel as laid out, in the search's coordinates.
    #[must_use]
    pub fn options_rect(&self) -> Option<Rect> {
        self.panel.rect().filter(|_| self.is_open())
    }

    /// The narrowest the query is laid out: the parts after it give way
    /// first, then the search reaches back over the app's slots.
    #[must_use]
    pub fn query_min_width(theme: &Theme) -> f64 {
        field_min(theme)
    }

    /// Whether the replacement field holds the keyboard.
    #[must_use]
    pub fn replacing(&self) -> bool {
        self.has_keyboard() && self.replacing
    }

    /// `(current, total)` in a document, 1-based; `None` with no match.
    #[must_use]
    pub fn position(&self) -> Option<(usize, usize)> {
        if !self.remote {
            return self.driver.position();
        }
        self.field
            .count()
            .filter(|count| self.in_place == InPlace::Document && count.shown > 0)
            .map(|count| (count.shown, count.total))
    }

    /// How many matches the document's search found, here or (for an app in
    /// its own process) by the count it answered with.
    fn matches(&self) -> usize {
        if !self.remote {
            return self.driver.matches();
        }
        self.field
            .count()
            .filter(|_| self.in_place == InPlace::Document)
            .map_or(0, |count| count.total)
    }

    /// Whether the search is carried to an app in a process of its own.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        self.remote
    }

    /// What the field's counter shows.
    #[must_use]
    pub fn count(&self) -> Option<FilterCount> {
        self.field.count()
    }

    /// Name the window's app, for the widening button ("Search Notes").
    pub fn set_app_name(&mut self, name: &str) {
        if self.app_name != name {
            name.clone_into(&mut self.app_name);
        }
    }

    /// Place the resting control: `anchor` is its square, `limit` the
    /// furthest the open search may reach.
    pub fn place(&mut self, anchor: Rect, limit: f64) {
        self.anchor = anchor;
        self.limit = limit;
        self.cramped = anchor.x0;
        self.trailing = false;
    }

    /// Where the open search, placed after the tabs ([`Self::place`]), may
    /// start back over them when a narrow window leaves the query too
    /// narrow to type in.
    pub fn set_cramped_start(&mut self, x: f64) {
        if !self.trailing {
            self.cramped = x.min(self.anchor.x0);
        }
    }

    /// Place the control at the end of an app's slots: open, the search ends
    /// at `anchor` and reaches back as far as `reach`, covering the slots it
    /// stands over on the bar's ground. Where that leaves the query too
    /// narrow to type in (a narrow window), it reaches back as far as
    /// `cramped` instead (the start of the app's slots).
    pub fn place_trailing(&mut self, anchor: Rect, reach: f64, cramped: f64) {
        self.anchor = anchor;
        self.limit = reach;
        self.cramped = cramped.min(reach);
        self.trailing = true;
    }

    /// Where the app's own coordinates start in the search's (the top left
    /// of the frame it lays out in): what the open options cover of the
    /// app is told to it in its coordinates ([`Findable::set_find_cover`]).
    pub fn set_frame_origin(&mut self, origin: Point) {
        self.frame_origin = origin;
    }

    /// What the open options cover of the app, in its coordinates.
    fn cover(&self) -> Option<Rect> {
        self.panel
            .rect()
            .filter(|rect| rect.area() > 0.0)
            .map(|rect| rect - self.frame_origin.to_vec2())
    }

    /// The colour the bar stands on.
    pub fn set_ground(&mut self, ground: vello::peniko::Color) {
        self.ground = Some(ground);
    }

    /// Move everything by `dx` (the strip panned under a laid-out window).
    pub(crate) fn offset_layout(&mut self, dx: f64) {
        let shift = vello::kurbo::Vec2::new(dx, 0.0);
        self.anchor = self.anchor + shift;
        self.limit += dx;
        self.cramped += dx;
        self.field_rect = self.field_rect + shift;
        for (_, rect) in &mut self.parts {
            *rect = *rect + shift;
        }
        let field = self.field_rect;
        self.field.apply_layout_external(field);
    }

    /// The resting control's square.
    #[must_use]
    pub fn anchor(&self) -> Rect {
        self.anchor
    }

    /// Whether `point` is on the control: its resting square, or the open
    /// field and its buttons. A press there is the search's, never the
    /// start of carrying the window by its bar.
    #[must_use]
    pub fn hit(&self, point: Point) -> bool {
        self.anchor.contains(point)
            || (self.is_open()
                && (self.field_rect.contains(point)
                    || self.part_at(point).is_some()
                    || self.panel.contains(point)))
    }

    /// Open in place on what the app offers, the field holding the keyboard
    /// with its query selected (so typing replaces it, as Find does).
    pub fn open(&mut self, in_place: InPlace, source: MotionSource) {
        self.in_place = in_place;
        // A query view searches the whole app: the field says so.
        let hint = if in_place == InPlace::Query {
            self.app_label()
        } else {
            tr("window-search-placeholder")
        };
        self.field.set_placeholder(&hint);
        self.finishing = false;
        self.remote = false;
        self.sent = None;
        // An app in its own process opens each search on the defaults.
        self.options_sent = Some(FindOptions::default());
        self.adopted_serial = None;
        self.abilities = FindAbilities::default();
        self.replace = None;
        self.replace_wanted = false;
        self.panel.set_open(false);
        self.keyboard = true;
        self.focus_query();
        self.field.select_all();
        // The query runs again on what the app shows now.
        self.driver.open(in_place);
        self.presence.show(
            true,
            Disclose::SPEC,
            self.motion,
            source,
            crate::motion::frame_now(),
        );
    }

    /// Open in place on an app that runs in a process of its own and offers
    /// `in_place`: what is typed goes to it as events
    /// ([`Self::take_remote_events`]), and it answers with the count
    /// ([`Self::set_remote_count`]).
    pub fn open_remote(&mut self, in_place: InPlace, source: MotionSource) {
        self.open(in_place, source);
        self.remote = true;
        self.field.set_count(None);
    }

    /// What the document of an app in its own process offers beyond
    /// matching, as its window says.
    pub fn set_remote_abilities(&mut self, abilities: FindAbilities) {
        if self.remote {
            self.learn_abilities(abilities);
        }
    }

    /// The document said what it offers: the panel shows that, and a
    /// replace chord waiting for it is answered.
    fn learn_abilities(&mut self, abilities: FindAbilities) {
        self.abilities = abilities;
        if !abilities.replace && self.replacing {
            self.focus_query();
        }
        if std::mem::take(&mut self.replace_wanted) && abilities.replace {
            self.focus_replacement();
        }
    }

    /// The replace chord (Ctrl+H, ⌘⌥F) on the open search: the options open
    /// with the replacement holding the keyboard, its text selected. Before
    /// the document has said whether it can replace (the search just
    /// opened), it does so once it says it can.
    pub fn focus_replacement(&mut self) {
        if !self.is_open() || self.in_place != InPlace::Document {
            return;
        }
        if !self.abilities.replace {
            self.replace_wanted = true;
            self.take_keyboard();
            return;
        }
        self.panel.set_open(true);
        self.keyboard = true;
        self.replacing = true;
        self.field.set_focused(false);
        self.panel.replace_field.set_focused(true);
        self.panel.replace_field.select_all();
    }

    /// Open the search as `opening` asks: in place on what the app offers,
    /// with the replacement holding the keyboard for the replace chord.
    pub fn open_as(&mut self, in_place: InPlace, source: MotionSource, opening: SearchOpening) {
        self.open(in_place, source);
        if opening == SearchOpening::Replace {
            self.focus_replacement();
        }
    }

    /// The query field holds the search's keyboard.
    fn focus_query(&mut self) {
        self.replacing = false;
        self.panel.replace_field.set_focused(false);
        self.field.set_focused(true);
    }

    /// Choose `choice` in the options: set an option (the next drive finds
    /// again under it) or ask for a replace.
    fn choose(&mut self, choice: Choice) {
        match choice {
            Choice::Replace => self.request_replace(false),
            Choice::ReplaceAll => self.request_replace(true),
            _ => self.options = choice.apply(self.options),
        }
    }

    /// Set the find's options (an agent, a test); the next drive finds again
    /// under them.
    pub fn set_options(&mut self, options: FindOptions) {
        self.options = options;
    }

    /// Replace the current match with what the replacement holds, or every
    /// match (`all`), on the next drive.
    pub fn request_replace(&mut self, all: bool) {
        if self.in_place == InPlace::Document && self.abilities.replace {
            self.replace = Some(all);
        }
    }

    /// Open or close the options panel.
    fn toggle_options(&mut self) {
        let open = !self.panel.is_open();
        self.panel.set_open(open);
        if !open && self.replacing {
            self.focus_query();
        }
    }

    /// What the search asks of an app in its own process since the last
    /// call: the query when it changed (the first time on opening), a step,
    /// or the close.
    pub fn take_remote_events(&mut self) -> Vec<crate::window_toolbar::ToolbarEvent> {
        use crate::window_toolbar::ToolbarEvent;
        let mut events = Vec::new();
        if !self.remote {
            return events;
        }
        if std::mem::take(&mut self.finishing) {
            events.push(ToolbarEvent::SearchClose);
            self.remote = false;
            self.sent = None;
            self.options_sent = None;
            self.cover_sent = None;
            self.step = None;
            self.replace = None;
            self.in_place = InPlace::Nothing;
            self.field.set_count(None);
            return events;
        }
        if !self.is_open() || self.in_place == InPlace::Nothing {
            return events;
        }
        let options = self.options();
        if self.in_place == InPlace::Document && self.options_sent != Some(options) {
            events.push(ToolbarEvent::SearchOptions { options });
            self.options_sent = Some(options);
        }
        let cover = self.cover();
        if self.in_place == InPlace::Document && self.cover_sent != cover {
            events.push(ToolbarEvent::SearchCover {
                cover: cover.map(crate::window_toolbar::WholeRect::around),
            });
            self.cover_sent = cover;
        }
        let query = self.field.value();
        if self.sent.as_deref() != Some(query) {
            events.push(ToolbarEvent::Search {
                query: query.to_owned(),
            });
            self.sent = Some(query.to_owned());
        }
        if let Some(forward) = self.step.take() {
            events.push(ToolbarEvent::SearchStep { forward });
        }
        if let Some(all) = self.replace.take() {
            events.push(ToolbarEvent::SearchReplace {
                with: self.panel.replace_field.value().to_owned(),
                all,
            });
        }
        events
    }

    /// Show `query` under `options` in the open search, as the find the
    /// document ran on its own that the search took over: the field holds
    /// it (selected, so typing replaces it) and the search runs it from now
    /// on.
    fn show_query(&mut self, query: &str, options: FindOptions) {
        if self.field.value() != query {
            self.field.set_query(query);
        }
        if self.keyboard && !self.replacing {
            self.field.select_all();
        }
        self.options = options;
    }

    /// The app in its own process took over a find its document ran on its
    /// own ([`crate::window_toolbar::AdoptedQuery`]): the field shows it, and
    /// it is what the app was last sent, so it is not sent back.
    pub fn set_remote_adopted(&mut self, adopted: &crate::window_toolbar::AdoptedQuery) {
        if !self.remote || !self.is_open() || self.adopted_serial == Some(adopted.serial) {
            return;
        }
        self.adopted_serial = Some(adopted.serial);
        self.show_query(&adopted.query, adopted.options);
        self.sent = Some(adopted.query.clone());
        self.options_sent = Some(adopted.options);
    }

    /// The count an app in its own process answered with.
    pub fn set_remote_count(&mut self, count: Option<FilterCount>) {
        if self.remote && self.is_open() {
            let count = count.filter(|_| !self.field.value().is_empty());
            if self.field.count() != count {
                self.field.set_count(count);
            }
        }
    }

    /// Close: the field gives up the keyboard and the app is told on the
    /// next drive to stop highlighting or narrowing.
    pub fn close(&mut self, source: MotionSource) {
        if !self.is_open() {
            return;
        }
        self.keyboard = false;
        self.field.set_focused(false);
        self.replacing = false;
        self.replace_wanted = false;
        self.panel.set_open(false);
        self.finishing = self.in_place != InPlace::Nothing;
        self.step = None;
        self.replace = None;
        self.hover = None;
        self.presence.show(
            false,
            Disclose::SPEC,
            self.motion,
            source,
            crate::motion::frame_now(),
        );
    }

    /// Give the keyboard back to the app, leaving the search open with its
    /// matches (a click into the content).
    pub fn release_keyboard(&mut self) {
        self.keyboard = false;
        self.field.set_focused(false);
        self.panel.replace_field.set_focused(false);
        self.panel.set_open(false);
        self.replacing = false;
    }

    /// Take the keyboard again (Ctrl+F on an open search).
    pub fn take_keyboard(&mut self) {
        self.keyboard = true;
        self.focus_query();
        self.field.select_all();
    }

    /// Step to the next (or previous) match in a document; in a query view,
    /// hand the keyboard to the results.
    pub fn step(&mut self, forward: bool) {
        match self.in_place {
            InPlace::Document => self.step = Some(forward),
            InPlace::Query if forward => {
                self.step = Some(true);
                self.release_keyboard();
            }
            _ => {}
        }
    }

    /// What the person asked for since the last call.
    pub fn take_requests(&mut self) -> Vec<WindowSearchRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Whether driving the app would change anything: a query not yet
    /// carried to it, a step, or a close it has not heard of.
    #[must_use]
    pub fn wants_drive(&self) -> bool {
        self.finishing
            || self.step.is_some()
            || self.replace.is_some()
            || (self.is_open() && self.in_place != InPlace::Nothing)
    }

    /// Carry the search to the app's `root`: find or narrow when the query
    /// changed, follow the content when it changed under the search, step,
    /// or stop when the search closed. Returns whether anything the app
    /// shows changed.
    pub fn drive(&mut self, root: &mut dyn Widget) -> bool {
        if self.remote {
            // An app in its own process is driven by its own runtime.
            return false;
        }
        let query = self.field.value().to_owned();
        let step = self.step.take();
        let finishing = std::mem::take(&mut self.finishing);
        let options = self.options;
        self.driver.set_cover(self.cover());
        let mut touched = self.driver.drive(root, &query, options, step, finishing);
        if let Some((query, options)) = self.driver.take_adopted() {
            self.show_query(&query, options);
        }
        if !finishing && self.in_place == InPlace::Document {
            let abilities = self.driver.abilities();
            if abilities != self.abilities || self.replace_wanted {
                self.learn_abilities(abilities);
            }
        }
        if let Some(all) = self.replace.take()
            && !finishing
        {
            let query = self.field.value().to_owned();
            let with = self.panel.replace_field.value().to_owned();
            touched |= self.driver.replace(root, &query, options, &with, all);
        }
        let count = self.driver.count();
        if self.field.count() != count {
            self.field.set_count(count);
        }
        if finishing {
            self.in_place = InPlace::Nothing;
        }
        touched
    }

    /// The app went away under an open search (its tab closed or another
    /// became active): forget what was found without telling it.
    pub fn detach(&mut self) {
        self.driver.forget();
        self.finishing = false;
        self.step = None;
        self.replace = None;
        self.replace_wanted = false;
        self.sent = None;
        self.options_sent = None;
        self.cover_sent = None;
        self.remote = false;
        self.field.set_count(None);
    }

    /// Route one event. Returns whether the search consumed it; the window
    /// then stops it reaching the bar and the app.
    pub fn route(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) -> bool {
        let open = self.is_open();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                self.rest_hover = !open && self.anchor.contains(self.last_cursor);
                self.hover = open.then(|| self.part_at(self.last_cursor)).flatten();
                self.panel.set_hover(open.then_some(self.last_cursor));
                if open {
                    // A selection drag in a field follows the pointer.
                    let mut field_handled = false;
                    let field = if self.replacing {
                        &mut self.panel.replace_field
                    } else {
                        &mut self.field
                    };
                    field.handle_event(
                        event,
                        &mut crate::widget::EventCtx {
                            handled: &mut field_handled,
                            ..ctx.reborrow(ctx.path, ctx.node_id)
                        },
                    );
                }
                // Over the open panel, what is beneath it is not hovered.
                self.panel.contains(self.last_cursor)
            }
            WindowEvent::CursorLeft { .. } => {
                self.rest_hover = false;
                self.hover = None;
                self.panel.set_hover(None);
                false
            }
            WindowEvent::MouseWheel { .. } => open && self.panel.contains(self.last_cursor),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.press(event, ctx),
            WindowEvent::MouseInput { .. } => {
                if open && self.field_rect.contains(self.last_cursor) {
                    self.field.handle_event(event, ctx);
                    return true;
                }
                if open && self.panel.on_replace_field(self.last_cursor) {
                    self.panel.replace_field.handle_event(event, ctx);
                    return true;
                }
                open && self.panel.contains(self.last_cursor)
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.has_keyboard() => {
                self.key(logical_key, event, ctx);
                true
            }
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) if self.has_keyboard() => {
                if self.replacing {
                    self.panel.replace_field.handle_event(event, ctx);
                } else {
                    self.field.handle_event(event, ctx);
                }
                true
            }
            _ => false,
        }
    }

    fn press(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) -> bool {
        if !self.is_open() {
            if self.anchor.contains(self.last_cursor) {
                self.requests.push(WindowSearchRequest::Open);
                return true;
            }
            return false;
        }
        let point = self.last_cursor;
        if self.panel.is_open() {
            if let Some(choice) = self.panel.choice_at(point) {
                self.choose(choice);
                return true;
            }
            if self.panel.on_replace_field(point) {
                self.keyboard = true;
                self.replacing = true;
                self.field.set_focused(false);
                self.panel.replace_field.set_focused(true);
                self.panel.replace_field.handle_event(event, ctx);
                return true;
            }
            if self.panel.contains(point) {
                return true;
            }
            if !self.field_rect.contains(point) && self.part_at(point).is_none() {
                // A press outside closes the panel and does not reach what
                // is beneath it, as for every popover.
                self.toggle_options();
                return true;
            }
        }
        if let Some(part) = self.part_at(point) {
            self.activate(part);
            return true;
        }
        if self.field_rect.contains(point) {
            self.keyboard = true;
            self.focus_query();
            self.field.handle_event(event, ctx);
            return true;
        }
        // A press anywhere else is the content's: the search stays open
        // with its matches, and typing goes back to the app.
        self.release_keyboard();
        false
    }

    fn activate(&mut self, part: Part) {
        match part {
            Part::Previous => self.step(false),
            Part::Next => self.step(true),
            Part::Options => self.toggle_options(),
            Part::App => self
                .requests
                .push(WindowSearchRequest::Widen(SearchReach::App)),
            Part::Everywhere => self
                .requests
                .push(WindowSearchRequest::Widen(SearchReach::Everywhere)),
            Part::Close => self.requests.push(WindowSearchRequest::Close),
        }
    }

    fn key(&mut self, key: &Key, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let modifiers = ctx.modifiers.state();
        let command = self.platform.command_modifier_down(modifiers);
        let chord = search_chord(event, modifiers, self.platform);
        if chord == Some(SearchOpening::Replace) && self.in_place == InPlace::Document {
            self.focus_replacement();
            return;
        }
        if self.replacing {
            if chord == Some(SearchOpening::Find) {
                // Find goes back to the query.
                self.take_keyboard();
            } else {
                self.replacement_key(key, event, ctx);
            }
            return;
        }
        match key {
            // The panel closes first, then the search.
            Key::Named(NamedKey::Escape) if self.panel.is_open() => self.toggle_options(),
            Key::Named(NamedKey::Escape) => self.requests.push(WindowSearchRequest::Close),
            Key::Named(NamedKey::Enter | NamedKey::F3)
                if self.in_place == InPlace::Document && self.matches() > 0 =>
            {
                self.step(!modifiers.shift_key());
            }
            // A query view searches the whole app already: Enter and Down go
            // into its results, and Find again keeps the field.
            Key::Named(NamedKey::Enter | NamedKey::ArrowDown)
                if self.in_place == InPlace::Query =>
            {
                if !self.field.value().is_empty() {
                    self.step(true);
                }
            }
            Key::Character(text)
                if command && text.eq_ignore_ascii_case("f") && self.in_place == InPlace::Query =>
            {
                self.field.select_all();
            }
            // Nothing here matches, or this window has no document to step
            // through: Enter widens to the whole app, as the next step does.
            Key::Named(NamedKey::Enter) if !self.field.value().is_empty() => {
                self.requests
                    .push(WindowSearchRequest::Widen(SearchReach::App));
            }
            // Find again widens: this view, then the whole app.
            Key::Character(text) if command && text.eq_ignore_ascii_case("f") => {
                self.requests
                    .push(WindowSearchRequest::Widen(SearchReach::App));
            }
            // Tab goes on to the replacement while the options show it;
            // else it hands the keyboard back to the content, the search open.
            Key::Named(NamedKey::Tab) if self.panel.replace_field_rect().is_some() => {
                self.focus_replacement();
            }
            Key::Named(NamedKey::Tab) => self.release_keyboard(),
            _ => self.field.handle_event(event, ctx),
        }
    }

    /// A key while the replacement holds the keyboard: Enter replaces the
    /// current match, Tab goes back to the query, Escape closes the options.
    fn replacement_key(
        &mut self,
        key: &Key,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        match key {
            Key::Named(NamedKey::Escape) => self.toggle_options(),
            Key::Named(NamedKey::Enter) => self.request_replace(false),
            Key::Named(NamedKey::Tab) => self.focus_query(),
            _ => self.panel.replace_field.handle_event(event, ctx),
        }
    }

    fn part_at(&self, point: Point) -> Option<Part> {
        if self.parts_opacity <= 0.0 {
            return None;
        }
        self.parts
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(part, _)| *part)
    }

    /// The resting control's tooltip: what it searches and the key that
    /// opens it, "Search in this window (Ctrl+F)". A bare magnifier beside
    /// an app's own search control left people guessing what it reached.
    fn rest_tooltip(&self) -> String {
        format!(
            "{} ({})",
            tr("window-search-name"),
            crate::platform::Shortcut::command("F").label(self.platform)
        )
    }

    /// The field holding the search's keyboard: the query, or the
    /// replacement.
    fn keyboard_field(&self) -> &SearchField {
        if self.replacing {
            &self.panel.replace_field
        } else {
            &self.field
        }
    }

    /// The options control's name: what its panel holds.
    fn options_label(&self) -> String {
        tr(if self.abilities.replace {
            "window-search-replace-options"
        } else {
            "window-search-options"
        })
    }

    /// The widening button's label: "Search Notes".
    fn app_label(&self) -> String {
        if self.app_name.is_empty() {
            tr("window-search-this-app")
        } else {
            crate::i18n::widget_format(
                "window-search-app",
                &[("app", self.app_name.clone().into())],
            )
        }
    }

    /// Lay the open search out at `progress` of its opening: the field grows
    /// from the resting control, the parts after it fade in at their place.
    fn layout(&mut self, progress: f64, text: &mut TextRenderer, theme: &Theme) {
        self.parts.clear();
        let gap = f64::from(theme.tokens.spacing.s1);
        let sibling = f64::from(theme.tokens.spacing.s2);
        let height = ControlSize::Small.height(theme);
        let y0 = self.anchor.center().y - height / 2.0;
        let y1 = y0 + height;
        // Where the open search may stand, left to right: from the control
        // to its reach, or, cramped, as far back as `cramped` too.
        let span = |cramped: bool| match (self.trailing, cramped) {
            (true, false) => (self.limit, self.anchor.x1),
            (true, true) => (self.cramped, self.anchor.x1),
            (false, false) => (self.anchor.x0, self.limit),
            (false, true) => (self.cramped, self.limit),
        };
        let room_of = |(lo, hi): (f64, f64)| (hi - lo).max(CONTROL_PX);
        let app_label = self.app_label();
        let everywhere_label = tr("window-search-everywhere");
        let width_of = |label: &str, text: &mut TextRenderer| {
            ButtonFace::new(Rect::ZERO, label)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .intrinsic_width(text, theme)
        };
        let app_w = width_of(&app_label, text);
        let everywhere_w = width_of(&everywhere_label, text);
        let stepper = self.in_place == InPlace::Document;
        // What follows the field, dropped from the widest reach inwards when
        // the bar is narrow: everywhere, then the app, then Previous, then
        // Next. A document's options never give way: they hold the mode and
        // Replace, which a narrow window (a phone, a touch screen with no
        // keyboard to press Enter on) needs as much as a wide one.
        let mut want_everywhere = true;
        // A query view searches the whole app in place already.
        let mut want_app = self.in_place != InPlace::Query;
        let mut want_previous = stepper;
        let mut want_next = stepper;
        let after = |everywhere: bool, app: bool, previous: bool, next: bool| {
            let mut w = CONTROL_PX + sibling;
            if stepper {
                w += CONTROL_PX + sibling;
            }
            if previous {
                w += CONTROL_PX + gap;
            }
            if next {
                w += CONTROL_PX + gap;
            }
            if app {
                w += app_w + gap;
            }
            if everywhere {
                w += everywhere_w + gap;
            }
            w
        };
        // The field's width when the bar has room for it: eleven controls.
        let field_max = 11.0 * height;
        let field_min = field_min(theme);
        // Where the reach leaves the query too narrow to type in even once
        // every part that gives way is gone, it reaches further back.
        let (lo, hi) = span(room_of(span(false)) - after(false, false, false, false) < field_min);
        let room = room_of((lo, hi));
        for give_way in 0..4 {
            if room - after(want_everywhere, want_app, want_previous, want_next) >= field_min {
                break;
            }
            match give_way {
                0 => want_everywhere = false,
                1 => want_app = false,
                2 => want_previous = false,
                _ => want_next = false,
            }
        }
        let after = after(want_everywhere, want_app, want_previous, want_next);
        let field_w = (room - after).clamp(CONTROL_PX, field_max);
        // Open, the search starts at the control and reaches right; at the
        // end of an app's slots it ends at the control and reaches back.
        let block = field_w + after;
        let start = if self.trailing {
            self.anchor.x1 - block
        } else {
            // Cramped, it starts back over the tabs as far as it must.
            (hi - block).min(self.anchor.x0).max(lo)
        };
        let p = progress.clamp(0.0, 1.0);
        let drawn_x0 = self.anchor.x0 + (start - self.anchor.x0) * p;
        let drawn_w = CONTROL_PX + (field_w - CONTROL_PX) * p;
        self.field_rect = Rect::new(drawn_x0, y0, drawn_x0 + drawn_w, y1);
        self.field.apply_layout_external(self.field_rect);
        // The parts stand where the open search puts them, fading in.
        let mut x = start + field_w + sibling;
        let square = |x: f64| Rect::new(x, y0, x + CONTROL_PX, y1);
        if want_previous {
            self.parts.push((Part::Previous, square(x)));
            x += CONTROL_PX + gap;
        }
        if want_next {
            self.parts.push((Part::Next, square(x)));
            x += CONTROL_PX + gap;
        }
        if stepper {
            self.parts.push((Part::Options, square(x)));
            x += CONTROL_PX + sibling;
        }
        if want_app {
            self.parts
                .push((Part::App, Rect::new(x, y0, x + app_w, y1)));
            x += app_w + gap;
        }
        if want_everywhere {
            self.parts
                .push((Part::Everywhere, Rect::new(x, y0, x + everywhere_w, y1)));
            x += everywhere_w + gap;
        }
        self.parts.push((Part::Close, square(x)));
        self.parts_opacity = Disclose::reveal(progress, 1.0).opacity;
        let options = self
            .parts
            .iter()
            .find(|(part, _)| *part == Part::Options)
            .map(|(_, rect)| *rect);
        match options {
            Some(anchor) => {
                let end = x + CONTROL_PX;
                let span = (lo, hi.max(end));
                let field = Rect::new(start, y0, start + field_w, y1);
                self.panel
                    .layout(field, anchor, span, self.abilities, text, theme);
            }
            // The bar gave the options no room: they close.
            None => self.panel.set_open(false),
        }
    }

    fn paint_part(
        &self,
        part: Part,
        rect: Rect,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let hovered = self.hover == Some(part);
        match part {
            Part::Options => {
                let name = self.options_label();
                let face = ButtonFace::icon_only(rect, Icon::Settings2, &name)
                    .control_size(ControlSize::Small)
                    .hovered(hovered)
                    .toggled(self.panel.is_open());
                Button::paint(scene, transform, text, theme, &face);
            }
            Part::Previous | Part::Next | Part::Close => {
                let (icon, name) = match part {
                    Part::Previous => (Icon::ChevronUp, tr("find-bar-previous")),
                    Part::Next => (Icon::ChevronDown, tr("find-bar-next")),
                    _ => (Icon::X, tr("window-search-close")),
                };
                let disabled = part != Part::Close && self.matches() == 0;
                let face = ButtonFace::icon_only(rect, icon, &name)
                    .control_size(ControlSize::Small)
                    .hovered(hovered)
                    .disabled(disabled);
                Button::paint(scene, transform, text, theme, &face);
            }
            Part::App | Part::Everywhere => {
                let label = if part == Part::App {
                    self.app_label()
                } else {
                    tr("window-search-everywhere")
                };
                let face = ButtonFace::new(rect, &label)
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small)
                    .hovered(hovered);
                Button::paint(scene, transform, text, theme, &face);
            }
        }
    }
}

impl WindowSearch {
    /// The options control to assistive technology and agents: a button
    /// that expands the panel, holding the options and the replacement as
    /// their own controls. They are there while the panel is closed too
    /// (hidden from a screen reader's reading), so an agent sets an option
    /// or replaces without opening it.
    fn options_node(&self, rect: Rect) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
        let open = self.panel.is_open();
        let options = self.options();
        let mut children = Vec::new();
        for choice in Choice::ALL {
            if !choice.offered(self.abilities) {
                continue;
            }
            let role = match choice {
                Choice::MatchCase | Choice::WholeWord | Choice::Regex => Role::Checkbox,
                Choice::Text | Choice::Hex => Role::RadioButton,
                Choice::Replace | Choice::ReplaceAll => Role::Button,
            };
            let mut state = AccessState::new().focusable(open);
            state.hidden = !open;
            if let Some(on) = choice.on(options) {
                state = state.checked(on);
            }
            if choice.on(options).is_none() && self.matches() == 0 {
                state.disabled = true;
            }
            let mut node = AccessNode::new(role)
                .with_name(choice.label())
                .with_action_target(choice.target())
                .with_state(state);
            if let Some(rect) = self.panel.choice_rect(choice) {
                node = node.with_bounds(rect);
            }
            if choice == Choice::Replace
                && let Some(field) = self.panel.replace_field.accessibility()
            {
                // The replacement comes before its buttons.
                let mut field = field
                    .with_name(tr("find-bar-replace-placeholder"))
                    .with_action_target("window-search-replacement");
                field.state.hidden = !open;
                if let Some(rect) = self.panel.replace_field_rect() {
                    field = field.with_bounds(rect);
                }
                children.push(field);
            }
            children.push(node);
        }
        AccessNode::new(Role::Button)
            .with_name(self.options_label())
            .with_bounds(rect)
            .with_action_target(Part::Options.target())
            .with_state(AccessState::new().focusable(true).expanded(open))
            .with_children(children)
    }

    /// An action on one of the options or the replacement; `None` when
    /// `target` is none of them.
    fn perform_option_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> Option<bool> {
        use crate::a11y::Action;
        let document = self.is_open() && self.in_place == InPlace::Document;
        if target == Part::Options.target() && matches!(action, Action::Expand | Action::Collapse) {
            let open = matches!(action, Action::Expand);
            if document && self.panel.is_open() != open {
                self.toggle_options();
            }
            return Some(document);
        }
        if target == "window-search-replacement" {
            if !document || !self.abilities.replace {
                return Some(false);
            }
            if matches!(action, Action::Focus) {
                self.focus_replacement();
                return Some(true);
            }
            return Some(
                self.panel
                    .replace_field
                    .perform_action(action, &[], state, path),
            );
        }
        let choice = Choice::ALL
            .into_iter()
            .find(|choice| choice.target() == target)?;
        if !document || !choice.offered(self.abilities) {
            return Some(false);
        }
        let acts = match choice.on(self.options()) {
            // A radio is chosen, never unchosen.
            Some(true) if matches!(choice, Choice::Text | Choice::Hex) => {
                return Some(matches!(action, Action::Activate));
            }
            Some(_) => matches!(action, Action::Activate | Action::ToggleCheck),
            None => matches!(action, Action::Activate),
        };
        if acts {
            self.choose(choice);
        }
        Some(acts)
    }
}

impl Widget for WindowSearch {
    /// The window lays the search out by hand in its bar ([`Self::place`]);
    /// as a leaf of its own it takes no room.
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        use crate::widget::TaffyTreeExt;
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.field.set_platform_conventions(conventions);
        self.panel
            .replace_field
            .set_platform_conventions(conventions);
    }

    fn get_rect(&self) -> Rect {
        if self.is_open() {
            let end = self.parts.last().map_or(self.field_rect.x1, |(_, r)| r.x1);
            let bar = Rect::new(self.anchor.x0, self.field_rect.y0, end, self.field_rect.y1);
            self.panel.rect().map_or(bar, |panel| bar.union(panel))
        } else {
            self.anchor
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
        self.motion = theme.motion();
        if self.anchor.area() <= 0.0 {
            return;
        }
        let now = crate::time::now();
        let progress = self.presence.progress(now);
        if progress <= 0.0 {
            let name = tr("window-search-name");
            let face = ButtonFace::icon_only(self.anchor, Icon::Search, &name)
                .control_size(ControlSize::Small)
                .hovered(self.rest_hover);
            Button::paint(scene, transform, text, theme, &face);
            return;
        }
        self.layout(progress, text, theme);
        if (self.trailing || self.field_rect.x0 < self.anchor.x0)
            && let Some(ground) = self.ground
        {
            // Over an app's slots, or a narrow window's tabs, the open
            // search covers what it stands on.
            let end = self
                .parts
                .last()
                .map_or(self.field_rect.x1, |(_, r)| r.x1)
                .max(self.anchor.x1);
            let cover = Rect::new(
                self.field_rect.x0,
                self.anchor.y0 - 2.0,
                end,
                self.anchor.y1 + 2.0,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                crate::color_compat::ColorCompat::with_alpha_factor(ground, progress as f32),
                None,
                &cover,
            );
        }
        self.field.draw(scene, transform, text, theme, presence);
        if self.parts_opacity > 0.0 {
            let bounds = self
                .parts
                .iter()
                .map(|(_, r)| *r)
                .reduce(|all, rect| all.union(rect))
                .unwrap_or(Rect::ZERO)
                .inflate(4.0, 4.0);
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                self.parts_opacity,
                transform,
                &bounds,
            );
            for (part, rect) in self.parts.clone() {
                self.paint_part(part, rect, scene, transform, text, theme);
            }
            scene.pop_layer();
        }
    }

    /// The options panel, in the window's top layer over the app.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let shown = options::Shown {
            options: self.options(),
            matches: self.matches(),
        };
        self.panel
            .paint(scene, transform, text, theme, presence, shown);
    }

    fn needs_redraw(&self) -> bool {
        self.presence.is_moving(crate::motion::frame_now())
            || self.field.needs_redraw()
            || self.panel.needs_redraw()
            || (self.panel.is_drawn() && self.panel.replace_field.needs_redraw())
    }

    fn needs_animation_frame(&self) -> bool {
        self.presence.is_moving(crate::motion::frame_now())
            || self.panel.is_moving()
            || (self.has_keyboard() && self.keyboard_field().needs_animation_frame())
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        // The caret's blink, while a field holds the keyboard.
        self.has_keyboard()
            .then(|| self.keyboard_field().next_wake_deadline())
            .flatten()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if !self.is_open() {
            return self
                .anchor
                .contains(point)
                .then_some(winit::window::CursorIcon::Pointer);
        }
        if self.part_at(point).is_some() || self.panel.choice_at(point).is_some() {
            return Some(winit::window::CursorIcon::Pointer);
        }
        if self.panel.on_replace_field(point) {
            return self.panel.replace_field.cursor_shape(point);
        }
        if self.panel.contains(point) {
            return Some(winit::window::CursorIcon::Default);
        }
        self.field.cursor_shape(point)
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        if !self.is_open() {
            return self
                .anchor
                .contains(point)
                .then(|| (self.anchor, self.rest_tooltip()));
        }
        let (part, rect) = self.parts.iter().find(|(_, r)| r.contains(point))?;
        let name = match part {
            Part::Previous => tr("find-bar-previous"),
            Part::Next => tr("find-bar-next"),
            Part::Options => self.options_label(),
            Part::Close => tr("window-search-close"),
            // Labelled buttons say what they do already.
            Part::App | Part::Everywhere => return None,
        };
        Some((*rect, name))
    }

    fn is_focused(&self) -> bool {
        self.has_keyboard()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.has_keyboard()
            .then(|| self.keyboard_field().ime_cursor_rect())
            .flatten()
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        self.has_keyboard()
            .then(|| self.keyboard_field().ime_content())
            .flatten()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        if self.replacing {
            let field = self.panel.replace_field.ime_edit_fingerprint(key)?;
            return Some(crate::ime::scoped_fingerprint(
                b"window-search-replacement",
                field,
                key,
            ));
        }
        let field = self.field.ime_edit_fingerprint(key)?;
        Some(crate::ime::scoped_fingerprint(b"window-search", field, key))
    }

    fn name(&self) -> &'static str {
        "WindowSearch"
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut out = vec![("window-search-open".to_owned(), self.anchor)];
        if self.is_open() {
            out.push(("window-search-query".to_owned(), self.field_rect));
            out.extend(
                self.parts
                    .iter()
                    .map(|(part, rect)| (part.target().to_owned(), *rect)),
            );
            out.extend(Choice::ALL.into_iter().filter_map(|choice| {
                self.panel
                    .choice_rect(choice)
                    .map(|rect| (choice.target().to_owned(), rect))
            }));
            if let Some(rect) = self.panel.replace_field_rect() {
                out.push(("window-search-replacement".to_owned(), rect));
            }
        }
        out
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let button = |role: Role, name: String, target: &str, rect: Rect| {
            AccessNode::new(role)
                .with_name(name)
                .with_bounds(rect)
                .with_action_target(target)
                .with_state(AccessState::new().focusable(true))
        };
        if !self.is_open() {
            return Some(button(
                Role::Button,
                tr("window-search-name"),
                "window-search-open",
                self.anchor,
            ));
        }
        let mut children = Vec::new();
        if let Some(field) = self.field.accessibility() {
            let mut field = field
                .with_name(tr("window-search-placeholder"))
                .with_bounds(self.field_rect)
                .with_action_target("window-search-query");
            if let Some(count) = self.field.count() {
                field = field.with_description(crate::i18n::widget_format(
                    "find-bar-position",
                    &[
                        ("current", count.shown.into()),
                        ("total", count.total.into()),
                    ],
                ));
            }
            children.push(field);
        }
        for (part, rect) in &self.parts {
            let name = match part {
                Part::Previous => tr("find-bar-previous"),
                Part::Next => tr("find-bar-next"),
                Part::Options => {
                    children.push(self.options_node(*rect));
                    continue;
                }
                Part::App => self.app_label(),
                Part::Everywhere => tr("window-search-everywhere"),
                Part::Close => tr("window-search-close"),
            };
            children.push(button(Role::Button, name, part.target(), *rect));
        }
        Some(
            AccessNode::new(Role::Toolbar)
                .with_name(tr("window-search-name"))
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        if target == "window-search-query" && self.is_open() {
            if matches!(action, crate::a11y::Action::Focus) {
                self.take_keyboard();
                return true;
            }
            return self.field.perform_action(action, &[], state, path);
        }
        if let Some(handled) = self.perform_option_action(target, action, state, path) {
            return handled;
        }
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        if target == "window-search-open" && !self.is_open() {
            self.requests.push(WindowSearchRequest::Open);
            return true;
        }
        let Some(part) = self
            .parts
            .iter()
            .map(|(part, _)| *part)
            .find(|part| part.target() == target)
        else {
            return false;
        };
        self.activate(part);
        true
    }
}

#[cfg(test)]
mod replace_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::find_bar::FindMatch;
    use crate::widget::search_field::Filterable;

    fn leaf(
        taffy: &mut taffy::TaffyTree<()>,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        widget: &mut (dyn Widget + 'static),
    ) -> taffy::NodeId {
        use crate::widget::TaffyTreeExt;
        taffy.widget_leaf(taffy::Style::default(), node_map, widget)
    }

    /// A document that keeps what it found, which match is current and
    /// whether it was told to stop.
    #[derive(Default)]
    struct Doc {
        text: String,
        found: usize,
        current: Option<usize>,
        cleared: bool,
        /// A find the document ran on its own, for the search to take over.
        own: Option<crate::widget::find_bar::OwnFind>,
    }

    impl Findable for Doc {
        fn find(&mut self, query: &str, _options: FindOptions) -> Vec<FindMatch> {
            if query.is_empty() {
                self.found = 0;
                return Vec::new();
            }
            let hay = self.text.to_lowercase();
            let found: Vec<FindMatch> = hay
                .match_indices(&query.to_lowercase())
                .map(|(start, m)| FindMatch::new(start, start + m.len()))
                .collect();
            self.found = found.len();
            found
        }
        fn reveal(&mut self, index: usize) {
            self.current = Some(index);
        }
        fn clear_find(&mut self) {
            self.cleared = true;
            self.found = 0;
            self.current = None;
        }
        fn take_own_find(&mut self) -> Option<crate::widget::find_bar::OwnFind> {
            self.own.take()
        }
    }

    impl Widget for Doc {
        crate::provides!(dyn Findable);
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            leaf(taffy, node_map, self)
        }
        fn name(&self) -> &'static str {
            "Doc"
        }
    }

    /// A list of names that narrows to what contains the query.
    struct Names {
        all: Vec<&'static str>,
        shown: usize,
    }

    impl Filterable for Names {
        fn set_filter(&mut self, query: &str) {
            let query = query.to_lowercase();
            self.shown = self
                .all
                .iter()
                .filter(|name| name.to_lowercase().contains(&query))
                .count();
        }
        fn filter_count(&self) -> Option<FilterCount> {
            Some(FilterCount::new(self.shown, self.all.len()))
        }
    }

    impl Widget for Names {
        crate::provides!(dyn Filterable);
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            leaf(taffy, node_map, self)
        }
        fn name(&self) -> &'static str {
            "Names"
        }
    }

    fn typed(search: &mut WindowSearch, query: &str) {
        search.field.set_query(query);
    }

    #[test]
    fn a_document_highlights_its_matches_with_n_of_m_and_steps_through_them() {
        let mut doc = Doc {
            text: "Cat cat concat cat".into(),
            ..Doc::default()
        };
        assert_eq!(InPlace::of(&mut doc), InPlace::Document);
        let mut search = WindowSearch::new();
        search.open(InPlace::Document, MotionSource::Keyboard);
        typed(&mut search, "cat");
        assert!(search.drive(&mut doc), "the document changed what it shows");
        assert_eq!(search.position(), Some((1, 4)));
        assert_eq!(
            search.count(),
            Some(FilterCount::new(1, 4)),
            "the field reads 1 of 4"
        );
        assert_eq!(doc.current, Some(0), "the first match is shown");

        search.step(true);
        search.drive(&mut doc);
        assert_eq!(search.position(), Some((2, 4)));
        search.step(false);
        search.step(false);
        search.drive(&mut doc);
        assert_eq!(doc.current, Some(0), "only the last step asked counts");

        assert!(
            !search.drive(&mut doc),
            "nothing new: the app is left alone"
        );

        search.close(MotionSource::Keyboard);
        assert!(search.drive(&mut doc));
        assert!(doc.cleared, "closing stops the highlights");
        assert_eq!(search.in_place(), InPlace::Nothing);
    }

    #[test]
    fn a_find_the_document_ran_on_its_own_shows_its_query_and_steps_on_from_its_match() {
        let mut doc = Doc {
            text: "Cat cat concat cat".into(),
            ..Doc::default()
        };
        let mut search = WindowSearch::new();
        search.open(InPlace::Document, MotionSource::Keyboard);
        search.drive(&mut doc);

        // An action went to the third match of "cat" (an agent's result).
        let whole_word = FindOptions {
            whole_word: true,
            ..FindOptions::default()
        };
        doc.own = Some(crate::widget::find_bar::OwnFind {
            query: "cat".into(),
            options: whole_word,
            current: 2,
        });
        assert!(search.drive(&mut doc));
        assert_eq!(search.query(), "cat", "the field shows the query");
        assert_eq!(search.options(), whole_word, "and its options");
        assert_eq!(search.position(), Some((3, 4)), "the match it went to");
        assert_eq!(search.count(), Some(FilterCount::new(3, 4)));
        assert_eq!(doc.current, Some(2), "the view stays on that match");
        assert!(
            !search.drive(&mut doc),
            "the field's query is the one found: nothing runs again"
        );

        search.step(true);
        search.drive(&mut doc);
        assert_eq!(search.position(), Some((4, 4)), "it steps on from there");
        assert_eq!(doc.current, Some(3));
    }

    #[test]
    fn an_app_in_its_own_process_shows_the_find_it_took_over_without_sending_it_back() {
        use crate::window_toolbar::{AdoptedQuery, ToolbarEvent};
        let mut search = WindowSearch::new();
        search.open_remote(InPlace::Document, MotionSource::Keyboard);
        let _ = search.take_remote_events();
        let adopted = AdoptedQuery {
            serial: 1,
            query: "cat".into(),
            options: FindOptions::default(),
        };
        search.set_remote_adopted(&adopted);
        assert_eq!(search.query(), "cat", "the field shows the app's query");
        assert!(
            !search
                .take_remote_events()
                .iter()
                .any(|event| matches!(event, ToolbarEvent::Search { .. })),
            "the app already runs it"
        );
        typed(&mut search, "dog");
        search.set_remote_adopted(&adopted);
        assert_eq!(
            search.query(),
            "dog",
            "a find already shown never overwrites what is typed since"
        );
    }

    #[test]
    fn a_list_narrows_to_what_matches_and_comes_back_whole_on_close() {
        let mut names = Names {
            all: vec!["Ada", "Grace", "Adele", "Linus"],
            shown: 4,
        };
        assert_eq!(InPlace::of(&mut names), InPlace::List);
        let mut search = WindowSearch::new();
        search.open(InPlace::List, MotionSource::Pointer);
        typed(&mut search, "ad");
        assert!(search.drive(&mut names));
        assert_eq!(search.count(), Some(FilterCount::new(2, 4)), "2 of 4");
        assert!(!search.drive(&mut names), "the same query narrows once");
        search.close(MotionSource::Pointer);
        search.drive(&mut names);
        assert_eq!(names.shown, 4, "the whole list is back");
    }

    /// An app whose views are queries: it records what the search told it.
    #[derive(Default)]
    struct Views {
        opened: usize,
        texts: Vec<String>,
        entered: usize,
        closed: usize,
    }

    impl Queryable for Views {
        fn query_opened(&mut self) {
            self.opened += 1;
        }
        fn set_query_text(&mut self, text: &str) {
            self.texts.push(text.to_owned());
        }
        fn enter_results(&mut self) {
            self.entered += 1;
        }
        fn query_closed(&mut self) {
            self.closed += 1;
        }
    }

    impl Widget for Views {
        crate::provides!(dyn Queryable);
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            leaf(taffy, node_map, self)
        }
        fn name(&self) -> &'static str {
            "Views"
        }
    }

    #[test]
    fn a_query_view_app_hears_the_open_each_keystroke_enter_and_the_close() {
        let mut views = Views::default();
        assert_eq!(InPlace::of(&mut views), InPlace::Query);
        let mut search = WindowSearch::new();
        search.set_app_name("Music");
        search.open(InPlace::Query, MotionSource::Keyboard);
        assert!(search.drive(&mut views));
        assert_eq!(
            views.opened, 1,
            "the field is focused before anything is typed"
        );
        assert_eq!(views.texts, [""]);
        typed(&mut search, "aur");
        search.drive(&mut views);
        typed(&mut search, "aurora");
        search.drive(&mut views);
        assert_eq!(
            views.texts,
            ["", "aur", "aurora"],
            "each keystroke goes out"
        );
        assert!(!search.drive(&mut views), "the same text goes out once");
        search.step(true);
        assert!(
            !search.has_keyboard(),
            "Enter hands the keyboard to the results"
        );
        search.drive(&mut views);
        assert_eq!(views.entered, 1);
        search.close(MotionSource::Keyboard);
        search.drive(&mut views);
        assert_eq!(views.closed, 1, "Escape returns to the view before");
        assert_eq!(views.opened, 1);
    }

    /// The resting magnifier sits beside the app's own controls, so it says
    /// what it searches and the key that opens it, to the pointer and to
    /// assistive technology alike.
    #[test]
    fn the_resting_control_names_what_it_searches_and_its_key() {
        let mut search = WindowSearch::new();
        search.set_platform_conventions(PlatformConventions::new(crate::platform::Platform::Linux));
        let anchor = Rect::new(100.0, 8.0, 128.0, 36.0);
        search.place(anchor, 600.0);
        let (rect, tip) = search
            .tooltip_target_at(anchor.center())
            .expect("the control has a tooltip");
        assert_eq!(rect, anchor);
        assert_eq!(tip, "Search in this window (Ctrl+F)");
        let node = search.accessibility().expect("the control is announced");
        assert_eq!(node.name.as_deref(), Some("Search in this window"));
    }

    #[test]
    fn the_search_widens_from_the_view_to_the_app_to_everywhere() {
        let mut search = WindowSearch::new();
        search.open(InPlace::Document, MotionSource::Keyboard);
        search.activate(Part::App);
        search.activate(Part::Everywhere);
        assert_eq!(
            search.take_requests(),
            [
                WindowSearchRequest::Widen(SearchReach::App),
                WindowSearchRequest::Widen(SearchReach::Everywhere)
            ]
        );
        // With nothing to search in place, the control opens the platform
        // search: the window asks to open and decides where.
        let mut closed = WindowSearch::new();
        closed.place(Rect::new(100.0, 8.0, 128.0, 36.0), 600.0);
        assert!(!closed.is_open());
        assert_eq!(
            InPlace::of(&mut crate::widget::Spacer::new()),
            InPlace::Nothing
        );
    }

    #[test]
    fn the_open_search_gives_way_from_the_widest_reach_inwards_on_a_narrow_bar() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut search = WindowSearch::new();
        search.set_app_name("Notes");
        search.open(InPlace::Document, MotionSource::Keyboard);
        search.place(Rect::new(100.0, 6.0, 128.0, 34.0), 1100.0);
        search.layout(1.0, &mut text, &theme);
        let parts: Vec<Part> = search.parts.iter().map(|(p, _)| *p).collect();
        assert_eq!(
            parts,
            [
                Part::Previous,
                Part::Next,
                Part::Options,
                Part::App,
                Part::Everywhere,
                Part::Close
            ]
        );
        let field_min = field_min(&theme);
        assert!(search.field_rect.width() >= field_min);
        assert!(search.parts.iter().all(|(_, r)| r.x1 <= 1100.0));

        search.place(Rect::new(100.0, 6.0, 128.0, 34.0), 520.0);
        search.layout(1.0, &mut text, &theme);
        let parts: Vec<Part> = search.parts.iter().map(|(p, _)| *p).collect();
        assert!(
            !parts.contains(&Part::Everywhere),
            "everywhere gives way first"
        );
        assert!(
            parts.contains(&Part::Close),
            "the search can always be closed"
        );
        assert!(search.field_rect.width() >= field_min);
        assert!(search.parts.iter().all(|(_, r)| r.x1 <= 520.0 + 0.5));

        search.layout(0.5, &mut text, &theme);
        assert!(search.field_rect.width() > CONTROL_PX);
        assert!(
            search.field_rect.x0 == 100.0,
            "it grows from where the control stands"
        );

        // A phone-wide bar: the steps give way, Previous first, but the
        // options (the mode, Replace) and Close stay.
        let narrow = |reach: f64, search: &mut WindowSearch, text: &mut TextRenderer| {
            search.place(Rect::new(40.0, 6.0, 68.0, 34.0), 40.0 + reach);
            search.layout(1.0, text, &theme);
            search.parts.iter().map(|(p, _)| *p).collect::<Vec<Part>>()
        };
        let one_step = field_min + 3.0 * CONTROL_PX + 2.0 * f64::from(theme.tokens.spacing.s2);
        assert_eq!(
            narrow(
                one_step + 4.0 * f64::from(theme.tokens.spacing.s1),
                &mut search,
                &mut text
            ),
            [Part::Next, Part::Options, Part::Close],
            "Previous gives way before Next"
        );
        assert_eq!(
            narrow(200.0, &mut search, &mut text),
            [Part::Options, Part::Close],
            "the options never give way"
        );
        assert!(search.parts.iter().all(|(_, r)| r.x1 <= 240.0 + 0.5));
    }
}
