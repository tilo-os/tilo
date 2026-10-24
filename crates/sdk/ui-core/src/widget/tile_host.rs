use crate::presence::PresenceCtx;
use crate::record::{RecordCheckout, RecordId, RecordStore};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use crate::widget::Spinner;
use crate::widget::TaffyTreeExt;
use crate::widget::app_widget::AppWidget;
use crate::widget::{AppModel, ModelPath};
use crate::widget::{
    AudiencePresentationProvider, AudiencePresentationWidgetExt, CompleteContentWidgetExt as _,
    Continuable, FloatingSurfaceProvider, FloatingSurfaceWidgetExt, FullscreenProvider,
    FullscreenWidgetExt, Widget,
};
use automerge::{AutoCommit, ObjId, ROOT};
use std::collections::{HashMap, VecDeque};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Shape};
use winit::event::WindowEvent;

use crate::motion::{Direction, WindowMotion};
use crate::tokens::{EffectiveMotion, MotionSource};

/// How far a window's arrival has come `elapsed` after it spawned: it fades
/// in and scales up in place (`motion::WindowMotion::OPEN`).
fn spawn_progress(elapsed: Duration) -> f64 {
    WindowMotion::OPEN.progress_after(
        Direction::Enter,
        EffectiveMotion::default(),
        MotionSource::Pointer,
        elapsed,
    )
}

/// How long a window's arrival runs.
fn spawn_time() -> Duration {
    WindowMotion::OPEN.settle_time(
        Direction::Enter,
        EffectiveMotion::default(),
        MotionSource::Pointer,
    )
}

/// How far the opening card's hand-over to the window has come: one motion
/// becoming another, a cross-fade (`motion::CROSS_FADE`).
fn handover_progress(elapsed: Duration) -> f64 {
    crate::motion::CROSS_FADE.progress_after(
        Direction::Exit,
        EffectiveMotion::default(),
        MotionSource::Pointer,
        elapsed,
    )
}

/// How long the hand-over runs.
fn handover_time() -> Duration {
    crate::motion::CROSS_FADE.settle_time(
        Direction::Exit,
        EffectiveMotion::default(),
        MotionSource::Pointer,
    )
}

/// One logical pixel: heavier reads as a drawn outline around the app,
/// not the boundary of a surface.
const EDGE_STROKE_WIDTH: f64 = 1.0;

/// Rects along `rect`'s outline that hold every pixel of an edge drawn
/// around it at corner `radius`: a square at each corner, where the edge
/// curves up to `radius` inside the outline, and between them a thin band
/// along each side, where it is a straight stroke at most its widest width
/// from the outline (outside it, or inside it when drawn flush). One more
/// pixel each way covers antialiasing. A focus change repaints these, so
/// the sides stay as thin as the stroke rather than as deep as the corners.
fn edge_bands(rect: Rect, radius: f64) -> Vec<Rect> {
    let outside = EDGE_STROKE_WIDTH + 1.0;
    let corner = radius.max(0.0) + outside;
    let outer = rect.inflate(outside, outside);
    let (left, right) = (rect.x0 + corner, rect.x1 - corner);
    let (top, bottom) = (rect.y0 + corner, rect.y1 - corner);
    if left >= right || top >= bottom {
        // Corners meeting: no straight side to keep thin.
        return vec![
            Rect::new(outer.x0, outer.y0, outer.x1, top.min(outer.y1)),
            Rect::new(outer.x0, bottom.max(outer.y0), outer.x1, outer.y1),
            Rect::new(outer.x0, outer.y0, left.min(outer.x1), outer.y1),
            Rect::new(right.max(outer.x0), outer.y0, outer.x1, outer.y1),
        ];
    }
    vec![
        Rect::new(outer.x0, outer.y0, left, top),
        Rect::new(right, outer.y0, outer.x1, top),
        Rect::new(outer.x0, bottom, left, outer.y1),
        Rect::new(right, bottom, outer.x1, outer.y1),
        Rect::new(left, outer.y0, right, rect.y0 + outside),
        Rect::new(left, rect.y1 - outside, right, outer.y1),
        Rect::new(outer.x0, top, rect.x0 + outside, bottom),
        Rect::new(rect.x1 - outside, top, outer.x1, bottom),
    ]
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum UndoStep {
    Undo,
    Redo,
}

fn undo_step(event: &WindowEvent, m: winit::event::Modifiers) -> Option<UndoStep> {
    if !(m.state().control_key() || m.state().super_key()) {
        return None;
    }
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return None;
    };
    if key.state != winit::event::ElementState::Pressed {
        return None;
    }
    let winit::keyboard::Key::Character(c) = &key.logical_key else {
        return None;
    };
    match c.as_str() {
        "z" | "Z" if m.state().shift_key() => Some(UndoStep::Redo),
        "z" | "Z" => Some(UndoStep::Undo),
        "y" | "Y" => Some(UndoStep::Redo),
        _ => None,
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AppKind {
    Terminal,
    Files,
    Editor,
    Browser,
    Settings,
    /// Any other app, by its own glyph from the icon set
    /// ([`crate::icons::app_glyph`]): a window wears its own app's mark,
    /// never another app's.
    App(crate::icons::Icon),
}

impl AppKind {
    pub fn as_str(&self) -> &str {
        match self {
            AppKind::Terminal => "Terminal",
            AppKind::Files => "Files",
            AppKind::Editor => "Editor",
            AppKind::Browser => "Browser",
            AppKind::Settings => "Settings",
            AppKind::App(_) => "App",
        }
    }
}

pub enum TileContent {
    App(Box<dyn AppWidget>),
    Stub {
        kind: AppKind,
        label: String,
    },
    /// Distinct from `Stub`'s "coming soon": `title` names the thing,
    /// `detail` says what's actually the case (e.g. a peer's app not
    /// installed here is absent, not coming soon).
    Notice {
        title: String,
        detail: String,
    },
    /// The place of an app whose code is still on its way (a browser
    /// bundle fetches each app's module the first time one of its windows
    /// opens). It has drawn nothing and is opening from the start, so its
    /// window keeps its frame and shows the opening card, as for any app
    /// asked for and not yet drawn; the app built in its place once the code
    /// is in takes the card over ([`TileHost::continue_opening_card`]). It
    /// holds no record and takes no focus or input: nothing typed while it
    /// waits lands in a field that is not there yet.
    Arriving,
}

/// How far around a widget's damage its drawing may reach: antialiasing,
/// shadows and focus rings outside its layout box, in logical pixels (as
/// the native window pads its widget damage).
const APP_DAMAGE_REACH: f64 = 16.0;

/// The app's drawings as a host comparing frames sees them
/// ([`crate::scene_parts::DamageHint`]): which generation it painted last,
/// where, and every widget's box then. A drawing at the same place changed
/// where its widgets say ([`Widget::collect_damage`]) and where widgets a
/// relayout moved were and are, as the native app window repaints; any
/// other drawing repaints the window.
#[derive(Debug)]
struct ContentDamage {
    source: u64,
    generation: u64,
    /// Where the last drawing placed the app, and its widgets' boxes then.
    painted: Option<(Affine, Vec<(crate::damage::WidgetIdentity, Rect)>)>,
    /// The last few drawings' damage, each with the generation it was
    /// measured from: a host may present only some of the drawings.
    steps: std::collections::VecDeque<(u64, Vec<Rect>)>,
}

/// How many drawings back a hint still says what changed.
const APP_DAMAGE_STEPS: usize = 4;

impl ContentDamage {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            source: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            generation: 0,
            painted: None,
            steps: std::collections::VecDeque::new(),
        }
    }

    /// Note that `app` was just drawn at `placement`, clipped to `clip`, and
    /// say what changed since its last drawing. No hint when it moved, when
    /// its widgets are not the ones drawn before (which pixels moved cannot
    /// be said), or when widgets paint other people's marks placed from
    /// their content, which their damage does not cover.
    fn hint(
        &mut self,
        app: &dyn Widget,
        placement: Affine,
        clip: Rect,
        presence: &PresenceCtx,
    ) -> Option<crate::scene_parts::DamageHint> {
        let boxes = crate::damage::widget_boxes(app);
        let from = self.generation;
        self.generation = self.generation.wrapping_add(1);
        let previous = self.painted.replace((placement, boxes));
        let moved = previous
            .filter(|(placed, _)| *placed == placement)
            .and_then(|(_, before)| {
                let (_, after) = self.painted.as_ref()?;
                crate::damage::moved_boxes(&before, after)
            });
        let Some(moved) = moved.filter(|_| !presence.places_content_marks(crate::time::now()))
        else {
            self.steps.clear();
            return None;
        };
        let mut damage = moved;
        app.collect_damage(&mut damage);
        let rects = damage
            .into_iter()
            .map(|rect| {
                placement
                    .transform_rect_bbox(rect.inflate(APP_DAMAGE_REACH, APP_DAMAGE_REACH))
                    .intersect(clip)
            })
            .filter(|rect| !rect.is_zero_area())
            .collect::<Vec<_>>();
        let earlier = (0..self.steps.len())
            .map(|first| {
                let since = self.steps.range(first..).flat_map(|(_, rects)| rects);
                (
                    self.steps[first].0,
                    since.chain(&rects).copied().collect::<Vec<_>>(),
                )
            })
            .collect();
        self.steps.push_back((from, rects.clone()));
        if self.steps.len() > APP_DAMAGE_STEPS {
            self.steps.pop_front();
        }
        Some(crate::scene_parts::DamageHint {
            source: self.source,
            from,
            to: self.generation,
            placement,
            rects,
            copies: Vec::new(),
            earlier,
        })
    }

    /// The app was not drawn: its next drawing follows none.
    fn forget(&mut self) {
        self.painted = None;
        self.steps.clear();
    }
}

/// A tab is an open thing, not a view onto one shared thing: the document
/// behind it is the tab's own, otherwise two tabs of the same app would
/// share one document, and undo/presence/search with it.
struct Binding {
    store: RecordStore,
    /// Where the tab's content lives while no file names it. Never
    /// changes for the life of the tab.
    tab: RecordCheckout,
    /// `tab.id` until the hosted app declares a file through
    /// [`Widget::record_intent`](crate::record::capability::record_intent) —
    /// opening a document *is* moving the tab onto that document's record;
    /// the scratch record it leaves behind is still its own document.
    live: RecordId,
    /// Whether the app has taken this record's document through a first
    /// `sync` yet. What that pass writes (the object a body lives in, the
    /// seed a fresh note carries) is the document's *shape*, not something
    /// the user did, so the history's baseline is taken after it —
    /// otherwise the first Ctrl+Z deletes the body wholesale instead of
    /// reverting what was typed into it.
    established: bool,
    /// A close/replacement may keep this binding alive. Suppress another
    /// settlement only while its exact committed frontier remains unchanged.
    settled_frontier: Option<Vec<automerge::ChangeHash>>,
}

struct PendingBinding {
    store: RecordStore,
    tab: RecordCheckout,
    create: bool,
}

struct OwnerTransitionError {
    message: String,
    retry_pending: bool,
}

impl Binding {
    fn settle_outgoing(&mut self) -> Option<crate::application::ComponentCloseReport> {
        // This comparison must not commit a local draft or advance any sibling.
        if let Some((heads, 0)) = self.store.read(&self.live, crate::doc::settle_mark)
            && self.settled_frontier.as_ref() == Some(&heads)
        {
            return None;
        }
        let error = self.store.try_flush_record(&self.live).err();
        // Capture only this owner's post-settlement committed frontier. Pending
        // edits or an unavailable document cannot establish a reusable marker.
        self.settled_frontier = self
            .store
            .read(&self.live, |doc| {
                let (heads, pending) = crate::doc::settle_mark(doc);
                (pending == 0).then_some(heads)
            })
            .flatten();
        Some(crate::application::ComponentCloseReport {
            recovery: None,
            view_error: None,
            view_retry: None,
            record_retry: error.as_ref().map(|_| self.store.clone()),
            record_retry_target: error.as_ref().map(|_| self.live.clone()),
            record_error: error,
            teardown_error: None,
        })
    }

    fn follow_checked(
        &mut self,
        want: Option<RecordCheckout>,
        reports: &mut Vec<crate::application::ComponentCloseReport>,
    ) -> Result<RecordId, crate::record::RecordOpenError> {
        if want.as_ref().is_some_and(|want| want.id != self.live)
            && let Some(report) = self.settle_outgoing()
        {
            reports.push(report);
        }
        let requested = want
            .as_ref()
            .or_else(|| (self.live == self.tab.id).then_some(&self.tab));
        if let Some(requested) = requested {
            self.store.open_existing_record_checked(requested)?;
        }
        if let Some(want) = want
            && want.id != self.live
        {
            self.live = want.id;
            self.established = false;
            self.settled_frontier = None;
        }
        Ok(self.live.clone())
    }
}

/// Why the tile's record cannot be opened, as the store said it: the
/// technical detail behind the card's "Details". A document the store is
/// still opening on its own thread is not a failure ([`OPENING`]).
fn record_unavailable(error: &crate::record::RecordOpenError) -> String {
    match error {
        crate::record::RecordOpenError::Opening => OPENING.to_owned(),
        other => other.to_string(),
    }
}

/// What the tile says while the store opens its record.
const OPENING: &str = "Opening the document";

/// The name of the card a tile shows while its record cannot be opened.
#[must_use]
pub fn record_unavailable_title() -> String {
    crate::i18n::I18nManager::lookup_plain("record-unavailable-title")
}

/// The card a tile shows while its record cannot be opened: what happened
/// and what to do, in the person's words, with the store's reason (`detail`)
/// behind "Details". While the store is still opening it, the tile says so.
fn record_unavailable_card(detail: &str) -> super::EmptyState {
    if detail == OPENING {
        return super::EmptyState::new(OPENING).busy();
    }
    super::EmptyState::new(record_unavailable_title())
        .with_icon(crate::icons::Icon::File)
        .with_message(crate::i18n::I18nManager::lookup_plain(
            "record-unavailable-hint",
        ))
        .with_details(detail)
}

