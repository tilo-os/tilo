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

const SPAWN_FADE_MS: f32 = 180.0;
const SPAWN_SCALE_START: f64 = 0.95;

/// One logical pixel: heavier reads as a drawn outline around the app,
/// not the boundary of a surface.
const EDGE_STROKE_WIDTH: f64 = 1.0;

/// Half a pixel heavier than `EDGE_STROKE_WIDTH`, in the focus colour, so
/// the focused surface reads as lit rather than fenced.
const FOCUS_EDGE_STROKE_WIDTH: f64 = 1.6;

/// `kurbo::Affine` composition is plain f64 matrix math with no panic
/// path; `arithmetic_side_effects` flags the `*` operator overload anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine composition is f64 matrix math; it never panics \
              or overflows, unlike integer +/-/*"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

/// See [`translated`]: scaling about a pivot point is translate, scale,
/// translate back — still the same plain f64 matrix math.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine composition is f64 matrix math; it never panics \
              or overflows, unlike integer +/-/*"
)]
fn scaled_about(transform: Affine, centre: Point, scale: f64) -> Affine {
    transform
        * Affine::translate((centre.x, centre.y))
        * Affine::scale(scale)
        * Affine::translate((-centre.x, -centre.y))
}

/// `Duration::as_secs_f32` converts precisely with no lossy integer
/// step, unlike `as_millis() as f32` (`u128` -> `f32`), which is what the
/// truncation/precision-loss lints below were actually catching.
fn elapsed_ms_f32(t: Instant) -> f32 {
    t.elapsed().as_secs_f32() * 1000.0
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
}

impl AppKind {
    pub fn as_str(&self) -> &str {
        match self {
            AppKind::Terminal => "Terminal",
            AppKind::Files => "Files",
            AppKind::Editor => "Editor",
            AppKind::Browser => "Browser",
            AppKind::Settings => "Settings",
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
        if let Some((heads, 0)) = self.store.read(&self.live, super::settle_mark)
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
                let (heads, pending) = super::settle_mark(doc);
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
    rect: Rect,
    /// How far [`Self::offset_layout`] moved the tile from where layout put
    /// it. The content keeps its own layout rect, so it is drawn this much
    /// further right instead.
    content_shift: f64,
    pub focused: bool,
    /// Adjacent canvas windows own exactly their layout rectangle, including
    /// their border; neither outlines nor shadows may spill into a sibling.
    flush_edges: bool,
    /// `None` by default, matching the "animations only fire for runtime
    /// insertions" rule `TabBar` uses.
    spawned_at: Option<Instant>,
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

/// Same length as the tab-close fade — one motion becoming another, short
/// enough nobody is waiting on it.
const HANDOVER_MS: u64 = 180;

/// Mirrors `HANDOVER_MS` as an `f32` for the dissolve's progress ratio —
/// kept as its own literal rather than `HANDOVER_MS as f32` so the
/// conversion is a compile-time constant, not a cast the lints must trust.
const HANDOVER_MS_F32: f32 = 180.0;

/// Larger than the toolkit default so it holds its own under a title on a
/// full-window card, small enough to stay an indicator rather than the
/// subject of the screen.
const OPENING_SPINNER_PX: f64 = 36.0;

/// Mirrors `OPENING_SPINNER_PX` as an `f32`: `Spinner::with_size` takes an
/// `f32`, while every rect computation below stays in `f64` alongside the
/// rest of this widget's geometry.
const OPENING_SPINNER_PX_F32: f32 = 36.0;

const OPENING_SPINNER_GAP: f64 = 24.0;

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
    /// Install a legacy tile's declared presentation against its settled actual
    /// store/checkout/lifetime. No outer State or invented component is used.
    /// # Errors
    /// Refuses missing, stale or foreign document custody and invalid scope metadata.
    /// Exact live component token for this retained history owner.
    pub fn document_history_target(&self) -> Option<String> {
        self.history.as_ref()?;
        let TileContent::App(app) = &self.content else {
            return None;
        };
        let binding = app.callback_binding()?;
        let _entry = binding.enter().ok()?;
        Some(binding.lifetime_id.to_string())
    }
    pub fn request_document_history(&self, target: &str) -> bool {
        if self.document_history_target().as_deref() != Some(target) {
            return false;
        }
        self.history
            .as_ref()
            .is_some_and(|history| history.request_open())
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
            record_transition_error: None,
            record_intent_transition_blocked: false,
            record_creation_refused: false,
            announced: None,
            structure_dirty: false,
            was_presented: std::cell::Cell::new(false),
            content_was_presented: std::cell::Cell::new(false),
            panic_scope: std::cell::Cell::new(None),
            rect: Rect::ZERO,
            content_shift: 0.0,
            focused: false,
            flush_edges: false,
            spawned_at: None,
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

    /// Name `tab` to a panic hook while this tile's application runs: see
    /// [`panic_scope`](super::panic_scope).
    pub fn set_panic_scope(&self, tab: u64) {
        self.panic_scope.set(Some(tab));
    }

    fn running(&self) -> super::panic_scope::Scope {
        super::panic_scope::enter(self.panic_scope.get())
    }

    pub fn set_opening_name(&mut self, name: &str) {
        if self.opening_name != name {
            self.opening_name.clear();
            self.opening_name.push_str(name);
        }
    }

    /// Test seam for the parent group's push — a card naming the wrong
    /// app, or an app-id, is the defect this exists to catch.
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
            .is_some_and(|t| t.elapsed() < Duration::from_millis(HANDOVER_MS))
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
            TileContent::Notice { .. } => false,
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
            title: "Document unavailable".into(),
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
                    root.poll_resource_operation(invocation, false, document, &ROOT)
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
            TileContent::Stub { .. } | TileContent::Notice { .. } => Ok(()),
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
                self.record_transition_error =
                    Some(format!("Document storage unavailable: {error}"));
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
            self.record_error = Some(format!("Document storage unavailable: {error}"));
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
            self.record_error = Some(format!("Document storage unavailable: {error}"));
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
                let shown = format!("Document storage unavailable: {error}");
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
            TileContent::Stub { .. } | TileContent::Notice { .. } => None,
        };
        let changing_owner = match (&self.binding, &want) {
            (Some(binding), Some(want)) if want.id != binding.live => {
                if let Err(error) = binding.store.open_existing_record_checked(want) {
                    let shown = format!("Document storage unavailable: {error}");
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
                self.record_error = Some(format!("Document storage unavailable: {error}"));
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
        }
    }
    /// Mutable legacy content access. Canonical components use checked replacement.
    /// # Panics
    /// Refuses extraction of a canonical component through the legacy content API.
    pub fn content_mut(&mut self) -> &mut TileContent {
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
            Some(t) => elapsed_ms_f32(t) < SPAWN_FADE_MS,
            None => false,
        }
    }

    /// Desktop-space card rect for an app-provided complete semantic canvas.
    /// The provider reports tile-content size; the shell card grows from the
    /// live tile's top-left and never shrinks below the live window surface.
    pub fn complete_content_rect(&self) -> Option<Rect> {
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
        let rect = self.complete_content_rect()?;
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
            TileContent::Stub { .. } | TileContent::Notice { .. } => false,
        };
        scene.pop_layer();
        if !painted {
            return None;
        }
        self.draw_edge_at(scene, transform, theme, radius, rect);
        Some(rect)
    }

    /// Exposed for tests to pin the curve shape without peeking into
    /// Vello's scene buffer.
    #[cfg(test)]
    pub fn spawn_alpha(&self) -> Option<f32> {
        self.spawned_at.and_then(|t| {
            let elapsed = elapsed_ms_f32(t);
            if elapsed >= SPAWN_FADE_MS {
                None
            } else {
                let raw = (elapsed / SPAWN_FADE_MS).clamp(0.0, 1.0);
                Some(1.0 - (1.0 - raw).powi(3))
            }
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

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        if let TileContent::App(widget) = &mut self.content {
            widget.on_floating_surface_event_source(id);
        }
    }

    fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode> {
        match &self.content {
            TileContent::App(widget) => widget.floating_surface_accessibility(id),
            TileContent::Stub { .. } | TileContent::Notice { .. } => None,
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
            TileContent::Stub { .. } | TileContent::Notice { .. } => false,
        }
    }

    fn floating_surface_ime(&self, id: &str) -> Option<(winit::window::ImePurpose, Rect)> {
        match &self.content {
            TileContent::App(widget) => widget.floating_surface_ime(id),
            TileContent::Stub { .. } | TileContent::Notice { .. } => None,
        }
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_floating_surface_request(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => None,
        }
    }

    fn take_capture_studio_request(&mut self) -> Option<super::CaptureMode> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_capture_studio_request(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => None,
        }
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_floating_surface_source_updates(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => Vec::new(),
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
            TileContent::Stub { .. } | TileContent::Notice { .. } => Vec::new(),
        }
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        match &self.content {
            TileContent::App(widget) => widget.active_floating_surface_ids(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => Vec::new(),
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
        dyn crate::component_lifecycle::ComponentLifecycle
    );

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
    fn needs_idle_sync(&self) -> bool {
        let _running = self.running();
        match &self.content {
            TileContent::App(app) => app.needs_idle_sync(),
            _ => false,
        }
    }

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
                app.start_operation(op, &mut crate::widget::Edits::lend(document), root)
            }),
            None => app.start_operation(op, &mut crate::widget::Edits::lend(document), root),
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
            let update = app.poll_resource_operation(invocation, cancellation_requested, doc, path);
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
                    app.poll_resource_operation(invocation, cancellation_requested, document, &ROOT)
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
            TileContent::App(app) => {
                app.poll_resource_operation(invocation, cancellation_requested, doc, path)
            }
            _ => None,
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
                    && let Some(update) = app.poll_resource_operation(invocation, false, doc, path)
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
                        let before = crate::widget::settle_mark(doc);
                        app.settle(&mut crate::widget::Edits::lend(doc), &ROOT);
                        // What it wrote shows in this same frame.
                        if crate::widget::settle_mark(doc) != before {
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
                        ..Default::default()
                    },
                    &[child],
                )
            }
            TileContent::Stub { .. } | TileContent::Notice { .. } => taffy.leaf_node(Style {
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
                if let Some(history) = &mut self.history {
                    history.apply_composition(
                        w.as_mut(),
                        taffy,
                        child_node,
                        Point::new(self.rect.x0, self.rect.y0),
                    );
                    self.structure_dirty |= history.width_needs_refresh(self.rect.width());
                } else {
                    w.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
                }
            } else {
                tracing::warn!(
                    "TileHost::apply_layout: App content with no taffy child; \
                     skipping (next frame's structure rebuild should resolve)"
                );
            }
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
            && elapsed_ms_f32(t) >= SPAWN_FADE_MS
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
            let elapsed = elapsed_ms_f32(started);
            let t = (elapsed / SPAWN_FADE_MS).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - t).powi(3);
            let alpha = eased;
            let scale = SPAWN_SCALE_START + (1.0 - SPAWN_SCALE_START) * f64::from(eased);
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
            let centre = self.rect.center();
            let anim_transform = scaled_about(transform, centre, scale);
            self.draw_steady_state(scene, anim_transform, tr, theme, presence);
            scene.pop_layer();
            return;
        }
        self.draw_steady_state(scene, transform, tr, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let _running = self.running();
        if *ctx.handled {
            return;
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
            *ctx.handled = true;
            return;
        };
        self.install_declared_history();
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        if self.record_error.is_some() && !(typing && self.typing_may_bypass_missing_record()) {
            *ctx.handled = true;
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
                    ),
                    UndoStep::Redo => history.redo_as(
                        doc,
                        Some(&author),
                        None,
                        crate::undo::get_state(app.as_ref()),
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
            return Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                    .with_name("Document unavailable")
                    .with_description(detail.clone())
                    .with_bounds(self.rect),
            );
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
            TileContent::App(_) | TileContent::Stub { .. } => None,
        }
    }
    /// Only an app can answer anything but `true` — a stub/notice is
    /// painted by this process and is on screen the moment the tile is.
    fn presented(&self) -> bool {
        if !self.content_was_presented.get() {
            let now = match &self.content {
                TileContent::App(w) => w.presented(),
                TileContent::Stub { .. } | TileContent::Notice { .. } => true,
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
            TileContent::Stub { .. } | TileContent::Notice { .. } => false,
        }
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn wants_pointer_capture(&self) -> bool {
        match &self.content {
            TileContent::App(widget) => widget.wants_pointer_capture(),
            TileContent::Stub { .. } | TileContent::Notice { .. } => false,
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
            TileContent::Stub { .. } | TileContent::Notice { .. } => false,
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
            TileContent::Stub { .. } | TileContent::Notice { .. } => Vec::new(),
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
        let (edge_width, edge_color) = if self.focused {
            (FOCUS_EDGE_STROKE_WIDTH, theme.tokens.colors.border_focus)
        } else {
            (EDGE_STROKE_WIDTH, theme.window_edge())
        };
        // Fill the border's two contours directly. GPU stroking can leave
        // solid tiles outside a rounded corner when its top crosses y=0.
        let outer_offset = if self.flush_edges { 0.0 } else { edge_width };
        let inner_offset = outer_offset - edge_width;
        let outer = RoundedRect::from_rect(
            rect.inflate(outer_offset, outer_offset),
            radius + outer_offset,
        );
        let inner = RoundedRect::from_rect(
            rect.inflate(inner_offset, inner_offset),
            (radius + inner_offset).max(0.0),
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
        // A tile casts no shadow: it is separated by the ground and its
        // window edge. Only floating things cast one.
        let radius = f64::from(theme.tokens.radii.tile);
        let shape = RoundedRect::from_rect(self.rect, radius);
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
        self.draw_edge(scene, transform, theme, radius);
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
            if let Some(detail) = &self.record_error {
                draw_card_text(
                    scene,
                    transform,
                    tr,
                    theme,
                    self.rect,
                    "Document unavailable",
                    detail,
                );
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
                            w.draw(scene, content_transform, tr, theme, presence);
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
                }
            }
            self.draw_transition_status(scene, transform, tr, theme);
            // Reduced motion takes the cut — the window just being there
            // is the end state.
            if theme.reduce_motion {
                self.opening_shown = false;
                self.handed_over_at = None;
            } else if let Some(started) = self.handed_over_at {
                let t = elapsed_ms_f32(started) / HANDOVER_MS_F32;
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
            translated(transform, cx - f64::from(tw) / 2.0, y),
            None,
            &style,
        );
    }
    let top = y + f64::from(style.size) + OPENING_SPINNER_GAP;
    spinner.set_rect(Rect::new(
        cx - OPENING_SPINNER_PX / 2.0,
        top,
        cx + OPENING_SPINNER_PX / 2.0,
        top + OPENING_SPINNER_PX,
    ));
    Widget::draw(spinner, scene, transform, tr, theme, presence);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Duration;

    /// Test fixtures below deal only in small, non-negative pixel
    /// coordinates (well under `u32::MAX`), so these narrowing casts
    /// can't truncate or flip sign in practice — the lint can't see that
    /// through the float math or fixture constants.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "test fixture coordinates are small non-negative constants"
    )]
    fn px(v: f64) -> u32 {
        v as u32
    }

    /// See [`px`]: the millisecond durations used in these tests are
    /// small positive compile-time constants.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "duration constants used in these tests are small positive values"
    )]
    fn millis(v: f32) -> u64 {
        v as u64
    }

    #[test]
    fn window_edge_shadow_and_focus_ring_stay_off_app_pixels() {
        // Case `a_hosted_app_cannot_square_off_the_window_or_erase_its_border`.
        // An app that fills its whole rect (as every hosted app does, since
        // it can't know it's being framed) must not square off the window's
        // rounded corners or paint over its border — both are the frame's.
        {
            use crate::offscreen::OffscreenRenderer;

            // Tile inset far enough from the surface edge that the ambient
            // shadow sprite has somewhere to land without wrapping.
            let (w, h) = (200u32, 200u32);
            let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
            let mut host = TileHost::new(TileContent::App(Box::new(FloodFill { rect })));
            host.rect = rect;

            let theme = Theme::default();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            host.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::empty(),
            );

            let mut r = OffscreenRenderer::new(w, h);
            // Distinct from both the flood colour and the card so a leak from
            // any of the three is identifiable in the failure message.
            let rgba = r.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 0xFF));

            let radius = f64::from(theme.tokens.radii.tile);
            // A point inside the tile's bounding box but outside its rounded
            // silhouette: one pixel in from the box corner, well inside the
            // corner arc's cut.
            let (cx, cy) = (px(rect.x0) + 2, px(rect.y0) + 2);
            assert!(
                radius > 8.0,
                "test assumes a visibly rounded card; radii.tile is {radius}",
            );
            let (cr, cg, cb, _) = OffscreenRenderer::sample(&rgba, w, cx, cy);
            assert!(
                cr < 128,
                "the hosted app's fill leaked outside the card's rounded \
             corner at ({cx},{cy}) — got ({cr},{cg},{cb}), which is the \
             app's flood colour. The corner belongs to the frame.",
            );

            // Dead centre is the app: the clip must not have swallowed it.
            let (mr, mg, mb, _) = OffscreenRenderer::sample(&rgba, w, 100, 100);
            assert!(
                mr > 200 && mg < 80 && mb < 80,
                "the hosted app must still paint inside the card; centre is \
             ({mr},{mg},{mb}), expected the app's flood colour",
            );

            // One pixel out from the boundary is the frame's edge: it must be
            // neither the app (flood escaped its clip) nor bare desktop (no
            // edge there at all).
            let edge_x = px(rect.x0) - 1;
            let (er, eg, eb, _) = OffscreenRenderer::sample(&rgba, w, edge_x, 100);
            assert!(
                (er, eg, eb) != (0xFF, 0x00, 0x00),
                "the hosted app's fill leaked onto the window's edge at \
             ({edge_x},100) — got ({er},{eg},{eb}), the app's flood colour",
            );
            assert!(
                (er, eg, eb) != (0x00, 0x00, 0xFF),
                "the window has no edge at ({edge_x},100) — the pixel is bare \
             desktop, so nothing separates the window from it",
            );

            // ...and the first pixel *inside* the boundary is the app's, whole.
            // The frame stops at the silhouette.
            let inside_x = px(rect.x0);
            let (nr, ng, nb, _) = OffscreenRenderer::sample(&rgba, w, inside_x, 100);
            assert_eq!(
                (nr, ng, nb),
                (0xFF, 0x00, 0x00),
                "the frame tinted a pixel the app owns at ({inside_x},100) — got \
             ({nr},{ng},{nb}), expected the app's flood colour",
            );
        }

        // Case `the_window_edge_never_tints_a_pixel_the_app_owns`.
        // The window's edge must never tint a pixel the hosted app owns —
        // stroking a constant over unknown app content reads right on one
        // ground and wrong on the rest of the ramp.
        {
            use crate::offscreen::OffscreenRenderer;

            // Spanning the ramp: a dark app, two mid grounds, and the light
            // chrome a word processor paints.
            let grounds: [(u8, u8, u8); 4] = [
                (0x10, 0x10, 0x12),
                (0x40, 0x40, 0x44),
                (0x80, 0x80, 0x84),
                (0xC0, 0xC0, 0xC4),
            ];
            let (w, h) = (200u32, 200u32);
            let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
            let theme = Theme::default();

            for (r8, g8, b8) in grounds {
                let color = vello::peniko::Color::from_rgb8(r8, g8, b8);
                let mut host = TileHost::new(TileContent::App(Box::new(Flood { rect, color })));
                host.rect = rect;
                let mut tr = TextRenderer::new();
                let mut scene = Scene::new();
                host.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &PresenceCtx::empty(),
                );
                let mut renderer = OffscreenRenderer::new(w, h);
                let rgba = renderer.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 0xFF));

                // Both vertical edges at mid-height, and the horizontal edges at
                // mid-width: the first pixel inside the silhouette on every side.
                let probes = [
                    ("left", px(rect.x0), 100u32),
                    ("right", px(rect.x1) - 1, 100u32),
                    ("top", 100u32, px(rect.y0)),
                    ("bottom", 100u32, px(rect.y1) - 1),
                ];
                for (side, px, py) in probes {
                    let (sr, sg, sb, _) = OffscreenRenderer::sample(&rgba, w, px, py);
                    assert_eq!(
                        (sr, sg, sb),
                        (r8, g8, b8),
                        "the frame tinted the app's own pixel on the {side} edge \
                     at ({px},{py}): app painted ({r8},{g8},{b8}), the render \
                     shows ({sr},{sg},{sb}). The window's edge belongs outside \
                     the card, on ground the shell knows — over app content it \
                     is a constant on an unknown surface",
                    );
                }
            }
        }

        // Case `focus_lights_the_card_s_edge_and_never_squares_off_its_corner`.
        // Focus is a change to the window's own edge, staying inside the
        // card's silhouette — not a square fence around the bounding box,
        // which would double-line the rounded corner and paint on desktop
        // diagonally outside it. Checked at the edge's flat and diagonally
        // outside the corner, across every theme preset.
        {
            use crate::offscreen::OffscreenRenderer;

            let (width, height) = (200u32, 200u32);
            let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
            // Desktop stand-in. Deliberately a colour no palette contains, so
            // "still desktop" cannot be confused with "some theme token".
            let ground = vello::peniko::Color::from_rgb8(0x00, 0xFF, 0x00);

            for theme in crate::theme::ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let name = theme.name.clone();
                let focus = theme.tokens.colors.border_focus;
                let (fr, fg, fb) = {
                    let rgba = focus.to_rgba8();
                    (rgba.r, rgba.g, rgba.b)
                };

                let shot = |focused: bool| {
                    let mut host = TileHost::new(TileContent::Stub {
                        kind: AppKind::Editor,
                        label: String::new(),
                    });
                    host.rect = rect;
                    host.set_focused(focused);
                    let mut tr = TextRenderer::new();
                    let mut scene = Scene::new();
                    host.draw(
                        &mut scene,
                        Affine::IDENTITY,
                        &mut tr,
                        &theme,
                        &PresenceCtx::empty(),
                    );
                    let mut renderer = OffscreenRenderer::new(width, height);
                    renderer.render(&scene, ground)
                };

                let lit = shot(true);
                let dark = shot(false);

                // Mid-span of the top edge: the band the stroke occupies sits
                // just outside the card boundary.
                let (sample_x, sample_y) = (100u32, px(rect.y0) - 1);
                let (lr, lg, lb, _) = OffscreenRenderer::sample(&lit, width, sample_x, sample_y);
                assert_eq!(
                    (lr, lg, lb),
                    (fr, fg, fb),
                    "{name}: the focused window's edge is not the theme's focus                  colour at ({sample_x},{sample_y}) — got ({lr},{lg},{lb}), expected                  ({fr},{fg},{fb}). Focus has to come from the theme: a baked                  constant is right on one palette and meaningless on the rest",
                );
                let (ur, ug, ub, _) = OffscreenRenderer::sample(&dark, width, sample_x, sample_y);
                assert_ne!(
                    (ur, ug, ub),
                    (fr, fg, fb),
                    "{name}: an unfocused window paints the focus colour on its                  edge at ({sample_x},{sample_y}), so focus distinguishes nothing",
                );

                // Well outside the rounded arc, where a square fence would
                // have painted. The ambient shadow legitimately darkens this
                // pixel, so the claim is only that focus colour isn't there,
                // not that it's untouched.
                let (corner_x, corner_y) = (px(rect.x0) - 2, px(rect.y0) - 2);
                for (label, buf) in [("focused", &lit), ("unfocused", &dark)] {
                    let (sr, sg, sb, _) = OffscreenRenderer::sample(buf, width, corner_x, corner_y);
                    assert_ne!(
                        (sr, sg, sb),
                        (fr, fg, fb),
                        "{name}: the {label} window painted the focus colour at ({corner_x},{corner_y}), outside its own rounded corner on bare desktop. Only a sharp-cornered rectangle drawn around the window reaches that pixel, which is the artefact this replaced",
                    );
                    assert!(
                        sg > sr && sg > sb,
                        "{name}: the {label} window covered the desktop at ({corner_x},{corner_y}) with ({sr},{sg},{sb}). Outside its rounded corner the desktop must still read as the desktop, shadowed at most, never stroked over",
                    );
                }
            }
        }
    }

    /// Notes, under its `text`, the body of whatever document it is handed
    /// ([`seen_by`]), and optionally declares a file it has open.
    struct Scribe {
        text: String,
        opens: Option<std::path::PathBuf>,
    }

    thread_local! {
        /// The body each [`Scribe`] last saw, by its text; empty for a
        /// document without one.
        static SEEN: std::cell::RefCell<std::collections::HashMap<String, String>> =
            std::cell::RefCell::default();
    }

    /// What the [`Scribe`] named `text` last saw: `None` when it was never
    /// handed a document.
    fn seen_by(text: &str) -> Option<String> {
        SEEN.with_borrow(|seen| seen.get(text).cloned())
    }

    /// Put `body` into the record `id`.
    fn write_body(store: &RecordStore, id: &RecordId, body: &str) {
        use automerge::transaction::Transactable;
        store
            .with(id, |doc| {
                doc.put(ROOT, "body", body).unwrap();
            })
            .unwrap();
    }

    impl Widget for Scribe {
        crate::provides!(dyn crate::record::capability::RecordScoped);

        fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
            use automerge::ReadDoc;
            let (state, path) = crate::doc::document_ref(state, path);
            let body = match state.get(path, "body") {
                Ok(Some((automerge::Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
                _ => None,
            };
            SEEN.with_borrow_mut(|seen| seen.insert(self.text.clone(), body.unwrap_or_default()));
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.widget_leaf(Style::default(), node_map, self)
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn name(&self) -> &'static str {
            "Scribe"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Document)
                    .with_name("Seeded hidden document")
                    .with_value(self.text.clone()),
            )
        }
    }

    impl crate::record::capability::RecordScoped for Scribe {
        fn record_intent(&self) -> Option<RecordCheckout> {
            self.opens.as_ref().map(|p| {
                RecordCheckout::new(
                    RecordId::for_file(p),
                    crate::record::RecordKind::new("file"),
                )
            })
        }
    }

    impl crate::widget::app_widget::AppWidget for Scribe {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Scribe"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("S")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    struct FloatingProbe {
        acknowledged: std::rc::Rc<std::cell::Cell<bool>>,
        update: Option<Rect>,
    }

    impl FloatingSurfaceProvider for FloatingProbe {
        fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
            self.update
                .take()
                .map(|rect| vec![("probe-pip".into(), rect)])
                .unwrap_or_default()
        }

        fn active_floating_surface_ids(&self) -> Vec<String> {
            vec!["probe-pip".into()]
        }

        fn on_floating_surface_host_message(
            &mut self,
            message: &crate::floating_surface::FloatingSurfaceHostMessage,
        ) {
            if matches!(
                message,
                crate::floating_surface::FloatingSurfaceHostMessage::Adopted { id, .. }
                    if id == "probe-pip"
            ) {
                self.acknowledged.set(true);
            }
        }
    }

    impl Widget for FloatingProbe {
        crate::provides!(dyn crate::widget::FloatingSurfaceProvider);

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.widget_leaf(Style::default(), node_map, self)
        }
        fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }
        fn name(&self) -> &'static str {
            "FloatingProbe"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
    }

    impl crate::widget::app_widget::AppWidget for FloatingProbe {
        fn app_kind(&self) -> AppKind {
            AppKind::Browser
        }
        fn app_title(&self) -> &'static str {
            "Floating probe"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("P")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    #[test]
    fn tile_host_forwards_surfaces_and_only_unhandled_events() {
        // Case `tile_host_forwards_the_complete_floating_surface_lifecycle`.
        {
            let acknowledged = std::rc::Rc::new(std::cell::Cell::new(false));
            let source = Rect::new(20.0, 30.0, 420.0, 255.0);
            let mut host = TileHost::new(TileContent::App(Box::new(FloatingProbe {
                acknowledged: acknowledged.clone(),
                update: Some(source),
            })));

            assert_eq!(
                FloatingSurfaceWidgetExt::active_floating_surface_ids(&host),
                vec!["probe-pip"]
            );
            assert_eq!(
                FloatingSurfaceWidgetExt::take_floating_surface_source_updates(&mut host),
                vec![("probe-pip".into(), source)]
            );
            assert!(
                FloatingSurfaceWidgetExt::take_floating_surface_source_updates(&mut host)
                    .is_empty()
            );

            FloatingSurfaceWidgetExt::on_floating_surface_host_message(
                &mut host,
                &crate::floating_surface::FloatingSurfaceHostMessage::Adopted {
                    id: "probe-pip".into(),
                    rect: [100.0, 80.0, 500.0, 305.0],
                },
            );
            assert!(acknowledged.get());
        }

        // Case `a_prehandled_event_never_reaches_the_hosted_app`.
        {
            let calls = std::rc::Rc::new(std::cell::Cell::new(0));
            let mut host = TileHost::new(TileContent::App(Box::new(EventProbe(calls.clone()))));
            let mut taffy = TaffyTree::new();
            let mut node_map = std::collections::HashMap::new();
            let node = host.build_layout(&mut taffy, &Theme::default(), &mut node_map);
            crate::testing::Dispatch::new()
                .handled(true)
                .layout(&taffy, node)
                .send(&mut host, &WindowEvent::Focused(true));
            assert_eq!(calls.get(), 0);
        }
    }

    fn tab_checkout(id: u64) -> RecordCheckout {
        RecordCheckout::new(RecordId::for_tab(id), crate::record::RecordKind::new("tab"))
    }

    fn canonical_store(root: &std::path::Path) -> RecordStore {
        let mut owners = crate::record::RecordOwners::new(root.to_path_buf());
        owners.open("so.tilo.tile-host-test", "room").unwrap()
    }

    fn inspection_has_semantic(node: &crate::headless::InspectionNode, name: &str) -> bool {
        fn access_has(node: &crate::a11y::AccessNode, name: &str) -> bool {
            node.name.as_deref() == Some(name)
                || node.children.iter().any(|child| access_has(child, name))
        }
        node.semantics
            .as_ref()
            .is_some_and(|access| access_has(access, name))
            || node
                .children
                .iter()
                .any(|child| inspection_has_semantic(child, name))
    }

    #[test]
    fn outgoing_records_settle_once_and_keep_failed_owners() {
        // Case `history_checked_canonical_tail_refusal_preserves_bytes_without_a_live_owner`.
        {
            use automerge::transaction::Transactable;
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("records");
            let checkout = tab_checkout(3903);
            {
                let store = canonical_store(&root);
                store.create_record(&checkout).unwrap();
                store.with(&checkout.id, |doc| {
                    doc.put(ROOT, "body", "valid original history").unwrap();
                });
                store.try_flush().unwrap();
            }
            let record_dir = app_state::DocStore::room_record_dir_at(
                &root,
                "so.tilo.tile-host-test",
                "room",
                &checkout.id.storage_key(),
            )
            .unwrap();
            let path =
                app_state::DocStore::open_canonical_at("so.tilo.tile-host-test", &record_dir)
                    .unwrap()
                    .document_path()
                    .to_path_buf();
            let mut doc = AutoCommit::load(&std::fs::read(&path).unwrap()).unwrap();
            let mut bytes = doc.save();
            doc.put(ROOT, "body", "later truncated history").unwrap();
            let increment = doc.save_incremental();
            assert!(increment.len() > 8);
            bytes.extend_from_slice(&increment[..increment.len() - 3]);
            std::fs::write(&path, &bytes).unwrap();
            let store = canonical_store(&root);
            let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
                text: "must never seed rejected history".into(),
                opens: None,
            })));
            host.bind_record(store.clone(), checkout.clone());
            for _ in 0..2 {
                host.sync(&AutoCommit::new(), &ROOT);
                store.tick();
                store.flush();
                assert!(
                    host.history_owner().is_none(),
                    "checked canonical admission must not repair and publish a partial graph"
                );
                assert!(!store.is_ready(&checkout.id));
                assert!(
                    store.save_status(&checkout.id).is_err(),
                    "no writer may be admitted"
                );
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
            }
            assert_eq!(
                host.accessibility().and_then(|node| node.name),
                Some("Document unavailable".into())
            );
        }

        // Case `outgoing_record_intent_keeps_failed_owner_and_unrelated_pending_record`.
        {
            use automerge::{ReadDoc, transaction::Transactable};
            for reject in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let root = temp.path().join("records");
                let store = canonical_store(&root);
                let a = tab_checkout(3980);
                let sibling = tab_checkout(3981);
                let file = temp.path().join("next.txt");
                let b = RecordCheckout::new(
                    RecordId::for_file(&file),
                    crate::record::RecordKind::new("file"),
                );
                for checkout in [&a, &sibling] {
                    store
                        .reserve_record(checkout)
                        .unwrap()
                        .initialize(|_| Ok(()))
                        .unwrap();
                }
                if !reject {
                    store
                        .reserve_record(&b)
                        .unwrap()
                        .initialize(|_| Ok(()))
                        .unwrap();
                }
                store
                    .with(&a.id, |doc| {
                        doc.put(ROOT, "body", "durable baseline").unwrap();
                    })
                    .unwrap();
                store.try_flush_record(&a.id).unwrap();
                store
                    .with(&sibling.id, |doc| {
                        doc.put(ROOT, "pending", "sibling").unwrap();
                    })
                    .unwrap();
                let pending = store
                    .read(&sibling.id, |doc| (doc.base_heads(), doc.pending_ops()))
                    .unwrap();
                let path = app_state::DocStore::room_record_dir_at(
                    &root,
                    "so.tilo.tile-host-test",
                    "room",
                    &a.id.storage_key(),
                )
                .unwrap()
                .join(app_state::DOCUMENT_FILE);
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
                let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
                    text: "unused".into(),
                    opens: Some(file),
                })));
                host.bind_record(store.clone(), a.clone());
                let old = host.history_owner().unwrap();
                store
                    .with(&a.id, |doc| {
                        doc.put(ROOT, "body", "unflushed A").unwrap();
                    })
                    .unwrap();
                assert_eq!(host.resolve_record().is_err(), reject);
                assert!(old.validate().is_err());
                drop(old);
                let mut reports =
                    crate::component_lifecycle::take_component_close_reports(&mut host);
                assert_eq!(
                    reports.len(),
                    1,
                    "intent change must settle the outgoing owner"
                );
                let mut report = reports.pop().unwrap();
                assert!(report.record_error.is_some());
                assert!(report.record_retry.as_ref().unwrap().same_owner(&store));
                assert_eq!(report.record_retry_target.as_ref(), Some(&a.id));
                let _ = host.resolve_record();
                assert!(
                    crate::component_lifecycle::take_component_close_reports(&mut host).is_empty()
                );
                drop(host);
                drop(store);
                std::fs::remove_dir(&path).unwrap();
                let retained = report.record_retry.as_ref().unwrap().clone();
                report.retry_record().unwrap();
                assert_eq!(
                    retained
                        .read(&sibling.id, |doc| (doc.base_heads(), doc.pending_ops()))
                        .unwrap(),
                    pending
                );
                drop(retained);
                let reopened = canonical_store(&root);
                reopened.try_open_record(&a).unwrap();
                assert_eq!(
                    reopened
                        .read(&a.id, |doc| doc
                            .get(ROOT, "body")
                            .unwrap()
                            .unwrap()
                            .0
                            .as_str()
                            .unwrap()
                            .to_owned())
                        .unwrap(),
                    "unflushed A"
                );
            }
        }

        // Case `outgoing_settlement_is_idempotent_but_new_edits_require_another_close`.
        {
            use automerge::transaction::Transactable;
            let temp = tempfile::tempdir().unwrap();
            let store = canonical_store(temp.path());
            let checkout = tab_checkout(3982);
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|_| Ok(()))
                .unwrap();
            let mut host = TileHost::new(declared_probe(&DECLARED_HISTORY, false));
            host.bind_record(store.clone(), checkout.clone());
            store
                .with(&checkout.id, |doc| {
                    doc.put(ROOT, "body", "first").unwrap();
                })
                .unwrap();
            host.replace_content(declared_probe(&DECLARED_HISTORY, false));
            let reports = crate::component_lifecycle::take_component_close_reports(&mut host);
            assert_eq!(reports.len(), 1);
            assert!(reports[0].record_error.is_none());
            let settled = store
                .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap();
            assert_eq!(settled.1, 0);
            host.bind_record(store.clone(), checkout.clone());
            assert!(crate::component_lifecycle::close_components(&mut host).is_empty());
            assert!(crate::component_lifecycle::close_components(&mut host).is_empty());
            assert_eq!(
                store
                    .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                    .unwrap(),
                settled
            );
            host.replace_content(declared_probe(&DECLARED_HISTORY, false));
            store
                .with(&checkout.id, |doc| {
                    doc.put(ROOT, "body", "later accepted edit").unwrap();
                })
                .unwrap();
            assert!(store.read(&checkout.id, |doc| doc.pending_ops()).unwrap() > 0);
            let reports = crate::component_lifecycle::close_components(&mut host);
            assert_eq!(reports.len(), 1);
            assert!(reports[0].record_error.is_none());
            let later = store
                .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap();
            assert_ne!(later.0, settled.0);
            assert_eq!(later.1, 0);
            assert!(crate::component_lifecycle::close_components(&mut host).is_empty());
        }
    }

    struct DeclaredHistoryProbe {
        definition: &'static crate::history_mode::HistoryDefinition,
        existing: bool,
        focused: bool,
    }
    static DECLARED_HISTORY: crate::history_mode::HistoryDefinition =
        crate::history_mode::HistoryDefinition {
            app_id: "so.tilo.tile-host-test",
            scopes: &[crate::history_mode::HistoryScopeDefinition {
                key: "body",
                title: "Body",
                content_schema: None,
                presentation: crate::history_mode::HistoryPresentation::Domain {
                    create: |_| Err("Preview is not needed for custody admission tests".into()),
                },
            }],
        };
    static FOREIGN_HISTORY: crate::history_mode::HistoryDefinition =
        crate::history_mode::HistoryDefinition {
            app_id: "foreign.app",
            scopes: DECLARED_HISTORY.scopes,
        };
    static EXISTING_HISTORY: crate::history_mode::HistoryDefinition =
        crate::history_mode::HistoryDefinition {
            app_id: "so.tilo.tile-host-test",
            scopes: &[crate::history_mode::HistoryScopeDefinition {
                key: "external",
                title: "External",
                content_schema: None,
                presentation: crate::history_mode::HistoryPresentation::External {
                    reason: "existing provider",
                },
            }],
        };
    impl Widget for DeclaredHistoryProbe {
        crate::provides!(dyn crate::history_mode::capability::DocumentHistory);

        fn name(&self) -> &str {
            "Declared history probe"
        }
        fn test_id(&self) -> Option<&str> {
            Some("declared-probe")
        }
        fn is_focusable(&self) -> bool {
            true
        }
        fn is_focused(&self) -> bool {
            self.focused
        }
        fn set_focus(&mut self, focused: bool) {
            self.focused = focused;
        }
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _: &Theme,
            nodes: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = tree.leaf_node(Style::default());
            nodes.insert(node, self as *mut dyn Widget);
            node
        }
        fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {}
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
    }

    impl crate::history_mode::capability::DocumentHistory for DeclaredHistoryProbe {
        fn history_registration(
            &self,
        ) -> Result<
            Option<crate::history_mode::HistoryRegistration>,
            crate::history_mode::HistoryRegistrationError,
        > {
            if !self.existing {
                return Ok(None);
            }
            crate::history_mode::HistoryRegistration::new(
                &EXISTING_HISTORY,
                vec![
                    crate::history_mode::HistorySourceRegistration::Unversioned {
                        scope: "external",
                        reason: "existing provider",
                    },
                ],
            )
            .map(Some)
        }
        fn document_history_definition(
            &self,
        ) -> Option<&'static crate::history_mode::HistoryDefinition> {
            Some(self.definition)
        }
    }
    impl crate::widget::AppWidget for DeclaredHistoryProbe {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &str {
            "Declared history probe"
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("H")
        }
    }
    fn declared_probe(
        definition: &'static crate::history_mode::HistoryDefinition,
        existing: bool,
    ) -> TileContent {
        TileContent::App(Box::new(DeclaredHistoryProbe {
            definition,
            existing,
            focused: true,
        }))
    }

    #[test]
    fn declared_history_binds_to_one_owner_for_its_lifetime() {
        // Case `declared_history_binds_once_and_replaces_its_actual_owner`.
        {
            let temp = tempfile::tempdir().unwrap();
            let first = canonical_store(&temp.path().join("first"));
            let second = canonical_store(&temp.path().join("second"));
            let checkout = tab_checkout(3910);
            for store in [&first, &second] {
                store
                    .reserve_record(&checkout)
                    .unwrap()
                    .initialize(|_| Ok(()))
                    .unwrap();
            }
            let mut host = TileHost::new(declared_probe(&DECLARED_HISTORY, false));
            assert!(host.history.is_none());
            host.bind_record(first.clone(), checkout.clone());
            let initial = host.history_owner().unwrap();
            assert!(host.automatic_history);
            host.bind_record(first, checkout.clone());
            assert!(initial.same_owner(&host.history_owner().unwrap()));
            host.replace_content(declared_probe(&DECLARED_HISTORY, false));
            assert!(initial.validate().is_err());
            assert!(
                host.history.is_some(),
                "replacement must consume the new root declaration"
            );
            let replaced = host.history_owner().unwrap();
            host.bind_record(second, checkout);
            assert!(replaced.validate().is_err());
            assert!(host.history.as_ref().unwrap().validate().is_ok());
            let mut count = 0;
            crate::history_mode::capability::visit_framework_history(&host, &mut |_, _| count += 1);
            assert_eq!(count, 1);
        }

        // Case `declared_history_waits_for_deferred_record_admission`.
        {
            let temp = tempfile::tempdir().unwrap();
            let store = canonical_store(&temp.path().join("records"));
            let checkout = tab_checkout(3913);
            let mut host = TileHost::new(declared_probe(&DECLARED_HISTORY, false));
            host.bind_record(store.clone(), checkout.clone());
            assert!(host.history.is_none());
            assert!(host.history_owner().is_none());
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|_| Ok(()))
                .unwrap();
            host.sync(&AutoCommit::new(), &ROOT);
            assert!(host.record_error.is_none());
            assert!(host.automatic_history);
            let owner = host.history_owner().unwrap();
            host.sync(&AutoCommit::new(), &ROOT);
            assert!(owner.same_owner(&host.history_owner().unwrap()));
            let mut count = 0;
            crate::history_mode::capability::visit_framework_history(&host, &mut |_, _| count += 1);
            assert_eq!(count, 1);
        }

        // Case `declared_history_rebind_cannot_restore_focus_into_same_named_new_owner`.
        {
            use crate::widget::layout::CompositionFocus;
            let temp = tempfile::tempdir().unwrap();
            let first = canonical_store(&temp.path().join("first"));
            let second = canonical_store(&temp.path().join("second"));
            let checkout = tab_checkout(3912);
            for store in [&first, &second] {
                store
                    .reserve_record(&checkout)
                    .unwrap()
                    .initialize(|_| Ok(()))
                    .unwrap();
            }
            let mut host = TileHost::new(declared_probe(&DECLARED_HISTORY, false));
            host.bind_record(first, checkout.clone());
            let mut tree = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            let root = host.build_layout(&mut tree, &Theme::dark(), &mut nodes);
            let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&host, &tree, root, &nodes);
            }
            let target = nodes
                .iter()
                .find_map(|(node, pointer)| {
                    (unsafe { &**pointer }.test_id() == Some("declared-probe")).then_some(*node)
                })
                .unwrap();
            let focus =
                unsafe { CompositionFocus::capture(Some(target), &nodes, &bindings) }.unwrap();
            assert_eq!(
                unsafe { focus.restore(&nodes, &bindings, None) },
                Some(target)
            );
            nodes.clear();
            bindings.clear();
            host.bind_record(second, checkout);
            tree = TaffyTree::new();
            let root = host.build_layout(&mut tree, &Theme::dark(), &mut nodes);
            unsafe {
                bindings.rebuild(&host, &tree, root, &nodes);
            }
            assert!(unsafe { focus.restore(&nodes, &bindings, None) }.is_none());
        }

        // Case `declared_history_refuses_foreign_custody_and_conflicting_root_provider`.
        {
            for (definition, existing) in [(&FOREIGN_HISTORY, false), (&DECLARED_HISTORY, true)] {
                let temp = tempfile::tempdir().unwrap();
                let store = canonical_store(&temp.path().join("records"));
                let checkout = tab_checkout(3911);
                store
                    .reserve_record(&checkout)
                    .unwrap()
                    .initialize(|_| Ok(()))
                    .unwrap();
                let mut host = TileHost::new(declared_probe(definition, existing));
                host.bind_record(store, checkout);
                assert!(host.record_error.is_some());
                assert!(host.history_owner().is_none());
                assert!(!host.automatic_history);
                for _ in 0..2 {
                    host.sync(&AutoCommit::new(), &ROOT);
                    host.update(&AutoCommit::new(), &ROOT);
                    assert!(
                        host.record_error.is_some(),
                        "refusal must survive ordinary frames"
                    );
                    assert!(host.history_owner().is_none());
                }
                if existing {
                    assert!(host.record_error.as_ref().unwrap().contains("Conflicting"));
                }
            }
        }

        // Case `history_owner_lifetime_expires_on_replacement_close_and_store_rebind`.
        {
            let first_dir = tempfile::tempdir().unwrap();
            let second_dir = tempfile::tempdir().unwrap();
            let first = canonical_store(&first_dir.path().join("records"));
            let second = canonical_store(&second_dir.path().join("records"));
            let checkout = tab_checkout(3902);
            for store in [&first, &second] {
                store
                    .reserve_record(&checkout)
                    .unwrap()
                    .initialize(|_| Ok(()))
                    .unwrap();
            }
            let content = || {
                TileContent::App(Box::new(Scribe {
                    text: "live".into(),
                    opens: None,
                }))
            };
            let mut host = TileHost::new(content());
            host.bind_record(first.clone(), checkout.clone());
            let original = host.history_owner().unwrap();
            host.bind_record(first, checkout.clone());
            assert!(original.same_owner(&host.history_owner().unwrap()));
            assert!(original.validate().is_ok());
            host.bind_record(second.clone(), checkout.clone());
            assert!(original.validate().is_err());
            let rebound = host.history_owner().unwrap();
            host.replace_content(content());
            assert!(rebound.validate().is_err());
            let replacement = host.history_owner().unwrap();
            let _ = crate::component_lifecycle::close_components(&mut host);
            assert!(replacement.validate().is_err());
            assert!(host.history_owner().is_none());
            host.replace_content(content());
            let before_missing = host.history_owner().unwrap();
            host.bind_record(second, tab_checkout(999999));
            assert!(before_missing.validate().is_err());
            assert!(host.history_owner().is_none());
            let mut dropped = TileHost::new(content());
            let store = canonical_store(&first_dir.path().join("third"));
            dropped.bind_or_create_record(store, checkout);
            let witness = dropped.history_owner().unwrap();
            drop(dropped);
            assert!(witness.validate().is_err());
        }
    }

    #[test]
    fn record_rebinding_revalidates_store_and_kind() {
        // Case `same_record_id_rebinding_revalidates_store_and_checkout`.
        {
            let first_dir = tempfile::tempdir().unwrap();
            let second_dir = tempfile::tempdir().unwrap();
            let first = canonical_store(&first_dir.path().join("records"));
            let second = canonical_store(&second_dir.path().join("records"));
            let checkout = tab_checkout(3901);
            for store in [&first, &second] {
                store
                    .reserve_record(&checkout)
                    .unwrap()
                    .initialize(|_| Ok(()))
                    .unwrap();
            }
            let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
                text: "first owner".into(),
                opens: None,
            })));
            host.bind_record(first.clone(), checkout.clone());
            host.sync(&AutoCommit::new(), &ROOT);
            host.bind_record(second.clone(), checkout.clone());
            assert!(
                host.binding.as_ref().unwrap().store.same_owner(&second),
                "same spelling in a new store must replace actual custody"
            );
            let wrong =
                RecordCheckout::new(checkout.id.clone(), crate::record::RecordKind::new("wrong"));
            host.bind_record(second, wrong);
            assert!(
                host.binding.is_none(),
                "same ID must not bypass kind validation"
            );
            assert!(host.record_error.is_some());
        }

        // Case `incompatible_canonical_kind_keeps_the_scratch_binding_and_surfaces_error`.
        {
            let directory = tempfile::tempdir().unwrap();
            let store = canonical_store(&directory.path().join("records"));
            let file = std::path::PathBuf::from("/work/report.txt");
            let file_id = RecordId::for_file(&file);
            let wrong = RecordCheckout::new(
                file_id.clone(),
                crate::record::RecordKind::new("different-kind"),
            );
            store
                .reserve_record(&wrong)
                .unwrap()
                .initialize(|doc| {
                    use automerge::transaction::Transactable;
                    doc.put(ROOT, "body", "the other kind's body")
                        .map_err(|error| error.to_string())
                })
                .unwrap();
            let scratch = tab_checkout(91);
            let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
                text: "kept off another kind".into(),
                opens: Some(file),
            })));
            host.bind_or_create_record(store.clone(), scratch.clone());
            host.sync(&AutoCommit::new(), &ROOT);

            assert_eq!(host.binding.as_ref().unwrap().live, scratch.id);
            assert!(
                host.record_error
                    .as_deref()
                    .is_some_and(|error| error.contains("identity does not match"))
            );
            assert_eq!(
                host.accessibility().and_then(|node| node.name),
                Some("Document unavailable".into())
            );
            assert_ne!(
                seen_by("kept off another kind"),
                Some(String::from("the other kind's body")),
                "the app is never handed a record of another kind"
            );
            let driver =
                crate::headless::Driver::new(crate::State::new(), Box::new(host), (400, 240));
            let inspection = driver.inspection();
            assert!(inspection_has_semantic(
                &inspection.root,
                "Document unavailable"
            ));
            assert!(
                !inspection_has_semantic(&inspection.root, "Seeded hidden document"),
                "the complete projected tree must omit the app hidden by the error card"
            );
        }

        // Case `unavailable_canonical_store_keeps_pending_binding_and_recovers_on_retry`.
        {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("records");
            let store = canonical_store(&root);
            std::fs::write(&root, "temporarily not a directory").unwrap();
            let scratch = tab_checkout(92);
            let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
                text: "recovered".into(),
                opens: None,
            })));
            host.bind_or_create_record(store.clone(), scratch.clone());
            assert!(host.binding.is_none());
            assert!(host.pending_binding.is_some());
            assert!(host.record_error.is_some());
            assert_eq!(
                host.accessibility().and_then(|node| node.name),
                Some("Document unavailable".into())
            );

            std::fs::remove_file(&root).unwrap();
            host.sync(&AutoCommit::new(), &ROOT);
            assert!(host.pending_binding.is_none());
            assert!(host.record_error.is_none());
            assert_eq!(host.binding.as_ref().unwrap().live, scratch.id);
            assert_eq!(
                seen_by("recovered"),
                Some(String::new()),
                "the app is handed the recovered scratch record"
            );
        }

        // Case `existing_follow_validates_kind_even_when_record_id_does_not_change`.
        {
            let store = RecordStore::in_memory();
            let file = std::path::PathBuf::from("/work/kind-conflict");
            let checkout = RecordCheckout::new(
                RecordId::for_file(&file),
                crate::record::RecordKind::new("note"),
            );
            let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
                text: "kept off a conflicting kind".into(),
                opens: Some(file),
            })));
            host.bind_or_create_record(store.clone(), checkout.clone());
            host.sync(&AutoCommit::new(), &ROOT);
            assert!(host.record_error.is_some());
            assert_eq!(host.binding.as_ref().unwrap().live, checkout.id);
            assert_eq!(
                seen_by("kept off a conflicting kind"),
                None,
                "an app whose record conflicts is shown nothing"
            );
        }
    }

    struct EventProbe(std::rc::Rc<std::cell::Cell<usize>>);

    impl Widget for EventProbe {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.widget_leaf(Style::default(), node_map, self)
        }
        fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }
        fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
            self.0.set(self.0.get().checked_add(1).unwrap());
        }
        fn name(&self) -> &'static str {
            "EventProbe"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
    }

    impl crate::widget::app_widget::AppWidget for EventProbe {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Event probe"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("E")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    /// Records the window a panic hook would name while it lays out.
    struct ScopeProbe(std::rc::Rc<std::cell::RefCell<Vec<Option<u64>>>>);

    impl Widget for ScopeProbe {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.0
                .borrow_mut()
                .push(super::super::panic_scope::current());
            taffy.widget_leaf(Style::default(), node_map, self)
        }
        fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }
        fn name(&self) -> &'static str {
            "ScopeProbe"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
    }

    impl crate::widget::app_widget::AppWidget for ScopeProbe {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Scope probe"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("S")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    #[test]
    fn a_windows_application_runs_under_its_tab_for_a_panic_hook() {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let host = TileHost::new(TileContent::App(Box::new(ScopeProbe(seen.clone()))));
        let mut group = crate::widget::WindowGroup::from_hosts(
            3,
            vec![(crate::widget::TabDescriptor::new(41, "Probe"), host)],
            41,
            crate::widget::WindowWidth::Fraction(500),
        );
        // A host swapped in after the group was built is named too.
        *group.host_for_tab_mut(41).unwrap() =
            TileHost::new(TileContent::App(Box::new(ScopeProbe(seen.clone()))));
        let mut taffy = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        for child in group.children_mut() {
            child.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        }
        assert_eq!(*seen.borrow(), [Some(41)]);
        assert_eq!(super::super::panic_scope::current(), None);
    }

    #[test]
    fn tiles_follow_the_record_their_app_names() {
        // Case `a_tile_follows_the_record_its_app_names`.
        // A tab with no file is its own record; a tab whose app has a file
        // open is that file's.
        {
            let store = RecordStore::in_memory();
            let parent = AutoCommit::new();

            let mut scratch = TileHost::new(TileContent::App(Box::new(Scribe {
                text: "unsaved".into(),
                opens: None,
            })));
            scratch.bind_or_create_record(store.clone(), tab_checkout(1));
            write_body(&store, &RecordId::for_tab(1), "the first tab");
            scratch.sync(&parent, &ROOT);
            assert_eq!(
                seen_by("unsaved"),
                Some(String::from("the first tab")),
                "a tab with no file shows the record the tab is",
            );

            let file = std::path::PathBuf::from("/work/report.odt");
            store.open_record(&RecordCheckout::new(
                RecordId::for_file(&file),
                crate::record::RecordKind::new("file"),
            ));
            let mut opened = TileHost::new(TileContent::App(Box::new(Scribe {
                text: "the report".into(),
                opens: Some(file.clone()),
            })));
            write_body(&store, &RecordId::for_file(&file), "the report's body");
            opened.bind_or_create_record(store.clone(), tab_checkout(2));
            write_body(&store, &RecordId::for_tab(2), "the second tab");
            opened.sync(&parent, &ROOT);
            assert_eq!(
                seen_by("the report"),
                Some(String::from("the report's body")),
                "an app that has a file open shows that file's record, \
                 not the scratch record its tab started on",
            );
            assert_eq!(
                seen_by("unsaved"),
                Some(String::from("the first tab")),
                "the other tab is untouched by any of it",
            );
        }

        // Case `explicit_scratch_creation_does_not_authorize_missing_file_follow`.
        {
            let store = RecordStore::in_memory();
            let file = std::path::PathBuf::from("/work/missing.txt");
            let file_id = RecordId::for_file(&file);
            let scratch = tab_checkout(94);
            let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
                text: "never creates the file's record".into(),
                opens: Some(file),
            })));
            host.bind_or_create_record(store.clone(), scratch.clone());
            assert!(store.open_existing_record_checked(&scratch).is_ok());
            host.sync(&AutoCommit::new(), &ROOT);
            assert_eq!(host.binding.as_ref().unwrap().live, scratch.id);
            assert!(host.record_error.is_some());
            assert!(
                !store
                    .open_records()
                    .iter()
                    .any(|checkout| checkout.id == file_id)
            );
        }

        // Case `two_tabs_on_one_file_are_one_document`.
        // Two tabs opened on one file are one document — a copy would
        // diverge, and no amount of merging makes two copies one file again.
        {
            let store = RecordStore::in_memory();
            let parent = AutoCommit::new();
            let file = std::path::PathBuf::from("/work/shared.odt");
            store.open_record(&RecordCheckout::new(
                RecordId::for_file(&file),
                crate::record::RecordKind::new("file"),
            ));
            write_body(&store, &RecordId::for_file(&file), "one shared body");
            for (tab, text) in [(3u64, "first"), (4u64, "second")] {
                let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
                    text: text.into(),
                    opens: Some(file.clone()),
                })));
                host.bind_or_create_record(store.clone(), tab_checkout(tab));
                host.sync(&parent, &ROOT);
            }
            assert_eq!(
                store.open_records().len(),
                3,
                "two scratch records and the one file both tabs opened",
            );
            for text in ["first", "second"] {
                assert_eq!(
                    seen_by(text),
                    Some(String::from("one shared body")),
                    "both tabs show the one document",
                );
            }
        }
    }

    /// Backdate the spawn so its fade window has already elapsed.
    fn expire_spawn_animation(h: &mut TileHost) {
        h.spawned_at =
            crate::time::now().checked_sub(Duration::from_millis(millis(SPAWN_FADE_MS) + 20));
    }

    fn stub_host() -> TileHost {
        TileHost::new(TileContent::Stub {
            kind: AppKind::Terminal,
            label: "t".into(),
        })
    }

    #[test]
    fn spawn_animation_is_in_flight_only_for_its_duration() {
        // Case `start_spawn_animation_records_timestamp`.
        {
            let mut h = stub_host();
            assert!(h.spawned_at.is_none(), "fresh host has no spawn timestamp");
            h.start_spawn_animation();
            assert!(
                h.spawned_at.is_some(),
                "start_spawn_animation must record an Instant",
            );
        }

        // Case `spawn_in_flight_returns_true_within_duration`.
        {
            let mut h = stub_host();
            h.start_spawn_animation();
            assert!(
                h.spawn_in_flight(),
                "spawn_in_flight must be true within SPAWN_FADE_MS",
            );
            let alpha = h.spawn_alpha().expect("alpha in flight");
            assert!(
                (0.0..1.0).contains(&alpha),
                "eased alpha mid-flight must be in [0, 1); got {alpha}",
            );
        }

        // Case `spawn_in_flight_returns_false_after_duration`.
        {
            let mut h = stub_host();
            h.start_spawn_animation();
            expire_spawn_animation(&mut h);
            assert!(
                !h.spawn_in_flight(),
                "spawn_in_flight must be false after SPAWN_FADE_MS elapses",
            );
            assert!(
                h.spawn_alpha().is_none(),
                "expired spawn must report no alpha",
            );
        }

        // Case `draw_without_spawn_animation_does_not_push_extra_layer`.
        {
            // Indirect: no direct hook to check the draw path skips the
            // wrapper layer, so this pins spawn_in_flight/spawn_alpha instead
            // — guards against the wrapper becoming always-on and costing an
            // extra layer + affine compose every frame.
            let h = stub_host();
            assert!(!h.spawn_in_flight(), "fresh host must not be in flight");
            assert!(
                h.spawn_alpha().is_none(),
                "fresh host must report no spawn alpha",
            );
        }

        // Case `needs_animation_frame_reflects_spawn_anim_state`.
        {
            let mut h = stub_host();
            assert!(
                !h.needs_animation_frame(),
                "static host must not demand animation frames",
            );
            h.start_spawn_animation();
            assert!(
                h.needs_animation_frame(),
                "in-flight spawn must pump the App run loop",
            );
            expire_spawn_animation(&mut h);
            assert!(
                !h.needs_animation_frame(),
                "after the spawn window elapses, no more frames are needed",
            );
        }
    }

    /// Fills its entire rect with one flat opaque colour, exactly as
    /// `FileExplorerWidget::paint_background` and every other hosted
    /// app's background pass does. Module-scoped (rather than nested in
    /// its one test) purely to keep that test's line count sane.
    struct FloodFill {
        rect: Rect,
    }
    const FLOOD: vello::peniko::Color = vello::peniko::Color::from_rgb8(0xFF, 0x00, 0x00);
    impl Widget for FloodFill {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.widget_leaf(Style::default(), node_map, self)
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                FLOOD,
                None,
                &self.rect,
            );
        }
        fn name(&self) -> &'static str {
            "FloodFill"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }
    impl crate::widget::app_widget::AppWidget for FloodFill {
        fn app_kind(&self) -> AppKind {
            AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "FloodFill"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("F")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    /// Module-scoped alongside [`FloodFill`] purely to keep its one test's
    /// line count sane.
    struct Flood {
        rect: Rect,
        color: vello::peniko::Color,
    }
    impl Widget for Flood {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.widget_leaf(Style::default(), node_map, self)
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                self.color,
                None,
                &self.rect,
            );
        }
        fn name(&self) -> &'static str {
            "Flood"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }
    impl crate::widget::app_widget::AppWidget for Flood {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Flood"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("F")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    /// An app that has been asked for and has not drawn: the two things
    /// its widget is allowed to say about itself.
    struct Starting {
        rect: Rect,
        drawn: std::rc::Rc<std::cell::Cell<bool>>,
        presented: bool,
        opening: bool,
    }

    impl Widget for Starting {
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &Theme,
            _m: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(Style::default())
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
            self.drawn.set(true);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                STARTING_FLOOD,
                None,
                &self.rect,
            );
        }
        fn name(&self) -> &'static str {
            "Starting"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
        fn presented(&self) -> bool {
            self.presented
        }
        fn opening(&self) -> bool {
            self.opening
        }
    }

    impl crate::widget::app_widget::AppWidget for Starting {
        fn app_kind(&self) -> AppKind {
            AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Starting"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("S")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    /// A presented app that remembers the transform it was drawn with.
    struct TransformProbe {
        rect: Rect,
        drawn_with: std::rc::Rc<std::cell::Cell<Option<Affine>>>,
    }

    impl Widget for TransformProbe {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &Theme,
            _m: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(Style::default())
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            transform: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
            self.drawn_with.set(Some(transform));
        }
        fn name(&self) -> &'static str {
            "TransformProbe"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    impl crate::widget::app_widget::AppWidget for TransformProbe {
        fn app_kind(&self) -> AppKind {
            AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Transform probe"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("T")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    #[test]
    fn a_moved_tile_draws_its_app_where_the_tile_went() {
        let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
        let drawn_with = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut host = TileHost::new(TileContent::App(Box::new(TransformProbe {
            rect,
            drawn_with: std::rc::Rc::clone(&drawn_with),
        })));
        host.rect = rect;
        let theme = Theme::default();
        let _ = paint(&mut host, &theme);
        assert_eq!(drawn_with.get(), Some(Affine::IDENTITY));

        host.offset_layout(40.0);
        assert_eq!(host.rect, rect + vello::kurbo::Vec2::new(40.0, 0.0));
        let _ = paint(&mut host, &theme);
        assert_eq!(
            drawn_with.get(),
            Some(Affine::translate((40.0, 0.0))),
            "the app keeps its layout rect and is drawn translated with the tile"
        );

        host.offset_layout(-40.0);
        assert_eq!(host.rect, rect);
        let _ = paint(&mut host, &theme);
        assert_eq!(drawn_with.get(), Some(Affine::IDENTITY));
    }

    /// Loud and unmistakable, so a single sampled pixel says whether the
    /// app painted.
    const STARTING_FLOOD: vello::peniko::Color = vello::peniko::Color::from_rgb8(0xFF, 0x00, 0x00);

    fn starting_host(
        presented: bool,
        opening: bool,
    ) -> (TileHost, std::rc::Rc<std::cell::Cell<bool>>) {
        let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
        let drawn = std::rc::Rc::new(std::cell::Cell::new(false));
        let mut host = TileHost::new(TileContent::App(Box::new(Starting {
            rect,
            drawn: std::rc::Rc::clone(&drawn),
            presented,
            opening,
        })));
        host.rect = rect;
        (host, drawn)
    }

    fn paint(host: &mut TileHost, theme: &Theme) -> Scene {
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        host.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            theme,
            &PresenceCtx::empty(),
        );
        scene
    }

    #[test]
    fn symbolic_notices_do_not_acquire_scratch_record_custody() {
        let store = RecordStore::in_memory();
        let checkout = tab_checkout(7120);
        let mut host = TileHost::new(TileContent::Notice {
            title: "Browser".into(),
            detail: "Opening here…".into(),
        });
        host.rect = Rect::new(20.0, 20.0, 480.0, 300.0);
        for create in [false, true, false] {
            if create {
                host.bind_or_create_record(store.clone(), checkout.clone());
            } else {
                host.bind_record(store.clone(), checkout.clone());
            }
            host.sync(&AutoCommit::new(), &ROOT);
            assert!(host.binding.is_none());
            assert!(host.pending_binding.is_none());
            assert!(
                host.record_error.is_none(),
                "notice must retain its launch status"
            );
            assert!(host.history_owner().is_none());
            assert!(
                store.open_records().is_empty(),
                "a notice owns no scratch document"
            );
            assert!(Widget::presented(&host));
            let _ = paint(&mut host, &Theme::default());
        }

        // A genuine failed document creation remains refused on group rebind.
        host.refuse_record_binding("creation refused".into());
        host.bind_record(store.clone(), checkout.clone());
        assert!(host.record_creation_refused());
        assert_eq!(host.record_error.as_deref(), Some("creation refused"));

        // Explicit replacement with an external notice clears that old refusal.
        host.replace_content(TileContent::Notice {
            title: "Browser".into(),
            detail: "Could not open it here".into(),
        });
        host.bind_record(store, checkout);
        assert!(!host.record_creation_refused());
        assert!(host.record_error.is_none());
        assert!(host.binding.is_none());
    }

    #[test]
    fn replacing_a_document_with_a_notice_detaches_the_record() {
        let store = RecordStore::in_memory();
        let checkout = tab_checkout(7121);
        store.create_record(&checkout).unwrap();
        let mut host = TileHost::new(declared_probe(&DECLARED_HISTORY, false));
        host.bind_record(store.clone(), checkout.clone());
        let old_owner = host.history_owner().expect("document history");
        host.replace_content(TileContent::Notice {
            title: "Browser".into(),
            detail: "Restoring this window…".into(),
        });
        assert!(
            old_owner.validate().is_err(),
            "outgoing document owner is retired"
        );
        assert!(host.binding.is_none());
        assert!(host.history_owner().is_none());
        assert!(host.record_error.is_none());
        assert!(
            store.open_records().contains(&checkout),
            "saved document survives"
        );
    }

    #[test]
    fn an_arrived_app_after_a_visible_card_still_asks_for_a_frame() {
        // Case `retiring_the_opening_card_keeps_one_paint_so_the_app_replaces_it`.
        let (mut host, _) = starting_host(true, false);
        host.mark_opening_shown_for_test();
        assert!(
            Widget::needs_animation_frame(&host),
            "the person already saw the opening card; without one more frame              the compositor sleeps on that empty fill (A05 Store)"
        );
    }

    #[test]
    fn a_tile_holds_its_place_opaquely_until_the_app_can_be_shown() {
        // Case `visible_notice_replacement_tracks_new_content_readiness_without_hiding_the_tile`.
        {
            let theme = Theme::default();
            let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
            let mut host = TileHost::new(TileContent::Notice {
                title: "Calculator".into(),
                detail: "Restoring this window…".into(),
            });
            host.rect = rect;
            assert!(Widget::presented(&host));
            let _ = paint(&mut host, &theme);
            let drawn = std::rc::Rc::new(std::cell::Cell::new(false));
            host.replace_content(TileContent::App(Box::new(Starting {
                rect,
                drawn: std::rc::Rc::clone(&drawn),
                presented: false,
                opening: false,
            })));

            assert!(
                Widget::presented(&host),
                "an already visible tab must not disappear"
            );
            assert!(
                Widget::opening(&host),
                "pending replacement content must use the opening card immediately"
            );
            let _ = paint(&mut host, &theme);
            assert!(
                !drawn.get(),
                "unready replacement must not draw behind the opening card"
            );
            let TileContent::App(app) = &mut host.content else {
                panic!("replacement app");
            };
            app.as_any_mut()
                .and_then(|app| app.downcast_mut::<Starting>())
                .expect("controlled replacement")
                .presented = true;
            assert!(
                !Widget::opening(&host),
                "opening must observe new content readiness without a parent presentation poll"
            );
            assert!(Widget::presented(&host));
            let _ = paint(&mut host, &theme);
            assert!(drawn.get(), "ready replacement must become visible");

            let TileContent::App(app) = &mut host.content else {
                panic!("replacement app");
            };
            let app = app
                .as_any_mut()
                .and_then(|app| app.downcast_mut::<Starting>())
                .expect("controlled replacement");
            app.presented = false;
            app.opening = true;
            assert!(Widget::presented(&host));
            assert!(
                !Widget::opening(&host),
                "the same content resizing must not reopen its card"
            );
        }

        // Case `a_tile_holding_a_place_does_not_paint_the_app_behind_it`.
        // An unpresented app has only its own early buffer size, so painting
        // it is the window appearing magnified then snapping down. The frame
        // holds the place instead, since it's the one thing that knows the
        // tab's size.
        {
            let theme = Theme::default();
            let (mut host, drawn) = starting_host(false, true);
            let _ = paint(&mut host, &theme);
            assert!(
                !drawn.get(),
                "the app painted into a tile that is still holding its place; \
             its buffer is not the size of the tab yet, and putting it on \
             screen is the resize-after-open this gate exists to prevent",
            );
        }

        // Case `a_tile_paints_the_app_as_soon_as_it_has_something_to_show`.
        // And the moment it has something to show, it is what is shown.
        {
            let theme = Theme::default();
            let (mut host, drawn) = starting_host(true, false);
            let _ = paint(&mut host, &theme);
            assert!(
                drawn.get(),
                "an app with a frame that fits its tab must be what the tile \
             paints; anything else is a placeholder standing in front of a \
             window that is already there",
            );
        }

        // Case `a_static_tile_holding_a_place_does_not_pump_frames_forever`.
        // Holding a place is not itself an animation. The hosted opening widget
        // (or the spinner painted by the host) must explicitly request motion;
        // otherwise a static reduced-motion card would keep the desktop awake
        // until the app presented its first frame.
        {
            let (mut host, _) = starting_host(false, true);
            let theme = Theme::default().with_reduce_motion(true);
            let _ = paint(&mut host, &theme);
            assert!(
                !host.needs_animation_frame(),
                "opening state alone must not turn a slow app into a full-rate compositor loop",
            );
        }

        // Case `a_tile_that_has_shown_a_window_never_holds_a_place_again`.
        // A tile that has ever had a window in it never goes back to holding
        // a place for one — without the latch, a drag on a group edge would
        // flash the opening card over a window the person is using.
        {
            let (host, _) = starting_host(true, false);
            assert!(Widget::presented(&host), "precondition: it presented");
            // The app now claims both, as a client whose buffer stopped
            // matching its tab mid-resize would.
            let (host_again, _) = starting_host(true, true);
            assert!(Widget::presented(&host_again));
            assert!(
                !Widget::opening(&host_again),
                "a tile that has presented must never answer `opening` again — \
             a window the person is looking at would be replaced by a card \
             saying it is on its way",
            );
        }

        // Case `the_card_that_holds_a_place_is_opaque`.
        // What is in the tile while the place is held is the frame's own
        // card, not the desktop showing through.
        {
            use crate::offscreen::OffscreenRenderer;
            let theme = Theme::default();
            let (mut host, _) = starting_host(false, true);
            let scene = paint(&mut host, &theme);
            let mut r = OffscreenRenderer::new(200, 200);
            // A ground colour that appears nowhere else, so a pixel of it in
            // the middle of the tile means the card did not cover it.
            let rgba = r.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 0xFF));
            let (cr, cg, cb, _) = OffscreenRenderer::sample(&rgba, 200, 100, 100);
            assert!(
                !(cr < 40 && cg < 40 && cb > 200),
                "the desktop showed through the middle of a window's own place \
             at (100,100) — got ({cr},{cg},{cb}), the backdrop colour. The \
             card has to hold the space, or the window is a hole until it \
             arrives",
            );
            assert!(
                !(cr > 200 && cg < 80 && cb < 80),
                "the app painted after all; got ({cr},{cg},{cb})",
            );
        }
    }

    /// Native Shell dispatches directly into TileHost: unlike App and the
    /// headless Driver, it does not pre-observe embedded History pointers.
    #[test]
    fn live_body_pointer_edges_follow_motion_out_of_history_toolbar() {
        use taffy::prelude::TaffyMaxContent;
        use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase};

        for scale in [1.0, 2.0] {
            let directory = tempfile::tempdir().unwrap();
            let store = canonical_store(&directory.path().join("records"));
            let checkout = tab_checkout(3914);
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|_| Ok(()))
                .unwrap();
            let calls = std::rc::Rc::new(std::cell::Cell::new(0));
            let mut host = TileHost::new(TileContent::App(Box::new(EventProbe(calls.clone()))));
            host.bind_record(store, checkout);
            host.register_document_history(&DECLARED_HISTORY).unwrap();
            let mut tree = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            let node = host.build_layout(&mut tree, &Theme::default(), &mut nodes);
            let mut style = tree.style(node).unwrap().clone();
            style.size = Size {
                width: Dimension::length(800.0),
                height: Dimension::length(480.0),
            };
            tree.set_style(node, style).unwrap();
            tree.compute_layout(node, Size::MAX_CONTENT).unwrap();
            host.apply_layout(&tree, node, Point::ORIGIN);
            let history = host.history.as_ref().unwrap();
            assert!(history.live_interactive());
            let toolbar = history.children().first().unwrap().get_rect();
            assert!(toolbar.width() > 0.0 && toolbar.height() > 0.0);
            let body = Point::new(toolbar.center().x, toolbar.y1 + 40.0);
            assert!(host.get_rect().contains(body) && !toolbar.contains(body));
            let device = winit::event::DeviceId::dummy();
            let motion = |point: Point| WindowEvent::CursorMoved {
                device_id: device,
                position: (point.x * scale, point.y * scale).into(),
            };
            let mut sender = crate::testing::Dispatch::new()
                .scale_factor(scale)
                .layout(&tree, node);
            let mut send = |event: &WindowEvent| {
                calls.set(0);
                let handled = sender.send(&mut host, event).handled;
                (handled, calls.get())
            };
            let wheel = WindowEvent::MouseWheel {
                device_id: device,
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: TouchPhase::Moved,
            };
            assert_eq!(
                send(&motion(toolbar.center())),
                (true, 0),
                "toolbar owns its motion"
            );
            assert_eq!(
                send(&wheel),
                (true, 0),
                "toolbar input must not reach the app"
            );
            assert_eq!(
                send(&motion(body)),
                (false, 1),
                "live body receives pointer motion"
            );
            let button = |state| WindowEvent::MouseInput {
                device_id: device,
                state,
                button: MouseButton::Left,
            };
            for (edge, event) in [
                ("press", button(ElementState::Pressed)),
                ("release", button(ElementState::Released)),
                ("wheel", wheel),
            ] {
                assert_eq!(
                    send(&event),
                    (false, 1),
                    "live body must receive {edge} after leaving toolbar at scale {scale}"
                );
            }
            assert_eq!(
                send(&motion(toolbar.center())),
                (true, 0),
                "returning to toolbar restores its input ownership"
            );
        }
    }

    #[test]
    fn flush_focus_edge_leaves_rounded_corner_cutouts_clear() {
        use crate::offscreen::OffscreenRenderer;
        let theme = Theme::default();
        let ground = vello::peniko::Color::from_rgb8(0, 255, 0);
        let mut renderer = OffscreenRenderer::new(256, 256);
        for right in [62.0, 62.5, 100.0, 120.0] {
            for top in [0.0, 0.5, 16.0] {
                let rect = Rect::new(-400.0, top, right, 400.0);
                let mut host = TileHost::new(TileContent::Stub {
                    kind: AppKind::Editor,
                    label: String::new(),
                });
                host.rect = rect;
                host.set_flush_edges(true);
                host.set_focused(true);
                let radius = f64::from(theme.tokens.radii.tile);
                let mut scene = Scene::new();
                host.draw_edge(&mut scene, Affine::scale(2.0), &theme, radius);
                let rgba = renderer.render(&scene, ground);
                for y in 0..px((top + radius) * 2.0) {
                    for x in px((right - radius) * 2.0)..px(right * 2.0) {
                        let dx = (f64::from(x) + 0.5) / 2.0 - (right - radius);
                        let dy = (f64::from(y) + 0.5) / 2.0 - (top + radius);
                        if dx * dx + dy * dy > (radius + 1.0).powi(2) {
                            let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 256, x, y);
                            assert_eq!(
                                (r, g, b),
                                (0, 255, 0),
                                "focus paint outside corner: right={right}, top={top}, pixel=({x},{y})"
                            );
                        }
                    }
                }
            }
        }
    }
}
