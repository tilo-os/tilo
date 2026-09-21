use crate::render_thread::timing::{MainStage, MainStages, record_main_stage};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{CursorIcon, Fullscreen, Window, WindowId},
};

use std::cell::{Cell, RefCell};

#[path = "profiling/native_input_isolation.rs"]
mod native_input_isolation;

#[path = "animation_schedule.rs"]
mod animation_schedule;

mod action_directory_snapshot;
mod bound_record_sync;
mod close_admission;
mod document_history;
pub use document_history::DocumentHistorySnapshot;

/// Toolkit-owned continuation schema used when an app has no narrower
/// domain-specific projection. The CRDT document travels on its stable record
/// route; this payload restores the complementary local presentation state.
pub const GENERIC_VIEW_CONTINUATION_SCHEMA: u16 = u16::MAX;
/// Framework-owned continuation envelope. Every new checkpoint uses this
/// schema, so app semantics augment rather than replace presentation state.
pub const STRUCTURAL_CONTINUATION_SCHEMA: u16 = u16::MAX - 1;

/// Main-thread-only state behind an opaque `ActionDirectory` cursor. Search
/// providers own their retained indexes; this retains just the provider slot
/// and its bounded continuation token, so no full app resource tree is ever
/// copied into the broker bridge.
#[derive(Clone)]
struct ActionDirectoryCursor {
    revision: String,
    needle: String,
    parent_resource: String,
    provider_index: usize,
    provider_cursor: Option<crate::search::SearchCursor>,
    browse_offset: usize,
    root_widget_index: usize,
    root_provider_cursor: Option<Vec<u8>>,
}

#[cfg(test)]
mod room_record_launch_tests {
    use super::App;
    use crate::record::{RecordCheckout, RecordId, RecordKind, RecordSet, room};
    use crate::{State, widget::Text};
    use automerge::AutoCommit;

    #[test]
    fn launch_reads_existing_room_index_without_opening_its_records() {
        let entry = RecordCheckout::new(RecordId::minted(), RecordKind::new("annotation-set"));
        let mut room_index = AutoCommit::new();
        assert!(room::reference(&mut room_index, &entry.id, &entry.kind));

        let app = App::new(State::new(), Box::new(Text::new("PDF")))
            .with_records(RecordSet::in_memory().with_home("room-a"), room_index);
        let records = app.records().expect("record-aware launch installs the set");
        assert_eq!(records.known_records(), vec![entry]);
        assert!(records.open_records().is_empty());
    }
}

const ACTION_DIRECTORY_MAX_RESULTS: usize = 128;
const ACTION_DIRECTORY_MAX_WORK: usize = 512;
const ACTION_DIRECTORY_MAX_BYTES: usize = 64 * 1024;
const ACTION_DIRECTORY_MAX_DEPTH: u16 = 16;
const ACTION_DIRECTORY_MAX_CURSORS: usize = 256;
const MAX_PENDING_APPLICATION_LAUNCHES: usize = 256;

/// Portable, declared launcher payload used by any app that hands verified
/// qdoc media to another installed app. The broker stages the bytes and owns
/// the target process; the source app never supplies a local path.
#[derive(serde::Deserialize)]
struct ApplicationLaunchPayload {
    app_id: String,
    content_hash: String,
}

impl ApplicationLaunchPayload {
    fn valid(&self) -> bool {
        !self.app_id.is_empty()
            && self.app_id.len() <= 128
            && self
                .app_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
            && self.content_hash.len() == 64
            && self
                .content_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

/// Host-owned generic continuation. Deliberately excludes widget `ephemeral`
/// strings and `RecordId::for_file` values: both may contain local paths or
/// app secrets. File navigation is restored by the shell's typed
/// `FileRecordId` launch intent; this fallback carries presentation only.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PortableContinuationView {
    scroll: Vec<f64>,
    focus_ordinal: Option<usize>,
    viewport: crate::view::ViewRect,
    view_mode: Option<String>,
}

impl PortableContinuationView {
    fn capture(view: crate::view::View) -> Self {
        let scroll = view
            .scroll
            .into_iter()
            .filter(|value| value.is_finite())
            .take(32)
            .collect();
        let view_mode = view.view_mode.filter(|mode| {
            !mode.is_empty()
                && mode.len() <= 64
                && mode
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        });
        Self {
            scroll,
            focus_ordinal: view.focus.map(|focus| focus.ordinal),
            viewport: view.viewport,
            view_mode,
        }
    }

    fn into_view(self) -> crate::view::View {
        crate::view::View {
            scroll: self.scroll,
            focus: self.focus_ordinal.map(|ordinal| crate::view::FocusView {
                ordinal,
                ephemeral: String::new(),
            }),
            // Stable file records are routed by the shell and never encoded
            // as the toolkit's path-bearing `RecordId::for_file`.
            open_resource: None,
            viewport: self.viewport,
            view_mode: self.view_mode,
        }
    }

    fn valid(&self) -> bool {
        self.scroll.len() <= 32
            && self.scroll.iter().all(|value| value.is_finite())
            && [
                self.viewport.x,
                self.viewport.y,
                self.viewport.width,
                self.viewport.height,
            ]
            .into_iter()
            .all(f64::is_finite)
            && self.view_mode.as_ref().is_none_or(|mode| {
                !mode.is_empty()
                    && mode.len() <= 64
                    && mode
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
    }
}

const CONTINUATION_FACET_PRESENTATION: u8 = 1 << 0;
const CONTINUATION_FACET_SEMANTIC: u8 = 1 << 1;
const CONTINUATION_KNOWN_FACETS: u8 = CONTINUATION_FACET_PRESENTATION | CONTINUATION_FACET_SEMANTIC;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StructuralContinuationEnvelope {
    facets: u8,
    presentation: PortableContinuationView,
    semantic: Option<StructuralSemanticProjection>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StructuralSemanticProjection {
    schema_version: u16,
    #[serde(with = "serde_bytes")]
    bytes: Vec<u8>,
}

/// Facets present in a validated structural continuation envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StructuralContinuationFacets(u8);

impl StructuralContinuationFacets {
    pub fn presentation(self) -> bool {
        self.0 & CONTINUATION_FACET_PRESENTATION != 0
    }

    pub fn semantic(self) -> bool {
        self.0 & CONTINUATION_FACET_SEMANTIC != 0
    }
}

fn valid_structural_envelope(envelope: &StructuralContinuationEnvelope) -> bool {
    envelope.facets & !CONTINUATION_KNOWN_FACETS == 0
        && envelope.facets & CONTINUATION_FACET_PRESENTATION != 0
        && (envelope.facets & CONTINUATION_FACET_SEMANTIC != 0) == envelope.semantic.is_some()
        && envelope.presentation.valid()
}

/// Inspect the framework facets without exposing their private wire shape.
pub fn structural_continuation_facets(bytes: &[u8]) -> Option<StructuralContinuationFacets> {
    let envelope = postcard::from_bytes::<StructuralContinuationEnvelope>(bytes).ok()?;
    valid_structural_envelope(&envelope).then_some(StructuralContinuationFacets(envelope.facets))
}

/// Compose the toolkit-owned checkpoint used by [`App`]. Public shell/runtime
/// hosts may use this to produce the identical envelope in conformance tests.
pub fn compose_structural_continuation_metadata(
    view: crate::view::View,
    semantic: Option<crate::collaboration::ContinuationMetadata>,
) -> Option<crate::collaboration::ContinuationMetadata> {
    // Invalid widget projections must never suppress the toolkit checkpoint.
    // Shell policy performs app-specific validation; these common bounds
    // prevent malformed implementations from poisoning publication.
    let semantic = semantic
        .filter(|metadata| {
            metadata.schema_version != STRUCTURAL_CONTINUATION_SCHEMA
                && metadata.schema_version != GENERIC_VIEW_CONTINUATION_SCHEMA
                && !metadata.bytes.is_empty()
                && metadata.bytes.len() <= 32 * 1024
        })
        .map(|metadata| StructuralSemanticProjection {
            schema_version: metadata.schema_version,
            bytes: metadata.bytes,
        });
    let facets = CONTINUATION_FACET_PRESENTATION
        | if semantic.is_some() {
            CONTINUATION_FACET_SEMANTIC
        } else {
            0
        };
    let mut envelope = StructuralContinuationEnvelope {
        facets,
        presentation: PortableContinuationView::capture(view),
        semantic,
    };
    let mut bytes = postcard::to_stdvec(&envelope).ok()?;
    if bytes.len() > 32 * 1024 && envelope.semantic.is_some() {
        envelope.semantic = None;
        envelope.facets &= !CONTINUATION_FACET_SEMANTIC;
        bytes = postcard::to_stdvec(&envelope).ok()?;
    }
    (!bytes.is_empty() && bytes.len() <= 32 * 1024).then_some(
        crate::collaboration::ContinuationMetadata {
            schema_version: STRUCTURAL_CONTINUATION_SCHEMA,
            bytes,
        },
    )
}

/// Validate and canonicalize a framework envelope at the shell boundary.
/// An unrecognized app projection is removed rather than retained as opaque
/// bytes; its generic presentation checkpoint remains usable on every device.
pub fn sanitize_structural_continuation_metadata(
    bytes: &[u8],
    semantic_is_safe: impl FnOnce(u16, &[u8]) -> bool,
) -> Option<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > 32 * 1024 {
        return None;
    }
    let mut envelope = postcard::from_bytes::<StructuralContinuationEnvelope>(bytes).ok()?;
    if !valid_structural_envelope(&envelope) {
        return None;
    }
    if envelope.semantic.as_ref().is_some_and(|semantic| {
        semantic.schema_version == STRUCTURAL_CONTINUATION_SCHEMA
            || semantic.schema_version == GENERIC_VIEW_CONTINUATION_SCHEMA
            || semantic.bytes.is_empty()
            || semantic.bytes.len() > 32 * 1024
            || !semantic_is_safe(semantic.schema_version, &semantic.bytes)
    }) {
        envelope.semantic = None;
        envelope.facets &= !CONTINUATION_FACET_SEMANTIC;
    }
    postcard::to_stdvec(&envelope)
        .ok()
        .filter(|canonical| !canonical.is_empty() && canonical.len() <= 32 * 1024)
}

/// Validate the toolkit-owned generic wire shape at the shell boundary.
/// App-provided opaque bytes must not be accepted merely because they use the
/// generic schema number.
pub fn validate_generic_continuation_metadata(bytes: &[u8]) -> bool {
    let Ok(view) = postcard::from_bytes::<PortableContinuationView>(bytes) else {
        return false;
    };
    view.valid()
}

/// Synthetic input for `EventLoopProxy`: each variant becomes the real
/// `WindowEvent`(s) and routes through the same handler, so injected and
/// real-hardware events take the identical code path.
#[derive(Debug, Clone)]
pub enum UserEvent {
    Hover {
        x: f64,
        y: f64,
    },
    /// `CursorMoved` â†’ Pressed â†’ Released, dispatched as one unit.
    Click {
        x: f64,
        y: f64,
    },
    Wheel {
        dx: f32,
        dy: f32,
    },
    /// Keyboard press and release routed through the ordinary window handler.
    Key(winit::keyboard::Key),
    /// Opt-in workload input with an absolute schedule and stable input identity.
    NativeScenarioInput(crate::native_scenario::Packet),
    RenderReady(winit::window::WindowId),
    /// Delivered to [`crate::widget::Widget::on_external_event`] rather than
    /// translated to a `WindowEvent` (e.g. installer progress).
    External(String),
    /// Synchronize and redraw after typed state was changed by a worker.
    /// Created automatically by [`crate::LocalStateWriter`].
    Wake(crate::wake::WakeEvent),
    /// No `WindowEvent` analogue, so accesskit events get their own variant.
    AccessKit(accesskit_winit::WindowEvent),
}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(e: accesskit_winit::Event) -> Self {
        UserEvent::AccessKit(e.window_event)
    }
}
use crate::collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, CollaborationManager, PresencePayload,
};
use crate::gpu::RenderContext;
use crate::permissions::PermissionContext;
use crate::presence::{PresenceCtx, PresenceKey};
use crate::render_thread::{FrameOutcome, RenderRequest, RenderThreadHandle, spawn_render_thread};
use crate::state::State;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{
    AudiencePresentationWidgetExt as _, FloatingSurfaceWidgetExt as _, FullscreenWidgetExt as _,
    Widget,
};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use taffy::{AvailableSpace, NodeId, TaffyTree};
use vello::{
    Renderer, RendererOptions, Scene, kurbo::Affine, kurbo::Point, kurbo::Rect as KurboRect,
    peniko::Color,
};

/// Cached `QUICK_UI_PERF=1` check, so the per-frame read is cheap.
fn perf_enabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var("QUICK_UI_PERF").is_ok_and(|s| !s.is_empty()))
}

/// `QUICK_UI_NO_SCROLL_OPT=1` disables the scroll-reposition fast path,
/// forcing a full `compute_layout` on every `layout_dirty` frame â€” a safety
/// valve if a subtree ever needs a re-solve without reporting
/// `layout_needs_measure`.
fn scroll_opt_disabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var("QUICK_UI_NO_SCROLL_OPT").is_ok_and(|s| !s.is_empty()))
}

/// Whether an animation request should keep the event loop on its 60 Hz pump.
/// Reduced-motion widgets paint their settled state without an animation
/// redraw, so their raw demand must not keep an otherwise-idle app awake.
const fn animation_pump_needed(needs_frame: bool, reduce_motion: bool) -> bool {
    needs_frame && !reduce_motion
}

fn continuation_publication(
    app_id: String,
    metadata: Option<&crate::collaboration::ContinuationMetadata>,
    had_previous: bool,
) -> Option<CollabRequest> {
    match metadata {
        Some(metadata) => Some(CollabRequest::ReportContinuation {
            app_id,
            schema_version: metadata.schema_version,
            metadata: metadata.bytes.clone(),
        }),
        None if had_previous => Some(CollabRequest::ClearContinuation { app_id }),
        None => None,
    }
}

#[cfg(test)]
mod animation_pump_tests {
    use super::{animation_pump_needed, continuation_publication};
    use crate::collaboration::{CollabRequest, ContinuationMetadata};

    #[test]
    fn reduced_motion_does_not_keep_the_idle_loop_awake() {
        assert!(animation_pump_needed(true, false));
        assert!(!animation_pump_needed(true, true));
        assert!(!animation_pump_needed(false, false));
    }

    #[test]
    fn continuation_disappearance_clears_the_durable_projection() {
        let request = continuation_publication("ebook".into(), None, true);
        assert!(matches!(
            request,
            Some(CollabRequest::ClearContinuation { app_id }) if app_id == "ebook"
        ));

        assert!(continuation_publication("ebook".into(), None, false).is_none());

        let metadata = ContinuationMetadata {
            schema_version: 3,
            bytes: vec![1, 2, 3],
        };
        assert!(matches!(
            continuation_publication("ebook".into(), Some(&metadata), false),
            Some(CollabRequest::ReportContinuation {
                app_id,
                schema_version: 3,
                metadata,
            }) if app_id == "ebook" && metadata == vec![1, 2, 3]
        ));
    }
}

fn next_boundary(s: &str, byte: usize) -> usize {
    let mut b = byte.min(s.len());
    loop {
        if b >= s.len() {
            return s.len();
        }
        b = b.saturating_add(1);
        if s.is_char_boundary(b) {
            return b;
        }
    }
}

fn prev_boundary(s: &str, byte: usize) -> usize {
    let mut b = byte.min(s.len());
    loop {
        if b == 0 {
            return 0;
        }
        b = b.saturating_sub(1);
        if s.is_char_boundary(b) {
            return b;
        }
    }
}

/// `None` when the caret is in whitespace / at a gap with no adjacent word.
fn word_at(s: &str, byte: usize) -> Option<String> {
    let b = byte.min(s.len());
    // On a boundary (between words or at the end), prefer the word to the
    // left so Home/End reads a real word.
    let is_ws = |c: char| c.is_whitespace();
    let char_at = |i: usize| s.get(i..).and_then(|t| t.chars().next());
    let on_word = char_at(b).is_some_and(|c| !is_ws(c));
    let anchor = if on_word {
        b
    } else {
        let left = prev_boundary(s, b);
        if char_at(left).is_some_and(|c| !is_ws(c)) {
            left
        } else {
            return None;
        }
    };
    let mut start = anchor;
    while start > 0 {
        let prev = prev_boundary(s, start);
        if char_at(prev).is_some_and(is_ws) {
            break;
        }
        start = prev;
    }
    let mut end = anchor;
    while end < s.len() {
        let next = next_boundary(s, end);
        if char_at(end).is_some_and(is_ws) {
            break;
        }
        end = next;
    }
    let word = s.get(start..end)?;
    if word.is_empty() {
        None
    } else {
        Some(word.to_string())
    }
}

/// Minimum gap between outbound pointer presence broadcasts, in
/// milliseconds. Winit delivers `CursorMoved` at up to 120 Hz (~8 ms/delta),
/// which would saturate the Unix socket for no visual gain; 33 ms caps the
/// wire rate at ~30 Hz, fluid enough without becoming the bottleneck.
pub(crate) const POINTER_BROADCAST_MIN_GAP_MS: u64 = 33;

#[derive(Clone, Copy, Debug)]
struct DirectedInvocation {
    wire_id: u64,
    cancellation_requested: bool,
}

/// Public-in-crate so the throttle decision is unit-testable without a real
/// event loop.
pub(crate) fn pointer_broadcast_allowed(
    last: Option<Instant>,
    now: Instant,
    min_gap: Duration,
) -> bool {
    match last {
        None => true,
        Some(t) => now.duration_since(t) >= min_gap,
    }
}

/// Free function so the fullscreen reconcile decision is unit-testable
/// without a live winit window (real OS fullscreen can't be exercised
/// headlessly).
pub(crate) fn fullscreen_request_decision(requested: bool, want: Option<bool>) -> Option<bool> {
    match want {
        Some(w) if w != requested => Some(w),
        _ => None,
    }
}

/// Lets a nested settings/appearance pane drive the app's global appearance
/// without a back-channel.
fn find_appearance_intent(w: &dyn Widget) -> Option<crate::theme::AppearanceIntent> {
    let _entry = crate::bound_dispatch::enter_widget(w).ok()?;
    if let Some(intent) = w.appearance_intent() {
        return Some(intent);
    }
    let mut found = None;
    w.for_each_child(&mut |child| {
        if found.is_none() {
            found = find_appearance_intent(child);
        }
    });
    found
}

#[derive(Debug, Clone, Copy)]
enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Fans cursor moves out to Iroh peers too, alongside the Unix-socket
/// send. Invoked after the throttle gate.
type PresenceBroadcastSink = Box<dyn Fn(&PresenceKey, &PresencePayload) + Send + Sync>;
/// Lets the shell drain `NeighbourMsg::PresenceMoved` into
/// [`PresenceCtx::remote_cursors`] without App knowing about Iroh.
/// Returns `true` only when the callback changed pixels. This keeps an
/// installed background pump from turning every event-loop wake into a full
/// window redraw.
type FrameTick = Box<dyn FnMut(&mut PresenceCtx) -> bool + Send + Sync>;
type ResolvedResourceInvocation<'a> = (
    Option<serde_json::Map<String, serde_json::Value>>,
    &'a tilo_actions::Resource,
    &'a tilo_actions::ActionSpec,
);

/// Exact host-side mutation key for the resource/action surface. Every route
/// that can mutate document or presentation-only widget state bumps it. This
/// deliberately avoids `AutoCommit::get_heads()`, which closes an open
/// transaction and could split a pending undo unit during a read-only query.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ResourceCacheKey {
    mutation_generation: u64,
}

struct ResourceCache {
    key: ResourceCacheKey,
    declared: Vec<tilo_actions::Resource>,
    // None is deferred; Some(empty) is a completed, rejected surface.
    complete: Option<Vec<tilo_actions::Resource>>,
}

struct LocalResourceCache {
    key: ResourceCacheKey,
    permission_generation: u64,
    // Control availability can exist before the complete local catalogue.
    resources: Option<Vec<tilo_actions::Resource>>,
    allowed: std::collections::HashSet<tilo_actions::ActionAddress>,
}

struct OpenResourceSearchCache {
    key: ResourceCacheKey,
    permission_generation: u64,
    provider: crate::search::OpenResourceSearch,
}

#[cfg(test)]
use crate::bound_dispatch::find_action_resource;

/// Runtime audit result for one interactive control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlBindingStatus {
    Resolved,
    /// A high-frequency framework-owned CRDT editing stream with automatic
    /// collaboration/history semantics rather than a discrete action address.
    CollaborativeDocument,
    SemanticAction,
    /// Multiple affordances intentionally invoke the same operation.
    SharedAddress,
    StaleAddress,
    ExplicitLegacy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlBindingCoverage {
    pub binding: crate::widget::ControlBindingMetadata,
    pub status: ControlBindingStatus,
}

/// One visible expression of a domain action in the retained UI. The action
/// address remains the execution identity; this route is presentation
/// metadata for focus, presence, animation, and â€œshow me whereâ€ behavior.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionUiRoute {
    pub address: tilo_actions::ActionAddress,
    pub control_id: String,
    /// Root-to-control semantic path. Fewer nodes is the cheaper UI route.
    pub path: Vec<crate::a11y_tree::SemanticId>,
    pub bounds: Option<vello::kurbo::Rect>,
    pub focused: bool,
    pub disabled: bool,
}

/// Host-owned state for retained layout and widget lookup.
///
/// Keeping these values together makes invalidation and layout solving one
/// responsibility instead of a collection of loosely related `App` fields.
struct LayoutRuntime {
    taffy: TaffyTree<()>,
    root_node: Option<NodeId>,
    node_map: std::collections::HashMap<NodeId, *mut dyn Widget>,
    layout_dirty: bool,
    /// Set when a widget reports `take_structure_dirty()`: without a taffy
    /// rebuild before the next layout pass, runtime-spawned widgets have no
    /// `NodeId` and `apply_layout` skips them.
    structure_dirty: bool,
    /// Diffed against `inner_size()` in `about_to_wait` to catch a resize
    /// that arrived without a `Resized` event.
    last_frame_size: Option<winit::dpi::PhysicalSize<u32>>,
    /// Last successfully settled logical viewport. Scale changes count even
    /// when physical pixels stay fixed; scroll-only frames reuse the solve.
    last_layout_logical: Option<(f64, f64)>,
}

impl LayoutRuntime {
    fn new() -> Self {
        Self {
            taffy: TaffyTree::new(),
            root_node: None,
            node_map: std::collections::HashMap::new(),
            layout_dirty: true,
            structure_dirty: false,
            last_frame_size: None,
            last_layout_logical: None,
        }
    }
}

/// Host-owned input modality, gesture, cursor, and tooltip state.
struct InputRuntime {
    modifiers: winit::event::Modifiers,
    interaction: crate::interaction::InteractionEnvironment,
    platform: crate::platform::PlatformConventions,
    nav_gestures: crate::gesture::NavGestureRecognizer,
    current_cursor: CursorIcon,
    tooltip_host: crate::widget::TooltipHost,
}

impl InputRuntime {
    fn new() -> Self {
        Self {
            modifiers: winit::event::Modifiers::default(),
            interaction: crate::interaction::InteractionEnvironment::default(),
            platform: crate::platform::PlatformConventions::default(),
            nav_gestures: crate::gesture::NavGestureRecognizer::new(),
            current_cursor: CursorIcon::Default,
            tooltip_host: crate::widget::TooltipHost::new(),
        }
    }
}

/// Host-owned action discovery, dispatch, and invocation state.
///
/// Actions remain directly connected to the retained widget tree; this
/// boundary centralizes their runtime lifecycle rather than separating them
/// from UI capabilities.
struct ActionRuntime {
    action_palette: Option<crate::action_palette::ActionPalette>,
    action_registry: tilo_actions::ActionRegistry,
    action_transport: Box<dyn tilo_actions::ActionTransport>,
    /// Resource declarations are expensive tree materialisations and used by
    /// input gating, Ctrl+K, accessibility and broker publication. Keep one
    /// exact snapshot until the host crosses a state-mutation boundary.
    resource_cache: RefCell<Option<ResourceCache>>,
    bound_resource_cache: crate::bound_dispatch::BoundResourceCache,
    bound_record_sync: Option<bound_record_sync::BoundRecordSync>,
    bound_publication: crate::bound_dispatch::BoundPublication,
    last_published_bound_route: Option<crate::collaboration::PrimaryRouteWitness>,
    #[cfg(test)]
    bound_broker_route_for_test: Option<crate::collaboration::PrimaryRouteWitness>,
    action_directory_stamp: Option<(action_directory_snapshot::DirectoryWitness, String)>,
    action_directory_snapshot: Option<action_directory_snapshot::ActionDirectorySnapshot>,
    bound_invocations: std::collections::HashMap<
        tilo_actions::InvocationId,
        crate::bound_dispatch::BoundActionTarget,
    >,
    local_resource_cache: RefCell<Option<LocalResourceCache>>,
    open_resource_search_cache: RefCell<Option<OpenResourceSearchCache>>,
    resource_generation: Cell<u64>,
    permission_generation: Cell<u64>,
    last_control_availability: Option<(ResourceCacheKey, u64)>,
    last_published_actions: Option<Vec<tilo_actions::ActionDescriptor>>,
    #[cfg(test)]
    action_surface_publication_attempts: u64,
    /// Opaque, revision-bound continuation state for the broker pull
    /// directory. It holds provider cursors, never resource bodies, and is
    /// touched only by the main/UI thread.
    action_directory_cursors: std::collections::HashMap<String, ActionDirectoryCursor>,
    /// Background/streaming operations admitted by the retained widget tree.
    /// They remain here until the widget reports exactly one terminal update.
    directed_invocations: std::collections::HashMap<tilo_actions::InvocationId, DirectedInvocation>,
    /// Same lifecycle for actions started from this window rather than by the
    /// broker; terminal state is reflected by the owning widget's UI.
    local_invocations: std::collections::HashMap<
        tilo_actions::InvocationId,
        (tilo_actions::ActionAddress, tilo_actions::OperationKind),
    >,
    /// A generic app-launch request finishes an already-admitted source
    /// invocation only after the session reports the staging/launch result.
    pending_application_launches: std::collections::HashMap<u64, tilo_actions::InvocationId>,
    /// Correlates local requests routed through the authoritative session
    /// broker. `None` means this process has consumed the complete u64
    /// namespace and must refuse rather than alias an earlier request.
    next_broker_request_id: Option<u64>,
}

impl ActionRuntime {
    fn new() -> Self {
        Self {
            action_palette: None,
            action_registry: tilo_actions::ActionRegistry::new(),
            action_transport: Box::new(tilo_actions::LocalTransport::new()),
            resource_cache: RefCell::new(None),
            bound_resource_cache: crate::bound_dispatch::BoundResourceCache::default(),
            bound_record_sync: None,
            bound_publication: crate::bound_dispatch::BoundPublication::default(),
            last_published_bound_route: None,
            #[cfg(test)]
            bound_broker_route_for_test: None,
            action_directory_stamp: None,
            action_directory_snapshot: None,
            bound_invocations: std::collections::HashMap::new(),
            local_resource_cache: RefCell::new(None),
            open_resource_search_cache: RefCell::new(None),
            resource_generation: Cell::new(0),
            permission_generation: Cell::new(0),
            last_control_availability: None,
            last_published_actions: None,
            #[cfg(test)]
            action_surface_publication_attempts: 0,
            action_directory_cursors: std::collections::HashMap::new(),
            directed_invocations: std::collections::HashMap::new(),
            local_invocations: std::collections::HashMap::new(),
            pending_application_launches: std::collections::HashMap::new(),
            next_broker_request_id: Some(0),
        }
    }
}

/// Host-owned accessibility state and semantic projection.
struct AccessibilityRuntime {
    /// `None` makes every narration call site a no-op, so a reader-off build
    /// remains unaffected.
    screen_reader: Option<crate::screen_reader::ScreenReader>,
    /// winit doesn't expose the Insert reader modifier as a `Modifiers` bit.
    screen_reader_modifier_down: bool,
    live_region_last: std::collections::HashMap<String, String>,
    focus_trap_active: bool,
    focus_before_trap: Option<NodeId>,
    /// Kept separate from the effective theme so disabling high contrast
    /// restores the configured theme exactly.
    base_theme: Theme,
    a11y: crate::a11y::A11ySettings,
    text_scale: f32,
    ui_scale: f32,
    semantic_tree: crate::a11y_tree::SemanticTree,
}

impl AccessibilityRuntime {
    fn new(base_theme: Theme, screen_reader: Option<crate::screen_reader::ScreenReader>) -> Self {
        Self {
            screen_reader,
            screen_reader_modifier_down: false,
            live_region_last: std::collections::HashMap::new(),
            focus_trap_active: false,
            focus_before_trap: None,
            base_theme,
            a11y: crate::a11y::A11ySettings::default(),
            text_scale: 1.0,
            ui_scale: 1.0,
            semantic_tree: crate::a11y_tree::SemanticTree::new(),
        }
    }
}

/// Host-owned persistence and record-checkout state.
struct PersistenceRuntime {
    autosave: Option<crate::persist::Autosave>,
    /// The live document belongs to the record selected by the root widget.
    records: Option<crate::record::RecordSet>,
    room_index: Option<crate::state::DocumentReplica>,
}

impl PersistenceRuntime {
    fn new() -> Self {
        Self {
            autosave: None,
            records: None,
            room_index: None,
        }
    }
}

/// Host-owned continuation publication and deferred restore state.
struct ContinuationRuntime {
    last_published: Option<crate::collaboration::ContinuationMetadata>,
    pending_restore: Option<crate::collaboration::ContinuationMetadata>,
    /// Generic view restoration waits until layout has built the focus order.
    pending_view: Option<crate::view::View>,
}

impl ContinuationRuntime {
    fn new(pending_restore: Option<crate::collaboration::ContinuationMetadata>) -> Self {
        Self {
            last_published: None,
            pending_restore,
            pending_view: None,
        }
    }
}

/// Host-owned collaboration transports and local authorship identity.
struct CollaborationRuntime {
    manager: CollaborationManager,
    /// Keeps the room record index subscribed while `manager` follows a
    /// checked-out record route.
    room_manager: Option<CollaborationManager>,
    local_author: crate::permissions::UserId,
}

impl CollaborationRuntime {
    fn new(manager: CollaborationManager, local_author: crate::permissions::UserId) -> Self {
        Self {
            manager,
            room_manager: None,
            local_author,
        }
    }
}

/// Host-owned presence publication, follow, and frame-pump state.
struct PresenceRuntime {
    broadcast_scope: Option<PresenceKey>,
    pointer_space: Option<crate::presence::PointerSpace>,
    followed_peer: Option<String>,
    last_pointer_broadcast: Option<Instant>,
    last_focus: Option<(PresenceKey, crate::collaboration::PresencePayload)>,
    last_location: Option<(PresenceKey, crate::collaboration::PresencePayload)>,
    last_view: Option<crate::view::ViewSubset>,
    broadcast_sink: Option<PresenceBroadcastSink>,
    frame_tick: Option<FrameTick>,
}

impl PresenceRuntime {
    fn new() -> Self {
        Self {
            broadcast_scope: None,
            pointer_space: None,
            followed_peer: None,
            last_pointer_broadcast: None,
            last_focus: None,
            last_location: None,
            last_view: None,
            broadcast_sink: None,
            frame_tick: None,
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independently toggled, orthogonal opt-in/state bit with its own doc comment; grouping them into sub-structs would ripple through every read/write site in this file for no behavioural gain"
)]
pub struct App {
    #[cfg(windows)]
    startup_renderer: Option<RendererPreparation>,
    wake_scope: Option<crate::wake::WakeScope>,
    state: State,
    root_widget: Box<dyn Widget>,
    history: Option<Box<crate::history_mode::host::HistoryHost>>,
    #[cfg(any(test, feature = "test-support"))]
    controlled_installed_history_definition:
        Option<&'static crate::history_mode::HistoryDefinition>,
    framework_input: std::rc::Rc<Cell<bool>>,
    history_key_release: bool,
    history_focus: Option<crate::widget::layout::CompositionFocus>,
    text_renderer: TextRenderer,
    palette_source: PaletteSource,
    watch_system_settings: bool,
    layout: LayoutRuntime,
    bindings: crate::bound_dispatch::BoundNodeMap,
    component_close_reports: Vec<crate::application::ComponentCloseReport>,
    close_admission: crate::close_admission::Admission,
    needs_sync: bool,
    pub theme: Theme,
    pub debug_mode: bool,
    pub focused_node: Option<NodeId>,
    pub focusable_order: Vec<NodeId>,
    /// Opt-in: focuses the first focusable widget after initial layout, so
    /// the app is keyboard-usable without a click.
    pub autofocus_first: bool,
    /// Opt-in: re-establishes keyboard focus on every structural rebuild.
    /// [`crate::widget::Widget::take_focus_reset`] jumps to the first
    /// focusable (wizard advancing a step); otherwise focus is preserved by
    /// ordinal position so an unrelated rebuild doesn't drop the caret.
    pub autofocus_on_structure_change: bool,
    /// Opt-in, off by default so host-themed apps pay nothing: polls the
    /// tree each tick for [`crate::widget::Widget::appearance_intent`] and
    /// re-themes live.
    pub appearance_reconcile: bool,
    /// Shared settings-file watcher for OS-wide theme changes made by Theme
    /// Studio. Throttled so the render loop does not stat the file per frame.
    theme_store_stamp: Option<std::time::SystemTime>,
    last_theme_store_check: Instant,
    input: InputRuntime,

    // Stable state for Undo/Redo tracking
    last_stable_focus_idx: Option<usize>,
    last_stable_metadata: Option<String>,

    collaboration: CollaborationRuntime,
    pub presence_ctx: PresenceCtx,
    pub permission_ctx: PermissionContext,

    presence: PresenceRuntime,

    accessibility: AccessibilityRuntime,

    /// Compared against [`crate::widget::Widget::fullscreen_intent`] each
    /// frame so a steady desired state issues one `set_fullscreen` call â€”
    /// see [`fullscreen_request_decision`].
    fullscreen_requested: bool,
    /// To distinguish app-driven from compositor-driven transitions.
    fullscreen_active: bool,

    pub(crate) legacy_persistence: crate::launch::LegacyPersistencePolicy,
    persistence: PersistenceRuntime,
    autosave_generation: u64,
    document_routes: crate::history_mode::routes::OwnerDocumentRoutes,

    actions: ActionRuntime,
    document_history: Box<crate::standalone_history_host::StandaloneHistoryHost>,
    history_chrome_enabled: bool,
    history_layout: Option<crate::standalone_history_host::HostHistoryLayout>,
    history_viewport_width: f64,
    history_focus_pending: bool,
    continuation: ContinuationRuntime,
    last_published_playback: Option<Vec<media::handoff::PlaybackReport>>,
    last_playback_publish: Option<Instant>,
    profiler: Option<crate::profiling::FrameProfiler>,
    native_input_probe: Option<Box<dyn Fn() -> serde_json::Value>>,

    /// Wins over the automatic env/package-id/binary-name resolution.
    window_title: Option<String>,
    /// Wins over `TILO_PACKAGE_ID`/binary-name resolution for the Wayland
    /// `xdg_toplevel.app_id`. System surfaces use this as a stable identity.
    window_app_id: Option<String>,

    /// Optional app â†’ shell lifecycle channel for detachable surfaces. It is
    /// discovered from the launcher-provided service fd and absent outside a
    /// Tilo session, where apps simply keep their overlays attached.
    floating_surface_client: Option<crate::floating_surface::FloatingSurfaceServiceClient>,
    last_floating_presentations:
        std::collections::HashMap<String, crate::floating_surface::FloatingSurfacePresentation>,
    /// Activity IDs from the previous service pump, including generic
    /// browser/capture mirrors that do not own a secondary native window.
    /// Comparing this set lets an app toggle `PiP` off with an explicit Dismiss
    /// instead of abandoning a compositor mirror until process exit.
    last_active_floating_ids: std::collections::HashSet<String>,
    /// IDs whose shell `PrepareSurface`/`Adopted` reply has established
    /// desktop coordinate space. Before this edge, presentation geometry is
    /// still app-local and must not overwrite the translated detach request.
    prepared_floating_presentations: std::collections::HashSet<String>,
    /// App-local â†’ desktop translation supplied by the shell for each
    /// prepared native surface. While a frozen transfer proxy owns the pixels,
    /// pointer capture can still arrive through the primary app window; the
    /// runtime uses this to keep that held drag in desktop coordinates.
    floating_coordinate_offsets: std::collections::HashMap<String, vello::kurbo::Vec2>,
    /// Native surfaces temporarily hidden while their live content is back
    /// inside the app. They retain the same compositor capability and warm
    /// renderer until the activity ends or detaches again.
    suspended_floating_presentations: std::collections::HashSet<String>,
    /// Lifecycle edges also drive the runtime's secondary-window set. The
    /// root sees them immediately; `AppHandler` drains this copy where an
    /// `ActiveEventLoop` is available for window creation/destruction.
    floating_host_messages: Vec<crate::floating_surface::FloatingSurfaceHostMessage>,
}

/// Lazy private inputs keep discovery at the original initializer/callback boundary.
/// Constructing this provider itself performs no discovery beyond its supplied manager.
enum ConstructionValue<T> {
    Lazy(fn() -> T),
    Ready(T),
}
impl<T> ConstructionValue<T> {
    fn resolve(self) -> T {
        match self {
            Self::Lazy(produce) => produce(),
            Self::Ready(value) => value,
        }
    }
}

#[derive(Clone, Copy)]
enum PaletteSource {
    Owned {
        renderer: fn() -> TextRenderer,
        field: fn() -> crate::widget::SearchField,
    },
    Host,
}

struct AppConstructionInputs {
    collab_manager: CollaborationManager,
    configured_theme: ConstructionValue<Theme>,
    text_renderer: ConstructionValue<TextRenderer>,
    palette_source: PaletteSource,
    watch_system_settings: bool,
    debug_mode: fn() -> bool,
    theme_store_stamp: fn() -> Option<std::time::SystemTime>,
    local_author: ConstructionValue<crate::permissions::UserId>,
    presence_ctx: ConstructionValue<PresenceCtx>,
    screen_reader: fn() -> Option<crate::screen_reader::ScreenReader>,
    high_contrast: fn() -> bool,
    reduce_motion: fn() -> bool,
    text_scale: fn() -> f32,
    ui_scale: fn() -> f32,
    pending_continuation_restore: fn() -> Option<crate::collaboration::ContinuationMetadata>,
    profiler: fn() -> Option<crate::profiling::FrameProfiler>,
    floating_surface_client: fn() -> Option<crate::floating_surface::FloatingSurfaceServiceClient>,
    startup_presence_scope: fn() -> Option<PresenceKey>,
    surface_id: ConstructionValue<String>,
}

impl AppConstructionInputs {
    fn native_component(
        inputs: crate::application::ComponentRunnerInputs,
        binding: &crate::bound_dispatch::CallbackBinding,
    ) -> Self {
        Self::component_with_services(
            inputs,
            binding,
            Self::production(CollaborationManager::for_record(&binding.checkout.id)),
        )
    }

    fn component_with_services(
        inputs: crate::application::ComponentRunnerInputs,
        binding: &crate::bound_dispatch::CallbackBinding,
        mut services: Self,
    ) -> Self {
        // Keep the explicit presentation and canonical author. Only the native
        // runner supplies process services; the application factory stays pure.
        services.configured_theme = ConstructionValue::Ready(inputs.theme);
        services.text_renderer = ConstructionValue::Ready(inputs.text_renderer);
        services.palette_source = PaletteSource::Host;
        services.local_author = ConstructionValue::Ready(binding.author.clone());
        let mut presence = services.presence_ctx.resolve();
        presence.add_own_identity(&binding.author.0);
        services.presence_ctx = ConstructionValue::Ready(presence);
        services
    }

    fn component(
        inputs: crate::application::ComponentRunnerInputs,
        binding: &crate::bound_dispatch::CallbackBinding,
    ) -> Self {
        let mut presence = PresenceCtx::empty();
        presence.set_own_identity([binding.author.0.clone()]);
        Self {
            collab_manager: CollaborationManager::disabled(),
            configured_theme: ConstructionValue::Ready(inputs.theme),
            text_renderer: ConstructionValue::Ready(inputs.text_renderer),
            palette_source: PaletteSource::Host,
            watch_system_settings: false,
            local_author: ConstructionValue::Ready(binding.author.clone()),
            presence_ctx: ConstructionValue::Ready(presence),
            surface_id: ConstructionValue::Ready(format!(
                "component:{}:{}",
                binding.instance_id, binding.lifetime_id
            )),
            debug_mode: || false,
            theme_store_stamp: || None,
            screen_reader: || None,
            high_contrast: || false,
            reduce_motion: || false,
            text_scale: || 1.0,
            ui_scale: || 1.0,
            pending_continuation_restore: || None,
            profiler: || None,
            floating_surface_client: || None,
            startup_presence_scope: || None,
        }
    }

    fn production(collab_manager: CollaborationManager) -> Self {
        Self {
            collab_manager,
            configured_theme: ConstructionValue::Lazy(Theme::configured),
            text_renderer: ConstructionValue::Lazy(TextRenderer::new),
            palette_source: PaletteSource::Owned {
                renderer: TextRenderer::new,
                field: crate::widget::SearchField::new,
            },
            watch_system_settings: true,
            debug_mode: || cfg!(debug_assertions) && std::env::var("QUICK_UI_DEBUG").is_ok(),
            theme_store_stamp: || {
                settings_store::settings_path()
                    .and_then(|path| std::fs::metadata(path).ok()?.modified().ok())
            },
            local_author: ConstructionValue::Lazy(crate::collaboration::local_user_id),
            presence_ctx: ConstructionValue::Lazy(App::initial_presence_context),
            screen_reader: || {
                if App::screen_reader_env_enabled()
                    || App::persisted_accessibility_bool("screen_reader")
                {
                    Some(App::configured_screen_reader())
                } else {
                    None
                }
            },
            high_contrast: App::high_contrast_env_enabled,
            reduce_motion: App::reduce_motion_env_enabled,
            text_scale: || App::text_scale_env().unwrap_or(1.0),
            ui_scale: || App::ui_scale_env().unwrap_or(1.0),
            pending_continuation_restore: crate::collaboration::continuation_restore_env,
            profiler: crate::profiling::FrameProfiler::from_env,
            floating_surface_client:
                crate::floating_surface::FloatingSurfaceServiceClient::discover,
            startup_presence_scope: PresenceKey::for_local_workspace,
            surface_id: ConstructionValue::Lazy(crate::local_surface_id),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn controlled() -> Self {
        Self {
            collab_manager: CollaborationManager::disabled(),
            configured_theme: ConstructionValue::Lazy(Theme::dark),
            text_renderer: ConstructionValue::Lazy(crate::font_catalog::fixture_renderer),
            palette_source: PaletteSource::Owned {
                renderer: crate::font_catalog::fixture_renderer,
                field: || crate::widget::SearchField::with_language("en_US"),
            },
            watch_system_settings: true,
            debug_mode: || false,
            theme_store_stamp: || None,
            local_author: ConstructionValue::Lazy(|| {
                crate::permissions::UserId("fixture-user".into())
            }),
            presence_ctx: ConstructionValue::Lazy(|| {
                let mut presence = PresenceCtx::empty();
                presence.set_own_identity(["fixture-user".to_owned()]);
                presence
            }),
            screen_reader: || None,
            high_contrast: || false,
            reduce_motion: || false,
            text_scale: || 1.0,
            ui_scale: || 1.0,
            pending_continuation_restore: || None,
            profiler: || None,
            floating_surface_client: || None,
            startup_presence_scope: || None,
            surface_id: ConstructionValue::Lazy(|| "fixture-surface".into()),
        }
    }
}

impl App {
    /// Install a read-only workload observer for scheduled native inputs.
    /// Snapshots verify fixture state changes; they do not prove pixel presentation.
    /// Keep this callback inexpensive: it executes on the UI thread.
    pub fn set_native_input_probe(&mut self, probe: impl Fn() -> serde_json::Value + 'static) {
        self.native_input_probe = Some(Box::new(probe));
    }

    pub(crate) fn native_input_snapshot(&self) -> Option<serde_json::Value> {
        self.native_input_probe
            .as_ref()
            .map(|probe| probe())
            .or_else(|| {
                self.root_widget
                    .native_input_snapshot(&self.state.doc, &self.state.root)
            })
    }

    /// Mount an already prepared component with explicit presentation and local services.
    /// # Errors
    /// Returns the original component recovery owners when mounting is refused.
    pub fn from_component(
        component: crate::application::AppComponent,
        inputs: crate::application::ComponentRunnerInputs,
    ) -> Result<Self, crate::launch::LaunchError> {
        Self::from_component_using(component, inputs, AppConstructionInputs::component)
    }

    /// Mount a standalone native component with the process's existing system
    /// services, including record-scoped collaboration and accessibility.
    /// Presentation, identity, storage and authority still belong to the host.
    /// # Errors
    /// Preserves the component's recovery owners on mount or route mismatch.
    pub fn from_component_with_native_services(
        component: crate::application::AppComponent,
        inputs: crate::application::ComponentRunnerInputs,
    ) -> Result<Self, crate::launch::LaunchError> {
        Self::from_component_using(component, inputs, AppConstructionInputs::native_component)
    }

    fn from_component_using(
        mut component: crate::application::AppComponent,
        inputs: crate::application::ComponentRunnerInputs,
        construction: fn(
            crate::application::ComponentRunnerInputs,
            &crate::bound_dispatch::CallbackBinding,
        ) -> AppConstructionInputs,
    ) -> Result<Self, crate::launch::LaunchError> {
        let scope = crate::wake::WakeScope::new();
        let _entry = scope.enter();
        let handle = scope.handle();
        if let Err(error) = component.mount(move |_| handle.wake()) {
            return Err(component.setup_error(error));
        }
        let binding = component.callback_binding();
        let inputs = construction(inputs, &binding);
        if let Some(route) = inputs.collab_manager.primary_route()
            && !binding.matches_broker_route(Some(route))
        {
            return Err(component.setup_error(crate::launch::LaunchError::BindingMismatch));
        }
        let root = Box::new(crate::widget::BoundAppWidget { component });
        Ok(Self::from_scoped_parts_with_inputs(
            scope,
            State::new(),
            root,
            || inputs,
        ))
    }

    pub(crate) fn native_component_presentation() -> crate::application::ComponentRunnerInputs {
        let theme = Theme::configured().with_a11y(
            &crate::a11y::A11ySettings {
                high_contrast: Self::high_contrast_env_enabled(),
                reduce_motion: Self::reduce_motion_env_enabled(),
                ..Default::default()
            },
            Self::text_scale_env().unwrap_or(1.0),
            Self::ui_scale_env().unwrap_or(1.0),
        );
        let mut text_renderer = TextRenderer::new();
        text_renderer.set_text_scale(theme.text_scale);
        crate::application::ComponentRunnerInputs {
            theme,
            text_renderer,
        }
    }

    /// Capture/close embedded components while the host still owns their trees.
    /// Recovery reports stay owned by this host until explicitly transferred.
    pub fn close_components(&mut self) {
        self.actions.bound_record_sync = None;
        self.component_close_reports
            .extend(self.root_widget.close_components());
        self.component_close_reports
            .extend(self.root_widget.take_component_close_reports());
        self.bindings.clear();
        self.layout.node_map.clear();
    }
    fn close_native_content(&mut self) {
        self.close_components();
        crate::widget::unmount_native_views(self.root_widget.as_mut());
    }

    pub fn component_close_reports(&self) -> &[crate::application::ComponentCloseReport] {
        &self.component_close_reports
    }
    /// Transfer pending recovery to another owner; ordinary tile destruction cannot lose it.
    pub fn take_component_close_reports(
        &mut self,
    ) -> Vec<crate::application::ComponentCloseReport> {
        self.component_close_reports
            .extend(self.root_widget.take_component_close_reports());
        std::mem::take(&mut self.component_close_reports)
    }

    fn inspect_node<R>(
        &self,
        node: NodeId,
        callback: impl FnOnce(&dyn Widget) -> R,
    ) -> Result<R, crate::bound_dispatch::BoundDispatchError> {
        // SAFETY: App owns this stable tree and no target reference escapes.
        unsafe {
            crate::bound_dispatch::inspect_bound_node(
                &self.bindings,
                &self.layout.node_map,
                node,
                callback,
            )
        }
    }
    fn mutate_node<R>(
        &mut self,
        node: NodeId,
        callback: impl FnOnce(&mut dyn Widget) -> R,
    ) -> Result<R, crate::bound_dispatch::BoundDispatchError> {
        // SAFETY: exclusive App access prevents overlapping target references/rebuild.
        unsafe {
            crate::bound_dispatch::mutate_bound_node(
                &self.bindings,
                &self.layout.node_map,
                node,
                callback,
            )
        }
    }
    fn dispatch_semantic_node<R>(
        &mut self,
        node: NodeId,
        action: &crate::a11y::Action,
        callback: impl FnOnce(&mut dyn Widget, &mut automerge::AutoCommit, &automerge::ObjId) -> R,
    ) -> Result<R, String> {
        let author = self.collaboration.local_author.clone();
        // SAFETY: the runner exclusively owns the unchanged admitted tree.
        unsafe {
            crate::bound_dispatch::invoke_bound_semantic_node(
                &self.bindings,
                &self.layout.node_map,
                node,
                &mut self.state.doc,
                &self.state.root,
                action,
                &self.permission_ctx,
                &author,
                callback,
            )
        }
    }
    fn dispatch_node<R>(
        &mut self,
        node: NodeId,
        callback: impl FnOnce(&mut dyn Widget, &mut automerge::AutoCommit, &automerge::ObjId) -> R,
    ) -> Result<R, crate::bound_dispatch::BoundDispatchError> {
        // SAFETY: exclusive App owns both maps and this stable tree.
        unsafe {
            crate::bound_dispatch::invoke_bound_node(
                &self.bindings,
                &self.layout.node_map,
                node,
                &mut self.state.doc,
                &self.state.root,
                callback,
            )
        }
    }

    /// Apply the native host's fallback for an Enter/Space press that the
    /// focused widget did not consume during normal event dispatch.
    fn handle_unconsumed_activation_key(&mut self, event: &WindowEvent, handled: &mut bool) {
        if *handled
            || !matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(
                            winit::keyboard::NamedKey::Enter | winit::keyboard::NamedKey::Space,
                        ),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                    ..
                }
            )
        {
            return;
        }
        let Some(focused) = self.focused_node else {
            return;
        };
        let activated = self
            .dispatch_node(focused, |widget, doc, root| {
                widget.perform_action(&crate::a11y::Action::Activate, doc, root)
            })
            .unwrap_or(false);
        if activated {
            *handled = true;
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
    }

    fn initial_presence_context() -> PresenceCtx {
        // Own identity, in preference order: the handle announced on `Hello`,
        // then the fallback app id. Anything else is somebody else and gets
        // painted.
        let mut context = PresenceCtx::empty();
        context.set_own_identity(
            crate::collaboration::presence_handle_env()
                .into_iter()
                .chain(std::iter::once(crate::collaboration::local_user_id().0)),
        );
        context
    }

    /// Detached convenience for explicitly supplied managed roots. Production
    /// launchers construct under their installed wake scope, then install History.
    /// # Errors
    /// Refuses invalid or mismatched managed owner registrations.
    pub fn from_managed_root(
        state: State,
        root: crate::history_mode::ManagedRoot,
    ) -> Result<Self, crate::history_mode::HistoryRegistrationError> {
        root.history.validate_root(root.widget.as_ref())?;
        let mut app = Self::new(state, root.widget);
        app.install_history(root.history)?;
        Ok(app)
    }

    pub(crate) fn install_history(
        &mut self,
        registration: crate::history_mode::HistoryRegistration,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        let _entry = self.enter_wake_scope();
        self.root_widget.sync(&mut self.state.doc, &self.state.root);
        registration.validate_root(self.root_widget.as_ref())?;
        #[cfg(any(test, feature = "test-support"))]
        let controlled_definition = registration.definition;
        self.history = Some(Box::new(crate::history_mode::host::HistoryHost::new(
            registration,
            &self.state.doc,
        )?));
        #[cfg(any(test, feature = "test-support"))]
        {
            self.controlled_installed_history_definition = Some(controlled_definition);
        }
        self.init_layout();
        self.validate_history_registration()
    }

    /// Construct an explicitly detached app for headless or test execution.
    /// This masks any ambient host while setup runs; native launchers use
    /// [`Self::from_factory`], [`Self::try_from_factory`] or [`Self::from_scoped_parts`].
    pub fn new(state: State, root_widget: Box<dyn Widget>) -> Self {
        let _entry = crate::wake::WakeScope::enter_detached();
        Self::with_collaboration_manager(state, root_widget, CollaborationManager::new())
    }

    /// Construct an explicitly detached app with a connected collaboration
    /// manager for headless or test execution.
    ///
    /// The detached context applies during App setup only. It does not adopt
    /// handles captured earlier by `state`, `root_widget`, or the manager.
    pub fn with_collaboration_manager(
        state: State,
        root_widget: Box<dyn Widget>,
        collab_manager: CollaborationManager,
    ) -> Self {
        Self::with_parts(
            None,
            state,
            root_widget,
            AppConstructionInputs::production(collab_manager),
        )
    }

    /// Construct a live app whose root factory captures a fresh owner.
    pub fn from_factory(state: State, factory: impl FnOnce() -> Box<dyn Widget>) -> Self {
        Self::from_factory_with_inputs(state, factory, || {
            AppConstructionInputs::production(CollaborationManager::new())
        })
    }

    fn from_factory_with_inputs(
        state: State,
        factory: impl FnOnce() -> Box<dyn Widget>,
        inputs: impl FnOnce() -> AppConstructionInputs,
    ) -> Self {
        let scope = crate::wake::WakeScope::new();
        let _entry = scope.enter();
        let root = factory();
        Self::from_scoped_parts_with_inputs(scope, state, root, inputs)
    }

    #[cfg(test)]
    fn from_factory_with_controlled_inputs(
        state: State,
        factory: impl FnOnce() -> Box<dyn Widget>,
    ) -> Self {
        Self::from_factory_with_inputs(state, factory, AppConstructionInputs::controlled)
    }

    /// Fallible form of [`Self::from_factory`]. A failed factory closes all
    /// capabilities it created before returning the error.
    ///
    /// # Errors
    ///
    /// Returns the error produced by `factory`.
    pub fn try_from_factory<E>(
        state: State,
        factory: impl FnOnce() -> Result<Box<dyn Widget>, E>,
    ) -> Result<Self, E> {
        let scope = crate::wake::WakeScope::new();
        let _entry = scope.enter();
        let root = factory()?;
        Ok(Self::from_scoped_parts(scope, state, root))
    }

    /// Transfer an existing owner into a live app.
    ///
    /// The caller must enter this exact scope before constructing all state,
    /// models, and widgets passed here. The owner is bound once at native
    /// startup; detached or foreign captures cannot be adopted retroactively.
    pub fn from_scoped_parts(
        wake_scope: crate::wake::WakeScope,
        state: State,
        root_widget: Box<dyn Widget>,
    ) -> Self {
        Self::from_scoped_parts_with_inputs(wake_scope, state, root_widget, || {
            AppConstructionInputs::production(CollaborationManager::new())
        })
    }

    /// Construct a live app without discovering workspace services or identity.
    ///
    /// Local effects and wake ownership remain available. The caller still owns
    /// storage policy; private windows must supply memory-only state/autosave.
    pub fn from_scoped_parts_local(
        wake_scope: crate::wake::WakeScope,
        state: State,
        root_widget: Box<dyn Widget>,
    ) -> Self {
        Self::from_scoped_parts_with_inputs(wake_scope, state, root_widget, || {
            let identity = uuid::Uuid::new_v4();
            let author = crate::permissions::UserId(format!("local:{identity}"));
            let mut presence = PresenceCtx::empty();
            presence.set_own_identity([author.0.clone()]);
            let mut inputs = AppConstructionInputs::production(CollaborationManager::disabled());
            inputs.local_author = ConstructionValue::Ready(author);
            inputs.presence_ctx = ConstructionValue::Ready(presence);
            inputs.surface_id = ConstructionValue::Ready(format!("local-surface:{identity}"));
            inputs.pending_continuation_restore = || None;
            inputs.floating_surface_client = || None;
            inputs.startup_presence_scope = || None;
            inputs
        })
    }

    fn from_scoped_parts_with_inputs(
        wake_scope: crate::wake::WakeScope,
        state: State,
        root_widget: Box<dyn Widget>,
        inputs: impl FnOnce() -> AppConstructionInputs,
    ) -> Self {
        let _entry = wake_scope.enter();
        Self::with_parts(Some(wake_scope), state, root_widget, inputs())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn from_scoped_parts_with_controlled_inputs(
        wake_scope: crate::wake::WakeScope,
        state: State,
        root_widget: Box<dyn Widget>,
    ) -> Self {
        Self::from_scoped_parts_with_inputs(
            wake_scope,
            state,
            root_widget,
            AppConstructionInputs::controlled,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_controlled_inputs(state: State, root_widget: Box<dyn Widget>) -> Self {
        let _entry = crate::wake::WakeScope::enter_detached();
        Self::with_parts(
            None,
            state,
            root_widget,
            AppConstructionInputs::controlled(),
        )
    }

    pub fn enter_wake_scope(&self) -> crate::wake::WakeScopeGuard {
        self.wake_scope.as_ref().map_or_else(
            crate::wake::WakeScope::enter_detached,
            crate::wake::WakeScope::enter,
        )
    }

    fn close_wake_scope(&mut self) {
        if let Some(scope) = self.wake_scope.as_mut() {
            scope.close();
        }
    }

    fn accept_wake(&mut self, event: &crate::wake::WakeEvent) -> bool {
        if !self
            .wake_scope
            .as_ref()
            .is_some_and(|scope| scope.accepts(event))
        {
            return false;
        }
        self.needs_sync |= event.requires_sync();
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "construction explicitly initializes all retained application state before applying startup projections"
    )]
    fn with_parts(
        wake_scope: Option<crate::wake::WakeScope>,
        state: State,
        root_widget: Box<dyn Widget>,
        inputs: AppConstructionInputs,
    ) -> Self {
        let _entry = wake_scope.as_ref().map_or_else(
            crate::wake::WakeScope::enter_detached,
            crate::wake::WakeScope::enter,
        );
        let configured_theme = inputs.configured_theme.resolve();
        let mut app = Self {
            #[cfg(windows)]
            startup_renderer: None,
            wake_scope,
            state,
            root_widget,
            history: None,
            #[cfg(any(test, feature = "test-support"))]
            controlled_installed_history_definition: None,
            framework_input: std::rc::Rc::new(Cell::new(false)),
            history_key_release: false,
            history_focus: None,
            text_renderer: inputs.text_renderer.resolve(),
            palette_source: inputs.palette_source,
            watch_system_settings: inputs.watch_system_settings,
            layout: LayoutRuntime::new(),
            bindings: crate::bound_dispatch::BoundNodeMap::new(),
            component_close_reports: vec![],
            close_admission: crate::close_admission::Admission::default(),
            input: InputRuntime::new(),
            needs_sync: true,
            theme: configured_theme.clone(),
            debug_mode: (inputs.debug_mode)(),
            focused_node: None,
            focusable_order: Vec::new(),
            autofocus_first: false,
            autofocus_on_structure_change: false,
            appearance_reconcile: false,
            theme_store_stamp: (inputs.theme_store_stamp)(),
            last_theme_store_check: Instant::now(),
            last_stable_focus_idx: None,
            last_stable_metadata: None,
            collaboration: CollaborationRuntime::new(
                inputs.collab_manager,
                inputs.local_author.resolve(),
            ),
            presence_ctx: inputs.presence_ctx.resolve(),
            permission_ctx: PermissionContext::permissive(),
            presence: PresenceRuntime::new(),
            accessibility: AccessibilityRuntime::new(configured_theme, (inputs.screen_reader)()),
            fullscreen_requested: false,
            fullscreen_active: false,
            legacy_persistence: crate::launch::LegacyPersistencePolicy::Host,
            persistence: PersistenceRuntime::new(),
            autosave_generation: 0,
            document_routes: crate::history_mode::routes::OwnerDocumentRoutes::default(),
            actions: ActionRuntime::new(),
            document_history: Box::new(crate::standalone_history_host::StandaloneHistoryHost::new()),
            history_chrome_enabled: false,
            history_layout: None,
            history_viewport_width: 1000.0,
            history_focus_pending: false,
            continuation: ContinuationRuntime::new((inputs.pending_continuation_restore)()),
            last_published_playback: None,
            last_playback_publish: None,
            profiler: (inputs.profiler)(),
            native_input_probe: None,
            window_title: None,
            window_app_id: None,
            floating_surface_client: (inputs.floating_surface_client)(),
            last_floating_presentations: std::collections::HashMap::new(),
            last_active_floating_ids: std::collections::HashSet::new(),
            prepared_floating_presentations: std::collections::HashSet::new(),
            floating_coordinate_offsets: std::collections::HashMap::new(),
            suspended_floating_presentations: std::collections::HashSet::new(),
            floating_host_messages: Vec::new(),
        };

        app.accessibility.a11y.high_contrast = (inputs.high_contrast)();
        app.accessibility.a11y.reduce_motion = match app.palette_source {
            PaletteSource::Host => app.accessibility.base_theme.reduce_motion,
            PaletteSource::Owned { .. } => (inputs.reduce_motion)(),
        };
        app.accessibility.a11y.screen_reader = app.accessibility.screen_reader.is_some();
        app.accessibility.text_scale = match app.palette_source {
            PaletteSource::Host => app.text_renderer.text_scale(),
            PaletteSource::Owned { .. } => (inputs.text_scale)(),
        };
        app.accessibility.ui_scale = match app.palette_source {
            PaletteSource::Host => app.accessibility.base_theme.ui_scale,
            PaletteSource::Owned { .. } => (inputs.ui_scale)(),
        };
        app.rebuild_theme();

        // The scope is also what sends `Arrived`, so an app that never set
        // one would never catch up on anything written before it opened.
        // No room means no scope.
        if let Some(scope) = (inputs.startup_presence_scope)() {
            app.set_presence_broadcast_scope(Some(scope));
        }
        app.presence_ctx.set_surface_id(inputs.surface_id.resolve());
        // Apply launcher-provided continuation before the first widget sync or
        // redraw.  Waiting for an input event left a restored app blank on a
        // quiet freshly-created window.
        app.apply_pending_continuation_restore();
        // The first report is level-triggered.  Sending it here makes a
        // launched app visible to continuity capture even if it receives no
        // input events before the next peer asks for a checkpoint.
        app.publish_continuation_metadata();
        app.publish_playback_handoff();
        app
    }

    fn rebuild_theme(&mut self) {
        self.theme = match self.palette_source {
            PaletteSource::Host => {
                self.text_renderer
                    .set_text_scale(self.accessibility.text_scale);
                self.accessibility.text_scale = self.text_renderer.text_scale();
                let base = if self.accessibility.a11y.high_contrast {
                    Theme::high_contrast()
                } else {
                    self.accessibility.base_theme.clone()
                };
                let theme = base
                    .with_component_scales(
                        self.accessibility.text_scale,
                        self.accessibility.ui_scale,
                    )
                    .with_reduce_motion(self.accessibility.a11y.reduce_motion);
                self.accessibility.ui_scale = theme.ui_scale;
                theme
            }
            PaletteSource::Owned { .. } => self.accessibility.base_theme.clone().with_a11y(
                &self.accessibility.a11y,
                self.accessibility.text_scale,
                self.accessibility.ui_scale,
            ),
        };
        // Deliver motion policy before the event loop consults
        // `needs_animation_frame`. This lets custom roots settle their own
        // transitions immediately and prevents a forgotten app-local curve
        // from keeping the compositor awake under reduced motion.
        self.root_widget
            .set_motion_reduced(self.theme.reduce_motion);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the service pump keeps one ordered lifecycle transaction in a single function"
    )]
    fn pump_floating_surface_service(&mut self) -> bool {
        let active_ids = self
            .root_widget
            .active_floating_surface_ids()
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let Some(client) = self.floating_surface_client.as_mut() else {
            let mut changed = false;
            // Outside a Tilo session there is no trusted shell chooser to
            // open. Drain the edge so reconnecting later cannot replay a
            // stale user gesture.
            while self.root_widget.take_capture_studio_request().is_some() {}
            while let Some(request) = self.root_widget.take_floating_surface_request() {
                let message = crate::floating_surface::FloatingSurfaceHostMessage::Reattach {
                    id: request.id,
                };
                self.root_widget.on_floating_surface_host_message(&message);
                self.floating_host_messages.push(message);
                changed = true;
            }
            let _ = self.root_widget.take_floating_surface_source_updates();
            self.last_active_floating_ids = self
                .root_widget
                .active_floating_surface_ids()
                .into_iter()
                .collect();
            return changed;
        };

        let mut changed = false;
        while let Some(mode) = self.root_widget.take_capture_studio_request() {
            let mode = match mode {
                crate::widget::CaptureMode::Screenshot => {
                    crate::floating_surface::CaptureStudioProtocolMode::Screenshot
                }
                crate::widget::CaptureMode::Recording => {
                    crate::floating_surface::CaptureStudioProtocolMode::Recording
                }
                crate::widget::CaptureMode::Pip => {
                    crate::floating_surface::CaptureStudioProtocolMode::PictureInPicture
                }
            };
            if let Err(error) = client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::OpenCaptureStudio { mode },
            ) {
                tracing::warn!(%error, "shell capture-studio request failed");
                self.floating_surface_client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
            changed = true;
        }
        while let Some(request) = self.root_widget.take_floating_surface_request() {
            self.suspended_floating_presentations.remove(&request.id);
            let message = crate::floating_surface::FloatingSurfaceClientMessage::Present {
                request: (&request).into(),
            };
            if let Err(error) = client.send(&message) {
                tracing::warn!(%error, "floating-surface service send failed; keeping app UI local");
                self.floating_surface_client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
        }
        for (id, region) in self.root_widget.take_floating_surface_source_updates() {
            if let Err(error) = client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::UpdateRegion {
                    id,
                    region: crate::floating_surface::protocol_rect_array(region),
                },
            ) {
                tracing::warn!(%error, "floating-surface source update failed");
                self.floating_surface_client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
        }

        // Generic live mirrors have no `FloatingSurfacePresentation`: their
        // pixels live in the shell capture host. The stable activity-ID edge
        // is therefore their authoritative close/toggle signal.
        for id in generic_floating_ids_to_dismiss(
            &self.last_active_floating_ids,
            &active_ids,
            &self.last_floating_presentations,
            &self.suspended_floating_presentations,
        ) {
            if let Err(error) =
                client.send(&crate::floating_surface::FloatingSurfaceClientMessage::Dismiss { id })
            {
                tracing::warn!(%error, "floating-surface dismiss failed");
                self.floating_surface_client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
        }

        let presentations = self
            .root_widget
            .floating_surface_presentations()
            .into_iter()
            .map(|presentation| (presentation.id.clone(), presentation))
            .collect::<std::collections::HashMap<_, _>>();
        for (id, presentation) in &presentations {
            if !self.prepared_floating_presentations.contains(id) {
                continue;
            }
            let changed = self
                .last_floating_presentations
                .get(id)
                .is_none_or(|previous| previous != presentation);
            if changed
                && let Err(error) = client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::SurfaceGeometry {
                        id: id.clone(),
                        rect: crate::floating_surface::protocol_rect_array(
                            presentation.window_rect,
                        ),
                        content_rect: crate::floating_surface::protocol_rect_array(
                            presentation.content_rect,
                        ),
                    },
                )
            {
                tracing::warn!(%error, "floating-surface geometry send failed");
                self.floating_surface_client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
            if presentation.returning_to_app && !self.suspended_floating_presentations.contains(id)
            {
                self.suspended_floating_presentations.insert(id.clone());
                if let Err(error) = client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::Suspend {
                        id: id.clone(),
                        rect: crate::floating_surface::protocol_rect_array(
                            presentation.window_rect,
                        ),
                        content_rect: crate::floating_surface::protocol_rect_array(
                            presentation.content_rect,
                        ),
                    },
                ) {
                    tracing::warn!(%error, "floating-surface return handoff failed");
                    self.floating_surface_client = None;
                    self.reattach_after_floating_service_loss();
                    return changed;
                }
            }
        }
        for id in self.last_floating_presentations.keys() {
            if presentations.contains_key(id) {
                continue;
            }
            if self.suspended_floating_presentations.contains(id) {
                continue;
            }
            let Some(previous) = self.last_floating_presentations.get(id) else {
                continue;
            };
            let message = if active_ids.contains(id) {
                self.suspended_floating_presentations.insert(id.clone());
                crate::floating_surface::FloatingSurfaceClientMessage::Suspend {
                    id: id.clone(),
                    rect: crate::floating_surface::protocol_rect_array(previous.window_rect),
                    content_rect: crate::floating_surface::protocol_rect_array(
                        previous.content_rect,
                    ),
                }
            } else {
                crate::floating_surface::FloatingSurfaceClientMessage::Dismiss { id: id.clone() }
            };
            if let Err(error) = client.send(&message) {
                tracing::warn!(%error, "floating-surface lifecycle send failed");
                self.floating_surface_client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
        }
        // The activity can end while its native surface is already suspended,
        // so no presentation edge remains to trigger permanent cleanup.
        let ended_suspended = self
            .suspended_floating_presentations
            .iter()
            .filter(|id| !active_ids.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in ended_suspended {
            if let Err(error) = client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::Dismiss { id: id.clone() },
            ) {
                tracing::warn!(%error, "floating-surface dismiss send failed");
                self.floating_surface_client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
            self.suspended_floating_presentations.remove(&id);
        }
        let geometry_changed = presentations != self.last_floating_presentations;
        self.last_floating_presentations = presentations;

        let messages = match client.pump() {
            Ok(messages) => messages,
            Err(error) => {
                tracing::warn!(%error, "floating-surface service disconnected; keeping app UI local");
                self.floating_surface_client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
        };
        for message in messages {
            match &message {
                crate::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                    id,
                    coordinate_offset,
                    ..
                } => {
                    self.floating_coordinate_offsets.insert(
                        id.clone(),
                        vello::kurbo::Vec2::new(coordinate_offset[0], coordinate_offset[1]),
                    );
                    if self.prepared_floating_presentations.insert(id.clone()) {
                        // Force one geometry publication on the next pump,
                        // even when the pixels did not move while preparing.
                        self.last_floating_presentations.remove(id);
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Adopted { id, .. } => {
                    if self.prepared_floating_presentations.insert(id.clone()) {
                        self.last_floating_presentations.remove(id);
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Reattach { id }
                | crate::floating_surface::FloatingSurfaceHostMessage::Closed { id } => {
                    self.prepared_floating_presentations.remove(id);
                    self.floating_coordinate_offsets.remove(id);
                    self.suspended_floating_presentations.remove(id);
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Suspended { id } => {
                    self.prepared_floating_presentations.remove(id);
                    self.floating_coordinate_offsets.remove(id);
                    self.suspended_floating_presentations.insert(id.clone());
                }
                crate::floating_surface::FloatingSurfaceHostMessage::DesktopBounds { .. } => {}
            }
            if let crate::floating_surface::FloatingSurfaceHostMessage::DesktopBounds { bounds } =
                &message
            {
                match crate::floating_surface::protocol_rect(*bounds) {
                    Ok(bounds) => {
                        self.root_widget.set_floating_surface_bounds(bounds);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "shell sent invalid floating-surface bounds");
                    }
                }
            }
            self.root_widget.on_floating_surface_host_message(&message);
            self.floating_host_messages.push(message);
            changed = true;
        }
        self.last_active_floating_ids = self
            .root_widget
            .active_floating_surface_ids()
            .into_iter()
            .collect();
        changed || geometry_changed
    }

    fn reattach_after_floating_service_loss(&mut self) {
        let ids = self
            .last_floating_presentations
            .keys()
            .chain(self.suspended_floating_presentations.iter())
            .cloned()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.last_floating_presentations.clear();
        self.last_active_floating_ids.clear();
        self.prepared_floating_presentations.clear();
        self.floating_coordinate_offsets.clear();
        self.suspended_floating_presentations.clear();
        for id in ids {
            let message = crate::floating_surface::FloatingSurfaceHostMessage::Reattach { id };
            self.root_widget.on_floating_surface_host_message(&message);
            self.floating_host_messages.push(message);
        }
    }

    fn take_floating_host_messages(
        &mut self,
    ) -> Vec<crate::floating_surface::FloatingSurfaceHostMessage> {
        std::mem::take(&mut self.floating_host_messages)
    }

    /// Re-express a held pointer delivered through the primary app window in
    /// desktop space while a prepared floating presentation is still active.
    /// This is the transfer-proxy interval: pixels already live in the shell,
    /// but the native secondary may not have committed and taken pointer focus
    /// yet. Once the root reattaches, its presentation disappears immediately,
    /// so the very next primary event remains app-local as usual.
    fn primary_floating_pointer_event(
        &self,
        event: &WindowEvent,
        scale_factor: f64,
    ) -> Option<WindowEvent> {
        if !self.root_widget.wants_pointer_capture() {
            return None;
        }
        let offset = self
            .root_widget
            .floating_surface_presentations()
            .iter()
            .find_map(|presentation| self.floating_coordinate_offsets.get(&presentation.id))?;
        let dx = offset.x * scale_factor;
        let dy = offset.y * scale_factor;
        match event {
            WindowEvent::CursorMoved {
                device_id,
                position,
            } => Some(WindowEvent::CursorMoved {
                device_id: *device_id,
                position: PhysicalPosition::new(position.x + dx, position.y + dy),
            }),
            WindowEvent::Touch(touch) => {
                let mut translated = *touch;
                translated.location.x += dx;
                translated.location.y += dy;
                Some(WindowEvent::Touch(translated))
            }
            _ => None,
        }
    }

    /// Lets the hosted app register its action types, e.g.
    /// `app.action_registry_mut().register::<TubeAction>("tube.action")`.
    pub fn action_registry_mut(&mut self) -> &mut tilo_actions::ActionRegistry {
        self.invalidate_widget_resource_cache();
        &mut self.actions.action_registry
    }

    pub fn set_action_transport(&mut self, transport: Box<dyn tilo_actions::ActionTransport>) {
        let _entry = self.enter_wake_scope();
        self.actions.action_transport = transport;
    }

    fn resource_cache_key(&self) -> ResourceCacheKey {
        ResourceCacheKey {
            mutation_generation: self.actions.resource_generation.get(),
        }
    }

    /// Invalidate declarations derived from document or widget-local state.
    fn invalidate_widget_resource_cache(&mut self) {
        // At `u64::MAX` there is no fresh key to issue. Eviction still makes
        // each later materialisation a new snapshot, so it cannot alias a
        // retained tree that happened to have the same terminal generation.
        if let Some(next) = self.actions.resource_generation.get().checked_add(1) {
            self.actions.resource_generation.set(next);
        }
        self.actions.action_directory_snapshot = None;
        self.actions.action_directory_stamp = None;
        self.actions.action_directory_cursors.clear();
        self.actions.resource_cache.get_mut().take();
        self.actions.local_resource_cache.get_mut().take();
        self.actions.open_resource_search_cache.get_mut().take();
        self.actions.last_control_availability = None;
    }

    fn trace_resource_stage(
        &self,
        stage: crate::profiling::resource_trace::Stage,
    ) -> crate::profiling::resource_trace::Span {
        crate::profiling::resource_trace::Span::start(
            stage,
            self.actions.resource_generation.get(),
            self.actions.permission_generation.get(),
        )
    }

    /// Materialize declarations without walking the semantic catalogue.
    fn ensure_declared_resource_cache(&self) -> ResourceCacheKey {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        let key = self.resource_cache_key();
        if self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| cache.key == key)
        {
            return key;
        }
        let trace = self.trace_resource_stage(Stage::DeclarationGeneration);
        let mut declared = Vec::new();
        crate::bound_dispatch::collect_unbound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &mut declared,
        );
        trace.finish(CacheResult::NotApplicable);
        *self.actions.resource_cache.borrow_mut() = Some(ResourceCache {
            key: key.clone(),
            declared,
            complete: None,
        });
        // A new declaration generation invalidates both local projections.
        self.actions.local_resource_cache.borrow_mut().take();
        key
    }

    fn ensure_resource_cache(&self) -> ResourceCacheKey {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        let key = self.resource_cache_key();
        let trace = self.trace_resource_stage(Stage::ResourceCache);
        if self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| cache.key == key && cache.complete.is_some())
        {
            trace.finish(CacheResult::Hit);
            return key;
        }
        trace.finish(CacheResult::Miss);
        let key = self.ensure_declared_resource_cache();
        let trace = self.trace_resource_stage(Stage::SemanticConstruction);
        let mut complete = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .map_or_else(Vec::new, |cache| cache.declared.clone());
        let mut semantics = crate::a11y_tree::SemanticTree::new();
        semantics.refresh_bound(
            self.root_widget.as_ref(),
            &self.layout.node_map,
            self.focused_node,
            &[],
        );
        complete.extend(semantics.action_resources());
        trace.finish(CacheResult::NotApplicable);
        let trace = self.trace_resource_stage(Stage::SurfaceValidation);
        let mut operation_handlers = self.root_widget.resource_operation_handlers();
        operation_handlers.insert(crate::a11y_tree::SEMANTIC_OPERATION_TYPE.to_string());
        let violations = tilo_actions::validate_surface(
            &complete,
            &self.actions.action_registry,
            &operation_handlers,
        );
        if !violations.is_empty() {
            for violation in violations {
                tracing::error!(
                    address = %violation.address,
                    reason = %violation.reason,
                    "refusing an invalid resource/action surface"
                );
            }
            complete.clear();
        }
        trace.finish(CacheResult::NotApplicable);
        if let Some(cache) = self
            .actions
            .resource_cache
            .borrow_mut()
            .as_mut()
            .filter(|cache| cache.key == key)
        {
            cache.complete = Some(complete);
        }
        // Completing this generation must retain its already-filtered allowed set.
        key
    }

    fn collect_declared_resources(&self) -> Vec<tilo_actions::Resource> {
        let key = self.ensure_resource_cache();
        self.actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .map_or_else(Vec::new, |cache| cache.declared.clone())
    }

    fn ensure_local_control_cache(&self) -> (ResourceCacheKey, u64) {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        fn collect_addresses(
            resources: &[tilo_actions::Resource],
            allowed: &mut std::collections::HashSet<tilo_actions::ActionAddress>,
        ) {
            for resource in resources {
                allowed.extend(resource.actions.iter().map(|action| {
                    tilo_actions::ActionAddress::new(resource.id.as_str(), &action.id)
                }));
                collect_addresses(&resource.children, allowed);
            }
        }

        let key = self.ensure_declared_resource_cache();
        let permission_generation = self.actions.permission_generation.get();
        let trace = self.trace_resource_stage(Stage::LocalCache);
        if self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| {
                cache.key == key && cache.permission_generation == permission_generation
            })
        {
            trace.finish(CacheResult::Hit);
            return (key, permission_generation);
        }
        trace.finish(CacheResult::Miss);
        let trace = self.trace_resource_stage(Stage::LocalPermissionProjection);
        let declared = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .map_or_else(Vec::new, |cache| cache.declared.clone());
        let allowed_resources = declared
            .into_iter()
            .filter_map(|resource| self.filter_resource_for_local_actor(resource))
            .collect::<Vec<_>>();
        let mut allowed = std::collections::HashSet::new();
        collect_addresses(&allowed_resources, &mut allowed);
        *self.actions.local_resource_cache.borrow_mut() = Some(LocalResourceCache {
            key: key.clone(),
            permission_generation,
            resources: None,
            allowed,
        });
        trace.finish(CacheResult::NotApplicable);
        (key, permission_generation)
    }

    fn ensure_local_resource_cache(&self) -> (ResourceCacheKey, u64) {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        // Every full consumer still constructs and validates the combined surface.
        self.ensure_resource_cache();
        let (key, permission_generation) = self.ensure_local_control_cache();
        let trace = self.trace_resource_stage(Stage::LocalCache);
        if self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| {
                cache.key == key
                    && cache.permission_generation == permission_generation
                    && cache.resources.is_some()
            })
        {
            trace.finish(CacheResult::Hit);
            return (key, permission_generation);
        }
        trace.finish(CacheResult::Miss);
        let trace = self.trace_resource_stage(Stage::LocalPermissionProjection);
        let complete = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .and_then(|cache| cache.complete.clone())
            .unwrap_or_default();
        let resources = complete
            .into_iter()
            .filter_map(|resource| self.filter_resource_for_local_actor(resource))
            .collect();
        if let Some(cache) = self
            .actions
            .local_resource_cache
            .borrow_mut()
            .as_mut()
            .filter(|cache| {
                cache.key == key && cache.permission_generation == permission_generation
            })
        {
            cache.resources = Some(resources);
        }
        trace.finish(CacheResult::NotApplicable);
        (key, permission_generation)
    }

    fn collect_local_resources(&self) -> Vec<tilo_actions::Resource> {
        let (key, permission_generation) = self.ensure_local_resource_cache();
        let mut resources = self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| {
                cache.key == key && cache.permission_generation == permission_generation
            })
            .and_then(|cache| cache.resources.clone())
            .unwrap_or_default();
        if let Ok(snapshots) = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        ) {
            resources.extend(
                snapshots
                    .into_iter()
                    .flat_map(|snapshot| snapshot.resources),
            );
        }
        resources
    }

    fn refresh_control_action_availability(&mut self) {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        let trace = self.trace_resource_stage(Stage::BoundDiscovery);
        self.refresh_history_controls();
        let snapshots = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        trace.finish(CacheResult::NotObserved);
        let trace = self.trace_resource_stage(Stage::BoundControlProjection);
        let _ =
            crate::bound_dispatch::refresh_bound_controls(self.root_widget.as_mut(), &snapshots);
        trace.finish(CacheResult::NotApplicable);
        // Only ActionEmitter controls consume this address projection. Semantic,
        // document and legacy controls still use their normal invocation doors.
        // Recheck presence: widget-local changes can add/remove emitters.
        if !self.root_widget.has_action_control_bindings() {
            // A replacement emitter must not inherit a prior refresh stamp.
            self.actions.last_control_availability = None;
            return;
        }
        let cache_key = self.ensure_local_control_cache();
        let trace = self.trace_resource_stage(Stage::ControlCache);
        if self.actions.last_control_availability.as_ref() == Some(&cache_key) {
            trace.finish(CacheResult::Hit);
            return;
        }
        trace.finish(CacheResult::Miss);
        let trace = self.trace_resource_stage(Stage::ControlRefresh);
        // Clone only the compact address set so the cache borrow does not
        // span a mutable call into the retained widget tree.
        let allowed = self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .map_or_else(std::collections::HashSet::new, |cache| {
                cache.allowed.clone()
            });
        self.root_widget
            .refresh_control_action_availability(&allowed);
        self.actions.last_control_availability = Some(cache_key);
        trace.finish(CacheResult::NotApplicable);
    }

    /// The complete machine-driveable surface: domain resources declared by
    /// the app plus ordinary controls projected from its retained semantic
    /// tree. Ctrl+K, scripts, and agents therefore consume one catalogue.
    pub fn collect_resources(&self) -> Vec<tilo_actions::Resource> {
        let _entry = self.enter_wake_scope();
        let key = self.ensure_resource_cache();
        let mut resources = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .and_then(|cache| cache.complete.clone())
            .unwrap_or_default();
        if let Ok(snapshots) = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        ) {
            resources.extend(
                snapshots
                    .into_iter()
                    .flat_map(|snapshot| snapshot.resources),
            );
        }
        resources
    }

    /// Audit every retained control binding against the fresh resource tree.
    /// This is public so development tooling can turn remaining explicit
    /// legacy controls and stale/duplicate action addresses into a coverage
    /// report without inspecting widget implementation details.
    pub fn control_action_coverage(&self) -> Vec<ControlBindingCoverage> {
        let _entry = self.enter_wake_scope();
        let resources = self.collect_resources();
        self.control_action_coverage_for(&resources)
    }

    /// Locate every on-screen affordance for one canonical action and rank
    /// usable, visible, short paths first. Agents still invoke `address`
    /// directly; the chosen route makes the resulting interaction project
    /// into the same UI a person sees without fragile coordinate clicking.
    pub fn action_ui_routes(
        &mut self,
        address: &tilo_actions::ActionAddress,
    ) -> Vec<ActionUiRoute> {
        self.refresh_semantic_tree();
        let mut routes = Vec::new();
        let bindings = self
            .with_native_presentation(|root, _, _, _| root.control_action_bindings())
            .unwrap_or_default();
        for binding in bindings {
            for path in self
                .accessibility
                .semantic_tree
                .control_paths(&binding.control_id)
            {
                let Some(semantic_id) = path.last() else {
                    continue;
                };
                // Match the canonical ResourceId emitted by SemanticTree.
                // Semantic node ids contain punctuation and address delimiters.
                let encoded_node_id = percent_encoding::utf8_percent_encode(
                    semantic_id.as_str(),
                    percent_encoding::NON_ALPHANUMERIC,
                );
                let semantic_address = address.resource == format!("ui:{encoded_node_id}")
                    && self
                        .accessibility
                        .semantic_tree
                        .action(semantic_id, &address.action)
                        .is_some();
                let matches_address = semantic_address
                    || matches!(
                        &binding.kind,
                        crate::widget::ControlBindingKind::Action(candidate) if candidate == address
                    );
                if !matches_address {
                    continue;
                }
                let Some(node) = self.accessibility.semantic_tree.node(semantic_id) else {
                    continue;
                };
                routes.push(ActionUiRoute {
                    address: address.clone(),
                    control_id: binding.control_id.clone(),
                    path,
                    bounds: node.bounds,
                    focused: node.state.focused,
                    disabled: node.state.disabled,
                });
            }
        }
        routes.sort_by(|left, right| {
            left.disabled
                .cmp(&right.disabled)
                .then_with(|| left.bounds.is_none().cmp(&right.bounds.is_none()))
                .then_with(|| left.path.len().cmp(&right.path.len()))
                .then_with(|| right.focused.cmp(&left.focused))
                .then_with(|| left.control_id.cmp(&right.control_id))
        });
        routes.dedup_by(|left, right| left.path.last() == right.path.last());
        routes
    }

    pub fn preferred_action_ui_route(
        &mut self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<ActionUiRoute> {
        self.action_ui_routes(address).into_iter().next()
    }

    fn control_action_coverage_for(
        &self,
        resources: &[tilo_actions::Resource],
    ) -> Vec<ControlBindingCoverage> {
        let mut seen = std::collections::HashSet::new();
        self.root_widget
            .control_action_bindings()
            .into_iter()
            .map(|binding| {
                let status = match &binding.kind {
                    crate::widget::ControlBindingKind::Action(address) => {
                        if tilo_actions::door::resolve(resources, address).is_none() {
                            ControlBindingStatus::StaleAddress
                        } else if !seen.insert(address.clone()) {
                            ControlBindingStatus::SharedAddress
                        } else {
                            ControlBindingStatus::Resolved
                        }
                    }
                    crate::widget::ControlBindingKind::CollaborativeDocument(_) => {
                        ControlBindingStatus::CollaborativeDocument
                    }
                    crate::widget::ControlBindingKind::SemanticAction(_) => {
                        ControlBindingStatus::SemanticAction
                    }
                    crate::widget::ControlBindingKind::LegacyDocumentPath(_)
                    | crate::widget::ControlBindingKind::LegacyCallback => {
                        ControlBindingStatus::ExplicitLegacy
                    }
                };
                ControlBindingCoverage { binding, status }
            })
            .collect()
    }

    fn invoke_semantic_operation_encoded(
        &mut self,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<bool, String> {
        if encoded.type_id != crate::a11y_tree::SEMANTIC_OPERATION_TYPE {
            return Ok(false);
        }
        let operation: crate::a11y_tree::SemanticOperation =
            serde_json::from_slice(&encoded.payload).map_err(|error| error.to_string())?;
        self.refresh_semantic_tree();
        self.invoke_semantic_action(
            &crate::a11y_tree::SemanticId(operation.node),
            &operation.action,
            operation.value.as_deref(),
        )
    }

    fn bound_broker_route(&self) -> Option<&crate::collaboration::PrimaryRouteWitness> {
        #[cfg(test)]
        if let Some(route) = &self.actions.bound_broker_route_for_test {
            return Some(route);
        }
        self.collaboration.manager.primary_route()
    }
    fn publication_app_id(&self) -> String {
        if crate::bound_dispatch::has_matching_broker_component(
            self.root_widget.as_ref(),
            self.bound_broker_route(),
        ) && let Some(witness) = self.bound_broker_route()
        {
            return witness.route.app_id.clone();
        }
        crate::collaboration::local_app_id()
    }
    fn require_bound_broker_route(
        &self,
        binding: &crate::bound_dispatch::CallbackBinding,
    ) -> Result<(), tilo_actions::DoorError> {
        if binding.matches_broker_route(self.bound_broker_route()) {
            return Ok(());
        }
        Err(tilo_actions::DoorError::Unavailable {
            code: "bound_broker_route_unavailable".into(),
            reason:
                "This component has no matching admitted app, workspace and record action route"
                    .into(),
        })
    }

    /// Transport-only projection. Each component is validated with its own
    /// registry before its domain resource is assigned an opaque publication alias.
    fn publication_resources(&mut self) -> Vec<tilo_actions::Resource> {
        fn remove_reserved(mut resource: tilo_actions::Resource) -> Option<tilo_actions::Resource> {
            if resource
                .id
                .as_str()
                .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
            {
                return None;
            }
            resource.children = resource
                .children
                .into_iter()
                .filter_map(remove_reserved)
                .collect();
            Some(resource)
        }
        let key = self.ensure_resource_cache();
        let mut resources = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .and_then(|cache| cache.complete.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(remove_reserved)
            .collect::<Vec<_>>();
        let components = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        for component in components {
            if self.require_bound_broker_route(&component.binding).is_err() {
                continue;
            }
            if !crate::bound_dispatch::validate_bound_resources(
                self.root_widget.as_mut(),
                &component.binding,
                &component.resources,
            )
            .unwrap_or(false)
            {
                continue;
            }
            for resource in component.resources {
                if let Ok(resource) = self
                    .actions
                    .bound_publication
                    .project(&component.binding, resource)
                {
                    resources.push(resource);
                }
            }
        }
        resources
    }

    /// Publish the live resource/action tree through the session broker.
    /// Level-triggered at the wire, diffed here so an unchanged frame emits
    /// nothing; publishing an empty list explicitly withdraws a closed tree.
    fn publish_action_surface(&mut self) {
        #[cfg(test)]
        {
            self.actions.action_surface_publication_attempts = self
                .actions
                .action_surface_publication_attempts
                .saturating_add(1);
        }
        if !self.collaboration.manager.is_connected() {
            return;
        }
        let route = self.bound_broker_route().cloned();
        if self.actions.last_published_bound_route != route {
            self.actions.last_published_actions = None;
            self.actions.action_directory_snapshot = None;
            self.actions.last_published_bound_route = route;
        }
        let publication_app_id = self.publication_app_id();
        // This is independent of the legacy eager surface below.  The
        // broker owns the capability and asks the UI thread to materialise a
        // bounded page; large apps therefore do not become a flattened RPC
        // catalogue just because they are visible in shell search.
        self.collaboration
            .manager
            .publish_action_directory(publication_app_id.clone());
        let resources = self.publication_resources();
        // Use this exact snapshot for coverage as well as publication. That
        // prevents recursive/repeated resource collection and makes the audit
        // correspond to the catalogue about to cross the wire.
        let binding_coverage = self.control_action_coverage_for(&self.collect_resources());
        if binding_coverage
            .iter()
            .any(|entry| entry.status == ControlBindingStatus::StaleAddress)
        {
            self.actions.action_directory_snapshot = None;
            self.actions.action_directory_stamp = None;
            self.actions.action_directory_cursors.clear();
            for entry in binding_coverage
                .iter()
                .filter(|entry| entry.status == ControlBindingStatus::StaleAddress)
            {
                tracing::error!(
                    control = %entry.binding.control_id,
                    binding = ?entry.binding.kind,
                    "refusing to publish a surface with a stale control action binding"
                );
            }
            if self
                .actions
                .last_published_actions
                .as_ref()
                .is_some_and(|actions| !actions.is_empty())
            {
                self.collaboration
                    .manager
                    .send(CollabRequest::PublishActions {
                        app_id: publication_app_id,
                        actions: Vec::new(),
                    });
                self.actions.last_published_actions = Some(Vec::new());
            }
            return;
        }
        let mut operation_handlers = self.root_widget.resource_operation_handlers();
        operation_handlers.insert(crate::a11y_tree::SEMANTIC_OPERATION_TYPE.to_string());
        // Bound components have already been validated against their own registries
        // by publication_resources. Only legacy resources use the host registry.
        let unbound_resources = resources
            .iter()
            .filter(|resource| {
                !resource
                    .id
                    .as_str()
                    .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
            })
            .cloned()
            .collect::<Vec<_>>();
        let violations = tilo_actions::validate_surface(
            &unbound_resources,
            &self.actions.action_registry,
            &operation_handlers,
        );
        if !violations.is_empty() {
            for violation in &violations {
                tracing::error!(
                    address = %violation.address,
                    reason = %violation.reason,
                    "refusing to publish an invalid resource/action surface"
                );
            }
            // Withdraw anything previously published. A stale, apparently
            // valid catalogue is more dangerous than an empty one.
            if self
                .actions
                .last_published_actions
                .as_ref()
                .is_none_or(Vec::is_empty)
            {
                return;
            }
            self.collaboration
                .manager
                .send(CollabRequest::PublishActions {
                    app_id: crate::collaboration::local_app_id(),
                    actions: Vec::new(),
                });
            self.actions.last_published_actions = Some(Vec::new());
            return;
        }
        let descriptors = tilo_actions::door::survey(&resources);
        self.capture_action_directory_snapshot(&resources, &descriptors);
        if self.actions.last_published_actions.as_ref() == Some(&descriptors) {
            return;
        }
        let actions = descriptors.iter().map(Self::exported_action).collect();
        self.collaboration
            .manager
            .send(CollabRequest::PublishActions {
                app_id: publication_app_id,
                actions,
            });
        self.actions.last_published_actions = Some(descriptors);
    }

    fn exported_action(
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> crate::collaboration::ExportedAction {
        crate::collaboration::ExportedAction {
            resource: descriptor.address.resource.clone(),
            action: descriptor.address.action.clone(),
            resource_kind: descriptor.resource_kind.clone(),
            resource_label: descriptor.resource_label.clone(),
            action_label: descriptor.action_label.clone(),
            description: descriptor.description.clone(),
            operation_kind: descriptor.kind.as_str().to_string(),
            params_json: serde_json::to_string(&descriptor.params).unwrap_or_default(),
            contract_json: serde_json::to_string(&descriptor.contract).unwrap_or_default(),
            input: match &descriptor.input {
                tilo_actions::InputSpec::None => crate::collaboration::ExportedInput::None,
                tilo_actions::InputSpec::Text {
                    prompt,
                    placeholder,
                    collects,
                } => crate::collaboration::ExportedInput::Text {
                    prompt: prompt.clone(),
                    placeholder: placeholder.clone(),
                    wants_content: *collects == tilo_actions::Collects::Content,
                },
            },
        }
    }

    fn action_directory_revision(&mut self) -> String {
        let owners =
            crate::bound_dispatch::directory_witness(self.root_widget.as_ref(), &self.state.doc);
        let reusable = owners
            .as_ref()
            .is_ok_and(|owners| owners.iter().all(|owner| owner.generation.is_some()));
        let witness = action_directory_snapshot::DirectoryWitness {
            key: self.resource_cache_key(),
            permissions: self.permission_ctx.clone(),
            permission_generation: self.actions.permission_generation.get(),
            route: self.bound_broker_route().cloned(),
            owners: owners.unwrap_or_default(),
            root_revision: self.root_widget.action_directory_revision(&self.state.doc),
            providers: crate::bound_dispatch::search_providers(self.root_widget.as_ref())
                .into_iter()
                .map(|provider| {
                    format!(
                        "{}:{:?}:{:?}",
                        provider.name(),
                        provider
                            .callback_binding()
                            .map(|binding| binding.lifetime_id),
                        provider.action_directory_generation()
                    )
                })
                .collect(),
        };
        if reusable
            && let Some((old, revision)) = &self.actions.action_directory_stamp
            && old == &witness
        {
            return revision.clone();
        }
        // Generations are updated at mutation boundaries; reading Automerge
        // heads here would commit an open transaction and split an undo unit.
        self.actions.action_directory_snapshot = None;
        self.actions.action_directory_cursors.clear();
        let revision = format!("v5:{}", uuid::Uuid::new_v4());
        self.actions.action_directory_stamp = Some((witness, revision.clone()));
        revision
    }

    fn exported_directory_action(
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> runtime_collab::ExportedAction {
        runtime_collab::ExportedAction {
            resource: descriptor.address.resource.clone(),
            action: descriptor.address.action.clone(),
            resource_kind: descriptor.resource_kind.clone(),
            resource_label: descriptor.resource_label.clone(),
            action_label: descriptor.action_label.clone(),
            description: descriptor.description.clone(),
            operation_kind: descriptor.kind.as_str().to_string(),
            params_json: serde_json::to_string(&descriptor.params).unwrap_or_default(),
            contract_json: serde_json::to_string(&descriptor.contract).unwrap_or_default(),
            input: match &descriptor.input {
                tilo_actions::InputSpec::None => runtime_collab::ExportedInput::None,
                tilo_actions::InputSpec::Text {
                    prompt,
                    placeholder,
                    collects,
                } => runtime_collab::ExportedInput::Text {
                    prompt: prompt.clone(),
                    placeholder: placeholder.clone(),
                    wants_content: *collects == tilo_actions::Collects::Content,
                },
            },
        }
    }

    fn directory_explicit_visibility(
        &self,
        resource: &tilo_actions::Resource,
    ) -> Option<tilo_actions::ActionAddress> {
        let target = self
            .actions
            .bound_publication
            .target(&tilo_actions::ActionAddress::new(resource.id.as_str(), ""))
            .ok()
            .flatten();
        crate::bound_dispatch::publication_visibility(
            self.root_widget.as_ref(),
            &self.state.doc,
            target.as_ref().map(|target| &target.binding),
            target.as_ref().map_or(resource.id.as_str(), |target| {
                target.address.address.resource.as_str()
            }),
        )
        .and_then(|mut address| {
            if let Some(target) = &target {
                address.resource = self
                    .actions
                    .bound_publication
                    .alias(&target.binding, &address.resource)
                    .ok()?;
            }
            Some(address)
        })
    }

    fn directory_resource_row(
        &self,
        resource: &tilo_actions::Resource,
        parent_resource: &str,
        may_descend: bool,
    ) -> Option<runtime_collab::ActionDirectoryRow> {
        // Preserve the explicit-or-View disclosure rule, finding only the
        // needed descriptor in this bounded provider result, not surveying a
        // complete action catalogue for each displayed row.
        fn visibility(
            resource: &tilo_actions::Resource,
            explicit: Option<&tilo_actions::ActionAddress>,
        ) -> Option<tilo_actions::ActionDescriptor> {
            resource
                .actions
                .iter()
                .find(|action| match explicit {
                    Some(address) => {
                        address.resource == resource.id.as_str() && address.action == action.id
                    }
                    None => action.kind == tilo_actions::OperationKind::View,
                })
                .map(|action| App::directory_descriptor(resource, action))
                .or_else(|| {
                    resource
                        .children
                        .iter()
                        .find_map(|child| visibility(child, explicit))
                })
        }
        let explicit = self.directory_explicit_visibility(resource);
        let visibility = visibility(resource, explicit.as_ref())?;
        Some(runtime_collab::ActionDirectoryRow {
            parent_resource: parent_resource.to_owned(),
            resource: resource.id.as_str().to_owned(),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            resource_presentation_json: resource
                .presentation
                .as_ref()
                .filter(|presentation| presentation.is_valid())
                .and_then(|presentation| serde_json::to_string(presentation).ok())
                .unwrap_or_default(),
            action: None,
            descriptor: None,
            has_children: may_descend
                && (!resource.children.is_empty() || !resource.actions.is_empty()),
            visibility_action: Some(visibility.address.action),
            visibility_contract_json: serde_json::to_string(&visibility.contract).ok()?,
        })
    }

    fn directory_descriptor(
        resource: &tilo_actions::Resource,
        action: &tilo_actions::ActionSpec,
    ) -> tilo_actions::ActionDescriptor {
        tilo_actions::ActionDescriptor {
            address: tilo_actions::ActionAddress::new(resource.id.as_str(), &action.id),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            action_label: action.label.clone(),
            description: action.description.clone(),
            locus: action.locus,
            kind: action.kind,
            contract: action.contract.clone(),
            input: match &action.input {
                tilo_actions::ActionInput::Immediate(_) => tilo_actions::InputSpec::None,
                tilo_actions::ActionInput::Prompted {
                    prompt,
                    placeholder,
                    collects,
                    ..
                } => tilo_actions::InputSpec::Text {
                    prompt: prompt.clone(),
                    placeholder: placeholder.clone(),
                    collects: *collects,
                },
            },
            params: action.params().to_vec(),
        }
    }

    fn directory_action_row(
        resource: &tilo_actions::Resource,
        action_id: &str,
    ) -> Option<runtime_collab::ActionDirectoryRow> {
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == action_id)?;
        let descriptor = Self::directory_descriptor(resource, action);
        let exported = Self::exported_directory_action(&descriptor);
        Some(runtime_collab::ActionDirectoryRow {
            parent_resource: resource.id.as_str().to_owned(),
            resource: resource.id.as_str().to_owned(),
            resource_kind: exported.resource_kind.clone(),
            resource_label: exported.resource_label.clone(),
            resource_presentation_json: String::new(),
            action: Some(action_id.to_owned()),
            descriptor: Some(exported.clone()),
            has_children: false,
            visibility_action: Some(action_id.to_owned()),
            visibility_contract_json: exported.contract_json,
        })
    }

    fn action_directory_cursor_key(&mut self) -> String {
        let revision = self.action_directory_revision();
        format!("tilo-dir-{}-{revision}", uuid::Uuid::new_v4())
    }

    fn action_directory_next_cursor(&mut self, state: ActionDirectoryCursor) -> Vec<u8> {
        if self.actions.action_directory_cursors.len() >= ACTION_DIRECTORY_MAX_CURSORS {
            // Bound memory even when a remote peer starts many abandoned
            // searches. Old cursors become stale rather than holding a tree.
            self.actions.action_directory_cursors.clear();
        }
        let key = self.action_directory_cursor_key();
        self.actions
            .action_directory_cursors
            .insert(key.clone(), state);
        key.into_bytes()
    }

    #[allow(
        clippy::too_many_lines,
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "every directory arithmetic/index is guarded by the hard page budgets and loop bounds directly above it"
    )]
    fn action_directory_page(
        &mut self,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> runtime_collab::ActionDirectoryPage {
        let revision = self.action_directory_revision();
        let requested_revision = query.revision;
        if !requested_revision.is_empty() && requested_revision != revision {
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: true,
                budget_exhausted: false,
            };
        }
        let max_results = usize::try_from(query.max_results)
            .unwrap_or(ACTION_DIRECTORY_MAX_RESULTS)
            .clamp(1, ACTION_DIRECTORY_MAX_RESULTS);
        let max_work = usize::try_from(query.max_work)
            .unwrap_or(ACTION_DIRECTORY_MAX_WORK)
            .clamp(1, ACTION_DIRECTORY_MAX_WORK);
        let max_bytes = usize::try_from(query.max_bytes)
            .unwrap_or(ACTION_DIRECTORY_MAX_BYTES)
            .clamp(1, ACTION_DIRECTORY_MAX_BYTES);
        let max_depth = query.max_depth.min(ACTION_DIRECTORY_MAX_DEPTH);
        let cursor_key = if query.cursor.is_empty() {
            None
        } else {
            match String::from_utf8(query.cursor) {
                Ok(cursor) => Some(cursor),
                Err(_) => {
                    return runtime_collab::ActionDirectoryPage {
                        revision,
                        rows: Vec::new(),
                        next_cursor: Vec::new(),
                        exhausted: false,
                        stale_cursor: true,
                        budget_exhausted: false,
                    };
                }
            }
        };
        let previous = cursor_key
            .as_deref()
            .and_then(|key| self.actions.action_directory_cursors.remove(key));
        if cursor_key.is_some() && previous.is_none() {
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: true,
                budget_exhausted: false,
            };
        }
        let state = previous.unwrap_or_else(|| ActionDirectoryCursor {
            revision: revision.clone(),
            needle: query.needle.clone(),
            parent_resource: query.parent_resource.clone(),
            provider_index: 0,
            provider_cursor: None,
            browse_offset: 0,
            root_widget_index: 0,
            root_provider_cursor: None,
        });
        if state.revision != revision
            || state.needle != query.needle
            || state.parent_resource != query.parent_resource
        {
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: true,
                budget_exhausted: false,
            };
        }

        let mut rows = Vec::new();
        let mut estimated_bytes = 0usize;
        let add = |row: runtime_collab::ActionDirectoryRow,
                   rows: &mut Vec<runtime_collab::ActionDirectoryRow>,
                   bytes: &mut usize| {
            let descriptor_bytes = row.descriptor.as_ref().map_or(0, |descriptor| {
                let input_bytes = match &descriptor.input {
                    runtime_collab::ExportedInput::None => 0,
                    runtime_collab::ExportedInput::Text {
                        prompt,
                        placeholder,
                        ..
                    } => prompt.len() + placeholder.len() + 8,
                };
                descriptor.resource.len()
                    + descriptor.action.len()
                    + descriptor.resource_kind.len()
                    + descriptor.resource_label.len()
                    + descriptor.action_label.len()
                    + descriptor.description.as_ref().map_or(0, String::len)
                    + descriptor.params_json.len()
                    + descriptor.contract_json.len()
                    + input_bytes
            });
            *bytes += row.parent_resource.len()
                + row.resource.len()
                + row.resource_kind.len()
                + row.resource_label.len()
                + row.resource_presentation_json.len()
                + row.action.as_ref().map_or(0, String::len)
                + row.visibility_action.as_ref().map_or(0, String::len)
                + row.visibility_contract_json.len()
                + descriptor_bytes
                + 64;
            rows.push(row);
        };

        if query.needle.is_empty() {
            if query.parent_resource.is_empty() {
                let mut widget_index = state.root_widget_index;
                let mut provider_cursor = state.root_provider_cursor.clone();
                let mut inspected = 0usize;
                let mut exhausted = false;
                let mut projected_offset = state.browse_offset;
                let mut projected_done = true;
                if let Some(snapshot) = &self.actions.action_directory_snapshot {
                    while let Some(row) = snapshot.roots.get(projected_offset) {
                        if rows.len() >= max_results || inspected >= max_work {
                            break;
                        }
                        let mut row = row.clone();
                        row.has_children &= max_depth > 0;
                        add(row, &mut rows, &mut estimated_bytes);
                        if estimated_bytes > max_bytes {
                            rows.pop();
                            break;
                        }
                        projected_offset = projected_offset.saturating_add(1);
                        inspected = inspected.saturating_add(1);
                    }
                    projected_done = projected_offset >= snapshot.roots.len();
                }
                while projected_done && rows.len() < max_results && inspected < max_work {
                    let Some(page) = crate::widget::action_directory_page_at(
                        self.root_widget.as_ref(),
                        &self.state.doc,
                        widget_index,
                        provider_cursor.as_deref().unwrap_or_default(),
                        1,
                    ) else {
                        exhausted = true;
                        break;
                    };
                    let Some((mut page, binding)) = page else {
                        widget_index = widget_index.saturating_add(1);
                        provider_cursor = None;
                        inspected = inspected.saturating_add(1);
                        continue;
                    };
                    if page.resources.len() > 1 || (!page.exhausted && page.next_cursor.is_empty())
                    {
                        // The declared page contract was violated. A hidden
                        // tail is worse than no remote hierarchy at all.
                        return runtime_collab::ActionDirectoryPage {
                            revision,
                            rows: Vec::new(),
                            next_cursor: Vec::new(),
                            exhausted: false,
                            stale_cursor: false,
                            budget_exhausted: true,
                        };
                    }
                    if let Some(binding) = binding {
                        if self.require_bound_broker_route(&binding).is_ok()
                            && crate::bound_dispatch::validate_bound_resources(
                                self.root_widget.as_mut(),
                                &binding,
                                &page.resources,
                            )
                            .unwrap_or(false)
                        {
                            page.resources = page
                                .resources
                                .into_iter()
                                .filter_map(|resource| {
                                    crate::bound_dispatch::filter_resource(&binding, resource)
                                })
                                .filter_map(|resource| {
                                    self.actions
                                        .bound_publication
                                        .project(&binding, resource)
                                        .ok()
                                })
                                .collect();
                        } else {
                            page.resources.clear();
                        }
                    } else {
                        page.resources.retain(|resource| {
                            !resource
                                .id
                                .as_str()
                                .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
                        });
                    }
                    let before_index = widget_index;
                    let before_cursor = provider_cursor.clone();
                    if let Some(resource) = page.resources.first()
                        && !self.actions.action_directory_snapshot.as_ref().is_some_and(
                            |snapshot| snapshot.nodes.contains_key(resource.id.as_str()),
                        )
                        && let Some(row) = self.directory_resource_row(resource, "", max_depth > 0)
                    {
                        add(row, &mut rows, &mut estimated_bytes);
                        if estimated_bytes > max_bytes {
                            rows.pop();
                            break;
                        }
                    }
                    if page.exhausted {
                        widget_index = widget_index.saturating_add(1);
                        provider_cursor = None;
                    } else {
                        provider_cursor = Some(page.next_cursor);
                    }
                    inspected = inspected.saturating_add(1);
                    if rows.len() >= max_results {
                        break;
                    }
                    // An empty non-exhausted source page may not advance
                    // indefinitely; its opaque cursor must still change.
                    if rows.is_empty()
                        && before_index == widget_index
                        && before_cursor == provider_cursor
                    {
                        return runtime_collab::ActionDirectoryPage {
                            revision,
                            rows: Vec::new(),
                            next_cursor: Vec::new(),
                            exhausted: false,
                            stale_cursor: false,
                            budget_exhausted: true,
                        };
                    }
                }
                let next_cursor = (!exhausted).then(|| {
                    self.action_directory_next_cursor(ActionDirectoryCursor {
                        browse_offset: projected_offset,
                        root_widget_index: widget_index,
                        root_provider_cursor: provider_cursor,
                        ..state
                    })
                });
                return runtime_collab::ActionDirectoryPage {
                    revision,
                    rows,
                    next_cursor: next_cursor.unwrap_or_default(),
                    exhausted,
                    stale_cursor: false,
                    budget_exhausted: !exhausted,
                };
            }
            let mut offset = state.browse_offset;
            let mut inspected = 0usize;
            let mut projected_total = 0usize;
            if let Some(node) = self
                .actions
                .action_directory_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.nodes.get(&query.parent_resource))
            {
                projected_total = node.rows.len();
                while let Some(row) = node.rows.get(offset) {
                    if rows.len() >= max_results || inspected >= max_work {
                        break;
                    }
                    let mut row = row.clone();
                    row.has_children &= max_depth > 0
                        && node.depth.saturating_add(1) < usize::from(ACTION_DIRECTORY_MAX_DEPTH);
                    add(row, &mut rows, &mut estimated_bytes);
                    if estimated_bytes > max_bytes {
                        rows.pop();
                        break;
                    }
                    offset = offset.saturating_add(1);
                    inspected = inspected.saturating_add(1);
                }
            }
            if offset < projected_total || rows.len() >= max_results || inspected >= max_work {
                let next_cursor = self.action_directory_next_cursor(ActionDirectoryCursor {
                    browse_offset: offset,
                    ..state
                });
                return runtime_collab::ActionDirectoryPage {
                    revision,
                    rows,
                    next_cursor,
                    exhausted: false,
                    stale_cursor: false,
                    budget_exhausted: true,
                };
            }
            let parent = self.resolve_publication_resource(&query.parent_resource, "");
            let parent_actions = parent.as_ref().map_or_else(Vec::new, |resource| {
                resource
                    .actions
                    .iter()
                    .map(|action| action.id.clone())
                    .collect()
            });
            let direct = parent
                .as_ref()
                .map_or_else(Vec::new, |resource| resource.children.clone());
            let total = projected_total
                .saturating_add(parent_actions.len())
                .saturating_add(direct.len());
            while offset < total && rows.len() < max_results && inspected < max_work {
                let provider_offset = offset.saturating_sub(projected_total);
                let row = if provider_offset < parent_actions.len() {
                    let action = &parent_actions[provider_offset];
                    let projected =
                        self.actions
                            .action_directory_snapshot
                            .as_ref()
                            .is_some_and(|snapshot| {
                                snapshot
                                    .actions
                                    .contains_key(&tilo_actions::ActionAddress::new(
                                        &query.parent_resource,
                                        action,
                                    ))
                            });
                    if projected {
                        None
                    } else {
                        parent
                            .as_ref()
                            .and_then(|resource| Self::directory_action_row(resource, action))
                    }
                } else {
                    let resource = &direct[provider_offset - parent_actions.len()];
                    if self
                        .actions
                        .action_directory_snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.nodes.contains_key(resource.id.as_str()))
                    {
                        None
                    } else {
                        self.directory_resource_row(resource, &query.parent_resource, max_depth > 0)
                    }
                };
                if let Some(row) = row {
                    add(row, &mut rows, &mut estimated_bytes);
                    if estimated_bytes > max_bytes {
                        rows.pop();
                        break;
                    }
                }
                offset += 1;
                inspected += 1;
            }
            let exhausted = offset >= total;
            let next_cursor = (!exhausted).then(|| {
                self.action_directory_next_cursor(ActionDirectoryCursor {
                    browse_offset: offset,
                    ..state
                })
            });
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows,
                next_cursor: next_cursor.unwrap_or_default(),
                exhausted,
                stale_cursor: false,
                budget_exhausted: !exhausted,
            };
        }

        let providers = crate::bound_dispatch::search_providers(self.root_widget.as_ref())
            .into_iter()
            .filter(|provider| provider.action_directory_generation().is_some())
            .collect::<Vec<_>>();
        let mut provider_index = state.provider_index;
        let mut provider_cursor = state.provider_cursor;
        let mut work_left = max_work;
        let mut byte_exhausted = false;
        while provider_index < providers.len() && rows.len() < max_results && work_left > 0 {
            if provider_cursor.as_ref().is_some_and(|cursor| {
                !providers[provider_index].action_directory_cursor_is_current(cursor)
            }) {
                return runtime_collab::ActionDirectoryPage {
                    revision,
                    rows: Vec::new(),
                    next_cursor: Vec::new(),
                    exhausted: false,
                    stale_cursor: true,
                    budget_exhausted: false,
                };
            }
            // Request one row at a time.  The provider's opaque cursor is
            // after its returned page, so asking for a larger page and then
            // stopping at the byte ceiling would silently skip the unfit tail.
            // This preserves every canonical result while still bounding this
            // request to `max_results` provider steps.
            let room = 1;
            let page = providers[provider_index].search_page(
                &query.needle,
                provider_cursor.as_ref(),
                work_left,
                room,
            );
            work_left = work_left.saturating_sub(page.inspected);
            for hit in page.results {
                let Some(address) = hit.action_address() else {
                    continue;
                };
                let binding = providers[provider_index].callback_binding();
                if binding
                    .as_ref()
                    .is_some_and(|binding| self.require_bound_broker_route(binding).is_err())
                {
                    continue;
                }
                let resource = providers[provider_index]
                    .resolve_action_resource(&address)
                    .or_else(|| {
                        if let Some(binding) = &binding {
                            crate::bound_dispatch::resolve_component_resource(
                                self.root_widget.as_mut(),
                                &self.state.doc,
                                binding,
                                &address.resource,
                                &address.action,
                            )
                            .ok()
                            .flatten()
                        } else {
                            crate::bound_dispatch::resolve_unbound_resource(
                                self.root_widget.as_ref(),
                                &self.state.doc,
                                &address.resource,
                            )
                        }
                    })
                    .and_then(|resource| {
                        binding.as_ref().map_or(Some(resource.clone()), |binding| {
                            crate::bound_dispatch::filter_resource(binding, resource)
                        })
                    });
                let Some(mut resource) = resource else {
                    continue;
                };
                if let Some(binding) = providers[provider_index].callback_binding() {
                    if !crate::bound_dispatch::validate_bound_resources(
                        self.root_widget.as_mut(),
                        &binding,
                        std::slice::from_ref(&resource),
                    )
                    .unwrap_or(false)
                    {
                        continue;
                    }
                    let Ok(projected) = self.actions.bound_publication.project(&binding, resource)
                    else {
                        continue;
                    };
                    resource = projected;
                } else if resource
                    .id
                    .as_str()
                    .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
                {
                    continue;
                }
                let Some(row) = Self::directory_action_row(&resource, &address.action) else {
                    continue;
                };
                let before = estimated_bytes;
                add(row, &mut rows, &mut estimated_bytes);
                if estimated_bytes > max_bytes {
                    rows.pop();
                    estimated_bytes = before;
                    byte_exhausted = true;
                    break;
                }
            }
            if byte_exhausted {
                break;
            }
            if let Some(next) = page.next {
                provider_cursor = Some(next);
                if rows.len() < max_results && estimated_bytes < max_bytes && work_left > 0 {
                    continue;
                }
                break;
            }
            provider_index += 1;
            provider_cursor = None;
        }
        let exhausted = !byte_exhausted && provider_index >= providers.len();
        // Keep the source cursor *before* an oversized row when earlier rows
        // fitted. A caller that raises maxBytes resumes exactly at that row;
        // dropping it would restart and duplicate the fitted prefix.
        let next_cursor = (!exhausted && (!byte_exhausted || !rows.is_empty())).then(|| {
            self.action_directory_next_cursor(ActionDirectoryCursor {
                provider_index,
                provider_cursor,
                ..state
            })
        });
        runtime_collab::ActionDirectoryPage {
            revision,
            rows,
            next_cursor: next_cursor.unwrap_or_default(),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }

    fn resolve_publication_resource(
        &mut self,
        resource_id: &str,
        action_id: &str,
    ) -> Option<tilo_actions::Resource> {
        let address = tilo_actions::ActionAddress::new(resource_id, action_id);
        match self.actions.bound_publication.target(&address) {
            Ok(Some(target)) => {
                self.require_bound_broker_route(&target.binding).ok()?;
                let resource = crate::bound_dispatch::resolve_component_resource(
                    self.root_widget.as_mut(),
                    &self.state.doc,
                    &target.binding,
                    &target.address.address.resource,
                    action_id,
                )
                .ok()
                .flatten()?;
                if !crate::bound_dispatch::validate_bound_resources(
                    self.root_widget.as_mut(),
                    &target.binding,
                    std::slice::from_ref(&resource),
                )
                .unwrap_or(false)
                {
                    return None;
                }
                self.actions
                    .bound_publication
                    .project(&target.binding, resource)
                    .ok()
            }
            Err(_) => None,
            Ok(None) => crate::bound_dispatch::resolve_unbound_resource(
                self.root_widget.as_ref(),
                &self.state.doc,
                resource_id,
            )
            .or_else(|| {
                crate::bound_dispatch::search_providers(self.root_widget.as_ref())
                    .into_iter()
                    .filter(|provider| {
                        provider.callback_binding().is_none()
                            && provider.action_directory_generation().is_some()
                    })
                    .find_map(|provider| provider.resolve_action_resource(&address))
            }),
        }
    }

    fn resolve_action_directory(
        &mut self,
        revision: &str,
        resource_id: &str,
        action_id: &str,
    ) -> runtime_collab::ActionDirectoryResolution {
        if revision != self.action_directory_revision() {
            return runtime_collab::ActionDirectoryResolution::StaleRevision;
        }
        if let Some(row) = self
            .actions
            .action_directory_snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .actions
                    .get(&tilo_actions::ActionAddress::new(resource_id, action_id))
            })
        {
            return runtime_collab::ActionDirectoryResolution::Found(Box::new(row.clone()));
        }
        let resource = self.resolve_publication_resource(resource_id, action_id);
        resource
            .as_ref()
            .and_then(|resource| Self::directory_action_row(resource, action_id))
            .map_or(runtime_collab::ActionDirectoryResolution::NotFound, |row| {
                runtime_collab::ActionDirectoryResolution::Found(Box::new(row))
            })
    }

    fn pump_action_directory_requests(&mut self) {
        for request in self.collaboration.manager.take_action_directory_requests() {
            match request {
                ActionDirectoryRequest::Query { query, reply } => {
                    let _ = reply.send(self.action_directory_page(query));
                }
                ActionDirectoryRequest::Resolve {
                    revision,
                    resource,
                    action,
                    reply,
                } => {
                    let _ =
                        reply.send(self.resolve_action_directory(&revision, &resource, &action));
                }
            }
        }
    }

    /// Publish only a changed, app-declared resume projection. This remains
    /// deliberately separate from document collaboration: a continuation
    /// report is private to the local session and cannot make a remote app
    /// focus itself or start playing media.
    fn publish_continuation_metadata(&mut self) {
        self.apply_pending_continuation_restore();
        if !self.collaboration.manager.is_connected() || self.continuation.pending_view.is_some() {
            return;
        }
        let semantic = self.root_widget.continuable().and_then(|continuable| {
            continuable.continuation_metadata(&self.state.doc, &self.state.root)
        });
        let view = <Self as crate::view::FreezeThaw>::capture_view(self);
        let metadata = compose_structural_continuation_metadata(view, semantic);
        if metadata == self.continuation.last_published {
            return;
        }
        if let Some(request) = continuation_publication(
            crate::collaboration::local_app_id(),
            metadata.as_ref(),
            self.continuation.last_published.is_some(),
        ) {
            self.collaboration.manager.send(request);
        }
        self.continuation.last_published = metadata;
    }

    /// Publish private source descriptors over the authenticated local app
    /// broker. Public cross-device state is derived by the shell and remains
    /// path/URL-free; this report never enters the room document.
    fn publish_playback_handoff(&mut self) {
        if !self.collaboration.manager.is_connected() {
            return;
        }
        let now = Instant::now();
        if self
            .last_playback_publish
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(500))
        {
            return;
        }
        let app_id = crate::collaboration::local_app_id();
        let workspace_id = crate::collaboration::local_cluster_id().to_string();
        let reports = media::handoff::playback_reports(&app_id, &workspace_id);
        if self.last_published_playback.as_ref() == Some(&reports) {
            return;
        }
        let Ok(reports_json) = serde_json::to_vec(&reports) else {
            return;
        };
        self.collaboration
            .manager
            .send(crate::collaboration::CollabRequest::ReportPlayback {
                app_id,
                reports_json,
            });
        self.last_published_playback = Some(reports);
        self.last_playback_publish = Some(now);
    }

    fn apply_pending_continuation_restore(&mut self) {
        if let Some(metadata) = self.continuation.pending_restore.take() {
            if metadata.schema_version == STRUCTURAL_CONTINUATION_SCHEMA {
                if let Ok(envelope) =
                    postcard::from_bytes::<StructuralContinuationEnvelope>(&metadata.bytes)
                    && valid_structural_envelope(&envelope)
                {
                    self.continuation.pending_view = Some(envelope.presentation.into_view());
                    if let Some(semantic) = envelope.semantic {
                        let root = self.state.root.clone();
                        let metadata = crate::collaboration::ContinuationMetadata {
                            schema_version: semantic.schema_version,
                            bytes: semantic.bytes,
                        };
                        if let Some(continuable) = self.root_widget.continuable_mut() {
                            let _ = continuable.restore_continuation_metadata(
                                &metadata,
                                &mut self.state.doc,
                                &root,
                            );
                        }
                        self.invalidate_widget_resource_cache();
                    }
                }
                return;
            }
            if metadata.schema_version == GENERIC_VIEW_CONTINUATION_SCHEMA {
                if let Ok(view) = postcard::from_bytes::<PortableContinuationView>(&metadata.bytes)
                {
                    self.continuation.pending_view = Some(view.into_view());
                }
                return;
            }
            let root = self.state.root.clone();
            if let Some(continuable) = self.root_widget.continuable_mut() {
                let _ = continuable.restore_continuation_metadata(
                    &metadata,
                    &mut self.state.doc,
                    &root,
                );
            }
            self.invalidate_widget_resource_cache();
        }
    }

    fn invoke_published_action(
        &mut self,
        id: u64,
        resource: String,
        action: String,
        argument: Option<&str>,
        actor: &str,
        ambient_context: Option<&tilo_actions::ActionContext>,
    ) {
        let address = tilo_actions::ActionAddress::new(resource, action);
        // Arrival on this directed stream means the broker already performed
        // permission and one-use confirmation-token checks.
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let result = self.invoke_resource_action(
            &address,
            argument,
            actor,
            true,
            true,
            None,
            Some(invocation),
            ambient_context,
        );
        let update = match result {
            Ok(crate::widget::ResourceOperationStart::Completed(output)) => {
                tilo_actions::InvocationUpdate::Succeeded { invocation, output }
            }
            Ok(crate::widget::ResourceOperationStart::Accepted) => {
                self.actions.directed_invocations.insert(
                    invocation,
                    DirectedInvocation {
                        wire_id: id,
                        cancellation_requested: false,
                    },
                );
                tilo_actions::InvocationUpdate::Accepted { invocation }
            }
            Err(error) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: crate::bound_dispatch::door_error_code(&error).to_string(),
                    message: error.to_string(),
                    details: None,
                    retryable: false,
                },
            },
        };
        self.collaboration
            .manager
            .send(CollabRequest::UpdateInvocation { id, update });
    }

    fn resolve_resource_invocation<'a>(
        resources: &'a [tilo_actions::Resource],
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
        confirmed_by_broker: bool,
    ) -> Result<ResolvedResourceInvocation<'a>, tilo_actions::DoorError> {
        crate::bound_dispatch::resolve_resource_invocation(
            resources,
            address,
            argument,
            None,
            confirmed_by_broker,
        )
    }

    fn encode_resource_action(
        resources: &[tilo_actions::Resource],
        address: &tilo_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> Result<tilo_actions::EncodedAction, tilo_actions::DoorError> {
        crate::bound_dispatch::encode_resource_action(resources, address, typed_arguments, argument)
    }

    /// Admit a locally initiated operation at the session broker before the
    /// application performs it. The broker records the invocation first,
    /// re-resolves the live published surface and current permissions, then
    /// directs the exact operation back to this connection. That ordering is
    /// load-bearing for Effects: if the app crashes while doing external work,
    /// history retains an admitted/incomplete operation instead of losing the
    /// action entirely.
    fn invoke_via_broker(
        &mut self,
        address: &tilo_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
        confirmation_evidence: Option<&serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        Self::dispatch_broker_request(
            &mut self.actions.next_broker_request_id,
            self.collaboration.manager.is_connected(),
            address,
            typed_arguments,
            argument,
            confirmation_evidence,
            |request| {
                self.collaboration.manager.send(request);
            },
        )
    }

    fn dispatch_broker_request(
        next_request_id: &mut Option<u64>,
        connected: bool,
        address: &tilo_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
        confirmation_evidence: Option<&serde_json::Value>,
        send: impl FnOnce(CollabRequest),
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        if !connected {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "authoritative_host_unavailable".into(),
                reason: "This operation requires the workspace action broker, but collaboration is offline".into(),
            });
        }
        let request_id = Self::allocate_broker_request_id(next_request_id)?;
        let arguments = typed_arguments.cloned().unwrap_or_else(|| {
            let mut arguments = serde_json::Map::new();
            if let Some(value) = argument {
                arguments.insert(
                    tilo_actions::PROMPTED_PARAM.to_string(),
                    serde_json::Value::String(value.to_string()),
                );
            }
            arguments
        });
        send(CollabRequest::InvokePublishedAction {
            request_id,
            peer_id: crate::collaboration::CURRENT_CONNECTION_PEER_ID,
            resource: address.resource.clone(),
            action: address.action.clone(),
            arguments,
            confirmation: confirmation_evidence
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        });
        Ok(crate::widget::ResourceOperationStart::Accepted)
    }

    fn take_broker_request_id(&mut self) -> Result<u64, tilo_actions::DoorError> {
        Self::allocate_broker_request_id(&mut self.actions.next_broker_request_id)
    }
    fn allocate_broker_request_id(
        next_request_id: &mut Option<u64>,
    ) -> Result<u64, tilo_actions::DoorError> {
        let request_id = next_request_id.take().ok_or_else(|| {
            tilo_actions::DoorError::Unavailable {
                code: "broker_request_identity_exhausted".into(),
                reason: "This app has exhausted its broker request identities; restart it before invoking another hosted operation".into(),
            }
        })?;
        *next_request_id = request_id.checked_add(1);
        Ok(request_id)
    }

    /// Retain exactly one broker request per admitted launcher invocation.
    /// The fixed cap is a failure boundary: overflowing it must refuse the
    /// invocation before it can be left Accepted without a terminal receipt.
    fn admit_application_launch(
        &mut self,
        request_id: u64,
        invocation: tilo_actions::InvocationId,
    ) -> Result<(), tilo_actions::DoorError> {
        if self.actions.pending_application_launches.len() >= MAX_PENDING_APPLICATION_LAUNCHES {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "application_launch_queue_full".into(),
                reason: "Too many application launches are awaiting broker receipts".into(),
            });
        }
        self.actions
            .pending_application_launches
            .insert(request_id, invocation);
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "document dispatch needs the resolved action, arguments, actor, and ambient provenance"
    )]
    fn invoke_document_resource_action(
        &mut self,
        resources: &[tilo_actions::Resource],
        address: &tilo_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
        actor: &str,
        resource: &tilo_actions::Resource,
        spec: &tilo_actions::ActionSpec,
        ambient_context: Option<&tilo_actions::ActionContext>,
        confirmed_by_broker: bool,
        confirmation_evidence: Option<&serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let encoded = Self::encode_resource_action(resources, address, typed_arguments, argument)?;
        let action = self
            .actions
            .action_registry
            .decode(&encoded)
            .map_err(|_| tilo_actions::DoorError::UnregisteredType(encoded.type_id.clone()))?;
        if action.locus() == tilo_actions::ExecutionLocus::ServerPreferred && !confirmed_by_broker {
            return self.invoke_via_broker(
                address,
                typed_arguments,
                argument,
                confirmation_evidence,
            );
        }
        // Only admitted local document execution owns a new change boundary.
        // Encoding failures and broker routing must leave the caller's pending
        // edit intact; the executor still receives the same committed baseline.
        let before = self.state.doc.get_heads();
        let started = crate::bound_dispatch::execute_resource_operation(
            self.root_widget.as_mut(),
            &mut self.state.doc,
            &self.state.root,
            &self.actions.action_registry,
            self.actions.action_transport.as_mut(),
            address,
            resource,
            spec,
            encoded,
            actor,
            &self.collaboration.local_author.0,
            confirmed_by_broker,
            tilo_actions::InvocationId::new(),
            ambient_context,
        )?;
        if before != self.state.doc.get_heads() {
            self.state.record_committed_action_authored(
                before,
                None,
                None,
                Some(crate::permissions::UserId(actor.to_string())),
            );
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            self.invalidate_widget_resource_cache();
        }
        Ok(started)
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the authority door deliberately keeps address, actor, confirmation, broker admission, evidence, invocation, and provenance explicit in one auditable boundary"
    )]
    #[allow(
        clippy::too_many_arguments,
        reason = "host broker admission retains exact origin and supplied evidence"
    )]
    fn invoke_bound_target(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        argument: Option<&str>,
        actor: &str,
        confirmed: bool,
        broker_admitted: bool,
        evidence: Option<&serde_json::Value>,
        invocation: Option<tilo_actions::InvocationId>,
        ambient: Option<&tilo_actions::ActionContext>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.invoke_bound_target_with_broker(
            target,
            argument,
            actor,
            confirmed,
            broker_admitted,
            evidence,
            invocation,
            ambient,
            None,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "host admission and optional synchronous broker delivery remain explicit"
    )]
    fn invoke_bound_target_with_broker(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        argument: Option<&str>,
        actor: &str,
        confirmed: bool,
        broker_admitted: bool,
        evidence: Option<&serde_json::Value>,
        invocation: Option<tilo_actions::InvocationId>,
        ambient: Option<&tilo_actions::ActionContext>,
        broker_delivery: Option<&mut dyn FnMut(CollabRequest)>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let _entry = target.binding.enter().map_err(|error| {
            crate::bound_dispatch::BoundActionError::Dispatch(error).into_door()
        })?;
        if broker_admitted {
            self.require_bound_broker_route(&target.binding)?;
        }
        let snapshots = crate::bound_dispatch::discover_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            Some(&target.address.address),
        )
        .map_err(|error| crate::bound_dispatch::BoundActionError::Dispatch(error).into_door())?;
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.binding.same_component(&target.binding))
            .ok_or_else(|| tilo_actions::DoorError::Unknown(target.address.address.clone()))?;
        let (arguments, _, spec) = crate::bound_dispatch::resolve_resource_invocation(
            &snapshot.resources,
            &target.address.address,
            argument,
            None,
            confirmed,
        )?;
        let kind = spec.kind;
        if !broker_admitted
            && ((kind == tilo_actions::OperationKind::Effect
                && invocation.is_none()
                && (broker_delivery.is_some() || self.collaboration.manager.is_connected()))
                || (kind == tilo_actions::OperationKind::Document
                    && spec.locus == tilo_actions::ExecutionLocus::ServerPreferred))
        {
            self.require_bound_broker_route(&target.binding)?;
            if !crate::bound_dispatch::validate_bound_resources(
                self.root_widget.as_mut(),
                &target.binding,
                &snapshot.resources,
            )
            .unwrap_or(false)
            {
                return Err(tilo_actions::DoorError::Unknown(
                    target.address.address.clone(),
                ));
            }
            let alias = self
                .actions
                .bound_publication
                .alias(&target.binding, &target.address.address.resource)
                .map_err(crate::bound_dispatch::BoundActionError::into_door)?;
            if let Some(send) = broker_delivery {
                return Self::dispatch_broker_request(
                    &mut self.actions.next_broker_request_id,
                    true,
                    &tilo_actions::ActionAddress::new(alias, target.address.address.action.clone()),
                    arguments.as_ref(),
                    argument,
                    evidence,
                    send,
                );
            }
            self.publish_action_surface();
            return self.invoke_via_broker(
                &tilo_actions::ActionAddress::new(alias, target.address.address.action.clone()),
                arguments.as_ref(),
                argument,
                evidence,
            );
        }
        let id = invocation.unwrap_or_default();
        let started = crate::bound_dispatch::invoke_resource_action(
            self.root_widget.as_mut(),
            &mut self.state.doc,
            target,
            self.actions.action_transport.as_mut(),
            argument,
            None,
            actor,
            confirmed,
            broker_admitted,
            id,
            ambient,
        )
        .map_err(crate::bound_dispatch::BoundActionError::into_door);
        // A post-delivery fault retains canonical writes and must still refresh
        // the host projection. Generation is advanced by the checked frame.
        self.needs_sync = true;
        self.layout.layout_dirty = true;
        self.invalidate_widget_resource_cache();
        let started = started?;
        if matches!(started, crate::widget::ResourceOperationStart::Accepted) {
            self.actions.bound_invocations.insert(id, target.clone());
            if invocation.is_none() {
                self.actions
                    .local_invocations
                    .insert(id, (target.address.address.clone(), kind));
            }
        }
        Ok(started)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "existing external action door inputs"
    )]
    fn invoke_resource_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
        actor: &str,
        confirmation_satisfied: bool,
        broker_admitted: bool,
        confirmation_evidence: Option<&serde_json::Value>,
        invocation: Option<tilo_actions::InvocationId>,
        ambient_context: Option<&tilo_actions::ActionContext>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        if let Some(target) = self
            .actions
            .bound_publication
            .target(address)
            .map_err(crate::bound_dispatch::BoundActionError::into_door)?
        {
            self.require_bound_broker_route(&target.binding)?;
            return self.invoke_bound_target(
                &target,
                argument,
                actor,
                confirmation_satisfied,
                broker_admitted,
                confirmation_evidence,
                invocation,
                ambient_context,
            );
        }
        let snapshots = crate::bound_dispatch::discover_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            Some(address),
        )
        .map_err(|error| crate::bound_dispatch::BoundActionError::Dispatch(error).into_door())?;
        let target = crate::bound_dispatch::select_bound_target(&snapshots, address, None)
            .map_err(crate::bound_dispatch::BoundActionError::into_door)?;
        if let Some(target) = target {
            return self.invoke_bound_target(
                &target,
                argument,
                actor,
                confirmation_satisfied,
                broker_admitted,
                confirmation_evidence,
                invocation,
                ambient_context,
            );
        }
        self.invoke_unbound_resource_action(
            address,
            argument,
            actor,
            confirmation_satisfied,
            broker_admitted,
            confirmation_evidence,
            invocation,
            ambient_context,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "legacy broker and semantic orchestration retains the existing explicit door inputs"
    )]
    fn invoke_unbound_resource_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
        actor: &str,
        confirmation_satisfied: bool,
        broker_admitted: bool,
        confirmation_evidence: Option<&serde_json::Value>,
        invocation: Option<tilo_actions::InvocationId>,
        ambient_context: Option<&tilo_actions::ActionContext>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let mut resources = if actor == self.collaboration.local_author.0 {
            self.collect_local_resources()
        } else {
            self.collect_resources()
        };
        if tilo_actions::door::resolve(&resources, address).is_none() {
            let fallback = crate::bound_dispatch::resolve_unbound_resource(
                self.root_widget.as_ref(),
                &self.state.doc,
                &address.resource,
            );
            let fallback = if actor == self.collaboration.local_author.0 {
                fallback.and_then(|resource| self.filter_resource_for_local_actor(resource))
            } else {
                fallback
            };
            if let Some(resource) = fallback {
                resources.push(resource);
            }
        }
        let (typed_arguments, resource, resolved) = Self::resolve_resource_invocation(
            &resources,
            address,
            argument,
            confirmation_satisfied,
        )?;
        let kind = resolved.kind;
        if kind == tilo_actions::OperationKind::View && actor != self.collaboration.local_author.0 {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "private_view".into(),
                reason: "A collaborator cannot move this participant's private focus or viewport; use Follow or Reveal explicitly".into(),
            });
        }
        // Every locally initiated external effect crosses the same broker
        // admission as an agent invocation before the handler touches the
        // outside world. Directed invocations carry a real id and
        // `broker_admitted=true`, so they execute exactly once when routed
        // back here. Standalone development runs retain the existing local
        // fallback below; installed Tilo apps always have the session broker.
        if kind == tilo_actions::OperationKind::Effect
            && invocation.is_none()
            && !broker_admitted
            && self.collaboration.manager.is_connected()
        {
            return self.invoke_via_broker(
                address,
                typed_arguments.as_ref(),
                argument,
                confirmation_evidence,
            );
        }
        if kind == tilo_actions::OperationKind::Document {
            return self.invoke_document_resource_action(
                &resources,
                address,
                typed_arguments.as_ref(),
                argument,
                actor,
                resource,
                resolved,
                ambient_context,
                broker_admitted,
                confirmation_evidence,
            );
        }
        let encoded =
            Self::encode_resource_action(&resources, address, typed_arguments.as_ref(), argument)?;
        // A launcher action has already passed the ordinary declaration,
        // argument-binding, confirmation, and broker-admission door above.
        // This branch runs only when the broker directs that admitted action
        // back to its source app, preserving the normal receipt lifecycle.
        if encoded.type_id == "tilo.application.launch" {
            let launch: ApplicationLaunchPayload = serde_json::from_slice(&encoded.payload)
                .map_err(|_| {
                    tilo_actions::DoorError::Failed("invalid application launch payload".into())
                })?;
            if !launch.valid() {
                return Err(tilo_actions::DoorError::Failed(
                    "application launch payload is out of bounds".into(),
                ));
            }
            if !self.collaboration.manager.is_connected() {
                return Err(tilo_actions::DoorError::Unavailable {
                    code: "authoritative_host_unavailable".into(),
                    reason: "Opening staged media requires the workspace action broker".into(),
                });
            }
            let request_id = self.take_broker_request_id()?;
            let Some(invocation) = invocation else {
                return Err(tilo_actions::DoorError::Failed(
                    "application launch was not admitted by the broker".into(),
                ));
            };
            self.admit_application_launch(request_id, invocation)?;
            self.collaboration
                .manager
                .send(CollabRequest::LaunchPublishedApplication {
                    request_id,
                    app_id: launch.app_id,
                    content_hash: Some(launch.content_hash),
                });
            return Ok(crate::widget::ResourceOperationStart::Accepted);
        }
        let operation_invocation = invocation.unwrap_or_default();
        if encoded.type_id == crate::a11y_tree::SEMANTIC_OPERATION_TYPE {
            let semantic_result = self.invoke_semantic_operation_encoded(&encoded);
            let handled = match semantic_result {
                Ok(handled) => handled,
                Err(message) => {
                    if kind == tilo_actions::OperationKind::Effect && invocation.is_none() {
                        self.collaboration
                            .manager
                            .send(CollabRequest::RecordLocalEffect {
                                address: address.clone(),
                                update: tilo_actions::InvocationUpdate::Failed {
                                    invocation: operation_invocation,
                                    error: tilo_actions::ActionFailure {
                                        code: "semantic_action_failed".into(),
                                        message: message.clone(),
                                        details: None,
                                        retryable: false,
                                    },
                                },
                            });
                    }
                    return Err(tilo_actions::DoorError::Failed(message));
                }
            };
            if !handled {
                if kind == tilo_actions::OperationKind::Effect && invocation.is_none() {
                    self.collaboration
                        .manager
                        .send(CollabRequest::RecordLocalEffect {
                            address: address.clone(),
                            update: tilo_actions::InvocationUpdate::Failed {
                                invocation: operation_invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "unknown_action".into(),
                                    message: "the semantic target no longer accepts this action"
                                        .into(),
                                    details: None,
                                    retryable: false,
                                },
                            },
                        });
                }
                return Err(tilo_actions::DoorError::Unknown(address.clone()));
            }
            // Focus/selection/navigation belong to private view state. A view
            // command must not finalize an unrelated in-progress document edit.
            if kind != tilo_actions::OperationKind::View {
                self.commit_action();
            }
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            if kind == tilo_actions::OperationKind::Effect && invocation.is_none() {
                self.collaboration
                    .manager
                    .send(CollabRequest::RecordLocalEffect {
                        address: address.clone(),
                        update: tilo_actions::InvocationUpdate::Succeeded {
                            invocation: operation_invocation,
                            output: tilo_actions::ActionOutput::default(),
                        },
                    });
            }
            return Ok(crate::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ));
        }
        let started = match crate::bound_dispatch::execute_resource_operation(
            self.root_widget.as_mut(),
            &mut self.state.doc,
            &self.state.root,
            &self.actions.action_registry,
            self.actions.action_transport.as_mut(),
            address,
            resource,
            resolved,
            encoded,
            actor,
            &self.collaboration.local_author.0,
            broker_admitted,
            operation_invocation,
            ambient_context,
        ) {
            Ok(started) => started,
            Err(message) => {
                if kind == tilo_actions::OperationKind::Effect && invocation.is_none() {
                    let update = tilo_actions::InvocationUpdate::Failed {
                        invocation: operation_invocation,
                        error: tilo_actions::ActionFailure {
                            code: "action_failed".into(),
                            message: message.to_string(),
                            details: None,
                            retryable: false,
                        },
                    };
                    self.collaboration
                        .manager
                        .send(CollabRequest::RecordLocalEffect {
                            address: address.clone(),
                            update,
                        });
                }
                return Err(message);
            }
        };
        if invocation.is_none()
            && matches!(started, crate::widget::ResourceOperationStart::Accepted)
        {
            self.actions
                .local_invocations
                .insert(operation_invocation, (address.clone(), kind));
        }
        if matches!(started, crate::widget::ResourceOperationStart::Accepted) {
            // Queued widget work advances from `sync`; admission must
            // therefore schedule a model pass even if no document
            // mutation or input event accompanied the invocation.
            self.needs_sync = true;
        }
        if let crate::widget::ResourceOperationStart::Completed(output) = &started
            && kind == tilo_actions::OperationKind::Effect
            && invocation.is_none()
        {
            self.collaboration
                .manager
                .send(CollabRequest::RecordLocalEffect {
                    address: address.clone(),
                    update: tilo_actions::InvocationUpdate::Succeeded {
                        invocation: operation_invocation,
                        output: output.clone(),
                    },
                });
        }
        // Effect/view operations are allowed to switch presentation-only
        // resources without touching Automerge.
        self.invalidate_widget_resource_cache();
        self.layout.layout_dirty = true;
        Ok(started)
    }

    fn dispatch_bound_control_invocation(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        intent: &tilo_actions::ControlInvocation,
    ) -> bool {
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(intent.arguments.clone())
        );
        let actor = self.collaboration.local_author.0.clone();
        let result = self.invoke_bound_target(
            target,
            Some(&argument),
            &actor,
            false,
            false,
            None,
            None,
            None,
        );
        if let Err(error) = crate::bound_dispatch::notify_control_invocation_result(
            self.root_widget.as_mut(),
            target,
            &result,
        ) {
            tracing::warn!(?error, "bound control feedback unavailable");
        }
        match result {
            Ok(_) => true,
            Err(tilo_actions::DoorError::ConfirmationRequired) => {
                self.open_action_palette();
                if let Some(palette) = self.actions.action_palette.as_mut() {
                    palette.activate_bound_with_arguments(target, &intent.arguments);
                }
                true
            }
            Err(error) => {
                tracing::warn!(%error, "bound control invocation refused");
                false
            }
        }
    }

    /// Resolve queued local control intents against the current resource tree
    /// and enter through the same door/lifecycle as every other caller.
    fn dispatch_control_invocations(&mut self) -> usize {
        self.service_close_admission();
        if self
            .history
            .as_ref()
            .is_some_and(|history| history.captures_input())
        {
            self.discard_history_local_intents();
            return 0;
        }
        // Embedded History refresh discards only its own hidden live queue.
        // Defer sibling work until this input frame ends without consuming it.
        if self.framework_input.get() {
            return 0;
        }
        let mut dispatched = self.dispatch_history_controls();
        let bound = crate::bound_dispatch::drain_control_invocations(self.root_widget.as_mut());
        if let Some(error) = bound.error {
            tracing::warn!(
                ?error,
                "control traversal incomplete; delivering already drained intents"
            );
        }
        for (target, intent) in bound.pending {
            if self.dispatch_bound_control_invocation(&target, &intent) {
                dispatched = dispatched.saturating_add(1);
            }
        }
        while let Some(intent) = self.root_widget.take_control_invocation() {
            let local_resources = self.collect_local_resources();
            let Some(resolved) = tilo_actions::door::resolve(&local_resources, &intent.address)
            else {
                tracing::warn!(
                    resource = %intent.address.resource,
                    action = %intent.address.action,
                    "action-bound control is unavailable to the local actor"
                );
                self.root_widget.on_control_invocation_result(
                    &intent.address,
                    &Err(tilo_actions::DoorError::Unknown(intent.address.clone())),
                );
                continue;
            };
            if !matches!(
                resolved.contract.confirmation,
                tilo_actions::ConfirmationPolicy::None
            ) {
                // A bound control never manufactures confirmation. Hand the
                // exact address to the standard palette flow, which owns the
                // explicit confirm/phrase UI and only then enters the door.
                self.root_widget.on_control_invocation_result(
                    &intent.address,
                    &Err(tilo_actions::DoorError::ConfirmationRequired),
                );
                self.open_action_palette();
                let activated = self.actions.action_palette.as_mut().is_some_and(|palette| {
                    palette.activate_address_with_arguments(&intent.address, &intent.arguments)
                });
                if !activated {
                    self.close_action_palette();
                }
                dispatched = dispatched.saturating_add(1);
                continue;
            }
            let json = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(intent.arguments.clone())
            );
            let actor = self.collaboration.local_author.0.clone();
            let result = self.invoke_resource_action(
                &intent.address,
                Some(&json),
                &actor,
                false,
                false,
                None,
                None,
                None,
            );
            self.root_widget
                .on_control_invocation_result(&intent.address, &result);
            match result {
                Ok(_) => dispatched = dispatched.saturating_add(1),
                Err(error) => tracing::warn!(
                    resource = %intent.address.resource,
                    action = %intent.address.action,
                    %error,
                    "action-bound control invocation refused"
                ),
            }
        }
        dispatched
    }

    fn request_invocation_cancellation(&mut self, wire_id: u64) {
        if let Some(pending) = self
            .actions
            .directed_invocations
            .values_mut()
            .find(|pending| pending.wire_id == wire_id)
        {
            pending.cancellation_requested = true;
        }
    }

    fn fail_pending_application_launches_on_disconnect(&mut self) -> bool {
        if self.collaboration.manager.is_connected() {
            return false;
        }

        let launches = std::mem::take(&mut self.actions.pending_application_launches);
        let mut changed = false;
        for (_request_id, invocation) in launches {
            let Some(directed) = self.actions.directed_invocations.remove(&invocation) else {
                continue;
            };
            self.collaboration.manager.send(CollabRequest::UpdateInvocation {
                id: directed.wire_id,
                update: tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "application_launch_broker_disconnected".into(),
                        message: "The launcher broker disconnected before confirming the application launch".into(),
                        details: None,
                        retryable: true,
                    },
                },
            });
            changed = true;
        }
        changed
    }

    /// Give retained widgets a non-blocking lifecycle tick and forward every
    /// progress/item/terminal update to the broker. A terminal update removes
    /// the job before it is sent, making terminal delivery at-most-once even
    /// if a widget accidentally returns the same update again.
    #[allow(
        clippy::too_many_lines,
        reason = "directed and local terminal delivery share the existing status sweep"
    )]
    fn poll_directed_invocations(&mut self) -> bool {
        // A broker disconnect cannot leave an admitted launcher action
        // Accepted forever. Draining the bounded table also makes late or
        // duplicate receipts inert because their request ids are gone.
        let mut changed = self.fail_pending_application_launches_on_disconnect();
        let invocations = self
            .actions
            .directed_invocations
            .iter()
            .map(|(invocation, pending)| (*invocation, *pending))
            .collect::<Vec<_>>();
        for (invocation, pending) in invocations {
            let polled = if let Some(target) = self.actions.bound_invocations.get(&invocation) {
                crate::bound_dispatch::poll_resource_operation(
                    self.root_widget.as_mut(),
                    &self.state.doc,
                    target,
                    invocation,
                    pending.cancellation_requested,
                )
            } else {
                Ok(self.root_widget.poll_resource_operation(
                    invocation,
                    pending.cancellation_requested,
                    &self.state.doc,
                    &self.state.root,
                ))
            };
            let mut update = match polled {
                Ok(Some(update)) => update,
                Ok(None) => continue,
                Err(_) => {
                    self.actions.bound_invocations.remove(&invocation);
                    self.actions.directed_invocations.remove(&invocation);
                    continue;
                }
            };
            if update.invocation() != invocation
                || matches!(update, tilo_actions::InvocationUpdate::Accepted { .. })
            {
                update = tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "tilo.invalid_lifecycle_update".into(),
                        message: "operation handler returned an invalid lifecycle update".into(),
                        details: None,
                        retryable: false,
                    },
                };
            }
            if update.is_terminal() {
                self.actions.bound_invocations.remove(&invocation);
                self.actions.directed_invocations.remove(&invocation);
            }
            self.collaboration
                .manager
                .send(CollabRequest::UpdateInvocation {
                    id: pending.wire_id,
                    update,
                });
            changed = true;
        }
        let local_invocations = self
            .actions
            .local_invocations
            .iter()
            .map(|(invocation, (address, kind))| (*invocation, address.clone(), *kind))
            .collect::<Vec<_>>();
        for (invocation, address, kind) in local_invocations {
            let polled = if let Some(target) = self.actions.bound_invocations.get(&invocation) {
                crate::bound_dispatch::poll_resource_operation(
                    self.root_widget.as_mut(),
                    &self.state.doc,
                    target,
                    invocation,
                    false,
                )
            } else {
                Ok(self.root_widget.poll_resource_operation(
                    invocation,
                    false,
                    &self.state.doc,
                    &self.state.root,
                ))
            };
            let update = match polled {
                Ok(Some(update)) => update,
                Ok(None) => continue,
                Err(_) => {
                    self.actions.bound_invocations.remove(&invocation);
                    self.actions.local_invocations.remove(&invocation);
                    if self.close_admission.save_local == Some(invocation) {
                        self.close_admission.save_result =
                            Some(Err("save target is no longer available".into()));
                    }
                    continue;
                }
            };
            let invalid_identity = update.invocation() != invocation;
            if !invalid_identity && self.close_admission.save_local == Some(invocation) {
                self.observe_close_save(&update);
            } else if invalid_identity && self.close_admission.save_local == Some(invocation) {
                self.close_admission.save_result =
                    Some(Err("save returned an invalid invocation identity".into()));
            }
            if update.is_terminal() || invalid_identity {
                self.actions.bound_invocations.remove(&invocation);
                self.actions.local_invocations.remove(&invocation);
            }
            if (update.is_terminal() || invalid_identity)
                && kind == tilo_actions::OperationKind::Effect
            {
                let update = if invalid_identity {
                    tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "tilo.invalid_lifecycle_update".into(),
                            message: "operation handler returned an update for another invocation"
                                .into(),
                            details: None,
                            retryable: false,
                        },
                    }
                } else {
                    update
                };
                self.collaboration
                    .manager
                    .send(CollabRequest::RecordLocalEffect { address, update });
            }
            changed = true;
        }
        if changed {
            self.invalidate_widget_resource_cache();
            // Accepted operations can finish from a later model pass without
            // another broker or native input event. Their progress/terminal
            // transition may change document-dependent operations, so refresh
            // the broker surface from this lifecycle boundary as well.
            self.publish_action_surface();
        }
        changed
    }

    pub fn open_action_palette(&mut self) {
        let _entry = self.enter_wake_scope();
        let (key, permission_generation) = self.ensure_local_resource_cache();
        let mut resources = self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| {
                cache.key == key && cache.permission_generation == permission_generation
            })
            .and_then(|cache| cache.resources.clone())
            .unwrap_or_default();
        resources.extend(self.history_local_resources());
        let cached_open_resources = {
            self.actions
                .open_resource_search_cache
                .borrow()
                .as_ref()
                .filter(|cache| {
                    cache.key == key && cache.permission_generation == permission_generation
                })
                .map(|cache| cache.provider.clone())
        };
        let cached_open_resources = if self.history_chrome_enabled {
            None
        } else {
            cached_open_resources
        };
        let open_resources = cached_open_resources.unwrap_or_else(|| {
            let provider = crate::search::OpenResourceSearch::new(&resources);
            *self.actions.open_resource_search_cache.borrow_mut() = Some(OpenResourceSearchCache {
                key: key.clone(),
                permission_generation,
                provider: provider.clone(),
            });
            provider
        });
        let bound = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        let mut providers = crate::bound_dispatch::search_providers(self.root_widget.as_ref());
        providers.push(Box::new(open_resources));
        let mut palette = match self.palette_source {
            PaletteSource::Owned { renderer, field } => {
                crate::action_palette::ActionPalette::with_inputs(renderer(), field)
            }
            PaletteSource::Host => crate::action_palette::ActionPalette::with_host_text(
                self.text_renderer.font_system.locale().to_owned(),
            ),
        };
        palette.load_open_resources(&resources);
        palette.load_bound_resources(bound);
        palette.set_providers(providers);
        palette.open();
        self.actions.action_palette = Some(palette);
    }

    fn open_action_palette_at(&mut self, address: &tilo_actions::ActionAddress) -> bool {
        self.open_action_palette();
        let activated =
            self.actions.action_palette.as_mut().is_some_and(|palette| {
                palette.activate_address(&address.resource, &address.action)
            });
        if !activated {
            self.close_action_palette();
        }
        activated
    }

    fn filter_resource_for_local_actor(
        &self,
        mut resource: tilo_actions::Resource,
    ) -> Option<tilo_actions::Resource> {
        resource.actions.retain(|action| {
            crate::bound_dispatch::permits_local_action(
                &self.permission_ctx,
                &crate::collaboration::local_app_id(),
                action,
            )
        });
        resource.children = resource
            .children
            .into_iter()
            .filter_map(|child| self.filter_resource_for_local_actor(child))
            .collect();
        (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
    }

    pub fn close_action_palette(&mut self) {
        self.actions.action_palette = None;
    }

    pub fn action_palette(&self) -> Option<&crate::action_palette::ActionPalette> {
        self.actions.action_palette.as_ref()
    }

    fn refresh_action_palette_resources(&mut self) {
        // A palette can stay open while document state or the local actor's
        // permissions change. Refresh its authority snapshot immediately
        // before activation; rows retain addresses, never the callbacks or
        // encoded actions from the frame in which search found them.
        // The unbound cache includes projected retained semantic controls as
        // well as app resources. Keep both when refreshing a pending row.
        let (key, generation) = self.ensure_local_resource_cache();
        let mut resources = self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key && cache.permission_generation == generation)
            .and_then(|cache| cache.resources.clone())
            .unwrap_or_default();
        resources.extend(self.history_local_resources());
        let lazy_resource_ids = self
            .actions
            .action_palette
            .as_ref()
            .map(crate::action_palette::ActionPalette::referenced_action_resource_ids)
            .unwrap_or_default();
        for id in lazy_resource_ids {
            if let Some(resource) = crate::bound_dispatch::resolve_unbound_resource(
                self.root_widget.as_ref(),
                &self.state.doc,
                &id,
            )
            .and_then(|resource| self.filter_resource_for_local_actor(resource))
            {
                resources.push(resource);
            }
        }
        let mut bound = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        let targets = self
            .actions
            .action_palette
            .as_ref()
            .map(crate::action_palette::ActionPalette::referenced_bound_targets)
            .unwrap_or_default();
        for target in targets {
            if let Some(snapshot) = bound
                .iter_mut()
                .find(|snapshot| snapshot.binding.same_component(&target.binding))
                && tilo_actions::door::resolve(&snapshot.resources, &target.address.address)
                    .is_none()
                && let Ok(Some(resource)) = crate::bound_dispatch::resolve_component_resource(
                    self.root_widget.as_mut(),
                    &self.state.doc,
                    &target.binding,
                    &target.address.address.resource,
                    &target.address.address.action,
                )
            {
                snapshot.resources.push(resource);
            }
        }
        if let Some(palette) = self.actions.action_palette.as_mut() {
            palette.refresh_open_resources(&resources);
            palette.load_bound_resources(bound);
        }
    }

    fn action_palette_handle_key(&mut self, event: &WindowEvent) -> bool {
        if self.actions.action_palette.is_none() {
            return false;
        }
        self.refresh_action_palette_resources();
        let Some(palette) = self.actions.action_palette.as_mut() else {
            return false;
        };
        let root = self.state.root.clone();
        let consumed = palette.handle_key(event, &self.input.modifiers, &mut self.state.doc, &root);
        // An operation row may have fired. Document mutations go through the
        // deterministic executor; effects/views route to the owning widget.
        let pending = self
            .actions
            .action_palette
            .as_mut()
            .and_then(crate::action_palette::ActionPalette::take_operation);
        if let Some(pending) = pending {
            let argument = pending.wire_argument();
            let actor = self.collaboration.local_author.0.clone();
            // Confirmation was collected by the palette, but the request has
            // not crossed the session broker yet. Host-required documents are
            // therefore submitted to that broker rather than applied here.
            let history_result =
                self.invoke_history_address_local(&pending.address, argument.as_deref());
            let result = if let Some(result) = history_result {
                result
            } else if let Some(target) = pending.bound.as_ref() {
                self.invoke_bound_target(
                    target,
                    argument.as_deref(),
                    &actor,
                    true,
                    false,
                    Some(pending.confirmation_evidence()),
                    None,
                    Some(&pending.context),
                )
            } else {
                self.invoke_resource_action(
                    &pending.address,
                    argument.as_deref(),
                    &actor,
                    true,
                    false,
                    Some(pending.confirmation_evidence()),
                    None,
                    Some(&pending.context),
                )
            };
            if let Err(error) = result {
                tracing::warn!(%error, "resource operation failed");
            } else {
                self.layout.layout_dirty = true;
            }
        }
        // A content row may have fired: hand its id to the app to navigate,
        // recording an undo point only if it mutated the shared doc.
        let navigation = self
            .actions
            .action_palette
            .as_mut()
            .and_then(super::action_palette::ActionPalette::take_scoped_navigation);
        if let Some((action_id, binding)) = navigation {
            if let Some(binding) = binding {
                let _ = crate::bound_dispatch::activate_bound_search(
                    self.root_widget.as_mut(),
                    &mut self.state.doc,
                    &binding,
                    &action_id,
                );
            } else {
                let nav_before = self.state.doc.get_heads();
                self.root_widget
                    .activate_search_result(&action_id, &mut self.state.doc, &root);
                if self.state.doc.get_heads() != nav_before {
                    self.state.record_committed_action(nav_before, None, None);
                }
            }
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        // If the palette closed (Escape, or a non-prompted selection), drop it.
        if self
            .actions
            .action_palette
            .as_ref()
            .is_some_and(|p| !p.is_open())
        {
            self.actions.action_palette = None;
        }
        if consumed {
            self.invalidate_widget_resource_cache();
        }
        consumed
    }

    fn screen_reader_env_enabled() -> bool {
        for key in ["TILO_A11Y_SCREEN_READER", "TILO_SCREEN_READER"] {
            if let Ok(v) = std::env::var(key) {
                let v = v.trim();
                if !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false") {
                    return true;
                }
            }
        }
        false
    }

    fn persisted_accessibility_bool(key: &str) -> bool {
        settings_store::load_section("accessibility")
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    fn configure_screen_reader(reader: &mut crate::screen_reader::ScreenReader) {
        let section = settings_store::load_section("accessibility");
        let rate_pct = section
            .get("screen_reader_rate")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(100)
            .clamp(50, 200);
        // Always in 50..=200, so this always fits u8 losslessly.
        let rate = f32::from(u8::try_from(rate_pct).unwrap_or(100)) / 100.0;
        reader.set_rate(rate);
        let voice = section
            .get("screen_reader_voice")
            .and_then(serde_json::Value::as_str)
            .and_then(|voice| match voice {
                "english_us" => Some("en-us"),
                "english_gb" => Some("en-gb"),
                "german" => Some("de"),
                "french" => Some("fr"),
                "spanish" => Some("es"),
                _ => None,
            });
        if let Some(voice) = voice {
            reader.set_voice(voice);
        }
    }

    fn configured_screen_reader() -> crate::screen_reader::ScreenReader {
        let mut reader = crate::screen_reader::ScreenReader::new();
        Self::configure_screen_reader(&mut reader);
        reader
    }

    /// Shared truthy-env-var gate for the low-vision flags: non-empty, not
    /// `"0"`/`"false"` (case-insensitive).
    fn bool_env(keys: &[&str]) -> bool {
        for key in keys {
            if let Ok(v) = std::env::var(key) {
                let v = v.trim();
                if !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false") {
                    return true;
                }
            }
        }
        false
    }

    fn high_contrast_env_enabled() -> bool {
        Self::bool_env(&["TILO_A11Y_HIGH_CONTRAST", "TILO_HIGH_CONTRAST"])
    }

    fn reduce_motion_env_enabled() -> bool {
        Self::bool_env(&["TILO_A11Y_REDUCE_MOTION", "TILO_REDUCE_MOTION"])
    }

    /// A bad value is ignored rather than crashing the app at startup.
    fn text_scale_env() -> Option<f32> {
        for key in ["TILO_A11Y_TEXT_SCALE", "TILO_TEXT_SCALE"] {
            if let Ok(v) = std::env::var(key)
                && let Ok(f) = v.trim().parse::<f32>()
                && f.is_finite()
                && f > 0.0
            {
                return Some(f);
            }
        }
        None
    }

    /// The shell mirrors the user's UI-scale preference into spawned
    /// sandboxes via this var so an app comes up already scaled.
    fn ui_scale_env() -> Option<f32> {
        for key in ["TILO_UI_SCALE", "TILO_A11Y_UI_SCALE"] {
            if let Ok(v) = std::env::var(key)
                && let Ok(f) = v.trim().parse::<f32>()
                && f.is_finite()
                && f > 0.0
            {
                return Some(f);
            }
        }
        None
    }

    /// The screen-reader flag tracks [`Self::screen_reader_enabled`].
    pub fn a11y_settings(&self) -> crate::a11y::A11ySettings {
        let mut s = self.accessibility.a11y;
        s.screen_reader = self.accessibility.screen_reader.is_some();
        s
    }

    pub fn text_scale(&self) -> f32 {
        self.accessibility.text_scale
    }

    /// Layout grows to fit so text never clips; the value is clamped to a
    /// sane range by the theme.
    pub fn set_text_scale(&mut self, factor: f32) {
        let _entry = self.enter_wake_scope();
        self.accessibility.text_scale = factor;
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.structure_dirty = true;
    }

    pub fn ui_scale(&self) -> f32 {
        self.accessibility.ui_scale
    }

    pub fn set_ui_scale(&mut self, factor: f32) {
        let _entry = self.enter_wake_scope();
        self.accessibility.ui_scale = factor;
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.structure_dirty = true;
    }

    /// Override the host-detected input environment. Useful for platform
    /// shells and deterministic tests; normal apps do not need to call it.
    pub fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        let _entry = self.enter_wake_scope();
        if self.input.interaction == environment {
            return;
        }
        self.input.interaction = environment;
        self.root_widget.set_interaction_environment(environment);
        self.invalidate_widget_resource_cache();
        self.layout.layout_dirty = true;
    }

    pub fn interaction_environment(&self) -> crate::interaction::InteractionEnvironment {
        self.input.interaction
    }

    /// Override platform HIG conventions for a native shell or deterministic
    /// cross-platform test. Ordinary apps use the host default.
    pub fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        let _entry = self.enter_wake_scope();
        if self.input.platform == conventions {
            return;
        }
        self.input.platform = conventions;
        self.root_widget.set_platform_conventions(conventions);
        self.invalidate_widget_resource_cache();
        self.layout.layout_dirty = true;
    }

    pub fn platform_conventions(&self) -> crate::platform::PlatformConventions {
        self.input.platform
    }

    pub fn high_contrast(&self) -> bool {
        self.accessibility.a11y.high_contrast
    }

    /// Off restores the captured base theme, not a hard-coded default.
    pub fn set_high_contrast(&mut self, on: bool) {
        let _entry = self.enter_wake_scope();
        if self.accessibility.a11y.high_contrast == on {
            return;
        }
        self.accessibility.a11y.high_contrast = on;
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.structure_dirty = true;
    }

    pub fn toggle_high_contrast(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        self.set_high_contrast(!self.accessibility.a11y.high_contrast);
        self.accessibility.a11y.high_contrast
    }

    pub fn reduce_motion(&self) -> bool {
        self.accessibility.a11y.reduce_motion
    }

    pub fn set_reduce_motion(&mut self, on: bool) {
        let _entry = self.enter_wake_scope();
        if self.accessibility.a11y.reduce_motion == on {
            return;
        }
        self.accessibility.a11y.reduce_motion = on;
        self.rebuild_theme();
    }

    /// Low-vision preferences are re-applied on top, so changing the base
    /// theme doesn't discard them.
    pub fn set_theme(&mut self, theme: Theme) {
        let _entry = self.enter_wake_scope();
        self.accessibility.base_theme = theme;
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.structure_dirty = true;
    }

    /// Disabling stops in-flight speech and returns every narration call
    /// site to a no-op.
    pub fn set_screen_reader_enabled(&mut self, enabled: bool) {
        let _entry = self.enter_wake_scope();
        match (enabled, self.accessibility.screen_reader.is_some()) {
            (true, false) => {
                self.accessibility.screen_reader = Some(Self::configured_screen_reader());
                self.announce_current_focus();
            }
            (false, true) => {
                if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                    sr.stop();
                }
                self.accessibility.screen_reader = None;
                self.accessibility.live_region_last.clear();
            }
            _ => {}
        }
    }

    pub fn screen_reader_enabled(&self) -> bool {
        self.accessibility.screen_reader.is_some()
    }

    /// Installs a caller-provided reader (e.g. over a test `NullSpeaker` so
    /// headless tests can assert on spoken phrases).
    #[doc(hidden)]
    pub fn set_screen_reader(&mut self, reader: Option<crate::screen_reader::ScreenReader>) {
        let _entry = self.enter_wake_scope();
        self.accessibility.screen_reader = reader;
    }

    #[doc(hidden)]
    pub fn screen_reader(&self) -> Option<&crate::screen_reader::ScreenReader> {
        self.accessibility.screen_reader.as_ref()
    }

    fn announce_current_focus(&mut self) {
        let Some(id) = self.focused_node else { return };
        let node = self
            .inspect_node(id, |widget| widget.accessibility())
            .ok()
            .flatten();
        if let (Some(node), Some(sr)) = (node, self.accessibility.screen_reader.as_mut()) {
            sr.announce_focus(&node);
        }
    }

    /// Bindings deliberately avoid Tab (focus order), Alt+arrows / bare
    /// arrows (directional focus) and Ctrl+keys (undo / app shortcuts):
    ///
    /// | Keys | Action |
    /// |---|---|
    /// | `Insert + Down`  | Read all |
    /// | `Insert + Right` | Read next |
    /// | `Insert + Left`  | Read previous |
    /// | `Insert + Up`    | Where am I |
    /// | `Insert + H`     | Next heading |
    /// | `Insert + Shift + H` | Previous heading |
    /// | `Insert + F`     | Next form field |
    /// | `Insert + Shift + F` | Previous form field |
    /// | `Insert + Escape`| Stop |
    ///
    /// `Insert` itself is swallowed but reported as not-a-command, so it
    /// doesn't suppress the following keypress's normal handling.
    fn handle_screen_reader_key(&mut self, event: &WindowEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, NamedKey};

        if self.accessibility.screen_reader.is_none() {
            return false;
        }
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };

        if let Key::Named(NamedKey::Insert) = ke.logical_key {
            self.accessibility.screen_reader_modifier_down = ke.state == ElementState::Pressed;
            return false;
        }

        if ke.state != ElementState::Pressed || !self.accessibility.screen_reader_modifier_down {
            return false;
        }

        // Handled before the named-key arm since winit delivers H/F as
        // character keys, not named keys.
        if let Key::Character(ref c) = ke.logical_key {
            let shift = self.input.modifiers.state().shift_key();
            match c.to_ascii_lowercase().as_str() {
                "h" => {
                    if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                        if shift {
                            sr.read_prev_heading(self.root_widget.as_ref());
                        } else {
                            sr.read_next_heading(self.root_widget.as_ref());
                        }
                    }
                    return true;
                }
                "f" => {
                    if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                        if shift {
                            sr.read_prev_control(self.root_widget.as_ref());
                        } else {
                            sr.read_next_control(self.root_widget.as_ref());
                        }
                    }
                    return true;
                }
                _ => {}
            }
        }

        let Key::Named(named) = ke.logical_key else {
            return false;
        };
        match named {
            NamedKey::ArrowDown => {
                if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                    sr.read_all(self.root_widget.as_ref());
                }
                true
            }
            NamedKey::ArrowRight => {
                if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                    sr.read_next(self.root_widget.as_ref());
                }
                true
            }
            NamedKey::ArrowLeft => {
                if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                    sr.read_prev(self.root_widget.as_ref());
                }
                true
            }
            NamedKey::ArrowUp => {
                self.announce_current_focus();
                true
            }
            NamedKey::Escape => {
                if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                    sr.stop();
                }
                true
            }
            _ => false,
        }
    }

    /// Reconciles both directions, idempotently: app-driven (push
    /// `fullscreen_intent` to the window on change) and compositor-driven
    /// (read the actual state back and notify the root on a transition, e.g.
    /// a global un-fullscreen keybind or a denied request).
    fn reconcile_fullscreen(&mut self, window: &Window) -> bool {
        let mut changed = false;

        let want = self.root_widget.fullscreen_intent();
        if let Some(new_state) = fullscreen_request_decision(self.fullscreen_requested, want) {
            if new_state {
                // An empty monitor list falls back to `Borderless(None)`,
                // still a full-output surface.
                let params = self.root_widget.fullscreen_params();
                let monitors: Vec<winit::monitor::MonitorHandle> =
                    window.available_monitors().collect();
                let primary = window.primary_monitor();
                let infos: Vec<crate::fullscreen::MonitorInfo> = monitors
                    .iter()
                    .enumerate()
                    .map(|(i, m)| crate::fullscreen::MonitorInfo {
                        index: u32::try_from(i).unwrap_or(u32::MAX),
                        width: m.size().width,
                        height: m.size().height,
                        primary: primary.as_ref() == Some(m),
                        scale: m.scale_factor(),
                    })
                    .collect();
                let chosen = crate::fullscreen::select_output(&infos, params.target)
                    .and_then(|idx| monitors.get(idx as usize).cloned());
                window.set_fullscreen(Some(Fullscreen::Borderless(chosen)));
            } else {
                window.set_fullscreen(None);
            }
            self.fullscreen_requested = new_state;
            changed = true;
        }

        let actual = window.fullscreen().is_some();
        changed |= self.note_actual_fullscreen(actual);

        changed
    }

    /// Split from [`Self::reconcile_fullscreen`] so the Appâ†’root notification
    /// wiring is unit-testable without a live winit window.
    fn note_actual_fullscreen(&mut self, actual: bool) -> bool {
        if actual == self.fullscreen_active {
            return false;
        }
        self.fullscreen_active = actual;
        self.root_widget.on_fullscreen_changed(actual);
        self.needs_sync = true;
        self.layout.layout_dirty = true;
        true
    }

    /// Level-triggered: a no-op once the user's choice is caught up.
    pub(crate) fn reconcile_appearance(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        if !self.appearance_reconcile {
            return false;
        }
        let Some(intent) = find_appearance_intent(self.root_widget.as_ref()) else {
            return false;
        };
        let mut changed = false;
        if intent.base_theme.name != self.accessibility.base_theme.name {
            self.accessibility.base_theme = intent.base_theme;
            changed = true;
        }
        if intent.high_contrast != self.accessibility.a11y.high_contrast {
            self.accessibility.a11y.high_contrast = intent.high_contrast;
            changed = true;
        }
        if intent.reduce_motion != self.accessibility.a11y.reduce_motion {
            self.accessibility.a11y.reduce_motion = intent.reduce_motion;
            changed = true;
        }
        if (intent.text_scale - self.accessibility.text_scale).abs() > f32::EPSILON {
            self.accessibility.text_scale = intent.text_scale;
            changed = true;
        }
        if (intent.ui_scale - self.accessibility.ui_scale).abs() > f32::EPSILON {
            self.accessibility.ui_scale = intent.ui_scale;
            changed = true;
        }
        if changed {
            self.rebuild_theme();
            self.layout.layout_dirty = true;
            self.layout.structure_dirty = true;
        }
        changed
    }

    fn reconcile_configured_theme(&mut self) -> bool {
        self.reconcile_configured_theme_with(
            Instant::now(),
            || {
                settings_store::settings_path()
                    .and_then(|path| std::fs::metadata(path).ok()?.modified().ok())
            },
            Self::apply_configured_theme,
        )
    }

    fn reconcile_configured_theme_with(
        &mut self,
        now: Instant,
        stamp: impl FnOnce() -> Option<std::time::SystemTime>,
        apply: impl FnOnce(&mut Self),
    ) -> bool {
        // Only an explicitly native service host watches process settings.
        if !self.watch_system_settings {
            return false;
        }
        if now.saturating_duration_since(self.last_theme_store_check) < Duration::from_millis(750) {
            return false;
        }
        self.last_theme_store_check = now;
        let stamp = stamp();
        if stamp == self.theme_store_stamp {
            return false;
        }
        self.theme_store_stamp = stamp;
        apply(self);
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.structure_dirty = true;
        true
    }

    fn apply_configured_theme(&mut self) {
        self.accessibility.base_theme = Theme::configured();
        // Screen reader is a device/personal setting, not a per-application
        // opt-in. Every Tilo app watches the same file and changes its real
        // narrator lifecycle when Personal Settings changes it.
        let configured_reader = Self::persisted_accessibility_bool("screen_reader")
            || Self::screen_reader_env_enabled();
        self.set_screen_reader_enabled(configured_reader);
        if let Some(reader) = self.accessibility.screen_reader.as_mut() {
            Self::configure_screen_reader(reader);
        }
        self.accessibility.a11y.screen_reader = self.accessibility.screen_reader.is_some();
    }

    /// The smallest-area matching rect wins, so a nested child beats its
    /// enclosing container. Works off `node_map` rather than walking
    /// `children` so it still reaches widgets nested under containers with
    /// private child storage.
    fn resolve_cursor(&self, point: Point) -> Option<CursorIcon> {
        let mut best: Option<(f64, CursorIcon)> = None;
        for &node in self.layout.node_map.keys() {
            let Ok((rect, icon)) = self.inspect_node(node, |widget| {
                (widget.get_rect(), widget.cursor_shape(point))
            }) else {
                continue;
            };
            if rect.contains(point)
                && let Some(icon) = icon
            {
                let area = (rect.width() * rect.height()).max(0.0);
                if best.is_none_or(|(a, _)| area < a) {
                    best = Some((area, icon));
                }
            }
        }
        best.map(|(_, icon)| icon)
    }

    /// Mirrors [`Self::resolve_cursor`]: smallest-area widget under `point`
    /// exposing a non-empty [`Widget::tooltip`], so a nested icon button
    /// beats its enclosing toolbar.
    fn tooltip_target_at(&self, point: Point) -> Option<(vello::kurbo::Rect, String)> {
        if !self.input.interaction.supports_hover() {
            return None;
        }
        let mut best: Option<(f64, vello::kurbo::Rect, String)> = None;
        for &node in self.layout.node_map.keys() {
            let Ok(target) = self.inspect_node(node, |widget| widget.tooltip_target_at(point))
            else {
                continue;
            };
            if let Some((rect, tip)) = target {
                let area = (rect.width() * rect.height()).max(0.0);
                if best.as_ref().is_none_or(|(a, _, _)| area < *a) {
                    best = Some((area, rect, tip));
                }
            }
        }
        best.map(|(_, rect, label)| (rect, label))
    }

    /// Called once per event-loop wakeup, so it must be cheap and
    /// non-blocking.
    pub fn set_frame_tick<F>(&mut self, tick: F)
    where
        F: FnMut(&mut PresenceCtx) -> bool + Send + Sync + 'static,
    {
        self.presence.frame_tick = Some(Box::new(tick));
    }

    pub fn set_presence_broadcast_sink<F>(&mut self, sink: F)
    where
        F: Fn(&PresenceKey, &PresencePayload) + Send + Sync + 'static,
    {
        self.presence.broadcast_sink = Some(Box::new(sink));
    }

    pub fn clear_presence_broadcast_sink(&mut self) {
        self.presence.broadcast_sink = None;
    }

    pub fn set_pointer_space(&mut self, space: crate::presence::PointerSpace) {
        self.presence.pointer_space = Some(space);
    }

    fn local_pointer_key(&self) -> PresenceKey {
        match self.presence.pointer_space.clone() {
            Some(space) => PresenceKey::Pointer { space },
            None => PresenceKey::local_surface_pointer(),
        }
    }

    /// Decides *whether* to publish and under what scope, not what space the
    /// pointer is in â€” see [`Self::set_pointer_space`].
    pub fn set_presence_broadcast_scope(&mut self, key: Option<PresenceKey>) {
        let _entry = self.enter_wake_scope();
        self.presence.broadcast_scope.clone_from(&key);
        // Reset the throttle so a new scope's first move fires immediately.
        self.presence.last_pointer_broadcast = None;
        // Arrival remains ephemeral presence only. Document catch-up is a
        // broker-directed request with one selected snapshot responder.
        if let Some(k) = key {
            tracing::debug!(
                target: "tilo.state",
                event = "presence_arrived",
                connected = self.collaboration.manager.is_connected(),
                "publishing collaboration presence"
            );
            self.broadcast_presence(k, crate::collaboration::PresencePayload::Arrived);
        }
    }

    pub fn presence_broadcast_scope(&self) -> Option<&PresenceKey> {
        self.presence.broadcast_scope.as_ref()
    }

    pub fn presence_ctx(&self) -> &PresenceCtx {
        &self.presence_ctx
    }
    pub fn presence_ctx_mut(&mut self) -> &mut PresenceCtx {
        &mut self.presence_ctx
    }

    pub fn set_followed_peer(&mut self, user_id: Option<String>) {
        self.presence.followed_peer = user_id;
    }

    pub fn followed_peer(&self) -> Option<&str> {
        self.presence.followed_peer.as_deref()
    }

    pub fn followed_peer_target(&self) -> Option<[f32; 2]> {
        let id = self.presence.followed_peer.as_deref()?;
        self.presence_ctx.peer_pointer(id)
    }

    /// A miss leaves the current follow target untouched.
    pub fn try_follow_click(
        &mut self,
        viewport: vello::kurbo::Rect,
        point: vello::kurbo::Point,
    ) -> bool {
        let _entry = self.enter_wake_scope();
        if let Some(uid) = self.presence_ctx.hit_test_offscreen_mist(viewport, point) {
            self.presence.followed_peer = Some(uid);
            true
        } else {
            false
        }
    }

    pub fn permission_ctx(&self) -> &PermissionContext {
        &self.permission_ctx
    }
    pub fn set_permission_ctx(&mut self, ctx: PermissionContext) {
        let _entry = self.enter_wake_scope();
        self.permission_ctx = ctx;
        // See `invalidate_widget_resource_cache`: once the numeric namespace
        // is exhausted, dropping the projection is the collision-free way to
        // make the changed permissions visible without reusing a live cache.
        if let Some(next) = self.actions.permission_generation.get().checked_add(1) {
            self.actions.permission_generation.set(next);
        }
        self.actions.action_directory_snapshot = None;
        self.actions.action_directory_stamp = None;
        self.actions.action_directory_cursors.clear();
        self.actions.local_resource_cache.get_mut().take();
        self.actions.open_resource_search_cache.get_mut().take();
        self.actions.last_control_availability = None;
        self.refresh_control_action_availability();
        self.layout.layout_dirty = true;
    }

    /// The server attributes the broadcast to this user's id automatically,
    /// so callers only specify what changed.
    pub fn broadcast_presence(
        &mut self,
        key: crate::presence::PresenceKey,
        payload: crate::collaboration::PresencePayload,
    ) {
        let _entry = self.enter_wake_scope();
        // Fan out to the optional out-of-band sink first (the shell's Iroh
        // bridge) so a slow socket never delays the network path.
        if let Some(sink) = self.presence.broadcast_sink.as_ref() {
            sink(&key, &payload);
        }
        self.collaboration
            .manager
            .send(CollabRequest::Presence { key, payload });
    }

    /// Publish the collaboration rail's current status through the real room
    /// presence path. `None` is an explicit clear. Only workspace keys are
    /// accepted, preventing an intention from being accidentally scoped to a
    /// single widget and disappearing as the participant navigates.
    pub fn broadcast_status(
        &mut self,
        workspace: crate::presence::PresenceKey,
        status: Option<&crate::live_collaboration::LiveStatus>,
    ) -> bool {
        if !matches!(workspace, crate::presence::PresenceKey::Workspace { .. }) {
            return false;
        }
        let (text, automatic) = status.map_or_else(
            || (String::new(), false),
            |status| {
                (
                    status.text.clone(),
                    matches!(
                        status.origin,
                        crate::live_collaboration::StatusOrigin::Inferred { .. }
                    ),
                )
            },
        );
        self.broadcast_presence(
            workspace,
            crate::collaboration::PresencePayload::Status { text, automatic },
        );
        true
    }

    /// Live-edit path (the late-join handshake still ships a full snapshot).
    fn broadcast_doc_delta(&mut self) {
        if self.collaboration.manager.is_connected() {
            match self.pending_primary_document_delta() {
                Ok((delta, heads)) if !delta.is_empty() => {
                    tracing::debug!(
                        target: "tilo.state",
                        event = "document_send",
                        kind = "incremental",
                        bytes = delta.len(),
                        heads = heads.len(),
                        "queueing document mutation"
                    );
                    if self.collaboration.manager.send(
                        crate::collaboration::CollabRequest::document_incremental(delta),
                    ) {
                        self.advance_primary_document(heads);
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "collab: primary document is unavailable");
                }
            }
        }
        self.broadcast_room_index_delta();
    }

    /// The room index is a distinct document from the live record.  Keep its
    /// incremental cursor and route separate so an annotation/document delta
    /// can never be merged into the index (or vice versa).
    fn broadcast_room_index_delta(&mut self) {
        let connected = self.collaboration.room_manager.as_ref().map_or_else(
            || self.persistence.records.is_some() && self.collaboration.manager.is_connected(),
            CollaborationManager::is_connected,
        );
        if !connected {
            return;
        }
        let Some(room) = self.persistence.room_index.as_mut() else {
            return;
        };
        let (delta, heads) = room.pending_outbound_delta();
        if delta.is_empty() {
            return;
        }
        if let Some(manager) = self.collaboration.room_manager.as_mut() {
            if manager.is_connected()
                && manager.send(crate::collaboration::CollabRequest::document_incremental(
                    delta,
                ))
            {
                room.advance_outbound(heads);
            }
        } else if self.persistence.records.is_some() && self.collaboration.manager.is_connected() {
            // Before the first record checkout the primary manager is the
            // room route. It becomes `collaboration.room_manager` at that checkout.
            // Do not advance this cursor for ordinary (non-record) apps.
            if self.collaboration.manager.send(
                crate::collaboration::CollabRequest::document_incremental(delta),
            ) {
                room.advance_outbound(heads);
            }
        }
    }

    fn broadcast_focus_presence(&mut self) {
        use crate::collaboration::PresencePayload;
        let current = self.focused_node.and_then(|id| {
            self.inspect_node(id, |widget| widget.presence_payload())
                .ok()
                .flatten()
        });
        if current != self.presence.last_focus {
            // When the caret advances forward under the same scope, publish
            // `Typing` for the inserted run so peers light up a fading
            // highlighter. Backspace/left-arrow and scope changes don't
            // qualify.
            if let (
                Some((new_key, PresencePayload::Caret { pos: new_pos })),
                Some((old_key, PresencePayload::Caret { pos: old_pos })),
            ) = (&current, &self.presence.last_focus)
                && new_key == old_key
                && new_pos > old_pos
            {
                self.broadcast_presence(
                    new_key.clone(),
                    PresencePayload::Typing {
                        start: *old_pos,
                        end: *new_pos,
                    },
                );
            }
            if let Some((key, payload)) = current.clone() {
                self.broadcast_presence(key, payload);
            }
            self.presence.last_focus = current;
        }
    }

    /// Unlike focus presence, this is polled on the root regardless of
    /// focus.
    fn broadcast_location_presence(&mut self) {
        let current = self.root_widget.presence_payload();
        if current != self.presence.last_location {
            if let Some((key, payload)) = current.clone() {
                self.broadcast_presence(key, payload);
            }
            self.presence.last_location = current;
        }
    }

    /// The subset has no `PresencePayload` variant of its own â€” it rides the
    /// existing presence frame inside a
    /// [`PresenceKey::view`](crate::presence::PresenceKey::view) key with an
    /// inert paired payload; the receiver recognises the key and decodes the
    /// subset instead.
    fn broadcast_view_presence(&mut self) {
        if !self.collaboration.manager.is_connected() && self.presence.broadcast_sink.is_none() {
            return;
        }
        let view = <Self as crate::view::FreezeThaw>::capture_view(self);
        let subset = crate::view::ViewSubset::from_view(&view);
        if self.presence.last_view.as_ref() == Some(&subset) {
            return;
        }
        self.broadcast_presence(
            crate::presence::PresenceKey::view(&subset),
            // Inert: the receiver dispatches on the key before inspecting
            // the payload; `Arrived` because the SDK transport relays it
            // with the routing key intact.
            crate::collaboration::PresencePayload::Arrived,
        );
        self.presence.last_view = Some(subset);
    }

    /// Following a peer who hasn't published a View yet latches the intent
    /// without moving anything; the viewport snaps to them once their first
    /// View arrives.
    pub fn follow(&mut self, peer_id: impl Into<String>) {
        let _entry = self.enter_wake_scope();
        self.presence.followed_peer = Some(peer_id.into());
        self.apply_follow();
    }

    pub fn unfollow(&mut self) {
        let _entry = self.enter_wake_scope();
        self.presence.followed_peer = None;
    }

    fn apply_follow(&mut self) {
        let Some(id) = self.presence.followed_peer.clone() else {
            return;
        };
        let Some(view) = self
            .presence_ctx
            .peer_view(&crate::permissions::UserId(id))
            .cloned()
        else {
            return;
        };
        self.root_widget
            .restore_view_state(&crate::view::ViewContribution {
                scroll: view.scroll.clone(),
                view_mode: view.view_mode,
            });
        self.layout.layout_dirty = true;
        self.needs_sync = true;
    }

    /// Lets integration tests drive the collab pump without a real winit
    /// event loop.
    #[doc(hidden)]
    pub fn poll_and_apply_collab_events(&mut self) {
        let _entry = self.enter_wake_scope();
        self.pump_room_index_events();
        let events = self.collaboration.manager.poll();
        for event in events {
            self.apply_collab_event_for_test(event);
        }
    }

    /// Apply a document envelope received on the unqualified room route.
    /// The broker authenticated the route; discovery still validates and
    /// bounds its names before exposing them to the app.
    fn apply_room_index_document_envelope(
        &mut self,
        envelope: &collab_types::SignedOp,
        decrypted_payload: Option<&[u8]>,
    ) -> bool {
        let Some(room) = self.persistence.room_index.as_mut() else {
            return false;
        };
        let applied = match collab_types::DocumentMutation::decode(
            decrypted_payload.unwrap_or(&envelope.payload),
        ) {
            Ok(collab_types::DocumentMutation::Snapshot(data)) => {
                room.merge_snapshot(&data).is_ok()
            }
            Ok(collab_types::DocumentMutation::Incremental(data)) => {
                room.load_incremental(&data).is_ok()
            }
            Err(error) => {
                tracing::warn!(%error, "collab: broker delivered an untyped room-index op");
                false
            }
        };
        if applied
            && let (Some(records), Some(room)) = (
                self.persistence.records.as_mut(),
                self.persistence.room_index.as_ref(),
            )
        {
            records.discover_room(room.document());
            self.invalidate_widget_resource_cache();
        }
        applied
    }

    /// Drain only document traffic on the room-index route. Actions and
    /// presence are record scoped, so an index update cannot alter the app's
    /// resource/action namespace or presentation state.
    fn pump_room_index_events(&mut self) -> bool {
        let Some(manager) = self.collaboration.room_manager.as_mut() else {
            return false;
        };
        let events = manager.poll();
        let mut any = false;
        for event in events {
            match event {
                CollabEvent::Document {
                    envelope,
                    decrypted_payload,
                    ..
                } => {
                    any |= self.apply_room_index_document_envelope(
                        &envelope,
                        decrypted_payload.as_deref(),
                    );
                }
                CollabEvent::ScrollbackBatch {
                    ops,
                    decrypted_payloads,
                    ..
                } => {
                    for (index, envelope) in ops.into_iter().enumerate() {
                        any |= self.apply_room_index_document_envelope(
                            &envelope,
                            decrypted_payloads
                                .get(index)
                                .and_then(|payload| payload.as_deref()),
                        );
                    }
                }
                CollabEvent::DocumentSnapshotRequested { .. } => {
                    if let (Some(manager), Some(room)) = (
                        self.collaboration.room_manager.as_mut(),
                        self.persistence.room_index.as_mut(),
                    ) {
                        let snapshot = room.document_mut().save();
                        let _ = manager.send(
                            crate::collaboration::CollabRequest::document_snapshot(snapshot),
                        );
                    }
                }
                _ => {}
            }
        }
        any
    }

    /// Shared by the per-frame tick and the headless test path. Returns
    /// `true` iff at least one event was processed.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered pump drains directory callbacks and applies every collaboration lifecycle event before the next frame"
    )]
    pub fn pump_collab_presence(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        let mut any = self.pump_room_index_events();
        // Replies to broker pull-directory callbacks must run before regular
        // inbound events. This is the sole place capability requests acquire
        // widget/document authority, and it always runs on the App owner
        // thread.
        self.pump_action_directory_requests();
        self.refresh_document_routes();
        self.drain_document_routes();
        self.publish_owned_documents();
        for request in crate::widget::take_collab_requests(self.root_widget.as_mut()) {
            self.collaboration.manager.send(request);
        }
        let events = self.collaboration.manager.poll();
        let mut action_surface_may_have_changed = false;
        for event in events {
            any = true;
            action_surface_may_have_changed |= Self::collab_event_may_change_action_surface(&event);
            self.forward_session_notification(&event);
            match event {
                CollabEvent::Context { collaborators, .. } => {
                    self.apply_collab_context(collaborators);
                }
                CollabEvent::FollowChanged { peer_id } => match peer_id {
                    Some(peer_id) => self.follow(peer_id),
                    None => self.unfollow(),
                },
                CollabEvent::DocumentSnapshotRequested { .. } => {
                    match self.primary_document_snapshot() {
                        Ok(data) => {
                            let _ = self
                                .collaboration
                                .manager
                                .send(crate::collaboration::CollabRequest::document_snapshot(data));
                        }
                        Err(error) => {
                            tracing::warn!(%error, "collab: primary snapshot refused");
                        }
                    }
                }
                CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                    self.presence_ctx
                        .upsert_remote_cursor(user_id, [x, y], None);
                }
                CollabEvent::RemotePresenceUpdate {
                    user_id,
                    key,
                    payload,
                } => {
                    self.apply_remote_presence(user_id, key, payload);
                }
                CollabEvent::ActionArrived {
                    type_id,
                    payload,
                    from_user_id,
                } => {
                    self.apply_collab_action_arrived(&type_id, &payload, &from_user_id);
                }
                CollabEvent::Document {
                    envelope,
                    decrypted_payload,
                    ..
                } => {
                    // The broker already validated the envelope (signature +
                    // role table + replay defence); the toolkit trusts it.
                    tracing::debug!(
                        target: "tilo.state",
                        event = "document_receive",
                        bytes = envelope.payload.len(),
                        author = ?envelope.author,
                        sequence = envelope.seq,
                        encrypted = envelope.encryption.is_some(),
                        "applying document mutation"
                    );
                    self.apply_signed_document_envelope(&envelope, decrypted_payload.as_deref());
                }
                CollabEvent::ScrollbackBatch {
                    artifact: _,
                    ops,
                    decrypted_payloads,
                    exhausted: _,
                } => {
                    // Broker pre-validated each envelope, trusted the same
                    // way as a live document mutation.
                    if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                        crate::diag!("[app] recv ScrollbackBatch ({} ops)", ops.len());
                    }
                    self.apply_document_scrollback(ops, &decrypted_payloads);
                }
                CollabEvent::Invocation {
                    id,
                    resource,
                    action,
                    argument,
                    actor,
                    context,
                } => {
                    self.invoke_published_action(
                        id,
                        resource,
                        action,
                        argument.as_deref(),
                        &actor,
                        context.as_deref(),
                    );
                }
                CollabEvent::InvocationCancellation { id } => {
                    self.request_invocation_cancellation(id);
                }
                CollabEvent::PublishedInvocationResult { request_id, result } => {
                    if self.close_broker_result(request_id, &result) {
                        continue;
                    }
                    let Some(invocation) = self
                        .actions
                        .pending_application_launches
                        .remove(&request_id)
                    else {
                        continue;
                    };
                    let Some(pending) = self.actions.directed_invocations.remove(&invocation)
                    else {
                        continue;
                    };
                    let update = match result {
                        Ok((_id, tilo_actions::InvocationUpdate::Succeeded { output, .. })) => {
                            tilo_actions::InvocationUpdate::Succeeded { invocation, output }
                        }
                        Ok((_id, tilo_actions::InvocationUpdate::Failed { error, .. })) => {
                            tilo_actions::InvocationUpdate::Failed { invocation, error }
                        }
                        Ok((_id, tilo_actions::InvocationUpdate::Cancelled { message, .. })) => {
                            tilo_actions::InvocationUpdate::Cancelled {
                                invocation,
                                message,
                            }
                        }
                        Ok((_id, other)) => tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "application_launch_invalid_receipt".into(),
                                message: format!(
                                    "application launch returned non-terminal {other:?}"
                                ),
                                details: None,
                                retryable: false,
                            },
                        },
                        Err(message) => tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "application_launch_failed".into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        },
                    };
                    self.collaboration
                        .manager
                        .send(CollabRequest::UpdateInvocation {
                            id: pending.wire_id,
                            update,
                        });
                }
                // The generic catalogue/client responses are consumed by
                // widgets such as Agent through their collaboration event
                // hook; an ordinary application has no local state to apply.
                CollabEvent::ActionCatalogue { .. }
                | CollabEvent::TaskRun(_)
                | CollabEvent::TaskActionInvocationResult { .. }
                | CollabEvent::TaskRunCancellation { .. }
                | CollabEvent::WorkspaceControl { .. }
                | CollabEvent::RealtimeCommandResult { .. }
                | CollabEvent::NearbyTransferCommandResult { .. }
                | CollabEvent::Realtime(_) => {}
            }
        }
        if any {
            self.invalidate_widget_resource_cache();
        }
        let published_snapshot_was_evicted = self.collaboration.manager.is_connected()
            && self.actions.last_published_actions.is_some()
            && self.actions.action_directory_snapshot.is_none();
        if action_surface_may_have_changed || published_snapshot_was_evicted {
            // The normal idle pump also repairs invalidation from a later
            // model sync or a presence-only batch. Neither necessarily brings
            // another action event after a terminal operation was published.
            // Query/resolve remain bounded reads; this refresh runs even with
            // no directory requests or inbound events, and skips stable idle.
            self.publish_action_surface();
        }
        any
    }

    fn collab_event_may_change_action_surface(event: &CollabEvent) -> bool {
        !matches!(
            event,
            CollabEvent::FollowChanged { .. }
                | CollabEvent::RemoteCursorMoved { .. }
                | CollabEvent::RemotePresenceUpdate { .. }
        )
    }

    /// The self row carries the address others use for this participant (so
    /// a locally-derived id is recognised in a room-keyed roster) and the
    /// host-assigned name; it never enters the roster itself.
    fn apply_collab_context(&mut self, collaborators: Vec<crate::collaboration::Collaborator>) {
        for participant in &collaborators {
            self.presence_ctx.set_participant_kind(
                crate::permissions::UserId(participant.id.clone()),
                participant.is_agent,
            );
        }
        if let Some(me) = collaborators.iter().find(|c| c.is_self) {
            self.presence_ctx
                .set_self_participant(crate::permissions::UserId(me.id.clone()), me.name.clone());
        }
        // Filtered to others: letting the self row through would put the
        // local user in every list built from presence.
        for c in collaborators.into_iter().filter(|c| !c.is_self) {
            // Relabel only â€” the roster doesn't say where a peer's pointer
            // is, so it must not mint one at the origin.
            self.presence_ctx
                .set_display_name(crate::permissions::UserId(c.id.clone()), c.name.clone());
            self.presence_ctx.set_participant_kind(
                crate::permissions::UserId(c.id),
                if c.is_agent {
                    crate::presence::ParticipantKind::Agent
                } else {
                    crate::presence::ParticipantKind::Person
                },
            );
        }
    }

    /// Every app in the room is handed every action; the one whose registry
    /// knows the type applies it, so the owner stays the only writer.
    /// `before` MUST be read ahead of apply: a baseline left behind the
    /// incoming change means the user's next Ctrl+Z reverts someone else's
    /// edit along with their own keystroke.
    fn apply_collab_action_arrived(&mut self, type_id: &str, payload: &[u8], from_user_id: &str) {
        let before = self.state.doc.get_heads();
        let applied = self
            .root_widget
            .apply_room_action(&mut self.state.doc, type_id, payload);
        if applied {
            self.invalidate_widget_resource_cache();
            // Named after the publisher so local Ctrl+Z walks past it
            // instead of through it.
            self.state.record_committed_action_authored(
                before,
                None,
                None,
                Some(crate::permissions::UserId(from_user_id.to_string())),
            );
            self.needs_sync = true;
            self.layout.structure_dirty = true;
        }
    }

    /// Bypasses the collab manager's socket/poll path â€” for tests using a
    /// `MemoryRelay`.
    #[doc(hidden)]
    // This test adapter deliberately mirrors the complete collaboration event
    // match so lifecycle tests exercise the same routing as the runtime loop.
    #[allow(
        clippy::too_many_lines,
        reason = "test adapter mirrors complete collaboration event routing for lifecycle coverage"
    )]
    pub fn apply_collab_event_for_test(&mut self, event: CollabEvent) {
        let _entry = self.enter_wake_scope();
        let action_surface_may_have_changed = Self::collab_event_may_change_action_surface(&event);
        self.forward_session_notification(&event);
        match event {
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key,
                payload,
            } => {
                self.apply_remote_presence(user_id, key, payload);
            }
            CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                self.presence_ctx
                    .upsert_remote_cursor(user_id, [x, y], None);
            }
            CollabEvent::Document {
                envelope,
                decrypted_payload,
                ..
            } => {
                self.apply_signed_document_envelope(&envelope, decrypted_payload.as_deref());
            }
            CollabEvent::ScrollbackBatch {
                ops,
                decrypted_payloads,
                ..
            } => {
                self.apply_document_scrollback(ops, &decrypted_payloads);
            }
            CollabEvent::FollowChanged { peer_id } => match peer_id {
                Some(peer_id) => self.follow(peer_id),
                None => self.unfollow(),
            },
            CollabEvent::DocumentSnapshotRequested { .. } => {
                let data = self.state.save();
                let _ = self
                    .collaboration
                    .manager
                    .send(crate::collaboration::CollabRequest::document_snapshot(data));
            }
            CollabEvent::Context { collaborators, .. } => {
                self.apply_collab_context(collaborators);
            }
            CollabEvent::Invocation {
                id,
                resource,
                action,
                argument,
                actor,
                context,
            } => self.invoke_published_action(
                id,
                resource,
                action,
                argument.as_deref(),
                &actor,
                context.as_deref(),
            ),
            CollabEvent::InvocationCancellation { id } => {
                self.request_invocation_cancellation(id);
            }
            CollabEvent::PublishedInvocationResult { request_id, result } => {
                if self.close_broker_result(request_id, &result) {
                    return;
                }
                let Some(invocation) = self
                    .actions
                    .pending_application_launches
                    .remove(&request_id)
                else {
                    return;
                };
                let Some(pending) = self.actions.directed_invocations.remove(&invocation) else {
                    return;
                };
                let update = match result {
                    Ok((_id, tilo_actions::InvocationUpdate::Succeeded { output, .. })) => {
                        tilo_actions::InvocationUpdate::Succeeded { invocation, output }
                    }
                    Ok((_id, tilo_actions::InvocationUpdate::Failed { error, .. })) => {
                        tilo_actions::InvocationUpdate::Failed { invocation, error }
                    }
                    Ok((_id, tilo_actions::InvocationUpdate::Cancelled { message, .. })) => {
                        tilo_actions::InvocationUpdate::Cancelled {
                            invocation,
                            message,
                        }
                    }
                    Ok((_id, other)) => tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "application_launch_invalid_receipt".into(),
                            message: format!("application launch returned non-terminal {other:?}"),
                            details: None,
                            retryable: false,
                        },
                    },
                    Err(message) => tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "application_launch_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    },
                };
                self.collaboration
                    .manager
                    .send(CollabRequest::UpdateInvocation {
                        id: pending.wire_id,
                        update,
                    });
            }
            _ => {}
        }
        self.invalidate_widget_resource_cache();
        if action_surface_may_have_changed {
            self.publish_action_surface();
        }
    }

    pub fn local_author(&self) -> &crate::permissions::UserId {
        &self.collaboration.local_author
    }

    #[doc(hidden)]
    pub fn state_mut_for_test(&mut self) -> &mut crate::state::State {
        self.invalidate_widget_resource_cache();
        &mut self.state
    }

    fn update_focusable_order(&mut self) {
        fn collect_focusable(
            node_id: NodeId,
            widget: &dyn Widget,
            taffy: &TaffyTree<()>,
            order: &mut Vec<NodeId>,
        ) {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
                return;
            };
            if widget.is_focusable() && widget.is_enabled() {
                order.push(node_id);
            }
            let children = widget.children();
            if let Ok(child_nodes) = taffy.children(node_id) {
                for (child_widget, &child_node) in children.iter().zip(child_nodes.iter()) {
                    collect_focusable(child_node, *child_widget, taffy, order);
                }
            }
        }
        // The deepest trapping widget wins (a modal opened from within one).
        fn find_trap_focusables(
            node_id: NodeId,
            widget: &dyn Widget,
            taffy: &TaffyTree<()>,
        ) -> Option<Vec<NodeId>> {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
                return None;
            };
            let children = widget.children();
            let child_nodes = taffy.children(node_id).ok();
            // Recurse first so a nested trap takes precedence.
            if let Some(child_nodes) = &child_nodes {
                for (child_widget, &child_node) in children.iter().zip(child_nodes.iter()) {
                    if let Some(inner) = find_trap_focusables(child_node, *child_widget, taffy) {
                        return Some(inner);
                    }
                }
            }
            if widget.traps_focus() {
                let mut order = Vec::new();
                collect_focusable(node_id, widget, taffy, &mut order);
                return Some(order);
            }
            None
        }

        if self.history.is_some()
            || crate::history_mode::embedded::has_surfaces(self.root_widget.as_ref())
        {
            self.update_history_focus_order();
            return;
        }
        self.focusable_order.clear();
        let Some(root) = self.layout.root_node else {
            return;
        };

        let (trap_active, order) = self
            .with_native_presentation(|widget, _, _, taffy| {
                let trap = find_trap_focusables(root, widget, taffy);
                if let Some(order) = trap {
                    (true, order)
                } else {
                    let mut order = Vec::new();
                    collect_focusable(root, widget, taffy, &mut order);
                    (false, order)
                }
            })
            .unwrap_or_default();
        self.focusable_order = order;
        match (self.accessibility.focus_trap_active, trap_active) {
            (false, true) => {
                // Modal just opened: remember prior focus, move into the trap.
                self.accessibility.focus_before_trap = self.focused_node;
                if let Some(&first) = self.focusable_order.first() {
                    self.set_focused_node(Some(first));
                } else {
                    // No focusable content â€” drop focus so Tab can't reach
                    // the content behind the backdrop.
                    self.set_focused_node(None);
                }
            }
            (true, false) => {
                // Modal closed: restore only if that element still exists.
                let restore = self
                    .accessibility
                    .focus_before_trap
                    .take()
                    .filter(|id| self.focusable_order.contains(id));
                self.set_focused_node(restore);
            }
            (true, true) => {
                // Still trapped: pull focus back if a rebuild dropped it
                // outside the modal.
                let inside = self
                    .focused_node
                    .is_some_and(|id| self.focusable_order.contains(&id));
                if !inside {
                    self.set_focused_node(self.focusable_order.first().copied());
                }
            }
            (false, false) => {}
        }
        self.accessibility.focus_trap_active = trap_active;
    }

    /// The decode lives on [`crate::presence::PresenceCtx`] so the
    /// compositor reads presence the same way an app does; what stays here
    /// is what only a document holder can do (follow a peer's viewport,
    /// answer a late joiner with full state).
    fn apply_remote_presence(
        &mut self,
        user_id: String,
        key: crate::presence::PresenceKey,
        mut payload: crate::collaboration::PresencePayload,
    ) {
        use crate::presence::RemotePresenceEffect;
        // Domain actions arrive without UI coordinates by design. If this
        // frame exposes the same address through one or more controls, bind
        // the interaction to the preferred live semantic route before the
        // monotonic presence store sees it. Every later phase resolves by the
        // same address, so identity remains stable while menus/toolbars move.
        if let crate::collaboration::PresencePayload::Interaction(update) = &mut payload
            && matches!(&update.anchor, tilo_actions::InteractionAnchor::Resource)
        {
            let address = tilo_actions::ActionAddress::new(
                update.resource.id.clone(),
                update.action.id.clone(),
            );
            if let Some(route) = self.preferred_action_ui_route(&address)
                && let Some(target) = route.path.last()
            {
                update.anchor = tilo_actions::InteractionAnchor::SemanticControl {
                    id: target.as_str().to_string(),
                };
            }
        }
        let interaction = match &payload {
            crate::collaboration::PresencePayload::Interaction(update) => Some((**update).clone()),
            _ => None,
        };
        match self
            .presence_ctx
            .apply_remote_presence(user_id, key, payload)
        {
            RemotePresenceEffect::None | RemotePresenceEffect::PeerArrived => {}
            RemotePresenceEffect::PeerViewChanged => self.apply_follow(),
            RemotePresenceEffect::InteractionAdvanced => {
                if let Some(interaction) = interaction
                    && let Some(text) = self.presence_ctx.interaction_announcement(&interaction)
                    && let Some(reader) = self.accessibility.screen_reader.as_mut()
                {
                    let assertive = matches!(
                        interaction.phase,
                        tilo_actions::InteractionPhase::Failed { .. }
                    );
                    reader.announce(
                        text,
                        if assertive {
                            tts::Priority::Important
                        } else {
                            tts::Priority::Notification
                        },
                        assertive,
                    );
                }
            }
        }
    }

    fn apply_undo_entry(&mut self, entry: crate::state::UndoEntry) {
        if let Some(idx) = entry.focused_node_idx {
            if let Some(&id) = self.focusable_order.get(idx) {
                self.set_focused_node(Some(id));
                if let Some(metadata) = entry.widget_metadata {
                    let _ = self.mutate_node(id, |widget| widget.set_state(&metadata));
                    self.last_stable_metadata = Some(metadata);
                }
                self.last_stable_focus_idx = Some(idx);
            }
        } else {
            self.set_focused_node(None);
            self.last_stable_focus_idx = None;
            self.last_stable_metadata = None;
        }
        self.layout.layout_dirty = true;
        self.needs_sync = true;
        self.invalidate_widget_resource_cache();
    }

    /// Unique admitted node visits keep child reports and requests off shell history.
    fn service_history(&mut self) -> bool {
        if self.framework_input.get()
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.captures_input())
        {
            return false;
        }
        // SAFETY: App exclusively owns this stable live layout for the sweep.
        let mut changed = unsafe {
            crate::undo::record_dispatched_action_bound(
                &mut self.state,
                std::ptr::from_mut(self.root_widget.as_mut()),
                &self.layout.node_map,
                &self.bindings,
                &self.collaboration.local_author,
                &self.permission_ctx,
            )
        };
        let mut visited = std::collections::HashSet::new();
        for (&node, pointer) in &self.layout.node_map {
            if !visited.insert(pointer.cast::<()>()) {
                continue;
            }
            let binding = self.bindings.binding_for(node);
            // SAFETY: no target reference escapes and this layout is unchanged.
            let request = unsafe {
                crate::bound_dispatch::mutate_bound_node(
                    &self.bindings,
                    &self.layout.node_map,
                    node,
                    |widget| widget.take_undo_request(),
                )
            }
            .ok()
            .flatten();
            if let Some(request) = request {
                let entry = if let Some(binding) = &binding {
                    binding
                        .history_request(&mut self.state.doc, request)
                        .ok()
                        .flatten()
                } else if let Some(owner) = self.bindings.legacy_for(node) {
                    crate::bound_dispatch::legacy_history_request(
                        &owner,
                        request,
                        &self.permission_ctx,
                        &self.collaboration.local_author,
                    )
                    .ok()
                    .flatten()
                } else {
                    match request {
                        crate::undo::UndoRequest::Undo => {
                            self.state
                                .undo_as(Some(&self.collaboration.local_author), None, None)
                        }
                        crate::undo::UndoRequest::Redo => {
                            self.state
                                .redo_as(Some(&self.collaboration.local_author), None, None)
                        }
                    }
                };
                changed |= entry.is_some();
            }
            // The former eager status read flushed open transactions whenever
            // history existed. Preserve that boundary without deriving labels.
            if binding.is_none()
                && !crate::history_mode::embedded::has_surfaces(self.root_widget.as_ref())
                && (self
                    .state
                    .can_undo_as(Some(&self.collaboration.local_author))
                    || self
                        .state
                        .can_redo_as(Some(&self.collaboration.local_author)))
            {
                self.state.doc.get_heads();
            }
            // Bound histories intentionally keep completed-action labels warm
            // across pending edits and dynamically mounted controls.
            let bound_status = binding
                .as_ref()
                .map(|binding| binding.undo_status(&mut self.state.doc).unwrap_or_default())
                .or_else(|| {
                    self.bindings
                        .legacy_status(node, &self.collaboration.local_author)
                });
            let embedded = crate::history_mode::embedded::has_surfaces(self.root_widget.as_ref());
            let mut status = || {
                bound_status.clone().unwrap_or_else(|| {
                    if embedded {
                        self.state
                            .undo_status_snapshot(Some(&self.collaboration.local_author))
                    } else {
                        self.state
                            .undo_status(Some(&self.collaboration.local_author))
                    }
                })
            };
            // SAFETY: the same unique admitted node remains live and exclusive.
            let _ = unsafe {
                crate::bound_dispatch::mutate_bound_node(
                    &self.bindings,
                    &self.layout.node_map,
                    node,
                    |widget| widget.set_undo_status(&mut status),
                )
            };
        }
        changed
    }

    /// Finalize the document boundary for a handled native event. Shared host
    /// controls only change presentation and must not close pending app typing.
    fn finalize_handled_native_event(
        &mut self,
        event: &WindowEvent,
        is_undo_redo: bool,
        history_handled: bool,
    ) {
        let character_input = matches!(event, WindowEvent::KeyboardInput { event, .. }
            if matches!(event.logical_key, winit::keyboard::Key::Character(_)));
        if !is_undo_redo && !history_handled && !character_input {
            self.commit_action();
        }
    }

    fn commit_action(&mut self) {
        if self.framework_input.get()
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.captures_input())
        {
            return;
        }
        if self.service_history() {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        // Called after every handled event, many of which (e.g. a
        // selection-drag `CursorMoved`) mutate only ephemeral widget state,
        // not the CRDT â€” `changed` lets those skip persisting/syncing.
        let context = self.root_widget.take_commit_context();
        let changed = self.state.commit_action_authored_with_context(
            self.last_stable_focus_idx,
            self.last_stable_metadata.clone(),
            Some(self.collaboration.local_author.clone()),
            context,
        );
        self.last_stable_focus_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.last_stable_metadata = self.focused_node.and_then(|id| {
            self.inspect_node(id, |widget| widget.get_state())
                .ok()
                .flatten()
        });
        self.invalidate_widget_resource_cache();

        // Incremental delta, not the whole document â€” serialising per
        // keystroke scales with document size. `UndoLog::apply_inverse`
        // writes the inverse as new changes, so a delta carries undo/redo
        // like any other edit.
        if changed {
            self.broadcast_doc_delta();
        }
    }

    fn set_focused_node(&mut self, new_id: Option<NodeId>) {
        self.suspend_history_focus(new_id);
        if new_id.is_some_and(|node| self.is_history_node(node)) {
            self.remember_history_content();
        }
        if self.focused_node != new_id {
            if let Some(old) = self.focused_node {
                let _ = self.mutate_node(old, |widget| widget.set_focus(false));
            }
            self.focused_node = new_id;
            if let Some(node) = new_id {
                let _ = self.mutate_node(node, |widget| widget.set_focus(true));
                if self.accessibility.screen_reader.is_some() {
                    let access = self
                        .inspect_node(node, |widget| widget.accessibility())
                        .ok()
                        .flatten();
                    if let (Some(access), Some(reader)) =
                        (access, self.accessibility.screen_reader.as_mut())
                    {
                        reader.announce_focus(&access);
                    }
                }
            }
            self.layout.layout_dirty = true;
            self.needs_sync = true;
            self.invalidate_widget_resource_cache();
        }
    }

    /// The platform IME contract for the focused widget. A field opts in via
    /// [`Widget::ime_purpose`]; buttons and other focusable controls therefore
    /// never activate `text-input-v3` accidentally.
    fn ime_window_state(&self) -> Option<ImeWindowState> {
        let id = self.focused_node?;
        let purpose = self
            .inspect_node(id, |widget| widget.ime_purpose())
            .ok()
            .flatten()?;
        let rect = self
            .inspect_node(id, |widget| widget.ime_cursor_rect())
            .ok()
            .flatten()?;
        let bounded = |v: f64| {
            if v.is_finite() {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "value is clamped to [0.0, u32::MAX] on the line above, so the cast is exact"
                )]
                {
                    v.max(0.0).min(f64::from(u32::MAX)).round() as u32
                }
            } else {
                0
            }
        };
        Some(ImeWindowState {
            purpose,
            x: bounded(rect.x0),
            y: bounded(rect.y0),
            width: bounded(rect.width()).max(1),
            height: bounded(rect.height()).max(1),
        })
    }

    fn focused_state_snapshot(&self) -> Option<(NodeId, String)> {
        self.accessibility.screen_reader.as_ref()?;
        let id = self.focused_node?;
        let node = self
            .inspect_node(id, |widget| widget.accessibility())
            .ok()
            .flatten()?;
        Some((id, crate::screen_reader::describe_state(&node)))
    }

    /// Only fires if focus did NOT move â€” a move is narrated separately by
    /// [`set_focused_node`].
    fn announce_state_change(&mut self, pre: Option<(NodeId, String)>) {
        let Some((pre_id, pre_state)) = pre else {
            return;
        };
        if self.focused_node != Some(pre_id) {
            return;
        }
        if self.accessibility.screen_reader.is_none() {
            return;
        }
        let Some(node) = self
            .inspect_node(pre_id, |widget| widget.accessibility())
            .ok()
            .flatten()
        else {
            return;
        };
        let now = crate::screen_reader::describe_state(&node);
        if now.is_empty() || now == pre_state {
            return;
        }
        if let Some(sr) = self.accessibility.screen_reader.as_mut() {
            sr.announce_value_change(now);
        }
    }

    fn text_review_snapshot(&self) -> Option<(NodeId, usize, String)> {
        self.accessibility.screen_reader.as_ref()?;
        let id = self.focused_node?;
        let (caret, text) = self
            .inspect_node(id, |widget| widget.text_review())
            .ok()
            .flatten()?;
        Some((id, caret, text))
    }

    /// Typing is left to the widget's own echo so this never double-speaks
    /// an edit.
    fn announce_text_review(&mut self, pre: Option<(NodeId, usize, String)>) {
        let Some((pre_id, pre_caret, pre_text)) = pre else {
            return;
        };
        if self.focused_node != Some(pre_id) {
            return;
        }
        if self.accessibility.screen_reader.is_none() {
            return;
        }
        let Some((now_caret, now_text)) = self
            .inspect_node(pre_id, |widget| widget.text_review())
            .ok()
            .flatten()
        else {
            return;
        };
        if now_text != pre_text || now_caret == pre_caret {
            return;
        }
        let single_step = next_boundary(&now_text, pre_caret) == now_caret
            || prev_boundary(&now_text, pre_caret) == now_caret;
        if single_step {
            // Character to the caret's right; fall back left at end of text.
            let ch = now_text
                .get(now_caret..)
                .and_then(|s| s.chars().next())
                .or_else(|| {
                    now_text
                        .get(..now_caret)
                        .and_then(|s| s.chars().next_back())
                });
            if let (Some(ch), Some(sr)) = (ch, self.accessibility.screen_reader.as_mut()) {
                sr.echo_char(ch);
            }
        } else if let Some(word) = word_at(&now_text, now_caret)
            && let Some(sr) = self.accessibility.screen_reader.as_mut()
        {
            sr.echo_word(&word);
        }
    }

    /// Announces changed [live regions](crate::a11y::AccessNode::live)
    /// without moving focus.
    fn scan_live_regions(&mut self) {
        fn visit(w: &dyn Widget, out: &mut Vec<(String, crate::a11y::Politeness, String)>) {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(w) else {
                return;
            };
            if let Some(node) = w.accessibility()
                && let (Some(politeness), Some(name)) = (node.live, node.name.clone())
            {
                out.push((name, politeness, crate::screen_reader::describe(&node)));
            }
            w.for_each_child(&mut |child| visit(child, out));
        }
        if self.accessibility.screen_reader.is_none() {
            return;
        }
        // A live region without an accessible name can't be tracked stably
        // across frames, so it's skipped.
        let mut seen: Vec<(String, crate::a11y::Politeness, String)> = Vec::new();
        visit(self.root_widget.as_ref(), &mut seen);

        let mut current_keys = std::collections::HashSet::new();
        let mut to_announce: Vec<(crate::a11y::Politeness, String)> = Vec::new();
        for (key, politeness, text) in seen {
            current_keys.insert(key.clone());
            let changed = self.accessibility.live_region_last.get(&key) != Some(&text);
            self.accessibility
                .live_region_last
                .insert(key, text.clone());
            // Clearing a region to empty updates the snapshot silently.
            if changed && !text.is_empty() {
                to_announce.push((politeness, text));
            }
        }
        // Drop snapshots for regions that left the tree, so a region that
        // reappears with the same text announces afresh.
        self.accessibility
            .live_region_last
            .retain(|k, _| current_keys.contains(k));

        if let Some(sr) = self.accessibility.screen_reader.as_mut() {
            for (politeness, text) in to_announce {
                let (priority, interrupt) = match politeness {
                    // Assertive cuts in; polite is queued at notification
                    // priority and never interrupts in-flight speech.
                    crate::a11y::Politeness::Assertive => (tts::Priority::Important, true),
                    crate::a11y::Politeness::Polite => (tts::Priority::Notification, false),
                };
                sr.announce(text, priority, interrupt);
            }
        }
    }

    fn navigate_focus(&mut self, direction: Direction) {
        let Some(current_id) = self.focused_node else {
            if let Some(&first) = self.focusable_order.first() {
                self.set_focused_node(Some(first));
            }
            return;
        };
        let Ok(current_rect) = self.inspect_node(current_id, |widget| widget.get_rect()) else {
            return;
        };
        let current_center = current_rect.center();
        let mut best_candidate = None;
        let mut best_score = f64::MAX;
        for &candidate_id in &self.focusable_order {
            if candidate_id == current_id {
                continue;
            }
            if let Ok(candidate_rect) = self.inspect_node(candidate_id, |widget| widget.get_rect())
            {
                let candidate_center = candidate_rect.center();
                let vector = vello::kurbo::Vec2::new(
                    candidate_center.x - current_center.x,
                    candidate_center.y - current_center.y,
                );
                let (primary_dist, secondary_dist, in_direction) = match direction {
                    Direction::Up => (-vector.y, vector.x.abs(), vector.y < -1.0),
                    Direction::Down => (vector.y, vector.x.abs(), vector.y > 1.0),
                    Direction::Left => (-vector.x, vector.y.abs(), vector.x < -1.0),
                    Direction::Right => (vector.x, vector.y.abs(), vector.x > 1.0),
                };
                if in_direction {
                    let score = primary_dist + (secondary_dist * 2.0);
                    if score < best_score {
                        best_score = score;
                        best_candidate = Some(candidate_id);
                    }
                }
            }
        }
        if let Some(new_id) = best_candidate {
            self.set_focused_node(Some(new_id));
        }
    }

    /// Gated on the focus-trap signal, not `modal_capture`: that governs only
    /// pointer click-through â€” gating Tab on it instead either froze focus
    /// inside a dialog or let it escape with no capture.
    fn keyboard_nav_allowed(&self, modal_rect: Option<vello::kurbo::Rect>) -> bool {
        modal_rect.is_none() || self.accessibility.focus_trap_active
    }

    /// Tab normally belongs to the host's focus order. A focused editor-like
    /// widget may explicitly claim it first (for indentation or completion),
    /// without broadening that exception to arbitrary widgets or escaping an
    /// active focus trap.
    fn focused_widget_wants_tab_input(&self) -> bool {
        self.focused_node.is_some_and(|node| {
            self.inspect_node(node, |widget| widget.wants_tab_input())
                .unwrap_or(false)
        })
    }

    /// Shared native Tab admission, preserving text-input and modal capture.
    fn handle_tab_navigation(
        &mut self,
        event: &WindowEvent,
        modal_rect: Option<KurboRect>,
    ) -> bool {
        if self.focused_widget_wants_tab_input() || !self.keyboard_nav_allowed(modal_rect) {
            return false;
        }
        if matches!(event, WindowEvent::KeyboardInput { event, .. }
            if event.state == winit::event::ElementState::Pressed
                && event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab))
        {
            return self.focus_tab(self.input.modifiers.state().shift_key());
        }
        false
    }

    fn focus_tab(&mut self, shift: bool) -> bool {
        let len = self.focusable_order.len();
        if len == 0 {
            return false;
        }
        let current_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        let next_idx = match current_idx {
            Some(idx) => {
                if shift {
                    if idx == 0 {
                        len.saturating_sub(1)
                    } else {
                        idx.saturating_sub(1)
                    }
                } else {
                    let next = idx.saturating_add(1);
                    if next >= len { 0 } else { next }
                }
            }
            None => 0,
        };
        if let Some(&id) = self.focusable_order.get(next_idx) {
            self.set_focused_node(Some(id));
        }
        true
    }

    /// `scale` is the window scale factor so bounds land in physical pixels
    /// (what AT-SPI expects).
    pub fn build_a11y_tree_update(&mut self, scale: f64) -> accesskit::TreeUpdate {
        let _entry = self.enter_wake_scope();
        let exported = self.bindings.exported_nodes(&self.layout.node_map);
        if let Some(history) = &self.history {
            let resources = if history.live_interactive() {
                self.collect_declared_resources()
            } else {
                vec![]
            };
            return crate::accesskit_bridge::build_presentation_with_resources_bound(
                &self.history_presentation_roots(),
                &exported.nodes,
                exported.focus(self.focused_node),
                scale,
                &resources,
            );
        }
        let mut resources = self.collect_declared_resources();
        resources.extend(self.history_local_resources());
        self.with_native_presentation(|root, nodes, focused, _| {
            crate::accesskit_bridge::build_tree_update_with_resources_bound(
                root, nodes, focused, scale, &resources,
            )
        })
        .unwrap_or_else(|_| {
            crate::accesskit_bridge::build_tree_update_with_resources_bound(
                self.root_widget.as_ref(),
                &self.layout.node_map,
                self.focused_node,
                scale,
                &self.collect_declared_resources(),
            )
        })
    }
    fn find_resource_action(
        resources: &[tilo_actions::Resource],
        target: accesskit::NodeId,
    ) -> Option<tilo_actions::ActionAddress> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|spec| {
                    crate::accesskit_bridge::resource_action_node_id(resource.id.as_str(), &spec.id)
                        == target
                })
                .map(|spec| tilo_actions::ActionAddress::new(resource.id.as_str(), &spec.id))
                .or_else(|| Self::find_resource_action(&resource.children, target))
        })
    }

    fn dispatch_resource_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> bool {
        use crate::a11y::Action;

        let argument = match action {
            Action::Activate | Action::ToggleCheck => Some(None),
            Action::SetValue(value) => Some(Some(value.as_str())),
            _ => None,
        };
        let Some(argument) = argument else {
            return false;
        };
        let mut resources = self.collect_declared_resources();
        resources.extend(self.history_local_resources());
        let Some(address) = Self::find_resource_action(&resources, target) else {
            return false;
        };
        if self
            .invoke_history_address_local(&address, argument)
            .is_some()
        {
            return true;
        }
        let confirmation = tilo_actions::door::resolve(&resources, &address)
            .map(|spec| spec.contract.confirmation.clone())
            .unwrap_or_default();
        if !matches!(confirmation, tilo_actions::ConfirmationPolicy::None) {
            let _ = self.open_action_palette_at(&address);
            return true;
        }
        let actor = self.collaboration.local_author.0.clone();
        if self
            .invoke_resource_action(&address, argument, &actor, false, false, None, None, None)
            .is_ok()
        {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        true
    }

    /// Resolves both retained AccessKit nodes and synthetic semantic children
    /// (virtual rows/cells) to their owning widget. The widget gets first
    /// refusal; synthetic children additionally carry an opaque stable target
    /// produced by that widget. If a retained widget declines, `Action::Focus`
    /// moves keyboard focus and activation returns click coordinates.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered native accessibility door preserves existing resource, semantic and pointer fallback precedence"
    )]
    pub fn dispatch_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> Option<(f64, f64)> {
        use crate::a11y::Action;
        use crate::accesskit_bridge::A11yTarget;

        let _entry = self.enter_wake_scope();

        self.refresh_control_action_availability();
        // A retained AT id is operable only while the exact target is exported
        // in the current presentation; hidden resources cannot bypass chrome.
        if !self
            .build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .any(|(id, _)| *id == target)
        {
            return None;
        }
        if self.dispatch_resource_a11y_action(target, action) {
            return None;
        }

        let exported = self.bindings.exported_nodes(&self.layout.node_map);
        let resolved = if self.history.is_some() {
            self.history_presentation_roots()
                .into_iter()
                .find_map(|root| exported.resolve(root, target))?
        } else {
            self.with_native_presentation(|root, _, _, _| exported.resolve(root, target))
                .ok()
                .flatten()?
        };
        let (taffy_id, key_path, ordinal_path, semantic_target, synthetic) = match resolved {
            A11yTarget::Root => return None,
            A11yTarget::Widget(id) => (id, Vec::new(), Vec::new(), None, false),
            A11yTarget::Synthetic {
                owner,
                key_path,
                ordinal_path,
                semantic_target,
            } => (owner, key_path, ordinal_path, semantic_target, true),
        };
        let framework = crate::history_mode::embedded::owns_node(
            self.root_widget.as_ref(),
            taffy_id,
            &self.layout.taffy,
        ) || self
            .history
            .as_ref()
            .is_some_and(|history| history.owns_node(taffy_id, &self.layout.taffy));
        let _history_frame = crate::history_mode::frame::FrameworkInputFrame::new(
            &self.framework_input,
            framework || self.bindings.owns_document(taffy_id),
        );
        if framework {
            self.suspend_history_focus(Some(taffy_id));
        }
        let handled = self
            .dispatch_semantic_node(taffy_id, action, |widget, doc, root| {
                if let Some(target) = semantic_target.as_deref() {
                    widget.perform_semantic_action(target, action, doc, root)
                } else if synthetic {
                    widget.perform_action_at(action, &key_path, doc, root)
                } else {
                    widget.perform_action(action, doc, root)
                }
            })
            .ok()?;
        if handled {
            if matches!(action, Action::Focus)
                || (matches!(action, Action::SetSelection { .. })
                    && self
                        .inspect_node(taffy_id, |widget| widget.is_focused())
                        .unwrap_or(false))
            {
                self.set_focused_node(Some(taffy_id));
            }
            // `Widget::perform_action` is deliberately the same path as a
            // keyboard or pointer activation.  Action-bound controls queue a
            // control intent there; drain it before publishing the next
            // AccessKit update so an AT-SPI DoAction never reports success
            // while leaving the visible state unchanged.
            self.dispatch_control_invocations();
            self.invalidate_widget_resource_cache();
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            if framework {
                self.refresh_framework_history(
                    self.layout.last_layout_logical.map_or(900.0, |size| size.0),
                );
            }
            return None;
        }

        match action {
            Action::Focus => {
                if synthetic && !ordinal_path.is_empty() {
                    let _ = self.mutate_node(taffy_id, |widget| {
                        widget.focus_synthetic_accessibility(&ordinal_path)
                    });
                }
                self.set_focused_node(Some(taffy_id));
                None
            }
            Action::Activate | Action::ToggleCheck => {
                let rect = if synthetic {
                    self.inspect_node(taffy_id, |widget| {
                        widget.synthetic_accessibility_rect(&ordinal_path)
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| {
                        self.inspect_node(taffy_id, |widget| widget.get_rect())
                            .unwrap_or(KurboRect::ZERO)
                    })
                } else {
                    self.inspect_node(taffy_id, |widget| widget.get_rect())
                        .unwrap_or(KurboRect::ZERO)
                };
                let c = rect.center();
                Some((c.x, c.y))
            }
            _ => None,
        }
    }

    /// Complete an accessibility activation for legacy hand-hit-tested
    /// controls that do not implement `perform_action` yet. The semantic
    /// target has already resolved to this exact point; routing the usual
    /// move/press/release sequence through the widget tree preserves the same
    /// behavior as hardware input instead of reporting a false success.
    fn dispatch_semantic_click(&mut self, x: f64, y: f64) -> bool {
        self.refresh_control_action_availability();
        if self.layout.root_node.is_none() {
            return false;
        }
        let events = [
            WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: PhysicalPosition::new(x, y),
            },
            WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        ];
        let mut handled_any = false;
        for event in events {
            if self.history_controls_event(&event, 1.0) {
                handled_any |= self.dispatch_framework_history_event(&event, 1.0);
                continue;
            }
            let mut handled = false;
            let mut focus_target = None;
            self.presentation_handle_event(&event, 1.0, &mut handled, &mut focus_target);
            if let Some(id) = focus_target {
                self.set_focused_node(Some(id));
            }
            handled_any |= handled;
        }
        if handled_any {
            self.dispatch_control_invocations();
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        handled_any
    }

    /// Node identity is preserved for everything that recurred, so a
    /// consumer reacts to just the diff.
    pub fn refresh_semantic_tree(&mut self) -> crate::a11y_tree::TreeDelta {
        let _entry = self.enter_wake_scope();
        let mut resources = self.collect_declared_resources();
        resources.extend(self.history_local_resources());
        let components = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        let mut tree = std::mem::take(&mut self.accessibility.semantic_tree);
        let delta = self
            .with_native_presentation(|root, nodes, focused, _| {
                tree.refresh_components(root, nodes, focused, &resources, &components)
            })
            .unwrap_or_else(|_| {
                tree.refresh_components(
                    self.root_widget.as_ref(),
                    &self.layout.node_map,
                    self.focused_node,
                    &self.collect_declared_resources(),
                    &components,
                )
            });
        self.accessibility.semantic_tree = tree;
        delta
    }
    pub fn semantic_tree(&self) -> &crate::a11y_tree::SemanticTree {
        &self.accessibility.semantic_tree
    }

    fn invoke_bound_semantic_action(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        arg: Option<&str>,
    ) -> Result<bool, String> {
        let actor = self.collaboration.local_author.0.clone();
        match self.invoke_bound_target(target, arg, &actor, false, false, None, None, None) {
            Ok(_) => Ok(true),
            Err(tilo_actions::DoorError::ConfirmationRequired) => {
                self.open_action_palette();
                Ok(self
                    .actions
                    .action_palette
                    .as_mut()
                    .is_some_and(|palette| palette.activate_bound(target)))
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// A [`Verb`](crate::a11y_tree::SemanticActionKind::Verb) routes through
    /// the same [`dispatch_a11y_action`](Self::dispatch_a11y_action) path an
    /// AccessKit `ActionRequest` takes; an
    /// [`App`](crate::a11y_tree::SemanticActionKind::App) affordance
    /// dispatches through [`tilo_actions::dispatch`], the same audited path
    /// the Ctrl+K palette uses. Call
    /// [`refresh_semantic_tree`](Self::refresh_semantic_tree) first so the
    /// ids are current.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the underlying app action fails to decode or
    /// dispatch (see [`Self::dispatch_encoded_semantic`]).
    pub fn invoke_semantic_action(
        &mut self,
        id: &crate::a11y_tree::SemanticId,
        name: &str,
        arg: Option<&str>,
    ) -> Result<bool, String> {
        use crate::a11y_tree::SemanticActionKind;

        let _entry = self.enter_wake_scope();
        // Cloned so the mutation below is free to borrow doc/registry/transport.
        let Some(action) = self.accessibility.semantic_tree.action(id, name).cloned() else {
            return Ok(false);
        };
        if let Some(target) = self.accessibility.semantic_tree.bound_target(id, name) {
            return self.invoke_bound_semantic_action(&target, arg);
        }
        match action.kind {
            SemanticActionKind::App(encoded) => {
                if self.is_history_encoded_local(&encoded) {
                    self.execute_history_encoded_local(&encoded)?;
                    Ok(true)
                } else {
                    self.dispatch_encoded_semantic(encoded)
                }
            }
            SemanticActionKind::AppPrompted { .. } => {
                let arg = arg.unwrap_or("");
                let Some(encoded) = self
                    .accessibility
                    .semantic_tree
                    .build_prompted(id, name, arg)
                else {
                    return Ok(false);
                };
                if self.is_history_encoded_local(&encoded) {
                    self.execute_history_encoded_local(&encoded)?;
                    return Ok(true);
                }
                self.dispatch_encoded_semantic(encoded)
            }
            SemanticActionKind::Verb(verb) => {
                let verb = match verb {
                    crate::a11y::Action::SetValue(_) => {
                        crate::a11y::Action::SetValue(arg.unwrap_or_default().to_string())
                    }
                    other => other,
                };
                let Some((taffy_raw, semantic_target, bounds)) =
                    self.accessibility.semantic_tree.node(id).and_then(|n| {
                        n.widget_taffy_id
                            .map(|taffy| (taffy, n.semantic_target.clone(), n.bounds))
                    })
                else {
                    return Ok(false);
                };
                if let Some(semantic_target) = semantic_target {
                    // Semantic owners use the same lifetime-scoped aliases as
                    // native accessibility. Resolve only current admissions;
                    // an expired alias must never fall back to a raw layout id.
                    let exported = self.bindings.exported_nodes(&self.layout.node_map);
                    let Some(crate::accesskit_bridge::A11yTarget::Widget(taffy_id)) = exported
                        .resolve(
                            self.root_widget.as_ref(),
                            accesskit::NodeId(taffy_raw.saturating_add(1)),
                        )
                    else {
                        return Ok(false);
                    };
                    let keyed_path = semantic_target
                        .strip_prefix("tilo.key-action:")
                        .and_then(|target| target.rsplit_once(':'))
                        .map(|(_, path)| {
                            path.split('/')
                                .map(str::parse::<u64>)
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .transpose()
                        .map_err(|error| format!("invalid keyed semantic action: {error}"))?;
                    let handled = self
                        .dispatch_node(taffy_id, |widget, doc, root| {
                            if let Some(key_path) = keyed_path {
                                widget.perform_action_at(&verb, &key_path, doc, root)
                            } else {
                                widget.perform_semantic_action(&semantic_target, &verb, doc, root)
                            }
                        })
                        .map_err(|error| format!("component dispatch refused: {error:?}"))?;
                    if handled {
                        if matches!(verb, crate::a11y::Action::Focus) {
                            self.set_focused_node(Some(taffy_id));
                        }
                        self.invalidate_widget_resource_cache();
                        self.needs_sync = true;
                        self.layout.layout_dirty = true;
                        return Ok(true);
                    }
                    // A custom-painted widget may expose stable semantic
                    // targets and geometry while intentionally reusing its
                    // production pointer hit-testing instead of duplicating a
                    // `perform_semantic_action` switch. Route the action back
                    // through that exact event path.
                    if let Some(bounds) = bounds {
                        let center = bounds.center();
                        return Ok(self.dispatch_semantic_click(center.x, center.y));
                    }
                    return Ok(false);
                }
                let ak = accesskit::NodeId(taffy_raw.saturating_add(1));
                if let Some((x, y)) = self.dispatch_a11y_action(ak, &verb) {
                    return Ok(self.dispatch_semantic_click(x, y));
                }
                Ok(true)
            }
        }
    }

    /// Byte-for-byte the same path as a Ctrl+K palette selection
    /// ([`Self::action_palette_handle_key`]).
    fn dispatch_encoded_semantic(
        &mut self,
        encoded: tilo_actions::EncodedAction,
    ) -> Result<bool, String> {
        let before = self.state.doc.get_heads();
        let action = self
            .actions
            .action_registry
            .decode(&encoded)
            .map_err(|e| e.to_string())?;
        let dispatched = tilo_actions::dispatch(
            &mut self.state.doc,
            action.as_ref(),
            encoded,
            self.actions.action_transport.as_mut(),
        )
        .map_err(|e| e.to_string())?;
        if dispatched.mutated_local() {
            self.state.record_committed_action(before, None, None);
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            self.invalidate_widget_resource_cache();
        }
        Ok(true)
    }

    /// Lets headless tests stand the App up without spinning a winit loop.
    #[doc(hidden)]
    pub fn init_layout(&mut self) {
        let _entry = self.enter_wake_scope();
        self.refresh_control_action_availability();
        self.component_close_reports
            .extend(self.root_widget.take_component_close_reports());
        self.bindings.clear();
        self.layout.node_map.clear();
        self.build_history_presentation();
        self.update_focusable_order();
        if let Some(view) = self.continuation.pending_view.take() {
            <Self as crate::view::FreezeThaw>::restore_view(self, &view);
        }
        if self.autofocus_first
            && self.focused_node.is_none()
            && let Some(&first) = self.focusable_order.first()
        {
            self.set_focused_node(Some(first));
        }
        self.last_stable_focus_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.last_stable_metadata = self.focused_node.and_then(|id| {
            self.inspect_node(id, |widget| widget.get_state())
                .ok()
                .flatten()
        });
    }

    /// Exposed so headless tests can drive modal open/close without a winit
    /// loop.
    #[doc(hidden)]
    pub fn refresh_focus_order(&mut self) {
        let _entry = self.enter_wake_scope();
        self.root_widget.sync(&mut self.state.doc, &self.state.root);
        self.invalidate_widget_resource_cache();
        self.update_focusable_order();
    }

    /// When [`Self::autofocus_on_structure_change`] is set, restores the
    /// focused widget's caret/selection only if focus lands back on the SAME
    /// focusable ordinal after rebuild (never onto a different field);
    /// without this an unrelated rebuild snaps the caret to offset 0.
    /// [`Widget::take_focus_reset`] instead jumps to the first focusable
    /// with no caret restore, since the field legitimately changed.
    #[doc(hidden)]
    pub fn rebuild_structure(&mut self) {
        let _entry = self.enter_wake_scope();
        // A reported mutation can already have invalidated old widget pointers.
        let topology_changed =
            self.layout.structure_dirty || self.root_widget.take_structure_dirty();
        // Node ids below are about to be invalidated.
        let (prev_focus_idx, prev_focus_state, focus_reset) = if self.autofocus_on_structure_change
        {
            let idx = self
                .focused_node
                .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
            let state = if topology_changed {
                None
            } else {
                self.focused_node.and_then(|id| {
                    self.inspect_node(id, |widget| widget.get_state())
                        .ok()
                        .flatten()
                })
            };
            (idx, state, self.root_widget.take_focus_reset())
        } else {
            (None, None, false)
        };

        // NodeId values can be reused by the new tree. Clear the old handle
        // without dispatching through its potentially invalid pointer, so a
        // replacement control at the same id still receives set_focus(true).
        self.focused_node = None;
        self.layout.taffy = TaffyTree::new();
        self.component_close_reports
            .extend(self.root_widget.take_component_close_reports());
        self.bindings.clear();
        self.layout.node_map.clear();
        self.build_history_presentation();
        self.layout.structure_dirty = false;
        self.layout.layout_dirty = true;
        self.update_focusable_order();

        // A focus trap already placed focus inside its subtree in
        // `update_focusable_order`; don't let it escape the modal.
        if self.autofocus_on_structure_change && !self.accessibility.focus_trap_active {
            if focus_reset {
                if let Some(&first) = self.focusable_order.first() {
                    self.set_focused_node(Some(first));
                }
            } else if let Some(idx) = prev_focus_idx {
                let exact = self.focusable_order.get(idx).copied();
                let target = exact.or_else(|| self.focusable_order.last().copied());
                if let Some(id) = target {
                    self.set_focused_node(Some(id));
                    if exact.is_some()
                        && let Some(state) = prev_focus_state.as_deref()
                    {
                        let _ = self.mutate_node(id, |widget| widget.set_state(state));
                    }
                }
            }
        }

        // The widgets built above are fresh instances the OLD `sync` never
        // ran against, so without this they draw blank until the next event.
        self.root_widget.sync(&mut self.state.doc, &self.state.root);
        self.needs_sync = false;
        if self.root_widget.take_structure_dirty() {
            // Fresh sync can replace delegates. Rebuild before any map access.
            self.layout.structure_dirty = true;
            self.bindings.clear();
            self.layout.node_map.clear();
        }
        self.invalidate_widget_resource_cache();
    }

    /// Pair with [`crate::persist::open`], which hands back the document the
    /// previous session left plus its writer:
    ///
    /// ```ignore
    /// let (state, autosave) = tilo_ui::persist::open("so.tilo.my-app");
    /// tilo_ui::App::from_factory(state, my_app::app_root)
    ///     .with_autosave(autosave)
    ///     .run();
    /// ```
    #[must_use]
    /// # Panics
    /// Fails closed if the host storage lifetime counter is exhausted.
    #[allow(
        clippy::expect_used,
        reason = "storage identity must fail closed rather than wrap and admit an old owner"
    )]
    pub fn with_autosave(mut self, autosave: crate::persist::Autosave) -> Self {
        self.autosave_generation = self
            .autosave_generation
            .checked_add(1)
            .expect("autosave generation exhausted");
        if let Some(history) = &mut self.history {
            self.layout.node_map.clear();
            self.bindings.clear();
            self.focused_node = None;
            history.invalidate_owner();
            self.layout.structure_dirty = true;
        }
        self.persistence.autosave = Some(autosave);
        self
    }
    pub(crate) fn history_storage_generation(&self) -> u64 {
        self.autosave_generation
    }
    pub(crate) fn install_app_state_history(
        &mut self,
        definition: &'static crate::history_mode::HistoryDefinition,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        if self.persistence.records.is_some() {
            return Err(crate::history_mode::HistoryRegistrationError::UnsupportedOwner);
        }
        let registration = crate::history_mode::HistoryRegistration::installed_app_state(
            definition,
            self.persistence.autosave.as_ref(),
            uuid::Uuid::new_v4(),
        )?;
        self.install_history(registration)
    }

    /// Inspect controlled launch composition without opening a native window.
    /// Available only to downstream runner tests through `test-support`.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn controlled_history_definitions_for_test(
        &self,
    ) -> (
        Option<&'static crate::history_mode::HistoryDefinition>,
        Option<&'static crate::history_mode::HistoryDefinition>,
    ) {
        (
            self.controlled_installed_history_definition,
            self.root_widget.document_history_definition(),
        )
    }

    pub(crate) fn with_legacy_persistence(
        mut self,
        policy: crate::launch::LegacyPersistencePolicy,
    ) -> Self {
        self.legacy_persistence = policy;
        self
    }

    /// Takes effect when the window is created, so set before [`App::run`].
    #[must_use]
    pub fn with_window_title(mut self, title: impl Into<String>) -> Self {
        self.window_title = Some(title.into());
        self
    }

    /// Set the stable Wayland application id announced by this window.
    #[must_use]
    pub fn with_window_app_id(mut self, app_id: impl Into<String>) -> Self {
        self.window_app_id = Some(app_id.into());
        self
    }

    /// `None` clears the override and restores automatic resolution.
    pub fn set_window_title(&mut self, title: Option<String>) {
        self.window_title = title;
    }

    /// Precedence: explicit override â†’ `TILO_APP_NAME` â†’ package id â†’ binary
    /// name â†’ generic default.
    pub fn resolved_window_title(&self) -> String {
        resolve_window_title(self.window_title.as_deref())
    }

    /// `records` owns per-record documents and persistence; `room_index` is
    /// which records the room holds. Pair with [`App::with_autosave`] only if
    /// the room index itself should persist â€” records persist through
    /// `records`.
    ///
    /// # Panics
    /// Panics if the document generation counter is exhausted, refusing identity reuse.
    #[allow(
        clippy::expect_used,
        reason = "exhausted document generation must fail closed rather than reuse custody"
    )]
    #[must_use]
    pub fn with_records(
        mut self,
        mut records: crate::record::RecordSet,
        room_index: automerge::AutoCommit,
    ) -> Self {
        let _entry = self.enter_wake_scope();
        self.autosave_generation = self
            .autosave_generation
            .checked_add(1)
            .expect("document generation exhausted");
        if let Some(history) = &mut self.history {
            self.layout.node_map.clear();
            self.bindings.clear();
            self.focused_node = None;
            history.invalidate_owner();
            self.layout.structure_dirty = true;
        }
        // Learn the current room grant before the widget selects a record.
        // Discovery is metadata-only: it must not create an empty record
        // replica or join a collaboration route for every indexed document.
        records.discover_room(&room_index);
        self.persistence.records = Some(records);
        self.persistence.room_index = Some(crate::state::DocumentReplica::new(room_index));
        self
    }

    pub fn records(&self) -> Option<&crate::record::RecordSet> {
        self.persistence.records.as_ref()
    }

    pub fn room_index(&self) -> Option<&automerge::AutoCommit> {
        self.persistence
            .room_index
            .as_ref()
            .map(crate::state::DocumentReplica::document)
    }

    /// The record is the collaboration routing key, so a checkout is a
    /// change of address â€” without re-announcing it, deltas keep publishing
    /// under the previous record's name.
    ///
    /// # Panics
    /// Panics if the document generation counter is exhausted, refusing identity reuse.
    #[allow(
        clippy::expect_used,
        reason = "exhausted document generation must fail closed rather than reuse custody"
    )]
    pub fn record_tick(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        let Some(set) = self.persistence.records.as_mut() else {
            return false;
        };
        let switched = crate::record::tick(
            set,
            &mut self.state,
            self.persistence
                .room_index
                .as_mut()
                .map(crate::state::DocumentReplica::document_mut),
            &*self.root_widget,
        );
        match switched {
            Some(id) => {
                self.autosave_generation = self
                    .autosave_generation
                    .checked_add(1)
                    .expect("document generation exhausted");
                if let Some(history) = &mut self.history {
                    self.layout.node_map.clear();
                    self.bindings.clear();
                    self.focused_node = None;
                    history.invalidate_owner();
                    self.layout.structure_dirty = true;
                }
                self.state.rebase_outbound();
                // Preserve the launcher's unqualified route for the room
                // index before moving the editable document to its record
                // route. The two Automerge graphs never share a channel or
                // an apply target.
                if self.collaboration.room_manager.is_none() {
                    let room = std::mem::replace(
                        &mut self.collaboration.manager,
                        CollaborationManager::for_record(&id),
                    );
                    self.collaboration.room_manager = Some(room);
                } else {
                    self.collaboration.manager = CollaborationManager::for_record(&id);
                }
                tracing::debug!(
                    target: "tilo.state",
                    event = "record_checkout",
                    record = id.as_str(),
                    connected = self.collaboration.manager.is_connected(),
                    "switched document collaboration route"
                );
                self.needs_sync = true;
                self.layout.layout_dirty = true;
                // Presence announces the location; document catch-up is an
                // independently directed broker request on the new route.
                if let Some(scope) = self.presence.broadcast_scope.clone() {
                    self.broadcast_presence(scope, crate::collaboration::PresencePayload::Arrived);
                }
                let _ = self
                    .collaboration
                    .manager
                    .send(crate::collaboration::CollabRequest::RequestDocumentSnapshot);
                true
            }
            None => false,
        }
    }

    /// With records in play, the App's autosave writes the room index
    /// instead of the live document â€” sending the live doc to both would put
    /// a record's content in the room index.
    fn autosave_tick(&mut self) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        match self.persistence.records.as_mut() {
            Some(set) => {
                set.tick(&mut self.state.doc);
                if let (Some(saver), Some(room)) = (
                    self.persistence.autosave.as_mut(),
                    self.persistence.room_index.as_mut(),
                ) {
                    saver.request(room.document_mut());
                }
            }
            None => {
                if let Some(saver) = self.persistence.autosave.as_mut() {
                    saver.request(&mut self.state.doc);
                }
            }
        }
        // Collect through the same live component boundary used by dispatch.
        // Distinct embedded owners participate; repeated views share one tick.
        let mut owners: Vec<crate::record::RecordStore> = Vec::new();
        let mut visit = |binding: &crate::bound_dispatch::CallbackBinding,
                         _: &mut dyn crate::widget::AppWidget,
                         _: &tilo_actions::ActionRegistry| {
            if !owners
                .iter()
                .any(|owner| owner.same_owner(&binding.records))
            {
                owners.push(binding.records.clone());
            }
            None
        };
        let traversal =
            self.root_widget
                .visit_components(&mut crate::bound_dispatch::ComponentRequest {
                    target: None,
                    found: false,
                    visit: &mut visit,
                });
        // Release component callback loans before requesting writer work. Owners
        // already collected still progress if another live component refuses entry.
        for owner in owners {
            owner.tick();
        }
        traversal
    }

    /// Called on event-loop teardown so a `kill` right after the last
    /// keystroke still finds it on disk.
    fn autosave_flush(&mut self) {
        if self.legacy_persistence == crate::launch::LegacyPersistencePolicy::WidgetOwned {
            self.close_components();
            if let Err(error) = flush_widget_owned_persistence(
                self.root_widget.as_mut(),
                &mut self.component_close_reports,
            ) {
                tracing::error!(%error, "widget-owned persistence failed during shutdown");
                eprintln!("tilo: notebook save failed during shutdown: {error}");
            }
            return;
        }
        if let Some(set) = self.persistence.records.as_mut() {
            set.flush(&mut self.state.doc);
            if let (Some(saver), Some(room)) = (
                self.persistence.autosave.as_mut(),
                self.persistence.room_index.as_mut(),
            ) {
                saver.request(room.document_mut());
                saver.flush();
            }
            return;
        }
        if let Some(saver) = self.persistence.autosave.as_mut() {
            saver.request(&mut self.state.doc);
            saver.flush();
        }
    }

    /// Lets a caller check [`crate::persist::Autosave::last_error`] rather
    /// than assuming the disk accepted the writes.
    pub fn autosave(&self) -> Option<&crate::persist::Autosave> {
        self.persistence.autosave.as_ref()
    }

    pub fn run(self) {
        self.run_with_setup(|_| {});
    }

    #[cfg(windows)]
    pub(crate) fn with_startup_renderer(mut self, renderer: Option<RendererPreparation>) -> Self {
        self.startup_renderer = renderer;
        self
    }

    /// Like [`App::run`] but exposes the winit [`EventLoopProxy`] before the
    /// loop starts, so callers can spawn threads that inject [`UserEvent`]s.
    ///
    /// # Panics
    ///
    /// Panics if the platform windowing system can't create the event loop.
    /// Once running, a Wayland compositor disconnect is a normal application
    /// shutdown boundary (session logout, compositor restart), so an
    /// `ExitFailure` is logged and returned rather than converted into a panic.
    #[allow(
        clippy::expect_used,
        clippy::unwrap_used,
        reason = "event-loop creation/run failure is unrecoverable at process startup; the public run()/run_with_setup() signature is called from every app's main() and returning Result here would ripple across the whole app surface for a failure with no fallback"
    )]
    pub fn run_with_setup<F>(mut self, setup: F)
    where
        F: FnOnce(EventLoopProxy<UserEvent>),
    {
        assert!(
            self.wake_scope.is_some(),
            "live App requires from_factory, try_from_factory, or from_scoped_parts"
        );
        if self.history.is_none() {
            self.enable_document_history_chrome();
        }
        let mut event_loop_builder = EventLoop::<UserEvent>::with_user_event();
        #[cfg(target_os = "macos")]
        if std::env::var("QOS_PROFILE_WINDOW_NO_ACTIVATE").as_deref() == Ok("1") {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            // Native scripted input does not need to take the user's keyboard.
            // Keep normal launches unchanged; this is an opt-in profiling mode.
            event_loop_builder.with_activate_ignoring_other_apps(false);
        }
        let event_loop = event_loop_builder.build().unwrap();
        let proxy = event_loop.create_proxy();
        let wake_proxy = proxy.clone();
        self.wake_scope
            .as_mut()
            .unwrap()
            .bind(move |event| {
                let _ = wake_proxy.send_event(UserEvent::Wake(event));
            })
            .expect("App wake scope must be open and unbound at native startup");
        // The AccessKit adapter routes events back through this as
        // `UserEvent::AccessKit`.
        let a11y_proxy = proxy.clone();
        #[allow(
            clippy::panic,
            reason = "invalid explicitly requested benchmark input must stop startup rather than silently run an unmeasured scenario"
        )]
        let native_scenario = crate::native_scenario::load_from_env()
            .unwrap_or_else(|error| panic!("invalid native performance scenario: {error}"));
        assert!(
            native_scenario.is_none() || self.profiler.is_some(),
            "native performance scenarios require QOS_PROFILE_OUT or QOS_PROFILE"
        );
        {
            let _entry = self.enter_wake_scope();
            setup(proxy);
            self.init_layout();
        }

        let mut handler = AppHandler {
            app: self,
            window: None,
            render: None,
            audience_presentation: None,
            floating_windows: Vec::new(),
            a11y_adapter: None,
            a11y_proxy,
            ime_state: None,
            primary_frames_sent: 0,
            primary_frames_completed: 0,
            last_primary_frame_presented: 0,
            primary_retry: crate::render_thread::retry::RenderRetryState::default(),
            floating_source_clear_frames: std::collections::HashMap::new(),
            floating_destination_present_frames: std::collections::HashMap::new(),
            touch_pointer: crate::interaction::TouchPointerEmulator::default(),
            dispatching_touch_pointer: false,
            native_scenario,
            native_input_ids: Vec::new(),
            native_input_origin: None,
            native_input_pointer_passthrough: false,
            native_input_route_isolation: false,
            animation_schedule: animation_schedule::AnimationSchedule::default(),
        };
        match event_loop.run_app(&mut handler) {
            Ok(()) => {}
            Err(winit::error::EventLoopError::ExitFailure(status)) => {
                tracing::debug!(status, "window system disconnected; application exiting");
            }
            Err(error) => {
                tracing::error!(%error, "application event loop failed; application exiting");
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.bindings.clear();
        self.layout.node_map.clear();
        self.close_wake_scope();
        let _entry = self.enter_wake_scope();
        let root = std::mem::replace(
            &mut self.root_widget,
            Box::new(crate::widget::Container::new()),
        );
        drop(root);
    }
}

fn native_floating_logical_size(rect: KurboRect) -> winit::dpi::LogicalSize<f64> {
    winit::dpi::LogicalSize::new(
        rect.width().round().max(1.0),
        rect.height().round().max(1.0),
    )
}

fn generic_floating_ids_to_dismiss(
    previous: &std::collections::HashSet<String>,
    current: &std::collections::HashSet<String>,
    native_presentations: &std::collections::HashMap<
        String,
        crate::floating_surface::FloatingSurfacePresentation,
    >,
    suspended_native: &std::collections::HashSet<String>,
) -> Vec<String> {
    previous
        .difference(current)
        .filter(|id| !native_presentations.contains_key(*id) && !suspended_native.contains(*id))
        .cloned()
        .collect()
}

fn completed_primary_frame_gates(
    gates: &std::collections::HashMap<String, Option<u64>>,
    last_presented: u64,
) -> Vec<String> {
    gates
        .iter()
        .filter_map(|(id, required)| {
            required
                .is_some_and(|required| required <= last_presented)
                .then_some(id.clone())
        })
        .collect()
}

#[cfg(test)]
mod occlusion_probe_event_tests {
    use super::*;
    #[test]
    fn occlusion_probe_only_explicit_exposure_or_interaction_bypasses_backoff() {
        let pointer = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(10.0, 20.0),
        };
        assert!(render_probe_event(&pointer));
        let mut retry = crate::render_thread::retry::RenderRetryState::default();
        assert!(
            !retry.request_probe(),
            "ordinary pointer motion adds no redraw"
        );
        retry.admitted();
        retry.observe(FrameOutcome::Occluded, Instant::now());
        assert!(
            retry.request_probe(),
            "pointer interaction wakes a blocked surface"
        );
        for event in [
            WindowEvent::Occluded(false),
            WindowEvent::Focused(true),
            WindowEvent::Resized(winit::dpi::PhysicalSize::new(800, 600)),
        ] {
            assert!(render_probe_event(&event));
        }
        for event in [
            WindowEvent::RedrawRequested,
            WindowEvent::Occluded(true),
            WindowEvent::Focused(false),
        ] {
            assert!(
                !render_probe_event(&event),
                "worker redraws must not bypass backoff"
            );
        }
    }
}

fn render_probe_event(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::Occluded(false)
            | WindowEvent::Focused(true)
            | WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. }
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::CursorMoved { .. }
            | WindowEvent::Touch(_)
            | WindowEvent::Ime(_)
            | WindowEvent::DroppedFile(_)
    )
}

fn auxiliary_redraw_allowed(render: Option<&RenderThreadHandle>) -> bool {
    render.is_none_or(|render| {
        !render.buffers.redraw_deferred() && render.retry.allows(Instant::now())
    })
}

#[cfg(test)]
#[allow(dead_code, reason = "used by feature-gated render retry tests")]
fn render_retry_deadline(outcome: FrameOutcome, now: Instant) -> Option<Instant> {
    matches!(outcome, FrameOutcome::Skipped | FrameOutcome::Reconfigured).then(|| {
        now.checked_add(Duration::from_nanos(16_666_667))
            .unwrap_or(now)
    })
}

/// The first surface submission may race a Wayland compositor's initial
/// configure. In that case wgpu reports `Reconfigured`, but no input or frame
/// callback exists yet to trigger the redraw that can consume that result.
/// Pump only until one primary frame is actually presented; steady-state apps
/// remain event-driven and idle at zero wakeups.
const fn primary_startup_pump_needed(frames_sent: u64, last_presented: u64) -> bool {
    frames_sent > 0 && last_presented == 0
}

#[cfg(test)]
mod generic_floating_activity_tests {
    use super::{
        completed_primary_frame_gates, generic_floating_ids_to_dismiss,
        native_floating_logical_size, primary_startup_pump_needed,
    };
    use crate::floating_surface::FloatingSurfacePresentation;
    use std::collections::{HashMap, HashSet};
    use vello::kurbo::Rect;

    #[test]
    fn primary_surface_pumps_only_until_its_first_presented_frame() {
        assert!(!primary_startup_pump_needed(0, 0));
        assert!(primary_startup_pump_needed(1, 0));
        assert!(primary_startup_pump_needed(3, 0));
        assert!(!primary_startup_pump_needed(3, 1));
    }

    #[test]
    fn generic_pip_activity_edge_dismisses_without_touching_native_surfaces() {
        let previous = HashSet::from([
            "browser.media-pip".to_string(),
            "chat-call:huddle".to_string(),
            "navigation:route".to_string(),
        ]);
        let current = HashSet::new();
        let native = HashMap::from([(
            "chat-call:huddle".to_string(),
            FloatingSurfacePresentation {
                id: "chat-call:huddle".into(),
                content_rect: Rect::new(40.0, 40.0, 400.0, 260.0),
                window_rect: Rect::new(32.0, 32.0, 408.0, 268.0),
                returning_to_app: false,
            },
        )]);
        let suspended = HashSet::from(["navigation:route".to_string()]);
        assert_eq!(
            generic_floating_ids_to_dismiss(&previous, &current, &native, &suspended),
            vec!["browser.media-pip".to_string()]
        );
    }

    #[test]
    fn native_float_allocation_follows_the_complete_soft_chrome() {
        let compact = native_floating_logical_size(Rect::new(80.0, 40.0, 456.0, 292.0));
        let expanded = native_floating_logical_size(Rect::new(80.0, 40.0, 456.0, 352.0));
        assert_eq!((compact.width, compact.height), (376.0, 252.0));
        assert_eq!((expanded.width, expanded.height), (376.0, 312.0));
    }

    #[test]
    fn ownership_proxy_waits_for_the_exact_primary_frame_to_present() {
        let gates = HashMap::from([
            ("waiting".to_string(), None),
            ("next".to_string(), Some(12)),
            ("later".to_string(), Some(13)),
        ]);
        assert!(completed_primary_frame_gates(&gates, 11).is_empty());
        assert_eq!(
            completed_primary_frame_gates(&gates, 12),
            vec!["next".to_string()]
        );
    }
}

/// See [`resolve_title_from`] for the precedence.
fn resolve_window_title(explicit: Option<&str>) -> String {
    let app_name = std::env::var("TILO_APP_NAME").ok();
    let package_id = std::env::var("TILO_PACKAGE_ID").ok();
    let binary = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    resolve_title_from(
        explicit,
        app_name.as_deref(),
        package_id.as_deref(),
        binary.as_deref(),
    )
}

/// Public because it is half of a protocol: what a pointer published in this
/// surface's content coordinates is scoped to
/// ([`crate::presence::PointerSpace::Surface`]) â€” both ends must spell the
/// surface the same way by calling this same function.
pub fn local_surface_id() -> String {
    let package_id = std::env::var("TILO_PACKAGE_ID").ok();
    let binary = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    resolve_app_id(package_id.as_deref(), binary.as_deref())
}

/// A shared generic id would collapse every id-keyed compositor lookup (tab
/// label, launch placement, "is this document already open") onto every
/// other app; it's a last resort for no-package, no-binary-name.
fn resolve_app_id(package_id: Option<&str>, binary: Option<&str>) -> String {
    let non_blank = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    if let Some(id) = package_id.and_then(non_blank) {
        return id;
    }
    if let Some(bin) = binary.and_then(non_blank) {
        return bin;
    }
    "org.tilo.app".to_string()
}

/// Pure, so the precedence is unit-testable without touching the process
/// environment. First non-blank candidate wins.
fn resolve_title_from(
    explicit: Option<&str>,
    app_name: Option<&str>,
    package_id: Option<&str>,
    binary: Option<&str>,
) -> String {
    let non_blank = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    if let Some(t) = explicit.and_then(non_blank) {
        return t;
    }
    if let Some(t) = app_name.and_then(non_blank) {
        return t;
    }
    if let Some(id) = package_id.and_then(non_blank) {
        let seg = id.rsplit('.').next().unwrap_or(id.as_str());
        let human = humanize_ident(seg);
        if !human.is_empty() {
            return human;
        }
    }
    if let Some(bin) = binary.and_then(non_blank) {
        let bin = bin.as_str();
        #[cfg(windows)]
        let bin = bin
            .rsplit_once('.')
            .filter(|(_, extension)| extension.eq_ignore_ascii_case("exe"))
            .map_or(bin, |(name, _)| name);
        let human = humanize_ident(bin);
        if !human.is_empty() {
            return human;
        }
    }
    "Tilo App".to_string()
}

fn humanize_ident(ident: &str) -> String {
    ident
        .split(|c: char| c == '-' || c == '_' || c == '.' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// wgpu `RenderContext`, `RenderSurface`, and `vello::Renderer` live on a
/// dedicated worker thread (see [`crate::render_thread`]) â€” this struct
/// keeps only the channel ends to ship frames and recycle scene buffers.
struct AppHandler {
    app: App,
    animation_schedule: animation_schedule::AnimationSchedule,
    /// Taken exactly once after the first primary worker presentation.
    native_scenario: Option<crate::native_scenario::Scenario>,
    /// Inputs dispatched since the last committed primary CPU frame.
    native_input_ids: Vec<usize>,
    // Opt-in native timeline shares one Instant origin across input/redraw stages.
    native_input_origin: Option<Instant>,
    /// Actual backend acknowledgement for opt-in benchmark pointer isolation.
    native_input_pointer_passthrough: bool,
    /// Frozen for the benchmark window, including the interval before its first packet.
    native_input_route_isolation: bool,
    /// `None` until the first `resumed` â€” every modern platform gates window
    /// creation behind an `ActiveEventLoop`.
    window: Option<Arc<Window>>,
    /// `None` again in `exiting`, after the worker is joined, so its surface
    /// drops BEFORE the `Window` arc goes away.
    render: Option<RenderThreadHandle>,
    /// A real, separate fullscreen output for a root widget's audience view.
    /// It is deliberately not modelled as a floating surface: it has no
    /// desktop-space proxy or compositor handoff and is always destroyed
    /// before its renderer/window during stop and app teardown.
    audience_presentation: Option<AudiencePresentationWindow>,
    floating_windows: Vec<NativeFloatingWindow>,
    /// `None` until `resumed` creates the window (must be built while
    /// hidden); lazy, so the hot path pays nothing with no screen reader
    /// running.
    a11y_adapter: Option<accesskit_winit::Adapter>,
    a11y_proxy: EventLoopProxy<UserEvent>,
    /// Last text-input-v3 state applied to winit. Edge-triggering this avoids
    /// emitting a protocol commit on every idle loop.
    ime_state: Option<ImeWindowState>,
    primary_frames_sent: u64,
    primary_frames_completed: u64,
    last_primary_frame_presented: u64,
    primary_retry: crate::render_thread::retry::RenderRetryState,
    /// `None` waits for the next primary submission; `Some(sequence)` waits
    /// until that exact-or-newer frame is reported Presented by the worker.
    floating_source_clear_frames: std::collections::HashMap<String, Option<u64>>,
    /// Reverse ownership gate. A suspended native surface remains covered by
    /// its compositor-frozen final frame until one primary app frame with the
    /// reattached widget is reported Presented.
    floating_destination_present_frames: std::collections::HashMap<String, Option<u64>>,
    /// Maps one-finger taps onto the existing pointer event contract so every
    /// standard and app-local control gets touch without bespoke branches.
    touch_pointer: crate::interaction::TouchPointerEmulator,
    /// Synthetic mouse events must retain touch modality rather than making
    /// the host immediately classify them as a fine pointer.
    dispatching_touch_pointer: bool,
}

struct AudiencePresentationWindow {
    monitor: winit::monitor::MonitorHandle,
    window: Arc<Window>,
    render: Option<RenderThreadHandle>,
    render_init: Option<mpsc::Receiver<Result<RenderThreadHandle, String>>>,
}

/// Keep the audience bound to the same monitor object across hotplug list
/// reordering; indices are only a one-tick selection aid.
fn audience_monitor_still_selected<T: PartialEq>(
    current: Option<&T>,
    selected: Option<&T>,
) -> bool {
    current
        .zip(selected)
        .is_some_and(|(current, selected)| current == selected)
}

#[cfg(test)]
mod audience_monitor_identity_tests {
    use super::audience_monitor_still_selected;

    #[test]
    fn stable_identity_survives_monitor_enumeration_reordering() {
        let audience = "connector-2";
        let reordered = ["connector-2", "connector-1"];
        assert!(audience_monitor_still_selected(
            reordered.first(),
            Some(&audience)
        ));
        assert!(!audience_monitor_still_selected(
            reordered.get(1),
            Some(&audience)
        ));
    }
}

struct NativeFloatingWindow {
    id: String,
    token: String,
    desktop_rect: KurboRect,
    window: Arc<Window>,
    render: Option<RenderThreadHandle>,
    /// Device/surface creation is much slower than a frame on software and
    /// headless backends.  Keeping it off the winit thread lets the frozen
    /// compositor proxy continue following the pointer during handoff.
    render_init: Option<mpsc::Receiver<Result<RenderThreadHandle, String>>>,
    prepared_at: Instant,
    adopted: bool,
    suspended: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImeWindowState {
    purpose: winit::window::ImePurpose,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Snapshot widget damage while its draw-updated geometry still describes the
/// scene being submitted. Callers deliberately clear dirty flags afterwards.
fn collect_submitted_widget_damage(widget: &dyn Widget, regions: &mut Vec<KurboRect>) {
    widget.collect_damage(regions);
}

/// Split out of `AppHandler::handle_redraw_requested` (`clippy::too_many_lines`).
/// Draws the root widget, its overlay, the action palette, and any visible
/// tooltip into `scene`. Returns the transform and logical window rect the
/// rest of the frame (legacy cursors, remote presence, debug outlines, HUD)
/// draws with, plus the draw-pass duration for the perf log line.
#[allow(
    clippy::too_many_lines,
    reason = "frame drawing is one ordered composition pass whose sequencing must remain explicit"
)]
fn draw_frame_content(
    app: &mut App,
    scene: &mut Scene,
    width: u32,
    height: u32,
    scale_factor: f32,
    t_draw_start: Option<std::time::Instant>,
) -> (Affine, KurboRect, u128) {
    let __prof_resources = app.profiler.as_ref().map(|_| std::time::Instant::now());
    app.refresh_control_action_availability();
    if let Some(p) = app.profiler.as_mut() {
        p.record_since(
            crate::profiling::Pass::ResourceAvailability,
            __prof_resources,
        );
    }
    // Widget rects are logical; this transform rasterises at the physical
    // resolution, so the frame stays crisp at any fractional scale.
    let transform = crate::scale::composite_transform(f64::from(scale_factor));
    let (vp_w, vp_h) = crate::scale::logical_size((width, height), f64::from(scale_factor));
    let window_rect = KurboRect::new(0.0, 0.0, vp_w, vp_h);
    let prev_vp = app.presence_ctx.push_viewport(window_rect);
    // No GPU damage clip: vello allocates a fresh `target_texture` per
    // frame, so clipping out a static region would just paint background
    // instead of preserving the last frame.
    let __prof_draw = app.profiler.as_ref().map(|_| std::time::Instant::now());
    if let Some(history) = &mut app.history {
        history.draw(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    if app
        .history
        .as_ref()
        .is_none_or(|history| history.live_visible())
        && app.history_layout.is_none_or(|layout| layout.app_visible)
    {
        crate::widget::native_view_frame_start(app.root_widget.as_mut());
        app.root_widget.draw_with_presence(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
            &app.permission_ctx,
        );
    }
    app.presence_ctx.pop_viewport(prev_vp);
    // Widgets whose visual extent escapes their layout box (an open
    // `Dropdown`, a popover) composite above every later-drawn sibling here.
    // No-op by default.
    if app
        .history
        .as_ref()
        .is_none_or(|history| history.live_visible())
        && app.history_layout.is_none_or(|layout| layout.app_visible)
    {
        app.root_widget.draw_overlay(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    if app.history_chrome_enabled {
        let context = crate::standalone_history::HistoryReadContext {
            document: &app.state.doc,
            active_record: app
                .persistence
                .records
                .as_ref()
                .and_then(|records| records.active()),
            document_identity: app.state.document_identity(),
            permissions: &app.permission_ctx,
        };
        if let Err(error) = app.document_history.draw(
            &context,
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        ) {
            tracing::debug!(?error, "history frame refused");
            app.layout.structure_dirty = true;
            app.layout.layout_dirty = true;
        }
    }
    if let Some(dialog) = app.close_admission.dialog.as_mut() {
        dialog.draw(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    if let Some(ap) = app.actions.action_palette.as_mut() {
        let win_w = f64::from(width) / f64::from(scale_factor);
        let card_w = (win_w * 0.6).clamp(360.0, 640.0);
        let x0 = ((win_w - card_w) / 2.0).max(0.0);
        let y0 = (f64::from(height) / f64::from(scale_factor) * 0.12).max(24.0);
        match app.palette_source {
            PaletteSource::Owned { .. } => ap.layout(&app.theme, Point::new(x0, y0), card_w),
            PaletteSource::Host => ap.layout_with_text(
                &app.theme,
                Point::new(x0, y0),
                card_w,
                &mut app.text_renderer,
            ),
        }
        ap.draw(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    draw_visible_tooltip(app, scene, transform, width, height, scale_factor);

    if let Some(p) = app.profiler.as_mut() {
        p.record_since(crate::profiling::Pass::Draw, __prof_draw);
    }
    let t_draw_us = t_draw_start.map_or(0, |t| t.elapsed().as_micros());
    (transform, window_rect, t_draw_us)
}

fn draw_visible_tooltip(
    app: &mut App,
    scene: &mut Scene,
    transform: Affine,
    width: u32,
    height: u32,
    scale_factor: f32,
) {
    app.input.tooltip_host.tick(Instant::now());
    let Some((text, anchor)) = app
        .input
        .tooltip_host
        .visible()
        .map(|(label, anchor)| (crate::i18n::I18nManager::lookup(label, None), anchor))
    else {
        return;
    };
    let viewport = KurboRect::new(
        0.0,
        0.0,
        f64::from(width) / f64::from(scale_factor),
        f64::from(height) / f64::from(scale_factor),
    );
    crate::widget::tooltip::draw_hover_tooltip(
        scene,
        transform,
        &mut app.text_renderer,
        &app.theme,
        anchor,
        &text,
        viewport,
    );
}

/// Split out of `AppHandler::handle_redraw_requested` (`clippy::too_many_lines`).
/// Only active in `App::debug_mode` â€” draws each widget's layout rect and a
/// depth-cycled label over the frame just rendered.
fn draw_legacy_cursors(app: &mut App, scene: &mut Scene, transform: Affine) {
    for cursor in app.state.collab.cursors.values() {
        let mut path = vello::kurbo::BezPath::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(14.0, 14.0));
        path.line_to(Point::new(6.0, 16.0));
        path.line_to(Point::new(0.0, 22.0));
        path.close_path();

        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine composition/translation is matrix multiplication on f64 fields; it cannot panic or overflow"
        )]
        let cursor_transform =
            transform * Affine::translate((f64::from(cursor.x), f64::from(cursor.y)));
        scene.fill(
            vello::peniko::Fill::NonZero,
            cursor_transform,
            cursor.color,
            None,
            &path,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            cursor_transform,
            app.theme.tokens.colors.on_surface(cursor.color).ink,
            None,
            &path,
        );

        app.text_renderer.draw_text(
            scene,
            &cursor.name,
            10.0,
            app.theme.tokens.colors.on_surface(cursor.color).ink,
            {
                #[allow(clippy::arithmetic_side_effects, reason = "Affine composition/translation is matrix multiplication on f64 fields; it cannot panic or overflow")]
                let name_transform = cursor_transform * Affine::translate((10.0, 20.0));
                name_transform
            },
            None,
        );
    }
}

fn draw_debug_outlines(app: &mut App, scene: &mut Scene, transform: Affine) {
    fn draw_debug_recursive(
        widget: &dyn Widget,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        depth: usize,
        cycle: &[Color; 3],
    ) {
        let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
            return;
        };
        let rect = widget.get_rect();
        let &[a, b, c] = cycle;
        let color = match depth.checked_rem(3).unwrap_or(0) {
            0 => a,
            1 => b,
            _ => c,
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            color,
            None,
            &rect,
        );
        let label = format!("{}: {}", widget.name(), depth);
        let text_y = (rect.y0 + 12.0).min(rect.y1);
        let bg_rect = KurboRect::new(rect.x0, rect.y0, rect.x0 + 100.0, rect.y0 + 14.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 180),
            None,
            &bg_rect,
        );
        text_renderer.draw_text(
            scene,
            &label,
            10.0,
            color,
            {
                #[allow(clippy::arithmetic_side_effects, reason = "Affine composition/translation is matrix multiplication on f64 fields; it cannot panic or overflow")]
                let label_transform = transform * Affine::translate((rect.x0 + 2.0, text_y));
                label_transform
            },
            None,
        );
        widget.for_each_child(&mut |child| {
            draw_debug_recursive(
                child,
                scene,
                transform,
                text_renderer,
                depth.saturating_add(1),
                cycle,
            );
        });
    }
    let debug_outline_cycle = [
        app.theme.tokens.colors.semantic.warning,
        app.theme.tokens.colors.semantic.info,
        app.theme.tokens.colors.semantic.success,
    ];
    draw_debug_recursive(
        app.root_widget.as_ref(),
        scene,
        transform,
        &mut app.text_renderer,
        0,
        &debug_outline_cycle,
    );
}

/// Split out of `AppHandler::handle_redraw_requested` (`clippy::too_many_lines`).
/// `None` means taffy layout computation failed and the caller must abandon
/// the frame; otherwise `(relayout_branch, compute_us, apply_us)` for the
/// perf log line.
#[allow(
    clippy::too_many_lines,
    reason = "one bounded solve/recompose/focus/mount lifecycle keeps ownership and final-frame ordering explicit"
)]
fn run_layout_pass(
    app: &mut App,
    root_node: NodeId,
    width: u32,
    height: u32,
    scale_factor: f32,
    structure_rebuilt: bool,
    native_host: Option<&crate::widget::NativeViewHost<'_>>,
) -> Option<(bool, u128, u128)> {
    use crate::widget::layout::{
        CompositionFocus, composition_geometry, settle_widget_composition,
    };
    let logical = crate::scale::logical_size((width, height), f64::from(scale_factor));
    if app.close_admission.dialog.is_some() {
        app.layout_close_dialog(logical);
        return Some((true, 0, 0));
    }
    app.refresh_framework_history(logical.0);
    let viewport_changed = app.layout.last_layout_logical != Some(logical);
    if app.history_chrome_enabled {
        if (app.history_viewport_width >= 900.0) != (logical.0 >= 900.0) {
            app.layout.structure_dirty = true;
        }
        app.history_viewport_width = logical.0;
        let _ = app.history_local_resources();
        app.document_history
            .sync(&mut app.state.doc, &app.state.root);
    }
    if !app.layout.layout_dirty && !viewport_changed {
        return Some((false, 0, 0));
    }
    let mut t_compute_us = 0u128;
    let mut t_apply_us = 0u128;
    let mut current_root = root_node;
    let mut recomposed = false;
    let view = app.root_widget.view_state();
    // A pending structural rebuild means the old map may already contain
    // removed controls. Capture only from an admitted, unchanged topology.
    app.layout.structure_dirty |= app.root_widget.take_structure_dirty();
    let focus = if app.layout.structure_dirty {
        CompositionFocus::capture_live(app.root_widget.as_ref())
    } else {
        // SAFETY: the admitted maps belong to App's unchanged exclusive tree.
        unsafe { CompositionFocus::capture(app.focused_node, &app.layout.node_map, &app.bindings) }
    };
    let must_compute = structure_rebuilt
        || app.layout.structure_dirty
        || viewport_changed
        || scroll_opt_disabled()
        || app.root_widget.layout_needs_measure();
    let result = settle_widget_composition(viewport_changed, |rebuild| {
        if rebuild || app.layout.structure_dirty {
            recomposed = true;
            // Capture before build replaces controls. Shared view hooks carry
            // root-owned scroll/mode; persisted Scrollable offsets also survive.
            let autofocus = app.autofocus_on_structure_change;
            app.autofocus_on_structure_change = false;
            app.rebuild_structure();
            app.autofocus_on_structure_change = autofocus;
            if let Some(view) = &view {
                app.root_widget.restore_view_state(view);
            }
            // Sync may legitimately replace newly mounted delegates. Never
            // inspect or measure stale raw pointers from that build.
            if app.layout.structure_dirty {
                return Ok((None, true));
            }
            if let Some(focus) = &focus {
                // SAFETY: rebuild completed, and sync retained this topology.
                // A trap narrows eligible targets; it must not bypass identity
                // restoration or send saved selection to a fallback control.
                app.focused_node = unsafe {
                    focus.restore(
                        &app.layout.node_map,
                        &app.bindings,
                        Some(&app.focusable_order),
                    )
                };
                if app.accessibility.focus_trap_active && app.focused_node.is_none() {
                    app.set_focused_node(app.focusable_order.first().copied());
                }
            } else {
                // Anonymous retained widgets keep their own state. Resolve a
                // current admitted focus path; the old NodeId is invalid here.
                app.focused_node = unsafe {
                    CompositionFocus::current(
                        &app.layout.node_map,
                        &app.bindings,
                        Some(&app.focusable_order),
                    )
                };
            }
            current_root = app
                .layout
                .root_node
                .ok_or(crate::widget::WidgetLayoutError::CompositionDidNotSettle)?;
        }
        if must_compute || rebuild {
            let start = std::time::Instant::now();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "Taffy uses f32 logical pixels"
            )]
            // SAFETY: App exclusively owns this exact stable tree and maps.
            unsafe {
                crate::widget::compute_widget_layout(
                    &mut app.layout.taffy,
                    current_root,
                    taffy::Size {
                        width: AvailableSpace::Definite(logical.0 as f32),
                        height: AvailableSpace::Definite(logical.1 as f32),
                    },
                    &app.layout.node_map,
                    &app.bindings,
                    &mut app.text_renderer,
                )?;
            }
            t_compute_us = t_compute_us.saturating_add(start.elapsed().as_micros());
        }
        let start = std::time::Instant::now();
        app.apply_history_presentation(current_root);
        t_apply_us = t_apply_us.saturating_add(start.elapsed().as_micros());
        let dirty = app.root_widget.take_structure_dirty();
        // Translation-only frames avoid both composition and a tree snapshot.
        let geometry = if viewport_changed {
            composition_geometry(&app.layout.taffy, current_root)?
        } else {
            vec![]
        };
        Ok((Some(geometry), dirty))
    });
    if let Err(error) = result {
        tracing::error!(?error, "widget layout did not settle; skipping frame");
        app.layout.layout_dirty = true;
        app.layout.last_layout_logical = None;
        return None;
    }
    if app.settle_history_presentation() {
        let root = app.layout.root_node?;
        return run_layout_pass(app, root, width, height, scale_factor, true, native_host);
    }
    // NativeView measurement is independent of platform attachment. Mount only
    // the successfully settled tree; mount publishes its retained geometry.
    if (structure_rebuilt || recomposed)
        && app
            .history
            .as_ref()
            .is_none_or(|history| history.live_interactive())
        && let Some(host) = native_host
    {
        crate::widget::mount_native_views(app.root_widget.as_mut(), host);
    }
    if app
        .history
        .as_ref()
        .is_some_and(|history| !history.live_interactive())
    {
        crate::widget::unmount_native_views(app.root_widget.as_mut());
    }
    app.layout.last_layout_logical = Some(logical);
    app.layout.layout_dirty = false;
    if must_compute || recomposed {
        app.update_focusable_order();
    }
    app.restore_history_focus();
    if app
        .focused_node
        .is_some_and(|node| !app.focusable_order.contains(&node))
    {
        app.set_focused_node(app.focusable_order.first().copied());
    }
    if let Some(profiler) = app.profiler.as_mut() {
        for (pass, micros) in [
            (crate::profiling::Pass::BuildLayout, t_compute_us),
            (crate::profiling::Pass::ApplyLayout, t_apply_us),
        ] {
            profiler.record(
                pass,
                std::time::Duration::from_micros(u64::try_from(micros).unwrap_or(u64::MAX)),
            );
        }
    }
    Some((true, t_compute_us, t_apply_us))
}

fn create_window_render(
    window: &Arc<Window>,
    transparent: bool,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<RenderThreadHandle, String> {
    create_window_render_prepared(window, transparent, None, proxy)
}

pub(crate) struct PreparedRenderer {
    context: RenderContext,
    device_id: usize,
    renderer: Renderer,
}

#[cfg(windows)]
pub(crate) type RendererPreparation = mpsc::Receiver<Result<PreparedRenderer, String>>;

/// Start only for a real native launch, before loading app state and fonts.
/// This owns no window/surface and can safely finish if construction fails:
/// dropping the receiving end drops the prepared GPU resources at send.
#[cfg(windows)]
pub(crate) fn prepare_native_renderer() -> Option<RendererPreparation> {
    if std::env::var_os("TILO_SERIAL_RENDER_INIT").is_some() {
        return None;
    }
    let (sender, receiver) = mpsc::channel();
    match std::thread::Builder::new()
        .name("native-render-prepare".into())
        .spawn(move || {
            let prepare = || {
                tilo_startup_metrics::mark("native-render-prepare-start");
                let mut context = RenderContext::new();
                let device_id = pollster::block_on(context.device(None))
                    .ok_or_else(|| "could not prepare a drawing device".to_string())?;
                let device = context
                    .devices
                    .get(device_id)
                    .ok_or_else(|| "prepared drawing device index out of range".to_string())?;
                let renderer = create_native_renderer(device)?;
                tilo_startup_metrics::mark("native-render-prepare-done");
                Ok(PreparedRenderer {
                    context,
                    device_id,
                    renderer,
                })
            };
            let _ = sender.send(prepare());
        }) {
        Ok(_) => Some(receiver),
        Err(error) => {
            tracing::warn!(%error, "could not prepare renderer in parallel");
            None
        }
    }
}

fn create_window_render_prepared(
    window: &Arc<Window>,
    transparent: bool,
    prepared: Option<PreparedRenderer>,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<RenderThreadHandle, String> {
    tilo_startup_metrics::mark("window-render-context-start");
    let size = window.inner_size();
    let (mut render_cx, prepared_renderer) = match prepared {
        Some(prepared) => (
            prepared.context,
            Some((prepared.device_id, prepared.renderer)),
        ),
        None => (RenderContext::new(), None),
    };
    let mut surface = pollster::block_on(render_cx.create_surface(
        window.clone(),
        size.width.max(1),
        size.height.max(1),
        vello::wgpu::PresentMode::AutoVsync,
    ))
    .map_err(|e| format!("could not create a drawing surface: {e}"))?;
    if transparent {
        // `Auto` commonly resolves to Opaque on Wayland even when winit's
        // window was created transparent. Floating surfaces paint
        // premultiplied Vello output, so request the matching compositor mode
        // explicitly; otherwise the transparent gutter becomes a black box.
        surface.config.alpha_mode = vello::wgpu::CompositeAlphaMode::PreMultiplied;
    }
    let device_handle = render_cx
        .devices
        .get(surface.dev_id)
        .ok_or_else(|| "render device index out of range".to_string())?;
    // Flipping to the sRGB suffix here would panic on mismatch with the
    // format `create_surface` already chose. `configure` returns no
    // `Result`, so guard it like the worker's reconfigures do
    // (`render_thread::configure_surface_checked`) â€” an already-lost
    // surface would otherwise hit wgpu's panicking default handler.
    tilo_startup_metrics::mark("window-surface-validation-scope-start");
    let configure_scope = device_handle
        .device
        .push_error_scope(vello::wgpu::ErrorFilter::Validation);
    tilo_startup_metrics::mark("window-surface-checked-configure-start");
    surface
        .surface
        .configure(&device_handle.device, &surface.config);
    tilo_startup_metrics::mark("window-surface-checked-configure-done");
    if let Some(err) = pollster::block_on(configure_scope.pop()) {
        return Err(format!("could not configure the drawing surface: {err}"));
    }
    tilo_startup_metrics::mark("window-surface-validation-scope-done");
    // create_surface checks presentation support and selects another device
    // if necessary. Never use pipelines built for a different device.
    let renderer = match prepared_renderer {
        Some((device_id, renderer)) if device_id == surface.dev_id => renderer,
        _ => create_native_renderer(device_handle)?,
    };
    // After this, every GPU call happens on its render worker.
    let window_id = window.id();
    let render = spawn_render_thread(
        render_cx,
        surface,
        renderer,
        Arc::new(move || {
            // Avoid synchronous macOS main-thread dispatch while the UI waits on submission.
            let _ = proxy.send_event(UserEvent::RenderReady(window_id));
        }),
    )?;
    tilo_startup_metrics::mark("window-render-context-done");
    Ok(render)
}

fn create_native_renderer(device_handle: &crate::gpu::DeviceHandle) -> Result<Renderer, String> {
    let device = &device_handle.device;
    tilo_startup_metrics::mark("window-vello-init-start");
    let renderer = Renderer::new(
        device,
        RendererOptions {
            // Diagnostic/low-end fallback: CPU coarse stages with the same
            // GPU fine raster. Kept opt-in until the benchmark proves it is
            // faster on a device; the default remains the GPU pipeline.
            use_cpu: std::env::var_os("QUICK_UI_VELLO_CPU").is_some(),
            // Every frame below renders with `AaConfig::Area`. Advertising
            // 8x/16x MSAA as well only compiles and retains shader variants
            // this framework never selects, increasing startup and GPU
            // memory pressure on exactly the low-end devices we target.
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .map_err(|e| format!("could not create the renderer: {e}"))?;
    tilo_startup_metrics::mark("window-vello-init-done");
    #[cfg(windows)]
    device_handle.finish_shader_cache();
    Ok(renderer)
}

fn begin_window_render_init(
    window: Arc<Window>,
    transparent: bool,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<mpsc::Receiver<Result<RenderThreadHandle, String>>, String> {
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("floating-render-init".into())
        .spawn(move || {
            let result = create_window_render(&window, transparent, proxy);
            // The surface may have been permanently closed while a slow GPU
            // backend was still probing. In that case, stop the newly-created
            // render worker here so its surface is dropped before `window`.
            if let Err(mpsc::SendError(result)) = result_tx.send(result)
                && let Ok(mut render) = result
            {
                render.shutdown_and_join();
            }
        })
        .map_err(|error| format!("could not start floating renderer setup: {error}"))?;
    Ok(result_rx)
}

impl AppHandler {
    /// Called before showing the window on Windows, and from the first
    /// `RedrawRequested` elsewhere so GPU init doesn't starve Wayland's
    /// initial focus events. `Err` is returned, not panicked, since every step here can
    /// fail when the compositor exits mid-open.
    fn init_render(&mut self, window: &Arc<Window>) -> Result<(), String> {
        #[cfg(windows)]
        if let Some(preparation) = self.app.startup_renderer.take() {
            match preparation.recv() {
                Ok(Ok(prepared)) => {
                    self.render = Some(create_window_render_prepared(
                        window,
                        false,
                        Some(prepared),
                        self.a11y_proxy.clone(),
                    )?);
                    return Ok(());
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "renderer preparation failed; selecting device for the actual surface")
                }
                Err(error) => {
                    tracing::warn!(%error, "renderer preparation disconnected; selecting device for the actual surface")
                }
            }
        }
        self.render = Some(create_window_render(
            window,
            false,
            self.a11y_proxy.clone(),
        )?);
        Ok(())
    }

    /// Reconcile the root's optional audience projection with a *separate*
    /// native toplevel. A missing monitor list, a one-output session, or an
    /// invalid/self-target all leave this `None`; the root's normal fullscreen
    /// intent then supplies the single-output fallback.
    fn sync_audience_presentation(
        &mut self,
        event_loop: &ActiveEventLoop,
        presenter: &Arc<Window>,
    ) {
        let _entry = self.app.enter_wake_scope();
        let request = self.app.root_widget.audience_presentation_request();
        let monitors: Vec<winit::monitor::MonitorHandle> = presenter.available_monitors().collect();
        let primary_monitor = presenter.primary_monitor();
        let local_monitor = presenter
            .current_monitor()
            .or_else(|| primary_monitor.clone());
        let presenter_index = local_monitor.as_ref().and_then(|current| {
            monitors
                .iter()
                .position(|monitor| monitor == current)
                .and_then(|index| u32::try_from(index).ok())
        });
        let infos: Vec<crate::fullscreen::MonitorInfo> = monitors
            .iter()
            .enumerate()
            .map(|(index, monitor)| crate::fullscreen::MonitorInfo {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                width: monitor.size().width,
                height: monitor.size().height,
                primary: primary_monitor.as_ref() == Some(monitor),
                scale: monitor.scale_factor(),
            })
            .collect();
        let target = request.and_then(|request| {
            crate::fullscreen::select_audience_output(&infos, request.target, presenter_index)
        });

        if self.audience_presentation.as_ref().is_some_and(|audience| {
            audience_monitor_still_selected(
                target.and_then(|index| monitors.get(index as usize)),
                Some(&audience.monitor),
            )
        }) {
            return;
        }
        self.close_audience_presentation();
        let Some(index) = target else {
            return;
        };
        let Some(monitor) = monitors.get(index as usize).cloned() else {
            return;
        };

        let attributes = Window::default_attributes()
            .with_title("Audience presentation")
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false);
        #[cfg(target_os = "linux")]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            let package_id = std::env::var("TILO_PACKAGE_ID").ok();
            let app_id = resolve_app_id(
                self.app.window_app_id.as_deref().or(package_id.as_deref()),
                std::env::current_exe()
                    .ok()
                    .and_then(|path| {
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
                    .as_deref(),
            );
            // A role-like suffix keeps a compositor from mistaking the
            // audience output for a duplicate editor surface.
            attributes.with_name(format!("{app_id}.audience"), "")
        };
        let Ok(window) = event_loop.create_window(attributes) else {
            tracing::warn!(
                "could not create separate audience presentation window; using single-output fallback"
            );
            return;
        };
        let window = Arc::new(window);
        window.set_fullscreen(Some(Fullscreen::Borderless(Some(monitor.clone()))));
        window.set_visible(true);
        window.request_redraw();
        self.audience_presentation = Some(AudiencePresentationWindow {
            monitor,
            window,
            render: None,
            render_init: None,
        });
        self.app.root_widget.on_audience_presentation_changed(true);
        self.app.layout.layout_dirty = true;
        presenter.request_redraw();
    }

    /// Drop the render worker before the Window Arc, as required by wgpu's
    /// surface lifetime. This is deliberately idempotent: hotplug, explicit
    /// stop, and process teardown can race through the same state transition.
    fn close_audience_presentation(&mut self) {
        let _entry = self.app.enter_wake_scope();
        let Some(mut audience) = self.audience_presentation.take() else {
            return;
        };
        audience.window.set_visible(false);
        if let Some(mut render) = audience.render.take() {
            render.shutdown_and_join();
        }
        self.app.root_widget.on_audience_presentation_changed(false);
        self.app.layout.layout_dirty = true;
    }

    /// Paint one audience frame without running a second widget layout or
    /// synchronising a second document. The root is responsible for a pure
    /// audience projection of the same model used by the presenter surface.
    fn handle_audience_redraw(&mut self) -> bool {
        let _entry = self.app.enter_wake_scope();
        let Some(audience) = self.audience_presentation.as_mut() else {
            return false;
        };
        if audience.render.is_none() {
            if audience.render_init.is_none() {
                match begin_window_render_init(
                    audience.window.clone(),
                    false,
                    self.a11y_proxy.clone(),
                ) {
                    Ok(init) => audience.render_init = Some(init),
                    Err(error) => tracing::warn!(%error, "could not start audience renderer"),
                }
            }
            let Some(init) = audience.render_init.as_ref() else {
                return true;
            };
            match init.try_recv() {
                Ok(Ok(render)) => {
                    audience.render = Some(render);
                    audience.render_init = None;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "could not initialise audience renderer");
                    audience.render_init = None;
                    return false;
                }
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    audience.render_init = None;
                    return false;
                }
            }
        }
        let Some(render) = audience.render.as_mut() else {
            return false;
        };
        while let Ok(outcome) = render.frame_complete.try_recv() {
            match outcome {
                FrameOutcome::Presented
                | FrameOutcome::Reconfigured
                | FrameOutcome::Skipped
                | FrameOutcome::Occluded => {
                    render.retry.observe(outcome, Instant::now());
                }
                FrameOutcome::SurfaceGone => return false,
            }
        }
        match crate::render_thread::prepare_frame(
            &mut render.retry,
            &mut render.buffers,
            Instant::now(),
        ) {
            Ok(true) => {}
            Ok(false) => return true,
            Err(_) => return false,
        }
        let (mut scene, mut damage_regions) = match render.buffers.try_recv() {
            Ok(scene) => scene,
            Err(mpsc::TryRecvError::Empty) => {
                return true;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                tracing::warn!("audience render worker exited");
                return false;
            }
        };
        damage_regions.clear();
        let size = audience.window.inner_size();
        let scale = audience.window.scale_factor();
        let rect = vello::kurbo::Rect::new(
            0.0,
            0.0,
            f64::from(size.width) / scale,
            f64::from(size.height) / scale,
        );
        let drawn = self.app.root_widget.draw_audience_presentation(
            &mut scene,
            Affine::scale(scale),
            &mut self.app.text_renderer,
            &self.app.theme,
            rect,
        );
        if !drawn {
            return false;
        }
        if render
            .render_tx
            .send(RenderRequest::Frame {
                cpu_frame_id: None,
                scene,
                width: size.width.max(1),
                height: size.height.max(1),
                base_color: self.app.theme.bg_app,
                damage_regions,
                full_redraw: true,
            })
            .is_err()
        {
            tracing::warn!(
                "audience render worker stopped; returning to the single-output fallback"
            );
            return false;
        }
        true
    }

    fn sync_ime(&mut self, window: &Window) {
        let _entry = self.app.enter_wake_scope();
        let next = self.app.ime_window_state();
        if next == self.ime_state {
            return;
        }
        match next {
            Some(intent) => {
                // Apply purpose before enabling: winit's Wayland backend
                // commits the enable immediately, so the first activation
                // must already carry the right content type. Cursor geometry
                // follows enable because winit intentionally ignores cursor
                // updates while IME is disabled.
                window.set_ime_purpose(intent.purpose);
                window.set_ime_allowed(true);
                window.set_ime_cursor_area(
                    winit::dpi::LogicalPosition::new(intent.x, intent.y),
                    winit::dpi::LogicalSize::new(intent.width, intent.height),
                );
            }
            None => window.set_ime_allowed(false),
        }
        self.ime_state = next;
    }

    fn create_native_floating_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: String,
        token: String,
        rect: KurboRect,
    ) -> Result<(), String> {
        let _entry = self.app.enter_wake_scope();
        if let Some(existing) = self
            .floating_windows
            .iter_mut()
            .find(|surface| surface.id == id)
        {
            if existing.token != token {
                return Err("shell changed the token of a live floating surface".into());
            }
            if existing.desktop_rect.size() != rect.size() {
                let _ = existing
                    .window
                    .request_inner_size(native_floating_logical_size(rect));
            }
            existing.desktop_rect = rect;
            existing.prepared_at = Instant::now();
            existing.adopted = false;
            existing.suspended = false;
            existing.window.set_visible(true);
            existing.window.request_redraw();
            return Ok(());
        }
        let logical_size = native_floating_logical_size(rect);
        let attributes = Window::default_attributes()
            .with_title(format!("Floating Â· {id}"))
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_visible(false)
            .with_inner_size(logical_size);
        #[cfg(target_os = "linux")]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes.with_name(
                format!(
                    "{}{}",
                    crate::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX,
                    token
                ),
                "",
            )
        };
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| format!("create floating surface window: {error}"))?,
        );
        window.set_visible(true);
        window.request_redraw();
        self.floating_windows.push(NativeFloatingWindow {
            id,
            token,
            desktop_rect: rect,
            window,
            render: None,
            render_init: None,
            prepared_at: Instant::now(),
            adopted: false,
            suspended: false,
        });
        Ok(())
    }

    fn suspend_native_floating_window(&mut self, id: &str) -> bool {
        let _entry = self.app.enter_wake_scope();
        let Some(surface) = self
            .floating_windows
            .iter_mut()
            .find(|surface| surface.id == id)
        else {
            return false;
        };
        surface.window.set_visible(false);
        surface.suspended = true;
        surface.adopted = false;
        true
    }

    fn close_native_floating_window(&mut self, id: &str) -> bool {
        let _entry = self.app.enter_wake_scope();
        let Some(index) = self
            .floating_windows
            .iter()
            .position(|surface| surface.id == id)
        else {
            return false;
        };
        let mut surface = self.floating_windows.remove(index);
        surface.window.set_visible(false);
        if let Some(mut render) = surface.render.take() {
            render.shutdown_and_join();
        }
        true
    }

    fn apply_floating_host_messages(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let _entry = self.app.enter_wake_scope();
        let mut changed = false;
        for message in self.app.take_floating_host_messages() {
            match message {
                crate::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                    id,
                    token,
                    rect,
                    content_rect: _,
                    transfer_covered,
                    coordinate_offset: _,
                } => match crate::floating_surface::protocol_rect(rect) {
                    Ok(rect) => {
                        if transfer_covered {
                            self.floating_source_clear_frames.insert(id.clone(), None);
                            if let Some(primary) = self.window.as_ref() {
                                primary.request_redraw();
                            }
                        }
                        match self.create_native_floating_window(event_loop, id, token, rect) {
                            Ok(()) => changed = true,
                            Err(error) => {
                                tracing::warn!(%error, "could not prepare native floating surface");
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "shell sent invalid floating surface rect");
                    }
                },
                crate::floating_surface::FloatingSurfaceHostMessage::Adopted { id, rect } => {
                    if crate::floating_surface::protocol_rect(rect).is_ok()
                        && let Some(surface) = self
                            .floating_windows
                            .iter_mut()
                            .find(|surface| surface.id == id)
                    {
                        surface.adopted = true;
                        surface.window.request_redraw();
                        changed = true;
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Reattach { id }
                | crate::floating_surface::FloatingSurfaceHostMessage::Closed { id } => {
                    self.floating_source_clear_frames.remove(&id);
                    self.floating_destination_present_frames.remove(&id);
                    changed |= self.close_native_floating_window(&id);
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Suspended { id } => {
                    self.floating_source_clear_frames.remove(&id);
                    self.floating_destination_present_frames
                        .insert(id.clone(), None);
                    changed |= self.suspend_native_floating_window(&id);
                    if let Some(primary) = self.window.as_ref() {
                        primary.request_redraw();
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::DesktopBounds { .. } => {}
            }
        }
        changed
    }

    /// A prepared surface that never commits must not strand its app-owned
    /// content half outside the primary window forever. Keep polling only
    /// while the first buffer is pending, then ask the shell to close the
    /// abandoned capability; `Closed` drives the ordinary animated return.
    fn recover_stalled_floating_windows(&mut self) -> bool {
        // A frozen compositor-side transfer frame keeps the surface visually
        // continuous while a cold GPU backend prepares. This deadline is now
        // a genuine failure bound, not a UX timer: ordinary adoption replaces
        // the proxy on its first committed buffer.
        // A cold secondary WGPU surface can spend several seconds enumerating
        // fallbacks on software/headless systems. The compositor's transfer
        // proxy is already covering every pixel, so returning at four seconds
        // caused a needless visible snap just before a viable backend could
        // finish. Ten seconds remains a bounded failure path without turning
        // ordinary slow device discovery into UX churn.
        const FIRST_BUFFER_DEADLINE: Duration = Duration::from_secs(10);
        let _entry = self.app.enter_wake_scope();
        let stalled = self
            .floating_windows
            .iter()
            .filter(|surface| {
                !surface.suspended
                    && !surface.adopted
                    && surface.prepared_at.elapsed() >= FIRST_BUFFER_DEADLINE
            })
            .map(|surface| surface.id.clone())
            .collect::<Vec<_>>();
        if stalled.is_empty() {
            return false;
        }
        for id in stalled {
            tracing::warn!(%id, "floating surface missed its first-buffer deadline; returning it to the app");
            if let Some(client) = self.app.floating_surface_client.as_mut() {
                let _ = client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::Dismiss {
                        id: id.clone(),
                    },
                );
            }
            self.close_native_floating_window(&id);
        }
        true
    }

    fn sync_native_floating_geometry(&mut self) -> bool {
        let _entry = self.app.enter_wake_scope();
        let presentations = self.app.root_widget.floating_surface_presentations();
        let mut changed = false;
        for surface in &mut self.floating_windows {
            if let Some(presentation) = presentations
                .iter()
                .find(|presentation| presentation.id == surface.id)
                && presentation.window_rect != surface.desktop_rect
            {
                if presentation.window_rect.size() != surface.desktop_rect.size() {
                    // App-owned live activities can change silhouette while
                    // detached: a call gains participant rows, navigation
                    // reveals another instruction, or accessibility text
                    // grows. Resize the real transparent buffer with the
                    // shared presentation rect so its soft chrome is never
                    // clipped inside the old native allocation.
                    let _ = surface
                        .window
                        .request_inner_size(native_floating_logical_size(presentation.window_rect));
                }
                surface.desktop_rect = presentation.window_rect;
                surface.window.request_redraw();
                changed = true;
            }
        }
        changed
    }

    fn handle_floating_redraw(&mut self, index: usize) -> bool {
        let _entry = self.app.enter_wake_scope();
        let (app, surfaces) = (&mut self.app, &mut self.floating_windows);
        let Some(surface) = surfaces.get_mut(index) else {
            return false;
        };
        if surface.suspended {
            return true;
        }
        if surface.render.is_none() {
            if surface.render_init.is_none() {
                match begin_window_render_init(
                    surface.window.clone(),
                    true,
                    self.a11y_proxy.clone(),
                ) {
                    Ok(init) => surface.render_init = Some(init),
                    Err(error) => {
                        tracing::warn!(%error, id = %surface.id, "floating surface renderer failed");
                        return false;
                    }
                }
            }
            let Some(init) = surface.render_init.as_ref() else {
                return false;
            };
            match init.try_recv() {
                Ok(Ok(render)) => {
                    surface.render = Some(render);
                    surface.render_init = None;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, id = %surface.id, "floating surface renderer failed");
                    surface.render_init = None;
                    return false;
                }
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    tracing::warn!(id = %surface.id, "floating surface renderer setup stopped");
                    surface.render_init = None;
                    return false;
                }
            }
        }
        let Some(render) = surface.render.as_mut() else {
            return false;
        };
        while let Ok(outcome) = render.frame_complete.try_recv() {
            match outcome {
                FrameOutcome::Presented
                | FrameOutcome::Reconfigured
                | FrameOutcome::Skipped
                | FrameOutcome::Occluded => {
                    render.retry.observe(outcome, Instant::now());
                }
                FrameOutcome::SurfaceGone => return false,
            }
        }
        match crate::render_thread::prepare_frame(
            &mut render.retry,
            &mut render.buffers,
            Instant::now(),
        ) {
            Ok(true) => {}
            Ok(false) => return true,
            Err(_) => return false,
        }
        let (mut scene, mut damage_regions) = match render.buffers.try_recv() {
            Ok(buffers) => buffers,
            Err(mpsc::TryRecvError::Empty) => return true,
            Err(mpsc::TryRecvError::Disconnected) => return false,
        };
        damage_regions.clear();
        // Every pooled scene is either newly seeded or reset by the worker
        // before recycling. Resetting it again here only rewrote all stream
        // lengths on every frame.
        let inner = surface.window.inner_size();
        let scale = surface.window.scale_factor();
        let transform = crate::scale::composite_transform(scale)
            .pre_translate((-surface.desktop_rect.x0, -surface.desktop_rect.y0).into());
        app.root_widget.draw_floating_surfaces(
            &mut scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
        if render
            .render_tx
            .send(RenderRequest::Frame {
                cpu_frame_id: None,
                scene,
                width: inner.width.max(1),
                height: inner.height.max(1),
                base_color: Color::TRANSPARENT,
                damage_regions,
                full_redraw: true,
            })
            .is_err()
        {
            return false;
        }
        true
    }

    fn dispatch_floating_event(
        &mut self,
        window: &Arc<Window>,
        desktop_rect: KurboRect,
        event: WindowEvent,
    ) {
        let _entry = self.app.enter_wake_scope();
        let scale = window.scale_factor();
        let event = match event {
            WindowEvent::CursorMoved {
                device_id,
                position,
            } => WindowEvent::CursorMoved {
                device_id,
                position: PhysicalPosition::new(
                    position.x + desktop_rect.x0 * scale,
                    position.y + desktop_rect.y0 * scale,
                ),
            },
            WindowEvent::Touch(mut touch) => {
                touch.location = PhysicalPosition::new(
                    touch.location.x + desktop_rect.x0 * scale,
                    touch.location.y + desktop_rect.y0 * scale,
                );
                WindowEvent::Touch(touch)
            }
            other => other,
        };
        let app = &mut self.app;
        let Some(root_node) = app
            .history_layout
            .map(|layout| layout.app)
            .or(app.layout.root_node)
        else {
            return;
        };
        let mut handled = false;
        let mut focus_target = None;
        app.refresh_control_action_availability();
        app.root_widget.handle_event(
            &event,
            scale,
            &app.input.modifiers,
            &mut app.state.doc,
            &app.state.root,
            &mut handled,
            &mut focus_target,
            root_node,
            &app.layout.taffy,
        );
        // `handle_event` may change presentation-only resource declarations
        // without touching the CRDT (selected tab, open media, local mode).
        app.invalidate_widget_resource_cache();
        if let Some(id) = focus_target {
            app.set_focused_node(Some(id));
        }
        if handled {
            app.commit_action();
            app.needs_sync = true;
            app.layout.layout_dirty |= app.root_widget.is_layout_dirty();
            if let Some(primary) = self.window.as_ref() {
                primary.request_redraw();
            }
            for surface in &self.floating_windows {
                surface.window.request_redraw();
            }
        }
    }

    fn visible_auxiliary_needs_shared_root(&self) -> bool {
        self.floating_windows.iter().any(|surface| {
            !surface.suspended
                && surface
                    .render
                    .as_ref()
                    .is_none_or(|render| !render.retry.known_occluded())
        }) || self.audience_presentation.as_ref().is_some_and(|audience| {
            audience
                .render
                .as_ref()
                .is_none_or(|render| !render.retry.known_occluded())
        })
    }

    /// Split out of `window_event`'s `RedrawRequested` arm (`clippy::too_many_lines`).
    /// Body unchanged from the original inline arm â€” see `window_event` for the
    /// event-loop plumbing this is called from. The layout pass, draw pass,
    /// legacy cursor drawing, and debug-outline drawing are further split
    /// into [`run_layout_pass`], [`draw_frame_content`], [`draw_legacy_cursors`]
    /// and [`draw_debug_outlines`] respectively; what remains is a single
    /// frame's render-thread handshake (acquire scene, draw, send, log) whose
    /// phases share `render`/`app`/`scene`/timing locals and several early
    /// `return`s â€” further fragmenting it risks changing frame-timing or
    /// error-path behaviour with no clippy/cargo loop available here to
    /// verify against.
    #[allow(clippy::too_many_lines, reason = "see doc comment above")]
    fn handle_redraw_requested(&mut self, event_loop: &ActiveEventLoop, window: &Arc<Window>) {
        let native_redraw_entry = self
            .native_input_origin
            .map(|origin| (origin, Instant::now()));
        // Capture only the initial frame attempts, not steady-state redraws.
        // An early return leaves its final marker absent rather than inventing
        // a completed stage. A later successful submission ends capture.
        let first_primary_frame = self.primary_frames_sent == 0;
        let mark_first_frame = |name| {
            if first_primary_frame {
                tilo_startup_metrics::mark(name);
            }
        };
        mark_first_frame("first-frame-redraw-start");
        let _entry = self.app.enter_wake_scope();
        if self.render.is_none() {
            tilo_startup_metrics::mark("first-redraw-init-render-start");
            if let Err(reason) = self.init_render(window) {
                // Retrying every redraw would only spin.
                crate::diag!("tilo: {reason}");
                event_loop.exit();
                return;
            }
            tilo_startup_metrics::mark("first-redraw-init-render-done");
        }
        let visible_sibling = self.visible_auxiliary_needs_shared_root();
        if visible_sibling {
            self.primary_retry.keep_shared_cadence(Instant::now());
        }
        let Some(render) = self.render.as_mut() else {
            return;
        };
        // A `Reconfigured` or `Skipped` previous frame re-requests a
        // redraw so the user doesn't see a stale image.
        let mut needs_followup_redraw = false;
        let mut surface_gone = false;
        while let Ok(outcome) = render.frame_complete.try_recv() {
            self.primary_frames_completed = self.primary_frames_completed.saturating_add(1);
            match outcome {
                FrameOutcome::Presented => {
                    self.last_primary_frame_presented = self.primary_frames_completed;
                    self.primary_retry.observe_for_shared_root(
                        outcome,
                        Instant::now(),
                        visible_sibling,
                    );
                }
                FrameOutcome::Reconfigured | FrameOutcome::Skipped | FrameOutcome::Occluded => {
                    self.primary_retry.observe_for_shared_root(
                        outcome,
                        Instant::now(),
                        visible_sibling,
                    );
                }
                FrameOutcome::SurfaceGone => {
                    surface_gone = true;
                }
            }
        }
        let cleared = completed_primary_frame_gates(
            &self.floating_source_clear_frames,
            self.last_primary_frame_presented,
        );
        for id in cleared {
            let sent =
                self.app
                    .floating_surface_client
                    .as_mut()
                    .is_some_and(|client| {
                        client
                    .send(&crate::floating_surface::FloatingSurfaceClientMessage::SourceCleared {
                        id: id.clone(),
                    })
                    .is_ok()
                    });
            if sent {
                self.floating_source_clear_frames.remove(&id);
            }
        }
        let destinations_presented = completed_primary_frame_gates(
            &self.floating_destination_present_frames,
            self.last_primary_frame_presented,
        );
        for id in destinations_presented {
            let sent = self
                .app
                .floating_surface_client
                .as_mut()
                .is_some_and(|client| {
                    client
                        .send(
                            &crate::floating_surface::FloatingSurfaceClientMessage::DestinationPresented {
                                id: id.clone(),
                            },
                        )
                        .is_ok()
                });
            if sent {
                self.floating_destination_present_frames.remove(&id);
            }
        }
        if surface_gone {
            tracing::info!("drawing surface is gone; closing the window");
            event_loop.exit();
            return;
        }
        // Do not consume sync/layout/dirty state unless a finite render buffer
        // is available. Worker completion wakes this window if admission defers.
        match crate::render_thread::prepare_frame(
            &mut self.primary_retry,
            &mut render.buffers,
            Instant::now(),
        ) {
            Ok(true) => {}
            Ok(false) => return,
            Err(_) => {
                tracing::error!("ui-render thread died; closing window");
                event_loop.exit();
                return;
            }
        }
        let app = &mut self.app;

        // Continue one bounded provider page per frame. Re-querying resets the
        // palette generation/cursors, so stale work cannot leak into a newer
        // query. A follow-up redraw is requested only while more pages exist.
        let (search_pending, search_announcement) =
            app.actions
                .action_palette
                .as_mut()
                .map_or((false, None), |palette| {
                    let progress = palette.advance_search_page();
                    (progress.pending, palette.take_search_announcement())
                });
        needs_followup_redraw |= search_pending;
        if let (Some(reader), Some(announcement)) = (
            app.accessibility.screen_reader.as_mut(),
            search_announcement,
        ) {
            reader.announce(announcement, tts::Priority::Notification, false);
        }

        let inner = window.inner_size();
        let width = inner.width;
        let height = inner.height;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "scale factor is intentionally narrowed to f32 for the render pipeline; identical to today's output"
        )]
        let scale_factor = window.scale_factor() as f32;
        app.layout.last_frame_size = Some(inner);
        let mut floating_content_changed =
            app.layout.layout_dirty || app.layout.structure_dirty || app.needs_sync;
        let mut full_redraw_required = floating_content_changed;

        // Records first: if the root widget opened a different
        // record, sync against that record's document, not the
        // outgoing one's.
        mark_first_frame("first-frame-record-start");
        let record_changed = app.record_tick();
        mark_first_frame("first-frame-record-done");
        floating_content_changed |= record_changed;
        full_redraw_required |= record_changed;

        let __prof_frame_start = app.profiler.as_ref().map(|_| std::time::Instant::now());
        // Drain model notifications and advance update-driven widgets before
        // synchronization. A worker's `Wake` event reaches this pass without
        // requiring the app to own a proxy or poll inside its model.
        mark_first_frame("first-frame-update-start");
        if app.root_widget.update(&mut app.state.doc, &app.state.root) {
            floating_content_changed = true;
            full_redraw_required = true;
            app.invalidate_widget_resource_cache();
            app.needs_sync = true;
            app.layout.layout_dirty = true;
        }
        mark_first_frame("first-frame-update-done");
        mark_first_frame("first-frame-sync-start");
        if app.needs_sync {
            floating_content_changed = true;
            full_redraw_required = true;
            app.root_widget.sync(&mut app.state.doc, &app.state.root);
            app.invalidate_widget_resource_cache();
            app.needs_sync = false;
        }
        if app.root_widget.take_window_close_intent() {
            event_loop.exit();
            return;
        }
        mark_first_frame("first-frame-sync-done");
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::Sync, __prof_frame_start);
        }

        mark_first_frame("first-frame-live-regions-start");
        app.scan_live_regions();
        mark_first_frame("first-frame-live-regions-done");

        // E.g. a tab split inserting a new WindowGroup: rebuild the
        // taffy tree from scratch so new widgets get node ids the
        // layout pass below can see.
        mark_first_frame("first-frame-build-layout-start");
        app.layout.structure_dirty |= app.root_widget.take_structure_dirty();
        // Captured BEFORE `rebuild_structure` clears it â€” a fresh
        // taffy tree always needs a full solve regardless of the
        // scroll fast path below.
        let structure_rebuilt = app.layout.structure_dirty;
        floating_content_changed |= structure_rebuilt;
        let __prof_build = app.profiler.as_ref().map(|_| std::time::Instant::now());
        if app.layout.structure_dirty {
            app.rebuild_structure();
        }
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::BuildLayout, __prof_build);
        }

        mark_first_frame("first-frame-build-layout-done");

        let perf = perf_enabled();
        let t_frame_start = if perf {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let Some(root_node) = app.layout.root_node else {
            tracing::error!("root_node unset during a redraw; skipping frame");
            return;
        };
        mark_first_frame("first-frame-layout-start");
        let Some((t_relayout_branch, t_compute_us, t_apply_us)) = run_layout_pass(
            app,
            root_node,
            width,
            height,
            scale_factor,
            structure_rebuilt,
            Some(&crate::widget::NativeViewHost::live(
                window,
                app.input.platform,
            )),
        ) else {
            return;
        };
        mark_first_frame("first-frame-layout-done");
        full_redraw_required |= structure_rebuilt || t_relayout_branch;
        // Read before `draw_frame_content` drains widget dirty flags via
        // `mark_drawn`. Clean compositor/exposure redraws do not invalidate
        // independent native floating surfaces.
        floating_content_changed |= app.root_widget.needs_redraw() || app.history_needs_redraw();

        // Admission reserved this pair before state updates. Taking it cannot
        // wait on GPU work; aborted layouts retain it in the handle for retry.
        let __prof_pool_wait = app.profiler.as_ref().map(|_| std::time::Instant::now());
        let Ok((mut scene, mut damage_regions)) = render.buffers.recv() else {
            tracing::error!("ui-render thread died; closing window");
            event_loop.exit();
            return;
        };
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::ScenePoolWait, __prof_pool_wait);
        }
        damage_regions.clear();
        full_redraw_required |= app.debug_mode
            || app.actions.action_palette.is_some()
            || app.input.tooltip_host.needs_frame()
            || app.input.tooltip_host.visible().is_some()
            || !app.presence_ctx.remote_cursors.is_empty()
            || app
                .profiler
                .as_ref()
                .is_some_and(super::profiling::FrameProfiler::hud_enabled);
        // The worker resets scenes before returning them to this pool; the
        // pre-seeded scenes are empty too.

        let t_draw_start = if perf {
            Some(std::time::Instant::now())
        } else {
            None
        };
        crate::webview::frame_trace::begin_scene();
        let (transform, window_rect, t_draw_us) =
            draw_frame_content(app, &mut scene, width, height, scale_factor, t_draw_start);

        let __prof_presence = app.profiler.as_ref().map(|_| std::time::Instant::now());
        draw_legacy_cursors(app, &mut scene, transform);

        // A pointer that arrived on a desktop scope is the
        // compositor's to draw, and the helper skips it â€” see
        // `presence::PointerOwner`. Only the *painting* has one
        // owner; the shell threads this same context into its own
        // overlay too.
        let interaction_now = std::time::Instant::now();
        app.presence_ctx.prune_interactions(interaction_now);
        crate::presence::draw_remote_presence(
            &mut scene,
            transform,
            window_rect,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
        // Controls and named rows already publish their real accessibility
        // geometry. Reuse that exact focus/selection outline for semantic
        // action presence; this decorates the target without changing local
        // focus, selection, active tab, or scroll position.
        let visible_interactions = app.presence_ctx.visible_interactions(interaction_now);
        if !visible_interactions.is_empty() {
            app.refresh_semantic_tree();
        }
        let native_targets = visible_interactions
            .iter()
            .filter_map(|interaction| {
                let exact = match &interaction.anchor {
                    tilo_actions::InteractionAnchor::SemanticControl { id } => app
                        .semantic_tree()
                        .node(&crate::a11y_tree::SemanticId(id.clone()))
                        .and_then(|node| node.bounds),
                    tilo_actions::InteractionAnchor::ListItems { .. }
                    | tilo_actions::InteractionAnchor::Resource => {
                        let mut matches = app
                            .semantic_tree()
                            .iter()
                            .filter(|node| {
                                node.name.as_deref() == Some(interaction.resource.label.as_str())
                            })
                            .filter_map(|node| node.bounds);
                        let only = matches.next();
                        only.filter(|_| matches.next().is_none())
                    }
                    _ => None,
                }?;
                Some((interaction.clone(), exact))
            })
            .collect::<Vec<_>>();
        for (interaction, rect) in native_targets {
            let user = crate::permissions::UserId(interaction.actor.id);
            let label = app.presence_ctx.display_name(&user);
            app.presence_ctx.draw_peer_outline_labeled(
                &mut scene,
                &app.theme,
                &mut app.text_renderer,
                transform,
                &user,
                &label,
                rect,
                f64::from(app.theme.tokens.radii.sm),
            );
        }
        // Action/resource callers have no synthetic pointer. Their truthful
        // semantic lifecycle is painted as an app-level activity rail until a
        // domain widget projects the stable anchor into native geometry.
        app.presence_ctx.draw_interactions(
            &mut scene,
            transform,
            window_rect,
            &mut app.text_renderer,
            &app.theme,
            interaction_now,
        );

        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::PresenceOverlay, __prof_presence);
        }

        let mut native_main_stages = (self.native_input_origin.is_some()
            && crate::render_thread::timing::enabled())
        .then(MainStages::default);
        record_main_stage(&mut native_main_stages, MainStage::BeforeProfilerCommit);

        // Folds in any `handle_event` time banked since last frame.
        if let (Some(p), Some(start)) = (app.profiler.as_mut(), __prof_frame_start) {
            p.commit_frame(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX));
            record_main_stage(&mut native_main_stages, MainStage::AfterProfilerCommit);
            if !self.native_input_ids.is_empty() {
                let cpu_frame_id = p.history().back().map(|sample| sample.frame);
                crate::native_scenario::log::emit_json(
                    "qos-native-frame",
                    serde_json::json!({
                        "cpu_frame_id": cpu_frame_id,
                        "input_ids": std::mem::take(&mut self.native_input_ids),
                        "frame_commit_us": self.native_input_origin.map(|origin| origin.elapsed().as_micros()),
                        // Capture after drawing: layout may clamp scroll state.
                        // Input-time effects alone cannot prove the frame used it.
                        "state": app.native_input_snapshot(),
                    }),
                );
            }
        } else {
            record_main_stage(&mut native_main_stages, MainStage::AfterProfilerCommit);
        }
        crate::webview::frame_trace::finish_scene(
            app.profiler
                .as_ref()
                .and_then(|p| p.history().back())
                .map(|sample| sample.frame),
        );
        record_main_stage(&mut native_main_stages, MainStage::AfterNativeFrameLog);

        if app.debug_mode {
            draw_debug_outlines(app, &mut scene, transform);
        }
        record_main_stage(&mut native_main_stages, MainStage::AfterDebugOutlines);

        // Animated widgets update their visual bounds while drawing (for
        // example, an indeterminate progress segment records its old and new
        // positions there). Capture damage only after that work, but before
        // `mark_drawn` clears regular dirty flags. This makes the submitted
        // scene and its damage describe the same frame rather than trailing
        // animation by one frame.
        if !full_redraw_required {
            collect_submitted_widget_damage(app.root_widget.as_ref(), &mut damage_regions);
            let physical_viewport = KurboRect::new(0.0, 0.0, f64::from(width), f64::from(height));
            let scale = f64::from(scale_factor);
            for rect in &mut damage_regions {
                *rect = KurboRect::new(
                    rect.x0 * scale,
                    rect.y0 * scale,
                    rect.x1 * scale,
                    rect.y1 * scale,
                )
                .inflate(16.0 * scale, 16.0 * scale)
                .intersect(physical_viewport);
            }
            damage_regions.retain(|rect| !rect.is_zero_area());
        }
        // This must follow damage collection: widget `draw` may update
        // damage geometry and `mark_drawn` consumes persistent dirty flags.
        app.root_widget.mark_drawn();
        record_main_stage(&mut native_main_stages, MainStage::AfterDamageMarkDrawn);

        // `update_if_active` only builds + sends when an AT is
        // attached, so this is free on the common no-reader path.
        if let Some(adapter) = self.a11y_adapter.as_mut() {
            let app_ref: &mut App = app;
            let scale = f64::from(scale_factor);
            adapter.update_if_active(|| {
                crate::accesskit_bridge::native_tree_update(app_ref.build_a11y_tree_update(scale))
            });
        }
        record_main_stage(&mut native_main_stages, MainStage::AfterAccessibility);

        // Painted last, after the frame timing was committed above,
        // so the HUD's own cost never enters its numbers.
        if app
            .profiler
            .as_ref()
            .is_some_and(super::profiling::FrameProfiler::hud_enabled)
        {
            let (hud_w, hud_h) =
                crate::scale::logical_size((width, height), f64::from(scale_factor));
            if let Some(p) = app.profiler.as_ref() {
                p.draw_hud(
                    &mut scene,
                    transform,
                    &mut app.text_renderer,
                    &app.theme,
                    (hud_w, hud_h),
                );
            }
        }
        let req = RenderRequest::Frame {
            cpu_frame_id: app
                .profiler
                .as_ref()
                .and_then(|profiler| profiler.history().back())
                .map(|sample| sample.frame),
            scene,
            width,
            height,
            base_color: app.theme.bg_app,
            damage_regions,
            full_redraw: full_redraw_required,
        };
        record_main_stage(&mut native_main_stages, MainStage::AfterHudRequest);
        if let Some((origin, entered)) = native_redraw_entry {
            // All three values use the input timeline's monotonic origin.
            // This is a CPU submit attempt, not GPU/display completion.
            crate::native_scenario::log::emit_json(
                "qos-native-redraw",
                serde_json::json!({
                    "cpu_frame_id": app.profiler.as_ref().and_then(|p| p.history().back()).map(|s| s.frame),
                    "redraw_enter_us": entered.duration_since(origin).as_micros(),
                    "timed_build_start_us": __prof_frame_start.map(|start| start.duration_since(origin).as_micros()),
                    "submit_attempt_us": origin.elapsed().as_micros(),
                }),
            );
        }
        record_main_stage(&mut native_main_stages, MainStage::AfterNativeRedrawLog);
        let send_cpu_frame_id = match &req {
            RenderRequest::Frame { cpu_frame_id, .. } => *cpu_frame_id,
            RenderRequest::Shutdown => None,
        };
        let (send_result, send_bounds) = crate::render_thread::timing::around(
            self.native_input_origin.is_some() && crate::render_thread::timing::enabled(),
            || render.render_tx.send(req),
        );
        if let Some(bounds) = send_bounds {
            crate::native_scenario::log::emit_json(
                "qos-native-send",
                serde_json::json!({
                    "cpu_frame_id": send_cpu_frame_id,
                    "host_clock": crate::native_scenario::host_clock::CLOCK_NAME,
                    "before_host_lo_s": bounds.before.map(|b| b.lo_s),
                    "before_host_hi_s": bounds.before.map(|b| b.hi_s),
                    "after_host_lo_s": bounds.after.map(|b| b.lo_s),
                    "after_host_hi_s": bounds.after.map(|b| b.hi_s),
                    "send_ok": send_result.is_ok(),
                }),
            );
        }
        if let Some(stages) = native_main_stages {
            crate::native_scenario::log::emit_json(
                "qos-native-main-stages",
                stages.row(send_cpu_frame_id),
            );
        }
        if send_result.is_err() {
            tracing::error!("ui-render thread receiver closed; exiting");
            event_loop.exit();
            return;
        }
        mark_first_frame("first-frame-submitted");
        self.primary_frames_sent = self.primary_frames_sent.saturating_add(1);
        let submitted = self.primary_frames_sent;
        for required in self.floating_source_clear_frames.values_mut() {
            if required.is_none() {
                *required = Some(submitted);
            }
        }
        for required in self.floating_destination_present_frames.values_mut() {
            if required.is_none() {
                *required = Some(submitted);
            }
        }

        let t_send_us = t_frame_start.map_or(0, |t| t.elapsed().as_micros());
        if let Some(_start) = t_frame_start {
            // On a static frame this reads `shape=0us hit=N miss=0`.
            let (shape_hits, shape_misses, shape_ns) = app.text_renderer.take_shape_stats();
            crate::diag!(
                "perf frame: main_total={:>5}us layout={}({}+{}us) draw={:>5}us (shape={:>4}us hit={} miss={}) send_to_worker={:>5}us",
                t_send_us,
                if t_relayout_branch { "Y" } else { "n" },
                t_compute_us,
                t_apply_us,
                t_draw_us,
                shape_ns.checked_div(1000).unwrap_or(0),
                shape_hits,
                shape_misses,
                t_send_us,
            );
        }

        if needs_followup_redraw {
            window.request_redraw();
        }
        // The secondary surfaces paint the same app-owned state. A primary
        // frame therefore invalidates each of them exactly once; their own
        // presentation never recursively requests another primary frame.
        if floating_content_changed {
            for surface in &self.floating_windows {
                surface.window.request_redraw();
            }
        }
        if let Some(audience) = self.audience_presentation.as_ref() {
            audience.window.request_redraw();
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "input routing shares mutable state and must retain fixed precedence"
    )]
    fn handle_other_window_event(&mut self, window: &Arc<Window>, other: WindowEvent) {
        let _entry = self.app.enter_wake_scope();
        let event = other;
        if self.app.close_handle_event(&event, window.scale_factor()) {
            window.request_redraw();
            return;
        }
        let app = &mut self.app;
        let history_input = app.history_controls_event(&event, window.scale_factor());
        let _history_frame = crate::history_mode::frame::FrameworkInputFrame::new(
            &app.framework_input,
            history_input,
        );
        let detected_interaction = if self.dispatching_touch_pointer {
            None
        } else {
            match &event {
                WindowEvent::Touch(_) => Some(app.input.interaction.with_touch_modality()),
                WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. } => {
                    Some(app.input.interaction.with_pointer_modality())
                }
                WindowEvent::KeyboardInput {
                    event:
                        winit::event::KeyEvent {
                            state: ElementState::Pressed,
                            ..
                        },
                    ..
                } => Some(app.input.interaction.with_keyboard_modality()),
                _ => None,
            }
        };
        if let Some(environment) = detected_interaction {
            app.set_interaction_environment(environment);
        }
        if history_input {
            app.dispatch_framework_history_event(&event, window.scale_factor());
            window.request_redraw();
            return;
        }
        let __prof_input = app.profiler.as_ref().map(|_| std::time::Instant::now());
        // RUST_LOG=tilo_ui::app=debug: pointer-move spam is intentionally
        // not throttled here â€” when the bug is "no clicks work", you
        // want to see whether moves arrive at all.
        match &event {
            WindowEvent::MouseInput { state, button, .. } => {
                tracing::debug!(?state, ?button, "App::window_event MouseInput");
            }
            WindowEvent::CursorMoved { position, .. } => {
                tracing::trace!(
                    x = position.x,
                    y = position.y,
                    "App::window_event CursorMoved"
                );
            }
            WindowEvent::KeyboardInput { event: ke, .. } => {
                tracing::debug!(key = ?ke.logical_key, state = ?ke.state, "App::window_event KeyboardInput");
            }
            _ => {}
        }
        // `CursorMoved` is measured in this window's own content,
        // never the desktop — see the `pointer_space` field.
        if app.presence.broadcast_scope.is_some() {
            match &event {
                WindowEvent::CursorMoved { position, .. } => {
                    let now = Instant::now();
                    if pointer_broadcast_allowed(
                        app.presence.last_pointer_broadcast,
                        now,
                        Duration::from_millis(POINTER_BROADCAST_MIN_GAP_MS),
                    ) {
                        app.presence.last_pointer_broadcast = Some(now);
                        let where_it_is = app.local_pointer_key();
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "presence coordinates are intentionally narrowed to f32 on the wire; identical to today's output"
                        )]
                        app.broadcast_presence(
                            where_it_is,
                            PresencePayload::Pointer {
                                x: position.x as f32,
                                y: position.y as f32,
                            },
                        );
                    }
                }
                WindowEvent::CursorLeft { .. } => {
                    let where_it_was = app.local_pointer_key();
                    app.broadcast_presence(where_it_was, PresencePayload::Left);
                    app.presence.last_pointer_broadcast = None;
                }
                _ => {}
            }
        }

        let mut handled = false;
        let mut history_handled = matches!(event, WindowEvent::KeyboardInput { .. })
            && app
                .focused_node
                .is_some_and(|node| app.is_history_node(node));
        let mut focus_target = None;
        // A synthesized hover-clear may be handled by a control, but it is
        // presentation state and must not create an empty undo boundary.
        let mut is_undo_redo =
            self.dispatching_touch_pointer && matches!(event, WindowEvent::CursorMoved { .. });
        let mut floating_pointer_needs_redraw = false;

        // The palette owns the keyboard while open; Ctrl+K itself is
        // handled below, so let it through.
        if app.actions.action_palette.is_some() {
            let is_ctrl_k = matches!(
                &event,
                WindowEvent::KeyboardInput {
                    event: winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Character(c),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                    ..
                } if (c.as_str() == "k" || c.as_str() == "K")
                    && app
                        .input
                        .platform
                        .command_modifier_down(app.input.modifiers.state())
            );
            if !is_ctrl_k
                && matches!(event, WindowEvent::KeyboardInput { .. })
                && app.action_palette_handle_key(&event)
            {
                handled = true;
            }
        }

        // Runs FIRST so it wins over Tab/arrow handling.
        if app.handle_screen_reader_key(&event) {
            handled = true;
            is_undo_redo = true; // suppress an undo commit for a read cmd
        }

        let state_pre = if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
        ) {
            app.focused_state_snapshot()
        } else {
            None
        };

        let review_pre = if matches!(event, WindowEvent::KeyboardInput { .. }) {
            app.text_review_snapshot()
        } else {
            None
        };

        // Governs pointer click-through only; keyboard focus
        // containment is the separate `traps_focus()` concern (see
        // `keyboard_nav_allowed`).
        let modal_rect = app.root_widget.modal_capture();

        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::F12),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            app.debug_mode = !app.debug_mode;
            handled = true;
        }

        if !handled
            && app
                .input
                .platform
                .command_modifier_down(app.input.modifiers.state())
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Character(ref c),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let shift = app.input.modifiers.state().shift_key();
            let history_request = crate::undo::shortcut(c, true, shift);
            if let Some(request) = history_request {
                if app.root_widget.handle_local_undo_request(request) {
                    app.needs_sync = true;
                    window.request_redraw();
                } else {
                    // Otherwise round-tripping document undo/redo loses caret
                    // position.
                    let current_meta = app.root_widget.get_state();
                    let current_focus_idx = app
                        .focused_node
                        .and_then(|id| app.focusable_order.iter().position(|x| *x == id));
                    let binding = app
                        .focused_node
                        .and_then(|node| app.bindings.binding_for(node))
                        .or_else(|| app.root_widget.callback_binding());
                    let owned_target = binding.is_some()
                        || app
                            .focused_node
                            .is_some_and(|node| app.bindings.owns_document(node));
                    let entry = if let Some(binding) = binding {
                        binding
                            .history_request(&mut app.state.doc, request)
                            .ok()
                            .flatten()
                    } else if let Some(owner) = app
                        .focused_node
                        .and_then(|node| app.bindings.legacy_for(node))
                    {
                        crate::bound_dispatch::legacy_history_request(
                            &owner,
                            request,
                            &app.permission_ctx,
                            &app.collaboration.local_author,
                        )
                        .ok()
                        .flatten()
                    } else {
                        match request {
                            crate::undo::UndoRequest::Undo => app.state.undo_as(
                                Some(&app.collaboration.local_author),
                                current_focus_idx,
                                current_meta,
                            ),
                            crate::undo::UndoRequest::Redo => app.state.redo_as(
                                Some(&app.collaboration.local_author),
                                current_focus_idx,
                                current_meta,
                            ),
                        }
                    };
                    if let Some(entry) = entry {
                        if owned_target {
                            app.needs_sync = true;
                            app.layout.layout_dirty = true;
                        } else {
                            app.apply_undo_entry(entry);
                        }
                    }
                }
                is_undo_redo = true;
                handled = true;
            } else {
                match c.as_str() {
                    // A second Ctrl+K closes the palette.
                    "k" | "K" => {
                        if app.actions.action_palette.is_some() {
                            app.close_action_palette();
                        } else {
                            app.open_action_palette();
                        }
                        handled = true;
                    }
                    _ => {}
                }
            }
        }

        if !handled && app.handle_tab_navigation(&event, modal_rect) {
            handled = true;
        }

        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(logical_key),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let dir = match logical_key {
                winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up),
                winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left),
                winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                _ => None,
            };
            if let (Some(d), true, true) = (
                dir,
                app.input.modifiers.state().alt_key(),
                app.keyboard_nav_allowed(modal_rect),
            ) {
                app.navigate_focus(d);
                handled = true;
            }
        }

        // Two-finger swipe / edge pull; delivered via `on_nav_gesture`.
        if !handled && !self.dispatching_touch_pointer {
            use crate::gesture::GesturePhase;
            let map_phase = |p: winit::event::TouchPhase| match p {
                winit::event::TouchPhase::Started => GesturePhase::Begin,
                winit::event::TouchPhase::Moved => GesturePhase::Update,
                winit::event::TouchPhase::Ended => GesturePhase::End,
                winit::event::TouchPhase::Cancelled => GesturePhase::Cancel,
            };
            let nav = match &event {
                WindowEvent::MouseWheel { delta, phase, .. } => {
                    let (dx, dy, precise) = match delta {
                        winit::event::MouseScrollDelta::PixelDelta(p) => (p.x, p.y, true),
                        winit::event::MouseScrollDelta::LineDelta(x, y) => {
                            (f64::from(*x), f64::from(*y), false)
                        }
                    };
                    app.input
                        .nav_gestures
                        .on_scroll(dx, dy, map_phase(*phase), precise)
                }
                WindowEvent::PanGesture { delta, phase, .. } => app.input.nav_gestures.on_scroll(
                    f64::from(delta.x),
                    f64::from(delta.y),
                    map_phase(*phase),
                    true,
                ),
                WindowEvent::Touch(t) => {
                    let width = f64::from(window.inner_size().width);
                    app.input.nav_gestures.on_touch(
                        t.id,
                        t.location.x,
                        t.location.y,
                        map_phase(t.phase),
                        width,
                    )
                }
                _ => None,
            };
            if let Some(dir) = nav {
                let consumed =
                    app.root_widget
                        .on_nav_gesture(dir, &mut app.state.doc, &app.state.root);
                if consumed {
                    app.needs_sync = true;
                    window.request_redraw();
                    handled = true;
                }
            }
        }

        if !handled && app.layout.root_node.is_some() {
            app.refresh_control_action_availability();
            let scale_factor = window.scale_factor();
            let presentation_was_floating =
                !app.root_widget.floating_surface_presentations().is_empty();
            let floating_pointer_event = app.primary_floating_pointer_event(&event, scale_factor);
            let used_desktop_pointer_space = floating_pointer_event.is_some();
            let event_for_root = floating_pointer_event.as_ref().unwrap_or(&event);
            let was_mouse_click = matches!(
                event,
                WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
            );
            history_handled |= app.presentation_handle_event(
                event_for_root,
                scale_factor,
                &mut handled,
                &mut focus_target,
            );
            // Even a document-free interaction may switch which resources a
            // widget presents. Invalidate once at the mutable tree boundary;
            // all later consumers in this event/frame share the rebuilt cache.
            app.invalidate_widget_resource_cache();
            let presentation_is_floating =
                !app.root_widget.floating_surface_presentations().is_empty();
            floating_pointer_needs_redraw = matches!(event, WindowEvent::CursorMoved { .. })
                && (used_desktop_pointer_space
                    || presentation_was_floating != presentation_is_floating);
            if was_mouse_click {
                tracing::debug!(
                    handled,
                    needs_redraw = app.root_widget.needs_redraw(),
                    is_layout_dirty = app.root_widget.is_layout_dirty(),
                    "root_widget.handle_event(MouseInput Pressed) returned"
                );
            }
            // A press the modal didn't consume (the dimmed backdrop)
            // is still marked handled so it can't reach widgets
            // behind the scrim.
            if modal_rect.is_some()
                && matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: winit::event::ElementState::Pressed,
                        ..
                    } | WindowEvent::MouseInput {
                        state: winit::event::ElementState::Released,
                        ..
                    }
                )
            {
                handled = true;
            }
        }

        // If no widget consumed Enter/Space (text inputs do), activate
        // the focused control the same way a mouse click would.
        app.handle_unconsumed_activation_key(&event, &mut handled);

        // A focused range control consumes Left/Right; Up/Down and Tab
        // still move focus. Modifiers scale the step:
        //   plain  â†’ one step       Shift â†’ four steps (coarse)
        //   Ctrl   â†’ jump to min/max (Left = min, Right = max)
        // Other widgets ignore the action, falling through to
        // directional navigation below.
        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(named),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            use crate::a11y::Action;
            let ctrl = app
                .input
                .platform
                .command_modifier_down(app.input.modifiers.state());
            let shift = app.input.modifiers.state().shift_key();
            // (action to repeat, how many times)
            let (action, times) = match named {
                winit::keyboard::NamedKey::ArrowLeft if ctrl => {
                    (Some(Action::SetValue("0".into())), 1)
                }
                winit::keyboard::NamedKey::ArrowRight if ctrl => {
                    (Some(Action::SetValue("100".into())), 1)
                }
                winit::keyboard::NamedKey::ArrowLeft => {
                    (Some(Action::Decrement), if shift { 4 } else { 1 })
                }
                winit::keyboard::NamedKey::ArrowRight => {
                    (Some(Action::Increment), if shift { 4 } else { 1 })
                }
                _ => (None, 0),
            };
            if let (Some(act), Some(fid)) = (action, app.focused_node) {
                let mut any = false;
                for _ in 0..times {
                    any |= app
                        .dispatch_node(fid, |widget, doc, root| {
                            widget.perform_action(&act, doc, root)
                        })
                        .unwrap_or(false);
                }
                if any {
                    handled = true;
                    app.needs_sync = true;
                    app.layout.layout_dirty = true;
                }
            }
        }

        if !handled
            && app.keyboard_nav_allowed(modal_rect)
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(logical_key),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let dir = match logical_key {
                winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up),
                winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left),
                winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                _ => None,
            };
            if let Some(d) = dir {
                app.navigate_focus(d);
                handled = true;
            }
        }

        if app.dispatch_control_invocations() != 0 {
            handled = true;
            app.needs_sync = true;
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        if let Some(address) = app.root_widget.take_action_palette_request()
            && app.open_action_palette_at(&address)
        {
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        app.broadcast_focus_presence();
        app.broadcast_location_presence();
        app.broadcast_view_presence();
        app.publish_action_surface();
        app.publish_continuation_metadata();
        app.publish_playback_handoff();

        if let Some(id) = focus_target {
            app.set_focused_node(Some(id));
            handled = true;
        }

        app.announce_state_change(state_pre);
        app.announce_text_review(review_pre);

        if handled {
            app.finalize_handled_native_event(&event, is_undo_redo, history_handled);
            app.needs_sync = true;
            // Gating every handled event on `needs_redraw()` is
            // wrong: container widgets can mutate their own
            // active-index state without setting a descendant leaf's
            // `dirty` flag, so recursive `needs_redraw` misses it.
            // Only pointer MOVES (the hot 60+ Hz stream) skip a
            // redraw when nothing's dirty.
            let is_pointer_move = matches!(&event, WindowEvent::CursorMoved { .. });
            if !is_pointer_move || app.root_widget.is_layout_dirty() {
                app.layout.layout_dirty = true;
            }
            if !is_pointer_move
                || floating_pointer_needs_redraw
                || app.root_widget.needs_redraw()
                || app.history_needs_redraw()
            {
                window.request_redraw();
            }
        }

        // Resolved on every `CursorMoved`, not just handled ones, so
        // the shape stays correct crossing into a widget that
        // doesn't flip its own hover.
        if let WindowEvent::CursorMoved { position, .. } = &event {
            let scale = window.scale_factor();
            let logical = Point::new(position.x / scale, position.y / scale);
            let desired = app.resolve_cursor(logical).unwrap_or(CursorIcon::Default);
            if app.input.current_cursor != desired {
                app.input.current_cursor = desired;
                window.set_cursor(desired);
            }
            // `on_pointer` keeps the dwell running over the same
            // region and restarts it on a new one.
            let target = app.tooltip_target_at(logical);
            if app.input.tooltip_host.on_pointer(target, Instant::now()) {
                window.request_redraw();
            }
        }

        if matches!(
            &event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::CursorLeft { .. }
        ) && app.input.tooltip_host.dismiss()
        {
            window.request_redraw();
        }

        if let (Some(start), Some(p)) = (__prof_input, app.profiler.as_mut()) {
            p.record(crate::profiling::Pass::HandleEvent, start.elapsed());
        }
    }
}

impl AppHandler {
    /// `UserEvent` injection keeps the ordinary window dispatch after admission.
    fn injected_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        self.route_window_event(
            event_loop,
            window_id,
            event,
            native_input_isolation::EventOrigin::Injected,
        );
    }

    fn route_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
        origin: native_input_isolation::EventOrigin,
    ) {
        native_input_isolation::dispatch(
            self.native_input_route_isolation,
            origin,
            event,
            |event| {
                self.handle_routed_window_event(event_loop, window_id, event);
            },
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "winit requires the complete per-window event dispatch at this callback boundary"
    )]
    fn handle_routed_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let _entry = self.app.enter_wake_scope();
        if render_probe_event(&event) {
            let auxiliary_event = self
                .floating_windows
                .iter()
                .any(|surface| surface.window.id() == window_id)
                || self
                    .audience_presentation
                    .as_ref()
                    .is_some_and(|audience| audience.window.id() == window_id);
            if auxiliary_event
                && self.primary_retry.request_probe()
                && let Some(primary) = &self.window
            {
                primary.request_redraw();
            }

            if self
                .window
                .as_ref()
                .is_some_and(|window| window.id() == window_id)
                && self.primary_retry.request_probe()
                && let Some(window) = &self.window
            {
                window.request_redraw();
            }
            if let Some(surface) = self
                .floating_windows
                .iter_mut()
                .find(|surface| surface.window.id() == window_id)
                && surface
                    .render
                    .as_mut()
                    .is_some_and(|render| render.retry.request_probe())
            {
                surface.window.request_redraw();
            }
            if let Some(audience) = self
                .audience_presentation
                .as_mut()
                .filter(|audience| audience.window.id() == window_id)
                && audience
                    .render
                    .as_mut()
                    .is_some_and(|render| render.retry.request_probe())
            {
                audience.window.request_redraw();
            }
        }

        if self
            .audience_presentation
            .as_ref()
            .is_some_and(|audience| audience.window.id() == window_id)
        {
            let audience_window = self
                .audience_presentation
                .as_ref()
                .map(|audience| audience.window.clone());
            let Some(audience_window) = audience_window else {
                return;
            };
            match event {
                WindowEvent::CloseRequested => {
                    // Closing the audience is an explicit stop, unlike a
                    // temporary hotplug reconciliation which retains the
                    // presentation and falls back to the primary output.
                    self.app.root_widget.on_audience_presentation_closed();
                    self.close_audience_presentation();
                    if let Some(primary) = self.window.as_ref() {
                        primary.request_redraw();
                    }
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    audience_window.request_redraw();
                }
                WindowEvent::RedrawRequested => {
                    if !self.handle_audience_redraw() {
                        self.close_audience_presentation();
                        if let Some(primary) = self.window.as_ref() {
                            primary.request_redraw();
                        }
                    }
                }
                WindowEvent::ModifiersChanged(modifiers) => self.app.input.modifiers = modifiers,
                other => {
                    let handled = self.app.root_widget.handle_audience_presentation_event(
                        &other,
                        &mut self.app.state.doc,
                        &self.app.state.root,
                    );
                    if handled {
                        self.app.commit_action();
                        self.app.needs_sync = true;
                        self.app.layout.layout_dirty = true;
                        if let Some(primary) = self.window.as_ref() {
                            primary.request_redraw();
                        }
                        audience_window.request_redraw();
                    }
                }
            }
            return;
        }
        if let Some(index) = self
            .floating_windows
            .iter()
            .position(|surface| surface.window.id() == window_id)
        {
            let Some(surface) = self.floating_windows.get(index) else {
                return;
            };
            let window = surface.window.clone();
            let desktop_rect = surface.desktop_rect;
            let surface_id = surface.id.clone();
            match event {
                WindowEvent::CloseRequested => {
                    if let Some(client) = self.app.floating_surface_client.as_mut() {
                        let _ = client.send(
                            &crate::floating_surface::FloatingSurfaceClientMessage::Dismiss {
                                id: surface_id.clone(),
                            },
                        );
                    }
                    self.close_native_floating_window(&surface_id);
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    window.request_redraw();
                }
                WindowEvent::RedrawRequested => {
                    if !self.handle_floating_redraw(index) {
                        let id = self
                            .floating_windows
                            .get(index)
                            .map(|surface| surface.id.clone());
                        if let Some(id) = id {
                            self.close_native_floating_window(&id);
                        }
                    }
                }
                WindowEvent::ModifiersChanged(modifiers) => self.app.input.modifiers = modifiers,
                other => self.dispatch_floating_event(&window, desktop_rect, other),
            }
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        if window.id() != window_id {
            return;
        }
        // No-op when no AT is attached.
        if let Some(adapter) = self.a11y_adapter.as_mut() {
            adapter.process_event(&window, &event);
        }
        if let WindowEvent::Touch(touch) = &event {
            self.app
                .set_interaction_environment(self.app.input.interaction.with_touch_modality());
            let pointer_events = self.touch_pointer.events(touch);
            self.dispatching_touch_pointer = true;
            for pointer_event in pointer_events {
                let pointer_event = match pointer_event {
                    crate::interaction::TouchPointerEvent::Move(position) => {
                        WindowEvent::CursorMoved {
                            device_id: touch.device_id,
                            position,
                        }
                    }
                    crate::interaction::TouchPointerEvent::Press => WindowEvent::MouseInput {
                        device_id: touch.device_id,
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                    },
                    crate::interaction::TouchPointerEvent::Release => WindowEvent::MouseInput {
                        device_id: touch.device_id,
                        state: ElementState::Released,
                        button: MouseButton::Left,
                    },
                    crate::interaction::TouchPointerEvent::Scroll { delta, phase } => {
                        WindowEvent::MouseWheel {
                            device_id: touch.device_id,
                            delta: winit::event::MouseScrollDelta::PixelDelta(delta),
                            phase,
                        }
                    }
                };
                self.handle_other_window_event(&window, pointer_event);
            }
            self.dispatching_touch_pointer = false;
        }
        match event {
            WindowEvent::CloseRequested => {
                if self.app.request_close() && self.app.finish_close_if_ready() {
                    event_loop.exit();
                } else {
                    window.request_redraw();
                }
            }
            WindowEvent::Resized(_size) => {
                // Handled lazily on the worker via the next `RenderRequest`'s
                // dimensions; main only needs to drive a redraw.
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                // The render path reads `window.scale_factor()` each frame,
                // so we just need to invalidate layout and redraw.
                crate::widget::mount_native_views(
                    self.app.root_widget.as_mut(),
                    &crate::widget::NativeViewHost::live(&window, self.app.input.platform),
                );
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => self.handle_redraw_requested(event_loop, &window),
            WindowEvent::ModifiersChanged(modifiers) => {
                self.app.input.modifiers = modifiers;
            }
            other => self.handle_other_window_event(&window, other),
        }
    }
}

impl ApplicationHandler<UserEvent> for AppHandler {
    // SAFETY: the zeroed-DeviceId transmutes below are safe because winit's
    // DeviceId is opaque and the dispatch chain never inspects it (same
    // trick `shell::compositor::native` uses headless).
    #[allow(
        clippy::too_many_lines,
        reason = "winit requires the complete synthetic event dispatch at this callback boundary"
    )]
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let _entry = self.app.enter_wake_scope();
        let event = match event {
            UserEvent::RenderReady(id) => {
                if let Some(window) = self.window.as_ref().filter(|window| window.id() == id) {
                    window.request_redraw();
                } else if let Some(surface) = self
                    .floating_windows
                    .iter()
                    .find(|surface| surface.window.id() == id)
                {
                    surface.window.request_redraw();
                } else if let Some(audience) = self
                    .audience_presentation
                    .as_ref()
                    .filter(|audience| audience.window.id() == id)
                {
                    audience.window.request_redraw();
                }
                return;
            }
            UserEvent::Wake(wake) => {
                if self.app.accept_wake(&wake)
                    && wake.requires_sync()
                    && let Some(window) = self.window.as_ref()
                {
                    window.request_redraw();
                }
                return;
            }
            other => other,
        };
        let Some(window) = self.window.clone() else {
            return;
        };
        let window_id = window.id();
        let mut effect_before = None;
        let event = match event {
            UserEvent::NativeScenarioInput(packet) => {
                if self.native_input_origin.is_none() {
                    let size = window.inner_size();
                    crate::native_scenario::log::emit_json(
                        "qos-native-window",
                        serde_json::json!({
                            "scale_factor": window.scale_factor(),
                            "physical_width": size.width,
                            "physical_height": size.height,
                            "pointer_passthrough": self.native_input_pointer_passthrough,
                            "input_route_isolation": self.native_input_route_isolation,
                        }),
                    );
                }
                self.native_input_origin.get_or_insert(packet.origin);
                let (now, dispatch_host) = crate::native_scenario::host_clock::sample();
                crate::native_scenario::log::emit_json(
                    "qos-native-input",
                    serde_json::json!({
                        "id": packet.id,
                        "warmup": packet.warmup,
                        "input": packet.input,
                        "scheduled_us": packet.scheduled_at.duration_since(packet.origin).as_micros(),
                        "sent_us": packet.sent_at.duration_since(packet.origin).as_micros(),
                        "dispatch_start_us": now.duration_since(packet.origin).as_micros(),
                        "host_clock": crate::native_scenario::host_clock::CLOCK_NAME,
                        "sent_host_lo_s": packet.sent_host.map(|bounds| bounds.lo_s),
                        "sent_host_hi_s": packet.sent_host.map(|bounds| bounds.hi_s),
                        "dispatch_host_lo_s": dispatch_host.map(|bounds| bounds.lo_s),
                        "dispatch_host_hi_s": dispatch_host.map(|bounds| bounds.hi_s),
                        "lateness_us": now.saturating_duration_since(packet.scheduled_at).as_micros(),
                    }),
                );
                effect_before = self
                    .app
                    .native_input_snapshot()
                    .map(|value| (packet.id, value));
                self.native_input_ids.push(packet.id);
                match packet.input {
                    crate::native_scenario::Input::Hover { x, y } => UserEvent::Hover { x, y },
                    crate::native_scenario::Input::Click { x, y } => UserEvent::Click { x, y },
                    crate::native_scenario::Input::Wheel { dx, dy } => UserEvent::Wheel { dx, dy },
                    crate::native_scenario::Input::Character { value } => {
                        UserEvent::Key(winit::keyboard::Key::Character(value.to_string().into()))
                    }
                    crate::native_scenario::Input::Key { key } => UserEvent::Key(key.into()),
                }
            }
            other => other,
        };
        match event {
            UserEvent::Hover { x, y } => {
                let ev = WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: PhysicalPosition::new(x, y),
                };
                self.injected_window_event(event_loop, window_id, ev);
            }
            UserEvent::Click { x, y } => {
                let moved = WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: PhysicalPosition::new(x, y),
                };
                self.injected_window_event(event_loop, window_id, moved);
                let press = WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                };
                self.injected_window_event(event_loop, window_id, press);
                let release = WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Released,
                    button: MouseButton::Left,
                };
                self.injected_window_event(event_loop, window_id, release);
            }
            UserEvent::Key(key) => {
                for state in [ElementState::Pressed, ElementState::Released] {
                    self.injected_window_event(
                        event_loop,
                        window_id,
                        crate::synthetic_input::key_event(&key, state),
                    );
                }
            }
            UserEvent::Wheel { dx, dy } => {
                let ev = WindowEvent::MouseWheel {
                    device_id: unsafe { std::mem::zeroed() },
                    delta: winit::event::MouseScrollDelta::LineDelta(dx, dy),
                    phase: winit::event::TouchPhase::Moved,
                };
                self.injected_window_event(event_loop, window_id, ev);
            }
            UserEvent::External(payload) => {
                let app = &mut self.app;
                app.root_widget
                    .on_external_event(&payload, &mut app.state.doc, &app.state.root);
                app.needs_sync = true;
                window.request_redraw();
            }
            UserEvent::Wake(_) | UserEvent::NativeScenarioInput(_) | UserEvent::RenderReady(_) => {}
            UserEvent::AccessKit(ak_event) => {
                use accesskit_winit::WindowEvent as AkWindowEvent;
                match ak_event {
                    AkWindowEvent::InitialTreeRequested => {
                        if let Some(adapter) = self.a11y_adapter.as_mut() {
                            let scale = window.scale_factor();
                            let app_ref: &mut App = &mut self.app;
                            adapter.update_if_active(|| {
                                crate::accesskit_bridge::native_tree_update(
                                    app_ref.build_a11y_tree_update(scale),
                                )
                            });
                        }
                        window.request_redraw();
                    }
                    AkWindowEvent::ActionRequested(req) => {
                        // `Some((x, y))` means "synthesise a click here".
                        if let Some(action) = crate::accesskit_bridge::accesskit_action_to_ours(
                            req.action,
                            req.data.as_ref(),
                        ) {
                            let click = self.app.dispatch_a11y_action(req.target_node, &action);
                            if let Some((x, y)) = click {
                                let scale = window.scale_factor();
                                let (px, py) = (x * scale, y * scale);
                                let moved = WindowEvent::CursorMoved {
                                    device_id: unsafe { std::mem::zeroed() },
                                    position: PhysicalPosition::new(px, py),
                                };
                                self.injected_window_event(event_loop, window_id, moved);
                                let press = WindowEvent::MouseInput {
                                    device_id: unsafe { std::mem::zeroed() },
                                    state: ElementState::Pressed,
                                    button: MouseButton::Left,
                                };
                                self.injected_window_event(event_loop, window_id, press);
                                let release = WindowEvent::MouseInput {
                                    device_id: unsafe { std::mem::zeroed() },
                                    state: ElementState::Released,
                                    button: MouseButton::Left,
                                };
                                self.injected_window_event(event_loop, window_id, release);
                            }
                            window.request_redraw();
                        }
                    }
                    AkWindowEvent::AccessibilityDeactivated => {}
                }
            }
        }
        if let Some((id, before)) = effect_before {
            crate::native_scenario::log::emit_json(
                "qos-native-effect",
                serde_json::json!({
                    "id": id, "before": before, "after": self.app.native_input_snapshot(),
                }),
            );
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let _entry = self.app.enter_wake_scope();
        // Can fire more than once (suspend/resume on mobile).
        if self.window.is_some() {
            return;
        }
        // Required, not incidental: the compositor only accepts
        // `set_selection` from the connection that owns the focused surface.
        #[cfg(target_os = "linux")]
        {
            crate::clipboard::install_wayland_backend_from_display(event_loop);
        }
        // AccessKit's adapter asserts it's created while the window is
        // hidden, so build invisible, create the adapter, show.
        let title = resolve_window_title(self.app.window_title.as_deref());
        let icon = crate::window_icon::resolve();
        let window_attributes = Window::default_attributes()
            .with_title(title)
            .with_window_icon(icon.clone())
            // Draggable title bar on compositors with no server-side
            // decorations (e.g. GNOME/mutter); explicit though it's the
            // winit default, so the intent is unmistakable.
            .with_decorations(true)
            .with_visible(false)
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        // Optional profiling placement keeps concurrent benchmark windows from
        // completely occluding one another. Platform window managers may ignore it.
        let window_attributes = match std::env::var("QOS_PROFILE_WINDOW_POSITION")
            .ok()
            .and_then(|value| crate::profiling::profile_window_position(&value))
        {
            Some((x, y)) => window_attributes.with_position(winit::dpi::LogicalPosition::new(x, y)),
            None => window_attributes,
        };
        // Benchmarks can keep their own windows above unrelated desktop apps.
        // This does not change normal application window ordering.
        let window_attributes = if std::env::var("QOS_PROFILE_WINDOW_FRONT").as_deref() == Ok("1") {
            window_attributes.with_window_level(winit::window::WindowLevel::AlwaysOnTop)
        } else {
            window_attributes
        };
        // See `resolve_app_id`: a shared id collapses per-app compositor
        // lookups.
        #[cfg(target_os = "linux")]
        let window_attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            let package_id = std::env::var("TILO_PACKAGE_ID")
                .or_else(|_| std::env::var("QOS_PACKAGE_ID"))
                .ok();
            let app_id = resolve_app_id(
                self.app.window_app_id.as_deref().or(package_id.as_deref()),
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .as_deref(),
            );
            window_attributes.with_name(app_id, "")
        };
        // `resumed` is a winit `ApplicationHandler` callback returning `()`,
        // so a `create_window` failure (compositor gone) has nowhere to
        // propagate to; there is no window to run without it either way.
        #[allow(
            clippy::unwrap_used,
            reason = "fixed () trait signature; window-creation failure at startup is unrecoverable"
        )]
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::ActiveEventLoopExtMacOS;
            if let Err(error) = event_loop.set_quit_request_window(Some(window.id())) {
                crate::diag!("tilo: cannot protect application Quit: {error}");
                event_loop.exit();
                return;
            }
        }
        tilo_startup_metrics::mark("primary-window-created");
        self.native_input_pointer_passthrough = false;
        self.native_input_route_isolation = false;
        if std::env::var("QOS_PROFILE_POINTER_PASSTHROUGH").as_deref() == Ok("1")
            && std::env::var("QOS_PROFILE_WINDOW_NO_ACTIVATE").as_deref() == Ok("1")
            && (self.native_scenario.is_some() || self.native_input_origin.is_some())
        {
            // Physical pointer movement must not replace the scripted hover.
            // Scripted inputs still use the normal UserEvent dispatch route.
            if let Err(error) = window.set_cursor_hittest(false) {
                eprintln!("Native benchmark pointer isolation failed: {error}");
                event_loop.exit();
                return;
            }
            self.native_input_pointer_passthrough = true;
            self.native_input_route_isolation = true;
        }
        // Some backends only honour the icon post-creation; a no-op on
        // Wayland compositors that source it from the app_id instead.
        window.set_window_icon(icon);

        // Must be created before the window is shown.
        self.a11y_adapter = Some(accesskit_winit::Adapter::with_event_loop_proxy(
            event_loop,
            &window,
            self.a11y_proxy.clone(),
        ));
        crate::widget::mount_native_views(
            self.app.root_widget.as_mut(),
            &crate::widget::NativeViewHost::live(&window, self.app.input.platform),
        );
        #[cfg(windows)]
        if let Err(reason) = self.init_render(&window) {
            crate::diag!("tilo: {reason}");
            event_loop.exit();
            return;
        }
        tilo_startup_metrics::mark("primary-window-show-start");
        window.set_visible(true);
        tilo_startup_metrics::mark("primary-window-show-done");

        self.app.layout.layout_dirty = true;
        // GPU init deferred to first `RedrawRequested` (`init_render`): the
        // ~50ms surface+device init here would block `resumed`'s seat
        // wiring â€” on mutter that intermittently left the toplevel
        // unfocused with dead input on first launch.
        window.request_redraw();
        tilo_startup_metrics::mark("primary-window-redraw-requested");
        crate::dnd::set_drag_window(&window);
        self.window = Some(window);
        tilo_startup_metrics::mark("primary-window-resumed-done");
    }

    /// BEFORE the `Window` arc is dropped â€” the worker's
    /// `RenderSurface<'static>` references it, and wgpu may panic if the
    /// surface outlives the window.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.app.close_wake_scope();
        let _entry = self.app.enter_wake_scope();
        // The Unix adapter owns an asynchronous AT-SPI provider.  Drop it
        // before the native views and window disappear, so external clients
        // receive the provider's normal unregister/teardown rather than being
        // left with object paths whose backing Wayland surface is already
        // gone.  This also makes the ordering explicit instead of relying on
        // AppHandler field-drop order.
        self.a11y_adapter = None;
        if !self.app.close_admission.admitted {
            self.app.autosave_flush();
        }
        self.app.close_native_content();
        self.close_audience_presentation();
        for surface in &mut self.floating_windows {
            if let Some(mut render) = surface.render.take() {
                render.shutdown_and_join();
            }
        }
        self.floating_windows.clear();
        if let Some(mut render) = self.render.take() {
            render.shutdown_and_join();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        self.route_window_event(
            event_loop,
            window_id,
            event,
            native_input_isolation::EventOrigin::Platform,
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "winit idle work must remain ordered within its single lifecycle callback"
    )]
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.app.service_close_admission();
        if self.app.finish_close_if_ready() {
            event_loop.exit();
            return;
        }
        let _entry = self.app.enter_wake_scope();
        let Some(window) = self.window.clone() else {
            return;
        };
        // Conservative shared-root fallback: hidden primary work may stay at
        // 60Hz while a visible auxiliary depends on its update/sync projection.
        // Only all-known-occluded surfaces receive full idle backoff.
        if self.visible_auxiliary_needs_shared_root() {
            self.primary_retry.keep_shared_cadence(Instant::now());
        }
        // Once admission defers, periodic animation/startup demand must not
        // spin redraw events against the busy worker. Keep updating dirty state;
        // the worker's armed completion callback requests the next paint.
        let mut auxiliary_retry_at: Option<Instant> = None;
        for (aux_window, render) in self
            .floating_windows
            .iter()
            .filter(|surface| !surface.suspended)
            .map(|surface| (&surface.window, surface.render.as_ref()))
            .chain(
                self.audience_presentation
                    .iter()
                    .map(|audience| (&audience.window, audience.render.as_ref())),
            )
        {
            if let Some(deadline) = render.and_then(|render| render.retry.deadline()) {
                auxiliary_retry_at =
                    Some(auxiliary_retry_at.map_or(deadline, |prior| prior.min(deadline)));
                if deadline <= Instant::now() && auxiliary_redraw_allowed(render) {
                    aux_window.request_redraw();
                }
            }
        }
        let auxiliary_retry_due =
            auxiliary_retry_at.is_some_and(|deadline| deadline <= Instant::now());
        let retry_now = Instant::now();
        let primary_blocked_until = self
            .primary_retry
            .occlusion_blocked(retry_now)
            .then(|| self.primary_retry.deadline())
            .flatten();
        let any_render_available = primary_blocked_until.is_none()
            || self.floating_windows.iter().any(|surface| {
                !surface.suspended
                    && surface
                        .render
                        .as_ref()
                        .is_none_or(|render| !render.retry.occlusion_blocked(retry_now))
            })
            || self.audience_presentation.as_ref().is_some_and(|audience| {
                audience
                    .render
                    .as_ref()
                    .is_none_or(|render| !render.retry.occlusion_blocked(retry_now))
            });
        let retry_at = self.primary_retry.deadline();
        let retry_due = retry_at.is_some_and(|deadline| deadline <= Instant::now());
        let redraw_deferred = self
            .render
            .as_ref()
            .is_some_and(|render| render.buffers.redraw_deferred())
            || retry_at.is_some_and(|deadline| Instant::now() < deadline);
        let request_primary_redraw = || {
            if !redraw_deferred {
                window.request_redraw();
            }
        };
        if retry_due {
            request_primary_redraw();
        }
        self.sync_ime(&window);
        if self.last_primary_frame_presented > 0
            && let Some(scenario) = self.native_scenario.take()
        {
            crate::native_scenario::log::emit_json(
                "qos-native-scenario",
                serde_json::json!({
                    "name": scenario.name, "steps": scenario.steps.len(),
                    "warmup_steps": scenario.warmup_steps, "interval_us": scenario.interval_us,
                    "event": "started_after_first_worker_presentation",
                }),
            );
            let proxy = self.a11y_proxy.clone();
            if let Err(error) = crate::native_scenario::spawn(scenario, move |packet| {
                proxy
                    .send_event(UserEvent::NativeScenarioInput(packet))
                    .is_ok()
            }) {
                crate::native_scenario::log::emit_json(
                    "qos-native-scenario",
                    serde_json::json!({"event":"failed", "error":error}),
                );
                event_loop.exit();
            }
        }

        if self.app.pump_floating_surface_service() {
            self.app.invalidate_widget_resource_cache();
            self.app.layout.layout_dirty = true;
            request_primary_redraw();
        }
        if self.apply_floating_host_messages(event_loop) {
            self.app.invalidate_widget_resource_cache();
            request_primary_redraw();
        }
        self.sync_native_floating_geometry();
        if self.recover_stalled_floating_windows() {
            request_primary_redraw();
        }
        // `RedrawRequested` polls the asynchronous renderer setup. The shell
        // keeps showing its live transfer proxy in the meantime, so these
        // frames are cheap but essential: the first real buffer should take
        // ownership as soon as it is ready, without waiting for fresh input.
        for surface in &self.floating_windows {
            if !surface.suspended
                && !surface.adopted
                && auxiliary_redraw_allowed(surface.render.as_ref())
            {
                surface.window.request_redraw();
            }
        }
        let awaiting_primary_first_present = primary_startup_pump_needed(
            self.primary_frames_sent,
            self.last_primary_frame_presented,
        );
        if awaiting_primary_first_present {
            request_primary_redraw();
        }
        // This runs before the ordinary fullscreen reconciliation below. If a
        // real second output appears, the root is notified first and keeps
        // the local window as the presenter dashboard; if it disappears the
        // existing fullscreen path immediately becomes the safe fallback.
        self.sync_audience_presentation(event_loop, &window);
        if let Some(audience) = self.audience_presentation.as_ref()
            && audience.render_init.is_some()
        {
            // Poll the worker result without ever blocking the presenter
            // event loop; this is short-lived only while GPU setup runs.
            audience.window.request_redraw();
        }
        let app = &mut self.app;

        // Catches a compositor reconfigure that changes `inner_size()`
        // without delivering `Resized`.
        if let Some(prev) = app.layout.last_frame_size
            && window.inner_size() != prev
        {
            app.layout.layout_dirty = true;
            request_primary_redraw();
        }
        if app.pump_collab_presence() {
            request_primary_redraw();
        }
        if app.poll_directed_invocations() {
            app.needs_sync = true;
            app.layout.layout_dirty = true;
            request_primary_redraw();
        }
        app.publish_playback_handoff();

        // Keep pumping frames while any "just typed" highlighter mark is
        // still fading, so the taper animates instead of freezing.
        let typing_now = Instant::now();
        app.presence_ctx.prune_typing(typing_now);
        let typing_active = app.presence_ctx.has_active_typing(typing_now);
        if typing_active {
            request_primary_redraw();
        }

        if app.dispatch_control_invocations() != 0 {
            app.needs_sync = true;
            app.layout.layout_dirty = true;
            request_primary_redraw();
        }

        if app.service_history() {
            app.needs_sync = true;
            app.layout.layout_dirty = true;
            request_primary_redraw();
        }

        if app.root_widget.needs_commit() {
            if let Some(origin) = self.native_input_origin {
                crate::native_scenario::log::emit_json(
                    "qos-native-commit-redraw",
                    serde_json::json!({
                        "after_cpu_frame": app.profiler.as_ref().and_then(|p| p.history().back()).map(|s| s.frame),
                        "at_us": origin.elapsed().as_micros(),
                    }),
                );
            }
            app.commit_action();
            app.root_widget.on_commit();
            app.needs_sync = true;
            request_primary_redraw();
        }

        // Level-triggered rather than hooked into individual mutations:
        // widgets write to the document from many places.
        if let Err(error) = app.autosave_tick() {
            crate::diag!("[app] component autosave tick refused: {error:?}");
        }

        if app.reconcile_fullscreen(&window) {
            request_primary_redraw();
        }

        // Run both: Settings' open Appearance pane may intentionally override
        // the file-level base with its richer live accessibility intent.
        let configured_changed = app.reconcile_configured_theme();
        let appearance_changed = app.reconcile_appearance();
        if configured_changed || appearance_changed {
            app.needs_sync = true;
            request_primary_redraw();
        }

        // The active page/view can change during `sync` (e.g. the initial
        // auto-select), which the post-event broadcast above wouldn't catch.
        app.broadcast_location_presence();
        app.broadcast_view_presence();

        // Run here too, not only `RedrawRequested`, since painting isn't
        // guaranteed â€” a background tab, or a first frame behind a shader
        // compile.
        if app.record_tick() {
            request_primary_redraw();
        }

        // Without this a first peer with no input never re-syncs to seed its
        // doc after the grace window, so a late joiner diverges.
        if app.collaboration.manager.is_connected() && app.root_widget.needs_idle_sync() {
            app.needs_sync = true;
            request_primary_redraw();
        }

        app.broadcast_doc_delta();

        // Flag drained by the draw handler, not here â€” just pump a redraw.
        if app.root_widget.take_structure_dirty() {
            app.layout.structure_dirty = true;
            request_primary_redraw();
        }

        let scheduled_deadline = crate::close_admission::wake_deadline(
            app.root_widget.next_wake_deadline(),
            app.close_admission.save_waiting,
            Instant::now(),
        );
        let scheduled_due = scheduled_deadline.is_some_and(|deadline| deadline <= Instant::now());
        if scheduled_due {
            app.needs_sync = true;
            request_primary_redraw();
        }

        // A hover tooltip is not motion, so it reveals even under
        // reduce-motion — requested independently of the animation pump.
        let tooltip_pending = app.input.tooltip_host.needs_frame();
        if tooltip_pending {
            request_primary_redraw();
        }

        // Reduced motion lets the loop go idle since animated widgets
        // already paint their resting state.
        let needs_frame = app.root_widget.needs_animation_frame();
        let (animation_due, animation_deadline) = self.animation_schedule.poll(
            Instant::now(),
            any_render_available && animation_pump_needed(needs_frame, app.theme.reduce_motion),
        );
        if animation_due {
            app.needs_sync = true;
            // Forces `apply_layout` since an animation tick (e.g. momentum
            // fling) can reposition content without an input event.
            app.layout.layout_dirty = true;
            request_primary_redraw();
            for surface in &self.floating_windows {
                if auxiliary_redraw_allowed(surface.render.as_ref()) {
                    surface.window.request_redraw();
                }
            }
            if let Some(audience) = self.audience_presentation.as_ref()
                && auxiliary_redraw_allowed(audience.render.as_ref())
            {
                audience.window.request_redraw();
            }
        }

        // A background pump can run while its visible projection is stable.
        // Redrawing only on a changed snapshot avoids full-scene GPU work on
        // every otherwise-idle collaboration wake.
        if let Some(tick) = app.presence.frame_tick.as_mut()
            && tick(&mut app.presence_ctx)
        {
            request_primary_redraw();
        }

        // `Wait` sleeps until real input, zero CPU; escalate to a bounded
        // ~60 Hz tick only while something no winit event surfaces on its
        // own needs draining â€” a live collab socket, or a root still
        // wanting another frame.
        let awaiting_floating_pixels = self.floating_windows.iter().any(|surface| {
            !surface.suspended
                && !surface.adopted
                && surface
                    .render
                    .as_ref()
                    .is_none_or(|render| !render.retry.occlusion_blocked(Instant::now()))
        });
        let retry_at = retry_at.into_iter().chain(auxiliary_retry_at).min();
        // Widget deadlines are serviced by sync/redraw. While occluded they can
        // be delayed by at most the 250ms probe cap; independent history, record,
        // autosave and collaboration work above continues on its existing doors.
        let demand = crate::render_thread::retry::WakeDemand {
            model: app.collaboration.manager.is_connected(),
            typing: typing_active,
            primary_startup: awaiting_primary_first_present,
            tooltip: tooltip_pending,
            widget_due: scheduled_due,
            animation: animation_pump_needed(needs_frame, app.theme.reduce_motion),
            auxiliary_startup: awaiting_floating_pixels,
            retry_due: retry_due || auxiliary_retry_due,
        };
        match crate::render_thread::retry::next_wake(
            Instant::now(),
            primary_blocked_until,
            any_render_available,
            demand,
            scheduled_deadline,
            retry_at,
            animation_deadline,
        ) {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

#[cfg(test)]
mod window_title_tests {
    use super::*;

    #[test]
    fn humanize_ident_titlecases_and_splits() {
        assert_eq!(humanize_ident("notes"), "Notes");
        assert_eq!(humanize_ident("file-explorer"), "File Explorer");
        assert_eq!(humanize_ident("word-processor"), "Word Processor");
        assert_eq!(humanize_ident("db_client"), "Db Client");
        assert_eq!(humanize_ident("notes"), "Notes");
        assert_eq!(humanize_ident(""), "");
    }

    #[test]
    fn explicit_title_wins_over_everything() {
        let t = resolve_title_from(
            Some("My Doc â€” Notes"),
            Some("Notes"),
            Some("so.tilo.notes"),
            Some("notes"),
        );
        assert_eq!(t, "My Doc â€” Notes");
    }

    #[test]
    fn app_name_env_resolves_to_the_manifest_name() {
        let t = resolve_title_from(None, Some("Notes"), Some("so.tilo.notes"), Some("notes"));
        assert_eq!(t, "Notes");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn package_id_falls_back_to_humanized_last_segment() {
        let t = resolve_title_from(
            None,
            None,
            Some("so.tilo.file-explorer"),
            Some("file-explorer"),
        );
        assert_eq!(t, "File Explorer");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn binary_name_is_the_dev_run_fallback() {
        let t = resolve_title_from(None, None, None, Some("terminal"));
        assert_eq!(t, "Terminal");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn blank_candidates_are_skipped_not_used() {
        let t = resolve_title_from(Some("  "), Some(""), Some("so.tilo.notes"), Some("notes"));
        assert_eq!(t, "Notes");
    }

    #[test]
    fn generic_default_is_tilo_app_never_quickui_app() {
        let t = resolve_title_from(None, None, None, None);
        assert_eq!(t, "Tilo App");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn the_app_id_is_never_the_generic_one_when_anything_identifies_the_app() {
        assert_eq!(
            resolve_app_id(Some("so.tilo.file-explorer"), Some("file-explorer")),
            "so.tilo.file-explorer",
            "the package id is the identity everything else is keyed on"
        );
        assert_eq!(
            resolve_app_id(None, Some("file-explorer")),
            "file-explorer",
            "outside a sandbox the binary still names this app, and it \
             matches the trailing-component convention package ids use"
        );
        assert_eq!(
            resolve_app_id(Some("   "), Some("terminal")),
            "terminal",
            "a blank package id is no package id"
        );
        assert_eq!(
            resolve_app_id(None, None),
            "org.tilo.app",
            "the generic id survives only when there is nothing to say"
        );
        assert_ne!(resolve_app_id(None, Some("file-explorer")), "org.tilo.app");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn env_path_end_to_end_prefers_app_name() {
        let prev_name = std::env::var_os("TILO_APP_NAME");
        let prev_id = std::env::var_os("TILO_PACKAGE_ID");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe {
            std::env::set_var("TILO_APP_NAME", "Weather");
            std::env::set_var("TILO_PACKAGE_ID", "so.tilo.weather");
        }
        let resolved = resolve_window_title(None);
        // Restore before asserting so a failure doesn't leak env.
        unsafe {
            match prev_name {
                Some(v) => std::env::set_var("TILO_APP_NAME", v),
                None => std::env::remove_var("TILO_APP_NAME"),
            }
            match prev_id {
                Some(v) => std::env::set_var("TILO_PACKAGE_ID", v),
                None => std::env::remove_var("TILO_PACKAGE_ID"),
            }
        }
        assert_eq!(resolved, "Weather");
        assert_ne!(resolved, "QuickUI App");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn env_path_falls_back_to_package_id_without_app_name() {
        let prev_name = std::env::var_os("TILO_APP_NAME");
        let prev_id = std::env::var_os("TILO_PACKAGE_ID");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe {
            std::env::remove_var("TILO_APP_NAME");
            std::env::set_var("TILO_PACKAGE_ID", "so.tilo.calculator");
        }
        let resolved = resolve_window_title(None);
        unsafe {
            match prev_name {
                Some(v) => std::env::set_var("TILO_APP_NAME", v),
                None => std::env::remove_var("TILO_APP_NAME"),
            }
            match prev_id {
                Some(v) => std::env::set_var("TILO_PACKAGE_ID", v),
                None => std::env::remove_var("TILO_PACKAGE_ID"),
            }
        }
        assert_eq!(resolved, "Calculator");
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "tests assert exact assigned values without floating-point computation"
)]
mod pointer_broadcast_tests {
    use super::*;

    #[test]
    fn pointer_broadcast_allowed_first_call_always_passes() {
        let now = Instant::now();
        assert!(pointer_broadcast_allowed(
            None,
            now,
            Duration::from_millis(33)
        ));
    }

    #[test]
    fn pointer_broadcast_allowed_blocks_inside_gap() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(10);
        assert!(!pointer_broadcast_allowed(
            Some(t0),
            t1,
            Duration::from_millis(33),
        ));
    }

    #[test]
    fn pointer_broadcast_allowed_passes_at_and_past_gap() {
        // Exactly at the gap boundary is allowed â€” strict `>` would
        // accumulate single-ms drift under a sawtooth wakeup schedule.
        let t0 = Instant::now();
        assert!(pointer_broadcast_allowed(
            Some(t0),
            t0 + Duration::from_millis(33),
            Duration::from_millis(33),
        ));
        assert!(pointer_broadcast_allowed(
            Some(t0),
            t0 + Duration::from_millis(50),
            Duration::from_millis(33),
        ));
    }

    #[test]
    fn set_presence_broadcast_scope_resets_throttle() {
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        app.presence.last_pointer_broadcast = Some(Instant::now());
        app.set_presence_broadcast_scope(Some(PresenceKey::Workspace { id: "home".into() }));
        assert!(
            app.presence.last_pointer_broadcast.is_none(),
            "scope change must clear the throttle so the next move broadcasts immediately"
        );
        assert_eq!(
            app.presence_broadcast_scope(),
            Some(&PresenceKey::Workspace { id: "home".into() })
        );
    }

    /// Guards against `commit_action` serialising the whole document (which
    /// advances `AutoCommit::save`'s cursor and leaves `broadcast_doc_delta`
    /// nothing to send).
    #[test]
    fn a_commit_leaves_the_edit_for_the_incremental_broadcast() {
        use automerge::transaction::Transactable;

        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        // Drain the baseline so what the assertion sees is this edit alone.
        let _ = app.state.save_incremental();

        app.state_mut_for_test()
            .doc
            .put(&automerge::ROOT, "body", "the quarterly report")
            .expect("put on the root object");
        app.commit_action();

        assert!(
            !app.state.save_incremental().is_empty(),
            "the committed edit must survive as an incremental delta; an \
             intervening full save() would have advanced the save cursor and \
             left broadcast_doc_delta with nothing to send"
        );
    }

    /// The scope is what sends `Arrived`, so opt-in would mean an app that
    /// forgets to set it never receives state written before it opened.
    #[test]
    #[serial_test::serial]
    fn an_app_in_a_workspace_publishes_presence_without_being_asked() {
        let room = "40fc5b85-22d7-4294-a9df-ecc8a391cfce";
        // SAFETY: serialised against the sibling env test below.
        unsafe { std::env::set_var("TILO_WORKSPACE_ID", room) };
        let app = App::new(State::new(), Box::new(crate::widget::Text::new("t")));
        assert_eq!(
            app.presence_broadcast_scope(),
            Some(&PresenceKey::Workspace {
                id: room.to_string()
            }),
            "a workspace in the environment is a scope, with no app-side call"
        );
        unsafe { std::env::remove_var("TILO_WORKSPACE_ID") };
    }

    /// The nil cluster addresses nobody, so no room means nothing published.
    #[test]
    #[serial_test::serial]
    fn an_app_with_no_workspace_publishes_nothing() {
        // SAFETY: see above.
        unsafe {
            std::env::remove_var("TILO_WORKSPACE_ID");
            std::env::remove_var("QUICK_CLUSTER_ID");
        }
        let app = App::new(State::new(), Box::new(crate::widget::Text::new("t")));
        assert!(app.presence_broadcast_scope().is_none());
    }

    #[test]
    fn setting_scope_announces_arrival() {
        use std::sync::{Arc, Mutex};
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        let captured: Arc<Mutex<Vec<(PresenceKey, PresencePayload)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_captured = Arc::clone(&captured);
        app.set_presence_broadcast_sink(move |k, p| {
            sink_captured.lock().unwrap().push((k.clone(), p.clone()));
        });
        let key = PresenceKey::Workspace { id: "home".into() };
        app.set_presence_broadcast_scope(Some(key.clone()));
        // Clone out of the guard so the lock is released before the assertion,
        // rather than held across it.
        let observed = captured.lock().unwrap().clone();
        assert!(
            observed
                .iter()
                .any(|(k, p)| *k == key && matches!(p, PresencePayload::Arrived)),
            "setting a scope must announce Arrival, got {observed:?}"
        );
    }

    #[test]
    fn document_snapshot_merges_not_replaces() {
        use automerge::transaction::Transactable;
        use automerge::{ObjType, ROOT, ReadDoc};
        let mut base = automerge::AutoCommit::new();
        let m = base.put_object(&ROOT, "m", ObjType::Map).unwrap();
        base.put(&m, "x", 1).unwrap();
        let base_bytes = base.save();

        let app_state = State::load(&base_bytes).unwrap();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(app_state, root);
        let route = crate::collaboration::RouteSpec::app("snapshot-fixture", uuid::Uuid::new_v4());
        app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
            route: route.clone(),
            epoch: uuid::Uuid::new_v4(),
        });
        {
            let doc = &mut app.state_mut_for_test().doc;
            let Ok(Some((automerge::Value::Object(ObjType::Map), m))) = doc.get(&ROOT, "m") else {
                panic!("m exists")
            };
            doc.put(&m, "a", 10).unwrap();
        }

        let mut peer = automerge::AutoCommit::load(&base_bytes).unwrap();
        let Ok(Some((automerge::Value::Object(ObjType::Map), pm))) = peer.get(&ROOT, "m") else {
            panic!("m exists")
        };
        peer.put(&pm, "b", 20).unwrap();
        let incoming = peer.save();

        let envelope = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(incoming)
                .encode()
                .unwrap(),
            collab_types::document_artifact_id(&route.app_id, &route.cluster_id.to_string(), ""),
            collab_types::Ed25519PublicKey::from_bytes([0; 32]),
            collab_types::document_attestation_id(&route.app_id, &route.cluster_id.to_string(), ""),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            0,
        );
        app.state.commit_action(None, None);
        app.apply_signed_document_envelope(&envelope, None);

        // A replace would drop "a".
        let doc = &mut app.state_mut_for_test().doc;
        let Ok(Some((automerge::Value::Object(ObjType::Map), m))) = doc.get(&ROOT, "m") else {
            panic!("m exists")
        };
        assert!(
            doc.get(&m, "a").unwrap().is_some(),
            "local edit survives merge"
        );
        assert!(doc.get(&m, "b").unwrap().is_some(), "remote edit merged in");
        assert!(doc.get(&m, "x").unwrap().is_some(), "base value present");
    }

    #[test]
    fn presence_broadcast_sink_receives_both_key_and_payload() {
        use std::sync::{Arc, Mutex};
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        let captured: Arc<Mutex<Vec<(PresenceKey, PresencePayload)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_captured = Arc::clone(&captured);
        app.set_presence_broadcast_sink(move |k, p| {
            sink_captured.lock().unwrap().push((k.clone(), p.clone()));
        });
        let key = PresenceKey::Workspace { id: "home".into() };
        app.broadcast_presence(key.clone(), PresencePayload::Pointer { x: 5.0, y: 7.0 });
        let observed = captured.lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].0, key);
        match observed[0].1 {
            PresencePayload::Pointer { x, y } => {
                assert_eq!(x, 5.0);
                assert_eq!(y, 7.0);
            }
            ref other => panic!("expected Pointer payload, got {other:?}"),
        }
    }

    #[test]
    fn clear_presence_broadcast_sink_drops_callback() {
        use std::sync::{Arc, Mutex};
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        let count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let sink_count = Arc::clone(&count);
        app.set_presence_broadcast_sink(move |_, _| {
            *sink_count.lock().unwrap() += 1;
        });
        app.broadcast_presence(
            PresenceKey::Workspace { id: "x".into() },
            PresencePayload::Arrived,
        );
        app.clear_presence_broadcast_sink();
        app.broadcast_presence(
            PresenceKey::Workspace { id: "x".into() },
            PresencePayload::Arrived,
        );
        assert_eq!(
            *count.lock().unwrap(),
            1,
            "sink fires before clear, never after"
        );
    }
}

#[cfg(test)]
mod rebuild_focus_tests {
    use super::*;
    use crate::widget::{Text, TextInput};
    use automerge::{AutoCommit, ObjId};
    use vello::kurbo::Rect;

    /// Recreates its children on every `build_layout`: the focused
    /// `TextInput` is a brand-new instance after rebuild, so its caret
    /// resets unless the App restores it via `get_state`/`set_state`.
    struct RebuildHarness {
        show_status: bool,
        request_reset: bool,
        children: Vec<Box<dyn Widget>>,
        rect: Rect,
    }

    impl RebuildHarness {
        fn new() -> Self {
            Self {
                show_status: false,
                request_reset: false,
                children: Vec::new(),
                rect: Rect::ZERO,
            }
        }

        fn rebuild_children(&mut self) {
            let mut kids: Vec<Box<dyn Widget>> = Vec::new();
            // `with_initial_value` parks the caret at the END ("hello" -> 5),
            // so a restored caret of 3 is distinguishable from both the
            // reset-to-0 bug and the new instance's own default.
            kids.push(Box::new(TextInput::local().with_initial_value("hello")));
            if self.show_status {
                kids.push(Box::new(Text::new("status")));
            }
            self.children = kids;
        }
    }

    impl Widget for RebuildHarness {
        fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
            for c in &mut self.children {
                c.sync(state, path);
            }
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.rebuild_children();
            let child_nodes: Vec<NodeId> = self
                .children
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            let node = taffy
                .new_with_children(taffy::Style::default(), &child_nodes)
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let layout = taffy.layout(node).unwrap();
            self.rect = Rect::new(
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
                origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
                origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
            );
            if let Ok(child_nodes) = taffy.children(node) {
                for (c, &cn) in self.children.iter_mut().zip(child_nodes.iter()) {
                    c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
                }
            }
        }
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _theme: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut AutoCommit,
            _r: &ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _n: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.children.iter().map(Box::as_ref).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.children
                .iter_mut()
                .map(|c| c.as_mut() as &mut (dyn Widget + '_))
                .collect()
        }
        fn on_external_event(&mut self, payload: &str, _s: &mut AutoCommit, _r: &ObjId) {
            match payload {
                "show_status" => self.show_status = true,
                "reset" => self.request_reset = true,
                _ => {}
            }
        }
        fn take_focus_reset(&mut self) -> bool {
            std::mem::take(&mut self.request_reset)
        }
        fn name(&self) -> &'static str {
            "RebuildHarness"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    fn caret(app: &App) -> Option<String> {
        let id = app.focused_node?;
        let w = *app.layout.node_map.get(&id)?;
        unsafe { (*w).get_state() }
    }

    #[test]
    fn caret_preserved_across_structure_rebuild() {
        let mut app =
            App::new_with_controlled_inputs(State::new(), Box::new(RebuildHarness::new()));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();

        let id = app.focused_node.expect("input focused at startup");
        unsafe {
            (*(*app.layout.node_map.get(&id).unwrap())).set_state("3");
        }
        assert_eq!(caret(&app).as_deref(), Some("3"));

        // Triggers a structural rebuild that recreates the input as a fresh
        // instance whose own caret default is 5.
        let mut tmp = AutoCommit::new();
        app.root_widget
            .on_external_event("show_status", &mut tmp, &automerge::ROOT);
        app.rebuild_structure();

        assert!(app.focused_node.is_some(), "focus preserved across rebuild");
        assert_eq!(
            caret(&app).as_deref(),
            Some("3"),
            "caret offset must be preserved across the rebuild"
        );
        assert_eq!(
            app.root_widget.children().len(),
            2,
            "status sibling appeared"
        );
        assert_eq!(app.focusable_order.len(), 1, "status is non-focusable");
    }

    #[test]
    fn focus_reset_does_not_restore_caret() {
        let mut app =
            App::new_with_controlled_inputs(State::new(), Box::new(RebuildHarness::new()));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();

        let id = app.focused_node.unwrap();
        unsafe {
            (*(*app.layout.node_map.get(&id).unwrap())).set_state("3");
        }

        let mut tmp = AutoCommit::new();
        app.root_widget
            .on_external_event("reset", &mut tmp, &automerge::ROOT);
        app.rebuild_structure();

        assert_eq!(
            caret(&app).as_deref(),
            Some("5"),
            "focus_reset must not paste the old caret onto the new field"
        );
    }
}

/// Mutating events should route through this so denied interactions
/// short-circuit before the widget ever observes them.
pub fn dispatch_gated_event<W: crate::widget::Widget + ?Sized>(
    ctx: &crate::permissions::PermissionContext,
    widget: &W,
    dispatch: impl FnOnce(),
) -> bool {
    if let Some(scope) = widget.permission_scope()
        && !ctx.check(&scope)
    {
        return false;
    }
    dispatch();
    true
}

/// The focus-narration path (`set_focused_node`) is private, so these tests
/// drive it through the public `init_layout` (autofocus) + the a11y action
/// dispatch / setter API.
#[cfg(test)]
mod screen_reader_tests {
    use super::*;
    use crate::screen_reader::ScreenReader;
    use crate::widget::{Button, Widget};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use tts::{NullSpeaker, Speaker, Utterance};

    #[derive(Clone, Default)]
    struct SharedRecorder(Rc<RefCell<NullSpeaker>>);
    impl Speaker for SharedRecorder {
        fn speak_utterance(&mut self, u: Utterance) {
            self.0.borrow_mut().speak_utterance(u);
        }
        fn stop(&mut self) {
            self.0.borrow_mut().stop();
        }
        fn set_rate(&mut self, r: f32) {
            self.0.borrow_mut().set_rate(r);
        }
        fn set_voice(&mut self, v: &str) {
            self.0.borrow_mut().set_voice(v);
        }
        fn is_speaking(&self) -> bool {
            self.0.borrow().is_speaking()
        }
        fn backend_name(&self) -> &'static str {
            "shared-null"
        }
    }
    // SAFETY: test-only, single-threaded, so the !Send Rc is fine.
    #[allow(
        clippy::non_send_fields_in_send_ty,
        reason = "the Rc<RefCell<_>> is deliberately asserted Send above â€” this test double \
                  never actually crosses a thread"
    )]
    unsafe impl Send for SharedRecorder {}

    struct TwoButtons {
        kids: Vec<Box<dyn Widget>>,
    }
    impl TwoButtons {
        fn new() -> Self {
            Self {
                kids: vec![
                    Box::new(Button::new("a", "Save", None)),
                    Box::new(Button::new("b", "Cancel", None)),
                ],
            }
        }
    }
    impl Widget for TwoButtons {
        fn sync(&mut self, s: &mut automerge::AutoCommit, p: &automerge::ObjId) {
            for c in &mut self.kids {
                c.sync(s, p);
            }
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let kids: Vec<_> = self
                .kids
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            let node = taffy
                .new_with_children(taffy::Style::default(), &kids)
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
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
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut automerge::AutoCommit,
            _p: &automerge::ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _ni: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.kids.iter().map(Box::as_ref).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.kids
                .iter_mut()
                .map(|c| &mut **c as &mut (dyn Widget + '_))
                .collect()
        }
        fn name(&self) -> &'static str {
            "two-buttons"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
    }

    fn app_with_reader() -> (App, SharedRecorder) {
        let rec = SharedRecorder::default();
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(TwoButtons::new()));
        app.set_screen_reader(Some(ScreenReader::with_speaker(Box::new(rec.clone()))));
        (app, rec)
    }

    #[test]
    fn autofocus_narrates_the_focused_widget() {
        let (mut app, rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        assert_eq!(rec.0.borrow().spoken(), &["Save, button".to_string()]);
    }

    #[test]
    fn directional_focus_change_narrates_new_widget() {
        let (mut app, rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        let second = app.focusable_order[1];
        let ak_id = accesskit::NodeId(u64::from(second) + 1);
        app.dispatch_a11y_action(ak_id, &crate::a11y::Action::Focus);
        let spoken = rec.0.borrow();
        assert_eq!(
            spoken.spoken(),
            &["Save, button".to_string(), "Cancel, button".to_string()],
            "each focus change narrates the newly focused control"
        );
    }

    #[test]
    fn disabling_reader_silences_and_stops() {
        let (mut app, rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        assert!(app.screen_reader_enabled());
        app.set_screen_reader_enabled(false);
        assert!(!app.screen_reader_enabled());
        assert!(
            rec.0.borrow().stop_count() >= 1,
            "disabling stops in-flight speech"
        );
        let before = rec.0.borrow().spoken().len();
        let second = app.focusable_order[1];
        let ak_id = accesskit::NodeId(u64::from(second) + 1);
        app.dispatch_a11y_action(ak_id, &crate::a11y::Action::Focus);
        assert_eq!(
            rec.0.borrow().spoken().len(),
            before,
            "no narration after the reader is disabled"
        );
    }

    #[test]
    fn reader_disabled_by_default_unless_env() {
        let mut app = App::new(State::new(), Box::new(TwoButtons::new()));
        app.autofocus_first = true;
        app.init_layout();
        assert!(
            !app.screen_reader_enabled(),
            "the reader is opt-in; default-off builds are unaffected"
        );
    }

    #[test]
    fn screen_reader_key_layer_is_noop_when_disabled() {
        // winit's KeyEvent has no public constructor; the disabled-guard is
        // the first line of the method, so a non-keyboard event exercises it.
        let mut app = App::new(State::new(), Box::new(TwoButtons::new()));
        app.init_layout();
        assert!(!app.screen_reader_enabled());
        let ev = WindowEvent::CursorLeft {
            device_id: unsafe { std::mem::zeroed() },
        };
        assert!(
            !app.handle_screen_reader_key(&ev),
            "reader-off App must never consume an event in the SR key layer"
        );
    }

    #[test]
    fn screen_reader_key_layer_ignores_non_keyboard_when_enabled() {
        let (mut app, _rec) = app_with_reader();
        app.init_layout();
        assert!(app.screen_reader_enabled());
        let ev = WindowEvent::CursorLeft {
            device_id: unsafe { std::mem::zeroed() },
        };
        assert!(!app.handle_screen_reader_key(&ev));
    }

    // ----- Live regions -----

    /// One live region whose spoken value is mutable between frames.
    struct LiveWidget {
        name: String,
        value: Rc<RefCell<String>>,
        politeness: crate::a11y::Politeness,
    }
    impl LiveWidget {
        fn new(name: &str, politeness: crate::a11y::Politeness) -> Self {
            Self {
                name: name.to_string(),
                value: Rc::new(RefCell::new(String::new())),
                politeness,
            }
        }
    }
    impl Widget for LiveWidget {
        fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
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
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut automerge::AutoCommit,
            _p: &automerge::ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _ni: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            vec![]
        }
        fn name(&self) -> &'static str {
            "live-widget"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            let v = self.value.borrow().clone();
            let mut node = crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_name(self.name.clone())
                .with_live(self.politeness);
            if !v.is_empty() {
                node = node.with_value(v);
            }
            Some(node)
        }
    }

    fn app_with_reader_over(root: Box<dyn Widget>) -> (App, SharedRecorder) {
        let rec = SharedRecorder::default();
        let mut app = App::new_with_controlled_inputs(State::new(), root);
        app.set_screen_reader(Some(ScreenReader::with_speaker(Box::new(rec.clone()))));
        app.init_layout();
        (app, rec)
    }

    #[test]
    fn live_region_change_announces_without_moving_focus() {
        let live = LiveWidget::new("Notifications", crate::a11y::Politeness::Assertive);
        let val = live.value.clone();
        let (mut app, rec) = app_with_reader_over(Box::new(live));
        let focus_before = app.focused_node;

        // Seed the snapshot, then forget it so we assert purely on the change.
        app.scan_live_regions();
        rec.0.borrow_mut().reset();

        *val.borrow_mut() = "File saved".to_string();
        app.scan_live_regions();
        assert_eq!(
            rec.0.borrow().last(),
            Some("Notifications, group, File saved"),
            "the changed live text is announced (name, role, value)"
        );
        assert_eq!(
            app.focused_node, focus_before,
            "live region must not move focus"
        );

        rec.0.borrow_mut().reset();
        app.scan_live_regions();
        assert!(
            rec.0.borrow().spoken().is_empty(),
            "unchanged live text is not re-announced"
        );
    }

    #[test]
    fn assertive_live_region_uses_important_interrupting_priority() {
        let live = LiveWidget::new("Errors", crate::a11y::Politeness::Assertive);
        let val = live.value.clone();
        let (mut app, rec) = app_with_reader_over(Box::new(live));
        *val.borrow_mut() = "Permission denied".to_string();
        app.scan_live_regions();
        let recref = rec.0.borrow();
        let last = recref
            .utterances()
            .last()
            .expect("an utterance was recorded");
        assert_eq!(last.priority, tts::Priority::Important);
        assert!(last.interrupt, "assertive live regions interrupt");
    }

    #[test]
    fn polite_live_region_uses_notification_non_interrupting_priority() {
        let live = LiveWidget::new("Status", crate::a11y::Politeness::Polite);
        let val = live.value.clone();
        let (mut app, rec) = app_with_reader_over(Box::new(live));
        *val.borrow_mut() = "Uploading, 40%".to_string();
        app.scan_live_regions();
        let recref = rec.0.borrow();
        let last = recref
            .utterances()
            .last()
            .expect("an utterance was recorded");
        assert_eq!(last.priority, tts::Priority::Notification);
        assert!(!last.interrupt, "polite live regions do not interrupt");
    }

    #[test]
    fn live_region_scan_is_noop_when_reader_off() {
        let live = LiveWidget::new("Notifications", crate::a11y::Politeness::Assertive);
        let val = live.value.clone();
        let mut app = App::new(State::new(), Box::new(live));
        app.init_layout();
        assert!(!app.screen_reader_enabled());
        *val.borrow_mut() = "anything".to_string();
        app.scan_live_regions();
    }

    // ----- Text review (caret movement over existing content) -----

    fn app_with_focused_input(initial: &str) -> (App, SharedRecorder, NodeId) {
        let rec = SharedRecorder::default();
        let input = crate::widget::TextInput::local().with_initial_value(initial);
        let mut app = App::new(State::new(), Box::new(input));
        app.set_screen_reader(Some(ScreenReader::with_speaker(Box::new(rec.clone()))));
        app.autofocus_first = true;
        app.init_layout();
        let id = app.focused_node.expect("the input is focused at startup");
        (app, rec, id)
    }

    #[test]
    fn caret_move_one_char_announces_the_character() {
        let (mut app, rec, id) = app_with_focused_input("abc");
        let pre = app.text_review_snapshot();
        assert!(pre.is_some(), "focused text input yields a review snapshot");
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        unsafe {
            (*(*app.layout.node_map.get(&id).unwrap())).set_state("2");
        }
        app.announce_text_review(pre);
        assert_eq!(
            rec.0.borrow().last(),
            Some("c"),
            "single-step move echoes the char"
        );
    }

    #[test]
    fn caret_jump_to_start_announces_the_word() {
        let (mut app, rec, id) = app_with_focused_input("hello world");
        let pre = app.text_review_snapshot();
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        unsafe {
            (*(*app.layout.node_map.get(&id).unwrap())).set_state("0");
        }
        app.announce_text_review(pre);
        assert_eq!(
            rec.0.borrow().last(),
            Some("hello"),
            "a long jump echoes the word"
        );
    }

    #[test]
    fn no_caret_move_announces_nothing() {
        let (mut app, rec, _id) = app_with_focused_input("abc");
        let pre = app.text_review_snapshot();
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        app.announce_text_review(pre);
        assert!(
            rec.0.borrow().spoken().is_empty(),
            "no move, no announcement"
        );
    }

    #[test]
    fn text_review_snapshot_is_none_for_non_text_widget() {
        let (mut app, _rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        assert!(app.focused_node.is_some());
        assert!(
            app.text_review_snapshot().is_none(),
            "a button has no caret to review"
        );
    }
}

/// The modal's open flag is a state change (no structural rebuild), so node
/// ids stay stable and `refresh_focus_order` re-evaluates the trap.
#[cfg(test)]
mod focus_trap_tests {
    use super::*;
    use crate::widget::modal_dialog::ModalDialog;
    use crate::widget::{Button, Widget};
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::rc::Rc;

    struct TwoKids {
        kids: Vec<Box<dyn Widget>>,
    }
    impl TwoKids {
        fn new() -> Self {
            Self {
                kids: vec![
                    Box::new(Button::new("ok", "OK", None)),
                    Box::new(Button::new("cancel", "Cancel", None)),
                ],
            }
        }
    }
    impl Widget for TwoKids {
        fn sync(&mut self, s: &mut automerge::AutoCommit, p: &automerge::ObjId) {
            for c in &mut self.kids {
                c.sync(s, p);
            }
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let kids: Vec<_> = self
                .kids
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            let node = taffy
                .new_with_children(taffy::Style::default(), &kids)
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
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
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut automerge::AutoCommit,
            _p: &automerge::ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _ni: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.kids.iter().map(Box::as_ref).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.kids
                .iter_mut()
                .map(|c| &mut **c as &mut (dyn Widget + '_))
                .collect()
        }
        fn name(&self) -> &'static str {
            "two-kids"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
    }

    /// A shared `want_open` cell flips the modal's state in `sync`, modelling
    /// a host that opens/closes without rebuilding the widget tree.
    struct ModalHost {
        outer: Vec<Box<dyn Widget>>,
        modal: ModalDialog<TwoKids>,
        want_open: Rc<Cell<bool>>,
    }
    impl ModalHost {
        fn new(want_open: Rc<Cell<bool>>) -> Self {
            Self {
                outer: vec![
                    Box::new(Button::new("a", "Open", None)),
                    Box::new(Button::new("b", "Other", None)),
                ],
                modal: ModalDialog::new("Confirm", TwoKids::new()),
                want_open,
            }
        }
    }
    impl Widget for ModalHost {
        fn sync(&mut self, s: &mut automerge::AutoCommit, p: &automerge::ObjId) {
            self.modal.set_open(self.want_open.get());
            for c in &mut self.outer {
                c.sync(s, p);
            }
            self.modal.sync(s, p);
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.modal.set_open(self.want_open.get());
            let mut kids: Vec<_> = self
                .outer
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            kids.push(self.modal.build_layout(taffy, theme, node_map));
            let node = taffy
                .new_with_children(taffy::Style::default(), &kids)
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
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
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut automerge::AutoCommit,
            _p: &automerge::ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _ni: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            let mut v: Vec<&dyn Widget> = self.outer.iter().map(Box::as_ref).collect();
            v.push(&self.modal);
            v
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            let mut v: Vec<&mut (dyn Widget + '_)> = self
                .outer
                .iter_mut()
                .map(|c| &mut **c as &mut (dyn Widget + '_))
                .collect();
            v.push(&mut self.modal as &mut (dyn Widget + '_));
            v
        }
        fn name(&self) -> &'static str {
            "modal-host"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
    }

    /// Built closed; flipping the cell + calling `refresh_focus_order`
    /// models the per-frame re-evaluation.
    fn host_app() -> (App, Rc<Cell<bool>>) {
        let cell = Rc::new(Cell::new(false));
        let app =
            App::new_with_controlled_inputs(State::new(), Box::new(ModalHost::new(cell.clone())));
        (app, cell)
    }

    #[test]
    fn container_overlay_traps_tab_in_its_controls() {
        use crate::widget::{Column, Container};
        let root = Column::new()
            .push(Box::new(Button::new("background", "Background", None)))
            .push(Box::new(
                Container::new()
                    .with_focus_trap()
                    .with_child(Box::new(TwoKids::new())),
            ));
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        assert!(app.accessibility.focus_trap_active);
        assert_eq!(app.focusable_order.len(), 2);
        assert!(app.keyboard_nav_allowed(Some(KurboRect::new(0.0, 0.0, 100.0, 100.0))));
        let first = app.focusable_order[0];
        let second = app.focusable_order[1];
        app.set_focused_node(Some(first));
        assert!(app.focus_tab(false));
        assert_eq!(app.focused_node, Some(second));
        assert!(app.focus_tab(false));
        assert_eq!(app.focused_node, Some(first));
        assert!(app.focus_tab(true));
        assert_eq!(app.focused_node, Some(second));
    }

    #[test]
    fn closed_modal_does_not_trap_focus() {
        let (mut app, _open) = host_app();
        app.init_layout();
        assert!(
            !app.accessibility.focus_trap_active,
            "no trap while the modal is closed"
        );
        assert_eq!(
            app.focusable_order.len(),
            2,
            "closed modal controls stay out of the background focus order"
        );
    }

    #[test]
    fn open_modal_confines_focus_order_to_its_subtree() {
        let (mut app, open) = host_app();
        app.init_layout();
        let full = app.focusable_order.len();

        open.set(true);
        app.refresh_focus_order();

        assert!(
            app.accessibility.focus_trap_active,
            "an open modal traps focus"
        );
        assert_eq!(
            app.focusable_order.len(),
            2,
            "focus is confined to the modal's two buttons (was {full})"
        );
        assert!(
            app.focused_node
                .is_some_and(|id| app.focusable_order.contains(&id)),
            "focus moves into the modal when it opens"
        );
    }

    #[test]
    fn tab_cycles_within_the_trapped_modal_via_the_real_handler() {
        let (mut app, open) = host_app();
        app.init_layout();
        open.set(true);
        app.refresh_focus_order();
        assert!(app.accessibility.focus_trap_active);
        assert_eq!(app.focusable_order.len(), 2);
        let modal_set: std::collections::HashSet<_> = app.focusable_order.iter().copied().collect();
        let first = app.focusable_order[0];
        let second = app.focusable_order[1];

        // Must be allowed EVEN while a modal captures the pointer, or the
        // dialog's buttons would be unreachable by keyboard.
        assert!(
            app.keyboard_nav_allowed(Some(KurboRect::new(0.0, 0.0, 10.0, 10.0))),
            "an active focus trap allows Tab to run (scoped to the modal)"
        );

        app.set_focused_node(Some(first));
        assert!(app.focus_tab(false), "Tab moves focus");
        assert_eq!(
            app.focused_node,
            Some(second),
            "Tab advances within the modal"
        );
        assert!(modal_set.contains(&second));
        assert!(app.focus_tab(false));
        assert_eq!(
            app.focused_node,
            Some(first),
            "Tab from the last wraps to the first"
        );
        assert!(app.focus_tab(true));
        assert_eq!(
            app.focused_node,
            Some(second),
            "Shift+Tab from the first wraps to the last"
        );
        assert!(
            modal_set.contains(&app.focused_node.unwrap()),
            "Shift+Tab stays inside the modal subtree"
        );
    }

    #[test]
    fn pointer_capture_without_a_trap_suppresses_app_tab() {
        // A pointer-capturing, non-trapping surface (a context menu) must
        // switch off the App's own Tab/arrow navigation.
        let (mut app, _open) = host_app();
        app.init_layout();
        assert!(!app.accessibility.focus_trap_active);
        assert!(
            app.keyboard_nav_allowed(None),
            "with no modal, Tab runs normally"
        );
        assert!(
            !app.keyboard_nav_allowed(Some(KurboRect::new(0.0, 0.0, 10.0, 10.0))),
            "a pointer-capturing surface with no focus trap suppresses App-level Tab"
        );
    }

    #[test]
    fn closing_modal_restores_prior_focus() {
        let (mut app, open) = host_app();
        app.init_layout();
        let outer = app.focusable_order[0];
        app.set_focused_node(Some(outer));

        open.set(true);
        app.refresh_focus_order();
        assert!(app.accessibility.focus_trap_active);
        assert_ne!(app.focused_node, Some(outer), "focus moved into the modal");

        open.set(false);
        app.refresh_focus_order();
        assert!(
            !app.accessibility.focus_trap_active,
            "trap released when the modal closes"
        );
        assert_eq!(
            app.focused_node,
            Some(outer),
            "focus is restored to where it was before the modal opened"
        );
    }

    #[test]
    fn modal_exposes_dialog_role_with_title_as_name() {
        use crate::a11y::Role;
        let m = ModalDialog::new("Delete file?", TwoKids::new()).open(true);
        let node = Widget::accessibility(&m).expect("an open dialog is in the tree");
        assert_eq!(node.role, Role::Dialog);
        assert_eq!(node.name.as_deref(), Some("Delete file?"));
        assert!(node.state.modal, "an open modal traps focus â†’ modal flag");
        assert!(!node.state.hidden);
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact preference values are part of the test contract"
)]
mod low_vision_tests {
    use super::*;
    use crate::a11y::contrast_ratio;

    fn app() -> App {
        App::new(State::new(), Box::new(crate::widget::Text::new("t")))
    }

    #[test]
    fn defaults_are_off() {
        let a = app();
        assert!(!a.high_contrast());
        assert!(!a.reduce_motion());
        assert_eq!(a.text_scale(), 1.0);
        assert!(!a.theme.reduce_motion);
        assert_eq!(a.theme.text_scale, 1.0);
    }

    #[test]
    fn toggle_high_contrast_swaps_the_theme() {
        let mut a = app();
        let plain_ratio = contrast_ratio(a.theme.text_primary, a.theme.bg_app);

        let now_on = a.toggle_high_contrast();
        assert!(now_on);
        assert!(a.high_contrast());
        let hc_ratio = contrast_ratio(a.theme.text_primary, a.theme.bg_app);
        assert!(
            hc_ratio > plain_ratio,
            "high contrast must raise fg/bg contrast ({hc_ratio:.1} vs {plain_ratio:.1})",
        );
        assert_eq!(a.theme.name, "High Contrast");

        let now_off = a.toggle_high_contrast();
        assert!(!now_off);
        assert!(!a.high_contrast());
        let restored = contrast_ratio(a.theme.text_primary, a.theme.bg_app);
        assert!(
            (restored - plain_ratio).abs() < 1e-6,
            "off restores base colours"
        );
    }

    #[test]
    fn text_scale_setter_scales_the_theme_type_size() {
        let mut a = app();
        let base = a.theme.tokens.typography.scale.base;
        a.set_text_scale(2.0);
        assert_eq!(a.text_scale(), 2.0);
        assert!((a.theme.tokens.typography.scale.base - base * 2.0).abs() < 1e-3);
        // A scale change must re-run layout so the larger text doesn't clip.
        assert!(a.layout.layout_dirty);
    }

    #[test]
    fn text_scale_survives_a_high_contrast_toggle() {
        let mut a = app();
        a.set_text_scale(1.5);
        let hc_base = Theme::high_contrast().tokens.typography.scale.base;
        a.set_high_contrast(true);
        assert_eq!(
            a.text_scale(),
            1.5,
            "scale preference is preserved across the swap"
        );
        assert!((a.theme.tokens.typography.scale.base - hc_base * 1.5).abs() < 1e-3);
        assert_eq!(a.theme.name, "High Contrast");
    }

    #[test]
    fn reduce_motion_setter_threads_onto_the_theme() {
        let mut a = app();
        assert!(!a.theme.reduce_motion);
        a.set_reduce_motion(true);
        assert!(a.reduce_motion());
        assert!(
            a.theme.reduce_motion,
            "the flag reaches the theme widgets draw against"
        );
        a.set_reduce_motion(false);
        assert!(!a.theme.reduce_motion);
    }

    #[test]
    fn set_theme_keeps_low_vision_preferences() {
        let mut a = app();
        a.set_text_scale(1.25);
        a.set_reduce_motion(true);
        a.set_high_contrast(true);
        // Swap the *base* theme â€” the a11y layer must re-apply on top.
        a.set_theme(Theme::light());
        assert!(a.theme.reduce_motion);
        assert_eq!(a.text_scale(), 1.25);
        // High contrast still wins over the freshly-set light base.
        assert_eq!(a.theme.name, "High Contrast");
    }

    #[test]
    fn ui_scale_setter_grows_spacing_radii_and_type_together() {
        let mut a = app();
        let base_gap = a.theme.tokens.spacing.s3;
        let base_radius = a.theme.tokens.radii.md;
        let base_type = a.theme.tokens.typography.scale.base;
        a.set_ui_scale(1.5);
        assert_eq!(a.ui_scale(), 1.5);
        assert!(
            (a.theme.tokens.spacing.s3 - base_gap * 1.5).abs() < 1e-3,
            "spacing grows"
        );
        assert!(
            (a.theme.tokens.radii.md - base_radius * 1.5).abs() < 1e-3,
            "radii grow"
        );
        assert!(
            (a.theme.tokens.typography.scale.base - base_type * 1.5).abs() < 1e-3,
            "type grows"
        );
        // A UI-scale change must re-run layout so the whole interface re-flows.
        assert!(a.layout.layout_dirty);
    }

    #[test]
    fn ui_scale_and_text_scale_compose_on_the_live_theme() {
        let mut a = app();
        let base_type = a.theme.tokens.typography.scale.base;
        let base_gap = a.theme.tokens.spacing.s3;
        a.set_ui_scale(1.25);
        a.set_text_scale(1.2);
        assert_eq!(a.ui_scale(), 1.25);
        assert_eq!(a.text_scale(), 1.2);
        // Type picks up BOTH factors; spacing only the UI factor.
        assert!((a.theme.tokens.typography.scale.base - base_type * 1.25 * 1.2).abs() < 1e-2);
        assert!((a.theme.tokens.spacing.s3 - base_gap * 1.25).abs() < 1e-2);
    }

    #[test]
    fn ui_scale_survives_a_high_contrast_toggle() {
        let mut a = app();
        a.set_ui_scale(1.5);
        let hc_gap = Theme::high_contrast().tokens.spacing.s3;
        a.set_high_contrast(true);
        assert_eq!(
            a.ui_scale(),
            1.5,
            "UI-scale preference is preserved across the swap"
        );
        assert!((a.theme.tokens.spacing.s3 - hc_gap * 1.5).abs() < 1e-2);
        assert_eq!(a.theme.name, "High Contrast");
    }

    struct AppearanceRoot {
        intent: crate::theme::AppearanceIntent,
    }

    impl Widget for AppearanceRoot {
        fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _theme: &Theme,
            _p: &crate::presence::PresenceCtx,
        ) {
        }
        #[allow(
            clippy::too_many_arguments,
            reason = "implementation must match the shared Widget event signature"
        )]
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut automerge::AutoCommit,
            _r: &automerge::ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _n: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
            Some(self.intent.clone())
        }
        fn name(&self) -> &'static str {
            "AppearanceRoot"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }
    }

    fn intent(
        theme: Theme,
        ui_scale: f32,
        text_scale: f32,
        high_contrast: bool,
    ) -> crate::theme::AppearanceIntent {
        crate::theme::AppearanceIntent {
            base_theme: theme,
            high_contrast,
            reduce_motion: false,
            text_scale,
            ui_scale,
        }
    }

    #[test]
    fn appearance_reconcile_off_is_a_noop() {
        let root = AppearanceRoot {
            intent: intent(Theme::nord(), 1.5, 1.3, true),
        };
        let mut a = App::new(State::new(), Box::new(root));
        assert!(!a.reconcile_appearance());
        assert_eq!(a.theme.name, "Dark");
        assert_eq!(a.ui_scale(), 1.0);
    }

    #[test]
    fn appearance_reconcile_applies_scale_theme_and_contrast_live() {
        let base_gap = Theme::high_contrast().tokens.spacing.s3;
        let base_type = Theme::high_contrast().tokens.typography.scale.base;
        // Nord base + high_contrast=true â†’ high-contrast track wins; ui 1.5, text 1.2.
        let root = AppearanceRoot {
            intent: intent(Theme::nord(), 1.5, 1.2, true),
        };
        let mut a = App::new(State::new(), Box::new(root));
        a.appearance_reconcile = true;
        assert!(
            a.reconcile_appearance(),
            "first reconcile applies the intent"
        );
        assert_eq!(a.ui_scale(), 1.5);
        assert_eq!(a.text_scale(), 1.2);
        assert!(a.high_contrast());
        assert_eq!(
            a.theme.name, "High Contrast",
            "high_contrast overrides the Nord base"
        );
        // Spacing grew by the UI factor; type by UI Ã— text.
        assert!((a.theme.tokens.spacing.s3 - base_gap * 1.5).abs() < 1e-1);
        assert!((a.theme.tokens.typography.scale.base - base_type * 1.5 * 1.2).abs() < 1e-1);
        assert!(
            a.layout.layout_dirty,
            "a live appearance change re-runs layout"
        );
        assert!(
            !a.reconcile_appearance(),
            "unchanged intent does not re-theme"
        );
    }
}

#[cfg(test)]
mod fullscreen_tests {
    use super::*;
    use automerge::{AutoCommit, ObjId};
    use vello::kurbo::Rect;

    use std::cell::RefCell;
    use std::rc::Rc;

    /// Lets the App's fullscreen reconcile wiring be exercised without a
    /// live window.
    struct FsRoot {
        want: Option<bool>,
        rect: Rect,
        changes: Rc<RefCell<Vec<bool>>>,
    }

    impl FsRoot {
        fn new(want: Option<bool>) -> (Self, Rc<RefCell<Vec<bool>>>) {
            let changes = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    want,
                    rect: Rect::ZERO,
                    changes: changes.clone(),
                },
                changes,
            )
        }
    }

    impl crate::widget::FullscreenProvider for FsRoot {
        fn fullscreen_intent(&self) -> Option<bool> {
            self.want
        }

        fn on_fullscreen_changed(&mut self, active: bool) {
            self.changes.borrow_mut().push(active);
        }
    }

    impl Widget for FsRoot {
        fn fullscreen_provider(&self) -> Option<&dyn crate::widget::FullscreenProvider> {
            Some(self)
        }

        fn fullscreen_provider_mut(
            &mut self,
        ) -> Option<&mut dyn crate::widget::FullscreenProvider> {
            Some(self)
        }
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _theme: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut AutoCommit,
            _r: &ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _n: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "FsRoot"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    #[test]
    fn decision_truth_table() {
        assert_eq!(fullscreen_request_decision(false, None), None);
        assert_eq!(fullscreen_request_decision(true, None), None);
        assert_eq!(fullscreen_request_decision(false, Some(false)), None);
        assert_eq!(fullscreen_request_decision(true, Some(true)), None);
        assert_eq!(fullscreen_request_decision(false, Some(true)), Some(true));
        assert_eq!(fullscreen_request_decision(true, Some(false)), Some(false));
    }

    #[test]
    fn actual_state_transitions_notify_root_once_per_edge() {
        let (root, changes) = FsRoot::new(None);
        let mut app = App::new(State::new(), Box::new(root));

        assert!(app.note_actual_fullscreen(true));
        assert!(!app.note_actual_fullscreen(true));
        assert!(app.note_actual_fullscreen(false));

        assert_eq!(*changes.borrow(), vec![true, false]);
        assert!(!app.fullscreen_active);
    }

    #[test]
    fn entering_fullscreen_marks_resync_and_relayout() {
        let (root, _changes) = FsRoot::new(Some(true));
        let mut app = App::new(State::new(), Box::new(root));
        app.needs_sync = false;
        app.layout.layout_dirty = false;
        assert!(app.note_actual_fullscreen(true));
        assert!(app.needs_sync);
        assert!(app.layout.layout_dirty);
    }
}

#[cfg(test)]
mod action_palette_tests {
    use super::*;
    use crate::widget::{Button, Widget};
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ObjId, ROOT, ReadDoc, ScalarValue};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use tilo_actions::{Action, ActionError, ActionSpec, Doc, Resource};
    use winit::event::{ElementState, KeyEvent};
    use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Flip {
        key: String,
    }
    const FLIP_TYPE: &str = "test.flip";
    impl Action for Flip {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            doc.put(&ROOT, self.key.as_str(), ScalarValue::Boolean(true))?;
            Ok(())
        }
    }

    /// Stands in for an app's contacts/files provider.
    struct PeopleProvider;
    impl crate::search::SearchProvider for PeopleProvider {
        fn name(&self) -> &'static str {
            "People"
        }
        fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
            ["Ada Lovelace", "Grace Hopper"]
                .into_iter()
                .filter(|n| crate::search::fuzzy_score(query, n).is_some())
                .map(|n| crate::search::SearchResult::new("People", n, format!("person:{n}")))
                .collect()
        }
    }

    struct LazyProvider;
    impl crate::search::SearchProvider for LazyProvider {
        fn name(&self) -> &'static str {
            "Lazy"
        }

        fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
            if query.eq_ignore_ascii_case("restricted") {
                vec![crate::search::SearchResult::for_action(
                    "Lazy",
                    "Restricted lazy action",
                    tilo_actions::ActionAddress::new("test.lazy:1", "flip"),
                )]
            } else {
                Vec::new()
            }
        }

        fn resolve_action_resource(
            &self,
            _address: &tilo_actions::ActionAddress,
        ) -> Option<Resource> {
            panic!("search providers must never be asked for authority")
        }
    }

    /// One open resource exporting a [`Flip`] action, plus a searchable
    /// People provider.
    struct Publisher;
    struct ConsumingKeyboardRoot;
    impl Widget for ConsumingKeyboardRoot {
        fn sync(&mut self, _: &mut AutoCommit, _: &ObjId) {}
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _: &Theme,
            nodes: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = tree.new_leaf(taffy::Style::default()).unwrap();
            nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            _: f64,
            _: &winit::event::Modifiers,
            doc: &mut AutoCommit,
            _: &ObjId,
            handled: &mut bool,
            _: &mut Option<NodeId>,
            _: NodeId,
            _: &TaffyTree<()>,
        ) {
            if matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            ) {
                doc.put(&ROOT, "escaped_keyboard", true).unwrap();
                *handled = true;
            }
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn name(&self) -> &'static str {
            "consuming-keyboard-root"
        }
    }
    impl Widget for Publisher {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
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
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut AutoCommit,
            _p: &ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _ni: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "publisher"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
            vec![
                Resource::new("test.obj:1", "test.obj", "Widget Object").with_action(
                    ActionSpec::immediate(
                        "flip",
                        "Flip it",
                        FLIP_TYPE,
                        &Flip {
                            key: "flipped".into(),
                        },
                    )
                    .unwrap(),
                ),
            ]
        }
        fn resource(&self, _doc: &AutoCommit, id: &str) -> Option<Resource> {
            (id == "test.lazy:1").then(|| {
                Resource::new("test.lazy:1", "test.lazy", "Lazy object").with_action(
                    ActionSpec::immediate(
                        "flip",
                        "Flip lazy object",
                        FLIP_TYPE,
                        &Flip {
                            key: "lazy-flipped".into(),
                        },
                    )
                    .unwrap()
                    .requiring(tilo_actions::PermissionRequirement::Write),
                )
            })
        }
        fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
            vec![Box::new(PeopleProvider), Box::new(LazyProvider)]
        }
        fn activate_search_result(&mut self, action_id: &str, doc: &mut AutoCommit, _p: &ObjId) {
            doc.put(&ROOT, "navigated", ScalarValue::Str(action_id.into()))
                .unwrap();
        }
    }

    struct CountingPublisher(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl Widget for CountingPublisher {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
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
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut AutoCommit,
            _p: &ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _ni: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "counting-publisher"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            vec![Resource::new("test.cache:1", "test.cache", "Cached")]
        }
    }

    include!("resource_projection_tests.rs");

    #[test]
    fn control_refresh_without_action_bindings_does_not_materialize_catalog() {
        use std::sync::atomic::Ordering;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CountingPublisher(std::sync::Arc::clone(&calls))),
        );
        for _ in 0..3 {
            app.invalidate_widget_resource_cache();
            app.refresh_control_action_availability();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        app.actions.last_control_availability = Some((
            app.resource_cache_key(),
            app.actions.permission_generation.get(),
        ));
        app.refresh_control_action_availability();
        assert!(app.actions.last_control_availability.is_none());
        // Discovery still sees the complete surface on demand.
        assert!(
            app.collect_resources()
                .iter()
                .any(|r| r.id.as_str() == "test.cache:1")
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn resource_tree_is_materialized_once_per_exact_generation() {
        use std::sync::atomic::Ordering;

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CountingPublisher(std::sync::Arc::clone(&calls))),
        );
        let _ = app.collect_resources();
        let _ = app.collect_resources();
        app.refresh_control_action_availability();
        app.refresh_control_action_availability();
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        app.state_mut_for_test()
            .doc
            .put(&ROOT, "cache-version", 1_i64)
            .expect("test document mutation");
        let _ = app.collect_resources();
        assert_eq!(calls.load(Ordering::Relaxed), 2);

        app.invalidate_widget_resource_cache();
        let _ = app.collect_resources();
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn exhausted_resource_generation_evicts_instead_of_reusing_a_snapshot() {
        use std::sync::atomic::Ordering;

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CountingPublisher(std::sync::Arc::clone(&calls))),
        );
        app.actions.resource_generation.set(u64::MAX);

        let _ = app.collect_resources();
        app.invalidate_widget_resource_cache();
        let _ = app.collect_resources();

        assert_eq!(app.actions.resource_generation.get(), u64::MAX);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "each invalidation at the terminal generation rematerializes"
        );
    }

    #[test]
    fn exhausted_permission_generation_evicts_the_old_authority_projection() {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        assert_eq!(app.collect_local_resources().len(), 1);
        app.actions.permission_generation.set(u64::MAX);

        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));

        assert_eq!(app.actions.permission_generation.get(), u64::MAX);
        assert!(
            app.collect_local_resources().is_empty(),
            "the cached write action cannot survive a permission change"
        );
    }

    pub(super) fn synth_enter() -> WindowEvent {
        let event = KeyEvent::synthetic(
            winit::event::RawKeyEvent {
                physical_key: PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified,
                ),
                state: ElementState::Pressed,
            },
            Key::Named(NamedKey::Enter),
            None,
            KeyLocation::Standard,
            false,
            None,
            Key::Named(NamedKey::Enter),
        );
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    #[test]
    fn tab_focused_host_buttons_keep_enter_and_space_out_of_consuming_app() {
        for open_search in [false, true] {
            for key in [NamedKey::Enter, NamedKey::Space] {
                let mut app =
                    App::new_with_controlled_inputs(State::new(), Box::new(ConsumingKeyboardRoot));
                app.enable_document_history_chrome();
                app.init_layout();
                let layout = app.history_layout.unwrap();
                let target = if open_search {
                    layout.search.unwrap()
                } else {
                    layout.toolbar
                };
                app.set_focused_node(None);
                let mut tab = synth_enter();
                if let WindowEvent::KeyboardInput { event, .. } = &mut tab {
                    event.logical_key = Key::Named(NamedKey::Tab);
                }
                // Exercise the real native Tab event route, rather than assigning
                // the target directly or activating the Button as an AT action.
                for _ in 0..app.focusable_order.len() {
                    assert!(app.handle_tab_navigation(&tab, None));
                    if app.focused_node == Some(target) {
                        break;
                    }
                }
                assert_eq!(app.focused_node, Some(target), "Tab reaches host control");
                let ime = WindowEvent::Ime(winit::event::Ime::Commit("composed text".into()));
                let mut handled = false;
                let mut focus = None;
                assert!(app.presentation_handle_event(&ime, 1.0, &mut handled, &mut focus));
                assert!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .is_none(),
                    "IME input focused on host chrome must never reach app content"
                );
                let mut event = synth_enter();
                if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                    event.logical_key = Key::Named(key);
                }
                let mut handled = false;
                let mut focus = None;
                assert!(app.presentation_handle_event(&event, 1.0, &mut handled, &mut focus));
                assert!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .is_none(),
                    "focused chrome input must never reach consuming app content"
                );
                assert!(!handled, "Button leaves activation to the native fallback");
                app.handle_unconsumed_activation_key(&event, &mut handled);
                assert!(handled);
                assert_eq!(app.dispatch_control_invocations(), 1);
                assert_eq!(app.action_palette().is_some(), open_search);
                assert_eq!(app.document_history.is_open(), !open_search);
                assert!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .is_none()
                );
                // Positive control: the same fixture consumes and mutates when
                // its own app node receives this event with chrome unfocused.
                app.set_focused_node(Some(layout.app));
                let mut handled = false;
                app.presentation_handle_event(&event, 1.0, &mut handled, &mut focus);
                assert!(handled);
                assert_eq!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .and_then(|(value, _)| value.to_bool()),
                    Some(true)
                );
                app.state.doc.delete(&ROOT, "escaped_keyboard").unwrap();
                let mut handled = false;
                app.presentation_handle_event(&ime, 1.0, &mut handled, &mut focus);
                assert!(
                    handled,
                    "app-focused IME reaches the same consuming fixture"
                );
                assert_eq!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .and_then(|(value, _)| value.to_bool()),
                    Some(true)
                );
            }
        }
    }

    #[test]
    fn visible_search_uses_existing_palette_and_fresh_authority() {
        for revoke in [false, true] {
            let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
            app.action_registry_mut().register::<Flip>(FLIP_TYPE);
            app.enable_document_history_chrome();
            app.init_layout();
            let node = app
                .history_layout
                .unwrap()
                .search
                .expect("retained Search button");
            assert!(app.is_history_node(node), "Search is admitted local chrome");
            let button = app
                .document_history
                .search_mut()
                .expect("palette-capable host shows Search");
            assert_eq!(button.test_id(), Some("standalone-search-open"));
            assert!(button.perform_action(
                &crate::a11y::Action::Activate,
                &mut app.state.doc,
                &app.state.root
            ));
            assert_eq!(app.dispatch_history_controls(), 1);
            assert!(
                app.action_palette().is_some(),
                "real semantic button opens existing palette"
            );
            app.actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_query("Flip it");
            assert!(
                app.action_palette()
                    .unwrap()
                    .result_labels()
                    .iter()
                    .any(|label| label.contains("Flip it"))
            );
            if revoke {
                app.set_permission_ctx(PermissionContext::from_capabilities(
                    collab_types::caps::Actor::person(None, "restricted"),
                    Vec::new(),
                ));
            }
            assert!(app.action_palette_handle_key(&synth_enter()));
            assert_eq!(
                app.state.doc.get(&ROOT, "flipped").unwrap().is_some(),
                !revoke,
                "activation re-resolves current permission rather than cached row authority"
            );
            assert!(
                !app.collect_resources()
                    .iter()
                    .any(|resource| resource.id.as_str()
                        == crate::standalone_history_host::SEARCH_RESOURCE),
                "host Search never becomes an application/broker resource"
            );
            let address = tilo_actions::ActionAddress::new(
                crate::standalone_history_host::SEARCH_RESOURCE,
                "open",
            );
            assert!(
                app.invoke_resource_action(
                    &address,
                    None,
                    "same-local-name",
                    false,
                    true,
                    None,
                    None,
                    None
                )
                .is_err(),
                "broker admission and matching actor names do not grant host UI locality"
            );
        }
    }

    #[test]
    fn ctrl_k_scopes_to_resources_then_dispatch_mutates_doc_and_records_undo() {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);

        let res = app.collect_resources();
        assert_eq!(res.len(), 1, "one open resource");
        assert_eq!(res[0].kind, "test.obj");

        // Scopes the palette to those resources AND folds in the widget's
        // searchable content â€” one surface, both kinds.
        app.open_action_palette();
        let ap = app.action_palette().expect("palette is open");
        assert!(ap.is_open());
        let labels = ap.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Flip it")),
            "palette lists the widget's action: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "Ada Lovelace"),
            "palette folds in the widget's searchable content: {labels:?}"
        );

        assert!(app.state.doc.get(&ROOT, "flipped").unwrap().is_none());
        assert!(app.state.peek_last_undo().is_none());
        app.needs_sync = false;
        app.layout.layout_dirty = false;

        // Enter selects the top row (the action) and dispatches it.
        let consumed = app.action_palette_handle_key(&synth_enter());
        assert!(consumed, "the open palette consumed the Enter key");

        let flipped = app
            .state
            .doc
            .get(&ROOT, "flipped")
            .unwrap()
            .and_then(|(v, _)| v.to_bool());
        assert_eq!(
            flipped,
            Some(true),
            "dispatched action set the flag in the CRDT"
        );
        assert!(
            app.state.peek_last_undo().is_some(),
            "the dispatched action is undoable"
        );
        assert!(app.needs_sync, "dispatch marks the app for re-sync");
        assert!(
            app.layout.layout_dirty,
            "dispatch marks the app for relayout"
        );
        assert!(
            app.action_palette().is_none(),
            "palette closed after selection"
        );
    }

    #[test]
    fn reopening_palette_reuses_live_resource_index_until_state_mutates() {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);

        app.open_action_palette();
        let first = app
            .actions
            .open_resource_search_cache
            .borrow()
            .as_ref()
            .expect("first open builds the live-resource index")
            .provider
            .clone();
        app.close_action_palette();
        app.open_action_palette();
        let second = app
            .actions
            .open_resource_search_cache
            .borrow()
            .as_ref()
            .expect("second open retains the index")
            .provider
            .clone();
        assert!(first.shares_storage_with(&second));

        app.invalidate_widget_resource_cache();
        assert!(app.actions.open_resource_search_cache.borrow().is_none());
    }

    #[test]
    fn synthesized_semantic_action_routes_back_to_its_visible_control() {
        let mut app = App::new(
            State::new(),
            Box::new(Button::new("complete", "Complete", Some("completed"))),
        );
        app.init_layout();
        let resource = app
            .collect_resources()
            .into_iter()
            .find(|resource| {
                resource.kind == "tilo.ui.control"
                    && resource.label == "Complete"
                    && resource
                        .actions
                        .iter()
                        .any(|action| action.id == "activate")
            })
            .expect("collaborative button publishes a semantic action");
        let address = tilo_actions::ActionAddress::new(resource.id.as_str(), "activate");
        let route = app
            .preferred_action_ui_route(&address)
            .expect("semantic action resolves to visible UI");

        assert_eq!(route.control_id, "complete");
        assert!(route.bounds.is_some());
        assert!(!route.disabled);
    }

    #[test]
    fn exact_widget_palette_request_resolves_only_declared_actions() {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        assert!(
            app.open_action_palette_at(&tilo_actions::ActionAddress::new("test.obj:1", "flip"))
        );
        assert!(
            !app.open_action_palette_at(&tilo_actions::ActionAddress::new(
                "test.obj:1",
                "not-declared"
            )),
            "a stale or invented widget request is refused"
        );
        assert!(
            app.action_palette().is_none(),
            "a refused exact request does not leave an unrelated palette open"
        );
    }

    /// A content row does NOT dispatch â€” it hands its id back through
    /// [`Widget::activate_search_result`] instead.
    #[test]
    fn ctrl_k_content_row_navigates_through_the_app() {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("ada");
        let labels = app.action_palette().unwrap().result_labels();
        assert!(
            labels.iter().any(|l| l == "Ada Lovelace"),
            "content hit present: {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l.contains("Flip it")),
            "the action row is filtered out by the query: {labels:?}"
        );

        assert!(app.state.doc.get(&ROOT, "navigated").unwrap().is_none());
        app.needs_sync = false;
        app.layout.layout_dirty = false;

        let consumed = app.action_palette_handle_key(&synth_enter());
        assert!(consumed, "the open palette consumed the Enter key");

        let nav = app
            .state
            .doc
            .get(&ROOT, "navigated")
            .unwrap()
            .and_then(|(v, _)| v.to_str().map(str::to_string));
        assert_eq!(
            nav,
            Some("person:Ada Lovelace".to_string()),
            "the content row's id reached the app to navigate"
        );
        assert!(
            app.action_palette().is_none(),
            "navigation closes the palette"
        );
        assert!(
            app.needs_sync && app.layout.layout_dirty,
            "navigation marks a redraw"
        );
    }

    #[test]
    fn denied_lazy_search_action_never_crosses_the_host_permission_door() {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("restricted");
        // A query advances one provider page immediately; remaining providers
        // (including the host's open resources) advance on later frames.
        // Bound the drain so a pagination regression still fails this test.
        let mut progress = app
            .actions
            .action_palette
            .as_mut()
            .unwrap()
            .advance_search_page();
        for _ in 0..8 {
            if !progress.pending {
                break;
            }
            progress = app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .advance_search_page();
        }
        assert!(!progress.pending, "the tiny provider set must be exhausted");
        assert_eq!(
            app.action_palette().unwrap().result_labels(),
            vec!["Restricted lazy action"]
        );

        assert!(app.action_palette_handle_key(&synth_enter()));
        assert!(
            app.state.doc.get(&ROOT, "lazy-flipped").unwrap().is_none(),
            "the provider hit cannot bypass the host's current permission context"
        );
        assert!(
            app.action_palette().is_some(),
            "a refused row leaves search open instead of pretending it ran"
        );
    }

    #[test]
    fn projected_view_preserves_pending_edit_and_eventual_attribution() {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(Button::new("focus-me", "Focus me", None)),
        );
        app.init_layout();
        app.set_focused_node(None);
        assert!(!app.root_widget.is_focused());
        let before = app.state.doc.get_heads();
        app.state.doc.put(ROOT, "draft", "unfinished").unwrap();
        let pending = app.state.doc.pending_ops();
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("focus");
        assert!(app.action_palette_handle_key(&synth_enter()));
        assert!(app.root_widget.is_focused());
        assert_eq!(app.state.doc.pending_ops(), pending);
        assert_eq!(app.state.doc.base_heads(), before);
        assert!(app.state.peek_last_undo().is_none());

        app.state.doc.put(ROOT, "draft", "finished").unwrap();
        let context = tilo_actions::ActionContext::for_resource(
            "Finish draft",
            "test.draft",
            "note",
            "Draft",
        );
        assert!(app.state.commit_action_authored_with_context(
            None,
            None,
            Some(app.collaboration.local_author.clone()),
            Some(context),
        ));
        let changes = app.state.doc.get_changes(&before);
        assert_eq!(changes.len(), 1, "view must not split the pending edit");
        let context = changes[0]
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
            .unwrap();
        assert_eq!(context.action_label, "Finish draft");
    }

    #[test]
    fn ctrl_k_invokes_a_projected_semantic_view_control() {
        let button = Button::new("drive-me", "Drive me", None);
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(button));
        app.init_layout();
        app.set_focused_node(None);
        assert!(!app.root_widget.is_focused());
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("focus");

        assert!(app.action_palette_handle_key(&synth_enter()));
        assert!(
            app.root_widget.is_focused(),
            "Ctrl+K reaches the real retained control"
        );
    }
}

// See `crate::view` for the (de)serialize-as-a-whole design this implements.
impl crate::view::FreezeThaw for App {
    /// `AutoCommit::save` takes `&mut self` to flush its open transaction;
    /// freeze is `&self` on a cold path, so save a clone rather than widen
    /// the signature.
    fn serialize_state(&self) -> Vec<u8> {
        self.state.doc.clone().save()
    }

    /// Also resets the undo history â€” the old stacks name changes this
    /// document has never seen.
    fn restore_state(&mut self, bytes: &[u8]) -> Result<(), crate::view::ThawError> {
        let doc = automerge::AutoCommit::load(bytes)
            .map_err(|e| crate::view::ThawError::State(e.to_string()))?;
        self.state.adopt_doc(doc);
        Ok(())
    }

    fn capture_view(&self) -> crate::view::View {
        let _entry = self.enter_wake_scope();
        let ordinal = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        let focus = crate::view::capture_focus(ordinal, || {
            self.focused_node.and_then(|id| {
                self.inspect_node(id, |widget| widget.get_state())
                    .ok()
                    .flatten()
            })
        });
        let viewport = self
            .layout
            .last_frame_size
            .map(|s| crate::view::ViewRect {
                x: 0.0,
                y: 0.0,
                width: f64::from(s.width),
                height: f64::from(s.height),
            })
            .unwrap_or_default();
        crate::view::capture_widget_view_bound(&*self.root_widget, focus, viewport)
    }

    /// `open_resource` is carried for presence/reboot to reopen the record;
    /// record-navigation itself is the host's and is not driven here.
    fn restore_view(&mut self, view: &crate::view::View) {
        let _entry = self.enter_wake_scope();
        crate::view::restore_widget_view_bound(&mut *self.root_widget, view);

        if let Some(f) = &view.focus
            && let Some(&id) = self.focusable_order.get(f.ordinal)
        {
            self.set_focused_node(Some(id));
            let _ = self.mutate_node(id, |widget| {
                crate::view::restore_focus_state_bound(widget, f);
            });
        }

        self.layout.layout_dirty = true;
        self.needs_sync = true;
    }
}

// View presence: exercised on the real broadcast/receive path â€”
// `broadcast_view_presence` through the presence sink, and
// `apply_collab_event_for_test` through `apply_remote_presence`.
#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "serialized presence fields must round-trip exactly"
)]
mod view_presence_tests {
    use super::*;
    use crate::PresenceCtx;
    use crate::TextRenderer;
    use crate::Theme;
    use crate::record::{RecordCheckout, RecordId, RecordKind};
    use crate::state::State;
    use crate::view::{FreezeThaw, ViewContribution, ViewSubset};
    use crate::widget::Widget;
    use automerge::{AutoCommit, ObjId};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use taffy::TaffyTree;
    use vello::Scene;
    use vello::kurbo::{Affine, Point, Rect as KurboRect};
    use winit::event::WindowEvent;

    /// No focus field â€” [`ViewSubset`] excludes the caret.
    struct ViewRoot {
        scroll: Vec<f64>,
        mode: Option<String>,
        resource: Option<String>,
        rect: KurboRect,
    }

    impl ViewRoot {
        fn new() -> Self {
            Self {
                scroll: Vec::new(),
                mode: None,
                resource: None,
                rect: KurboRect::ZERO,
            }
        }
    }

    impl Widget for ViewRoot {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: taffy::NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        #[allow(
            clippy::too_many_arguments,
            reason = "implementation must match the shared Widget event signature"
        )]
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut AutoCommit,
            _p: &ObjId,
            _h: &mut bool,
            _ft: &mut Option<taffy::NodeId>,
            _ni: taffy::NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "view-root"
        }
        fn get_rect(&self) -> KurboRect {
            self.rect
        }
        fn view_state(&self) -> Option<ViewContribution> {
            Some(ViewContribution {
                scroll: self.scroll.clone(),
                view_mode: self.mode.clone(),
            })
        }
        fn restore_view_state(&mut self, view: &ViewContribution) {
            self.scroll = view.scroll.clone();
            self.mode = view.view_mode.clone();
        }
        fn record_intent(&self) -> Option<RecordCheckout> {
            let r = self.resource.as_ref()?;
            RecordId::parse(r).map(|id| RecordCheckout::new(id, RecordKind::new("document")))
        }
    }

    fn app_with_root(root: ViewRoot) -> App {
        let mut app = App::new(State::new(), Box::new(root));
        app.init_layout();
        app
    }

    #[test]
    fn application_launch_receipts_are_terminal_once_and_late_replies_are_inert() {
        let mut app = app_with_root(ViewRoot::new());
        for (request_id, update) in [
            (
                1,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: tilo_actions::InvocationId::new(),
                    output: tilo_actions::ActionOutput::default(),
                },
            ),
            (
                2,
                tilo_actions::InvocationUpdate::Failed {
                    invocation: tilo_actions::InvocationId::new(),
                    error: tilo_actions::ActionFailure {
                        code: "missing_hash".into(),
                        message: "launch content is unavailable on this device".into(),
                        details: None,
                        retryable: false,
                    },
                },
            ),
            (
                3,
                tilo_actions::InvocationUpdate::Cancelled {
                    invocation: tilo_actions::InvocationId::new(),
                    message: Some("launcher refused".into()),
                },
            ),
        ] {
            let invocation = tilo_actions::InvocationId::new();
            app.actions
                .pending_application_launches
                .insert(request_id, invocation);
            app.actions.directed_invocations.insert(
                invocation,
                DirectedInvocation {
                    wire_id: request_id + 100,
                    cancellation_requested: false,
                },
            );
            app.apply_collab_event_for_test(CollabEvent::PublishedInvocationResult {
                request_id,
                result: Ok((request_id + 10, update)),
            });
            assert!(
                !app.actions
                    .pending_application_launches
                    .contains_key(&request_id)
            );
            assert!(!app.actions.directed_invocations.contains_key(&invocation));
            // A duplicate broker receipt cannot emit a second terminal update.
            app.apply_collab_event_for_test(CollabEvent::PublishedInvocationResult {
                request_id,
                result: Err("late duplicate".into()),
            });
            assert!(!app.actions.directed_invocations.contains_key(&invocation));
        }
    }

    #[test]
    fn disconnected_application_launches_are_bounded_and_failed_once() {
        let mut app = app_with_root(ViewRoot::new());
        let invocation = tilo_actions::InvocationId::new();
        app.actions
            .pending_application_launches
            .insert(1, invocation);
        app.actions.directed_invocations.insert(
            invocation,
            DirectedInvocation {
                wire_id: 9,
                cancellation_requested: false,
            },
        );
        assert!(app.poll_directed_invocations());
        assert!(app.actions.pending_application_launches.is_empty());
        assert!(!app.actions.directed_invocations.contains_key(&invocation));
    }

    #[test]
    fn application_launch_admission_refuses_a_full_receipt_queue() {
        let mut app = app_with_root(ViewRoot::new());
        for request_id in 0..MAX_PENDING_APPLICATION_LAUNCHES as u64 {
            app.actions
                .pending_application_launches
                .insert(request_id, tilo_actions::InvocationId::new());
        }

        assert!(matches!(
            app.admit_application_launch(
                MAX_PENDING_APPLICATION_LAUNCHES as u64,
                tilo_actions::InvocationId::new(),
            ),
            Err(tilo_actions::DoorError::Unavailable { code, .. })
                if code == "application_launch_queue_full"
        ));
        assert_eq!(
            app.actions.pending_application_launches.len(),
            MAX_PENDING_APPLICATION_LAUNCHES
        );
    }

    #[test]
    fn generic_continuation_restores_view_for_apps_without_custom_metadata() {
        let mut sender = app_with_root(ViewRoot {
            scroll: vec![48.0, 12.0],
            mode: Some("grid".into()),
            resource: Some("record:stable-photo".into()),
            rect: KurboRect::ZERO,
        });
        sender.layout.last_frame_size = Some(winit::dpi::PhysicalSize::new(900, 600));
        let view = <App as FreezeThaw>::capture_view(&sender);
        let metadata = crate::collaboration::ContinuationMetadata {
            schema_version: GENERIC_VIEW_CONTINUATION_SCHEMA,
            bytes: postcard::to_stdvec(&PortableContinuationView::capture(view)).unwrap(),
        };

        // Model launcher delivery before first layout, which is the real
        // continuation order. The view waits until focus/layout nodes exist.
        let mut receiver = App::new(State::new(), Box::new(ViewRoot::new()));
        receiver.continuation.pending_restore = Some(metadata);
        receiver.apply_pending_continuation_restore();
        assert!(receiver.continuation.pending_view.is_some());
        receiver.init_layout();

        let restored = <App as FreezeThaw>::capture_view(&receiver);
        assert_eq!(restored.scroll, vec![48.0, 12.0]);
        assert_eq!(restored.view_mode.as_deref(), Some("grid"));
        assert!(receiver.continuation.pending_view.is_none());
    }

    #[test]
    fn generic_continuation_never_serializes_path_or_widget_ephemeral_state() {
        let portable = PortableContinuationView::capture(crate::view::View {
            scroll: vec![1.0],
            focus: Some(crate::view::FocusView {
                ordinal: 2,
                ephemeral: "/home/alice/private-token".into(),
            }),
            open_resource: Some("file:/home/alice/private.txt".into()),
            viewport: crate::view::ViewRect::default(),
            view_mode: Some("grid".into()),
        });
        let bytes = postcard::to_stdvec(&portable).unwrap();
        assert!(!bytes.windows(5).any(|window| window == b"/home"));
        let restored = portable.into_view();
        assert_eq!(restored.open_resource, None);
        assert_eq!(restored.focus.unwrap().ephemeral, "");
    }

    #[test]
    fn custom_semantics_cannot_replace_framework_view_fidelity() {
        let view = crate::view::View {
            scroll: vec![81.0, 13.0],
            focus: Some(crate::view::FocusView {
                ordinal: 3,
                ephemeral: "private-editor-state".into(),
            }),
            open_resource: Some("file:/private/document".into()),
            viewport: crate::view::ViewRect {
                x: 1.0,
                y: 2.0,
                width: 1280.0,
                height: 720.0,
            },
            view_mode: Some("reader".into()),
        };
        let metadata = compose_structural_continuation_metadata(
            view,
            Some(crate::collaboration::ContinuationMetadata {
                schema_version: 7,
                bytes: b"custom-safe-projection".to_vec(),
            }),
        )
        .expect("framework envelope");

        assert_eq!(metadata.schema_version, STRUCTURAL_CONTINUATION_SCHEMA);
        let facets = structural_continuation_facets(&metadata.bytes).expect("valid facets");
        assert!(facets.presentation());
        assert!(facets.semantic());
        let envelope = postcard::from_bytes::<StructuralContinuationEnvelope>(&metadata.bytes)
            .expect("structural envelope");
        assert_eq!(envelope.presentation.scroll, vec![81.0, 13.0]);
        assert_eq!(envelope.presentation.focus_ordinal, Some(3));
        assert_eq!(envelope.presentation.view_mode.as_deref(), Some("reader"));
        assert!(!metadata.bytes.windows(7).any(|window| window == b"private"));
    }

    #[test]
    fn unsafe_custom_semantics_are_stripped_without_losing_the_view() {
        let metadata = compose_structural_continuation_metadata(
            crate::view::View {
                scroll: vec![42.0],
                focus: None,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some("grid".into()),
            },
            Some(crate::collaboration::ContinuationMetadata {
                schema_version: 1,
                bytes: b"opaque-unknown-payload".to_vec(),
            }),
        )
        .unwrap();
        let sanitized = sanitize_structural_continuation_metadata(&metadata.bytes, |_, _| false)
            .expect("valid presentation survives");
        let facets = structural_continuation_facets(&sanitized).unwrap();
        assert!(facets.presentation());
        assert!(!facets.semantic());
        let envelope = postcard::from_bytes::<StructuralContinuationEnvelope>(&sanitized).unwrap();
        assert_eq!(envelope.presentation.scroll, vec![42.0]);
        assert_eq!(envelope.presentation.view_mode.as_deref(), Some("grid"));
        assert!(!sanitized.windows(6).any(|window| window == b"opaque"));
    }

    #[test]
    fn oversized_custom_semantics_cannot_suppress_framework_view_fidelity() {
        let metadata = compose_structural_continuation_metadata(
            crate::view::View {
                scroll: vec![7.0],
                focus: None,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some("list".into()),
            },
            Some(crate::collaboration::ContinuationMetadata {
                schema_version: 1,
                bytes: vec![b'x'; 32 * 1024],
            }),
        )
        .expect("presentation-only fallback");
        let facets = structural_continuation_facets(&metadata.bytes).unwrap();
        assert!(facets.presentation());
        assert!(!facets.semantic());
    }

    #[test]
    fn structural_continuation_restores_framework_view_before_layout() {
        let metadata = compose_structural_continuation_metadata(
            crate::view::View {
                scroll: vec![64.0],
                focus: None,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some("list".into()),
            },
            None,
        )
        .unwrap();
        let mut receiver = App::new(State::new(), Box::new(ViewRoot::new()));
        receiver.continuation.pending_restore = Some(metadata);
        receiver.apply_pending_continuation_restore();
        assert!(receiver.continuation.pending_view.is_some());
        receiver.init_layout();
        let restored = <App as FreezeThaw>::capture_view(&receiver);
        assert_eq!(restored.scroll, vec![64.0]);
        assert_eq!(restored.view_mode.as_deref(), Some("list"));
    }

    /// Asserted on both the frame that leaves the sender (via the broadcast
    /// sink) and the frame the receiver applies.
    #[test]
    fn view_subset_broadcasts_through_real_presence_path() {
        #[cfg(windows)]
        let file_record = r"file:C:\report.md";
        #[cfg(not(windows))]
        let file_record = "file:/report.md";
        let mut sender = app_with_root(ViewRoot {
            scroll: vec![10.0, 20.0],
            mode: Some("grid".into()),
            resource: Some(file_record.into()),
            rect: KurboRect::ZERO,
        });
        sender.layout.last_frame_size = Some(winit::dpi::PhysicalSize::new(1024, 768));

        let captured: Arc<Mutex<Vec<(PresenceKey, PresencePayload)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        sender.set_presence_broadcast_sink(move |k, p| {
            sink.lock().unwrap().push((k.clone(), p.clone()));
        });

        sender.broadcast_view_presence();

        let frames = captured.lock().unwrap().clone();
        assert_eq!(frames.len(), 1, "exactly one View frame is broadcast");
        let (key, _payload) = &frames[0];
        let sent = key
            .as_view()
            .expect("the broadcast key carries a decodable View subset");
        assert_eq!(sent.scroll, vec![10.0, 20.0], "scroll rode the frame");
        assert_eq!(
            sent.open_resource.as_deref(),
            Some(file_record),
            "the open resource rode the frame"
        );
        assert_eq!(
            sent.view_mode.as_deref(),
            Some("grid"),
            "view mode rode the frame"
        );
        assert_eq!(sent.viewport.width, 1024.0, "viewport width rode the frame");
        assert_eq!(
            sent.viewport.height, 768.0,
            "viewport height rode the frame"
        );

        let mut receiver = app_with_root(ViewRoot::new());
        assert!(
            receiver
                .presence_ctx()
                .peer_view(&crate::permissions::UserId("alice".into()))
                .is_none(),
            "no View known for the peer before their broadcast arrives"
        );
        receiver.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "alice".into(),
            key: key.clone(),
            payload: PresencePayload::Arrived,
        });

        let peer_view = receiver
            .presence_ctx()
            .peer_view(&crate::permissions::UserId("alice".into()))
            .expect("the peer's View is stored after their broadcast");
        assert_eq!(
            peer_view, &sent,
            "the receiver stores the peer's exact View subset"
        );

        // The inert carrier payload must not be processed as a real Arrived.
        assert!(
            receiver.presence_ctx().remote_cursors.is_empty(),
            "a View frame must not register a phantom cursor"
        );
    }

    #[test]
    fn follow_binds_local_viewport_and_unfollow_releases() {
        fn deliver(app: &mut App, peer: &str, scroll: Vec<f64>, mode: &str) {
            let subset = ViewSubset {
                scroll,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some(mode.into()),
            };
            app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
                user_id: peer.into(),
                key: PresenceKey::view(&subset),
                payload: PresencePayload::Arrived,
            });
        }

        let mut app = app_with_root(ViewRoot::new());

        deliver(&mut app, "bob", vec![100.0], "list");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            Vec::<f64>::new(),
            "receiving a peer's View must not move the local view on its own"
        );

        app.follow("bob");
        let v = <App as FreezeThaw>::capture_view(&app);
        assert_eq!(
            v.scroll,
            vec![100.0],
            "follow moved the local view to the peer's scroll"
        );
        assert_eq!(
            v.view_mode.as_deref(),
            Some("list"),
            "follow adopted the peer's view mode"
        );

        deliver(&mut app, "bob", vec![250.0], "board");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            vec![250.0],
            "a followed peer's later View keeps moving the local view"
        );

        app.unfollow();
        deliver(&mut app, "bob", vec![999.0], "grid");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            vec![250.0],
            "after unfollow the local view is released and stops tracking"
        );
    }

    #[test]
    fn following_absent_peer_is_a_noop() {
        let mut app = app_with_root(ViewRoot {
            scroll: vec![7.0],
            mode: Some("grid".into()),
            resource: None,
            rect: KurboRect::ZERO,
        });

        app.follow("ghost");

        let v = <App as FreezeThaw>::capture_view(&app);
        assert_eq!(
            v.scroll,
            vec![7.0],
            "following an absent peer leaves the local view untouched"
        );
        assert_eq!(
            app.followed_peer(),
            Some("ghost"),
            "the follow intent is still latched"
        );
    }
}

#[cfg(test)]
mod semantic_tree_app_tests {
    use super::*;
    use crate::a11y_tree::{SemanticActionKind, SemanticId};
    use crate::state::State;
    use crate::widget::{Button, Widget};
    use automerge::transaction::Transactable;
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use tilo_actions::{
        Action as QAction, ActionError, ActionSpec, Doc, EncodedAction, ExecutionLocus,
        OperationKind, PermissionRequirement, Resource,
    };

    #[derive(Debug, Clone, Serialize, Deserialize)]
    enum CounterAction {
        Bump,
        Reset,
        HostBump,
        SetLabel(String),
    }
    const COUNTER_TYPE: &str = "test.counter";
    const ASYNC_COUNTER_TYPE: &str = "test.async-counter";

    #[derive(Clone)]
    struct LargeDirectoryProvider {
        catalog: crate::search::SearchCatalog,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        largest_budget: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl LargeDirectoryProvider {
        fn new(records: usize) -> Self {
            let catalog = crate::search::SearchCatalog::new("Large directory");
            for index in 0..records {
                let resource = format!("test.item:{index}");
                let address = tilo_actions::ActionAddress::new(resource.clone(), "open");
                catalog
                    .upsert(
                        resource,
                        format!("record {index} needle-{index}"),
                        crate::search::SearchResult::for_action(
                            "Large directory",
                            index.to_string(),
                            address,
                        ),
                    )
                    .expect("canonical synthetic directory row");
            }
            Self {
                catalog,
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                largest_budget: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn resource_for(address: &tilo_actions::ActionAddress) -> Option<Resource> {
            address
                .resource
                .strip_prefix("test.item:")?
                .parse::<usize>()
                .ok()?;
            Some(
                Resource::new(address.resource.clone(), "test.item", "Synthetic record")
                    .with_action(
                        ActionSpec::operation(
                            "open",
                            "Open",
                            OperationKind::View,
                            COUNTER_TYPE,
                            &CounterAction::Bump,
                        )
                        .ok()?
                        .with_contract(tilo_actions::ActionContract::view()),
                    ),
            )
        }
    }

    impl crate::search::SearchProvider for LargeDirectoryProvider {
        fn name(&self) -> &'static str {
            "Large directory"
        }

        fn action_directory_generation(&self) -> Option<u64> {
            Some(self.catalog.stats().generation)
        }

        fn action_directory_cursor_is_current(&self, cursor: &crate::search::SearchCursor) -> bool {
            self.catalog.cursor_is_current(cursor)
        }

        fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
            self.catalog.search(query)
        }

        fn search_page(
            &self,
            query: &str,
            cursor: Option<&crate::search::SearchCursor>,
            work_budget: usize,
            result_limit: usize,
        ) -> crate::search::SearchPage {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.largest_budget
                .fetch_max(work_budget, std::sync::atomic::Ordering::Relaxed);
            self.catalog
                .query_page(query, cursor, work_budget, result_limit)
        }

        fn resolve_action_resource(
            &self,
            address: &tilo_actions::ActionAddress,
        ) -> Option<Resource> {
            Self::resource_for(address)
        }
    }

    impl QAction for CounterAction {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            match self {
                CounterAction::Bump | CounterAction::HostBump => {
                    let n = match doc.get(&ROOT, "n") {
                        Ok(Some((v, _))) => v.to_i64().unwrap_or(0),
                        _ => 0,
                    };
                    doc.put(&ROOT, "n", ScalarValue::Int(n.saturating_add(1)))?;
                }
                CounterAction::Reset => {
                    doc.put(&ROOT, "n", ScalarValue::Int(0))?;
                }
                CounterAction::SetLabel(s) => {
                    doc.put(&ROOT, "label", s.as_str())?;
                }
            }
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            if matches!(self, CounterAction::HostBump) {
                ExecutionLocus::ServerPreferred
            } else {
                ExecutionLocus::ClientLocal
            }
        }
    }

    /// One open resource exporting an immediate `bump` and a prompted
    /// `set-label`.
    struct CounterCard {
        button: Button,
        destructive: bool,
        lazy_resource: Option<Resource>,
        directory_provider: Option<LargeDirectoryProvider>,
        directory_roots: usize,
        post_bump_reset: bool,
        async_bump: bool,
        pending_bump: Option<tilo_actions::InvocationId>,
        resource_result: Option<tilo_actions::InvocationUpdate>,
    }

    fn counter_resource(destructive: bool) -> Resource {
        counter_resource_with_reset(destructive, false)
    }

    fn counter_resource_with_reset(destructive: bool, reset_visible: bool) -> Resource {
        let bump = ActionSpec::immediate("bump", "Bump", COUNTER_TYPE, &CounterAction::Bump)
            .unwrap()
            .requiring(PermissionRequirement::Write);
        let bump = if destructive {
            bump.destructive()
        } else {
            bump
        };
        let resource = Resource::new("test.counter", "test.counter", "Counter")
            .with_action(bump)
            .with_action(
                ActionSpec::immediate(
                    "host-bump",
                    "Host bump",
                    COUNTER_TYPE,
                    &CounterAction::HostBump,
                )
                .unwrap()
                .requiring(PermissionRequirement::Write),
            )
            .with_action(
                ActionSpec::operation(
                    "inspect",
                    "Inspect counter",
                    OperationKind::View,
                    COUNTER_TYPE,
                    &CounterAction::Bump,
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract::view()),
            )
            .with_action(
                ActionSpec::operation(
                    "ping",
                    "Ping service",
                    OperationKind::Effect,
                    COUNTER_TYPE,
                    &CounterAction::Bump,
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract::default()),
            )
            .with_action(ActionSpec::prompted(
                "set-label",
                "Set label",
                "Label",
                "nameâ€¦",
                ExecutionLocus::ClientLocal,
                |s: &str| {
                    EncodedAction::encode(COUNTER_TYPE, &CounterAction::SetLabel(s.to_string()))
                        .unwrap()
                },
            ));
        if reset_visible {
            resource.with_action(
                ActionSpec::immediate("reset", "Reset", COUNTER_TYPE, &CounterAction::Reset)
                    .unwrap()
                    .requiring(PermissionRequirement::Write),
            )
        } else {
            resource
        }
    }

    impl CounterCard {
        fn new() -> Self {
            Self::with_destructive(false)
        }

        fn with_destructive(destructive: bool) -> Self {
            let resource = counter_resource(destructive);
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "bump").unwrap();
            Self {
                button: Button::new("bump", "Bump", None).on_action(binding),
                destructive,
                lazy_resource: None,
                directory_provider: None,
                directory_roots: 1,
                post_bump_reset: false,
                async_bump: false,
                pending_bump: None,
                resource_result: None,
            }
        }

        fn with_post_bump_reset() -> Self {
            let mut card = Self::new();
            card.post_bump_reset = true;
            card
        }

        fn with_async_post_bump_reset() -> Self {
            let mut card = Self::with_post_bump_reset();
            card.async_bump = true;
            card
        }

        fn with_lazy_resource(resource: Resource) -> Self {
            Self {
                button: Button::new("lazy-resource", "Lazy resource", None),
                destructive: false,
                lazy_resource: Some(resource),
                directory_provider: None,
                directory_roots: 1,
                post_bump_reset: false,
                async_bump: false,
                pending_bump: None,
                resource_result: None,
            }
        }

        fn with_directory_provider(provider: LargeDirectoryProvider) -> Self {
            let mut card = Self::new();
            card.directory_provider = Some(provider);
            card
        }

        fn with_directory_roots(count: usize) -> Self {
            let mut card = Self::new();
            card.directory_roots = count;
            card
        }

        fn current_resource(&self, doc: &automerge::AutoCommit) -> Resource {
            let resource = counter_resource_with_reset(
                self.destructive,
                self.post_bump_reset && counter_in_doc(doc) > 0,
            );
            if self.async_bump {
                resource.with_action(
                    ActionSpec::operation(
                        "async-bump",
                        "Asynchronous bump",
                        OperationKind::Effect,
                        ASYNC_COUNTER_TYPE,
                        &(),
                    )
                    .unwrap()
                    .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
                    .returns(tilo_actions::ResultSpec::Job),
                )
            } else {
                resource
            }
        }
    }
    impl Widget for CounterCard {
        fn sync(&mut self, s: &mut automerge::AutoCommit, p: &automerge::ObjId) {
            self.button.sync(s, p);
            if let Some(invocation) = self.pending_bump.take() {
                CounterAction::Bump.apply(s).unwrap();
                self.resource_result = Some(tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                });
            }
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let kid = self.button.build_layout(taffy, theme, node_map);
            let node = taffy
                .new_with_children(taffy::Style::default(), &[kid])
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
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
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut automerge::AutoCommit,
            _p: &automerge::ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _ni: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            vec![&self.button]
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            vec![&mut self.button]
        }
        fn name(&self) -> &'static str {
            "CounterCard"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn resources(&self, doc: &automerge::AutoCommit) -> Vec<Resource> {
            self.lazy_resource
                .is_none()
                .then(|| self.current_resource(doc))
                .into_iter()
                .collect()
        }
        fn action_directory_resource_page(
            &self,
            doc: &automerge::AutoCommit,
            cursor: &[u8],
            max_results: usize,
        ) -> Option<crate::widget::ActionDirectoryResourcePage> {
            let offset = std::str::from_utf8(cursor)
                .ok()
                .and_then(|cursor| cursor.parse::<usize>().ok())
                .unwrap_or(0);
            let end = offset.saturating_add(max_results).min(self.directory_roots);
            let resources = (offset..end)
                .map(|index| {
                    if index == 0 {
                        self.current_resource(doc)
                    } else {
                        Resource::new(
                            format!("test.counter:{index}"),
                            "test.counter",
                            format!("Counter {index}"),
                        )
                        .with_action(
                            ActionSpec::operation(
                                "inspect",
                                "Inspect counter",
                                OperationKind::View,
                                COUNTER_TYPE,
                                &CounterAction::Bump,
                            )
                            .unwrap()
                            .with_contract(tilo_actions::ActionContract::view()),
                        )
                    }
                })
                .collect();
            Some(crate::widget::ActionDirectoryResourcePage {
                resources,
                next_cursor: end.to_string().into_bytes(),
                exhausted: end >= self.directory_roots,
            })
        }
        fn resource(&self, doc: &automerge::AutoCommit, id: &str) -> Option<Resource> {
            if let Some(resource) = self.lazy_resource.as_ref().filter(|resource| {
                find_action_resource(std::slice::from_ref(resource), id).is_some()
            }) {
                return Some(resource.clone());
            }
            if id == "test.counter" {
                return Some(self.current_resource(doc));
            }
            id.strip_prefix("test.counter:")
                .and_then(|suffix| suffix.parse::<usize>().ok())
                .filter(|index| *index < self.directory_roots)
                .map(|index| {
                    Resource::new(
                        format!("test.counter:{index}"),
                        "test.counter",
                        format!("Counter {index}"),
                    )
                    .with_action(
                        ActionSpec::operation(
                            "inspect",
                            "Inspect counter",
                            OperationKind::View,
                            COUNTER_TYPE,
                            &CounterAction::Bump,
                        )
                        .unwrap()
                        .with_contract(tilo_actions::ActionContract::view()),
                    )
                })
        }
        fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
            self.directory_provider
                .clone()
                .map(|provider| vec![Box::new(provider) as Box<dyn crate::search::SearchProvider>])
                .unwrap_or_default()
        }
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &[COUNTER_TYPE, ASYNC_COUNTER_TYPE]
        }
        fn start_resource_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            kind: tilo_actions::OperationKind,
            type_id: &str,
            payload: &[u8],
            doc: &automerge::AutoCommit,
            path: &automerge::ObjId,
        ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
            if self.async_bump && type_id == ASYNC_COUNTER_TYPE {
                self.pending_bump = Some(invocation);
                return Ok(Some(crate::widget::ResourceOperationStart::Accepted));
            }
            self.invoke_resource_operation_v2(kind, type_id, payload, doc, path)
                .map(|output| output.map(crate::widget::ResourceOperationStart::Completed))
        }
        fn poll_resource_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            _cancellation_requested: bool,
            _doc: &automerge::AutoCommit,
            _path: &automerge::ObjId,
        ) -> Option<tilo_actions::InvocationUpdate> {
            if self
                .resource_result
                .as_ref()
                .is_some_and(|update| update.invocation() == invocation)
            {
                self.resource_result.take()
            } else {
                None
            }
        }
        fn invoke_resource_operation_v2(
            &mut self,
            kind: tilo_actions::OperationKind,
            type_id: &str,
            _payload: &[u8],
            _doc: &automerge::AutoCommit,
            _path: &automerge::ObjId,
        ) -> Result<Option<tilo_actions::ActionOutput>, String> {
            Ok(
                (kind == tilo_actions::OperationKind::Effect && type_id == COUNTER_TYPE)
                    .then(tilo_actions::ActionOutput::default),
            )
        }
    }

    fn app() -> App {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(CounterCard::new()));
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        app
    }

    fn lazy_resource(id: &str) -> Resource {
        Resource::new(id, id, "Lazy counter").with_action(
            ActionSpec::immediate("bump", "Bump", COUNTER_TYPE, &CounterAction::Bump)
                .unwrap()
                .requiring(PermissionRequirement::Write),
        )
    }

    fn lazy_app(resource: Resource) -> App {
        let mut app = App::new(
            State::new(),
            Box::new(CounterCard::with_lazy_resource(resource)),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        app
    }

    fn counter(app: &App) -> i64 {
        counter_in_doc(&app.state.doc)
    }

    fn counter_in_doc(doc: &automerge::AutoCommit) -> i64 {
        match doc.get(&ROOT, "n") {
            Ok(Some((v, _))) => v.to_i64().unwrap_or(0),
            _ => 0,
        }
    }

    fn activate_bound_button(app: &mut App) {
        let node = app.focusable_order[0];
        let pointer = app.layout.node_map[&node];
        assert!(unsafe {
            (*pointer).perform_action(
                &crate::a11y::Action::Activate,
                &mut app.state.doc,
                &app.state.root,
            )
        });
    }

    #[test]
    fn lazy_resource_fallback_invokes_after_retained_cache_snapshot() {
        let resource = lazy_resource("test.lazy");
        let mut app = lazy_app(resource);
        let address = tilo_actions::ActionAddress::new("test.lazy", "bump");
        let snapshot = app.collect_resources();
        assert!(
            tilo_actions::door::resolve(&snapshot, &address).is_none(),
            "the retained snapshot must omit the lazy action"
        );
        assert!(
            app.actions.resource_cache.borrow().is_some(),
            "the snapshot is retained before fallback"
        );

        let result = app.invoke_resource_action(
            &address,
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );

        assert!(
            result.is_ok(),
            "the lazy resource should resolve: {result:?}"
        );
        assert_eq!(counter(&app), 1);
    }

    #[test]
    fn lazy_resource_fallback_respects_local_permissions() {
        let mut app = lazy_app(lazy_resource("test.lazy"));
        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));

        let denied = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.lazy", "bump"),
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(matches!(denied, Err(tilo_actions::DoorError::Unknown(_))));
        assert_eq!(counter(&app), 0, "the denied fallback must not run");

        app.set_permission_ctx(PermissionContext::permissive());
        let allowed = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.lazy", "bump"),
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(
            allowed.is_ok(),
            "restoring write permission should admit it"
        );
        assert_eq!(counter(&app), 1);
    }

    #[test]
    fn recursive_lazy_resource_remains_discoverable_and_invocable() {
        let child = lazy_resource("test.lazy.child");
        let tree =
            Resource::new("test.lazy.parent", "test.lazy.parent", "Parent").with_child(child);
        let mut app = lazy_app(tree);
        let resolved = crate::widget::resolve_widget_resource(
            app.root_widget.as_ref(),
            &app.state.doc,
            "test.lazy.child",
        )
        .expect("the recursive lazy child is discoverable");
        assert_eq!(
            find_action_resource(&[resolved], "test.lazy.child")
                .map(|resource| resource.id.as_str()),
            Some("test.lazy.child")
        );

        let result = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.lazy.child", "bump"),
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "the recursive lazy action should resolve: {result:?}"
        );
        assert_eq!(counter(&app), 1);
    }

    #[test]
    fn action_bound_control_uses_fresh_door_and_records_history() {
        let mut app = app();
        activate_bound_button(&mut app);
        assert_eq!(counter(&app), 0, "control only queued an intent");
        assert_eq!(app.dispatch_control_invocations(), 1);
        assert_eq!(counter(&app), 1);
        assert!(app.state.peek_last_undo().is_some());
        assert_eq!(
            app.control_action_coverage()[0].status,
            ControlBindingStatus::Resolved
        );
    }

    #[test]
    fn a11y_activation_drains_an_action_bound_control_before_returning() {
        let mut app = app();
        let button = app.focusable_order[0];
        // Retained widget ids have the same stable AccessKit mapping used by
        // the native provider. This exercises the public AT-SPI action path,
        // rather than manually draining the widget's queued intent.
        let accesskit_button = accesskit::NodeId(u64::from(button) + 1);

        assert_eq!(
            app.dispatch_a11y_action(accesskit_button, &crate::a11y::Action::Activate),
            None
        );
        assert_eq!(
            counter(&app),
            1,
            "AT-SPI activation must apply the bound action before success is returned"
        );
        assert!(app.state.peek_last_undo().is_some());
    }

    #[test]
    fn directed_task_context_survives_into_action_history() {
        let mut app = app();
        let resources = app.collect_resources();
        assert!(!resources.is_empty(), "test action surface was rejected");
        let ambient = tilo_actions::ActionContext::default()
            .with_app("org.tilo.Agent", "Agent")
            .with_purpose("Make the opening concrete")
            .with_group("task:opening", "Rewrite the opening", "task");

        let result = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.counter", "bump"),
            None,
            "agent:Scribe",
            true,
            true,
            None,
            Some(tilo_actions::InvocationId::new()),
            Some(&ambient),
        );

        assert!(result.is_ok(), "the directed action failed: {result:?}");
        assert_eq!(counter(&app), 1, "the directed action was applied");
        let changes = app.state.doc.get_changes(&[]);
        let committed = changes.last().expect("the directed action committed");
        assert_eq!(committed.actor_id().to_bytes(), b"agent:Scribe");
        let context = committed
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
            .expect("the action commit retained semantic provenance");
        assert_eq!(context.action_label, "Bump");
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some("test.counter"),
            "the concrete addressed resource stays authoritative"
        );
        assert_eq!(context.purpose, ambient.purpose);
        assert_eq!(context.group, ambient.group);
        assert_eq!(context.app, ambient.app);
        assert_eq!(
            app.state
                .peek_last_undo()
                .and_then(|entry| entry.author.as_ref()),
            Some(&crate::permissions::UserId("agent:Scribe".into())),
            "undo attribution and durable history use the same actor"
        );
    }

    #[test]
    fn broker_invocation_republishes_a_document_dependent_action_surface() {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_post_bump_reset()),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        assert!(
            tilo_actions::door::survey(&app.collect_resources())
                .iter()
                .all(|descriptor| descriptor.address.action != "reset"),
            "reset is unavailable before the counter changes"
        );
        let publication_attempts = app.actions.action_surface_publication_attempts;

        app.apply_collab_event_for_test(CollabEvent::Invocation {
            id: 42,
            resource: "test.counter".into(),
            action: "bump".into(),
            argument: None,
            actor: "agent:Scribe".into(),
            context: None,
        });

        assert_eq!(counter(&app), 1, "the directed document action ran");
        assert!(
            tilo_actions::door::survey(&app.collect_resources())
                .iter()
                .any(|descriptor| descriptor.address.action == "reset"),
            "the post-mutation surface exposes the newly valid reset action"
        );
        assert_eq!(
            app.actions.action_surface_publication_attempts,
            publication_attempts.saturating_add(1),
            "the broker event batch republishes without waiting for native window input"
        );
    }

    #[test]
    fn accepted_broker_invocation_republishes_when_its_terminal_update_arrives() {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_async_post_bump_reset()),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();

        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(43));
        app.apply_collab_event_for_test(CollabEvent::Invocation {
            id: 43,
            resource: "test.counter".into(),
            action: "async-bump".into(),
            argument: None,
            actor: "agent:Scribe".into(),
            context: None,
        });
        assert_eq!(counter(&app), 0, "admission alone does not mutate");
        assert_eq!(
            app.actions
                .directed_invocations
                .get(&invocation)
                .map(|pending| pending.wire_id),
            Some(43),
            "Accepted remains pending until the widget reports a terminal update"
        );

        app.refresh_focus_order();
        assert_eq!(counter(&app), 1, "the later model pass applied the job");
        assert!(
            tilo_actions::door::survey(&app.collect_resources())
                .iter()
                .any(|descriptor| descriptor.address.action == "reset"),
            "the terminal document now exposes reset"
        );
        let publication_attempts = app.actions.action_surface_publication_attempts;

        assert!(
            app.poll_directed_invocations(),
            "the terminal update arrived"
        );
        assert!(app.actions.directed_invocations.is_empty());
        assert_eq!(
            app.actions.action_surface_publication_attempts,
            publication_attempts.saturating_add(1),
            "terminal lifecycle polling republishes without native input"
        );
    }

    #[test]
    fn undecodable_document_action_preserves_pending_edit() {
        let resource = Resource::new("test.invalid", "test.invalid", "Invalid action").with_action(
            ActionSpec::immediate(
                "decode",
                "Decode",
                "test.unregistered",
                &CounterAction::Bump,
            )
            .unwrap(),
        );
        let mut app = lazy_app(resource);
        let before = app.state.doc.get_heads();
        app.state.doc.put(ROOT, "draft", "unfinished").unwrap();
        let pending = app.state.doc.pending_ops();
        let result = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.invalid", "decode"),
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(
            matches!(result, Err(tilo_actions::DoorError::UnregisteredType(ref id))
            if id == "test.unregistered"),
            "unexpected result: {result:?}"
        );
        assert_eq!(app.state.doc.pending_ops(), pending);
        assert_eq!(app.state.doc.base_heads(), before);
        assert!(app.state.peek_last_undo().is_none());
    }

    #[test]
    fn refused_and_routed_actions_preserve_pending_edit() {
        let mut app = app();
        let before = app.state.doc.get_heads();
        app.state.doc.put(ROOT, "draft", "unfinished").unwrap();
        let pending = app.state.doc.pending_ops();
        for (action, actor) in [
            ("host-bump", app.collaboration.local_author.0.clone()),
            ("inspect", "agent:Scribe".to_string()),
            ("missing", app.collaboration.local_author.0.clone()),
        ] {
            let result = app.invoke_resource_action(
                &tilo_actions::ActionAddress::new("test.counter", action),
                None,
                &actor,
                false,
                false,
                None,
                None,
                None,
            );
            assert!(result.is_err(), "unadmitted action must fail: {action}");
            assert_eq!(app.state.doc.pending_ops(), pending, "{action}");
            assert_eq!(app.state.doc.base_heads(), before, "{action}");
            assert!(app.state.peek_last_undo().is_none());
        }
        app.state.doc.put(ROOT, "draft", "finished").unwrap();
        let context = tilo_actions::ActionContext::for_resource(
            "Finish draft",
            "test.draft",
            "note",
            "Draft",
        );
        assert!(app.state.commit_action_authored_with_context(
            None,
            None,
            Some(app.collaboration.local_author.clone()),
            Some(context),
        ));
        let changes = app.state.doc.get_changes(&before);
        assert_eq!(changes.len(), 1, "refusal must not split the pending edit");
        let context = changes[0]
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
            .unwrap();
        assert_eq!(context.action_label, "Finish draft");
    }

    #[test]
    fn host_required_document_action_runs_only_after_broker_admission() {
        let mut app = app();
        let address = tilo_actions::ActionAddress::new("test.counter", "host-bump");

        let local = app.invoke_resource_action(
            &address,
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(local.is_err(), "an unbrokered client must fail closed");
        assert_eq!(counter(&app), 0);

        let admitted = app.invoke_resource_action(
            &address,
            None,
            "agent:Scribe",
            true,
            true,
            None,
            Some(tilo_actions::InvocationId::new()),
            None,
        );
        assert!(admitted.is_ok(), "the authoritative broker admitted it");
        assert_eq!(counter(&app), 1);
        assert_eq!(
            app.state
                .peek_last_undo()
                .and_then(|entry| entry.author.as_ref()),
            Some(&crate::permissions::UserId("agent:Scribe".into()))
        );
    }

    #[test]
    fn directed_view_action_cannot_steal_the_local_view() {
        let mut app = app();
        let invocation = tilo_actions::InvocationId::new();
        let error = app
            .invoke_resource_action(
                &tilo_actions::ActionAddress::new("test.counter", "inspect"),
                None,
                "agent:Scribe",
                true,
                true,
                None,
                Some(invocation),
                None,
            )
            .expect_err("a remote presentation action must be refused");

        assert_eq!(counter(&app), 0, "the private view action never ran");
        assert!(
            matches!(
                error,
                tilo_actions::DoorError::Unavailable { ref code, .. } if code == "private_view"
            ),
            "unexpected refusal: {error:?}"
        );
    }

    #[test]
    fn local_control_result_does_not_broadcast_into_component_siblings() {
        use crate::application_test_support::ProbeHost;
        use crate::widget::{Column, TileHost};
        for closed in [false, true] {
            let owner = ProbeHost::new();
            let (component, sibling_seen) =
                crate::widget::control_result_probe::component(&owner, 11, false);
            let (legacy, legacy_seen) = crate::widget::control_result_probe::Probe::new(false);
            let root = Column::new()
                .push(Box::new(legacy))
                .push(Box::new(TileHost::from_component(component)));
            let mut host = App::new_with_controlled_inputs(State::new(), Box::new(root));
            if closed {
                host.close_components();
            }
            legacy_seen.enqueue("missing");
            host.dispatch_control_invocations();
            assert_eq!(legacy_seen.results.borrow().len(), 1);
            assert!(
                sibling_seen.results.borrow().is_empty(),
                "legacy feedback must not enter bound sibling"
            );
            assert_eq!(sibling_seen.calls.get(), 0);
        }
    }

    #[test]
    fn bound_local_control_result_targets_only_the_emitting_instance() {
        use crate::application_test_support::ProbeHost;
        use crate::widget::{Column, TileHost};
        let owner = ProbeHost::new();
        let (first, first_seen) = crate::widget::control_result_probe::component(&owner, 11, false);
        let (second, second_seen) =
            crate::widget::control_result_probe::component(&owner, 12, false);
        let root = Column::new()
            .push(Box::new(TileHost::from_component(first)))
            .push(Box::new(TileHost::from_component(second)));
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(root));
        first_seen.enqueue("run");
        host.dispatch_control_invocations();
        assert_eq!(first_seen.results.borrow().len(), 1);
        assert!(second_seen.results.borrow().is_empty());
        assert_eq!(first_seen.calls.get(), 1);
        assert_eq!(second_seen.calls.get(), 0);
    }

    #[test]
    fn bound_local_control_result_reports_permission_refusal_and_success() {
        use crate::application_test_support::{ProbeHost, runner_inputs};
        for accepted in [false, true] {
            let owner = ProbeHost::new();
            let (component, observed) =
                crate::widget::control_result_probe::component(&owner, 11, accepted);
            let binding = component.callback_binding();
            let mut host = App::from_component(component, runner_inputs()).unwrap();
            observed.enqueue("run");
            host.dispatch_control_invocations();
            assert_eq!(observed.calls.get(), 1);
            let expected = if accepted {
                crate::widget::ResourceOperationStart::Accepted
            } else {
                crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                )
            };
            assert_eq!(*observed.results.borrow(), vec![Ok(expected)]);
            observed.enqueue("run");
            *binding.permissions.borrow_mut() = PermissionContext::from_capabilities(
                collab_types::caps::Actor::person(Some(collab_types::role::Role::Viewer), "reader"),
                Vec::new(),
            );
            let before = owner
                .records
                .read(&binding.checkout.id, automerge::AutoCommit::base_heads)
                .unwrap();
            host.dispatch_control_invocations();
            assert_eq!(observed.calls.get(), 1, "revoked permission blocks handler");
            assert_eq!(
                owner
                    .records
                    .read(&binding.checkout.id, automerge::AutoCommit::base_heads)
                    .unwrap(),
                before
            );
            assert_eq!(observed.results.borrow().len(), 2);
            assert!(matches!(observed.results.borrow().last(), Some(Err(_))));
        }
    }

    #[test]
    fn local_control_result_keeps_confirmation_as_refusal() {
        let (mut probe, observed) = crate::widget::control_result_probe::Probe::new(false);
        probe.confirmation = true;
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));
        observed.enqueue("run");
        host.dispatch_control_invocations();
        assert_eq!(observed.calls.get(), 0);
        assert_eq!(
            *observed.results.borrow(),
            vec![Err(tilo_actions::DoorError::ConfirmationRequired)]
        );
        assert!(host.action_palette().is_some());
    }

    #[test]
    fn confirmed_control_preserves_captured_arguments_through_palette_dispatch() {
        let (mut probe, observed) = crate::widget::control_result_probe::Probe::new(false);
        probe.confirmation = true;
        probe.typed_text = true;
        observed.enqueue_text("run", "captured before confirmation");
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));

        assert_eq!(host.dispatch_control_invocations(), 1);
        assert!(
            host.action_palette()
                .is_some_and(crate::action_palette::ActionPalette::in_argument_mode),
            "captured arguments enter confirmation directly"
        );
        assert!(observed.payloads.borrow().is_empty());

        host.actions
            .action_palette
            .as_mut()
            .expect("confirmation palette is open")
            .set_query("CONFIRM");
        assert!(host.action_palette_handle_key(&action_palette_tests::synth_enter()));
        assert_eq!(
            observed.payloads.borrow().as_slice(),
            ["captured before confirmation"],
            "confirmation dispatches the original intent arguments"
        );
    }

    #[test]
    fn local_control_result_reports_completed_and_accepted() {
        use crate::widget::control_result_probe::Probe;
        for accepted in [false, true] {
            let (probe, observed) = Probe::new(accepted);
            let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));
            observed.enqueue("run");
            host.dispatch_control_invocations();
            assert_eq!(
                observed.calls.get(),
                1,
                "accepted={accepted}; feedback={:?}",
                observed.results.borrow()
            );
            let expected = if accepted {
                crate::widget::ResourceOperationStart::Accepted
            } else {
                crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                )
            };
            assert_eq!(*observed.results.borrow(), vec![Ok(expected)]);
            observed.enqueue("missing");
            host.dispatch_control_invocations();
            assert_eq!(observed.calls.get(), 1);
            assert!(matches!(
                observed.results.borrow().last(),
                Some(Err(tilo_actions::DoorError::Unknown(_)))
            ));
        }
    }

    #[test]
    fn local_control_result_reports_fresh_permission_refusal_without_mutation() {
        use crate::widget::control_result_probe::Probe;
        let (probe, observed) = Probe::new(false);
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));
        observed.enqueue("run");
        let readonly = PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(Some(collab_types::role::Role::Viewer), "reader"),
            Vec::new(),
        );
        host.set_permission_ctx(readonly);
        let before = host.state.doc.base_heads();
        host.dispatch_control_invocations();
        assert_eq!(observed.calls.get(), 0, "permission checked before handler");
        assert_eq!(host.state.doc.base_heads(), before);
        assert_eq!(
            automerge::transaction::Transactable::pending_ops(&host.state.doc),
            0
        );
        assert!(matches!(
            observed.results.borrow().as_slice(),
            [Err(tilo_actions::DoorError::Unknown(_))]
        ));
        assert!(observed.pending.borrow().is_empty());
    }

    #[test]
    fn local_effect_completion_does_not_mutate_the_app_document() {
        let mut app = app();
        let before = app.state.doc.get_heads();
        let result = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.counter", "ping"),
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(matches!(
            result,
            Ok(crate::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(app.state.doc.get_heads(), before);
        assert!(app.state.peek_last_undo().is_none());
    }

    #[test]
    fn permission_revocation_and_restore_refresh_bound_control_presentation() {
        let mut app = app();
        let node = app.focusable_order[0];
        let pointer = app.layout.node_map[&node];
        assert!(unsafe { (*pointer).is_enabled() });

        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));
        assert!(!unsafe { (*pointer).is_enabled() });
        assert!(!unsafe {
            (*pointer).perform_action(
                &crate::a11y::Action::Activate,
                &mut app.state.doc,
                &app.state.root,
            )
        });

        app.set_permission_ctx(PermissionContext::permissive());
        assert!(unsafe { (*pointer).is_enabled() });
    }

    #[test]
    fn action_bound_control_never_auto_confirms_destructive_action() {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_destructive(true)),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        activate_bound_button(&mut app);
        assert_eq!(app.dispatch_control_invocations(), 1);
        assert_eq!(counter(&app), 0);
        assert!(app.state.peek_last_undo().is_none());
        assert!(app.action_palette().is_some(), "confirmation UI is open");
    }

    #[test]
    fn action_bound_background_control_remains_pending_until_terminal_update() {
        let resource =
            crate::widget::resource_actions::resource("test.job", "Job", "run", "Run", true);
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "run").unwrap();
        let button = Button::new("run", "Run", None).on_action(binding);
        let root = crate::widget::resource_actions::background(Box::new(button), resource, |_| {
            let (sender, receiver) = std::sync::mpsc::channel();
            sender
                .send(Ok(tilo_actions::ActionOutput::default()))
                .unwrap();
            Ok(receiver)
        });
        let mut app = App::new_with_controlled_inputs(State::new(), root);
        app.init_layout();
        activate_bound_button(&mut app);
        assert_eq!(app.dispatch_control_invocations(), 1);
        assert_eq!(
            app.actions.local_invocations.len(),
            1,
            "Accepted is not success"
        );
        assert!(app.poll_directed_invocations());
        assert!(
            app.actions.local_invocations.is_empty(),
            "terminal result closes the tracked local job"
        );
    }
    fn label(app: &App) -> String {
        match app.state.doc.get(&ROOT, "label") {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    #[test]
    fn refresh_populates_the_tree_from_app_resources() {
        let mut app = app();
        app.refresh_semantic_tree();
        let tree = app.semantic_tree();
        let node = tree
            .node(&SemanticId("r:test.counter".into()))
            .expect("the app's open resource is a semantic node");
        assert_eq!(node.name.as_deref(), Some("Counter"));
        assert!(
            node.action("bump").is_some(),
            "the resource's action is an affordance"
        );
        assert!(node.action("set-label").is_some());
        // The root owns it (resources hang off the window root).
        let root = tree.node(tree.root()).unwrap();
        assert!(root.children.contains(&SemanticId("r:test.counter".into())));
    }

    #[test]
    fn identity_is_stable_across_refreshes() {
        let mut app = app();
        app.refresh_semantic_tree();
        let ids_before: Vec<SemanticId> =
            app.semantic_tree().iter().map(|n| n.id.clone()).collect();
        let delta = app.refresh_semantic_tree();
        assert!(
            delta.is_empty(),
            "a stable frame yields no delta, got {delta:?}"
        );
        let ids_after: Vec<SemanticId> = app.semantic_tree().iter().map(|n| n.id.clone()).collect();
        assert_eq!(ids_before, ids_after, "node identity survives a refresh");
    }

    #[test]
    fn semantic_action_carries_the_same_encoded_action_the_affordance_dispatches() {
        let mut app = app();
        app.refresh_semantic_tree();
        let bump = app
            .semantic_tree()
            .action(&SemanticId("r:test.counter".into()), "bump")
            .expect("bump affordance");

        let mut host = crate::action_host::ActionHost::new();
        let mut host_doc = automerge::AutoCommit::new();
        host.dispatch(&mut host_doc, COUNTER_TYPE, &CounterAction::Bump);
        let from_affordance = host
            .last_dispatched()
            .expect("the affordance dispatched")
            .clone();

        match &bump.kind {
            SemanticActionKind::App(enc) => assert_eq!(
                enc, &from_affordance,
                "the a11y node exposes the same EncodedAction the on-screen control dispatches"
            ),
            other => panic!("expected an app action, got {other:?}"),
        }
    }

    #[test]
    fn invoking_an_immediate_action_dispatches_it_through_the_action_model() {
        let mut app = app();
        app.refresh_semantic_tree();
        let id = SemanticId("r:test.counter".into());
        assert_eq!(counter(&app), 0);

        assert!(app.invoke_semantic_action(&id, "bump", None).unwrap());
        assert_eq!(
            counter(&app),
            1,
            "the invoked action mutated the shared doc"
        );
        assert!(app.invoke_semantic_action(&id, "bump", None).unwrap());
        assert_eq!(counter(&app), 2);

        // An unknown action name is a graceful no-op.
        assert!(!app.invoke_semantic_action(&id, "nope", None).unwrap());
        assert_eq!(counter(&app), 2);
    }

    #[test]
    fn invoking_a_prompted_action_uses_the_supplied_argument() {
        let mut app = app();
        app.refresh_semantic_tree();
        let id = SemanticId("r:test.counter".into());
        assert!(
            app.invoke_semantic_action(&id, "set-label", Some("hello"))
                .unwrap()
        );
        assert_eq!(
            label(&app),
            "hello",
            "the prompted argument reached the built action"
        );
    }

    #[test]
    fn invoke_matches_a_direct_affordance_dispatch_byte_for_byte() {
        let mut app = app();
        app.refresh_semantic_tree();
        app.invoke_semantic_action(&SemanticId("r:test.counter".into()), "bump", None)
            .unwrap();

        let mut host = crate::action_host::ActionHost::new();
        let mut host_doc = automerge::AutoCommit::new();
        host.dispatch(&mut host_doc, COUNTER_TYPE, &CounterAction::Bump);

        let via_a11y = counter(&app);
        let via_affordance = match host_doc.get(&ROOT, "n") {
            Ok(Some((v, _))) => v.to_i64().unwrap_or(0),
            _ => 0,
        };
        assert_eq!(
            via_a11y, via_affordance,
            "both paths produce the same result"
        );
    }

    #[test]
    fn virtual_tree_rows_dispatch_through_platform_and_semantic_routes() {
        let root = crate::widget::TreeNode::branch(
            1,
            "Root",
            (),
            vec![crate::widget::TreeNode::leaf(2, "Child", ())],
        );
        let mut app = App::new(
            State::new(),
            Box::new(
                crate::widget::TreeView::new(root)
                    .with_id("items-tree")
                    .with_expanded([1]),
            ),
        );
        app.init_layout();

        app.refresh_semantic_tree();
        let child = app
            .semantic_tree()
            .iter()
            .find(|node| {
                node.role == crate::a11y::Role::TreeItem && node.name.as_deref() == Some("Child")
            })
            .map(|node| node.id.clone())
            .expect("semantic child row");
        assert!(
            app.invoke_semantic_action(&child, "activate", None)
                .expect("semantic row activation")
        );
        app.refresh_semantic_tree();
        assert!(
            app.semantic_tree()
                .node(&child)
                .is_some_and(|node| node.state.selected)
        );

        let update = app.build_a11y_tree_update(1.0);
        assert_eq!(
            update
                .nodes
                .iter()
                .filter(|(_, node)| node.role() == accesskit::Role::TreeItem)
                .count(),
            2
        );
        let root_row = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == accesskit::Role::TreeItem)
            .map(|(id, node)| {
                assert!(node.supports_action(accesskit::Action::Collapse));
                *id
            })
            .expect("virtual root row");
        let route = crate::accesskit_bridge::resolve_node_id(
            app.root_widget.as_ref(),
            &app.layout.node_map,
            root_row,
        )
        .expect("root row route");
        assert!(matches!(
            route,
            crate::accesskit_bridge::A11yTarget::Synthetic {
                semantic_target: Some(ref target),
                ..
            } if target == "node:1"
        ));
        assert_eq!(
            app.dispatch_a11y_action(root_row, &crate::a11y::Action::Collapse),
            None
        );
        app.refresh_semantic_tree();
        let root_semantics = app
            .semantic_tree()
            .iter()
            .find(|node| {
                node.role == crate::a11y::Role::TreeItem && node.name.as_deref() == Some("Root")
            })
            .expect("root semantics after collapse");
        assert_eq!(root_semantics.state.expanded, Some(false));

        let collapsed = app.build_a11y_tree_update(1.0);
        let rows = collapsed
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == accesskit::Role::TreeItem)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "collapsing removes the child row");
        assert!(rows[0].1.supports_action(accesskit::Action::Expand));
    }

    #[test]
    fn nav_shell_accessible_autocomplete_edits_and_selects_through_host_dispatch() {
        let search = crate::widget::Autocomplete::new()
            .with_id("city-search")
            .with_placeholder("Search city")
            .with_suggester(|query| {
                if query == "Ber" {
                    vec![crate::widget::Suggestion::new("Berlin")]
                } else {
                    Vec::new()
                }
            });
        let nav =
            crate::widget::NavShell::new(Box::new(search), Box::new(crate::widget::Column::new()));
        let mut app = App::new(State::new(), Box::new(nav));
        app.init_layout();
        let update = app.build_a11y_tree_update(1.0);
        let field = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Search city"))
            .unwrap()
            .0;
        app.dispatch_a11y_action(field, &crate::a11y::Action::SetValue("Ber".into()));
        let update = app.build_a11y_tree_update(1.0);
        let result = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Berlin"))
            .expect("native edit must publish a result")
            .0;
        app.dispatch_a11y_action(result, &crate::a11y::Action::Activate);
        let update = app.build_a11y_tree_update(1.0);
        let field = &update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Search city"))
            .unwrap()
            .1;
        assert_eq!(field.value(), Some("Berlin"));
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("Berlin"))
        );
    }

    #[test]
    fn retained_tree_delegate_is_nested_once_and_keeps_its_own_action() {
        let presses = std::rc::Rc::new(std::cell::Cell::new(0_usize));
        let captured = std::rc::Rc::clone(&presses);
        let root = crate::widget::TreeNode::leaf(7, "Item", ());
        let tree = crate::widget::TreeView::new(root).with_row_delegate(move |_, id, _| {
            let button_id = format!("action-{id}");
            let captured = std::rc::Rc::clone(&captured);
            Box::new(
                crate::widget::Button::new(&button_id, "Action", None).on_click(move || {
                    captured.set(captured.get().saturating_add(1));
                }),
            )
        });
        let mut app = App::new(State::new(), Box::new(tree));
        app.init_layout();

        let update = app.build_a11y_tree_update(1.0);
        let buttons = update
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == accesskit::Role::Button)
            .collect::<Vec<_>>();
        assert_eq!(buttons.len(), 1, "delegate is not announced twice");
        let button_id = buttons[0].0;
        let row = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == accesskit::Role::TreeItem)
            .map(|(_, node)| node)
            .expect("tree item");
        assert_eq!(row.children(), &[button_id]);
        assert!(
            row.bounds().is_some(),
            "platform row keeps viewport geometry"
        );
        assert_eq!(
            app.dispatch_a11y_action(button_id, &crate::a11y::Action::Activate),
            None
        );
        assert_eq!(presses.get(), 1);

        app.refresh_semantic_tree();
        let semantic_buttons = app
            .semantic_tree()
            .iter()
            .filter(|node| node.role == crate::a11y::Role::Button)
            .collect::<Vec<_>>();
        assert_eq!(semantic_buttons.len(), 1);
        let semantic_row = app
            .semantic_tree()
            .iter()
            .find(|node| node.role == crate::a11y::Role::TreeItem)
            .expect("semantic tree item");
        assert!(semantic_row.children.contains(&semantic_buttons[0].id));
        assert!(
            semantic_row.bounds.is_some(),
            "inspection row keeps viewport geometry"
        );
        assert!(
            !semantic_row.state.selected,
            "button activation does not select row"
        );
    }

    #[test]
    fn synthetic_drawn_control_dispatches_through_retained_semantic_tree() {
        let changed = std::rc::Rc::new(std::cell::Cell::new(usize::MAX));
        let captured = std::rc::Rc::clone(&changed);
        let group = crate::widget::ToggleGroup::new(
            vec![
                crate::widget::toggle_group::Segment::new("Light"),
                crate::widget::toggle_group::Segment::new("Dark"),
            ],
            0,
        )
        .on_change(move |index| captured.set(index));
        let mut app = App::new(State::new(), Box::new(group));
        app.init_layout();
        app.refresh_semantic_tree();
        let dark = app
            .semantic_tree()
            .find_by_name("Dark")
            .map(|node| node.id.clone())
            .expect("synthetic Dark segment");

        assert!(
            app.invoke_semantic_action(&dark, "activate", None)
                .expect("semantic dispatch")
        );
        assert_eq!(changed.get(), 1);
    }

    #[test]
    fn effectful_control_is_not_exported_without_a_domain_contract() {
        let presses = std::rc::Rc::new(std::cell::Cell::new(0_usize));
        let captured = std::rc::Rc::clone(&presses);
        let button = Button::new("drive-me", "Drive me", None).on_click(move || {
            captured.set(captured.get().saturating_add(1));
        });
        let mut app = App::new(State::new(), Box::new(button));
        app.init_layout();

        let resource = app
            .collect_resources()
            .into_iter()
            .find(|resource| resource.kind == "tilo.ui.control" && resource.label == "Drive me");
        assert!(resource.is_none_or(|resource| {
            resource
                .actions
                .iter()
                .all(|action| action.id != "activate")
        }));
        assert_eq!(presses.get(), 0);
    }

    include!("app/action_directory_snapshot_tests.rs");

    fn directory_query(
        revision: impl Into<String>,
        parent_resource: impl Into<String>,
        max_results: u32,
        max_bytes: u32,
        max_work: u32,
    ) -> runtime_collab::ActionDirectoryQuery {
        runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: revision.into(),
            cursor: Vec::new(),
            needle: String::new(),
            parent_resource: parent_resource.into(),
            max_results,
            max_bytes,
            max_work,
            max_depth: 8,
        }
    }

    #[test]
    fn directory_pages_parent_actions_and_refuses_stale_resolution() {
        let mut app = app();
        let root = app.action_directory_page(directory_query("", "", 8, 8_192, 8));
        assert_eq!(root.rows.len(), 1);
        assert_eq!(root.rows[0].resource, "test.counter");
        assert!(root.rows[0].action.is_none());
        assert!(root.rows[0].has_children);

        let actions =
            app.action_directory_page(directory_query(root.revision, "test.counter", 8, 8_192, 8));
        assert!(actions.rows.len() >= 5, "parent exposes its action rows");
        assert!(actions.rows.iter().all(|row| row.action.is_some()));
        assert!(matches!(
            app.resolve_action_directory("stale", "test.counter", "inspect"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
    }

    #[test]
    fn broker_request_ids_use_the_final_value_then_fail_closed() {
        let mut app = app();
        app.actions.next_broker_request_id = Some(u64::MAX);
        assert_eq!(app.take_broker_request_id().unwrap(), u64::MAX);
        assert!(matches!(
            app.take_broker_request_id(),
            Err(tilo_actions::DoorError::Unavailable { code, .. })
                if code == "broker_request_identity_exhausted"
        ));
    }

    #[test]
    fn action_directory_cursor_keys_do_not_reuse_a_process_counter() {
        let mut app = app();
        let first = app.action_directory_cursor_key();
        let second = app.action_directory_cursor_key();
        assert_ne!(first, second);
        assert!(first.starts_with("tilo-dir-"));
        assert!(second.starts_with("tilo-dir-"));
    }

    #[test]
    fn directory_hard_caps_never_materialize_an_oversized_row() {
        let mut app = app();
        let page = app.action_directory_page(directory_query("", "", 1, 1, 1));
        assert!(
            page.rows.is_empty(),
            "one byte cannot contain a resource row"
        );
        assert!(page.budget_exhausted);
        assert!(!page.exhausted);
    }

    #[test]
    fn directory_pages_the_129th_widget_root_without_hiding_it() {
        let mut app = App::new(
            State::new(),
            Box::new(CounterCard::with_directory_roots(
                ACTION_DIRECTORY_MAX_RESULTS + 1,
            )),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        let first = app.action_directory_page(directory_query(
            "",
            "",
            u32::try_from(ACTION_DIRECTORY_MAX_RESULTS).unwrap_or(u32::MAX),
            64 * 1024,
            u32::try_from(ACTION_DIRECTORY_MAX_WORK).unwrap_or(u32::MAX),
        ));
        assert_eq!(first.rows.len(), ACTION_DIRECTORY_MAX_RESULTS);
        assert!(!first.exhausted);
        let tail = app.action_directory_page(runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: first.revision.clone(),
            cursor: first.next_cursor,
            needle: String::new(),
            parent_resource: String::new(),
            max_results: 8,
            max_bytes: 8_192,
            max_work: 32,
            max_depth: 8,
        });
        assert_eq!(tail.rows.len(), 1);
        assert_eq!(tail.rows[0].resource, "test.counter:128");
        assert!(tail.exhausted);
    }

    #[test]
    fn directory_finds_a_100k_tail_without_scanning_the_corpus() {
        let provider = LargeDirectoryProvider::new(100_000);
        let calls = std::sync::Arc::clone(&provider.calls);
        let largest_budget = std::sync::Arc::clone(&provider.largest_budget);
        let mut app = App::new(
            State::new(),
            Box::new(CounterCard::with_directory_provider(provider)),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        let page = app.action_directory_page(runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: String::new(),
            cursor: Vec::new(),
            needle: "needle-99999".into(),
            parent_resource: String::new(),
            max_results: 8,
            max_bytes: 8_192,
            max_work: 8,
            max_depth: 8,
        });
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].resource, "test.item:99999");
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(
            largest_budget.load(std::sync::atomic::Ordering::Relaxed) <= 8,
            "the adapter passed the caller's bounded work budget to the retained index"
        );
    }

    #[test]
    fn directory_cursor_saturation_evicts_old_cursors_as_stale() {
        let mut app = app();
        let revision = app.action_directory_revision();
        let first = app.action_directory_next_cursor(ActionDirectoryCursor {
            revision: revision.clone(),
            needle: String::new(),
            parent_resource: String::new(),
            provider_index: 0,
            provider_cursor: None,
            browse_offset: 0,
            root_widget_index: 0,
            root_provider_cursor: None,
        });
        for offset in 1..=ACTION_DIRECTORY_MAX_CURSORS {
            let _ = app.action_directory_next_cursor(ActionDirectoryCursor {
                revision: revision.clone(),
                needle: String::new(),
                parent_resource: String::new(),
                provider_index: 0,
                provider_cursor: None,
                browse_offset: offset,
                root_widget_index: 0,
                root_provider_cursor: None,
            });
        }
        assert!(app.actions.action_directory_cursors.len() <= ACTION_DIRECTORY_MAX_CURSORS);
        let page = app.action_directory_page(runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision,
            cursor: first,
            needle: String::new(),
            parent_resource: String::new(),
            max_results: 1,
            max_bytes: 1_024,
            max_work: 1,
            max_depth: 1,
        });
        assert!(page.stale_cursor);
    }
}

#[cfg(test)]
mod damage_lifecycle_tests {
    use super::*;
    use crate::widget::TaffyTreeExt;
    use automerge::{AutoCommit, ObjId};
    use std::collections::HashMap;

    struct DrawUpdatedDamage {
        damage: KurboRect,
        dirty: bool,
    }

    impl Widget for DrawUpdatedDamage {
        fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.leaf_node(taffy::Style::default());
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
            self.damage = KurboRect::new(80.0, 0.0, 100.0, 8.0);
        }

        fn handle_event(
            &mut self,
            _event: &WindowEvent,
            _scale_factor: f64,
            _modifiers: &winit::event::Modifiers,
            _state: &mut AutoCommit,
            _path: &ObjId,
            _handled: &mut bool,
            _focus_target: &mut Option<NodeId>,
            _node_id: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
        }

        fn name(&self) -> &'static str {
            "DrawUpdatedDamage"
        }

        fn get_rect(&self) -> KurboRect {
            self.damage
        }

        fn needs_redraw(&self) -> bool {
            self.dirty
        }

        fn collect_damage(&self, regions: &mut Vec<KurboRect>) {
            regions.push(self.damage);
        }

        fn mark_drawn(&mut self) {
            self.dirty = false;
        }
    }

    #[test]
    fn submitted_damage_uses_draw_updated_geometry_before_dirty_clear() {
        let mut widget = DrawUpdatedDamage {
            damage: KurboRect::new(0.0, 0.0, 20.0, 8.0),
            dirty: true,
        };
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::default(),
        );

        let mut regions = Vec::new();
        collect_submitted_widget_damage(&widget, &mut regions);
        assert_eq!(regions, vec![KurboRect::new(80.0, 0.0, 100.0, 8.0)]);
        assert!(widget.dirty, "collection must not consume the dirty flag");

        widget.mark_drawn();
        assert!(!widget.dirty);
    }
}

#[cfg(test)]
mod wake_integration_tests {
    use super::*;
    use crate::view::{FreezeThaw, ViewContribution};
    use crate::wake::{WakeEvent, WakeHandle, WakeScope};
    use crate::{LatestRequest, LocalState};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc::{self, Receiver, TryRecvError};

    fn bound_scope() -> (WakeScope, Receiver<WakeEvent>) {
        let (tx, rx) = mpsc::channel();
        let mut scope = WakeScope::new();
        scope
            .bind(move |event| {
                let _ = tx.send(event);
            })
            .unwrap();
        (scope, rx)
    }

    type Captures = Rc<RefCell<Vec<(&'static str, LatestRequest<u32>)>>>;

    struct CaptureRoot {
        captures: Captures,
    }

    impl CaptureRoot {
        fn capture(&self, label: &'static str) {
            self.captures
                .borrow_mut()
                .push((label, LatestRequest::new()));
        }
    }

    impl Drop for CaptureRoot {
        fn drop(&mut self) {
            self.capture("drop");
        }
    }

    impl Widget for CaptureRoot {
        fn sync(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) {
            self.capture("sync");
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.capture("layout");
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {}

        fn draw(
            &mut self,
            _: &mut vello::Scene,
            _: vello::kurbo::Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
        }

        #[allow(
            clippy::too_many_arguments,
            reason = "signature is fixed by the Widget trait"
        )]
        fn handle_event(
            &mut self,
            _: &WindowEvent,
            _: f64,
            _: &winit::event::Modifiers,
            _: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
            _: &mut bool,
            _: &mut Option<NodeId>,
            _: NodeId,
            _: &TaffyTree<()>,
        ) {
            self.capture("event");
        }

        fn name(&self) -> &'static str {
            "capture-root"
        }

        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }

        fn view_state(&self) -> Option<ViewContribution> {
            self.capture("capture-view");
            Some(ViewContribution::default())
        }

        fn restore_view_state(&mut self, _: &ViewContribution) {
            self.capture("restore-view");
        }
    }

    #[test]
    fn factory_state_stays_owned_before_binding_and_after_return() {
        let mut captured = None;
        let mut app = App::from_factory_with_controlled_inputs(State::new(), || {
            let state = LocalState::new(0);
            state.writer().set(1);
            captured = Some(state);
            Box::new(crate::widget::Text::new("root"))
        });
        let state = captured.unwrap();
        let (tx, rx) = mpsc::channel();
        app.wake_scope
            .as_mut()
            .unwrap()
            .bind(move |event| {
                let _ = tx.send(event);
            })
            .unwrap();
        let pending = rx.try_recv().unwrap();
        app.needs_sync = false;
        assert!(app.accept_wake(&pending));
        assert!(app.needs_sync);
        assert_eq!(state.get(), 1);
        drop(app);
        assert!(!state.writer().try_set(2));
    }

    #[test]
    fn transfer_preserves_the_exact_owner_and_rejects_foreign_events() {
        let (scope, rx) = bound_scope();
        let (other, other_rx) = bound_scope();
        let state = {
            let _entry = scope.enter();
            LocalState::new(0)
        };
        let mut app = App::from_scoped_parts_with_controlled_inputs(
            scope,
            State::new(),
            Box::new(crate::widget::Text::new("root")),
        );
        state.writer().set(1);
        let current = rx.try_recv().unwrap();
        other.handle().wake();
        let foreign = other_rx.try_recv().unwrap();
        app.needs_sync = false;
        assert!(!app.accept_wake(&foreign));
        assert!(!app.needs_sync);
        assert!(app.accept_wake(&current));
        assert!(app.needs_sync);
        app.close_wake_scope();
        app.needs_sync = false;
        assert!(!app.accept_wake(&current));
        assert!(!app.needs_sync);
    }

    #[test]
    fn collaboration_queue_wake_does_not_force_document_sync() {
        let (scope, rx) = bound_scope();
        let handle = scope.handle();
        let mut app = App::from_scoped_parts_with_controlled_inputs(
            scope,
            State::new(),
            Box::new(crate::widget::Text::new("root")),
        );
        app.needs_sync = false;
        handle.notify_events();
        assert!(app.accept_wake(&rx.try_recv().unwrap()));
        assert!(!app.needs_sync);
        app.needs_sync = true;
        handle.notify_events();
        assert!(app.accept_wake(&rx.try_recv().unwrap()));
        assert!(
            app.needs_sync,
            "queue wake must not erase an earlier model invalidation"
        );
    }

    #[test]
    fn fallible_factory_error_closes_captures_and_restores_parent_context() {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let mut captured = None;
        let result: Result<App, &'static str> = App::try_from_factory(State::new(), || {
            captured = Some(LocalState::new(0));
            Err("factory failed")
        });
        assert!(matches!(result, Err("factory failed")));
        assert!(!captured.unwrap().writer().try_set(1));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }

    #[test]
    fn factory_unwind_closes_captures_and_restores_parent_context() {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let mut captured = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            App::from_factory(State::new(), || {
                captured = Some(LocalState::new(0));
                panic!("factory panicked");
            })
        }));
        assert!(result.is_err());
        assert!(!captured.unwrap().writer().try_set(1));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }

    #[test]
    fn callback_created_requests_keep_app_owner_when_begun_elsewhere() {
        let (scope, rx) = bound_scope();
        let (other, other_rx) = bound_scope();
        let captures = Captures::default();
        let root = {
            let _entry = scope.enter();
            Box::new(CaptureRoot {
                captures: Rc::clone(&captures),
            })
        };
        let mut app = App::from_scoped_parts_with_controlled_inputs(scope, State::new(), root);
        {
            let _other = other.enter();
            app.init_layout();
            app.refresh_focus_order();
            let view = app.capture_view();
            app.restore_view(&view);
        }
        let requests = std::mem::take(&mut *captures.borrow_mut());
        for label in ["layout", "sync", "capture-view", "restore-view"] {
            assert!(requests.iter().any(|(actual, _)| *actual == label));
        }
        for (_, mut request) in requests {
            let completion = {
                let _other = other.enter();
                request.begin()
            };
            assert!(
                std::thread::spawn(move || completion.complete(7))
                    .join()
                    .unwrap()
            );
            assert!(app.accept_wake(&rx.try_recv().unwrap()));
            assert_eq!(request.try_take(), Some(7));
        }
        assert!(matches!(other_rx.try_recv(), Err(TryRecvError::Empty)));
        {
            let _other = other.enter();
            drop(app);
        }
        let mut after_drop = std::mem::take(&mut *captures.borrow_mut());
        assert!(after_drop.iter().any(|(label, _)| *label == "drop"));
        for (_, request) in &mut after_drop {
            assert!(request.try_begin().is_none());
        }
        assert!(matches!(other_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn detached_app_callbacks_mask_an_ambient_host() {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let captures = Captures::default();
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CaptureRoot {
                captures: Rc::clone(&captures),
            }),
        );
        app.refresh_focus_order();
        let mut requests = std::mem::take(&mut *captures.borrow_mut());
        assert!(!requests.is_empty());
        for (_, request) in &mut requests {
            assert!(request.begin().complete(1));
            assert_eq!(request.try_take(), Some(1));
        }
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }

    #[test]
    #[should_panic(expected = "live App requires from_factory")]
    fn detached_native_startup_fails_before_creating_an_event_loop() {
        App::new_with_controlled_inputs(State::new(), Box::new(crate::widget::Text::new("root")))
            .run();
    }
}

#[cfg(test)]
mod bound_component_tests {
    use super::*;
    use crate::application::{
        AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
    };
    use crate::widget::{AppKind, AppWidget, Column, IconDescriptor, TileHost};
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    use std::cell::RefCell;
    use std::rc::Rc;
    thread_local! { static CAPTURES: RefCell<Vec<(&'static str, crate::LatestRequest<u32>)>> = const { RefCell::new(vec![]) }; }
    thread_local! { static DISCOVERY_COUNT: Cell<usize> = const { Cell::new(0) }; }
    thread_local! { static CONTROL_AUTHORITY: RefCell<Vec<(bool, bool)>> = const { RefCell::new(vec![]) }; }
    thread_local! { static COMPOUND_MODE: Cell<u8> = const { Cell::new(0) }; }
    thread_local! {
        static PAGED_SEARCH_COUNTS: RefCell<Option<Rc<[Cell<usize>; 2]>>> = const { RefCell::new(None) };
    }
    struct PagedProbe {
        catalog: crate::search::SearchCatalog,
        resolutions: Rc<[Cell<usize>; 2]>,
        owner: usize,
    }
    impl PagedProbe {
        fn new(strict: bool, resolutions: Rc<[Cell<usize>; 2]>) -> Self {
            let catalog = crate::search::SearchCatalog::new("Bound paging probe");
            for id in 0..65 {
                let resource = format!("bound:paged:{id}");
                catalog
                    .upsert(
                        &resource,
                        "paged",
                        crate::search::SearchResult::for_action(
                            format!("Paged kind {id}"),
                            format!("{} paged {id}", if strict { "Allowed" } else { "Revoked" }),
                            tilo_actions::ActionAddress::new(&resource, "apply"),
                        ),
                    )
                    .unwrap();
            }
            Self {
                catalog,
                resolutions,
                owner: usize::from(strict),
            }
        }
    }
    impl crate::search::SearchProvider for PagedProbe {
        fn name(&self) -> &'static str {
            "Bound paging probe"
        }
        fn search(&self, _: &str) -> Vec<crate::search::SearchResult> {
            panic!("the controlled paging fixture must use search_page")
        }
        fn search_page(
            &self,
            query: &str,
            cursor: Option<&crate::search::SearchCursor>,
            work_budget: usize,
            result_limit: usize,
        ) -> crate::search::SearchPage {
            self.catalog
                .query_page(query, cursor, work_budget, result_limit)
        }
        // Deliberately inherit unknown generation: validation cannot cache a
        // claim that this provider's authorization declarations are stable.
        fn resolve_action_resource(
            &self,
            address: &tilo_actions::ActionAddress,
        ) -> Option<tilo_actions::Resource> {
            let counter = &self.resolutions[self.owner];
            counter.set(counter.get().saturating_add(1));
            address.resource.strip_prefix("bound:paged:")?;
            (address.action == "apply").then(|| {
                tilo_actions::Resource::new(&address.resource, "probe", "Paged").with_action(
                    tilo_actions::ActionSpec::immediate("apply", "Apply", "bound.apply", &FollowUp)
                        .unwrap()
                        .requiring(tilo_actions::PermissionRequirement::Write),
                )
            })
        }
    }
    #[derive(Default)]
    struct Probe {
        strict: bool,
        collision: Option<String>,
        actions: crate::action_host::ActionHost,
    }
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct FollowUp;
    impl tilo_actions::Action for FollowUp {
        fn apply(&self, doc: &mut automerge::AutoCommit) -> Result<(), tilo_actions::ActionError> {
            doc.put(ROOT, "follow-up", true)?;
            Ok(())
        }
    }
    #[derive(Debug, serde::Serialize)]
    struct EmptyFollowUp {
        refuse: bool,
    }
    impl tilo_actions::Action for EmptyFollowUp {
        fn apply(&self, doc: &mut automerge::AutoCommit) -> Result<(), tilo_actions::ActionError> {
            if self.refuse {
                doc.put(ROOT, "failed-action", true)?;
                Err(tilo_actions::ActionError::Refused(
                    "controlled failure".into(),
                ))
            } else {
                Ok(())
            }
        }
    }
    impl Probe {
        fn capture(label: &'static str) {
            CAPTURES.with_borrow_mut(|v| v.push((label, crate::LatestRequest::new())));
        }
    }

    struct LazyProbe {
        strict: bool,
    }
    impl crate::search::SearchProvider for LazyProbe {
        fn name(&self) -> &'static str {
            "Probe lazy"
        }
        fn action_directory_generation(&self) -> Option<u64> {
            Some(0)
        }
        fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
            match query {
                "sealed" => vec![crate::search::SearchResult::for_action(
                    "Probe",
                    if self.strict {
                        "Allowed sealed sibling title"
                    } else {
                        "Hidden sealed write title"
                    },
                    tilo_actions::ActionAddress::new("bound:sealed", "apply"),
                )],
                "sealedopaque" => vec![crate::search::SearchResult::new(
                    "Probe",
                    if self.strict {
                        "Allowed sealedopaque title"
                    } else {
                        "Hidden sealedopaque title"
                    },
                    "opaque",
                )],
                "lazy" => vec![crate::search::SearchResult::for_action(
                    "Probe",
                    "Lazy",
                    tilo_actions::ActionAddress::new("bound:lazy", "view"),
                )],
                "opaque" => vec![crate::search::SearchResult::new(
                    "Probe", "Opaque", "opaque",
                )],
                _ => vec![],
            }
        }
        fn resolve_action_resource(
            &self,
            address: &tilo_actions::ActionAddress,
        ) -> Option<tilo_actions::Resource> {
            if address.resource == "bound:sealed" && address.action == "apply" {
                return Some(
                    tilo_actions::Resource::new("bound:sealed", "probe", "Sealed").with_action(
                        tilo_actions::ActionSpec::immediate(
                            "apply",
                            "Apply",
                            "bound.apply",
                            &FollowUp,
                        )
                        .unwrap()
                        .requiring(tilo_actions::PermissionRequirement::Write),
                    ),
                );
            }
            (address.resource == "bound:lazy" && address.action == "view").then(|| {
                tilo_actions::Resource::new("bound:lazy", "probe", "Lazy").with_action(
                    tilo_actions::ActionSpec::operation(
                        "view",
                        "View",
                        tilo_actions::OperationKind::View,
                        "bound.view",
                        &(),
                    )
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view()),
                )
            })
        }
    }
    impl Widget for Probe {
        fn refresh_control_action_availability(
            &mut self,
            allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
        ) {
            CONTROL_AUTHORITY.with_borrow_mut(|seen| {
                seen.push((
                    self.strict,
                    allowed.contains(&tilo_actions::ActionAddress::new("bound:probe", "apply")),
                ));
            });
        }

        fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
            if let Some(counts) = PAGED_SEARCH_COUNTS.with_borrow(Clone::clone) {
                return vec![Box::new(PagedProbe::new(self.strict, counts))];
            }
            vec![Box::new(LazyProbe {
                strict: self.strict,
            })]
        }
        fn activate_search_result(
            &mut self,
            id: &str,
            doc: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
        ) {
            assert_eq!(id, "opaque");
            doc.put(ROOT, "navigation", true).unwrap();
        }

        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &["bound.follow-up", "bound.view"]
        }
        fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
            DISCOVERY_COUNT.set(DISCOVERY_COUNT.get().saturating_add(1));
            let mut action = tilo_actions::ActionSpec::operation(
                "follow-up",
                "Follow up",
                tilo_actions::OperationKind::Effect,
                "bound.follow-up",
                &(),
            )
            .unwrap();
            action.contract.execution =
                tilo_actions::ExecutionMode::Background { cancellable: true };
            action.contract.result = tilo_actions::ResultSpec::Job;
            if self.strict {
                action.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
            }
            vec![
                tilo_actions::Resource::new(
                    self.collision.as_deref().unwrap_or("bound:probe"),
                    "probe",
                    "Probe",
                )
                .with_action(action)
                .with_action(
                    tilo_actions::ActionSpec::immediate(
                        "apply",
                        "Apply follow up",
                        "bound.apply",
                        &FollowUp,
                    )
                    .unwrap(),
                )
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "view",
                        "View",
                        tilo_actions::OperationKind::View,
                        "bound.view",
                        &(),
                    )
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view()),
                ),
            ]
        }
        fn resource(
            &self,
            doc: &automerge::AutoCommit,
            id: &str,
        ) -> Option<tilo_actions::Resource> {
            self.resources(doc)
                .into_iter()
                .find(|resource| resource.id.as_str() == id)
        }
        fn action_directory_visibility(
            &self,
            _: &automerge::AutoCommit,
            id: &str,
        ) -> Option<tilo_actions::ActionAddress> {
            (self.strict && id == "bound:probe")
                .then(|| tilo_actions::ActionAddress::new(id, "follow-up"))
        }
        fn action_directory_resource_page(
            &self,
            doc: &automerge::AutoCommit,
            cursor: &[u8],
            max_results: usize,
        ) -> Option<crate::widget::ActionDirectoryResourcePage> {
            Some(crate::widget::ActionDirectoryResourcePage {
                resources: if cursor.is_empty() && max_results > 0 {
                    self.resources(doc)
                } else {
                    vec![]
                },
                next_cursor: vec![],
                exhausted: true,
            })
        }
        #[allow(
            clippy::panic_in_result_fn,
            reason = "probe asserts canonical admission before returning the toolkit result"
        )]
        fn start_resource_operation_with_document(
            &mut self,
            _: tilo_actions::InvocationId,
            _: tilo_actions::OperationKind,
            type_id: &str,
            _: &[u8],
            doc: &mut automerge::AutoCommit,
            path: &automerge::ObjId,
        ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
            if type_id == "bound.view" {
                if COMPOUND_MODE.get() == 1 {
                    self.actions.dispatch(doc, "bound.apply", &FollowUp);
                }
                Self::capture("bound-view");
                return Ok(Some(crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                )));
            }
            assert_eq!(type_id, "bound.follow-up");
            assert_eq!(path, &ROOT);
            assert_eq!(
                doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
                Some("child")
            );
            let mode = COMPOUND_MODE.get();
            if matches!(mode, 3 | 4 | 6) {
                doc.put(ROOT, "raw-before", true).unwrap();
            }
            match mode {
                2 | 3 => {}
                7 => self.actions.dispatch_with_context(
                    doc,
                    "bound.apply",
                    &FollowUp,
                    tilo_actions::ActionContext::for_resource(
                        "Declared follow-up",
                        "bound:probe",
                        "probe",
                        "Probe",
                    ),
                ),
                8 => {
                    let mut queue = tilo_actions::OptimisticQueue::new(doc);
                    self.actions
                        .dispatch_optimistic(doc, &mut queue, "bound.apply", &FollowUp)
                        .unwrap();
                }
                9 | 10 => {
                    assert_eq!(
                        self.actions
                            .try_dispatch(doc, "bound.empty", &EmptyFollowUp { refuse: mode == 10 })
                            .is_err(),
                        mode == 10
                    );
                }
                11 => {
                    doc.put(ROOT, "raw-unwind", true).unwrap();
                    panic!("controlled compound unwind");
                }
                _ => self.actions.dispatch(doc, "bound.apply", &FollowUp),
            }
            if matches!(mode, 5 | 6) {
                doc.put(ROOT, "raw-after", true).unwrap();
            }
            Self::capture("mutable-operation");
            Ok(Some(crate::widget::ResourceOperationStart::Accepted))
        }
        fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
            self.actions.take_committed()
        }
        fn poll_resource_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            cancellation_requested: bool,
            doc: &automerge::AutoCommit,
            _: &automerge::ObjId,
        ) -> Option<tilo_actions::InvocationUpdate> {
            if !cancellation_requested {
                return None;
            }
            assert_eq!(
                doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
                Some("child")
            );
            assert!(!crate::local_state::LocalState::new(0).writer().try_set(1));
            Self::capture("mutable-cancel");
            Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: None,
            })
        }

        fn sync(&mut self, doc: &mut automerge::AutoCommit, _: &automerge::ObjId) {
            assert_eq!(
                doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
                Some("child")
            );
            Self::capture("sync");
        }
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _: &Theme,
            map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            Self::capture("build");
            let node = tree.new_leaf(taffy::Style::default()).unwrap();
            map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn measure(
            &mut self,
            _: taffy::Size<Option<f32>>,
            _: taffy::Size<taffy::AvailableSpace>,
            _: &mut TextRenderer,
        ) -> taffy::Size<f32> {
            Self::capture("measure");
            taffy::Size {
                width: 30.0,
                height: 20.0,
            }
        }
        fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {
            Self::capture("apply");
        }
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
            Self::capture("draw");
        }
        fn handle_event(
            &mut self,
            _: &WindowEvent,
            _: f64,
            _: &winit::event::Modifiers,
            d: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
            _: &mut bool,
            _: &mut Option<NodeId>,
            _: NodeId,
            _: &TaffyTree<()>,
        ) {
            assert_eq!(
                d.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
                Some("child")
            );
            Self::capture("event");
        }
        fn name(&self) -> &'static str {
            "bound-probe"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }
        fn is_focusable(&self) -> bool {
            Self::capture("focus-order");
            true
        }
        fn set_focus(&mut self, _: bool) {
            Self::capture("focus");
        }
        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            Self::capture("accessibility");
            Some(crate::a11y::AccessNode::new(crate::a11y::Role::Button).with_name("Child"))
        }
        fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
            Self::capture("ime");
            Some(winit::window::ImePurpose::Normal)
        }
        fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
            Self::capture("ime-rect");
            Some(vello::kurbo::Rect::ZERO)
        }
        fn perform_action(
            &mut self,
            _: &crate::a11y::Action,
            doc: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
        ) -> bool {
            assert_eq!(
                doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
                Some("child")
            );
            doc.put(ROOT, "action", true).unwrap();
            Self::capture("action");
            true
        }
    }
    impl AppWidget for Probe {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Probe"
        }
        fn app_icon(&self) -> IconDescriptor {
            IconDescriptor::Glyph("P")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }
    #[allow(clippy::needless_pass_by_value, reason = "factory contract")]
    fn factory(
        context: HostContext,
        target: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, crate::launch::LaunchError> {
        build_probe(context, &target, false)
    }
    #[allow(clippy::needless_pass_by_value, reason = "factory contract")]
    fn strict_factory(
        context: HostContext,
        target: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, crate::launch::LaunchError> {
        build_probe(context, &target, true)
    }
    fn build_probe(
        mut context: HostContext,
        target: &LaunchTarget,
        strict: bool,
    ) -> Result<AppComponent, crate::launch::LaunchError> {
        if matches!(target, LaunchTarget::New) {
            context
                .initialize_new_record(|doc| {
                    doc.put(ROOT, "marker", "child").map_err(|e| e.to_string())
                })
                .map_err(crate::launch::LaunchError::Record)?;
        }
        let mut registry = tilo_actions::ActionRegistry::new();
        registry.register::<FollowUp>("bound.apply");
        Ok(AppComponent::new(
            context,
            Box::new(Probe {
                strict,
                collision: None,
                actions: crate::action_host::ActionHost::new(),
            }),
            registry,
        ))
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one actual App lifetime verifies descendant delivery, mutable admission and closed cancellation"
    )]
    fn real_app_descendant_focus_ime_semantics_and_measure_keep_child_binding() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut owners = crate::record::RecordOwners::new(root.clone());
        let records = owners.open("so.tilo.binding-probe", "A").unwrap();
        let views =
            Rc::new(crate::application_view::InstanceViewStore::new(root, "D", "A").unwrap());
        let definition = ApplicationDefinition {
            app_id: "so.tilo.binding-probe",
            title: "Probe",
            icon: IconDescriptor::Glyph("P"),
            record_kind: crate::record::RecordKind::new("probe"),
            required: vec![],
            optional: vec![],
            create: factory,
        };
        let context = HostContext::prepare(
            &definition,
            records.clone(),
            views,
            "D".into(),
            1,
            Rc::new(RefCell::new(
                crate::permissions::PermissionContext::permissive(),
            )),
            crate::permissions::UserId("probe-author".into()),
            tilo_actions::ActionContext::default(),
            &LaunchTarget::New,
        )
        .unwrap();
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let checkout = component.record().clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        component
            .mount(move |event| {
                sender.send(event).unwrap();
            })
            .unwrap();
        let mut outer = crate::wake::WakeScope::new();
        let (tx, rx) = std::sync::mpsc::channel();
        outer
            .bind(move |event| {
                tx.send(event).unwrap();
            })
            .unwrap();
        let mut state = State::new();
        state.doc.put(ROOT, "marker", "shell").unwrap();
        let mut app = App::from_scoped_parts_with_controlled_inputs(
            outer,
            state,
            Box::new(
                Column::new().push(Box::new(
                    crate::widget::Row::new().push(Box::new(
                        crate::widget::Container::new()
                            .with_child(Box::new(TileHost::from_component(component))),
                    )),
                )),
            ),
        );
        CAPTURES.with_borrow_mut(Vec::clear);
        app.init_layout();
        app.refresh_focus_order();
        let leaf = app.focusable_order[0];
        app.set_focused_node(Some(leaf));
        let _ = app.ime_window_state();
        app.dispatch_a11y_action(
            accesskit::NodeId(u64::from(leaf).saturating_add(1)),
            &crate::a11y::Action::Focus,
        );
        let layout_root = app.layout.root_node.unwrap();
        app.layout.layout_dirty = true;
        run_layout_pass(&mut app, layout_root, 400, 300, 1.0, true, None).unwrap();
        assert!(
            tilo_actions::door::resolve(
                &app.collect_local_resources(),
                &tilo_actions::ActionAddress::new("bound:probe", "follow-up")
            )
            .is_some(),
            "fixture must publish its operation before admission"
        );
        let actor = app.collaboration.local_author.0.clone();
        assert!(matches!(
            app.invoke_resource_action(
                &tilo_actions::ActionAddress::new("bound:probe", "follow-up"),
                None,
                &actor,
                false,
                false,
                None,
                Some(tilo_actions::InvocationId::new()),
                None,
            )
            .unwrap(),
            crate::widget::ResourceOperationStart::Accepted
        ));
        assert!(
            records
                .read(&checkout.id, |doc| doc
                    .get(ROOT, "follow-up")
                    .unwrap()
                    .unwrap()
                    .0
                    .to_bool()
                    .unwrap())
                .unwrap()
        );
        assert!(app.state.doc.get(ROOT, "follow-up").unwrap().is_none());
        let captures = CAPTURES.with_borrow_mut(std::mem::take);
        assert_eq!(
            captures
                .iter()
                .filter(|(label, _)| *label == "mutable-operation")
                .count(),
            1
        );
        for label in [
            "build",
            "sync",
            "measure",
            "apply",
            "focus-order",
            "focus",
            "ime",
            "accessibility",
            "action",
        ] {
            assert!(
                captures.iter().any(|(actual, _)| *actual == label),
                "missing {label}"
            );
        }
        for (label, mut request) in captures {
            assert!(request.begin().complete(1));
            assert!(receiver.try_recv().is_ok(), "{label} missed child");
            assert!(rx.try_recv().is_err(), "{label} woke outer");
        }
        let close_probe = *app.actions.bound_invocations.keys().next().unwrap();
        app.actions.local_invocations.insert(
            close_probe,
            (
                tilo_actions::ActionAddress::new("bound:probe", "follow-up"),
                tilo_actions::OperationKind::Effect,
            ),
        );
        app.close_admission.save_waiting = true;
        app.close_admission.save_local = Some(close_probe);
        app.close_components();
        assert_eq!(
            CAPTURES.with_borrow(|c| c
                .iter()
                .filter(|(label, _)| *label == "mutable-cancel")
                .count()),
            1
        );
        assert_eq!(app.component_close_reports().len(), 1);
        app.close_components();
        assert_eq!(app.component_close_reports().len(), 1);
        assert_eq!(app.take_component_close_reports().len(), 1);
        assert!(app.take_component_close_reports().is_empty());
    }
    struct ActionFixture {
        _temp: tempfile::TempDir,
        app: App,
        records: crate::record::RecordStore,
        bindings: Vec<crate::bound_dispatch::CallbackBinding>,
        permissions: Rc<RefCell<crate::permissions::PermissionContext>>,
    }

    #[test]
    fn document_history_keeps_focused_record_after_focus_moves_and_revokes_live() {
        let mut fixture = ActionFixture::new();
        let mut entry_ids = Vec::new();
        for (index, binding) in fixture.bindings.iter().enumerate() {
            let hash = fixture
                .records
                .with(&binding.checkout.id, |doc| {
                    doc.put(ROOT, "history_marker", format!("record-{index}"))
                        .unwrap();
                    doc.commit().unwrap()
                })
                .unwrap();
            entry_ids.push(crate::history_source::entry_id_from_hash(&hash.to_string()));
        }
        fixture
            .app
            .state
            .doc
            .put(ROOT, "history_marker", "outer")
            .unwrap();
        let pending = fixture.app.state.doc.pending_ops();
        let focus_for = |app: &App, instance| {
            app.layout
                .node_map
                .keys()
                .copied()
                .find(|node| {
                    app.bindings
                        .binding_for(*node)
                        .is_some_and(|binding| binding.instance_id == instance)
                })
                .unwrap()
        };
        fixture.app.focused_node = Some(focus_for(&fixture.app, fixture.bindings[0].instance_id));
        fixture.app.open_document_history().unwrap();
        fixture.app.focused_node = Some(focus_for(&fixture.app, fixture.bindings[1].instance_id));
        fixture
            .app
            .select_document_history(&[entry_ids[0]])
            .unwrap();
        let snapshot = fixture.app.document_history_snapshot().unwrap().unwrap();
        let after =
            automerge::AutoCommit::load(&snapshot.comparison.unwrap().after_document).unwrap();
        assert_eq!(
            after
                .get(ROOT, "history_marker")
                .unwrap()
                .unwrap()
                .0
                .to_str(),
            Some("record-0")
        );
        assert!(
            fixture
                .app
                .select_document_history(&[entry_ids[1]])
                .is_err()
        );
        assert_eq!(fixture.app.state.doc.pending_ops(), pending);
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "fixture-user"),
                vec![],
            )
            .at_unix_time(100);
        assert!(fixture.app.document_history_snapshot().is_err());
        assert!(!fixture.app.document_history.is_open());
    }

    #[test]
    fn document_history_does_not_fall_back_when_component_closes() {
        let mut fixture = ActionFixture::new();
        let binding = &fixture.bindings[0];
        fixture.app.focused_node = fixture.app.layout.node_map.keys().copied().find(|node| {
            fixture
                .app
                .bindings
                .binding_for(*node)
                .is_some_and(|candidate| candidate.instance_id == binding.instance_id)
        });
        assert!(fixture.app.focused_node.is_some());
        fixture.app.open_document_history().unwrap();
        binding.closing.set(true);
        fixture.app.focused_node = None;
        assert!(fixture.app.document_history_snapshot().is_err());
        assert!(!fixture.app.document_history.is_open());
    }

    #[test]
    fn document_history_requires_a_choice_when_multiple_components_have_no_focus() {
        let mut fixture = ActionFixture::new();
        fixture.app.focused_node = None;
        assert!(fixture.app.open_document_history().is_err());
        assert!(fixture.app.document_history_snapshot().unwrap().is_none());
    }

    impl ActionFixture {
        fn new() -> Self {
            Self::with_same_record(false)
        }
        fn with_same_record(shared: bool) -> Self {
            Self::with_authority(shared, false)
        }
        fn with_authority(shared: bool, separate: bool) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let mut owners = crate::record::RecordOwners::new(temp.path().into());
            let records = owners
                .open(
                    "so.tilo.binding-probe",
                    "00000000-0000-0000-0000-000000000004",
                )
                .unwrap();
            let views = Rc::new(
                crate::application_view::InstanceViewStore::new(
                    temp.path().into(),
                    "D",
                    "00000000-0000-0000-0000-000000000004",
                )
                .unwrap(),
            );
            let permissions = Rc::new(RefCell::new(
                crate::permissions::PermissionContext::from_capabilities(
                    crate::permissions::Actor::person(None, "fixture-user"),
                    vec![
                        crate::permissions::CapOverride::grant(
                            crate::permissions::Subject::person("fixture-user"),
                            crate::permissions::Scope::Workspace,
                            crate::permissions::Verb::Admin,
                        )
                        .with_expiry(110),
                    ],
                )
                .at_unix_time(100),
            ));
            let mut column = Column::new();
            let mut bindings: Vec<crate::bound_dispatch::CallbackBinding> = Vec::new();
            for (instance, create) in [
                (11, factory as crate::application::ComponentFactory),
                (12, strict_factory),
            ] {
                let definition = ApplicationDefinition {
                    app_id: "so.tilo.binding-probe",
                    title: "Probe",
                    icon: IconDescriptor::Glyph("P"),
                    record_kind: crate::record::RecordKind::new("probe"),
                    required: vec![],
                    optional: vec![],
                    create,
                };
                let target = if shared && !bindings.is_empty() {
                    LaunchTarget::Record {
                        checkout: bindings[0].checkout.clone(),
                        reveal: None,
                    }
                } else {
                    LaunchTarget::New
                };
                let context = HostContext::prepare(
                    &definition,
                    records.clone(),
                    views.clone(),
                    "D".into(),
                    instance,
                    if separate && instance == 12 {
                        Rc::new(RefCell::new(permissions.borrow().clone()))
                    } else {
                        permissions.clone()
                    },
                    crate::permissions::UserId("fixture-user".into()),
                    tilo_actions::ActionContext::default(),
                    &target,
                )
                .unwrap();
                let component = definition
                    .instantiate(context, target, RestoreInput::None)
                    .unwrap();
                bindings.push(component.callback_binding());
                column = column.push(Box::new(TileHost::from_component(component)));
            }
            let mut app = App::new_with_controlled_inputs(State::new(), Box::new(column));
            app.init_layout();
            Self {
                _temp: temp,
                app,
                records,
                bindings,
                permissions,
            }
        }
        fn broker() -> Self {
            let mut fixture = Self::with_same_record(true);
            fixture.app.actions.bound_broker_route_for_test =
                Some(crate::collaboration::PrimaryRouteWitness {
                    route: fixture.bindings[0].broker_route().unwrap(),
                    epoch: uuid::Uuid::new_v4(),
                });
            fixture
        }
        fn target(&self, index: usize, action: &str) -> crate::bound_dispatch::BoundActionTarget {
            let binding = self.bindings[index].clone();
            crate::bound_dispatch::BoundActionTarget {
                address: crate::bound_dispatch::BoundActionAddress {
                    instance_id: binding.instance_id,
                    address: tilo_actions::ActionAddress::new("bound:probe", action),
                },
                binding,
            }
        }
    }

    include!("app/bound_record_sync_tests.rs");

    #[test]
    fn task4_bound_publication_preserves_contracts_and_directed_owner() {
        let mut fixture = ActionFixture::broker();
        let outer = fixture.app.state.doc.get_heads();
        let address = tilo_actions::ActionAddress::new("bound:probe", "follow-up");
        let actor = fixture.app.collaboration.local_author.0.clone();
        let error = fixture
            .app
            .invoke_resource_action(&address, None, &actor, true, false, None, None, None)
            .unwrap_err();
        assert_eq!(
            crate::bound_dispatch::door_error_code(&error),
            "ambiguous_action_target"
        );
        let resources = fixture.app.publication_resources();
        let first = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[0], "bound:probe")
            .unwrap();
        let second = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[1], "bound:probe")
            .unwrap();
        assert_ne!(first, second);
        let descriptor = |id: &str| {
            tilo_actions::door::resolve(
                &resources,
                &tilo_actions::ActionAddress::new(id, "follow-up"),
            )
            .unwrap()
        };
        assert_eq!(
            descriptor(&first).contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
        );
        assert_eq!(
            descriptor(&second).contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        let revision = fixture.app.action_directory_revision();
        assert!(matches!(
            fixture
                .app
                .resolve_action_directory(&revision, &second, "follow-up"),
            runtime_collab::ActionDirectoryResolution::Found(_)
        ));
        fixture
            .app
            .invoke_resource_action(
                &tilo_actions::ActionAddress::new(&second, "follow-up"),
                None,
                &actor,
                true,
                true,
                Some(&serde_json::json!({"confirm": true})),
                Some(tilo_actions::InvocationId::new()),
                None,
            )
            .unwrap();
        assert!(
            fixture
                .records
                .read_history(&fixture.bindings[0].checkout.id, |_, history| history
                    .next_undo_in(&fixture.bindings[0].author, 11)
                    .is_none())
                .unwrap()
        );
        fixture
            .records
            .with_history(&fixture.bindings[1].checkout.id, |doc, history| {
                assert!(doc.get(ROOT, "follow-up").unwrap().is_some());
                assert_eq!(
                    history
                        .next_undo_in(&fixture.bindings[1].author, 12)
                        .unwrap()
                        .instance_id,
                    Some(12)
                );
            })
            .unwrap();
        assert_eq!(fixture.app.state.doc.get_heads(), outer);
    }

    #[test]
    fn task4_bound_cache_tracks_permission_time_and_never_reuses_exhaustion() {
        let mut fixture = ActionFixture::new();
        DISCOVERY_COUNT.set(0);
        let _ = fixture.app.collect_resources();
        let first = DISCOVERY_COUNT.get();
        let _ = fixture.app.collect_resources();
        assert_eq!(DISCOVERY_COUNT.get(), first);
        let expired = fixture.permissions.borrow().clone().at_unix_time(120);
        *fixture.permissions.borrow_mut() = expired;
        let resources = fixture.app.collect_resources();
        assert!(
            tilo_actions::door::resolve(
                &resources,
                &tilo_actions::ActionAddress::new("bound:probe", "follow-up")
            )
            .is_none()
        );
        assert!(DISCOVERY_COUNT.get() > first);
        let expired = fixture.permissions.borrow().clone();
        *fixture.permissions.borrow_mut() = expired.at_unix_time(100);
        fixture
            .records
            .with_action_state(&fixture.bindings[0].checkout.id, |_, _, state| {
                state.generation = Some(u64::MAX);
            })
            .unwrap();
        fixture.bindings[0]
            .with_document(&mut fixture.app.state.doc, |_, _| {})
            .unwrap();
        let _ = fixture.app.collect_resources();
        let before = DISCOVERY_COUNT.get();
        let _ = fixture.app.collect_resources();
        assert!(DISCOVERY_COUNT.get() > before);
        assert_eq!(
            fixture
                .records
                .action_generation(&fixture.bindings[0].checkout.id)
                .unwrap(),
            None
        );
    }

    #[test]
    fn task4_semantic_and_palette_targets_keep_equal_id_lifetimes() {
        let mut fixture = ActionFixture::new();
        fixture.app.refresh_semantic_tree();
        let targets = fixture
            .app
            .accessibility
            .semantic_tree
            .bound_actions_for_test();
        let (id, _) = targets
            .iter()
            .find(|(_, target)| {
                target.address.instance_id == 12 && target.address.address.action == "view"
            })
            .unwrap();
        let outer = fixture.app.state.doc.get_heads();
        assert!(
            fixture
                .app
                .invoke_semantic_action(id, "view", None)
                .unwrap()
        );
        fixture.app.open_action_palette();
        let target = fixture.target(0, "view");
        assert!(
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .activate_bound(&target)
        );
        let pending = fixture
            .app
            .actions
            .action_palette
            .as_mut()
            .unwrap()
            .take_operation()
            .unwrap();
        assert!(
            pending
                .bound
                .as_ref()
                .unwrap()
                .binding
                .same_component(&target.binding)
        );
        assert_eq!(fixture.app.state.doc.get_heads(), outer);
        fixture.app.close_components();
        assert!(
            fixture
                .app
                .invoke_bound_target(
                    &target,
                    None,
                    &fixture.app.collaboration.local_author.0.clone(),
                    true,
                    false,
                    None,
                    None,
                    None
                )
                .is_err()
        );
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one controlled lifetime traces publication through refusal and delivery"
    )]
    fn task4_bound_directory_limits_cursors_and_alias_refusal() {
        let mut fixture = ActionFixture::broker();
        let query = runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: String::new(),
            cursor: vec![],
            needle: String::new(),
            parent_resource: String::new(),
            max_results: 1,
            max_bytes: 8192,
            max_work: 16,
            max_depth: 8,
        };
        let first = fixture.app.action_directory_page(query.clone());
        assert_eq!(first.rows.len(), 1);
        assert!(!first.next_cursor.is_empty());
        let second = fixture
            .app
            .action_directory_page(runtime_collab::ActionDirectoryQuery {
                revision: first.revision.clone(),
                cursor: first.next_cursor.clone(),
                ..query.clone()
            });
        assert_eq!(second.rows.len(), 1);
        assert_ne!(first.rows[0].resource, second.rows[0].resource);
        assert_eq!(first.rows[0].visibility_action.as_deref(), Some("view"));
        assert_eq!(
            second.rows[0].visibility_action.as_deref(),
            Some("follow-up")
        );
        let eager = fixture.app.publication_resources();
        for page in [&first, &second] {
            assert!(
                eager
                    .iter()
                    .any(|resource| resource.id.as_str() == page.rows[0].resource)
            );
        }
        let alias = second.rows[0].resource.clone();
        let previous_permissions = fixture.permissions.borrow().clone();
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "fixture-user"),
                vec![],
            )
            .at_unix_time(100);
        let stale = fixture
            .app
            .action_directory_page(runtime_collab::ActionDirectoryQuery {
                revision: first.revision.clone(),
                cursor: first.next_cursor,
                ..query
            });
        assert!(stale.stale_cursor);
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "follow-up")
                .is_none()
        );
        let actor = fixture.app.collaboration.local_author.0.clone();
        assert!(
            fixture
                .app
                .invoke_resource_action(
                    &tilo_actions::ActionAddress::new(&alias, "follow-up"),
                    None,
                    &actor,
                    true,
                    true,
                    None,
                    None,
                    None,
                )
                .is_err()
        );
        *fixture.permissions.borrow_mut() = previous_permissions;
        for malformed in [
            format!("{}broken", crate::bound_dispatch::PUBLICATION_PREFIX),
            format!("{alias}:extra"),
            format!(
                "{}{}",
                crate::bound_dispatch::PUBLICATION_PREFIX,
                "x".repeat(4096)
            ),
        ] {
            assert!(
                fixture
                    .app
                    .actions
                    .bound_publication
                    .target(&tilo_actions::ActionAddress::new(&malformed, "view"))
                    .is_err()
            );
            assert!(
                fixture
                    .app
                    .resolve_publication_resource(&malformed, "view")
                    .is_none()
            );
        }
        fixture.app.close_components();
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "follow-up")
                .is_none()
        );
        let mut replacement = ActionFixture::broker();
        fixture.app.actions.bound_broker_route_for_test =
            replacement.app.actions.bound_broker_route_for_test.clone();
        fixture.app.root_widget = std::mem::replace(
            &mut replacement.app.root_widget,
            Box::new(crate::widget::Text::new("empty")),
        );
        fixture.app.invalidate_widget_resource_cache();
        let replacements = fixture.app.publication_resources();
        assert!(!replacements.is_empty());
        assert!(
            replacements
                .iter()
                .all(|resource| resource.id.as_str() != alias)
        );
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "view")
                .is_none()
        );
    }

    #[test]
    fn task4_compound_view_and_noop_effect_preserve_pending_typing() {
        for (action, mode) in [("view", 0), ("follow-up", 2), ("view", 1)] {
            COMPOUND_MODE.set(mode);
            let mut fixture = ActionFixture::with_same_record(true);
            let a = fixture.bindings[0].clone();
            let b = fixture.bindings[1].clone();
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "first").unwrap();
            })
            .unwrap();
            let pending_before = fixture
                .records
                .read(
                    &a.checkout.id,
                    automerge::transaction::Transactable::pending_ops,
                )
                .unwrap();
            let target = fixture.target(1, action);
            fixture
                .app
                .invoke_bound_target(
                    &target,
                    None,
                    "fixture-user",
                    true,
                    false,
                    None,
                    Some(tilo_actions::InvocationId(uuid::Uuid::from_u128(91))),
                    None,
                )
                .unwrap();
            if mode == 0 {
                assert_eq!(
                    fixture
                        .records
                        .read(
                            &a.checkout.id,
                            automerge::transaction::Transactable::pending_ops
                        )
                        .unwrap(),
                    pending_before
                );
            }
            let b_entry = fixture
                .records
                .read_history(&a.checkout.id, |_, history| {
                    history.next_undo_in(&b.author, b.instance_id).cloned()
                })
                .unwrap();
            assert_eq!(b_entry.is_some(), mode == 1);
            if mode != 1 {
                assert!(
                    fixture
                        .records
                        .read_history(&a.checkout.id, |_, history| history
                            .next_undo_in(&a.author, a.instance_id)
                            .is_none())
                        .unwrap()
                );
            }
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "continued").unwrap();
            })
            .unwrap();
            a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            fixture
                .records
                .read(&a.checkout.id, |doc| {
                    assert_eq!(doc.get(ROOT, "typing").unwrap().is_some(), mode == 1);
                    assert_eq!(doc.get(ROOT, "follow-up").unwrap().is_some(), mode == 1);
                })
                .unwrap();
            COMPOUND_MODE.set(0);
        }
    }

    #[test]
    fn task4_undeclared_effect_spans_are_retained_excluded_and_keep_custody() {
        for mode in [3, 4, 5, 6] {
            COMPOUND_MODE.set(mode);
            CAPTURES.with_borrow_mut(Vec::clear);
            let mut fixture = ActionFixture::with_same_record(true);
            let a = fixture.bindings[0].clone();
            let b = fixture.bindings[1].clone();
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "prior").unwrap();
            })
            .unwrap();
            let generation = fixture.records.action_generation(&a.checkout.id).unwrap();
            let target = fixture.target(1, "follow-up");
            let error = fixture
                .app
                .invoke_bound_target(
                    &target,
                    None,
                    "fixture-user",
                    true,
                    false,
                    None,
                    Some(tilo_actions::InvocationId(uuid::Uuid::from_u128(92))),
                    None,
                )
                .unwrap_err();
            assert_eq!(
                crate::bound_dispatch::door_error_code(&error),
                "undeclared_document_mutation"
            );
            assert!(error.to_string().contains("retained"));
            assert_ne!(
                fixture.records.action_generation(&a.checkout.id).unwrap(),
                generation
            );
            b.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "later", true).unwrap();
            })
            .unwrap();
            b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            if mode != 3 {
                b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                    .unwrap()
                    .unwrap();
            }
            assert!(!b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo);
            a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            fixture
                .records
                .read(&a.checkout.id, |doc| {
                    assert!(doc.get(ROOT, "later").unwrap().is_none());
                    assert!(doc.get(ROOT, "typing").unwrap().is_none());
                    assert!(doc.get(ROOT, "follow-up").unwrap().is_none());
                    assert_eq!(
                        doc.get(ROOT, "raw-before").unwrap().is_some(),
                        matches!(mode, 3 | 4 | 6)
                    );
                    assert_eq!(
                        doc.get(ROOT, "raw-after").unwrap().is_some(),
                        matches!(mode, 5 | 6)
                    );
                })
                .unwrap();
            drop(fixture);
            assert!(CAPTURES.with_borrow(|captures| {
                captures.iter().any(|(name, _)| *name == "mutable-cancel")
            }));
            COMPOUND_MODE.set(0);
        }
    }

    #[test]
    fn task4_compound_contextual_optimistic_noop_and_failed_producers() {
        for mode in [7, 8, 9, 10] {
            COMPOUND_MODE.set(mode);
            let mut fixture = ActionFixture::with_same_record(true);
            let a = fixture.bindings[0].clone();
            let b = fixture.bindings[1].clone();
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "prior").unwrap();
            })
            .unwrap();
            let target = fixture.target(1, "follow-up");
            fixture
                .app
                .invoke_bound_target(&target, None, "fixture-user", true, false, None, None, None)
                .unwrap();
            assert_eq!(
                b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo,
                mode < 9
            );
            if mode < 9 {
                b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                    .unwrap()
                    .unwrap();
                assert!(!b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo);
            } else {
                assert!(
                    fixture
                        .records
                        .read_history(&a.checkout.id, |_, history| history
                            .next_undo_in(&a.author, a.instance_id)
                            .is_none())
                        .unwrap()
                );
            }
            a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            fixture
                .records
                .read(&a.checkout.id, |doc| {
                    assert!(doc.get(ROOT, "typing").unwrap().is_none());
                    assert!(doc.get(ROOT, "follow-up").unwrap().is_none());
                    assert!(doc.get(ROOT, "failed-action").unwrap().is_none());
                })
                .unwrap();
            COMPOUND_MODE.set(0);
        }
    }

    #[test]
    fn task4_compound_unwind_restores_frame_and_preserves_unrelated_redo() {
        COMPOUND_MODE.set(11);
        let mut fixture = ActionFixture::with_same_record(true);
        let a = fixture.bindings[0].clone();
        let b = fixture.bindings[1].clone();
        a.with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.put(ROOT, "typing", "prior").unwrap();
        })
        .unwrap();
        a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
            .unwrap()
            .unwrap();
        let target = fixture.target(1, "follow-up");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = fixture.app.invoke_bound_target(
                &target,
                None,
                "fixture-user",
                true,
                false,
                None,
                None,
                None,
            );
        }));
        assert!(result.is_err());
        COMPOUND_MODE.set(0);
        assert!(a.undo_status(&mut fixture.app.state.doc).unwrap().can_redo);
        assert!(!b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo);
        b.with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.put(ROOT, "later", true).unwrap();
        })
        .unwrap();
        b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
            .unwrap()
            .unwrap();
        a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Redo)
            .unwrap()
            .unwrap();
        fixture
            .records
            .read(&a.checkout.id, |doc| {
                assert!(doc.get(ROOT, "raw-unwind").unwrap().is_some());
                assert!(doc.get(ROOT, "typing").unwrap().is_some());
                assert!(doc.get(ROOT, "later").unwrap().is_none());
            })
            .unwrap();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "fresh and revoked authority share the full visible-row and activation oracle"
    )]
    fn task4_denied_bound_provider_rows_are_not_disclosed() {
        for initially_allowed in [false, true] {
            let mut fixture = ActionFixture::with_authority(false, true);
            let denied = crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "fixture-user"),
                vec![],
            )
            .at_unix_time(100);
            if !initially_allowed {
                *fixture.permissions.borrow_mut() = denied.clone();
            }
            fixture.app.open_action_palette();
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_query("sealed");
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .advance_search_page();
            if initially_allowed {
                assert!(
                    fixture
                        .app
                        .action_palette()
                        .unwrap()
                        .result_labels()
                        .contains(&"Hidden sealed write title".to_owned())
                );
                *fixture.permissions.borrow_mut() = denied;
                fixture.app.refresh_action_palette_resources();
                fixture
                    .app
                    .actions
                    .action_palette
                    .as_mut()
                    .unwrap()
                    .advance_search_page();
            }
            let labels = fixture.app.action_palette().unwrap().result_labels();
            assert!(!labels.contains(&"Hidden sealed write title".to_owned()));
            let allowed = labels
                .iter()
                .position(|label| label == "Allowed sealed sibling title")
                .unwrap();
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_cursor(allowed);
            assert!(
                fixture
                    .app
                    .action_palette_handle_key(&action_palette_tests::synth_enter())
            );
            for (index, binding) in fixture.bindings.iter().enumerate() {
                fixture
                    .records
                    .read(&binding.checkout.id, |doc| {
                        assert_eq!(doc.get(ROOT, "follow-up").unwrap().is_some(), index == 1);
                    })
                    .unwrap();
            }
            fixture.app.open_action_palette();
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_query("sealedopaque");
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .advance_search_page();
            let labels = fixture.app.action_palette().unwrap().result_labels();
            assert!(!labels.contains(&"Hidden sealedopaque title".to_owned()));
            assert!(labels.contains(&"Allowed sealedopaque title".to_owned()));
            assert!(
                fixture
                    .records
                    .read(&fixture.bindings[0].checkout.id, |doc| doc
                        .get(ROOT, "navigation")
                        .unwrap()
                        .is_none())
                    .unwrap()
            );
            assert!(
                fixture
                    .app
                    .state
                    .doc
                    .get(ROOT, "follow-up")
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one controlled paging fixture covers completion, revocation and prompt pause"
    )]
    fn task4_completed_bound_search_validation_is_bounded() {
        let mut fixture = ActionFixture::with_authority(false, true);
        let confirmation = fixture.target(1, "follow-up");
        let counts = Rc::new([Cell::new(0), Cell::new(0)]);
        PAGED_SEARCH_COUNTS.with_borrow_mut(|slot| *slot = Some(counts.clone()));
        fixture.app.open_action_palette();
        PAGED_SEARCH_COUNTS.with_borrow_mut(|slot| *slot = None);
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        palette.set_query("paged");
        let mut ticks = 0;
        while palette.advance_search_page().pending {
            ticks += 1;
            assert!(ticks < 20, "initial bounded query must finish");
        }
        assert_eq!(palette.result_labels().len(), 130, "no total-kind clipping");
        let step = |palette: &mut crate::action_palette::ActionPalette| {
            for counter in counts.iter() {
                counter.set(0);
            }
            let progress = palette.advance_search_page();
            let callbacks: usize = counts.iter().map(Cell::get).sum();
            assert!(
                callbacks <= 32,
                "{callbacks} resolver callbacks in one step"
            );
            assert!(progress.inspected <= 32);
            assert!(callbacks <= progress.inspected, "unaccounted provider work");
            progress
        };
        // A completed-query redraw starts at most one finite validation pass.
        let mut progress = step(palette);
        ticks = 0;
        while progress.pending {
            progress = step(palette);
            ticks += 1;
            assert!(
                ticks < 20,
                "completed validation must stop requesting redraws"
            );
        }
        assert_eq!(palette.result_labels().len(), 130);
        // Revoke during the next finite pass after this owner was visited;
        // queue order must not make the already-validated-row oracle accidental.
        ticks = 0;
        loop {
            assert!(step(palette).pending);
            if counts[0].get() > 0 {
                break;
            }
            ticks += 1;
            assert!(ticks < 5, "the first owner must participate in this pass");
        }
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "fixture-user"),
                vec![],
            )
            .at_unix_time(100);
        ticks = 0;
        loop {
            progress = step(palette);
            assert!(
                palette
                    .result_labels()
                    .iter()
                    .all(|label| !label.starts_with("Revoked")),
                "unchecked stale rows must remain hidden"
            );
            if !progress.pending {
                break;
            }
            ticks += 1;
            assert!(ticks < 20, "revocation validation must finish");
        }
        assert_eq!(palette.result_labels().len(), 65);
        assert!(
            palette
                .result_labels()
                .iter()
                .all(|label| label.starts_with("Allowed"))
        );
        // The existing strict owner's follow-up enters the real confirmation
        // field. A partially drained validation pass must pause behind it.
        palette.set_query("");
        for tick in 0..20 {
            if !palette.advance_search_page().pending {
                break;
            }
            assert!(tick < 19, "reset query must complete");
        }
        assert!(step(palette).pending);
        assert!(palette.activate_bound(&confirmation));
        assert!(palette.in_argument_mode());
        assert!(palette.result_labels().is_empty());
        for _ in 0..2 {
            let progress = step(palette);
            assert_eq!(progress.inspected, 0);
            assert!(!progress.pending);
            assert!(palette.result_labels().is_empty());
        }
        palette.open();
        for tick in 0..20 {
            if !palette.advance_search_page().pending {
                break;
            }
            assert!(tick < 19, "reset query must complete");
        }
        assert_eq!(
            palette
                .result_labels()
                .iter()
                .filter(|label| label.starts_with("Allowed"))
                .count(),
            65
        );
    }

    #[test]
    fn task4_pending_status_selects_its_owner_without_closing_typing() {
        let mut fixture = ActionFixture::new();
        let binding = fixture.bindings[0].clone();
        let outer = fixture.app.state.doc.get_heads();
        binding
            .with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "pending").unwrap();
            })
            .unwrap();
        let pending = fixture
            .records
            .read(
                &binding.checkout.id,
                automerge::transaction::Transactable::pending_ops,
            )
            .unwrap();
        assert!(
            binding
                .undo_status(&mut fixture.app.state.doc)
                .unwrap()
                .can_undo
        );
        assert!(
            !fixture.bindings[1]
                .undo_status(&mut fixture.app.state.doc)
                .unwrap()
                .can_undo
        );
        assert_eq!(
            fixture
                .records
                .read(
                    &binding.checkout.id,
                    automerge::transaction::Transactable::pending_ops
                )
                .unwrap(),
            pending
        );
        let entry = binding
            .history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
            .unwrap()
            .unwrap();
        assert_eq!(entry.instance_id, Some(11));
        assert!(
            binding
                .undo_status(&mut fixture.app.state.doc)
                .unwrap()
                .can_redo
        );
        assert!(
            fixture
                .records
                .read(&binding.checkout.id, |doc| doc
                    .get(ROOT, "typing")
                    .unwrap()
                    .is_none())
                .unwrap()
        );
        assert_eq!(fixture.app.state.doc.get_heads(), outer);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one controlled lifetime traces publication through refusal and delivery"
    )]
    fn task4_qualified_broker_request_retains_chosen_contract_and_directed_owner() {
        let mut fixture = ActionFixture::broker();
        let target = fixture.target(1, "follow-up");
        let resources = fixture.app.publication_resources();
        let actor = fixture.app.collaboration.local_author.0.clone();
        let evidence = serde_json::json!({"token": "controlled-confirmation"});
        let mut requests = Vec::new();
        // This seam captures submission only; no broker acceptance is simulated.
        fixture
            .app
            .invoke_bound_target_with_broker(
                &target,
                None,
                &actor,
                true,
                false,
                Some(&evidence),
                None,
                None,
                Some(&mut |request| requests.push(request)),
            )
            .unwrap();
        assert_eq!(requests.len(), 1);
        let CollabRequest::InvokePublishedAction {
            request_id,
            peer_id,
            resource,
            action,
            arguments,
            confirmation,
        } = requests.pop().unwrap()
        else {
            panic!("expected action request")
        };
        assert_eq!(peer_id, crate::collaboration::CURRENT_CONNECTION_PEER_ID);
        assert_eq!(request_id, 0);
        assert!(arguments.is_empty());
        assert_eq!(confirmation, evidence);
        let descriptor = tilo_actions::door::resolve(
            &resources,
            &tilo_actions::ActionAddress::new(&resource, &action),
        )
        .unwrap();
        assert_eq!(
            descriptor.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert!(
            fixture
                .app
                .actions
                .bound_publication
                .target(&tilo_actions::ActionAddress::new(&resource, &action))
                .unwrap()
                .unwrap()
                .binding
                .same_component(&target.binding)
        );
        for binding in &fixture.bindings {
            assert!(
                fixture
                    .records
                    .read(&binding.checkout.id, |doc| doc
                        .get(ROOT, "follow-up")
                        .unwrap()
                        .is_none())
                    .unwrap()
            );
        }
        fixture
            .app
            .invoke_published_action(41, resource, action, None, &actor, None);
        assert!(
            fixture
                .records
                .read_history(&fixture.bindings[0].checkout.id, |_, history| history
                    .next_undo_in(&fixture.bindings[0].author, 11)
                    .is_none())
                .unwrap()
        );
        assert!(
            fixture
                .records
                .read(&target.binding.checkout.id, |doc| doc
                    .get(ROOT, "follow-up")
                    .unwrap()
                    .is_some())
                .unwrap()
        );
        assert_eq!(
            fixture
                .records
                .read_history(&target.binding.checkout.id, |_, history| history.revision())
                .unwrap(),
            1
        );
        let before = fixture.app.actions.next_broker_request_id;
        let mut sent = false;
        assert!(
            App::dispatch_broker_request(
                &mut fixture.app.actions.next_broker_request_id,
                false,
                &target.address.address,
                None,
                None,
                None,
                |_| sent = true
            )
            .is_err()
        );
        assert!(!sent);
        assert_eq!(fixture.app.actions.next_broker_request_id, before);
    }

    #[test]
    fn task4_checked_action_reuses_same_record_frame_and_refuses_cross_record() {
        for shared in [true, false] {
            let mut fixture = ActionFixture::with_same_record(shared);
            let target = fixture.target(1, "apply");
            let first = fixture.bindings[0].clone();
            let outer = fixture.app.state.doc.get_heads();
            let actor = fixture.app.collaboration.local_author.0.clone();
            let result = first
                .with_document(&mut fixture.app.state.doc, |doc, _| {
                    doc.put(ROOT, "before-nested", "A").unwrap();
                    let result = crate::bound_dispatch::invoke_resource_action(
                        fixture.app.root_widget.as_mut(),
                        doc,
                        &target,
                        fixture.app.actions.action_transport.as_mut(),
                        None,
                        None,
                        &actor,
                        true,
                        false,
                        tilo_actions::InvocationId::new(),
                        None,
                    );
                    doc.put(ROOT, "after-nested", "A").unwrap();
                    result
                })
                .unwrap();
            if shared {
                result.unwrap();
                first
                    .history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                    .unwrap()
                    .unwrap();
                fixture
                    .records
                    .read_history(&first.checkout.id, |doc, history| {
                        assert!(doc.get(ROOT, "after-nested").unwrap().is_none());
                        assert!(doc.get(ROOT, "before-nested").unwrap().is_some());
                        assert!(doc.get(ROOT, "follow-up").unwrap().is_some());
                        assert_eq!(
                            history.next_undo_in(&first.author, 12).unwrap().instance_id,
                            Some(12)
                        );
                    })
                    .unwrap();
            } else {
                assert!(matches!(
                    result,
                    Err(crate::bound_dispatch::BoundActionError::Dispatch(
                        crate::bound_dispatch::BoundDispatchError::CrossRecordReentry
                    ))
                ));
                assert!(
                    fixture
                        .records
                        .read(&target.binding.checkout.id, |doc| doc
                            .get(ROOT, "follow-up")
                            .unwrap()
                            .is_none())
                        .unwrap()
                );
            }
            assert_eq!(fixture.app.state.doc.get_heads(), outer);
        }
    }
    #[test]
    fn task4_lazy_palette_and_opaque_navigation_keep_provider_owner() {
        let mut fixture = ActionFixture::new();
        fixture.app.open_action_palette();
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        palette.set_query("lazy");
        for _ in 0..4 {
            palette.advance_search_page();
        }
        let targets = palette.referenced_bound_targets();
        assert_eq!(targets.len(), 2);
        fixture.app.refresh_action_palette_resources();
        let target = targets
            .iter()
            .find(|target| target.address.instance_id == 12)
            .unwrap();
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        assert!(palette.activate_bound(target));
        let pending = palette.take_operation().unwrap();
        assert!(
            pending
                .bound
                .unwrap()
                .binding
                .same_component(&target.binding)
        );
        fixture
            .app
            .invoke_bound_target(target, None, "fixture-user", true, false, None, None, None)
            .unwrap();
        fixture.app.open_action_palette();
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        palette.set_query("opaque");
        for _ in 0..4 {
            palette.advance_search_page();
        }
        assert_eq!(palette.len(), 2);
        palette.set_cursor(1);
        palette.activate_selected();
        let (id, binding) = palette.take_scoped_navigation().unwrap();
        let binding = binding.unwrap();
        crate::bound_dispatch::activate_bound_search(
            fixture.app.root_widget.as_mut(),
            &mut fixture.app.state.doc,
            &binding,
            &id,
        )
        .unwrap();
        for owner in &fixture.bindings {
            assert_eq!(
                fixture
                    .records
                    .read(&owner.checkout.id, |doc| doc
                        .get(ROOT, "navigation")
                        .unwrap()
                        .is_some())
                    .unwrap(),
                owner.same_component(&binding)
            );
        }
        assert!(fixture.app.state.doc.get_heads().is_empty());
    }

    #[test]
    fn task4_reserved_publication_collision_never_routes_to_legacy_widget() {
        let mut fixture = ActionFixture::broker();
        let alias = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[1], "bound:probe")
            .unwrap();
        let old = std::mem::replace(
            &mut fixture.app.root_widget,
            Box::new(crate::widget::Text::new("empty")),
        );
        fixture.app.root_widget = Box::new(
            Column::new()
                .push(Box::new(Probe {
                    collision: Some(alias.clone()),
                    ..Probe::default()
                }))
                .push(old),
        );
        fixture
            .app
            .actions
            .action_registry
            .register::<FollowUp>("bound.apply");
        fixture.app.invalidate_widget_resource_cache();
        let resources = fixture.app.publication_resources();
        assert_eq!(
            resources
                .iter()
                .filter(|resource| resource.id.as_str() == alias)
                .count(),
            1
        );
        let descriptor = tilo_actions::door::resolve(
            &resources,
            &tilo_actions::ActionAddress::new(&alias, "follow-up"),
        )
        .unwrap();
        assert_eq!(
            descriptor.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert!(
            fixture
                .app
                .actions
                .bound_publication
                .alias(&fixture.bindings[0], &"x".repeat(4096))
                .is_err()
        );
    }
    fn assert_bound_broker_route_refused(
        fixture: &mut ActionFixture,
        target: &crate::bound_dispatch::BoundActionTarget,
        alias: &str,
        revision: &str,
        witness: Option<crate::collaboration::PrimaryRouteWitness>,
    ) {
        fixture.app.actions.bound_broker_route_for_test = witness;
        let resources = fixture.app.publication_resources();
        assert!(resources.iter().all(|resource| {
            !resource
                .id
                .as_str()
                .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
        }));
        assert!(
            fixture
                .app
                .resolve_publication_resource(alias, "follow-up")
                .is_none()
        );
        assert_ne!(fixture.app.action_directory_revision(), revision);
        let next_id = fixture.app.actions.next_broker_request_id;
        let mut sent = false;
        let error = fixture
            .app
            .invoke_bound_target_with_broker(
                target,
                None,
                "fixture-user",
                true,
                false,
                None,
                None,
                None,
                Some(&mut |_| sent = true),
            )
            .unwrap_err();
        assert_eq!(
            crate::bound_dispatch::door_error_code(&error),
            "bound_broker_route_unavailable"
        );
        assert!(!sent);
        assert_eq!(fixture.app.actions.next_broker_request_id, next_id);
        fixture.app.invoke_published_action(
            50,
            alias.to_owned(),
            "follow-up".into(),
            None,
            "fixture-user",
            None,
        );
        assert!(
            fixture
                .records
                .read(&target.binding.checkout.id, |doc| doc
                    .get(ROOT, "follow-up")
                    .unwrap()
                    .is_none())
                .unwrap()
        );
        // Local component View actions do not need this manager's broker route.
        fixture
            .app
            .invoke_bound_target(
                &fixture.target(1, "view"),
                None,
                "fixture-user",
                true,
                false,
                None,
                None,
                None,
            )
            .unwrap();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one broker-route refusal test exercises every invalid witness against the same fixture"
    )]
    fn task4_bound_broker_route_refuses_wrong_app_workspace_record_and_missing_witness() {
        let mut fixture = ActionFixture::broker();
        let target = fixture.target(1, "follow-up");
        let matching = fixture
            .app
            .actions
            .bound_broker_route_for_test
            .clone()
            .unwrap();
        assert_eq!(fixture.app.publication_app_id(), matching.route.app_id);
        let alias = fixture
            .app
            .actions
            .bound_publication
            .alias(&target.binding, "bound:probe")
            .unwrap();
        let revision = fixture.app.action_directory_revision();
        let mut wrong_app = matching.clone();
        wrong_app.route.app_id = "so.tilo.other".into();
        let mut wrong_workspace = matching.clone();
        wrong_workspace.route.cluster_id = uuid::Uuid::nil();
        let mut wrong_record = matching.clone();
        wrong_record.route.record = Some("different-record".into());
        for witness in [
            Some(wrong_app),
            Some(wrong_workspace),
            Some(wrong_record),
            None,
        ] {
            assert_bound_broker_route_refused(&mut fixture, &target, &alias, &revision, witness);
        }
        let mut replacement = matching;
        replacement.epoch = uuid::Uuid::new_v4();
        fixture.app.actions.bound_broker_route_for_test = Some(replacement);
        assert_ne!(fixture.app.action_directory_revision(), revision);
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "follow-up")
                .is_some()
        );
    }

    #[test]
    fn task4_bound_control_authority_refreshes_without_an_outer_generation_change() {
        let mut fixture = ActionFixture::new();
        CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
        fixture.app.refresh_control_action_availability();
        assert_eq!(
            CONTROL_AUTHORITY.with_borrow(Clone::clone),
            vec![(false, true), (true, true)]
        );
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "fixture-user"),
                vec![],
            )
            .at_unix_time(100);
        CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
        fixture.app.refresh_control_action_availability();
        assert_eq!(
            CONTROL_AUTHORITY.with_borrow(Clone::clone),
            vec![(false, false), (true, false)]
        );
    }
}

#[cfg(test)]
pub(crate) fn draw_prepared_component_test_frame(app: &mut App) {
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    app.layout.layout_dirty = true;
    run_layout_pass(app, root, 800, 600, 1.0, true, None).unwrap();
    let mut scene = Scene::new();
    draw_frame_content(app, &mut scene, 800, 600, 1.0, None);
}

#[cfg(test)]
mod prepared_runner_tests {
    use super::*;
    use crate::application::{AppComponent, LaunchTarget, RestoreInput};
    use crate::application_test_support::*;
    use crate::widget::TileHost;
    use automerge::ReadDoc;
    use winit::keyboard::{Key, NamedKey};

    fn editable_component(host: &ProbeHost, instance: u64) -> AppComponent {
        runner_probe_definition()
            .instantiate(
                host.prepare_with_permissions(
                    runner_probe_definition(),
                    instance,
                    &LaunchTarget::New,
                    runner_permissions(),
                )
                .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap()
    }

    #[test]
    fn prepared_native_services_keep_explicit_presentation_and_watch_system_updates() {
        fn services(
            inputs: crate::application::ComponentRunnerInputs,
            binding: &crate::bound_dispatch::CallbackBinding,
        ) -> AppConstructionInputs {
            let mut services = AppConstructionInputs::controlled();
            services.debug_mode = || true;
            let mut presence = PresenceCtx::empty();
            presence.set_own_identity(["announced-handle"]);
            services.presence_ctx = ConstructionValue::Ready(presence);
            AppConstructionInputs::component_with_services(inputs, binding, services)
        }
        let host = ProbeHost::new();
        let component = editable_component(&host, 11);
        let mut app =
            App::from_component_using(component, presentation_inputs(2.0), services).unwrap();
        assert_eq!(app.local_author(), &author());
        assert!(app.presence_ctx.is_own_identity(&author().0));
        assert!(app.presence_ctx.is_own_identity("announced-handle"));
        assert!(app.debug_mode);
        assert!(app.watch_system_settings);
        assert_presentation(&app.theme, 1.5, 1.25);
        let due = app.last_theme_store_check + Duration::from_millis(750);
        assert!(app.reconcile_configured_theme_with(
            due,
            || Some(std::time::SystemTime::UNIX_EPOCH),
            |app| {
                app.accessibility.base_theme = Theme::light();
            },
        ));
        assert_eq!(app.theme.name, Theme::light().name);
        assert_eq!(app.theme.text_scale, 1.5);
        assert_eq!(app.theme.ui_scale, 1.25);
        assert!(app.theme.reduce_motion);
        draw_prepared_component_test_frame(&mut app);
        app.close_components();
        assert_eq!(app.component_close_reports().len(), 1);
        assert!(app.component_close_reports()[0].record_error.is_none());
    }

    #[test]
    fn prepared_native_due_theme_poll_preserves_explicit_inputs_without_discovery() {
        let host = ProbeHost::new();
        let component = editable_component(&host, 11);
        let mut app = App::from_component(component, presentation_inputs(2.0)).unwrap();
        let observed = observe_presentation();
        let checks = std::cell::Cell::new(0);
        let applies = std::cell::Cell::new(0);
        let due = app.last_theme_store_check + Duration::from_millis(750);
        let changed = app.reconcile_configured_theme_with(
            due,
            || {
                checks.set(checks.get() + 1);
                Some(std::time::SystemTime::UNIX_EPOCH)
            },
            |app| {
                applies.set(applies.get() + 1);
                app.accessibility.base_theme = Theme::light();
                // This controlled stand-in never creates a narrator or reads settings.
                app.accessibility.a11y.screen_reader = true;
            },
        );
        draw_prepared_component_test_frame(&mut app);
        assert_eq!(
            checks.get(),
            0,
            "prepared polling must not consult its source"
        );
        assert_eq!(applies.get(), 0);
        assert!(!changed);
        assert!(app.theme_store_stamp.is_none());
        assert!(app.accessibility.screen_reader.is_none());
        assert!(!app.accessibility.a11y.screen_reader);
        assert_presentation(&app.accessibility.base_theme, 2.0, 1.25);
        let observation = observed.borrow();
        let (theme, renderer_scale) = observation.draws.last().unwrap();
        assert_eq!(*renderer_scale, 1.5);
        assert_presentation(theme, 1.5, 1.25);
        app.close_components();
    }

    #[test]
    fn legacy_theme_poll_keeps_due_stamp_and_reload_behavior_with_controlled_sources() {
        let mut app = App::from_factory_with_controlled_inputs(State::new(), || {
            Box::new(crate::widget::Column::new())
        });
        let start = app.last_theme_store_check;
        let checks = std::cell::Cell::new(0);
        let applies = std::cell::Cell::new(0);
        let stamp = Some(std::time::SystemTime::UNIX_EPOCH);
        for (millis, source_stamp, expected_checks, expected_applies, expected_changed) in [
            (749, stamp, 0, 0, false),
            (750, stamp, 1, 1, true),
            (1500, stamp, 2, 1, false),
            (2250, None, 3, 2, true),
        ] {
            app.layout.layout_dirty = false;
            app.layout.structure_dirty = false;
            let changed = app.reconcile_configured_theme_with(
                start + Duration::from_millis(millis),
                || {
                    checks.set(checks.get() + 1);
                    source_stamp
                },
                |app| {
                    applies.set(applies.get() + 1);
                    app.accessibility.base_theme = Theme::light();
                },
            );
            assert_eq!(changed, expected_changed);
            assert_eq!(checks.get(), expected_checks);
            assert_eq!(applies.get(), expected_applies);
            assert_eq!(app.layout.layout_dirty, expected_changed);
            assert_eq!(app.layout.structure_dirty, expected_changed);
            assert!(app.accessibility.screen_reader.is_none());
        }
        assert!(app.theme_store_stamp.is_none());
        assert_eq!(app.theme.name, Theme::light().name);
    }

    fn wait_for_saved_record(host: &ProbeHost, binding: &crate::bound_dispatch::CallbackBinding) {
        assert!(host.records.same_owner(&binding.records));
        let deadline = Instant::now().checked_add(Duration::from_secs(5)).unwrap();
        loop {
            let status = host.records.save_status(&binding.checkout.id).unwrap();
            if status == crate::record::RecordSaveStatus::Saved {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "original writer did not acknowledge: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_saved_body(
        host: &ProbeHost,
        binding: &crate::bound_dispatch::CallbackBinding,
        expected: &str,
    ) {
        wait_for_saved_record(host, binding);
        let path = app_state::DocStore::room_record_dir_at(
            &host.root(),
            APP,
            ROOM,
            &binding.checkout.id.storage_key(),
        )
        .unwrap()
        .join(app_state::DOCUMENT_FILE);
        let doc = automerge::AutoCommit::load_with_options(
            &std::fs::read(path).unwrap(),
            automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Error),
        )
        .unwrap();
        assert_eq!(
            doc.get(automerge::ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .to_str(),
            Some(expected)
        );
    }

    fn edit_bound_record(app: &mut App, binding: &crate::bound_dispatch::CallbackBinding) {
        let target = crate::bound_dispatch::BoundActionTarget {
            binding: binding.clone(),
            address: crate::bound_dispatch::BoundActionAddress {
                instance_id: binding.instance_id,
                address: tilo_actions::ActionAddress::new("probe:shared", "edit"),
            },
        };
        app.invoke_bound_target(
            &target,
            None,
            "probe-author",
            false,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(body(&binding.records, &binding.checkout), "edited");
        assert_eq!(
            binding.records.save_status(&binding.checkout.id).unwrap(),
            crate::record::RecordSaveStatus::Saving
        );
    }

    #[test]
    fn prepared_native_autosave_tick_acknowledges_original_writer_before_close() {
        let host = ProbeHost::new();
        let component = editable_component(&host, 11);
        let binding = component.callback_binding();
        let mut app = App::from_component(component, runner_inputs()).unwrap();
        assert!(app.persistence.autosave.is_none() && app.persistence.records.is_none());
        assert_saved_body(&host, &binding, "initialized");
        edit_bound_record(&mut app, &binding);
        app.autosave_tick().unwrap();
        assert_saved_body(&host, &binding, "edited");
        assert!(binding.lifetime.is_active());
        assert!(app.component_close_reports().is_empty());
        app.close_components();
    }

    #[test]
    fn prepared_native_autosave_tick_reaches_distinct_embedded_owners_before_close() {
        let first_host = ProbeHost::new();
        let first = editable_component(&first_host, 11);
        let first_binding = first.callback_binding();
        let mut app = App::from_component(first, runner_inputs()).unwrap();
        let second_host = ProbeHost::new();
        let mut second = editable_component(&second_host, 22);
        let second_binding = second.callback_binding();
        assert!(!first_binding.records.same_owner(&second_binding.records));
        let handle = app.wake_scope.as_ref().unwrap().handle();
        second.mount(move |_| handle.wake()).unwrap();
        let first_root =
            std::mem::replace(&mut app.root_widget, Box::new(crate::widget::Column::new()));
        app.root_widget = Box::new(
            crate::widget::Column::new()
                .push(first_root)
                .push(Box::new(TileHost::from_component(second))),
        );
        assert_saved_body(&first_host, &first_binding, "initialized");
        assert_saved_body(&second_host, &second_binding, "initialized");
        edit_bound_record(&mut app, &first_binding);
        edit_bound_record(&mut app, &second_binding);
        app.autosave_tick().unwrap();
        assert_saved_body(&first_host, &first_binding, "edited");
        assert_saved_body(&second_host, &second_binding, "edited");
        assert!(first_binding.lifetime.is_active() && second_binding.lifetime.is_active());
        assert!(app.component_close_reports().is_empty());
        app.close_components();
    }

    enum Runner {
        App(Box<App>),
        Driver(Box<crate::headless::Driver>),
    }
    impl Runner {
        fn new(mode: u8, mut component: AppComponent) -> Self {
            match mode {
                0 => Self::App(Box::new(
                    App::from_component(component, runner_inputs()).unwrap(),
                )),
                1 => {
                    let scope = crate::WakeScope::new();
                    let handle = scope.handle();
                    component.mount(move |_| handle.wake()).unwrap();
                    let author = component.callback_binding().author;
                    Self::App(Box::new(App::from_scoped_parts_with_inputs(
                        scope,
                        State::new(),
                        Box::new(TileHost::from_component(component)),
                        || {
                            let mut inputs = AppConstructionInputs::controlled();
                            let mut presence = PresenceCtx::empty();
                            presence.set_own_identity([author.0.clone()]);
                            inputs.local_author = ConstructionValue::Ready(author);
                            inputs.presence_ctx = ConstructionValue::Ready(presence);
                            inputs
                        },
                    )))
                }
                _ => Self::Driver(Box::new(
                    crate::headless::Driver::from_component(component, (320, 180), runner_inputs())
                        .unwrap(),
                )),
            }
        }
        fn invoke(&mut self, action: &str) -> Result<(), String> {
            match self {
                Self::App(app) => {
                    app.init_layout();
                    app.refresh_semantic_tree();
                    let id = app
                        .semantic_tree()
                        .iter()
                        .find(|node| node.name.as_deref() == Some("Shared probe"))
                        .ok_or_else(|| "resource absent after authority projection".to_owned())?
                        .id
                        .clone();
                    if app.invoke_semantic_action(&id, action, None)? {
                        Ok(())
                    } else {
                        Err("action absent".into())
                    }
                }
                Self::Driver(driver) => driver
                    .invoke_bound_action(
                        &crate::bound_dispatch::BoundActionAddress {
                            instance_id: 11,
                            address: tilo_actions::ActionAddress::new("probe:shared", action),
                        },
                        &serde_json::Map::new(),
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
            }
        }
        fn close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
            match self {
                Self::App(app) => {
                    app.close_components();
                    app.take_component_close_reports()
                }
                Self::Driver(driver) => {
                    driver.close_components();
                    driver.take_component_close_reports()
                }
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one source record and View are compared across three real runner paths"
    )]
    fn prepared_three_runner_factory_record_view_action_and_denial_parity() {
        let seed = ProbeHost::new();
        let mut component = runner_probe_definition()
            .instantiate(
                seed.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                    .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = component.record().clone();
        component
            .restore_local_view(&crate::view::View {
                scroll: vec![37.0],
                focus: Some(crate::view::FocusView {
                    ordinal: 0,
                    ephemeral: "caret=9".into(),
                }),
                ..crate::view::View::default()
            })
            .unwrap();
        let saved = component.close().recovery.unwrap();
        let bytes = seed
            .records
            .with(&checkout.id, automerge::AutoCommit::save)
            .unwrap();
        let mut results = Vec::new();
        for mode in 0..3 {
            let host = ProbeHost::new();
            let path = app_state::DocStore::room_record_dir_at(
                &host.root(),
                APP,
                ROOM,
                &checkout.id.storage_key(),
            )
            .unwrap()
            .join(app_state::DOCUMENT_FILE);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let target = existing(&checkout);
            let permissions = runner_permissions();
            let context = host
                .prepare_with_permissions(
                    runner_probe_definition(),
                    11,
                    &target,
                    permissions.clone(),
                )
                .unwrap();
            let component = runner_probe_definition()
                .instantiate(context, target, RestoreInput::Local(saved.clone()))
                .unwrap();
            assert_eq!(calls(), 1);
            assert_eq!(initializations(), 0);
            assert_eq!(body(&host.records, &checkout), "initialized");
            assert_eq!(component.capture_local_view().unwrap(), saved.view);
            host.records
                .with(&checkout.id, |doc| {
                    use automerge::transaction::Transactable;
                    assert_eq!(doc.pending_ops(), 0);
                    // A fresh CRDT actor sorts before the copied historical actors.
                    // It is independent of the authenticated participant identity.
                    doc.set_actor(automerge::ActorId::from([0_u8].as_slice()));
                    assert_eq!(doc.pending_ops(), 0);
                })
                .unwrap();
            let mut runner = Runner::new(mode, component);
            runner.invoke("view").unwrap();
            runner.invoke("effect").unwrap();
            assert_eq!(
                *operation_observations()[0].borrow(),
                vec!["View", "Effect"]
            );
            runner.invoke("edit").unwrap();
            assert_eq!(body(&host.records, &checkout), "edited");
            host.records
                .read_history(&checkout.id, |_, history| {
                    assert!(history.next_undo_in(&author(), 11).is_some());
                })
                .unwrap();
            *permissions.borrow_mut() = crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "probe-author"),
                vec![],
            )
            .at_unix_time(100);
            assert!(runner.invoke("edit").is_err());
            assert_eq!(body(&host.records, &checkout), "edited");
            assert_eq!(
                *operation_observations()[0].borrow(),
                vec!["View", "Effect"]
            );
            let reports = runner.close();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].recovery.as_ref().unwrap().view, saved.view);
            assert!(reports[0].view_error.is_none());
            results.push(
                serde_json::to_vec(
                    &host
                        .records
                        .read(&checkout.id, |doc| {
                            doc.get(automerge::ROOT, "body")
                                .unwrap()
                                .unwrap()
                                .0
                                .to_str()
                                .unwrap()
                                .to_owned()
                        })
                        .unwrap(),
                )
                .unwrap(),
            );
            host.records
                .with_history(&checkout.id, |doc, history| {
                    assert!(history.undo_in(doc, &author(), 11, None, None).is_some());
                    assert!(history.next_undo_in(&author(), 11).is_none());
                })
                .unwrap();
            assert_eq!(body(&host.records, &checkout), "initialized");
        }
        assert_eq!(results, vec![b"\"edited\"".to_vec(); 3]);
    }

    #[test]
    fn prepared_three_runner_close_retains_original_view_store_for_retry() {
        for mode in 0..3 {
            let host = ProbeHost::new();
            let component = runner_probe_definition()
                .instantiate(
                    host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                        .unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let path = host.view_path(11);
            std::fs::create_dir_all(&path).unwrap();
            let mut runner = Runner::new(mode, component);
            drop(host.views);
            let mut reports = runner.close();
            assert_eq!(reports.len(), 1);
            let report = &mut reports[0];
            assert!(report.view_error.is_some());
            assert!(report.view_retry.is_some());
            assert_eq!(report.recovery.as_ref().unwrap().app_id, APP);
            drop(runner);
            std::fs::remove_dir(&path).unwrap();
            report.retry_view().unwrap();
            assert!(report.view_error.is_none());
            assert!(report.view_retry.is_none());
            let saved: crate::application_view::SavedInstanceView =
                postcard::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved.instance_id, 11);
            assert_eq!(saved.device_id, DEVICE);
            assert_eq!(saved.workspace_id, ROOM);
        }
    }
    #[test]
    fn prepared_native_close_retains_original_recovery_and_is_idempotent() {
        let host = ProbeHost::new();
        let component = runner_probe_definition()
            .instantiate(
                host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                    .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let binding = component.callback_binding();
        let path = host.view_path(11);
        std::fs::create_dir_all(&path).unwrap();
        let mut app = App::from_component(component, runner_inputs()).unwrap();
        drop(host.views);
        app.close_wake_scope();
        let _closed = app.enter_wake_scope();
        app.close_native_content();
        assert!(!binding.lifetime.is_active());
        assert_eq!(app.component_close_reports().len(), 1);
        assert!(app.component_close_reports()[0].view_error.is_some());
        app.close_native_content();
        let mut reports = app.take_component_close_reports();
        assert_eq!(reports.len(), 1);
        assert!(binding.records.read(&binding.checkout.id, |_| ()).is_some());
        assert_eq!(reports[0].recovery.as_ref().unwrap().instance_id, 11);
        std::fs::remove_dir(&path).unwrap();
        reports[0].retry_view().unwrap();
        assert!(path.is_file());
        assert!(app.take_component_close_reports().is_empty());
    }

    #[test]
    fn prepared_three_runner_close_cancels_local_operations_and_rejects_stale_results() {
        for mode in 0..3 {
            let host = ProbeHost::new();
            let component = runner_probe_definition()
                .instantiate(
                    host.prepare_with_permissions(
                        runner_probe_definition(),
                        11,
                        &LaunchTarget::New,
                        runner_permissions(),
                    )
                    .unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let checkout = component.record().clone();
            let writer = local_state().writer();
            let observed = operation_observations()[0].clone();
            let mut runner = Runner::new(mode, component);
            runner.invoke("pending").unwrap();
            assert_eq!(*observed.borrow(), vec!["pending"]);
            let reports = runner.close();
            assert_eq!(*observed.borrow(), vec!["pending", "cancelled"]);
            assert!(!writer.try_set("stale after close".into()));
            let target = existing(&checkout);
            let successor = runner_probe_definition()
                .instantiate(
                    host.prepare_with_permissions(
                        runner_probe_definition(),
                        11,
                        &target,
                        runner_permissions(),
                    )
                    .unwrap(),
                    target,
                    RestoreInput::Local(reports[0].recovery.clone().unwrap()),
                )
                .unwrap();
            let successor_local = local_state();
            let mut successor = Runner::new(mode, successor);
            assert!(!writer.try_set("stale after reopen".into()));
            assert_eq!(successor_local.with(Clone::clone), "construction wake");
            successor.invoke("view").unwrap();
            successor.close();
            assert_eq!(body(&host.records, &checkout), "initialized");
            assert_eq!(*observed.borrow(), vec!["pending", "cancelled"]);
        }
    }

    fn palette_key(app: &mut App, key: Key, text: Option<&str>) {
        let mut event = action_palette_tests::synth_enter();
        if let WindowEvent::KeyboardInput { event, .. } = &mut event {
            event.logical_key = key;
            event.text = text.map(Into::into);
        }
        assert!(app.action_palette_handle_key(&event));
    }

    #[test]
    fn prepared_presentation_replaces_effective_factors_across_runners_and_setters() {
        for theme_scale in [0.5, 1.0, 1.5, 2.0] {
            for native in [true, false] {
                let host = ProbeHost::new();
                let component = runner_probe_definition()
                    .instantiate(
                        host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                            .unwrap(),
                        LaunchTarget::New,
                        RestoreInput::None,
                    )
                    .unwrap();
                let inputs = presentation_inputs(theme_scale);
                let observed = observe_presentation();
                if native {
                    let mut app = App::from_component(component, inputs).unwrap();
                    for scale in [1.5, 0.5, 4.0, 4.0, 0.75] {
                        app.set_text_scale(scale);
                        draw_prepared_component_test_frame(&mut app);
                        let observation = observed.borrow();
                        let (theme, renderer_scale) = observation.draws.last().unwrap();
                        assert_eq!(*renderer_scale, scale);
                        assert_presentation(theme, scale, 1.25);
                        assert_presentation(observation.layouts.last().unwrap(), scale, 1.25);
                    }
                    app.set_ui_scale(1.5);
                    app.set_ui_scale(1.5);
                    app.set_high_contrast(true);
                    assert_eq!(app.theme.name, "High Contrast");
                    assert_eq!(app.theme.text_scale, 0.75);
                    assert_eq!(app.theme.ui_scale, 1.5);
                    app.set_high_contrast(false);
                    draw_prepared_component_test_frame(&mut app);
                    let observation = observed.borrow();
                    let (theme, renderer_scale) = observation.draws.last().unwrap();
                    assert_eq!(*renderer_scale, 0.75);
                    assert_presentation(theme, 0.75, 1.5);
                    assert!(!observation.motion.is_empty());
                    assert!(observation.motion.iter().all(|value| *value));
                    app.close_components();
                } else {
                    let mut driver =
                        crate::headless::Driver::from_component(component, (800, 600), inputs)
                            .unwrap();
                    let observation = observed.borrow();
                    let (theme, renderer_scale) = observation.draws.last().unwrap();
                    assert_eq!(*renderer_scale, 1.5);
                    assert_presentation(theme, 1.5, 1.25);
                    assert_presentation(observation.layouts.last().unwrap(), 1.5, 1.25);
                    assert!(!observation.motion.is_empty());
                    assert!(observation.motion.iter().all(|value| *value));
                    driver.close_components();
                }
            }
        }
    }
    fn palette_frame(app: &mut App) {
        let mut scene = Scene::new();
        draw_frame_content(app, &mut scene, 800, 600, 1.0, None);
        let palette = app.action_palette().unwrap();
        assert!(palette.rect().width() > 0.0 && palette.rect().height() > 0.0);
        assert_eq!(
            crate::action_palette::observed_field_language(palette),
            "de-DE"
        );
        assert_eq!(app.text_renderer.text_scale(), 1.5);
    }
    fn activate_palette_action(app: &mut App, action: &str) {
        app.refresh_semantic_tree();
        let target = app
            .accessibility
            .semantic_tree
            .bound_actions_for_test()
            .into_iter()
            .find(|(_, target)| {
                target.address.instance_id == 11 && target.address.address.action == action
            })
            .unwrap()
            .1;
        assert!(
            app.actions
                .action_palette
                .as_mut()
                .unwrap()
                .activate_bound(&target)
        );
    }
    #[test]
    fn component_palette_uses_host_text_through_prompt_confirmation_and_reopen() {
        let host = ProbeHost::new();
        let component = runner_probe_definition()
            .instantiate(
                host.prepare_with_permissions(
                    runner_probe_definition(),
                    11,
                    &LaunchTarget::New,
                    runner_permissions(),
                )
                .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = component.record().clone();
        let catalog = crate::font_catalog::FontCatalog::controlled_fixture(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"),
            "de-DE",
        );
        let mut renderer = TextRenderer::with_font_catalog(&catalog).unwrap();
        renderer.set_text_scale(1.5);
        renderer.bump_generation();
        let mut app = App::from_component(
            component,
            crate::application::ComponentRunnerInputs {
                theme: Theme::dark(),
                text_renderer: renderer,
            },
        )
        .unwrap();
        assert_eq!(app.local_author(), &author());
        app.init_layout();
        app.open_action_palette();
        let before = app.text_renderer.cache_stats();
        palette_frame(&mut app);
        assert!(
            app.text_renderer.cache_stats().1 > before.1,
            "measurement uses the host cache"
        );
        palette_key(&mut app, Key::Character("P".into()), Some("P"));
        assert_eq!(app.action_palette().unwrap().query(), "P");
        palette_frame(&mut app);
        activate_palette_action(&mut app, "prompt");
        assert!(app.action_palette().unwrap().in_argument_mode());
        palette_frame(&mut app);
        palette_key(&mut app, Key::Named(NamedKey::Escape), None);
        assert!(!app.action_palette().unwrap().in_argument_mode());
        palette_frame(&mut app);
        activate_palette_action(&mut app, "prompt");
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("prompted");
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "prompted");
        app.open_action_palette();
        palette_frame(&mut app);
        activate_palette_action(&mut app, "typed");
        palette_frame(&mut app);
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query(r#"{"value":"typed","enabled":true}"#);
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "typed");
        app.open_action_palette();
        activate_palette_action(&mut app, "confirm");
        palette_frame(&mut app);
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("wrong");
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "typed");
        app.actions.action_palette.as_mut().unwrap().set_query("JA");
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "confirmed");
        app.open_action_palette();
        let before = app.text_renderer.cache_stats();
        app.text_renderer.bump_generation();
        palette_frame(&mut app);
        assert!(app.text_renderer.cache_stats().1 > before.1);
        host.records
            .read_history(&checkout.id, |_, history| {
                assert_eq!(
                    history.next_undo_in(&author(), 11).unwrap().author.as_ref(),
                    Some(&author())
                );
            })
            .unwrap();
        app.close_components();
    }
}

#[cfg(test)]
mod card_grid_host_tests {
    use super::*;
    use crate::widget::{Button, Card, CardGrid, Column, Container, Element};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    fn fixture(count: usize) -> (App, Rc<RefCell<Vec<usize>>>) {
        let clicks = Rc::new(RefCell::new(Vec::new()));
        let cards: Vec<Element> = (0..count)
            .map(|i| {
                let clicks = clicks.clone();
                Box::new(Card::new(Box::new(
                    Button::new(&format!("card-{i}"), &format!("Card {i}"), None)
                        .on_click(move || clicks.borrow_mut().push(i)),
                ))) as Element
            })
            .collect();
        let root = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(20.0)
            .push(Box::new(
                CardGrid::new(cards)
                    .with_min_card_width(260.0)
                    .with_card_height(180.0)
                    .with_gap(16.0),
            ))
            .push(Box::new(
                Container::new().with_no_padding().with_height(10.0),
            ));
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        (app, clicks)
    }

    fn layout(app: &mut App, width: u32) {
        app.layout.layout_dirty = true;
        run_layout_pass(
            app,
            app.layout.root_node.unwrap(),
            width,
            2000,
            1.0,
            false,
            None,
        )
        .unwrap();
    }

    fn button_rects(app: &App) -> Vec<KurboRect> {
        app.root_widget.children()[0]
            .children()
            .iter()
            .map(|card| card.children()[0].children()[0].get_rect())
            .collect()
    }

    #[test]
    fn card_grid_buttons_have_host_focus_and_geometry_before_paint() {
        let (mut app, clicks) = fixture(6);
        layout(&mut app, 900);
        assert_eq!(
            app.focusable_order.len(),
            6,
            "all card buttons enter host focus traversal"
        );
        let before = button_rects(&app);
        for (index, &node) in app.focusable_order.iter().enumerate() {
            assert_eq!(
                app.inspect_node(node, |widget| widget.name().to_owned())
                    .unwrap(),
                "Button"
            );
            assert_eq!(
                app.inspect_node(node, |widget| widget.get_rect()).unwrap(),
                before[index]
            );
            assert!(before[index].width() > 0.0 && before[index].height() > 0.0);
            assert_eq!(
                app.inspect_node(node, |widget| widget.accessibility().unwrap().semantic_key)
                    .unwrap(),
                Some(format!("card-{index}"))
            );
        }
        let target = before[4].center();
        assert!(app.dispatch_semantic_click(target.x, target.y));
        assert_eq!(*clicks.borrow(), vec![4]);
        assert_eq!(app.focused_node, Some(app.focusable_order[4]));
        let order = app.focusable_order.clone();
        let mut scene = Scene::new();
        app.root_widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut app.text_renderer,
            &app.theme,
            &PresenceCtx::default(),
        );
        assert_eq!(
            button_rects(&app),
            before,
            "paint must not establish or change geometry"
        );
        assert_eq!(app.focusable_order, order);
    }

    #[test]
    fn card_grid_reflows_retained_nodes_and_keeps_explicit_rows() {
        let (mut app, _) = fixture(6);
        layout(&mut app, 900);
        let order = app.focusable_order.clone();
        app.set_focused_node(Some(order[4]));
        for (width, height, second_row_y) in [
            (900, 376.0, 196.0),
            (600, 572.0, 196.0),
            (200, 1160.0, 196.0),
            (900, 376.0, 196.0),
        ] {
            layout(&mut app, width);
            let grid = app.root_widget.children()[0];
            assert_eq!(grid.get_rect().height(), height);
            assert_eq!(grid.get_rect().width(), f64::from(width));
            let cards = grid.children();
            let columns = match width {
                900 => 3,
                600 => 2,
                _ => 1,
            };
            assert_eq!(cards[columns].get_rect().y0, second_row_y);
            for card in cards {
                assert_eq!(card.get_rect().height(), 180.0);
                assert!(card.get_rect().x1 <= f64::from(width));
            }
            assert_eq!(app.root_widget.children()[1].get_rect().y0, height + 20.0);
            assert_eq!(
                app.focusable_order, order,
                "resize retains focus identities"
            );
            assert_eq!(app.focused_node, Some(order[4]));
        }
    }

    #[test]
    fn card_grid_long_content_does_not_turn_fixed_rows_into_minimums() {
        let body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(crate::widget::Text::new(
                &"Long content wraps across lines. ".repeat(80),
            )))
            .push(Box::new(Button::new("long-action", "Action", None)));
        let root = CardGrid::new(vec![Box::new(Card::new(Box::new(body)))])
            .with_card_height(180.0)
            .with_min_card_width(260.0);
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        layout(&mut app, 200);
        assert_eq!(app.root_widget.get_rect().height(), 180.0);
        assert_eq!(app.root_widget.children()[0].get_rect().height(), 180.0);
    }

    #[test]
    fn card_grid_headless_uses_same_narrow_geometry_and_click_target() {
        let (mut app, clicks) = fixture(2);
        let root = std::mem::replace(&mut app.root_widget, Box::new(Column::new()));
        let mut driver = crate::headless::Driver::new_with_controlled_inputs(
            State::new(),
            root,
            (200, 600),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("grid-test".into()),
        );
        let snapshot = driver.inspection();
        let grid = &snapshot.root.children[0];
        assert_eq!(grid.rect, [0.0, 0.0, 200.0, 376.0]);
        assert_eq!(grid.children[1].rect, [0.0, 196.0, 200.0, 180.0]);
        driver.click_named("card-1").unwrap();
        assert_eq!(*clicks.borrow(), vec![1]);
    }

    #[test]
    fn card_grid_under_scrollable_clips_input_before_paint() {
        let (mut app, clicks) = fixture(6);
        let child = std::mem::replace(&mut app.root_widget, Box::new(Column::new()));
        app.root_widget = Box::new(crate::widget::Scrollable::new_vertical().with_child(child));
        app.init_layout();
        app.layout.layout_dirty = true;
        let root_node = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root_node, 200, 190, 1.0, true, None).unwrap();
        let root = app.root_widget.children()[0];
        assert_eq!(root.children()[0].get_rect().height(), 1160.0);
        let hidden_button = root.children()[0].children()[1].children()[0].children()[0].get_rect();
        assert!(hidden_button.y0 >= 190.0);
        let target = hidden_button.center();
        app.dispatch_semantic_click(target.x, target.y);
        assert!(
            clicks.borrow().is_empty(),
            "off-viewport card must not receive pointer activation"
        );
        let visible_button =
            app.root_widget.children()[0].children()[0].children()[0].children()[0].children()[0]
                .get_rect()
                .center();
        app.dispatch_semantic_click(visible_button.x, visible_button.y);
        assert_eq!(*clicks.borrow(), vec![0]);
    }

    #[test]
    fn card_grid_retains_local_edit_state_across_width_changes() {
        let input = crate::widget::TextInput::local().with_initial_value("saved value");
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CardGrid::new(vec![Box::new(Card::new(Box::new(input)))])),
        );
        app.init_layout();
        layout(&mut app, 900);
        let node = app.focusable_order[0];
        app.set_focused_node(Some(node));
        let state = app.inspect_node(node, |widget| widget.get_state()).unwrap();
        assert!(state.is_some());
        for width in [200, 600, 900] {
            layout(&mut app, width);
            assert_eq!(app.focusable_order, vec![node]);
            assert_eq!(app.focused_node, Some(node));
            assert_eq!(
                app.inspect_node(node, |widget| widget.get_state()).unwrap(),
                state
            );
        }
    }

    #[test]
    fn card_grid_forwards_retained_list_structure_changes_to_host() {
        use crate::widget::{ListModel, VirtualList};
        use std::cell::Cell;
        struct GrowingModel {
            keys: Rc<RefCell<Vec<u64>>>,
            changed: Rc<Cell<bool>>,
        }
        impl ListModel for GrowingModel {
            type Key = u64;
            fn len(&self) -> usize {
                self.keys.borrow().len()
            }
            fn key_at(&self, index: usize) -> Option<u64> {
                self.keys.borrow().get(index).copied()
            }
            fn index_of(&self, key: &u64) -> Option<usize> {
                self.keys
                    .borrow()
                    .iter()
                    .position(|candidate| candidate == key)
            }
            fn poll_changed(&mut self) -> bool {
                self.changed.replace(false)
            }
        }
        let keys = Rc::new(RefCell::new(vec![1, 2]));
        let changed = Rc::new(Cell::new(false));
        let list = VirtualList::new(
            GrowingModel {
                keys: keys.clone(),
                changed: changed.clone(),
            },
            32.0,
            |_, _, key| Box::new(Button::new(&format!("row-{key}"), "Row", None)),
        )
        .with_viewport_height(120.0);
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CardGrid::new(vec![Box::new(Card::new(Box::new(list)))])),
        );
        app.init_layout();
        layout(&mut app, 300);
        let button_keys = |app: &App| {
            app.focusable_order
                .iter()
                .filter_map(|&node| {
                    app.inspect_node(node, |widget| {
                        if widget.name() == "Button" {
                            widget
                                .accessibility()
                                .and_then(|access| access.semantic_key)
                        } else {
                            None
                        }
                    })
                    .unwrap()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(button_keys(&app), vec!["row-1", "row-2"]);
        app.root_widget.take_structure_dirty();
        keys.borrow_mut().push(3);
        changed.set(true);
        assert!(app.root_widget.update(&mut app.state.doc, &app.state.root));
        app.root_widget.sync(&mut app.state.doc, &app.state.root);
        assert!(app.root_widget.take_structure_dirty());
        app.rebuild_structure();
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 300, 2000, 1.0, true, None).unwrap();
        assert_eq!(button_keys(&app), vec!["row-1", "row-2", "row-3"]);
        // Rebuilding and post-build sync must preserve actual delegate geometry.
        for node in &app.focusable_order {
            assert!(app.layout.taffy.layout(*node).unwrap().size.height > 0.0);
            assert!(
                app.inspect_node(*node, |widget| widget.get_rect().height())
                    .unwrap()
                    > 0.0
            );
        }
    }

    #[test]
    fn card_grid_nested_below_minimum_uses_actual_parent_allocation() {
        let nested = CardGrid::new(vec![
            Box::new(Button::new("nested-a", "A", None)),
            Box::new(Button::new("nested-b", "B", None)),
        ])
        .with_min_card_width(300.0)
        .with_card_height(60.0)
        .with_gap(16.0);
        let outer = CardGrid::new(vec![Box::new(Card::new(Box::new(nested)))])
            .with_min_card_width(260.0)
            .with_card_height(180.0);
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(outer));
        app.init_layout();
        layout(&mut app, 200);
        let card = app.root_widget.children()[0];
        let surface = card.children()[0];
        let nested = surface.children()[0];
        assert_eq!(card.get_rect().width(), 200.0);
        assert!(nested.get_rect().width() < 200.0);
        let buttons = nested.children();
        assert_eq!(buttons[1].get_rect().y0 - buttons[0].get_rect().y0, 76.0);
        assert!(
            buttons
                .iter()
                .all(|button| button.get_rect().x1 <= card.get_rect().x1)
        );
        assert_eq!(app.focusable_order.len(), 2);
    }

    struct GrowingBody {
        column: Column,
        requested: Rc<Cell<usize>>,
        count: usize,
        dirty: bool,
        clicks: Rc<RefCell<Vec<usize>>>,
    }
    impl Widget for GrowingBody {
        fn update(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) -> bool {
            if self.count == self.requested.get() {
                return false;
            }
            self.count = self.requested.get();
            self.column = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(8.0);
            for index in 0..self.count {
                let clicks = self.clicks.clone();
                self.column = std::mem::take(&mut self.column).push(Box::new(
                    Button::new(&format!("dynamic-{index}"), "Action", None)
                        .on_click(move || clicks.borrow_mut().push(index)),
                ));
            }
            self.dirty = true;
            true
        }
        fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
            self.column.sync(state, path);
        }
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            theme: &Theme,
            map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.column.build_layout(tree, theme, map)
        }
        fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.column.apply_layout(tree, node, origin);
        }
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.column.draw(scene, transform, text, theme, presence);
        }
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            scale: f64,
            modifiers: &winit::event::Modifiers,
            state: &mut automerge::AutoCommit,
            path: &automerge::ObjId,
            handled: &mut bool,
            focus: &mut Option<NodeId>,
            node: NodeId,
            tree: &TaffyTree<()>,
        ) {
            self.column.handle_event(
                event, scale, modifiers, state, path, handled, focus, node, tree,
            );
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.column.children()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.column.children_mut()
        }
        fn get_rect(&self) -> KurboRect {
            self.column.get_rect()
        }
        fn name(&self) -> &'static str {
            "GrowingBody"
        }
        fn take_structure_dirty(&mut self) -> bool {
            std::mem::take(&mut self.dirty)
        }
    }

    #[test]
    fn card_grid_rebuilt_card_body_has_prepaint_geometry_and_input() {
        let requested = Rc::new(Cell::new(1));
        let clicks = Rc::new(RefCell::new(Vec::new()));
        let body = GrowingBody {
            column: Column::new(),
            requested: requested.clone(),
            count: 0,
            dirty: false,
            clicks: clicks.clone(),
        };
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(
                CardGrid::new(vec![Box::new(Card::new(Box::new(body)))]).with_card_height(180.0),
            ),
        );
        app.root_widget.update(&mut app.state.doc, &app.state.root);
        app.init_layout();
        layout(&mut app, 300);
        assert_eq!(app.focusable_order.len(), 1);
        app.root_widget.take_structure_dirty();
        requested.set(2);
        assert!(app.root_widget.update(&mut app.state.doc, &app.state.root));
        assert!(app.root_widget.take_structure_dirty());
        app.rebuild_structure();
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 300, 2000, 1.0, true, None).unwrap();
        assert_eq!(app.focusable_order.len(), 2);
        let rects = app
            .focusable_order
            .iter()
            .map(|&node| app.inspect_node(node, |widget| widget.get_rect()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rects[0].height(), 40.0);
        assert_eq!(rects[1].height(), 40.0);
        assert_eq!(rects[1].y0 - rects[0].y1, 8.0);
        let target = rects[1].center();
        assert!(app.dispatch_semantic_click(target.x, target.y));
        assert_eq!(*clicks.borrow(), vec![1]);
        let mut scene = Scene::new();
        app.root_widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut app.text_renderer,
            &app.theme,
            &PresenceCtx::default(),
        );
        for (&node, before) in app.focusable_order.iter().zip(rects) {
            assert_eq!(
                app.inspect_node(node, |widget| widget.get_rect()).unwrap(),
                before
            );
        }
    }

    #[test]
    fn card_grid_empty_and_single_keep_intrinsic_height() {
        for (count, height) in [(0, 0.0), (1, 180.0)] {
            let (mut app, _) = fixture(count);
            layout(&mut app, 200);
            assert_eq!(app.root_widget.children()[0].get_rect().height(), height);
            assert_eq!(app.focusable_order.len(), count);
        }
    }
}

#[cfg(test)]
mod viewport_composition_tests {
    use super::*;
    use crate::widget::TaffyTreeExt;
    use crate::widget::{Button, Element};
    use std::cell::Cell;
    use std::rc::Rc;

    /// Legacy composition deliberately does not report its breakpoint changes.
    struct ResponsiveRoot {
        rect: KurboRect,
        children: Vec<Element>,
        nodes: Vec<NodeId>,
        builds: Rc<Cell<usize>>,
        editing: bool,
        trap: bool,
        modal_on_narrow: bool,
        native_calls: Option<Rc<std::cell::RefCell<Vec<NativeCall>>>>,
        scroll: Rc<Cell<(f32, f32)>>,
        oscillate: Rc<Cell<bool>>,
        paints: Rc<Cell<usize>>,
    }
    impl ResponsiveRoot {
        fn new(builds: Rc<Cell<usize>>) -> Self {
            Self {
                rect: KurboRect::ZERO,
                children: vec![],
                nodes: vec![],
                builds,
                editing: false,
                trap: false,
                modal_on_narrow: false,
                native_calls: None,
                scroll: Rc::new(Cell::new((0.0, 120.0))),
                oscillate: Rc::default(),
                paints: Rc::default(),
            }
        }
    }
    impl Widget for ResponsiveRoot {
        fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
            for child in &mut self.children {
                child.sync(doc, path);
            }
        }
        fn name(&self) -> &'static str {
            "ResponsiveRoot"
        }
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            scale: f64,
            modifiers: &winit::event::Modifiers,
            doc: &mut automerge::AutoCommit,
            path: &automerge::ObjId,
            handled: &mut bool,
            focus: &mut Option<NodeId>,
            _: NodeId,
            taffy: &TaffyTree<()>,
        ) {
            for (child, node) in self.children.iter_mut().zip(&self.nodes) {
                child.handle_event(
                    event, scale, modifiers, doc, path, handled, focus, *node, taffy,
                );
            }
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.builds.set(self.builds.get().saturating_add(1));
            let count = if self.rect.width() >= 1100.0 {
                3
            } else if self.rect.width() >= 800.0 {
                2
            } else {
                1
            };
            self.children = (0..count)
                .map(|index| {
                    Box::new(Button::new(&format!("action-{index}"), "Action", None)) as Element
                })
                .collect();
            if self.editing {
                self.children = vec![
                    Box::new(
                        crate::widget::TextInput::new(vec![crate::presence::PathSegment::Field(
                            "body".into(),
                        )])
                        .with_on_change_path("body")
                        .with_id("editor"),
                    ),
                    Box::new(Button::new("other", "Other", None)),
                ];
                if self.rect.width() < 1100.0 {
                    self.children.reverse();
                }
                self.children.push(Box::new(
                    crate::widget::Scrollable::new_vertical()
                        .with_height(100.0)
                        .with_persisted_offset(self.scroll.clone())
                        .with_child(Box::new(
                            crate::widget::Container::new()
                                .with_no_padding()
                                .with_height(1000.0),
                        )),
                ));
            }
            if self.modal_on_narrow {
                // This fixture opens a modal whose fallback is another editor,
                // so accidental transfer of the outside selection is observable.
                if self.rect.width() < 1100.0 {
                    self.children.swap(0, 1);
                }
                let mut body = crate::widget::Column::new();
                for child in self.children.drain(..2) {
                    body = body.push(child);
                }
                let mut modal = crate::widget::ModalDialog::new("Modal", body);
                modal.set_open(self.rect.width() < 1100.0);
                self.children = vec![
                    Box::new(
                        crate::widget::TextInput::new(vec![])
                            .with_on_change_path("body")
                            .with_id("outside"),
                    ),
                    Box::new(modal),
                ];
            }
            if let Some(calls) = &self.native_calls {
                self.children = vec![Box::new(crate::widget::NativeView::new(
                    "Native",
                    CountingNativeBackend {
                        generation: self.builds.get(),
                        calls: calls.clone(),
                    },
                ))];
            }
            self.nodes = self
                .children
                .iter_mut()
                .map(|child| child.build_layout(taffy, theme, map))
                .collect();
            let node = taffy.parent_node(
                taffy::Style {
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::length(if self.editing {
                            400.0
                        } else if self.oscillate.get() && self.rect.height() == 100.0 {
                            101.0
                        } else {
                            100.0
                        }),
                    },
                    ..Default::default()
                },
                &self.nodes,
            );
            map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(
            &mut self,
            taffy: &TaffyTree<()>,
            node: NodeId,
            origin: vello::kurbo::Point,
        ) {
            let layout = taffy.layout(node).unwrap();
            self.rect = KurboRect::from_origin_size(
                origin,
                (f64::from(layout.size.width), f64::from(layout.size.height)),
            );
            for (child, node) in self.children.iter_mut().zip(&self.nodes) {
                child.apply_layout(taffy, *node, origin);
            }
        }
        fn draw(
            &mut self,
            scene: &mut Scene,
            affine: Affine,
            text: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.paints.set(self.paints.get().saturating_add(1));
            for child in &mut self.children {
                child.draw(scene, affine, text, theme, presence);
            }
        }
        fn get_rect(&self) -> KurboRect {
            self.rect
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.children.iter().map(AsRef::as_ref).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
            self.children
                .iter_mut()
                .map(|child| child.as_mut() as &mut dyn Widget)
                .collect()
        }
        fn traps_focus(&self) -> bool {
            self.trap
        }
        fn layout_needs_measure(&self) -> bool {
            false
        }
    }
    fn frame(app: &mut App, width: u32) {
        app.layout.layout_dirty = true;
        run_layout_pass(
            app,
            app.layout.root_node.unwrap(),
            width,
            400,
            1.0,
            false,
            None,
        )
        .unwrap();
    }
    #[test]
    fn legacy_composition_matches_fresh_at_multiple_viewports_before_paint() {
        let builds = Rc::new(Cell::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(ResponsiveRoot::new(builds.clone())),
        );
        app.init_layout();
        for (width, count) in [(1200, 3), (920, 2), (600, 1), (1200, 3), (920, 2)] {
            frame(&mut app, width);
            assert_eq!(app.root_widget.children().len(), count, "viewport {width}");
            let mut fresh = App::new_with_controlled_inputs(
                State::new(),
                Box::new(ResponsiveRoot::new(Rc::new(Cell::new(0)))),
            );
            fresh.init_layout();
            frame(&mut fresh, width);
            assert_eq!(
                app.root_widget
                    .children()
                    .iter()
                    .map(|child| child.get_rect())
                    .collect::<Vec<_>>(),
                fresh
                    .root_widget
                    .children()
                    .iter()
                    .map(|child| child.get_rect())
                    .collect::<Vec<_>>()
            );
        }
        let before = builds.get();
        frame(&mut app, 920);
        assert_eq!(
            builds.get(),
            before,
            "translation-only frames do not rebuild composition"
        );
        run_layout_pass(&mut app, NodeId::from(0usize), 920, 400, 1.0, false, None).unwrap();
        assert_eq!(
            builds.get(),
            before,
            "paint-only frames do not rebuild composition"
        );
    }
    #[test]
    fn logical_viewport_changes_recompose_even_at_unchanged_physical_size() {
        let builds = Rc::new(Cell::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(ResponsiveRoot::new(builds.clone())),
        );
        app.init_layout();
        frame(&mut app, 1200);
        let node = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, node, 1200, 400, 2.0, false, None).unwrap();
        assert_eq!(app.root_widget.children().len(), 1);
        let before = builds.get();
        let node = app.layout.root_node.unwrap();
        app.layout.layout_dirty = true;
        run_layout_pass(&mut app, node, 600, 200, 1.0, false, None).unwrap();
        assert_eq!(
            builds.get(),
            before,
            "equal logical viewport does not recompose"
        );
    }

    #[test]
    fn pending_structure_rebuild_is_measured_at_an_unchanged_viewport() {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(ResponsiveRoot::new(Rc::default())),
        );
        app.init_layout();
        frame(&mut app, 1200);
        app.layout.structure_dirty = true;
        frame(&mut app, 1200);
        assert_eq!(app.root_widget.get_rect().width(), 1200.0);
        assert!(
            app.root_widget
                .children()
                .iter()
                .all(|child| child.get_rect().height() > 0.0)
        );
    }

    fn editing_fixture() -> (State, ResponsiveRoot) {
        use automerge::transaction::Transactable;
        let mut state = State::new();
        let body = state
            .doc
            .put_object(&automerge::ROOT, "body", automerge::ObjType::Text)
            .unwrap();
        state.doc.splice_text(&body, 0, 0, "hello world").unwrap();
        let mut root = ResponsiveRoot::new(Rc::default());
        root.editing = true;
        (state, root)
    }

    #[test]
    fn resize_restores_fresh_editor_by_identity_with_selection_and_scroll() {
        let (state, root) = editing_fixture();
        let scroll = root.scroll.clone();
        let mut app = App::new_with_controlled_inputs(state, Box::new(root));
        app.init_layout();
        frame(&mut app, 1200);
        app.set_focused_node(Some(app.focusable_order[0]));
        app.mutate_node(app.focused_node.unwrap(), |widget| {
            widget.set_state("5:b:1");
        })
        .unwrap();
        for width in [920, 600, 1200] {
            frame(&mut app, width);
            let node = app.focused_node.expect("editor remains focused");
            let expected_index = usize::from(width < 1100);
            assert_eq!(
                node, app.focusable_order[expected_index],
                "focus follows identity across reordered controls"
            );
            assert_eq!(
                app.inspect_node(node, |widget| widget.accessibility().unwrap().semantic_key)
                    .unwrap()
                    .as_deref(),
                Some("editor")
            );
            assert_eq!(
                app.inspect_node(node, |widget| widget.get_state())
                    .unwrap()
                    .as_deref(),
                Some("5:b:1")
            );
            assert_eq!(
                app.inspect_node(node, |widget| widget.accessibility().unwrap().value)
                    .unwrap()
                    .as_deref(),
                Some("hello world")
            );
            assert_eq!(scroll.get(), (0.0, 120.0));
            assert!(
                app.inspect_node(node, |widget| widget.get_rect().width())
                    .unwrap()
                    > 0.0
            );
        }
    }

    fn harness(state: State, root: ResponsiveRoot, width: u32) -> crate::headless::FrameHarness {
        crate::headless::FrameHarness::new_with_controlled_inputs(
            state,
            Box::new(root),
            (width, 400),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("resize-test".into()),
        )
    }

    #[test]
    fn headless_resize_matches_fresh_and_preserves_selected_edit_target() {
        let builds = Rc::new(Cell::new(0));
        let mut driver = harness(State::new(), ResponsiveRoot::new(builds.clone()), 1200);
        for width in [920, 600, 1200] {
            driver.resize((width, 400)).unwrap();
            let fresh = harness(State::new(), ResponsiveRoot::new(Rc::default()), width);
            assert_eq!(driver.inspection().root.rect, fresh.inspection().root.rect);
            assert_eq!(
                driver
                    .inspection()
                    .root
                    .children
                    .iter()
                    .map(|child| child.rect)
                    .collect::<Vec<_>>(),
                fresh
                    .inspection()
                    .root
                    .children
                    .iter()
                    .map(|child| child.rect)
                    .collect::<Vec<_>>()
            );
        }
        let before = builds.get();
        driver.resize((1200, 400)).unwrap();
        assert_eq!(builds.get(), before);

        let (state, root) = editing_fixture();
        let scroll = root.scroll.clone();
        let mut editor = harness(state, root, 1200);
        editor.tab(false).expect("the editor is the first tab stop");
        editor.set_modifiers(&["ctrl".into()]);
        editor.key("a");
        editor.set_modifiers(&[]);
        editor.resize((920, 400)).unwrap();
        assert_eq!(
            editor
                .focused_access_node()
                .unwrap()
                .semantic_key
                .as_deref(),
            Some("editor")
        );
        assert_eq!(scroll.get(), (0.0, 120.0));
        editor.type_text("X");
        assert_eq!(
            editor.focused_access_node().unwrap().value.as_deref(),
            Some("X"),
            "typing replaces the selection retained across fresh composition"
        );
    }

    #[test]
    fn oscillating_composition_is_bounded_and_headless_skips_paint() {
        let root = ResponsiveRoot::new(Rc::default());
        let oscillate = root.oscillate.clone();
        let paints = root.paints.clone();
        let builds = root.builds.clone();
        let mut driver = harness(State::new(), root, 1200);
        oscillate.set(true);
        let before = builds.get();
        let painted = paints.get();
        assert!(matches!(
            driver.resize((920, 400)),
            Err(crate::widget::WidgetLayoutError::CompositionDidNotSettle)
        ));
        assert!(builds.get() - before <= 8);
        assert_eq!(
            paints.get(),
            painted,
            "failed settlement never reaches paint"
        );
        oscillate.set(false);
        driver.resize((920, 400)).unwrap();
        assert_eq!(paints.get(), painted + 1);

        let root = ResponsiveRoot::new(Rc::default());
        root.oscillate.set(true);
        let builds = root.builds.clone();
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        let root = app.layout.root_node.unwrap();
        assert!(run_layout_pass(&mut app, root, 1200, 400, 1.0, false, None).is_none());
        assert!(builds.get() <= 8);
        assert!(app.layout.last_layout_logical.is_none());
    }
    #[derive(Clone, Debug, PartialEq)]
    enum NativeCall {
        Mount(usize),
        Unmount(usize),
        Geometry(usize, KurboRect),
        Visible(usize, bool),
    }
    struct CountingNativeBackend {
        generation: usize,
        calls: Rc<std::cell::RefCell<Vec<NativeCall>>>,
    }
    impl crate::widget::NativeViewBackend for CountingNativeBackend {
        fn mount(&mut self, _: &crate::widget::NativeViewHost<'_>) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(NativeCall::Mount(self.generation));
            Ok(())
        }
        fn unmount(&mut self) {
            self.calls
                .borrow_mut()
                .push(NativeCall::Unmount(self.generation));
        }
        fn set_geometry(&mut self, geometry: crate::widget::NativeViewGeometry) {
            self.calls
                .borrow_mut()
                .push(NativeCall::Geometry(self.generation, geometry.logical));
        }
        fn set_visible(&mut self, visible: bool) {
            self.calls
                .borrow_mut()
                .push(NativeCall::Visible(self.generation, visible));
        }
        fn set_focus(&mut self, _: bool) {}
        fn set_enabled(&mut self, _: bool) {}
    }

    #[test]
    fn review_failed_headless_resize_can_return_to_last_successful_size() {
        let root = ResponsiveRoot::new(Rc::default());
        let oscillate = root.oscillate.clone();
        let mut driver = harness(State::new(), root, 1200);
        oscillate.set(true);
        assert!(driver.resize((920, 400)).is_err());
        oscillate.set(false);
        driver.resize((1200, 400)).unwrap();
        assert_eq!(driver.inspection().viewport, [1200, 400]);
        assert_eq!(driver.inspection().root.children.len(), 3);
        assert_eq!(driver.inspection().root.rect[2], 1200.0);
        driver.resize((920, 400)).unwrap();
        assert_eq!(driver.inspection().root.children.len(), 2);
    }

    #[test]
    fn review_active_trap_retains_keyed_selection_after_replacing_and_reordering_controls() {
        let (state, mut root) = editing_fixture();
        root.trap = true;
        let mut app = App::new_with_controlled_inputs(state, Box::new(root));
        app.init_layout();
        frame(&mut app, 1200);
        let editor = app.focusable_order[0];
        app.set_focused_node(Some(editor));
        app.mutate_node(editor, |widget| widget.set_state("5:b:1"))
            .unwrap();
        for width in [920, 600, 1200] {
            frame(&mut app, width);
            assert!(app.accessibility.focus_trap_active);
            let focused = app.focused_node.expect("modal editor remains focused");
            assert!(app.focusable_order.contains(&focused));
            assert_eq!(
                app.inspect_node(focused, |widget| widget
                    .accessibility()
                    .unwrap()
                    .semantic_key)
                    .unwrap()
                    .as_deref(),
                Some("editor")
            );
            assert!(
                app.inspect_node(focused, |widget| widget.is_focused())
                    .unwrap()
            );
            assert_eq!(
                app.inspect_node(focused, |widget| widget.get_state())
                    .unwrap()
                    .as_deref(),
                Some("5:b:1")
            );
        }
    }

    #[test]
    fn review_native_views_mount_only_after_successful_final_composition() {
        let mut root = ResponsiveRoot::new(Rc::default());
        let calls = Rc::new(std::cell::RefCell::new(Vec::new()));
        root.native_calls = Some(calls.clone());
        let builds = root.builds.clone();
        let oscillate = root.oscillate.clone();
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        let host = crate::widget::NativeViewHost::headless(app.input.platform, 1.0);
        crate::widget::mount_native_views(app.root_widget.as_mut(), &host);
        for width in [1200, 920] {
            let mounts = calls
                .borrow()
                .iter()
                .filter(|call| matches!(call, NativeCall::Mount(_)))
                .count();
            app.layout.layout_dirty = true;
            let node = app.layout.root_node.unwrap();
            run_layout_pass(&mut app, node, width, 400, 1.0, false, Some(&host)).unwrap();
            let final_generation = builds.get();
            assert_eq!(
                calls
                    .borrow()
                    .iter()
                    .filter(|call| matches!(call, NativeCall::Mount(_)))
                    .count(),
                mounts + 1,
                "mount final composition exactly once"
            );
            assert!(
                calls
                    .borrow()
                    .contains(&NativeCall::Mount(final_generation))
            );
            assert!(calls.borrow().iter().any(|call| matches!(call, NativeCall::Geometry(generation, rect) if *generation == final_generation && rect.width() == f64::from(width))));
            let mut scene = Scene::new();
            app.root_widget.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut app.text_renderer,
                &app.theme,
                &PresenceCtx::default(),
            );
            assert!(
                calls
                    .borrow()
                    .contains(&NativeCall::Visible(final_generation, true))
            );
        }
        let mounts = calls
            .borrow()
            .iter()
            .filter(|call| matches!(call, NativeCall::Mount(_)))
            .count();
        oscillate.set(true);
        app.layout.layout_dirty = true;
        let node = app.layout.root_node.unwrap();
        assert!(run_layout_pass(&mut app, node, 600, 400, 1.0, false, Some(&host)).is_none());
        assert_eq!(
            calls
                .borrow()
                .iter()
                .filter(|call| matches!(call, NativeCall::Mount(_)))
                .count(),
            mounts,
            "failed intermediate compositions never attach"
        );
        oscillate.set(false);
        let node = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, node, 920, 400, 1.0, false, Some(&host)).unwrap();
        assert_eq!(
            app.root_widget.get_rect().width(),
            920.0,
            "live recovery also invalidates failed geometry"
        );
    }
    #[test]
    fn review_new_modal_does_not_restore_outside_focus_or_copy_selection_to_fallback() {
        let (state, mut root) = editing_fixture();
        root.modal_on_narrow = true;
        let mut app = App::new_with_controlled_inputs(state, Box::new(root));
        app.init_layout();
        frame(&mut app, 1200);
        assert!(!app.accessibility.focus_trap_active);
        let outside = app.focusable_order[0];
        app.set_focused_node(Some(outside));
        app.mutate_node(outside, |widget| widget.set_state("5:b:1"))
            .unwrap();
        frame(&mut app, 920);
        assert!(app.accessibility.focus_trap_active);
        let focused = app.focused_node.expect("modal owns focus");
        assert!(app.focusable_order.contains(&focused));
        assert!(
            app.inspect_node(focused, |widget| widget.is_focused())
                .unwrap()
        );
        assert_ne!(
            app.inspect_node(focused, |widget| widget
                .accessibility()
                .unwrap()
                .semantic_key)
                .unwrap()
                .as_deref(),
            Some("outside")
        );
        assert_eq!(
            app.inspect_node(focused, |widget| widget.get_state())
                .unwrap()
                .as_deref(),
            Some("0"),
            "fallback editor keeps its own caret and no selection"
        );
        let outside = app
            .layout
            .node_map
            .keys()
            .copied()
            .find(|&node| {
                app.inspect_node(node, |widget| {
                    widget
                        .accessibility()
                        .and_then(|access| access.semantic_key)
                        .as_deref()
                        == Some("outside")
                })
                .unwrap_or(false)
            })
            .unwrap();
        assert!(
            !app.inspect_node(outside, |widget| widget.is_focused())
                .unwrap()
        );
    }
}

#[cfg(test)]
#[path = "app/card_composition_tests.rs"]
mod card_composition_tests;

#[cfg(test)]
#[path = "app/form_composition_tests.rs"]
mod form_composition_tests;

#[cfg(test)]
#[path = "app/manual_composition_tests.rs"]
mod manual_composition_tests;

#[cfg(test)]
#[path = "app/number_input_layout_tests.rs"]
mod number_input_layout_tests;

#[cfg(test)]
#[path = "app/history_tests.rs"]
mod framework_history_tests;

#[path = "app/history.rs"]
mod framework_history;

#[path = "app/document_routes.rs"]
mod document_routes;

#[cfg(test)]
#[path = "app/focus_selection_tests.rs"]
mod focus_selection_tests;

#[cfg(test)]
#[path = "app/local_privacy_tests.rs"]
mod local_privacy_tests;

/// Finish the current writer and every retained outgoing-owner failure before
/// reporting widget-owned durability. Reports remain owned by the runner.
pub(crate) fn flush_widget_owned_persistence(
    root: &mut dyn Widget,
    reports: &mut Vec<crate::application::ComponentCloseReport>,
) -> Result<(), String> {
    reports.extend(root.take_component_close_reports());
    let current = root
        .try_flush_owned_persistence()
        .unwrap_or_else(|| Err("widget-owned persistence requires a checked flush hook".into()));
    reports.extend(root.take_component_close_reports());
    for report in reports.iter_mut() {
        if report.record_error.is_some() && report.record_retry.is_some() {
            let _ = report.retry_record();
        }
    }
    current?;
    for report in reports.iter() {
        if let Some(error) = report
            .record_error
            .as_ref()
            .or(report.view_error.as_ref())
            .or(report.teardown_error.as_ref())
        {
            return Err(error.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "app/notes_rich_controls_tests.rs"]
mod notes_rich_controls_tests;