fn admit_record(
    store: &RecordStore,
    checkout: &RecordCheckout,
    create: bool,
) -> Result<(), crate::record::RecordOpenError> {
    if create && store.canonical_namespace().is_none() {
        return store.open_record_checked(checkout);
    }
    match store.open_existing_record_checked(checkout) {
        Ok(()) => Ok(()),
        Err(crate::record::RecordOpenError::MissingRecord) if create => {
            let mut reservation = store.reserve_record(checkout)?;
            reservation.initialize(|_| Ok(()))
        }
        Err(error) => Err(error),
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "history policy, record refusal, focus, layout dirtiness and presentation latches are independent"
)]
pub struct TileHost {
    content: TileContent,
    history: Option<Box<crate::history_mode::host::HistoryHost>>,
    /// Where the History control stands while History is closed: `None` in
    /// the app's own header row (a tile outside a desktop window), `Some`
    /// once a desktop window hosts the tile. `Some(true)`: the window's tab
    /// row presents it, so the tile draws no History control of its own;
    /// `Some(false)`: the tab row is hidden (a maximized window), so the
    /// app's header row may hold it, but never a band of its own.
    window_history: Option<bool>,
    automatic_history: bool,
    live_callback_node: Option<NodeId>,
    retained_invocations: HashMap<tilo_actions::InvocationId, crate::history_mode::HistoryOwner>,
    retained_updates: HashMap<tilo_actions::InvocationId, VecDeque<tilo_actions::InvocationUpdate>>,
    /// Operations the component shown admitted that have not ended, so a
    /// component taking its place cannot leave them unanswered.
    component_invocations: std::collections::HashSet<tilo_actions::InvocationId>,
    owner_bound_work: bool,
    owner_bound_custody: Option<crate::history_mode::HistoryOwner>,
    close_reports: Vec<crate::application::ComponentCloseReport>,
    /// `None` when nothing is bound — a bare `ui` host, a stub card, a
    /// test exercising only paint. Unbound tiles edit whatever document
    /// the parent pass carries.
    binding: Option<Binding>,
    history_lifetime: std::rc::Rc<crate::history_mode::owner::LegacyHistoryLifetime>,
    custody_parent: Option<crate::ControlDomain>,
    custody_scope: Option<crate::ControlDomain>,
    custody_renewal: Option<crate::control_custody::CustodyAttachment>,
    pending_binding: Option<PendingBinding>,
    record_error: Option<String>,
    /// The card shown for `record_error`, with the detail it was made for.
    error_card: Option<(String, super::EmptyState)>,
    record_transition_error: Option<String>,
    record_intent_transition_blocked: bool,
    record_creation_refused: bool,
    announced: Option<RecordId>,
    /// Set by [`TileHost::replace_content`]; drained by
    /// [`Widget::take_structure_dirty`]. Tells the layout pass this
    /// tile's node shape changed even though no hosted widget reported
    /// anything.
    structure_dirty: bool,
    /// Latched: once this tile has shown something, its window stays on
    /// screen for good. Must survive [`TileHost::replace_content`] — a
    /// peer's already-open window arrives as a card, then gets replaced
    /// by this machine's own not-yet-drawn app instance, and without the
    /// latch the window would blink out and back.
    was_presented: std::cell::Cell<bool>,
    /// Readiness of this occupant, separate from the tile's visibility.
    /// Reset on replacement so a visible notice cannot mark an app ready.
    content_was_presented: std::cell::Cell<bool>,
    /// The tab this tile shows, named to [`panic_scope`](super::panic_scope)
    /// while its application runs. Stamped by the owning `WindowGroup`.
    panic_scope: std::cell::Cell<Option<u64>>,
    /// The window this tile's app is shown in, when its host draws the
    /// app's slots in the window's bar ([`Self::set_hosted_window`]).
    hosted_window: Option<crate::hosted_window::HostedWindow>,
    rect: Rect,
    /// How far [`Self::offset_layout`] moved the tile from where layout put
    /// it. The content keeps its own layout rect, so it is drawn this much
    /// further right instead.
    content_shift: f64,
    /// What the app drew last, for a host that repaints only where it
    /// changed ([`ContentDamage`]).
    content_damage: ContentDamage,
    pub focused: bool,
    /// Last pointer position, so a button press (which carries none) can be
    /// attributed to this tile or to a neighbour.
    pointer: Option<Point>,
    /// Adjacent canvas windows own exactly their layout rectangle, including
    /// their border; neither outlines nor shadows may spill into a sibling.
    flush_edges: bool,
    /// The window's frame is growing past what this tile last drew (a
    /// window maximizing keeps its last layout until its frame rests): the
    /// frame's ground continues the tile, so no edge marks where it ends.
    inside_growing_frame: bool,
    /// The window fills its display ([`Widget::set_full_bleed`]): square,
    /// unclipped and without an edge.
    full_bleed: bool,
    /// The tile's bottom edge is the display's (a maximized window): its
    /// lower corners are square, its upper ones stay round under its bar.
    square_bottom: bool,
    /// `None` by default, matching the "animations only fire for runtime
    /// insertions" rule `TabBar` uses.
    spawned_at: Option<Instant>,
    /// What the window showed when the windows were held still
    /// ([`TileHost::hold_content`]), drawn in place of the live content
    /// until released.
    held: Option<held::HeldContent>,
    /// Pushed down by the parent [`crate::widget::WindowGroup`] every
    /// frame: the pill is where a wayland app-id becomes a name a person
    /// recognises, and resolving it a second time down here would only be
    /// a second chance for the two to disagree.
    opening_name: String,
    opening_spinner: Spinner,
    /// Set on the first frame the opening card is actually painted: a
    /// card nobody saw has nothing to dissolve, and fading one out over
    /// an app that opened in a blink would invent a transition out of a
    /// placeholder that was never on screen.
    opening_shown: bool,
    /// Drives the dissolve; only ever set once `opening_shown` is.
    handed_over_at: Option<Instant>,
}

/// Larger than the toolkit default so it holds its own under a title on a
/// full-window card, small enough to stay an indicator rather than the
/// subject of the screen.
const OPENING_SPINNER_PX: f64 = 36.0;

/// Mirrors `OPENING_SPINNER_PX` as an `f32`: `Spinner::with_size` takes an
/// `f32`, while every rect computation below stays in `f64` alongside the
/// rest of this widget's geometry.
const OPENING_SPINNER_PX_F32: f32 = 36.0;

const OPENING_SPINNER_GAP: f64 = 24.0;

/// While a tile's application runs: its tab named to a panic hook, and its
/// window current for its bar's slots.
struct Running {
    _panic: super::panic_scope::Scope,
    _window: crate::window_toolbar::WindowScope,
}

impl TileHost {
    /// Move the tile by `dx`; see
    /// [`super::window_group::WindowGroup::offset_layout`]. The content keeps
    /// the rect it was laid out at and is drawn translated instead.
    pub(crate) fn offset_layout(&mut self, dx: f64) {
        if dx == 0.0 {
            return;
        }
        self.rect = self.rect + vello::kurbo::Vec2::new(dx, 0.0);
        self.content_shift += dx;
    }

    fn install_live_custody(&mut self, attachment: crate::control_custody::CustodyAttachment) {
        let Some(parent) = self.custody_parent.clone() else {
            return;
        };
        let owner = self.history_owner();
        let domain = match &owner {
            Some(owner) => match &owner.0 {
                crate::history_mode::owner::OwnerKind::Component(binding) => {
                    binding.control_domain.upgrade()
                }
                crate::history_mode::owner::OwnerKind::Legacy { .. } => {
                    let domain = self.history_lifetime.control_domain.clone();
                    domain.set_owner(crate::control_custody::ControlOwner::Legacy(owner.clone()));
                    Some(domain)
                }
            },
            None if self.has_separate_document_owner() => None,
            None => Some(self.history_lifetime.control_domain.clone()),
        };
        if let Some(domain) = domain {
            if owner.is_none() && domain.inherit_owner(&parent).is_err() {
                parent.unsafe_attachment();
                return;
            }
            let scope = self.custody_scope.get_or_insert_with(|| parent.child());
            if scope
                .attach_to(&parent)
                .and_then(|()| domain.attach_to(scope))
                .is_err()
            {
                parent.unsafe_attachment();
                return;
            }
            let attachment = self.custody_renewal.take().unwrap_or(attachment);
            if let TileContent::App(root) = &mut self.content {
                crate::widget::controls::install_control_custody(
                    root.as_mut(),
                    &domain,
                    attachment,
                );
            }
            if let Some(history) = &mut self.history {
                history.set_control_domain(&domain);
            }
        }
    }

    pub fn take_host_intent(&mut self) -> Option<crate::widget::AppHostIntent> {
        match &mut self.content {
            TileContent::App(root) => root.take_host_intent(),
            _ => None,
        }
    }

    /// Bind a prepared component's wake lifetime before mounting it in a group.
    /// Setup failure retains the component's record and recovery reports.
    pub fn mount_component(
        mut component: crate::application::AppComponent,
        sink: impl Fn(crate::wake::WakeEvent) + Send + Sync + 'static,
    ) -> Result<Self, crate::launch::LaunchError> {
        if let Err(error) = component.mount(sink) {
            return Err(component.setup_error(error));
        }
        Ok(Self::from_component(component))
    }

    pub fn from_component(mut component: crate::application::AppComponent) -> Self {
        let registration = component.history.take();
        let mut tile = Self::new(TileContent::App(Box::new(
            crate::widget::app_widget::BoundAppWidget::new(component),
        )));
        if let Some(registration) = registration {
            tile.install_history_registration(registration);
        }
        tile
    }
    fn install_history_registration(
        &mut self,
        registration: crate::history_mode::HistoryRegistration,
    ) {
        let owner = self.history_owner();
        match crate::history_mode::host::HistoryHost::new(registration, &AutoCommit::new()) {
            Ok(mut history) => {
                history.set_enclosing_owner(owner);
                history.set_keyboard_active(false);
                if let Some(tab_presents) = self.window_history {
                    history.remove_fallback_bar();
                    history.set_open_button(!tab_presents);
                    history.set_window_presents(tab_presents);
                }
                self.history = Some(Box::new(history));
                self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
                self.structure_dirty = true;
            }
            Err(error) => {
                self.record_error = Some(format!("History unavailable: {error}"));
                self.structure_dirty = true;
            }
        }
    }

    /// A desktop window hosts this tile; `tab_presents` says whether the
    /// window's tab row shows (and so presents the History control). The
    /// tile then never draws a History band of its own: closed, History is
    /// opened from the tab row, or from the app's header row while the tab
    /// row is hidden.
    pub fn set_window_presents_history(&mut self, tab_presents: bool) {
        if self.window_history == Some(tab_presents) {
            return;
        }
        self.window_history = Some(tab_presents);
        if let Some(history) = &mut self.history {
            history.remove_fallback_bar();
            history.set_open_button(!tab_presents);
            history.set_window_presents(tab_presents);
            self.structure_dirty = true;
        }
        if let Some(app) = self.app_history_mut() {
            app.set_window_presents_history(tab_presents);
        }
    }

