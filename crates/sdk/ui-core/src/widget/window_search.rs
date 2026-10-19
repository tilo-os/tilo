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
//! the [`Findable`] document or the [`Filterable`] list it already has, and
//! the window reaches them through its tile ([`SearchableWindow`] for an app
//! that runs inside its own runtime). An app in a process of its own is
//! reached over its shell connection instead: the search sends it the query,
//! the steps and the close ([`WindowSearch::take_remote_events`]) and shows
//! the count it answers with, and the app's runtime runs the same
//! [`SearchDriver`] on its widgets. An app with neither opens the platform
//! search narrowed to itself, as the control always did.

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
use crate::widget::find_bar::{FindOptions, FindSession, Findable, with_findable};
use crate::widget::search_field::{FilterCount, apply_filter, with_filterable};
use crate::widget::{Button, ButtonFace, ButtonRole, ControlSize, SearchField, Widget};

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
    /// A list ([`Filterable`]): it narrows to what matches.
    List,
}

impl InPlace {
    /// What `root`'s widgets offer: a document wins over a list, since a
    /// window showing both (a sidebar of notes and the open note) is read in
    /// its document.
    pub fn of(root: &mut dyn Widget) -> Self {
        if with_findable(root, &mut |_| ()).is_some() {
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
    /// The query a list was last narrowed to.
    applied: Option<String>,
    count: Option<FilterCount>,
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
        self.applied = None;
        self.count = None;
    }

    /// Carry `query` to the app's `root`, step when asked, or stop when
    /// `finishing` (after which the driver is closed). Returns whether
    /// anything the app shows changed.
    pub fn drive(
        &mut self,
        root: &mut dyn Widget,
        query: &str,
        step: Option<bool>,
        finishing: bool,
    ) -> bool {
        let options = FindOptions::default();
        let mut touched = false;
        match self.in_place {
            InPlace::Nothing => {}
            InPlace::Document => {
                let session = &mut self.session;
                with_findable(root, &mut |target: &mut dyn Findable| {
                    if finishing {
                        session.clear(target);
                        touched = true;
                        return;
                    }
                    if session.is_stale(query, options) {
                        session.search(target, query, options, Some(0));
                        touched = true;
                    } else if session.content_moved(target) {
                        session.refresh(target, query, options);
                        touched = true;
                    }
                    if let Some(forward) = step {
                        session.step(target, forward);
                        touched = true;
                    }
                });
                self.count = (!query.is_empty() && !finishing).then(|| {
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
        }
        if finishing {
            self.in_place = InPlace::Nothing;
            self.forget();
        }
        touched
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
    /// Widen it beyond the window.
    Widen(SearchReach),
}

/// A clickable part of the open search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Previous,
    Next,
    App,
    Everywhere,
    Close,
}

impl Part {
    fn target(self) -> &'static str {
        match self {
            Self::Previous => "window-search-previous",
            Self::Next => "window-search-next",
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

/// The window bar's search: a control that expands into a field in place.
pub struct WindowSearch {
    presence: Presence,
    field: SearchField,
    /// The resting control, where the field grows from.
    anchor: Rect,
    /// The furthest the open search may reach: rightwards to the window's
    /// own controls, or (trailing) leftwards to an app's location.
    limit: f64,
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

    /// Whether the field holds the keyboard.
    #[must_use]
    pub fn has_keyboard(&self) -> bool {
        self.is_open() && self.keyboard
    }

    /// What the search acts on in place while open.
    #[must_use]
    pub fn in_place(&self) -> InPlace {
        self.in_place
    }

    /// The query typed so far.
    #[must_use]
    pub fn query(&self) -> &str {
        self.field.value()
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
        self.trailing = false;
    }

    /// Place the control at the end of an app's slots: open, the search ends
    /// at `anchor` and reaches back as far as `reach`, covering the slots it
    /// stands over on the bar's ground.
    pub fn place_trailing(&mut self, anchor: Rect, reach: f64) {
        self.anchor = anchor;
        self.limit = reach;
        self.trailing = true;
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

    /// Open in place on what the app offers, the field holding the keyboard
    /// with its query selected (so typing replaces it, as Find does).
    pub fn open(&mut self, in_place: InPlace, source: MotionSource) {
        self.in_place = in_place;
        self.finishing = false;
        self.remote = false;
        self.sent = None;
        self.keyboard = true;
        self.field.set_focused(true);
        self.field.select_all();
        // The query runs again on what the app shows now.
        self.driver.open(in_place);
        self.presence.show(
            true,
            Disclose::SPEC,
            self.motion,
            source,
            crate::time::now(),
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
            self.step = None;
            self.in_place = InPlace::Nothing;
            self.field.set_count(None);
            return events;
        }
        if !self.is_open() || self.in_place == InPlace::Nothing {
            return events;
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
        events
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
        self.finishing = self.in_place != InPlace::Nothing;
        self.step = None;
        self.hover = None;
        self.presence.show(
            false,
            Disclose::SPEC,
            self.motion,
            source,
            crate::time::now(),
        );
    }

    /// Give the keyboard back to the app, leaving the search open with its
    /// matches (a click into the content).
    pub fn release_keyboard(&mut self) {
        self.keyboard = false;
        self.field.set_focused(false);
    }

    /// Take the keyboard again (Ctrl+F on an open search).
    pub fn take_keyboard(&mut self) {
        self.keyboard = true;
        self.field.set_focused(true);
        self.field.select_all();
    }

    /// Step to the next (or previous) match in a document.
    pub fn step(&mut self, forward: bool) {
        if self.in_place == InPlace::Document {
            self.step = Some(forward);
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
        let touched = self.driver.drive(root, &query, step, finishing);
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
        self.sent = None;
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
                if open {
                    // A selection drag in the field follows the pointer.
                    let mut field_handled = false;
                    self.field.handle_event(
                        event,
                        &mut crate::widget::EventCtx {
                            handled: &mut field_handled,
                            ..ctx.reborrow(ctx.path, ctx.node_id)
                        },
                    );
                }
                false
            }
            WindowEvent::CursorLeft { .. } => {
                self.rest_hover = false;
                self.hover = None;
                false
            }
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
                false
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
                self.field.handle_event(event, ctx);
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
        if let Some(part) = self.part_at(self.last_cursor) {
            self.activate(part);
            return true;
        }
        if self.field_rect.contains(self.last_cursor) {
            self.keyboard = true;
            self.field.set_focused(true);
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
        match key {
            Key::Named(NamedKey::Escape) => self.requests.push(WindowSearchRequest::Close),
            Key::Named(NamedKey::Enter | NamedKey::F3)
                if self.in_place == InPlace::Document && self.matches() > 0 =>
            {
                self.step(!modifiers.shift_key());
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
            // Tab hands the keyboard back to the content, the search open.
            Key::Named(NamedKey::Tab) => self.release_keyboard(),
            _ => self.field.handle_event(event, ctx),
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
        let room = if self.trailing {
            (self.anchor.x1 - self.limit).max(CONTROL_PX)
        } else {
            (self.limit - self.anchor.x0).max(CONTROL_PX)
        };
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
        let steps_w = if stepper { 2.0 * CONTROL_PX + gap } else { 0.0 };
        // What follows the field, dropped from the widest reach inwards when
        // the bar is narrow: everywhere, then the app, then the steps.
        let mut want_everywhere = true;
        let mut want_app = true;
        let mut want_steps = stepper;
        let after = |everywhere: bool, app: bool, steps: bool| {
            let mut w = CONTROL_PX + sibling;
            if steps {
                w += steps_w + sibling;
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
        for give_way in 0..3 {
            if room - after(want_everywhere, want_app, want_steps) >= field_min {
                break;
            }
            match give_way {
                0 => want_everywhere = false,
                1 => want_app = false,
                _ => want_steps = false,
            }
        }
        let field_w =
            (room - after(want_everywhere, want_app, want_steps)).clamp(CONTROL_PX, field_max);
        // Open, the search starts at the control and reaches right; at the
        // end of an app's slots it ends at the control and reaches back.
        let block = field_w + after(want_everywhere, want_app, want_steps);
        let start = if self.trailing {
            self.anchor.x1 - block
        } else {
            self.anchor.x0
        };
        let p = progress.clamp(0.0, 1.0);
        let drawn_x0 = self.anchor.x0 + (start - self.anchor.x0) * p;
        let drawn_w = CONTROL_PX + (field_w - CONTROL_PX) * p;
        self.field_rect = Rect::new(drawn_x0, y0, drawn_x0 + drawn_w, y1);
        self.field.apply_layout_external(self.field_rect);
        // The parts stand where the open search puts them, fading in.
        let mut x = start + field_w + sibling;
        let square = |x: f64| Rect::new(x, y0, x + CONTROL_PX, y1);
        if want_steps {
            self.parts.push((Part::Previous, square(x)));
            x += CONTROL_PX + gap;
            self.parts.push((Part::Next, square(x)));
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
    }

    fn get_rect(&self) -> Rect {
        if self.is_open() {
            let end = self.parts.last().map_or(self.field_rect.x1, |(_, r)| r.x1);
            Rect::new(self.anchor.x0, self.field_rect.y0, end, self.field_rect.y1)
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
            let name = tr("window-search-open");
            let face = ButtonFace::icon_only(self.anchor, Icon::Search, &name)
                .control_size(ControlSize::Small)
                .hovered(self.rest_hover);
            Button::paint(scene, transform, text, theme, &face);
            return;
        }
        self.layout(progress, text, theme);
        if self.trailing
            && let Some(ground) = self.ground
        {
            // Over an app's slots the open search covers what it stands on.
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

    fn needs_redraw(&self) -> bool {
        self.presence.is_moving(crate::time::now()) || self.field.needs_redraw()
    }

    fn needs_animation_frame(&self) -> bool {
        self.presence.is_moving(crate::time::now())
            || (self.has_keyboard() && self.field.needs_animation_frame())
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        // The caret's blink, while the field holds the keyboard.
        self.has_keyboard()
            .then(|| self.field.next_wake_deadline())
            .flatten()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if !self.is_open() {
            return self
                .anchor
                .contains(point)
                .then_some(winit::window::CursorIcon::Pointer);
        }
        if self.part_at(point).is_some() {
            return Some(winit::window::CursorIcon::Pointer);
        }
        self.field.cursor_shape(point)
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        if !self.is_open() {
            return self
                .anchor
                .contains(point)
                .then(|| (self.anchor, tr("window-search-open")));
        }
        let (part, rect) = self.parts.iter().find(|(_, r)| r.contains(point))?;
        let name = match part {
            Part::Previous => tr("find-bar-previous"),
            Part::Next => tr("find-bar-next"),
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
            .then(|| self.field.ime_cursor_rect())
            .flatten()
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        self.has_keyboard()
            .then(|| self.field.ime_content())
            .flatten()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
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
                tr("window-search-open"),
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

    // USER REQUIREMENT (2026-10-19): "In place: it acts on this window or
    // app. Lists filter, documents highlight with "n of m"."
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

    // USER REQUIREMENT (2026-10-19): "The scope widens step by step (this
    // view → this app → everywhere)."
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

        // Halfway open, the field is between the control and its width.
        search.layout(0.5, &mut text, &theme);
        assert!(search.field_rect.width() > CONTROL_PX);
        assert!(
            search.field_rect.x0 == 100.0,
            "it grows from where the control stands"
        );
    }
}