    /// An app shown with a History of its own (an App in a region of the
    /// host's scene), rather than the one this tile hosts for a component.
    fn app_history(&self) -> Option<&dyn crate::widget::WindowHistory> {
        match &self.content {
            TileContent::App(app) => super::capability::capability::<
                dyn crate::widget::WindowHistory,
            >(app.as_ref() as &dyn Widget),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    fn app_history_mut(&mut self) -> Option<&mut (dyn crate::widget::WindowHistory + 'static)> {
        match &mut self.content {
            TileContent::App(app) => super::capability::capability_mut::<
                dyn crate::widget::WindowHistory,
            >(app.as_mut() as &mut dyn Widget),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    /// The History control the window's tab row shows for this tile: open
    /// or closed, or `None` where the tile has no History to open.
    #[must_use]
    pub fn history_control(&self) -> Option<crate::widget::HistoryControl> {
        match &self.history {
            Some(history) => history
                .offers_control()
                .then(|| crate::widget::HistoryControl::of(history.is_open())),
            None => self
                .app_history()
                .and_then(crate::widget::WindowHistory::window_history_control),
        }
    }

    /// Press the window's History control for this tile: open History, or
    /// close it while it shows, through the same History door the History
    /// shortcut and the palette use.
    ///
    /// # Errors
    ///
    /// The tile has no History, or its door refused the operation.
    pub fn invoke_history_control(
        &mut self,
        control: crate::widget::HistoryControl,
    ) -> Result<(), String> {
        let Some(history) = self.history.as_deref_mut() else {
            return self
                .app_history_mut()
                .ok_or_else(|| "History is unavailable".to_owned())?
                .invoke_window_history_control(control);
        };
        let intent = crate::standalone_history_host::history_control_binding(control)
            .invoke(())
            .map_err(|error| error.to_string())?;
        crate::history_mode::embedded::run_door_control(history, &intent)
            .map_err(|error| error.to_string())?;
        self.structure_dirty = true;
        Ok(())
    }

    /// Queue `invocation` as one of the hosted app's own controls: an action
    /// the person chose over this window (its in-app search) runs inside the
    /// app, under the app's custody and permissions, exactly as clicking the
    /// control in it would. The outer document never resolves an app's
    /// actions by address, so this is the only way such a choice reaches
    /// the app.
    pub fn queue_control(
        &mut self,
        invocation: tilo_actions::ControlInvocation,
    ) -> Result<(), tilo_actions::ControlInvocation> {
        let Some(binding) = crate::widget::Widget::callback_binding(self) else {
            return Err(invocation);
        };
        let Ok(_entry) = binding.enter() else {
            return Err(invocation);
        };
        let mut queued = binding.queued_controls.borrow_mut();
        if queued.len() >= 64 {
            queued.pop_front();
        }
        queued.push_back(invocation);
        Ok(())
    }

    /// Run `visit` on the root of this tile's app, where the window's search
    /// finds a document or a list ([`crate::widget::WindowSearch`]); `visit`
    /// says whether it changed what the app shows. `None` for a tile with no
    /// app (a placeholder, a notice).
    pub fn visit_search_root(
        &mut self,
        visit: &mut dyn FnMut(&mut dyn Widget) -> bool,
    ) -> Option<bool> {
        let TileContent::App(app) = &mut self.content else {
            return None;
        };
        let app = app.as_mut() as &mut dyn Widget;
        let runtime =
            super::capability::capability::<dyn crate::widget::SearchableWindow>(&*app).is_some();
        let changed = if runtime {
            super::capability::capability_mut::<dyn crate::widget::SearchableWindow>(app)
                .is_some_and(|window| window.visit_search_root(visit))
        } else {
            visit(app)
        };
        Some(changed)
    }

    /// What the window's search can act on in this tile's app.
    pub fn in_place_search(&mut self) -> crate::widget::InPlace {
        let mut found = crate::widget::InPlace::Nothing;
        let _ = self.visit_search_root(&mut |root| {
            found = crate::widget::InPlace::of(root);
            false
        });
        found
    }

    /// Inactive retained tiles have no nodes in the current Taffy tree.
    pub(crate) fn discard_layout_nodes(&mut self) {
        self.live_callback_node = None;
        if let Some(history) = &mut self.history {
            history.discard_layout_nodes();
        }
    }
    pub fn register_document_history(
        &mut self,
        definition: &'static crate::history_mode::HistoryDefinition,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        use crate::history_mode::{
            HistoryPresentation, HistoryRegistration, HistoryRegistrationError,
            HistorySourceRegistration,
        };
        self.resolve_record()
            .map_err(|_| HistoryRegistrationError::UnsupportedOwner)?;
        let owner = self
            .history_owner()
            .ok_or(HistoryRegistrationError::UnsupportedOwner)?;
        let sources = definition
            .scopes
            .iter()
            .map(|scope| match scope.presentation {
                HistoryPresentation::Domain { .. }
                | HistoryPresentation::DomainWithAssets { .. } => {
                    HistorySourceRegistration::owned(scope.key, definition.app_id, owner.clone())
                }
                HistoryPresentation::External { reason } => {
                    Ok(HistorySourceRegistration::Unversioned {
                        scope: scope.key,
                        reason,
                    })
                }
                HistoryPresentation::CheckedFiles { .. } => {
                    Err(HistoryRegistrationError::UnsupportedOwner)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.install_history_registration(HistoryRegistration::new(definition, sources)?);
        if self.record_error.is_some() {
            return Err(HistoryRegistrationError::UnsupportedOwner);
        }
        Ok(())
    }

    pub fn new(content: TileContent) -> Self {
        let registration = match &content {
            TileContent::App(root) => {
                crate::history_mode::capability::history_registration(root.as_ref())
            }
            _ => Ok(None),
        };
        let mut tile = Self {
            content,
            history: None,
            window_history: None,
            automatic_history: false,
            live_callback_node: None,
            retained_invocations: HashMap::new(),
            retained_updates: HashMap::new(),
            component_invocations: std::collections::HashSet::new(),
            owner_bound_work: false,
            owner_bound_custody: None,
            close_reports: vec![],
            binding: None,
            history_lifetime: crate::history_mode::owner::LegacyHistoryLifetime::new(),
            custody_parent: None,
            custody_scope: None,
            custody_renewal: None,
            pending_binding: None,
            record_error: None,
            error_card: None,
            record_transition_error: None,
            record_intent_transition_blocked: false,
            record_creation_refused: false,
            announced: None,
            structure_dirty: false,
            was_presented: std::cell::Cell::new(false),
            content_was_presented: std::cell::Cell::new(false),
            panic_scope: std::cell::Cell::new(None),
            hosted_window: None,
            rect: Rect::ZERO,
            content_shift: 0.0,
            content_damage: ContentDamage::new(),
            focused: false,
            pointer: None,
            flush_edges: false,
            inside_growing_frame: false,
            full_bleed: false,
            square_bottom: false,
            spawned_at: None,
            held: None,
            opening_name: String::new(),
            opening_spinner: Spinner::with_size(OPENING_SPINNER_PX_F32),
            opening_shown: false,
            handed_over_at: None,
        };
        match registration {
            Ok(Some(registration)) => tile.install_history_registration(registration),
            Err(error) => tile.record_error = Some(format!("History unavailable: {error}")),
            Ok(None) => {}
        }
        tile
    }

    pub(crate) fn set_flush_edges(&mut self, flush: bool) {
        self.flush_edges = flush;
    }

    pub(crate) fn set_inside_growing_frame(&mut self, inside: bool) {
        self.inside_growing_frame = inside;
    }

    /// Offer the window's bar to the hosted app's header row (see
    /// [`crate::widget::WindowHeader::set_window_bar`]). Returns whether a
    /// row took it. Only an app drawn in this process has a row to merge
    /// with; a foreign surface answers `false`.
    pub(crate) fn offer_window_bar(&mut self, bar: Option<super::WindowBar>) -> bool {
        let _running = self.running();
        match &mut self.content {
            TileContent::App(w) => {
                super::window_header::offer_window_bar(w.as_mut() as &mut dyn Widget, bar)
            }
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        }
    }

    /// The hosted app's header row holding the window's bar, after layout.
    pub(crate) fn window_bar_slot(&self) -> Option<super::WindowBarSlot> {
        match &self.content {
            TileContent::App(w) => super::window_header::window_bar_slot(w.as_ref() as &dyn Widget),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    /// Name `tab` to a panic hook while this tile's application runs: see
    /// [`panic_scope`](super::panic_scope).
    pub fn set_panic_scope(&self, tab: u64) {
        self.panic_scope.set(Some(tab));
    }

    fn running(&self) -> Running {
        Running {
            _panic: super::panic_scope::enter(self.panic_scope.get()),
            _window: crate::window_toolbar::enter(self.hosted_window.as_ref()),
        }
    }

    /// Show this tile's app in `window`: while the app runs, its
    /// [`crate::window_toolbar`] calls act on `window`, where the host reads
    /// its slots for the window's bar and hands back what the person did with
    /// them ([`crate::window_slots::WindowSlots`]). For a host that runs
    /// several apps in its own process and draws their bars.
    pub fn set_hosted_window(&mut self, window: crate::hosted_window::HostedWindow) {
        self.hosted_window = Some(window);
    }

    /// The window this tile's app is shown in, if the host gave it one.
    #[must_use]
    pub fn hosted_window(&self) -> Option<&crate::hosted_window::HostedWindow> {
        self.hosted_window.as_ref()
    }

    pub fn set_opening_name(&mut self, name: &str) {
        if self.opening_name != name {
            self.opening_name.clear();
            self.opening_name.push_str(name);
        }
    }

    /// Test seam for the parent group's push — a card naming the wrong
    /// app, or an app-id, is the defect this exists to catch.
    /// Built in place of `previous` (an app whose code just arrived, in
    /// the window that held its place): when `previous` had its opening card
    /// on screen, this host dissolves that card over what it shows instead of
    /// cutting to it, so the window fills in rather than jumping.
    pub fn continue_opening_card(&mut self, previous: &Self) {
        if previous.opening_shown && previous.handed_over_at.is_none() {
            self.opening_shown = true;
            self.set_opening_name(&previous.opening_name);
        }
    }

    #[cfg(test)]
    pub(crate) fn opening_name_for_test(&self) -> &str {
        &self.opening_name
    }

    #[cfg(test)]
    pub(crate) fn mark_opening_shown_for_test(&mut self) {
        self.opening_shown = true;
    }

    fn handover_in_flight(&self) -> bool {
        self.handed_over_at
            .is_some_and(|t| crate::time::now().saturating_duration_since(t) < handover_time())
    }

    /// Idempotent on the same record — rebinding would hand the tab a
    /// fresh empty document, and the tree re-binds every tile on rebuild.
    pub fn bind_record(&mut self, store: RecordStore, tab: RecordCheckout) {
        // A refused creation remains a document error until explicit replacement.
        if self.record_creation_refused {
            return;
        }
        if !self.uses_host_record() {
            self.clear_record_binding();
            return;
        }
        self.bind_record_intent(store, tab, false);
        self.install_declared_history();
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
    }

    /// Admit a newly created scratch resource owned by this caller. Creation
    /// applies only to this exact checkout, never to later record intents.
    pub fn bind_or_create_record(&mut self, store: RecordStore, tab: RecordCheckout) {
        // A refused creation remains a document error until explicit replacement.
        if self.record_creation_refused {
            return;
        }
        if !self.uses_host_record() {
            self.clear_record_binding();
            return;
        }
        self.bind_record_intent(store, tab, true);
        self.install_declared_history();
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
    }

    fn uses_host_record(&self) -> bool {
        match &self.content {
            TileContent::App(app) => app.uses_host_record(),
            TileContent::Stub { .. } => true,
            TileContent::Notice { .. } | TileContent::Arriving => false,
        }
    }

    /// External surfaces and symbolic notices own no host record: detach any earlier record
    /// custody so the outer document is never lent to them.
    fn clear_record_binding(&mut self) {
        if self.binding.is_none() && self.pending_binding.is_none() && self.record_error.is_none() {
            return;
        }
        self.settle_outgoing_record();
        self.binding = None;
        self.pending_binding = None;
        self.record_error = None;
        self.record_transition_error = None;
        self.record_intent_transition_blocked = false;
        self.record_creation_refused = false;
        self.announced = None;
        self.renew_history_lifetime();
    }

    /// Keep a failed fresh creation unavailable without attaching an existing
    /// record with the same spelling. Ordinary binding refresh cannot clear it;
    /// the owner must explicitly replace the resource/content first.
    pub fn refuse_record_binding(&mut self, reason: String) {
        if self.record_creation_refused && self.record_error.as_ref() == Some(&reason) {
            return;
        }
        self.replace_content(TileContent::Notice {
            title: record_unavailable_title(),
            detail: reason.clone(),
        });
        self.binding = None;
        self.pending_binding = None;
        self.history = None;
        self.retained_invocations.clear();
        self.record_error = Some(reason);
        self.record_creation_refused = true;
    }

    #[must_use]
    pub fn record_creation_refused(&self) -> bool {
        self.record_creation_refused
    }

    #[must_use]
    pub fn record_transition_error(&self) -> Option<&str> {
        self.record_transition_error.as_deref()
    }

    fn install_declared_history(&mut self) {
        if self.automatic_history || self.record_error.is_some() || self.binding.is_none() {
            return;
        }
        let definition = match &self.content {
            TileContent::App(root) => {
                crate::history_mode::capability::document_history_definition(root.as_ref())
            }
            _ => None,
        };
        let Some(definition) = definition else {
            return;
        };
        if self.history.is_some() {
            self.record_error = Some("Conflicting History declarations for this tile".into());
            return;
        }
        match self.register_document_history(definition) {
            Ok(()) => self.automatic_history = true,
            Err(error) => self.record_error = Some(format!("History unavailable: {error}")),
        }
    }

    fn settle_outgoing_record(&mut self) {
        if let Some(binding) = &mut self.binding
            && let Some(report) = binding.settle_outgoing()
        {
            self.close_reports.push(report);
        }
    }

    /// Record the step the app's owner reported against the record it wrote.
    /// The host's node sweep cannot reach a live root History hides, and a
    /// transition must not carry the bracket over to the next record. The
    /// bracket is taken only under the record's loan, so a busy record leaves
    /// it with the owner.
    fn record_owner_step_to_current_record(&mut self) -> Result<(), String> {
        fn take(widget: &mut dyn Widget, out: &mut Vec<Vec<automerge::ChangeHash>>) {
            if let Some(before) = crate::undo::take_committed_action(widget) {
                out.push(before);
            }
            for child in widget.children_mut() {
                if child.legacy_callback_boundary().is_none() {
                    take(child, out);
                }
            }
        }
        let (Some(binding), TileContent::App(root)) = (&self.binding, &mut self.content) else {
            return Ok(());
        };
        binding
            .store
            .with_action_state(&binding.live, |doc, history, _| {
                let mut brackets = Vec::new();
                take(root.as_mut(), &mut brackets);
                let Some(before) = brackets
                    .into_iter()
                    .max_by_key(|before| doc.get_changes(before).len())
                else {
                    return;
                };
                history.record_committed_action_authored(
                    doc,
                    before,
                    None,
                    crate::undo::get_state(root.as_ref()),
                    Some(crate::collaboration::local_user_id()),
                );
            })
            .map_err(|error| error.to_string())
    }

    fn collect_retained_updates_before_release(&mut self) -> Result<(), String> {
        let invocations = self
            .retained_invocations
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for invocation in invocations {
            if self
                .retained_updates
                .get(&invocation)
                .is_some_and(|updates| !updates.is_empty())
            {
                return Err(
                    "A retained operation update must be delivered before switching documents"
                        .into(),
                );
            }
            let Some(owner) = self.retained_invocations.get(&invocation).cloned() else {
                continue;
            };
            owner
                .validate()
                .map_err(|error| format!("Outgoing document observation unavailable: {error}"))?;
            let (store, checkout) = owner.store_checkout();
            let TileContent::App(root) = &mut self.content else {
                continue;
            };
            let update = store
                .read_checked(&checkout.id, |document| {
                    crate::widget::poll_resource_operation(
                        root.as_mut(),
                        invocation,
                        false,
                        document,
                        &ROOT,
                    )
                })
                .map_err(|error| format!("Outgoing document observation unavailable: {error}"))?;
            if let Some(update) = update {
                let terminal = update.is_terminal();
                self.retained_updates
                    .entry(invocation)
                    .or_default()
                    .push_back(update);
                if terminal {
                    self.retained_invocations.remove(&invocation);
                } else {
                    return Err(
                        "A retained operation update must be delivered before switching documents"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    fn take_retained_update(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let update = self.retained_updates.get_mut(&invocation)?.pop_front();
        if self
            .retained_updates
            .get(&invocation)
            .is_some_and(VecDeque::is_empty)
        {
            self.retained_updates.remove(&invocation);
        }
        update
    }

    fn unavailable_update(
        invocation: tilo_actions::InvocationId,
    ) -> tilo_actions::InvocationUpdate {
        tilo_actions::InvocationUpdate::Failed {
            invocation,
            error: tilo_actions::ActionFailure {
                code: "owner_unavailable".into(),
                message: "The admitted operation's document owner is no longer available.".into(),
                details: None,
                retryable: false,
            },
        }
    }

    fn prepare_owner_transition(&mut self) -> Result<(), OwnerTransitionError> {
        if !self.owner_bound_work {
            self.record_transition_error = None;
            return Ok(());
        }
        if !self.history_lifetime.active.get() {
            let error = "The current document owner is closed".to_string();
            self.record_transition_error = Some(error.clone());
            return Err(OwnerTransitionError {
                message: error,
                retry_pending: false,
            });
        }
        if let Err(error) = self.collect_retained_updates_before_release() {
            self.record_transition_error = Some(error.clone());
            return Err(OwnerTransitionError {
                message: error,
                retry_pending: true,
            });
        }
        if let Err(error) = self.record_owner_step_to_current_record() {
            self.record_transition_error = Some(error.clone());
            return Err(OwnerTransitionError {
                message: error,
                retry_pending: false,
            });
        }
        let release = match &mut self.content {
            TileContent::App(root) => {
                crate::record::capability::release_record_scope(root.as_mut())
            }
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => Ok(()),
        };
        if let Err(error) = release {
            self.record_transition_error = Some(error.clone());
            return Err(OwnerTransitionError {
                message: error,
                retry_pending: false,
            });
        }
        self.settle_outgoing_record();
        for invocation in self
            .retained_invocations
            .drain()
            .map(|(invocation, _)| invocation)
        {
            let updates = self.retained_updates.entry(invocation).or_default();
            if !updates
                .iter()
                .any(tilo_actions::InvocationUpdate::is_terminal)
            {
                updates.push_back(Self::unavailable_update(invocation));
            }
        }
        self.owner_bound_work = false;
        self.owner_bound_custody = None;
        self.record_transition_error = None;
        Ok(())
    }

    fn current_binding_owner(&self) -> Option<crate::history_mode::HistoryOwner> {
        if let Some(binding) = self.callback_binding() {
            return Some(crate::history_mode::HistoryOwner::component(binding));
        }
        let binding = self.binding.as_ref()?;
        let checkout = binding.store.verified_checkout(&binding.live).ok()?;
        Some(crate::history_mode::HistoryOwner::legacy(
            binding.store.clone(),
            checkout,
            &self.history_lifetime,
        ))
    }

    fn owner_bound_custody_changed(&self) -> bool {
        self.owner_bound_work
            && self.owner_bound_custody.as_ref().is_some_and(|admitted| {
                self.current_binding_owner()
                    .is_none_or(|current| !current.same_owner(admitted))
            })
    }

    fn checked_old_owner_for_recovery(&self) -> Option<RecordId> {
        if self.record_transition_error.is_none()
            || self.record_error.is_some()
            || !self.history_lifetime.active.get()
        {
            return None;
        }
        let admitted = self.owner_bound_custody.as_ref()?;
        admitted.validate().ok()?;
        let current = self.current_binding_owner()?;
        current.validate().ok()?;
        if !current.same_owner(admitted) {
            return None;
        }
        let binding = self.binding.as_ref()?;
        binding.store.verified_checkout(&binding.live).ok()?;
        Some(binding.live.clone())
    }

    fn renew_history_lifetime(&mut self) {
        if self.automatic_history {
            self.history = None;
            self.automatic_history = false;
        }
        self.history_lifetime.control_domain.close();
        self.history_lifetime.active.set(false);
        let previous = self.history_lifetime.control_domain.clone();
        self.history_lifetime = crate::history_mode::owner::LegacyHistoryLifetime::new();
        self.custody_renewal = Some(self.history_lifetime.control_domain.renewal_from(&previous));
    }

    /// Whether `event` is addressed to this tile: keys and IME go to the
    /// focused tile, pointer events to the tile under the pointer.
    fn event_is_for_this_tile(&self, event: &WindowEvent, scale_factor: f64) -> bool {
        match event {
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => self.focused,
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. } => self.pointer.is_some_and(|p| self.rect.contains(p)),
            WindowEvent::Touch(touch) => self.rect.contains(Point::new(
                touch.location.x / scale_factor,
                touch.location.y / scale_factor,
            )),
            _ => false,
        }
    }

    /// Typing may reach a live app that has no tab record yet. It must not
    /// bypass a renewed lifetime or admitted owner-bound custody.
    fn typing_may_bypass_missing_record(&self) -> bool {
        !self.owner_bound_work && self.record_transition_error.is_none()
    }

    fn prepare_record_resolution(&mut self) -> Result<(), String> {
        if !self.history_lifetime.active.get() {
            let error = "The current document owner is closed".to_string();
            self.record_transition_error = Some(error.clone());
            return Err(error);
        }
        if self.owner_bound_custody_changed() {
            self.prepare_owner_transition()
                .map_err(|error| error.message)?;
        }
        Ok(())
    }

    fn bind_record_intent(&mut self, store: RecordStore, tab: RecordCheckout, create: bool) {
        // Every explicit request supersedes the older queued intent, even if
        // this attempt refuses. Observation backpressure may queue it anew.
        self.pending_binding = None;
        if self.record_creation_refused {
            return;
        }
        if !self.history_lifetime.active.get() {
            self.record_transition_error = Some("The current document owner is closed".into());
            self.record_intent_transition_blocked = false;
            return;
        }
        if let Some(b) = &self.binding
            && b.store.same_owner(&store)
            && b.tab == tab
            && store.open_existing_record_checked(&tab).is_ok()
        {
            self.pending_binding = None;
            self.record_transition_error = None;
            self.record_intent_transition_blocked = false;
            return;
        }
        if let Err(error) = admit_record(&store, &tab, create) {
            if self.binding.is_some() && self.owner_bound_work {
                self.pending_binding = Some(PendingBinding { store, tab, create });
                self.record_transition_error = Some(record_unavailable(&error));
                self.record_intent_transition_blocked = false;
                return;
            }
            // Preserve the existing unavailable-binding behavior, but never
            // expose the outer document while the requested owner is absent.
            self.settle_outgoing_record();
            self.announced = None;
            self.renew_history_lifetime();
            self.binding = None;
            self.pending_binding = Some(PendingBinding { store, tab, create });
            self.record_error = Some(record_unavailable(&error));
            return;
        }
        if self.binding.is_some()
            && let Err(error) = self.prepare_owner_transition()
        {
            if error.retry_pending {
                self.pending_binding = Some(PendingBinding { store, tab, create });
            }
            self.record_intent_transition_blocked = false;
            return;
        }
        // A spelling is not custody: a replaced store or checkout must be
        // admitted afresh, including failed same-ID kind changes.
        self.settle_outgoing_record();
        self.announced = None;
        self.renew_history_lifetime();
        // A new lifetime is a new admission: the retained bindings still
        // describe the old one, so the host must recompose before it can
        // route anything to this content again (as `content_mut` does).
        self.structure_dirty = true;
        if let Err(error) = admit_record(&store, &tab, create) {
            self.binding = None;
            self.pending_binding = Some(PendingBinding { store, tab, create });
            self.record_error = Some(record_unavailable(&error));
            return;
        }
        self.pending_binding = None;
        self.record_error = None;
        self.record_transition_error = None;
        self.record_intent_transition_blocked = false;
        let live = tab.id.clone();
        self.binding = Some(Binding {
            store,
            tab,
            live,
            established: false,
            settled_frontier: None,
        });
        self.announce_scope();
    }

    fn resolve_record(&mut self) -> Result<Option<RecordId>, String> {
        self.prepare_record_resolution()?;
        let previous_live = self.binding.as_ref().map(|binding| binding.live.clone());
        let was_unavailable = self.record_error.is_some();
        if let Some(pending) = self.pending_binding.take() {
            if let Err(error) = admit_record(&pending.store, &pending.tab, pending.create) {
                self.pending_binding = Some(pending);
                // The tile shows the storage context; the caller gets the
                // exact admission refusal.
                let shown = record_unavailable(&error);
                if self.binding.is_some() {
                    self.record_transition_error = Some(shown);
                } else {
                    self.record_error = Some(shown);
                }
                return Err(error.to_string());
            }
            if self.binding.is_some() {
                if let Err(error) = self.prepare_owner_transition() {
                    if error.retry_pending {
                        self.pending_binding = Some(pending);
                    }
                    self.record_intent_transition_blocked = false;
                    return Err(error.message);
                }
                self.announced = None;
                self.renew_history_lifetime();
                self.binding = Some(Binding {
                    store: pending.store,
                    live: pending.tab.id.clone(),
                    tab: pending.tab,
                    established: false,
                    settled_frontier: None,
                });
                self.record_error = None;
                self.record_transition_error = None;
                self.record_intent_transition_blocked = false;
            } else {
                self.binding = Some(Binding {
                    store: pending.store,
                    live: pending.tab.id.clone(),
                    tab: pending.tab,
                    established: false,
                    settled_frontier: None,
                });
            }
        }
        let want = match &self.content {
            TileContent::App(w) => crate::record::capability::record_intent(w.as_ref()),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        };
        let changing_owner = match (&self.binding, &want) {
            (Some(binding), Some(want)) if want.id != binding.live => {
                if let Err(error) = binding.store.open_existing_record_checked(want) {
                    let shown = record_unavailable(&error);
                    if self.owner_bound_work {
                        self.record_transition_error = Some(shown);
                        self.record_intent_transition_blocked = true;
                    } else {
                        // The outgoing owner's lifetime ends here even though
                        // the new intent is refused: settle its record first so
                        // a failed flush is reported and retried, not dropped.
                        self.settle_outgoing_record();
                        if !was_unavailable {
                            self.renew_history_lifetime();
                        }
                        self.record_error = Some(shown);
                    }
                    return Err(error.to_string());
                }
                true
            }
            _ => false,
        };
        if changing_owner {
            if let Err(error) = self.prepare_owner_transition() {
                self.record_intent_transition_blocked = true;
                return Err(error.message);
            }
            self.record_intent_transition_blocked = false;
        } else if self.record_intent_transition_blocked {
            self.record_transition_error = None;
            self.record_intent_transition_blocked = false;
        }
        let Some(binding) = self.binding.as_mut() else {
            return Ok(None);
        };
        let live = match binding.follow_checked(want, &mut self.close_reports) {
            Ok(live) => live,
            Err(error) => {
                if !was_unavailable {
                    self.renew_history_lifetime();
                }
                self.record_error = Some(record_unavailable(&error));
                return Err(error.to_string());
            }
        };
        if was_unavailable || previous_live.as_ref() != Some(&live) {
            self.renew_history_lifetime();
        }
        self.record_error = None;
        self.announce_scope();
        Ok(Some(live))
    }

    fn announce_scope(&mut self) {
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        let Some(live) = self.binding.as_ref().map(|b| b.live.clone()) else {
            return;
        };
        if self.announced.as_ref() == Some(&live) {
            return;
        }
        if let TileContent::App(w) = &mut self.content {
            crate::record::capability::set_record_scope(w.as_mut(), &live);
        }
        self.announced = Some(live);
    }
    pub fn kind_string(&self) -> String {
        match &self.content {
            TileContent::App(_) => "App".into(),
            TileContent::Stub { kind, .. } => kind.as_str().into(),
            TileContent::Notice { title, .. } => title.clone(),
            TileContent::Arriving => "Arriving".into(),
        }
    }
    /// The hosted app, to reach a capability or concrete type of it in
    /// place. Unlike replacing the content, this leaves the tile's record,
    /// History and layout alone: a change inside the app reports its own
    /// structure.
    pub fn app_mut(&mut self) -> Option<&mut (dyn AppWidget + 'static)> {
        match &mut self.content {
            TileContent::App(app) => Some(app.as_mut()),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }
    /// Mutable legacy content access. Canonical components use checked replacement.
    ///
    /// Tests only: input and per-frame paths reach the app through
    /// [`Self::app_mut`] and replace it through [`Self::replace_content`] or
    /// [`Self::replace_component`]. A window's edge drag once went through
    /// here and turned an ordinary resize into this panic.
    /// # Panics
    /// Refuses extraction of a canonical component through the legacy content API.
    #[cfg(test)]
    pub(crate) fn content_mut(&mut self) -> &mut TileContent {
        assert!(
            self.callback_binding().is_none(),
            "canonical content requires checked replacement"
        );
        self.settle_outgoing_record();
        self.renew_history_lifetime();
        self.structure_dirty = true;
        &mut self.content
    }
    /// Replace a component after its complete closed-owner cleanup. The caller
    /// retains returned reports until recovery is durably resolved.
    #[must_use]
    pub fn replace_component(
        &mut self,
        mut component: crate::application::AppComponent,
    ) -> Vec<crate::application::ComponentCloseReport> {
        let history = component.history.take();
        self.replace_content(TileContent::App(Box::new(
            crate::widget::app_widget::BoundAppWidget::new(component),
        )));
        if let Some(history) = history {
            self.install_history_registration(history);
        }
        crate::component_lifecycle::take_component_close_reports(self)
    }
    /// Used when a window announced before it existed (an opening card)
    /// becomes the app; replacing the tab instead would hand focus
    /// elsewhere. Keeps the record binding but resets `announced` so the
    /// new occupant is told its scope, and flags a structural rebuild
    /// since `App` and the card variants have different taffy shapes.
    pub fn replace_content(&mut self, content: TileContent) {
        self.record_creation_refused = false;
        if let TileContent::App(root) = &mut self.content {
            self.close_reports
                .extend(crate::component_lifecycle::close_components(root.as_mut()));
            self.close_reports
                .extend(crate::component_lifecycle::take_component_close_reports(
                    root.as_mut(),
                ));
        }
        self.settle_outgoing_record();
        self.renew_history_lifetime();
        // The slots were the outgoing app's; the new one declares its own.
        if let Some(window) = &self.hosted_window {
            window.declare(None);
        }
        self.retained_invocations.clear();
        self.component_invocations.clear();
        self.retained_updates.clear();
        self.owner_bound_work = false;
        self.owner_bound_custody = None;
        self.record_transition_error = None;
        self.record_intent_transition_blocked = false;
        self.content = content;
        if !self.uses_host_record() {
            self.clear_record_binding();
        }
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        let registration = match &self.content {
            TileContent::App(root) => {
                crate::history_mode::capability::history_registration(root.as_ref())
            }
            _ => Ok(None),
        };
        self.history = None;
        match registration {
            Ok(Some(registration)) => self.install_history_registration(registration),
            Err(error) => self.record_error = Some(format!("History unavailable: {error}")),
            Ok(None) => {}
        }
        self.install_declared_history();
        // An app with a History of its own learns where the window presents
        // its controls, as the content it replaces did: an app that replaced
        // its opening card drew its own History and search beside the bar's.
        if let Some(tab_presents) = self.window_history
            && let Some(app) = self.app_history_mut()
        {
            app.set_window_presents_history(tab_presents);
        }
        self.content_was_presented.set(false);
        self.opening_shown = false;
        self.handed_over_at = None;
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        self.announced = None;
        self.structure_dirty = true;
    }
    pub fn content(&self) -> &TileContent {
        &self.content
    }
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// A call mid-animation restarts the curve.
    pub fn start_spawn_animation(&mut self) {
        self.spawned_at = Some(crate::time::now());
    }

    pub fn spawn_in_flight(&self) -> bool {
        match self.spawned_at {
            Some(t) => crate::time::now().saturating_duration_since(t) < spawn_time(),
            None => false,
        }
    }

    /// Desktop-space card rect for an app-provided complete semantic canvas.
    /// The provider reports tile-content size; the shell card grows from the
    /// live tile's top-left and never shrinks below the live window surface.
    pub fn complete_content_rect(&self) -> Option<Rect> {
        if let Some(held) = self.held.as_ref().filter(|_| self.content_held()) {
            return self.held_complete_rect(held);
        }
        self.live_complete_content_rect()
    }

    fn live_complete_content_rect(&self) -> Option<Rect> {
        let TileContent::App(app) = &self.content else {
            return None;
        };
        let size = app.complete_content_size()?;
        if !size.width.is_finite()
            || !size.height.is_finite()
            || size.width <= 0.0
            || size.height <= 0.0
        {
            return None;
        }
        Some(Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + size.width.max(self.rect.width()),
            self.rect.y0 + size.height.max(self.rect.height()),
        ))
    }

    /// Paint a complete app-provided canvas in the ordinary window card. The
    /// app owns only its semantic pixels; shared clipping, corner radius and
    /// focus edge remain toolkit code, exactly as on screen.
    pub fn draw_complete_content_capture(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Option<Rect> {
        if self.content_held() {
            return self.draw_held_complete_content(scene, transform);
        }
        let rect = self.live_complete_content_rect()?;
        let radius = f64::from(theme.tokens.radii.tile);
        let shape = RoundedRect::from_rect(rect, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &shape,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );
        let painted = match &mut self.content {
            TileContent::App(app) => {
                app.draw_complete_content(scene, rect, transform, tr, theme, presence)
            }
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        };
        scene.pop_layer();
        if !painted {
            return None;
        }
        if !self.inside_growing_frame {
            self.draw_edge_at(scene, transform, theme, radius, rect);
        }
        Some(rect)
    }

    /// Exposed for tests to pin the curve shape without peeking into
    /// Vello's scene buffer.
    #[cfg(test)]
    pub fn spawn_alpha(&self) -> Option<f32> {
        self.spawned_at.and_then(|t| {
            let elapsed = crate::time::now().saturating_duration_since(t);
            (elapsed < spawn_time()).then(|| crate::motion::unit_opacity(spawn_progress(elapsed)))
        })
    }
}

impl Continuable for TileHost {
    fn continuation_metadata(
        &self,
        state: &AutoCommit,
        path: &ObjId,
    ) -> Option<crate::collaboration::ContinuationMetadata> {
        match &self.content {
            TileContent::App(app) => {
                crate::widget::capability::capability::<dyn Continuable>(&**app)
                    .and_then(|continuable| continuable.continuation_metadata(state, path))
            }
            _ => None,
        }
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &crate::collaboration::ContinuationMetadata,
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        match &mut self.content {
            TileContent::App(app) => {
                crate::widget::capability::capability_mut::<dyn Continuable>(&mut **app)
                    .is_some_and(|continuable| {
                        continuable.restore_continuation_metadata(metadata, state, path)
                    })
            }
            _ => false,
        }
    }

    fn restore_live_continuation_metadata(
        &mut self,
        metadata: &crate::collaboration::ContinuationMetadata,
        state: &AutoCommit,
        path: &ObjId,
    ) -> bool {
        match &mut self.content {
            TileContent::App(app) => {
                crate::widget::capability::capability_mut::<dyn Continuable>(&mut **app)
                    .is_some_and(|continuable| {
                        continuable.restore_live_continuation_metadata(metadata, state, path)
                    })
            }
            _ => false,
        }
    }
}

impl FloatingSurfaceProvider for TileHost {
    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
        if let TileContent::App(widget) = &mut self.content {
            widget.on_floating_surface_focus_changed(id, focused);
        }
    }

    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
        if let TileContent::App(widget) = &mut self.content {
            widget.on_floating_surface_ime_source_changed(id);
        }
    }

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        if let TileContent::App(widget) = &mut self.content {
            widget.on_floating_surface_event_source(id);
        }
    }

    fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode> {
        match &self.content {
            TileContent::App(widget) => widget.floating_surface_accessibility(id),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        match &mut self.content {
            TileContent::App(widget) => widget
                .perform_floating_surface_accessibility_action(id, target, action, state, path),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        }
    }

    fn floating_surface_ime(&self, id: &str) -> Option<(crate::ime::InputContent, Rect)> {
        match &self.content {
            TileContent::App(widget) => widget.floating_surface_ime(id),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_floating_surface_request(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    fn take_capture_studio_request(&mut self) -> Option<super::CaptureMode> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_capture_studio_request(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_floating_surface_source_updates(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => {
                Vec::new()
            }
        }
    }

    fn draw_floating_surfaces(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let TileContent::App(widget) = &mut self.content {
            widget.draw_floating_surfaces(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_floating_surface(
        &mut self,
        id: &str,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> bool {
        match &mut self.content {
            TileContent::App(widget) => {
                widget.draw_floating_surface(id, scene, transform, text_renderer, theme, presence)
            }
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        }
    }

    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        if let TileContent::App(widget) = &mut self.content {
            widget.set_floating_surface_bounds(bounds);
        }
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
        match &self.content {
            TileContent::App(widget) => widget.floating_surface_presentations(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => {
                Vec::new()
            }
        }
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        match &self.content {
            TileContent::App(widget) => widget.active_floating_surface_ids(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => {
                Vec::new()
            }
        }
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
        if let TileContent::App(widget) = &mut self.content {
            widget.on_floating_surface_host_message(message);
        }
    }
}

impl AudiencePresentationProvider for TileHost {
    fn audience_presentation_request(
        &self,
    ) -> Option<crate::fullscreen::AudiencePresentationRequest> {
        match &self.content {
            TileContent::App(app) => app.audience_presentation_request(),
            _ => None,
        }
    }

    fn on_audience_presentation_changed(&mut self, active: bool) {
        if let TileContent::App(app) = &mut self.content {
            app.on_audience_presentation_changed(active);
        }
    }

    fn draw_audience_presentation(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) -> bool {
        match &mut self.content {
            TileContent::App(app) => {
                app.draw_audience_presentation(scene, transform, text_renderer, theme, rect)
            }
            _ => false,
        }
    }

    fn handle_audience_presentation_event(
        &mut self,
        event: &WindowEvent,
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        match &mut self.content {
            TileContent::App(app) => app.handle_audience_presentation_event(event, state, path),
            _ => false,
        }
    }

    fn on_audience_presentation_closed(&mut self) {
        if let TileContent::App(app) = &mut self.content {
            app.on_audience_presentation_closed();
        }
    }
}

impl FullscreenProvider for TileHost {
    fn fullscreen_intent(&self) -> Option<bool> {
        match &self.content {
            TileContent::App(app) => app.fullscreen_intent(),
            _ => None,
        }
    }

    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
        match &self.content {
            TileContent::App(app) => app.fullscreen_params(),
            _ => crate::fullscreen::FullscreenRequest::mirror(
                crate::fullscreen::FullscreenTarget::Largest,
            ),
        }
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        if let TileContent::App(app) = &mut self.content {
            app.on_fullscreen_changed(active);
        }
    }
}

impl Widget for TileHost {
    crate::provides!(
        dyn crate::widget::FullscreenProvider,
        dyn crate::widget::Continuable,
        dyn crate::widget::FloatingSurfaceProvider,
        dyn crate::widget::AudiencePresentationProvider,
        dyn crate::collaboration::participants::CollaborationTopology,
        dyn crate::history_mode::capability::FrameworkHistoryTopology,
        dyn crate::widget::controls::ActionControls,
        dyn crate::search::capability::SearchSource,
        dyn crate::presence::capability::PublishesPresence,
        dyn crate::view::capability::ViewState,
        dyn crate::component_lifecycle::ComponentLifecycle,
        dyn crate::widget::ResourceOperations
    );

    /// Lets a host that lays several tiles out in one tree find the window
    /// around a widget (the palette centres on the focused one).
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn primary_vertical_scroll(&self) -> Option<crate::widget::PrimaryScrollRegion> {
        if self
            .history
            .as_ref()
            .is_some_and(|history| !history.live_interactive())
        {
            return None;
        }
        if let TileContent::App(app) = &self.content {
            app.primary_vertical_scroll()
        } else {
            None
        }
    }
    fn apply_primary_vertical_scroll(
        &mut self,
        action: crate::widget::PrimaryScrollAction,
    ) -> crate::widget::PrimaryScrollResult {
        if self
            .history
            .as_ref()
            .is_some_and(|history| !history.live_interactive())
        {
            return crate::widget::PrimaryScrollResult::Unsupported;
        }
        if let TileContent::App(app) = &mut self.content {
            app.apply_primary_vertical_scroll(action)
        } else {
            crate::widget::PrimaryScrollResult::Unsupported
        }
    }

    fn resets_callback_binding(&self) -> bool {
        self.history.is_some()
    }
    fn inactive_callback_boundary(&self) -> Option<NodeId> {
        self.history
            .as_ref()
            .filter(|history| !history.live_interactive())
            .and_then(|history| history.live_layout_node())
    }
    fn legacy_callback_boundary(&self) -> Option<(NodeId, crate::history_mode::HistoryOwner)> {
        self.binding.as_ref()?;
        Some((self.live_callback_node?, self.history_owner()?))
    }
    #[allow(
        private_interfaces,
        reason = "toolkit-only component request preserves hidden live lifecycle without exposing root/registry loans"
    )]
    fn visit_components(
        &mut self,
        request: &mut crate::bound_dispatch::ComponentRequest<'_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        if let TileContent::App(live) = &mut self.content {
            live.visit_components(request)
        } else {
            Ok(())
        }
    }

    #[allow(
        private_interfaces,
        reason = "finite toolkit resource ownership requests"
    )]
    fn visit_resource_owners(
        &self,
        request: &mut crate::bound_dispatch::legacy_document::ResourceReadRequest<'_>,
    ) -> Result<(), tilo_actions::DoorError> {
        if let TileContent::App(live) = &self.content {
            request.tile(
                self.history_owner(),
                self.has_separate_document_owner(),
                self.history.as_ref().is_none_or(|h| h.live_interactive()),
                live.as_ref(),
            )
        } else {
            Ok(())
        }
    }
    #[allow(
        private_interfaces,
        reason = "finite toolkit resource ownership requests"
    )]
    fn visit_resource_owners_mut(
        &mut self,
        request: &mut crate::bound_dispatch::legacy_document::ResourceRequest<'_>,
    ) -> Result<(), tilo_actions::DoorError> {
        let owner = self.history_owner();
        let separate = self.has_separate_document_owner();
        let interactive = self.history.as_ref().is_none_or(|h| h.live_interactive());
        if request.effect_target(owner.as_ref()) {
            request.deliver_effect(self, interactive);
            return Ok(());
        }
        let history_doors = self
            .history
            .as_ref()
            .map(|history| history.door_addresses())
            .unwrap_or_default();
        if let TileContent::App(live) = &mut self.content {
            request.tile(
                owner.as_ref(),
                separate,
                interactive,
                &history_doors,
                live.as_mut(),
            )
        } else {
            Ok(())
        }
    }

    fn mount_native_views(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>) {
        let _running = self.running();
        if let TileContent::App(live) = &mut self.content {
            if self
                .history
                .as_ref()
                .is_none_or(|history| history.live_interactive())
            {
                live.mount_native_views(host);
            } else {
                live.unmount_native_views();
            }
        }
    }
    fn unmount_native_views(&mut self) {
        if let TileContent::App(live) = &mut self.content {
            live.unmount_native_views();
        }
    }
    fn native_view_frame_start(&mut self) {
        let _running = self.running();
        if self
            .history
            .as_ref()
            .is_none_or(|history| history.live_interactive())
            && let TileContent::App(live) = &mut self.content
        {
            live.native_view_frame_start();
        }
    }
    fn on_external_event(
        &mut self,
        payload: &str,
        state: &mut crate::widget::Edits<'_>,
        root: &ModelPath,
    ) {
        let _running = self.running();
        if let TileContent::App(app) = &mut self.content {
            app.on_external_event(payload, state, root);
        }
    }
    fn take_focus_reset(&mut self) -> bool {
        match &mut self.content {
            TileContent::App(app) => app.take_focus_reset(),
            _ => false,
        }
    }
    /// The focused window's tile is where Tab travels: the app's controls,
    /// never the window's bar or the desktop around it.
    fn scopes_keyboard_focus(&self) -> bool {
        self.focused
    }
    /// An app does not bring itself forward; only its desktop does.
    fn take_app_activation(&mut self) -> Option<crate::application::InstanceId> {
        None
    }
    fn needs_idle_sync(&self) -> bool {
        let _running = self.running();
        match &self.content {
            TileContent::App(app) => app.needs_idle_sync(),
            _ => false,
        }
    }

    fn history_owner(&self) -> Option<crate::history_mode::HistoryOwner> {
        if self.record_error.is_some() || !self.history_lifetime.active.get() {
            return None;
        }
        if let Some(binding) = self.callback_binding() {
            return Some(crate::history_mode::HistoryOwner::component(binding));
        }
        let binding = self.binding.as_ref()?;
        let checkout = binding.store.verified_checkout(&binding.live).ok()?;
        Some(crate::history_mode::HistoryOwner::legacy(
            binding.store.clone(),
            checkout,
            &self.history_lifetime,
        ))
    }

    fn has_separate_document_owner(&self) -> bool {
        self.binding.is_some()
            || self.pending_binding.is_some()
            || self.callback_binding().is_some()
    }

    fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
        match &self.content {
            TileContent::App(widget) => widget.callback_binding(),
            _ => None,
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if self.record_error.is_some() {
            return vec![];
        }
        let mut children = vec![];
        if let Some(history) = &self.history {
            children.push(history.as_ref() as &dyn Widget);
        }
        if self
            .history
            .as_ref()
            .is_none_or(|history| history.live_interactive())
            && let TileContent::App(widget) = &self.content
        {
            children.push(widget.as_ref());
        }
        children
    }
    /// The app also while History keeps it inert.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        if let Some(history) = &self.history {
            visitor(history.as_ref());
        }
        if let TileContent::App(widget) = &self.content {
            visitor(widget.as_ref());
        }
        true
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.record_error.is_some() {
            return vec![];
        }
        let interactive = self
            .history
            .as_ref()
            .is_none_or(|history| history.live_interactive());
        let mut children: Vec<&mut dyn Widget> = vec![];
        if let Some(history) = &mut self.history {
            children.push(history.as_mut());
        }
        if interactive && let TileContent::App(widget) = &mut self.content {
            children.push(widget.as_mut());
        }
        children
    }

    /// The tile's own node fills the window whatever it shows, so a notice
    /// or a stub has nothing to measure once laid out; app content and
    /// History report their own.
    fn is_layout_dirty(&self) -> bool {
        if self.record_error.is_some() {
            return true;
        }
        let mut has_children = false;
        let mut dirty = false;
        self.for_each_child(&mut |child| {
            has_children = true;
            dirty = dirty || child.is_layout_dirty();
        });
        if has_children {
            dirty
        } else {
            self.rect == Rect::ZERO
        }
    }

    fn sync(&mut self, s: &AppModel, p: &ModelPath) {
        let _running = self.running();
        if !self.history_lifetime.active.get()
            || crate::history_mode::frame::FrameworkInputFrame::active()
        {
            return;
        }
        // A component that asked to show another record in its place gets
        // the tile: its record and history follow.
        if let TileContent::App(widget) = &mut self.content
            && let Some(replacement) = crate::component_lifecycle::take_replacement(widget.as_mut())
        {
            let (component, outcome) = replacement.into_parts();
            outcome.answer(Ok(()));
            // The outgoing component answers the operations it admitted (the
            // open that asked) before it closes; the answers outlive it until
            // the host polls them, and work it cannot finish is reported
            // unavailable rather than inherited by the new component.
            let _ = self.collect_retained_updates_before_release();
            let mut answers = std::mem::take(&mut self.retained_updates);
            let (doc, path) = crate::doc::document_ref(s, p);
            for invocation in std::mem::take(&mut self.component_invocations) {
                if let TileContent::App(app) = &mut self.content
                    && let Some(update) = crate::widget::poll_resource_operation(
                        app.as_mut(),
                        invocation,
                        false,
                        doc,
                        path,
                    )
                {
                    answers.entry(invocation).or_default().push_back(update);
                }
                let updates = answers.entry(invocation).or_default();
                if !updates
                    .iter()
                    .any(tilo_actions::InvocationUpdate::is_terminal)
                {
                    updates.push_back(Self::unavailable_update(invocation));
                }
            }
            for invocation in self.retained_invocations.keys().copied() {
                let updates = answers.entry(invocation).or_default();
                if !updates
                    .iter()
                    .any(tilo_actions::InvocationUpdate::is_terminal)
                {
                    updates.push_back(Self::unavailable_update(invocation));
                }
            }
            let reports = self.replace_component(component);
            self.close_reports.extend(reports);
            self.retained_updates = answers;
        }
        let Ok(live) = self.resolve_record() else {
            return;
        };
        self.install_declared_history();
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        if self.record_error.is_some() {
            return;
        }
        let TileContent::App(w) = &mut self.content else {
            return;
        };
        match (live, &mut self.binding) {
            (Some(id), Some(b)) => {
                b.store.with(&id, |doc| w.sync(doc, &ROOT));
                if !b.established {
                    b.store
                        .with_history(&id, |doc, history| history.rebase(doc));
                    b.established = true;
                }
            }
            _ => w.sync(s, p),
        }
    }

    fn settle(&mut self, edits: &mut crate::widget::Edits<'_>, path: &ObjId) {
        let _running = self.running();
        if !self.history_lifetime.active.get()
            || crate::history_mode::frame::FrameworkInputFrame::active()
            || self.record_error.is_some()
        {
            return;
        }
        let live = self.binding.as_ref().map(|binding| binding.live.clone());
        match (
            live,
            self.binding.as_ref().map(|binding| binding.store.clone()),
        ) {
            (Some(id), Some(store)) => {
                let _ = store.with(&id, |doc| {
                    if let TileContent::App(app) = &mut self.content {
                        let before = crate::doc::settle_mark(doc);
                        app.settle(&mut crate::widget::Edits::lend(doc), &ROOT);
                        // What it wrote shows in this same frame.
                        if crate::doc::settle_mark(doc) != before {
                            app.update(doc, &ROOT);
                            app.sync(doc, &ROOT);
                        }
                    }
                });
                let _ = self.record_owner_step_to_current_record();
            }
            _ => {
                if let TileContent::App(app) = &mut self.content {
                    app.settle(edits, path);
                }
            }
        }
    }

    fn update(&mut self, s: &AppModel, p: &ModelPath) -> bool {
        let _running = self.running();
        // This custody is independent of the outer App State and remains
        // durable while History hides the live widget.
        if let Some(binding) = &self.binding {
            binding.store.schedule_committed_history(&binding.live);
            if let TileContent::App(app) = &mut self.content
                && app.take_save_retry()
                && let Err(error) = binding.store.try_flush_record(&binding.live)
            {
                tracing::warn!(%error, "retrying a failed record save");
            }
            if self.history_lifetime.active.get()
                && let (Ok(status), TileContent::App(app)) =
                    (binding.store.save_status(&binding.live), &mut self.content)
            {
                app.record_save_status(status);
            }
        }
        if !self.history_lifetime.active.get()
            || crate::history_mode::frame::FrameworkInputFrame::active()
        {
            return false;
        }
        let Ok(live) = self.resolve_record() else {
            return false;
        };
        self.install_declared_history();
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        if self.record_error.is_some() {
            return false;
        }
        let (store, id) = if let (Some(id), Some(binding)) = (live, self.binding.as_ref()) {
            (binding.store.clone(), id)
        } else {
            let TileContent::App(w) = &mut self.content else {
                return false;
            };
            return w.update(s, p);
        };
        let changed = store
            .with(&id, |doc| {
                let TileContent::App(w) = &mut self.content else {
                    return false;
                };
                w.update(doc, &ROOT)
            })
            .unwrap_or(false);
        let _ = self.record_owner_step_to_current_record();
        changed
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let _running = self.running();
        self.live_callback_node = None;
        let node = match &mut self.content {
            TileContent::App(w) => {
                let child = if let Some(history) = &mut self.history {
                    history.build_composition(w.as_mut(), taffy, theme, node_map)
                } else {
                    w.build_layout(taffy, theme, node_map)
                };
                self.live_callback_node = self
                    .history
                    .as_ref()
                    .map_or(Some(child), |history| history.live_layout_node());
                taffy.parent_node(
                    Style {
                        size: Size {
                            width: Dimension::percent(1.0),
                            height: Dimension::percent(1.0),
                        },
                        // The window gives its app its room; what the app
                        // shows never sets a floor under it. With the
                        // automatic minimum, a page whose document was taller
                        // than the window kept the tile from making way for
                        // the window's bar, and pushed the window past its
                        // strip.
                        min_size: Size {
                            width: Dimension::length(0.0),
                            height: Dimension::length(0.0),
                        },
                        ..Default::default()
                    },
                    &[child],
                )
            }
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => taffy
                .leaf_node(Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                }),
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        // This build composed the current content and admission, so a change
        // flagged before it (a rebind, a replacement) is satisfied. A host
        // that inspects the flag only after building must not discard the
        // maps it just made; a sync that replaces delegates after this build
        // sets the flag afresh.
        self.structure_dirty = false;
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let _running = self.running();
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let TileContent::App(w) = &mut self.content {
            // Guards a structural mismatch: content is `App` but the taffy
            // node has no child yet (build_layout ran while content was
            // still `Stub`, or structure_dirty hasn't fired).
            let children = taffy.children(node).unwrap_or_default();
            if let Some(&child_node) = children.first() {
                // The tile is its app's window: what the app centres on its
                // window (a dialog) centres on the tile, not on the screen
                // the strip's shared tree is rooted at.
                let rect = self.rect;
                let origin = Point::new(rect.x0, rect.y0);
                crate::widget::modal_dialog::within_window(node, rect, || {
                    if let Some(history) = &mut self.history {
                        history.apply_composition(w.as_mut(), taffy, child_node, origin);
                        self.structure_dirty |= history.width_needs_refresh(rect.width());
                    } else {
                        w.apply_layout(taffy, child_node, origin);
                    }
                });
            } else {
                tracing::warn!(
                    "TileHost::apply_layout: App content with no taffy child; \
                     skipping (next frame's structure rebuild should resolve)"
                );
            }
        }
    }
    /// The app's top layer, over its own window: the tile is the viewport
    /// its dialogs centre on, its menus and tooltips stay inside, as a
    /// standalone app's window is. Without this the top layer would see the
    /// whole screen the strip is drawn on and a dialog would centre there,
    /// spilling past a window narrower than the screen.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let outer = (self.rect.width() > 0.0 && self.rect.height() > 0.0)
            .then(|| presence.push_viewport(self.rect));
        crate::widget::defaults::draw_overlay(
            self.children_mut(),
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
        if let Some(outer) = outer {
            presence.pop_viewport(outer);
        }
    }
    fn set_full_bleed(&mut self, full_bleed: bool) {
        self.full_bleed = full_bleed;
        if let TileContent::App(app) = &mut self.content {
            app.set_full_bleed(full_bleed);
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let _running = self.running();
        // Clear an expired spawn timestamp before branching, or it stays
        // `Some` forever after the curve ends.
        if let Some(t) = self.spawned_at
            && crate::time::now().saturating_duration_since(t) >= spawn_time()
        {
            self.spawned_at = None;
        }
        if theme.reduce_motion {
            self.spawned_at = None;
        }
        // Wrap the steady-state paint in one alpha layer + centre-pivot
        // scale so the whole tile fades/scales as one unit.
        if self.spawn_in_flight()
            && let Some(started) = self.spawned_at
        {
            let progress =
                spawn_progress(crate::motion::frame_now().saturating_duration_since(started));
            let pose = WindowMotion::content_pose(progress, self.rect, theme.motion());
            let alpha = pose.opacity;
            // The window edge sits just outside the card; a few pixels of
            // clip keep it whole through the scale.
            let clip = Rect::new(
                self.rect.x0 - 4.0,
                self.rect.y0 - 4.0,
                self.rect.x1 + 4.0,
                self.rect.y1 + 4.0,
            );
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                alpha,
                transform,
                &clip,
            );
            let anim_transform = pose.applied_to(transform);
            self.draw_window(scene, anim_transform, tr, theme, presence);
            scene.pop_layer();
            crate::widget::draw_gate_and_peer_focus(self, scene, transform, theme, presence);
            return;
        }
        self.draw_window(scene, transform, tr, theme, presence);
        crate::widget::draw_gate_and_peer_focus(self, scene, transform, theme, presence);
    }
    fn needs_redraw(&self) -> bool {
        if let Some(redraw) = self.held_redraw() {
            return redraw;
        }
        super::defaults::needs_redraw(&|visit| self.for_each_child(visit))
    }
    fn mark_drawn(&mut self) {
        self.drop_ended_hold();
        super::defaults::mark_drawn(self.children_mut());
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let _running = self.running();
        if *ctx.handled {
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }
        // The card of a record that cannot be opened opens its "Details".
        if self.record_error.is_some()
            && let Some((_, card)) = &mut self.error_card
        {
            card.handle_event(event, ctx);
            if *ctx.handled {
                return;
            }
        }
        let typing = matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        );
        let live = if let Ok(live) = self.resolve_record() {
            live
        } else if let Some(live) = self.checked_old_owner_for_recovery() {
            Some(live)
        } else if typing && self.typing_may_bypass_missing_record() {
            // A running Terminal still receives keystrokes when the tab
            // record was never bound. A renewed or closed owner must not.
            None
        } else {
            // Refuse what is meant for this tile; leave the rest for its
            // neighbours (a loading window must not freeze the ones beside it).
            *ctx.handled |= self.event_is_for_this_tile(event, ctx.scale_factor);
            return;
        };
        self.install_declared_history();
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        if self.record_error.is_some() && !(typing && self.typing_may_bypass_missing_record()) {
            *ctx.handled |= self.event_is_for_this_tile(event, ctx.scale_factor);
            return;
        }
        let TileContent::App(app) = &mut self.content else {
            return;
        };
        if let Some(history) = &mut self.history {
            history.observe_pointer(event, ctx.scale_factor);
            if history.controls_event(event, ctx.scale_factor) {
                history.route_event(app.as_mut(), event, ctx);
                let leftover_typing = matches!(
                    event,
                    WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
                ) && !*ctx.handled
                    && !history.captures_input();
                if !leftover_typing {
                    return;
                }
            } else if !history.live_interactive() {
                return;
            }
        }
        // Same structural-mismatch guard as apply_layout above.
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let Some(child_node) = self
            .live_callback_node
            .or_else(|| children.first().copied())
        else {
            return;
        };
        let (Some(id), Some(bind)) = (live, &self.binding) else {
            app.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
            return;
        };
        // An `UndoEntry` is a pair of change hashes inside *this* record's
        // change graph, so running it against another tab's document
        // either does nothing or reverts the wrong thing.
        if app.is_focused()
            && let Some(step) = undo_step(event, *ctx.modifiers)
        {
            bind.store.with_history(&id, |doc, history| {
                let author = crate::collaboration::local_user_id();
                // Close any open burst first, so Ctrl+Z right after typing
                // reverts what was just typed, not the action before it —
                // free when the burst is already closed.
                if matches!(step, UndoStep::Undo) {
                    history.commit_action_authored(
                        doc,
                        None,
                        crate::undo::get_state(app.as_ref()),
                        Some(author.clone()),
                    );
                }
                match step {
                    UndoStep::Undo => history.undo_as(
                        doc,
                        Some(&author),
                        None,
                        crate::undo::get_state(app.as_ref()),
                        None,
                    ),
                    UndoStep::Redo => history.redo_as(
                        doc,
                        Some(&author),
                        None,
                        crate::undo::get_state(app.as_ref()),
                        None,
                    ),
                }
                .map(|entry| {
                    if let Some(meta) = entry.widget_metadata.as_deref() {
                        crate::undo::set_state(app.as_mut(), meta);
                    }
                })
            });
            *ctx.handled = true;
            return;
        }
        bind.store.with(&id, |doc| {
            app.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: ctx.modifiers,
                    state: crate::widget::Edits::lend(doc),
                    path: &ROOT,
                    handled: ctx.handled,
                    focus_target: ctx.focus_target,
                    node_id: child_node,
                    taffy: ctx.taffy,
                },
            );
        });
    }
    fn name(&self) -> &'static str {
        "TileHost"
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let _running = self.running();
        if let Some(detail) = &self.record_error {
            let card = record_unavailable_card(detail);
            let node = card
                .accessibility()
                .unwrap_or_else(|| crate::a11y::AccessNode::new(crate::a11y::Role::Group));
            return Some(node.with_bounds(self.rect));
        }
        if let Some(detail) = &self.record_transition_error {
            return Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                    .with_name("Document switch blocked")
                    .with_description(detail.clone())
                    .with_live(crate::a11y::Politeness::Assertive)
                    .with_bounds(self.rect),
            );
        }
        match &self.content {
            TileContent::Notice { title, detail } => Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                    .with_name(title.clone())
                    .with_description(detail.clone())
                    .with_bounds(self.rect),
            ),
            TileContent::App(_) | TileContent::Stub { .. } | TileContent::Arriving => None,
        }
    }
    /// Only an app can answer anything but `true` — a stub/notice is
    /// painted by this process and is on screen the moment the tile is.
    fn presented(&self) -> bool {
        if !self.content_was_presented.get() {
            let now = match &self.content {
                TileContent::App(w) => w.presented(),
                TileContent::Stub { .. } | TileContent::Notice { .. } => true,
                TileContent::Arriving => false,
            };
            self.content_was_presented.set(now);
            self.was_presented.set(self.was_presented.get() || now);
        }
        self.was_presented.get()
    }
    /// Current content keeps its readiness through resize, but a replacement
    /// gets its own opening presentation. A visible tile shows that card
    /// immediately rather than disappearing while the new app starts.
    fn opening(&self) -> bool {
        // Poll even outside WindowGroup's presentation pass, so a newly
        // ready occupant can retire its card when only redraw is queried.
        let _ = self.presented();
        if self.content_was_presented.get() {
            return false;
        }
        match &self.content {
            TileContent::App(w) => self.was_presented.get() || w.opening(),
            TileContent::Arriving => true,
            TileContent::Stub { .. } | TileContent::Notice { .. } => false,
        }
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn wants_pointer_capture(&self) -> bool {
        match &self.content {
            TileContent::App(widget) => widget.wants_pointer_capture(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        }
    }
    fn take_structure_dirty(&mut self) -> bool {
        // Both flags drained, not short-circuited, so neither can be left
        // set behind the other.
        let swapped = std::mem::take(&mut self.structure_dirty);
        let inner = match &mut self.content {
            TileContent::App(w) => w.take_structure_dirty(),
            _ => false,
        };
        swapped || inner
    }
    fn needs_animation_frame(&self) -> bool {
        let _running = self.running();
        // `TileHost` has no `children()`, so the default recursive
        // implementation would always say false; override so the spawn
        // fade and a hosted app's own signal both bubble up. The opening
        // card itself is state, not an animation: its spinner reports its own
        // motion through `inner`, and under reduced motion it is deliberately
        // static. Treating `opening()` as perpetual motion pinned the whole
        // compositor at display cadence until a slow app's first frame.
        let inner = match &self.content {
            TileContent::App(w) => w.needs_animation_frame(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        };
        let opening_indicator =
            Widget::opening(self) && Widget::needs_animation_frame(&self.opening_spinner);
        // The card is painted, then the app presents. If we sleep there the
        // last compositor frame is still the empty card — A05's Store shot.
        let pending_handover =
            self.opening_shown && !Widget::opening(self) && self.handed_over_at.is_none();
        self.spawn_in_flight()
            || self.handover_in_flight()
            || opening_indicator
            || inner
            || pending_handover
    }
}

impl crate::widget::ResourceOperations for TileHost {
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let _running = self.running();
        let (_, scope) = op.with_custody();
        op.check()?;
        if !matches!(&self.content, TileContent::App(_)) {
            return Ok(None);
        }
        if !self.history_lifetime.active.get() {
            return Err("The current document owner is closed".into());
        }
        // Resolve a requested legacy binding before exposing any document.
        // Failure must not advance/announce its scope or fall back to the host.
        let live = self.resolve_record()?;
        if scope.is_none() {
            self.announce_scope();
        }
        let owner = self.history_owner();
        let TileContent::App(app) = &mut self.content else {
            return Ok(None);
        };
        let mut start = |document: &mut AutoCommit, root: &ObjId| match scope {
            Some(scope) => scope.with_document_mutation(document, |document| {
                crate::widget::start_operation(
                    app.as_mut(),
                    op,
                    &mut crate::widget::Edits::lend(document),
                    root,
                )
            }),
            None => crate::widget::start_operation(
                app.as_mut(),
                op,
                &mut crate::widget::Edits::lend(document),
                root,
            ),
        };
        let started = match live.zip(self.binding.as_ref()) {
            Some((live, binding)) => binding
                .store
                .with_checked(&live, |document| start(document, &ROOT))
                .map_err(|error| error.to_string())??,
            // Unbound local work is polled straight from the app rather than
            // through retained owner custody.
            None if scope.is_none() => {
                let started = start(doc.document_mut(), path)?;
                if matches!(
                    started,
                    Some(crate::widget::ResourceOperationStart::Accepted)
                ) && self.callback_binding().is_some()
                {
                    self.component_invocations.insert(op.invocation());
                }
                return Ok(started);
            }
            None => start(doc.document_mut(), path)?,
        };
        if matches!(
            started,
            Some(crate::widget::ResourceOperationStart::Accepted)
        ) && let Some(owner) = owner
        {
            self.retained_invocations
                .insert(op.invocation(), owner.clone());
            self.owner_bound_work = true;
            self.owner_bound_custody.get_or_insert(owner);
        }
        Ok(started)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let _running = self.running();
        if let Some(update) = self.take_retained_update(invocation) {
            return Some(update);
        }
        if self.component_invocations.contains(&invocation) {
            let TileContent::App(app) = &mut self.content else {
                return None;
            };
            let update = crate::widget::poll_resource_operation(
                app.as_mut(),
                invocation,
                cancellation_requested,
                doc,
                path,
            );
            if update
                .as_ref()
                .is_some_and(tilo_actions::InvocationUpdate::is_terminal)
            {
                self.component_invocations.remove(&invocation);
            }
            return update;
        }
        // Admitted work belongs to the retained owner even while its local
        // presentation is hidden. A replacement must never inherit the id.
        if let Some(owner) = self.retained_invocations.get(&invocation).cloned() {
            if owner.validate().is_err()
                || self
                    .history_owner()
                    .is_none_or(|current| !current.same_owner(&owner))
            {
                self.retained_invocations.remove(&invocation);
                return Some(Self::unavailable_update(invocation));
            }
            let (store, checkout) = owner.store_checkout();
            let TileContent::App(app) = &mut self.content else {
                return None;
            };
            let update = store
                .read_checked(&checkout.id, |document| {
                    crate::widget::poll_resource_operation(
                        app.as_mut(),
                        invocation,
                        cancellation_requested,
                        document,
                        &ROOT,
                    )
                })
                .ok()
                .flatten();
            if update
                .as_ref()
                .is_some_and(tilo_actions::InvocationUpdate::is_terminal)
            {
                self.retained_invocations.remove(&invocation);
            }
            return update;
        }
        if self.binding.is_some() || self.pending_binding.is_some() {
            return None;
        }
        match &mut self.content {
            TileContent::App(app) => crate::widget::poll_resource_operation(
                app.as_mut(),
                invocation,
                cancellation_requested,
                doc,
                path,
            ),
            _ => None,
        }
    }
}

impl crate::component_lifecycle::ComponentLifecycle for TileHost {
    fn unsaved_documents(&self, doc: &AutoCommit) -> Vec<crate::close_admission::UnsavedDocument> {
        match &self.content {
            TileContent::App(root) => {
                if let Some(binding) = &self.binding {
                    binding
                        .store
                        .read(&binding.live, |document| {
                            crate::component_lifecycle::unsaved_documents(root.as_ref(), document)
                        })
                        .unwrap_or_default()
                } else {
                    crate::component_lifecycle::unsaved_documents(root.as_ref(), doc)
                }
            }
            _ => Vec::new(),
        }
    }
    fn checkpoint_components(&mut self) -> Vec<String> {
        let mut errors = crate::close_admission::retry_reports(&mut self.close_reports);
        errors.extend(self.record_error.iter().cloned());
        if let Some(binding) = &self.binding {
            if let Err(error) = binding.store.try_flush() {
                errors.push(error);
            }
        }
        if let TileContent::App(root) = &mut self.content {
            errors.extend(crate::component_lifecycle::checkpoint_components(
                root.as_mut(),
            ));
        }
        errors
    }
    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        if let TileContent::App(root) = &mut self.content {
            self.close_reports
                .extend(crate::component_lifecycle::finish_admitted_close(
                    root.as_mut(),
                ));
        }
        self.take_component_close_reports()
    }
    fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.history_lifetime.control_domain.close();
        if self.history_lifetime.active.replace(false) {
            self.settle_outgoing_record();
        }
        if let TileContent::App(root) = &mut self.content {
            self.close_reports
                .extend(crate::component_lifecycle::close_components(root.as_mut()));
        }
        self.take_component_close_reports()
    }
    fn take_component_close_reports(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        let mut reports = std::mem::take(&mut self.close_reports);
        if let TileContent::App(root) = &mut self.content {
            reports.extend(crate::component_lifecycle::take_component_close_reports(
                root.as_mut(),
            ));
        }
        reports
    }
}

impl crate::view::capability::ViewState for TileHost {
    fn view_state(&self) -> Option<crate::view::ViewContribution> {
        match &self.content {
            TileContent::App(app) => crate::view::capability::view_state(app.as_ref()),
            _ => None,
        }
    }
    fn restore_view_state(&mut self, view: &crate::view::ViewContribution) {
        if let TileContent::App(app) = &mut self.content {
            crate::view::capability::restore_view_state(app.as_mut(), view);
        }
    }
}

impl crate::presence::capability::PublishesPresence for TileHost {
    /// Presence identity is the record shown, not the tile — the only
    /// thing that tells two tabs of one app apart, since a peer's caret is
    /// a position in *a document*. Unbound tiles report nothing.
    fn presence_key(&self) -> Option<crate::presence::PresenceKey> {
        self.binding
            .as_ref()
            .map(|b| crate::presence::PresenceKey::Tab {
                id: b.live.as_str().to_string(),
            })
    }
}

impl crate::search::capability::SearchSource for TileHost {
    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        match &self.content {
            TileContent::App(app) => crate::search::capability::search_providers(app.as_ref()),
            _ => Vec::new(),
        }
    }
    fn search_provider_descriptors(
        &self,
    ) -> Vec<crate::search::capability::SearchProviderDescriptor> {
        match &self.content {
            TileContent::App(app) => {
                crate::search::capability::search_provider_descriptors(app.as_ref())
            }
            _ => Vec::new(),
        }
    }
    fn activate_search_result(
        &mut self,
        action_id: &str,
        doc: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) {
        if let TileContent::App(app) = &mut self.content {
            crate::search::capability::activate_search_result(app.as_mut(), action_id, doc, path);
        }
    }
}

impl crate::widget::controls::ActionControls for TileHost {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        // An unbound tile inherits its surrounding owner exactly once. Moving
        // within that incarnation is presentation reparenting; a different
        // owner needs an explicit close/renew transition, never this hook.
        if self.history_owner().is_none() && !self.has_separate_document_owner() {
            if domain.closed() {
                domain.unsafe_attachment();
                return;
            }
            if let Some(previous) = self.custody_parent.as_ref() {
                if domain.permits_renewal(previous, attachment) {
                    self.history_lifetime.control_domain.close();
                    self.history_lifetime.active.set(false);
                    self.history_lifetime =
                        crate::history_mode::owner::LegacyHistoryLifetime::new();
                    self.custody_scope = None;
                    self.custody_renewal = None;
                } else if previous.closed() || !previous.same_owner_incarnation(domain) {
                    domain.unsafe_attachment();
                    return;
                }
            }
        }
        self.custody_parent = Some(domain.clone());
        self.install_live_custody(attachment);
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if self.has_separate_document_owner() {
            return None;
        }
        self.children_mut()
            .into_iter()
            .find_map(crate::widget::controls::take_control_invocation)
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if self.has_separate_document_owner() {
            return;
        }
        for child in self.children_mut() {
            crate::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
}

impl crate::history_mode::capability::FrameworkHistoryTopology for TileHost {
    fn visit_framework_history(
        &self,
        visitor: &mut dyn FnMut(&crate::history_mode::FrameworkHistorySurface, &dyn Widget),
    ) {
        if let TileContent::App(live) = &self.content {
            if let Some(history) = &self.history {
                visitor(history, live.as_ref());
            }
            crate::history_mode::capability::visit_framework_history(live.as_ref(), visitor);
        }
    }
    fn visit_framework_history_mut(
        &mut self,
        visitor: &mut dyn FnMut(&mut crate::history_mode::FrameworkHistorySurface, &mut dyn Widget),
    ) {
        let owner = self.history_owner();
        if let TileContent::App(live) = &mut self.content {
            if let Some(history) = &mut self.history {
                history.set_enclosing_owner(owner);
                visitor(history, live.as_mut());
            }
            crate::history_mode::capability::visit_framework_history_mut(live.as_mut(), visitor);
        }
    }
}

impl crate::collaboration::participants::CollaborationTopology for TileHost {
    fn collaboration_children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match &mut self.content {
            TileContent::App(live) => vec![live.as_mut()],
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => {
                Vec::new()
            }
        }
    }
}

impl TileHost {
    /// The window's outline. Painted *outside* the card's silhouette, safe
    /// from app content by the clip in `draw_steady_state` rather than a
    /// paint-order race: a stroke over unknown app content can't pick one
    /// alpha that reads right on every ground, but the desktop outside is
    /// known, so `Theme::window_edge` can derive one that does. Inflated by
    /// half the stroke width so the band sits flush with zero overlap; the
    /// radius grows by the same half so it stays concentric at the
    /// corners. Focus reuses this same edge (colour + weight) rather than a
    /// second ring, keeping the signal part of the window's own silhouette.
    fn draw_edge_at(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        radius: f64,
        rect: Rect,
    ) {
        // Focus recolours the edge and never thickens it: the edge is drawn
        // outside the card (the app's pixels are its own), so a heavier
        // focus edge stood the focused window's outline above and beside
        // its neighbours', and the window the pointer was on looked lifted.
        let edge_width = EDGE_STROKE_WIDTH;
        let edge_color = if self.focused {
            theme.tokens.colors.window_edge_focused()
        } else {
            theme.window_edge()
        };
        // Fill the border's two contours directly. GPU stroking can leave
        // solid tiles outside a rounded corner when its top crosses y=0.
        let outer_offset = if self.flush_edges { 0.0 } else { edge_width };
        let inner_offset = outer_offset - edge_width;
        let outer = RoundedRect::from_rect(
            rect.inflate(outer_offset, outer_offset),
            self.corner_radii(radius + outer_offset),
        );
        let inner = RoundedRect::from_rect(
            rect.inflate(inner_offset, inner_offset),
            self.corner_radii((radius + inner_offset).max(0.0)),
        );
        let mut edge = outer.to_path(0.01);
        edge.extend(inner.path_elements(0.01));
        scene.fill(
            vello::peniko::Fill::EvenOdd,
            transform,
            edge_color,
            None,
            &edge,
        );
    }

    /// The tile's corners at `radius`: all round, or round only at the top
    /// while its bottom edge is the display's ([`Self::set_square_bottom`]).
    fn corner_radii(&self, radius: f64) -> vello::kurbo::RoundedRectRadii {
        if self.square_bottom {
            vello::kurbo::RoundedRectRadii::new(radius, radius, 0.0, 0.0)
        } else {
            vello::kurbo::RoundedRectRadii::from_single_radius(radius)
        }
    }

    /// The tile's bottom edge is the display's: square its lower corners.
    pub fn set_square_bottom(&mut self, square: bool) {
        self.square_bottom = square;
    }

    fn draw_edge(&self, scene: &mut Scene, transform: Affine, theme: &Theme, radius: f64) {
        self.draw_edge_at(scene, transform, theme, radius, self.rect);
    }

    fn draw_transition_status(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.record_error.is_some() {
            return;
        }
        let Some(detail) = &self.record_transition_error else {
            return;
        };
        let margin = 12.0;
        let banner = Rect::new(
            self.rect.x0 + margin,
            (self.rect.y1 - 76.0).max(self.rect.y0 + margin),
            self.rect.x1 - margin,
            self.rect.y1 - margin,
        );
        crate::widget::Banner::paint(
            &mut crate::paint::PaintCtx::new(scene, transform, tr, theme),
            banner,
            &crate::widget::BannerContent::new(
                crate::widget::BannerKind::Error,
                "Document switch blocked",
            )
            .with_message(Some(detail)),
            Point::ZERO,
        );
    }

    /// Content card + edge → hosted app or stub contents. Extracted from `draw` so the spawn-fade branch can
    /// wrap the same paint under one scaled layer.
    fn draw_steady_state(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.full_bleed {
            self.draw_full_bleed(scene, transform, tr, theme, presence);
            return;
        }
        // A window lifts off the desktop ground on one soft shadow
        // (`docs/design-language.md`, "Surfaces"), its own damage part so a
        // tile that moves repaints where its shadow was.
        let radius = f64::from(theme.tokens.radii.tile);
        let shadow = theme.tokens.elevation.window;
        // It falls below the tile only: tiles stand side by side, and a
        // shadow reaching sideways would dim the neighbour's edge.
        if let Some(reach) = crate::surface::shadow_bounds(self.rect, &shadow) {
            let below = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, reach.y1);
            let key = crate::scene_parts::PartKey::new("ui.tile-shadow", ());
            let bounds = vec![transform.transform_rect_bbox(below)];
            crate::scene_parts::part(scene, key, bounds, |scene| {
                scene.push_clip_layer(vello::peniko::Fill::NonZero, transform, &below);
                crate::surface::shadow(scene, transform, self.rect, radius, &shadow);
                scene.pop_layer();
            });
        }
        let shape = RoundedRect::from_rect(self.rect, self.corner_radii(radius));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &shape,
        );
        // Hosted apps fill their own background as a plain rectangle (an
        // app can't know it's being framed); without this clip the first
        // fill squares off the window's rounded corners.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );
        // Everything up to the matching pop is clipped to the tile, so the
        // tile bounds whatever its content changes: a damage part.
        let content = crate::scene_parts::PartKey::new("ui.tile-content", ());
        let bounds = vec![transform.transform_rect_bbox(self.rect)];
        crate::scene_parts::part(scene, content, bounds, |scene| {
            self.draw_clipped_content(scene, transform, tr, theme, presence, &shape);
        });
        scene.pop_layer();
        // The edge, and the focus ring it becomes, is a band along the
        // tile's outline: corner squares and thin side bands, not the
        // tile they surround.
        let edge = crate::scene_parts::PartKey::new("ui.tile-edge", ());
        let bounds = edge_bands(self.rect, radius)
            .into_iter()
            .map(|band| transform.transform_rect_bbox(band))
            .collect();
        if !self.inside_growing_frame {
            crate::scene_parts::part(scene, edge, bounds, |scene| {
                self.draw_edge(scene, transform, theme, radius);
            });
        }
    }

    /// A tile filling its display: the content edge to edge, square and
    /// with no clip or edge around it. Nothing beside it needs setting
    /// apart, and a client surface the content draws stays the last,
    /// unclipped thing drawn there, which the display can scan out.
    fn draw_full_bleed(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &self.rect,
        );
        let shape = RoundedRect::from_rect(self.rect, 0.0);
        let content = crate::scene_parts::PartKey::new("ui.tile-content", ());
        let bounds = vec![transform.transform_rect_bbox(self.rect)];
        crate::scene_parts::part(scene, content, bounds, |scene| {
            self.draw_clipped_content(scene, transform, tr, theme, presence, &shape);
        });
    }

    /// The tile's content inside its clip: the opening card, the app (or a
    /// card in its place), and the card dissolving over an arrived app.
    fn draw_clipped_content(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        shape: &RoundedRect,
    ) {
        // An app asked for but not arrived gets the card in place of the
        // app, not behind it: its early buffer is the client's own size,
        // not the tab's, and painting it would show the window visibly
        // resizing a moment after opening.
        if Widget::opening(self) {
            self.opening_shown = true;
            draw_opening_card(
                scene,
                transform,
                tr,
                theme,
                presence,
                self.rect,
                &self.opening_name,
                &mut self.opening_spinner,
            );
        } else {
            // Dissolve the card over the arrived window rather than cut
            // it away, so the tile is never briefly empty.
            if self.opening_shown && self.handed_over_at.is_none() {
                self.handed_over_at = Some(crate::time::now());
            }
            if let Some(detail) = self.record_error.clone() {
                if self
                    .error_card
                    .as_ref()
                    .is_none_or(|(shown, _)| *shown != detail)
                {
                    self.error_card = Some((detail.clone(), record_unavailable_card(&detail)));
                }
                let rect = self.rect;
                if let Some((_, card)) = &mut self.error_card {
                    card.apply_layout_external(rect);
                    card.draw(scene, transform, tr, theme, presence);
                }
            } else {
                // The app and its history were laid out where the tile
                // was; a moved tile draws them moved with it.
                let content_transform = if self.content_shift == 0.0 {
                    transform
                } else {
                    transform.pre_translate(vello::kurbo::Vec2::new(self.content_shift, 0.0))
                };
                match &mut self.content {
                    TileContent::App(w) => {
                        if self
                            .history
                            .as_ref()
                            .is_none_or(|history| history.live_visible())
                        {
                            // The app is a damage part of its own, hinted
                            // with its widgets' damage: a host comparing
                            // frames repaints a typed character, not the
                            // window.
                            let key = crate::scene_parts::PartKey::new("ui.tile-app", ());
                            crate::scene_parts::begin(scene, key);
                            w.draw(scene, content_transform, tr, theme, presence);
                            let hint = self.content_damage.hint(
                                &**w,
                                content_transform,
                                transform.transform_rect_bbox(self.rect),
                                presence,
                            );
                            crate::scene_parts::end_hinted(scene, hint);
                        } else {
                            self.content_damage.forget();
                        }
                        if let Some(history) = &mut self.history {
                            history.draw(scene, content_transform, tr, theme, presence);
                        }
                    }
                    TileContent::Stub { kind, label } => {
                        let detail = format!("{} coming soon", kind.as_str());
                        draw_card_text(scene, transform, tr, theme, self.rect, label, &detail);
                    }
                    TileContent::Notice { title, detail } => {
                        draw_card_text(scene, transform, tr, theme, self.rect, title, detail);
                    }
                    // The opening card above stands in for it.
                    TileContent::Arriving => {}
                }
            }
            self.draw_transition_status(scene, transform, tr, theme);
            // Reduced motion takes the cut — the window just being there
            // is the end state.
            if theme.reduce_motion {
                self.opening_shown = false;
                self.handed_over_at = None;
            } else if let Some(started) = self.handed_over_at {
                let t = crate::motion::unit_opacity(handover_progress(
                    crate::time::now().saturating_duration_since(started),
                ));
                if t >= 1.0 {
                    self.opening_shown = false;
                    self.handed_over_at = None;
                } else {
                    scene.push_layer(
                        vello::peniko::Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        1.0 - t,
                        transform,
                        shape,
                    );
                    draw_opening_card(
                        scene,
                        transform,
                        tr,
                        theme,
                        presence,
                        self.rect,
                        &self.opening_name,
                        &mut self.opening_spinner,
                    );
                    scene.pop_layer();
                }
            }
        }
    }
}

/// No sentence, deliberately: the app's name over a working indicator is
/// the only two things someone waiting for a window needs. Paints its own
/// ground because it is also the top half of the dissolve that retires
/// it, covering the arrived window underneath while still fading.
// Standard immediate-mode paint-helper signature for this vello renderer
// (scene + transform + text_renderer + theme, plus this draw's own
// content); every parameter is independently load-bearing.
fn draw_opening_card(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    rect: Rect,
    name: &str,
    spinner: &mut Spinner,
) {
    paint_opening_card(
        scene,
        transform,
        tr,
        theme,
        presence,
        rect,
        OpeningCardText { name, status: "" },
        Some(spinner),
    );
}

/// The moving indicator [`paint_opening_card`] draws, at the tile's size.
#[must_use]
pub fn opening_card_spinner() -> Spinner {
    Spinner::with_size(OPENING_SPINNER_PX_F32)
}

/// What the opening card says: the app's name, and a line under the
/// indicator when the wait has something worth saying — an install under
/// way, or a start taking longer than it should. An empty `status` draws
/// exactly the card the tile shows for an arriving window, so handing over
/// from one to the other changes nothing on screen.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpeningCardText<'a> {
    pub name: &'a str,
    pub status: &'a str,
}

/// The opening card, for a place held before any window exists. Draws
/// `spinner` in the indicator's place, or leaves that place to the caller
/// (a measured wait shows a bar there instead). Returns the place.
pub fn paint_opening_card(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    rect: Rect,
    text: OpeningCardText<'_>,
    spinner: Option<&mut Spinner>,
) -> Rect {
    let OpeningCardText { name, status } = text;
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.content,
        None,
        &rect,
    );
    // Measured against the card, not a fixed ramp slot: `neutral[1]` is
    // the *dark* end of a dark theme's ramp, and on that theme's own dark
    // card the name would land at ~1.05:1 contrast — invisible on screen.
    let on_card = theme.on_content().ink;
    let style = theme.text(TextRole::Title);
    let cx = rect.center().x;
    // Above centre, so name + indicator reads as one centred pair rather
    // than the name alone with the indicator hanging below.
    let y = rect.center().y - f64::from(style.size);
    if !name.is_empty() {
        let (tw, _) = tr.measure_style(name, None, &style);
        tr.draw_style(
            scene,
            name,
            on_card,
            transform.pre_translate((cx - f64::from(tw) / 2.0, y).into()),
            None,
            &style,
        );
    }
    let top = y + f64::from(style.size) + OPENING_SPINNER_GAP;
    let indicator = Rect::new(
        cx - OPENING_SPINNER_PX / 2.0,
        top,
        cx + OPENING_SPINNER_PX / 2.0,
        top + OPENING_SPINNER_PX,
    );
    if let Some(spinner) = spinner {
        spinner.set_rect(indicator);
        Widget::draw(spinner, scene, transform, tr, theme, presence);
    }
    if !status.is_empty() {
        // A line as long as a reason wraps within a readable measure inside
        // the card, so a narrow tile still shows all of it.
        let style = theme.text(TextRole::Body);
        let gutter = f64::from(theme.tokens.spacing.s4);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a text width in logical pixels"
        )]
        let measure = (rect.width() - gutter * 2.0)
            .clamp(1.0, f64::from(theme.tokens.metrics.message_measure))
            as f32;
        let (tw, _) = tr.measure_style(status, Some(measure), &style);
        tr.draw_style(
            scene,
            status,
            theme.on_content().ink_dim,
            transform.pre_translate(
                (cx - f64::from(tw) / 2.0, indicator.y1 + OPENING_SPINNER_GAP).into(),
            ),
            Some(measure),
            &style,
        );
    }
    indicator
}

/// One function so the two callers (`Stub`, `Notice`) can't drift apart —
/// only the sentence they supply is meant to read as meaningful.
fn draw_card_text(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    title: &str,
    detail: &str,
) {
    let mut content = super::EmptyStateContent::new(title);
    content.message = (!detail.is_empty()).then_some(detail);
    let mut paint = crate::paint::PaintCtx::new(scene, transform, tr, theme);
    // These are explanatory status cards, not one-line diagnostic summaries.
    // The shared stack wraps the full recovery text within the tile's gutter.
    super::EmptyState::paint(&mut paint, rect, &content, Point::new(-1.0, -1.0));
}

mod held;
pub use held::{hold_windows, release_windows};
#[cfg(test)]
mod held_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "tile_host_arriving_tests.rs"]
mod arriving_tests;
#[cfg(test)]
#[path = "tile_host_window_tests.rs"]
mod window_tests;
