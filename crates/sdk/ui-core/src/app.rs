use winit::{
    dpi::PhysicalPosition,
    event::{ElementState, MouseButton, WindowEvent},
};
// The native event-loop runner (`app/native_runner.rs`) reaches these through
// `use super::*`.
#[cfg(feature = "native")]
use crate::gpu::RenderContext;
#[cfg(feature = "native")]
use crate::render_thread::RenderThreadHandle;
#[cfg(feature = "native")]
use crate::render_thread::timing::{MainStage, MainStages, record_main_stage};
#[cfg(feature = "native")]
use crate::render_thread::{FrameOutcome, RenderRequest, spawn_render_thread};
#[cfg(feature = "native")]
use crate::widget::AudiencePresentationWidgetExt as _;
#[cfg(feature = "native")]
use crate::widget::{FloatingSurfaceWidgetExt as _, FullscreenWidgetExt as _};
#[cfg(feature = "native")]
use std::sync::{Arc, mpsc};
#[cfg(feature = "native")]
use taffy::AvailableSpace;
#[cfg(feature = "native")]
use vello::{Renderer, RendererOptions, peniko::Color};
#[cfg(feature = "native")]
use vello::{Scene, kurbo::Affine, kurbo::Point};
#[cfg(feature = "native")]
use winit::{
    application::ApplicationHandler,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{CursorIcon, Fullscreen, Window, WindowId},
};

use std::cell::Cell;

#[cfg(feature = "native")]
#[path = "profiling/native_input_isolation.rs"]
mod native_input_isolation;

#[cfg(feature = "native")]
#[path = "animation_schedule.rs"]
mod animation_schedule;

mod action_directory_snapshot;
mod bound_record_sync;
mod close_admission;
mod document_history;
pub use document_history::DocumentHistorySnapshot;
include!("app/floating.rs");
mod runtime;
use runtime::{
    AccessibilityRuntime, ActionRuntime, BrokerReply, CollaborationRuntime, ContinuationRuntime,
    HistoryRuntime, InputRuntime, LayoutRuntime, PersistenceRuntime, PresenceRuntime,
    WindowRuntime,
};
include!("app/continuation.rs");
include!("app/actions.rs");
include!("app/accessibility.rs");
include!("app/collaboration.rs");
include!("app/persistence.rs");
include!("app/host.rs");
#[cfg(feature = "native")]
mod native_runner;
#[cfg(all(windows, feature = "native"))]
pub(crate) use native_runner::prepare_native_renderer;

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

/// Synthetic input for `EventLoopProxy`: each variant becomes the real
/// `WindowEvent`(s) and routes through the same handler, so injected and
/// real-hardware events take the identical code path.
#[derive(Debug, Clone)]
pub enum UserEvent {
    Hover {
        x: f64,
        y: f64,
    },
    /// `CursorMoved` → Pressed → Released, dispatched as one unit.
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
    #[cfg(feature = "native")]
    AccessKit(accesskit_winit::WindowEvent),
}

#[cfg(feature = "native")]
impl From<accesskit_winit::Event> for UserEvent {
    fn from(e: accesskit_winit::Event) -> Self {
        UserEvent::AccessKit(e.window_event)
    }
}
use crate::collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, CollaborationManager, PresencePayload,
};
use crate::permissions::PermissionContext;
use crate::presence::{PresenceCtx, PresenceKey};
use crate::state::State;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::widget::Widget;
use taffy::{NodeId, TaffyTree};
use vello::kurbo::Rect as KurboRect;

/// Cached [`crate::host::RunnerDiagnostics::perf_log`] check
/// (`QUICK_UI_PERF=1` natively), so the per-frame read is cheap.
#[cfg(feature = "native")]
fn perf_enabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| crate::host::environment().diagnostics.perf_log)
}

/// `QUICK_UI_NO_SCROLL_OPT=1` disables the scroll-reposition fast path,
/// forcing a full `compute_layout` on every `layout_dirty` frame — a safety
/// valve if a subtree ever needs a re-solve without reporting
/// `layout_needs_measure`.
#[cfg(feature = "native")]
fn scroll_opt_disabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        crate::host::environment()
            .diagnostics
            .disable_scroll_fast_path
    })
}

/// Whether an animation request should keep the event loop on its 60 Hz pump.
/// Reduced-motion widgets paint their settled state without an animation
/// redraw, so their raw demand must not keep an otherwise-idle app awake.
#[cfg(feature = "native")]
const fn animation_pump_needed(needs_frame: bool, reduce_motion: bool) -> bool {
    needs_frame && !reduce_motion
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

#[cfg(feature = "native")]
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

#[cfg(feature = "native")]
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
#[cfg(feature = "native")]
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
#[cfg(feature = "native")]
pub(crate) const POINTER_BROADCAST_MIN_GAP_MS: u64 = 33;

#[derive(Clone, Copy, Debug)]
struct DirectedInvocation {
    wire_id: u64,
    cancellation_requested: bool,
}

/// Public-in-crate so the throttle decision is unit-testable without a real
/// event loop.
#[cfg(feature = "native")]
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
#[cfg(feature = "native")]
pub(crate) fn fullscreen_request_decision(requested: bool, want: Option<bool>) -> Option<bool> {
    match want {
        Some(w) if w != requested => Some(w),
        _ => None,
    }
}

/// Lets a nested settings/appearance pane drive the app's global appearance
/// without a back-channel.
#[cfg(feature = "native")]
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
/// metadata for focus, presence, animation, and “show me where” behavior.
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independently toggled, orthogonal opt-in/state bit with its own doc comment; grouping them into sub-structs would ripple through every read/write site in this file for no behavioural gain"
)]
#[cfg_attr(
    not(feature = "native"),
    allow(
        dead_code,
        reason = "some fields are read only by the native event-loop runner"
    )
)]
pub struct App {
    #[cfg(all(windows, feature = "native"))]
    startup_renderer: Option<native_runner::RendererPreparation>,
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
    theme_store_stamp: Option<crate::time::SystemTime>,
    last_theme_store_check: Instant,
    input: InputRuntime,

    focus_history: HistoryRuntime,

    collaboration: CollaborationRuntime,
    pub presence_ctx: PresenceCtx,
    pub permission_ctx: PermissionContext,

    presence: PresenceRuntime,

    accessibility: AccessibilityRuntime,

    /// Compared against [`crate::widget::Widget::fullscreen_intent`] each
    /// frame so a steady desired state issues one `set_fullscreen` call —
    /// see [`fullscreen_request_decision`].
    window: WindowRuntime,
    /// To distinguish app-driven from compositor-driven transitions.
    pub(crate) legacy_persistence: crate::launch::LegacyPersistencePolicy,
    persistence: PersistenceRuntime,
    autosave_generation: u64,
    document_routes: crate::history_mode::routes::OwnerDocumentRoutes,

    actions: ActionRuntime,
    document_history: Box<crate::standalone_history_host::StandaloneHistoryHost>,
    history_chrome_enabled: bool,
    native_document_chrome: bool,
    history_layout: Option<crate::standalone_history_host::HostHistoryLayout>,
    history_viewport_width: f64,
    history_focus_pending: bool,
    continuation: ContinuationRuntime,
    last_published_playback: Option<Vec<media::handoff::PlaybackReport>>,
    last_playback_publish: Option<Instant>,
    profiler: Option<crate::profiling::FrameProfiler>,
    native_input_probe: Option<Box<dyn Fn() -> serde_json::Value>>,

    /// Optional app → shell lifecycle channel for detachable surfaces. It is
    /// discovered from the launcher-provided service fd and absent outside a
    /// Tilo session, where apps simply keep their overlays attached.
    floating: FloatingRuntime,
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
    theme_store_stamp: fn() -> Option<crate::time::SystemTime>,
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
        // The screen reader below speaks through the host's services, so a
        // process that never installed any gets the native desktop's.
        #[cfg(feature = "native")]
        crate::host::native::install_default_if_unset(None);
        Self {
            collab_manager,
            configured_theme: ConstructionValue::Lazy(Theme::configured),
            text_renderer: ConstructionValue::Lazy(TextRenderer::new),
            palette_source: PaletteSource::Owned {
                renderer: TextRenderer::new,
                field: crate::widget::SearchField::new,
            },
            watch_system_settings: true,
            debug_mode: || {
                cfg!(debug_assertions) && crate::host::environment().diagnostics.debug_overlay
            },
            theme_store_stamp: || {
                settings_store::settings_path()
                    .and_then(|path| std::fs::metadata(path).ok()?.modified().ok())
            },
            local_author: ConstructionValue::Lazy(crate::collaboration::local_user_id),
            presence_ctx: ConstructionValue::Lazy(App::initial_presence_context),
            screen_reader: || {
                if crate::host::environment().accessibility.screen_reader
                    || App::persisted_accessibility_bool("screen_reader")
                {
                    Some(App::configured_screen_reader())
                } else {
                    None
                }
            },
            high_contrast: || crate::host::environment().accessibility.high_contrast,
            reduce_motion: || crate::host::environment().accessibility.reduce_motion,
            text_scale: || {
                crate::host::environment()
                    .accessibility
                    .text_scale
                    .unwrap_or(1.0)
            },
            ui_scale: || {
                crate::host::environment()
                    .accessibility
                    .ui_scale
                    .unwrap_or(1.0)
            },
            pending_continuation_restore: crate::collaboration::continuation_restore_env,
            profiler: crate::profiling::FrameProfiler::from_env,
            floating_surface_client:
                crate::floating_surface::FloatingSurfaceServiceClient::discover,
            startup_presence_scope: PresenceKey::for_local_workspace,
            surface_id: ConstructionValue::Lazy(crate::local_surface_id),
        }
    }

    #[cfg(any(test, all(feature = "native", feature = "test-support")))]
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

    #[cfg(feature = "native")]
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
        let history = component.history.take();
        let mut root = Box::new(crate::widget::BoundAppWidget { component });
        if let Some(registration) = &history
            && let Err(error) = registration.validate_root(root.as_ref())
        {
            return Err(root
                .component
                .setup_error(crate::launch::LaunchError::Operation(error.to_string())));
        }
        let mut app = Self::from_scoped_parts_with_inputs(scope, State::new(), root, || inputs);
        if let Some(registration) = history
            && let Err(error) = app.install_history(registration)
        {
            app.close_components();
            return Err(crate::launch::LaunchError::ComponentSetup {
                source: Box::new(crate::launch::LaunchError::Operation(error.to_string())),
                record: binding.checkout,
                records: binding.records,
                reports: app.take_component_close_reports(),
            });
        }
        Ok(app)
    }

    pub(crate) fn native_component_presentation() -> crate::application::ComponentRunnerInputs {
        let accessibility = crate::host::environment().accessibility;
        let theme = Theme::configured().with_a11y(
            &crate::a11y::A11ySettings {
                high_contrast: accessibility.high_contrast,
                reduce_motion: accessibility.reduce_motion,
                ..Default::default()
            },
            accessibility.text_scale.unwrap_or(1.0),
            accessibility.ui_scale.unwrap_or(1.0),
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
    #[cfg(feature = "native")]
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

    #[cfg(any(test, all(feature = "native", feature = "test-support")))]
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

    #[cfg(feature = "native")]
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
            #[cfg(all(windows, feature = "native"))]
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
            last_theme_store_check: crate::time::now(),
            focus_history: HistoryRuntime::new(),
            collaboration: CollaborationRuntime::new(
                inputs.collab_manager,
                inputs.local_author.resolve(),
            ),
            presence_ctx: inputs.presence_ctx.resolve(),
            permission_ctx: PermissionContext::permissive(),
            presence: PresenceRuntime::new(),
            accessibility: AccessibilityRuntime::new(configured_theme, (inputs.screen_reader)()),
            window: WindowRuntime::new(),
            legacy_persistence: crate::launch::LegacyPersistencePolicy::Host,
            persistence: PersistenceRuntime::new(),
            autosave_generation: 0,
            document_routes: crate::history_mode::routes::OwnerDocumentRoutes::default(),
            actions: ActionRuntime::new(),
            document_history: Box::new(crate::standalone_history_host::StandaloneHistoryHost::new()),
            history_chrome_enabled: false,
            native_document_chrome: true,
            history_layout: None,
            history_viewport_width: 1000.0,
            history_focus_pending: false,
            continuation: ContinuationRuntime::new((inputs.pending_continuation_restore)()),
            last_published_playback: None,
            last_playback_publish: None,
            profiler: (inputs.profiler)(),
            native_input_probe: None,
            floating: FloatingRuntime::new((inputs.floating_surface_client)()),
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
}

impl App {
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

    /// The complete machine-driveable surface: domain resources declared by
    /// the app plus ordinary controls projected from its retained semantic
    /// tree. Ctrl+K, scripts, and agents therefore consume one catalogue.

    /// Audit every retained control binding against the fresh resource tree.
    /// This is public so development tooling can turn remaining explicit
    /// legacy controls and stale/duplicate action addresses into a coverage
    /// report without inspecting widget implementation details.

    /// Locate every on-screen affordance for one canonical action and rank
    /// usable, visible, short paths first. Agents still invoke `address`
    /// directly; the chosen route makes the resulting interaction project
    /// into the same UI a person sees without fragile coordinate clicking.

    /// Transport-only projection. Each component is validated with its own
    /// registry before its domain resource is assigned an opaque publication alias.

    /// Publish the live resource/action tree through the session broker.
    /// Level-triggered at the wire, diffed here so an unchanged frame emits
    /// nothing; publishing an empty list explicitly withdraws a closed tree.

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
}

impl App {
    /// Publish private source descriptors over the authenticated local app
    /// broker. Public cross-device state is derived by the shell and remains
    /// path/URL-free; this report never enters the room document.
    fn publish_playback_handoff(&mut self) {
        if !self.collaboration.manager.is_connected() {
            return;
        }
        let now = crate::time::now();
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
}

impl App {
    /// Reconciles both directions, idempotently: app-driven (push
    /// `fullscreen_intent` to the window on change) and compositor-driven
    /// (read the actual state back and notify the root on a transition, e.g.
    /// a global un-fullscreen keybind or a denied request).
    #[cfg(feature = "native")]
    fn reconcile_fullscreen(&mut self, window: &Window) -> bool {
        let mut changed = false;

        let want = self.root_widget.fullscreen_intent();
        if let Some(new_state) = fullscreen_request_decision(self.window.fullscreen_requested, want)
        {
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
            self.window.fullscreen_requested = new_state;
            changed = true;
        }

        let actual = window.fullscreen().is_some();
        changed |= self.note_actual_fullscreen(actual);

        changed
    }

    /// Split from [`Self::reconcile_fullscreen`] so the App→root notification
    /// wiring is unit-testable without a live winit window.
    #[cfg(feature = "native")]
    fn note_actual_fullscreen(&mut self, actual: bool) -> bool {
        if actual == self.window.fullscreen_active {
            return false;
        }
        self.window.fullscreen_active = actual;
        self.root_widget.on_fullscreen_changed(actual);
        self.needs_sync = true;
        self.layout.layout_dirty = true;
        true
    }

    /// Level-triggered: a no-op once the user's choice is caught up.
    #[cfg(feature = "native")]
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

    #[cfg(feature = "native")]
    fn reconcile_configured_theme(&mut self) -> bool {
        self.reconcile_configured_theme_with(
            crate::time::now(),
            || {
                settings_store::settings_path()
                    .and_then(|path| std::fs::metadata(path).ok()?.modified().ok())
            },
            Self::apply_configured_theme,
        )
    }

    #[cfg(feature = "native")]
    fn reconcile_configured_theme_with(
        &mut self,
        now: Instant,
        stamp: impl FnOnce() -> Option<crate::time::SystemTime>,
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

    #[cfg(feature = "native")]
    fn apply_configured_theme(&mut self) {
        self.accessibility.base_theme = Theme::configured();
        // Screen reader is a device/personal setting, not a per-application
        // opt-in. Every Tilo app watches the same file and changes its real
        // narrator lifecycle when Personal Settings changes it.
        let configured_reader = Self::persisted_accessibility_bool("screen_reader")
            || crate::host::environment().accessibility.screen_reader;
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
    #[cfg(feature = "native")]
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
    #[cfg(feature = "native")]
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
}

impl App {
    /// Called once per event-loop wakeup, so it must be cheap and
    /// non-blocking.

    /// Decides *whether* to publish and under what scope, not what space the
    /// pointer is in — see [`Self::set_pointer_space`].

    /// A miss leaves the current follow target untouched.

    /// The server attributes the broadcast to this user's id automatically,
    /// so callers only specify what changed.

    /// Publish the collaboration rail's current status through the real room
    /// presence path. `None` is an explicit clear. Only workspace keys are
    /// accepted, preventing an intention from being accidentally scoped to a
    /// single widget and disappearing as the participant navigates.

    /// Live-edit path (the late-join handshake still ships a full snapshot).

    /// The room index is a distinct document from the live record.  Keep its
    /// incremental cursor and route separate so an annotation/document delta
    /// can never be merged into the index (or vice versa).

    /// Unlike focus presence, this is polled on the root regardless of
    /// focus.

    /// The subset has no `PresencePayload` variant of its own — it rides the
    /// existing presence frame inside a
    /// [`PresenceKey::view`](crate::presence::PresenceKey::view) key with an
    /// inert paired payload; the receiver recognises the key and decodes the
    /// subset instead.

    /// Following a peer who hasn't published a View yet latches the intent
    /// without moving anything; the viewport snaps to them once their first
    /// View arrives.

    /// Lets integration tests drive the collab pump without a real winit
    /// event loop.
    #[doc(hidden)]

    /// Apply a document envelope received on the unqualified room route.
    /// The broker authenticated the route; discovery still validates and
    /// bounds its names before exposing them to the app.

    /// Drain only document traffic on the room-index route. Actions and
    /// presence are record scoped, so an index update cannot alter the app's
    /// resource/action namespace or presentation state.

    /// Shared by the per-frame tick and the headless test path. Returns
    /// `true` iff at least one event was processed.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered pump drains directory callbacks and applies every collaboration lifecycle event before the next frame"
    )]

    fn collab_event_may_change_action_surface(event: &CollabEvent) -> bool {
        !matches!(
            event,
            CollabEvent::FollowChanged { .. }
                | CollabEvent::RemoteCursorMoved { .. }
                | CollabEvent::RemotePresenceUpdate { .. }
        )
    }
}

impl App {
    #[doc(hidden)]
    pub fn state_mut_for_test(&mut self) -> &mut crate::state::State {
        self.invalidate_widget_resource_cache();
        &mut self.state
    }

    /// The decode lives on [`crate::presence::PresenceCtx`] so the
    /// compositor reads presence the same way an app does; what stays here
    /// is what only a document holder can do (follow a peer's viewport,
    /// answer a late joiner with full state).

    /// Unique admitted node visits keep child reports and requests off shell history.

    /// Finalize the document boundary for a handled native event. Shared host
    /// controls only change presentation and must not close pending app typing.

    /// The platform IME contract for the focused widget. A field opts in via
    /// [`Widget::ime_purpose`]; buttons and other focusable controls therefore
    /// never activate `text-input-v3` accidentally.

    /// Only fires if focus did NOT move — a move is narrated separately by
    /// [`set_focused_node`].

    /// Typing is left to the widget's own echo so this never double-speaks
    /// an edit.

    /// Announces changed [live regions](crate::a11y::AccessNode::live)
    /// without moving focus.

    /// Gated on the focus-trap signal, not `modal_capture`: that governs only
    /// pointer click-through — gating Tab on it instead either froze focus
    /// inside a dialog or let it escape with no capture.

    /// Tab normally belongs to the host's focus order. A focused editor-like
    /// widget may explicitly claim it first (for indentation or completion),
    /// without broadening that exception to arbitrary widgets or escaping an
    /// active focus trap.

    /// Shared native Tab admission, preserving text-input and modal capture.

    /// `scale` is the window scale factor so bounds land in physical pixels
    /// (what AT-SPI expects).

    /// Resolves both retained AccessKit nodes and synthetic semantic children
    /// (virtual rows/cells) to their owning widget. The widget gets first
    /// refusal; synthetic children additionally carry an opaque stable target
    /// produced by that widget. If a retained widget declines, `Action::Focus`
    /// moves keyboard focus and activation returns click coordinates.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered native accessibility door preserves existing resource, semantic and pointer fallback precedence"
    )]

    /// Complete an accessibility activation for legacy hand-hit-tested
    /// controls that do not implement `perform_action` yet. The semantic
    /// target has already resolved to this exact point; routing the usual
    /// move/press/release sequence through the widget tree preserves the same
    /// behavior as hardware input instead of reporting a false success.

    /// Node identity is preserved for everything that recurred, so a
    /// consumer reacts to just the diff.

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

    /// Byte-for-byte the same path as a Ctrl+K palette selection
    /// ([`Self::action_palette_handle_key`]).

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
        self.focus_history.stable_focus_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.focus_history.stable_metadata = self.focused_node.and_then(|id| {
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
    #[cfg(feature = "native")]
    pub(crate) fn history_storage_generation(&self) -> u64 {
        self.autosave_generation
    }
    #[cfg(feature = "native")]
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

    /// Takes effect when the window is created, so set before [`App::run`].
    #[must_use]
    pub fn with_window_title(mut self, title: impl Into<String>) -> Self {
        self.window.title = Some(title.into());
        self
    }

    /// Set the stable Wayland application id announced by this window.
    #[must_use]
    pub fn with_window_app_id(mut self, app_id: impl Into<String>) -> Self {
        self.window.app_id = Some(app_id.into());
        self
    }

    /// `None` clears the override and restores automatic resolution.
    pub fn set_window_title(&mut self, title: Option<String>) {
        self.window.title = title;
    }

    /// Precedence: explicit override → `TILO_APP_NAME` → package id → binary
    /// name → generic default.
    pub fn resolved_window_title(&self) -> String {
        resolve_window_title(self.window.title.as_deref())
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

#[cfg(feature = "native")]
fn native_floating_logical_size(rect: KurboRect) -> winit::dpi::LogicalSize<f64> {
    winit::dpi::LogicalSize::new(
        rect.width().round().max(1.0),
        rect.height().round().max(1.0),
    )
}

#[cfg(feature = "native")]
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

#[cfg(feature = "native")]
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
        retry.observe(FrameOutcome::Occluded, crate::time::now());
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

#[cfg(feature = "native")]
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

#[cfg(feature = "native")]
fn auxiliary_redraw_allowed(render: Option<&RenderThreadHandle>) -> bool {
    render.is_none_or(|render| {
        !render.buffers.redraw_deferred() && render.retry.allows(crate::time::now())
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
#[cfg(feature = "native")]
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
    let environment = crate::host::environment();
    let app_name = environment.identity.app_name.clone();
    let package_id = environment.identity.package_id.clone();
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
/// ([`crate::presence::PointerSpace::Surface`]) — both ends must spell the
/// surface the same way by calling this same function.
pub fn local_surface_id() -> String {
    let package_id = crate::host::environment().identity.package_id.clone();
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

#[cfg(test)]
#[path = "app/large_document_tests.rs"]
mod large_document_tests;

/// Finish the current writer and every retained outgoing-owner failure before
/// reporting widget-owned durability. Reports remain owned by the runner.
///
/// # Errors
/// Returns the current writer failure or a retained owner's unresolved failure.
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
            // The report retains the actual outgoing owner and exact legacy
            // record target. Successful retry clears only that owner's error.
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
mod render_retry_tests {
    use super::*;
    #[test]
    fn unavailable_surface_retries_wait_one_frame_instead_of_spinning() {
        let now = crate::time::now();
        for outcome in [FrameOutcome::Skipped, FrameOutcome::Reconfigured] {
            let deadline = render_retry_deadline(outcome, now).unwrap();
            assert_eq!(
                deadline.duration_since(now),
                Duration::from_nanos(16_666_667)
            );
        }
        assert!(render_retry_deadline(FrameOutcome::Presented, now).is_none());
        assert!(render_retry_deadline(FrameOutcome::SurfaceGone, now).is_none());
    }
}

#[cfg(test)]
mod legacy_owner_routing_tests {
    use super::*;
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    use std::{collections::HashMap, rc::Rc};

    struct OwnerProbe {
        label: &'static str,
        report: Option<crate::application::ComponentCloseReport>,
        flushes: Rc<Cell<usize>>,
    }

    impl Widget for OwnerProbe {
        fn sync(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) {}
        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            _: &Theme,
            nodes: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = tree.new_leaf(taffy::Style::default()).unwrap();
            nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _: &taffy::TaffyTree<()>, _: NodeId, _: Point) {}
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
            _: &WindowEvent,
            _: f64,
            _: &winit::event::Modifiers,
            _: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
            _: &mut bool,
            _: &mut Option<NodeId>,
            _: NodeId,
            _: &taffy::TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "LegacyOwnerProbe"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            use crate::a11y::{AccessNode, AccessState, Role};
            Some(AccessNode::new(Role::Generic).with_children(vec![
                AccessNode::new(Role::Button).with_key(1)
                    .with_name(format!("{} named", self.label))
                    .with_action_target("owner:named")
                    .with_state(AccessState::new().focusable(true)),
                AccessNode::new(Role::Button).with_key(7)
                    .with_name(format!("{} keyed", self.label))
                    .with_action_target("tilo.key-action:owner:7"),
            ]))
        }
        fn perform_semantic_action(
            &mut self,
            target: &str,
            action: &crate::a11y::Action,
            doc: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
        ) -> bool {
            if target != "owner:named" {
                return false;
            }
            match action {
                crate::a11y::Action::Activate => {
                    doc.put(ROOT, "selected", format!("{} named", self.label))
                        .unwrap();
                    true
                }
                crate::a11y::Action::Focus => true,
                _ => false,
            }
        }
        fn perform_action_at(
            &mut self,
            action: &crate::a11y::Action,
            keys: &[u64],
            doc: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
        ) -> bool {
            if keys != [7] || !matches!(action, crate::a11y::Action::Activate) {
                return false;
            }
            doc.put(ROOT, "selected", format!("{} keyed", self.label))
                .unwrap();
            true
        }
        fn take_component_close_reports(
            &mut self,
        ) -> Vec<crate::application::ComponentCloseReport> {
            self.report.take().into_iter().collect()
        }
        fn try_flush_owned_persistence(&mut self) -> Option<Result<(), String>> {
            assert!(
                self.report.is_none(),
                "the outgoing failure has moved into host custody"
            );
            self.flushes.set(self.flushes.get() + 1);
            Some(Ok(()))
        }
    }
    impl crate::widget::AppWidget for OwnerProbe {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Editor
        }
        fn app_title(&self) -> &str {
            self.label
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("T")
        }
        fn permission_hint(&self) -> Option<crate::widget::AppKind> {
            None
        }
    }

    fn tile(
        store: &crate::record::RecordStore,
        checkout: &crate::record::RecordCheckout,
        label: &'static str,
    ) -> crate::widget::TileHost {
        let mut tile =
            crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(OwnerProbe {
                label,
                report: None,
                flushes: Rc::new(Cell::new(0)),
            })));
        tile.bind_record(store.clone(), checkout.clone());
        tile
    }

    #[test]
    fn semantic_targets_resolve_current_legacy_aliases_across_disjoint_roots() {
        let store = crate::record::RecordStore::in_memory();
        let first = crate::record::RecordCheckout::new(
            crate::record::RecordId::for_tab(9901),
            crate::record::RecordKind::new("note"),
        );
        let second = crate::record::RecordCheckout::new(
            crate::record::RecordId::for_tab(9902),
            crate::record::RecordKind::new("note"),
        );
        store.create_record(&first).unwrap();
        store.create_record(&second).unwrap();
        let root = crate::widget::Column::new()
            .push(Box::new(tile(&store, &first, "first")))
            .push(Box::new(tile(&store, &second, "second")));
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.enable_document_history_chrome();
        app.init_layout();
        app.refresh_semantic_tree();
        let named = app
            .semantic_tree()
            .find_by_name("first named")
            .unwrap()
            .clone();
        let keyed = app
            .semantic_tree()
            .find_by_name("second keyed")
            .unwrap()
            .id
            .clone();
        let alias = named.widget_taffy_id.unwrap();
        assert!(
            alias >= (1 << 47),
            "fixture must exercise a legacy lifetime alias"
        );
        assert!(!app.layout.node_map.contains_key(&NodeId::from(alias)));
        assert!(
            app.invoke_semantic_action(&named.id, "activate", None)
                .unwrap()
        );
        assert!(
            app.invoke_semantic_action(&named.id, "focus", None)
                .unwrap()
        );
        let focused = app
            .focused_node
            .expect("focus uses the actual admitted owner");
        assert!(app.layout.node_map.contains_key(&focused));
        assert_ne!(u64::from(focused), alias);
        assert!(
            app.invoke_semantic_action(&keyed, "activate", None)
                .unwrap()
        );
        for (checkout, expected) in [(&first, "first named"), (&second, "second keyed")] {
            assert_eq!(
                store
                    .read_checked(&checkout.id, |doc| {
                        doc.get(ROOT, "selected")
                            .unwrap()
                            .unwrap()
                            .0
                            .as_str()
                            .unwrap()
                            .to_owned()
                    })
                    .unwrap(),
                expected
            );
        }
        assert!(app.state.doc.get(ROOT, "selected").unwrap().is_none());

        // Retain the old semantic tree while replacing the owners. Current
        // admission must refuse both stale targets instead of retargeting them.
        app.close_components();
        app.root_widget = Box::new(tile(&store, &first, "replacement"));
        app.init_layout();
        assert!(
            !app.invoke_semantic_action(&named.id, "activate", None)
                .unwrap()
        );
        assert!(
            !app.invoke_semantic_action(&keyed, "activate", None)
                .unwrap()
        );
        assert_eq!(
            store
                .read_checked(&first.id, |doc| {
                    doc.get(ROOT, "selected")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .unwrap(),
            "first named"
        );
    }

    #[test]
    fn widget_owned_runner_rejects_outgoing_error_drained_before_final_flush() {
        let flushes = Rc::new(Cell::new(0));
        let probe = OwnerProbe {
            label: "current owner",
            flushes: Rc::clone(&flushes),
            report: Some(crate::application::ComponentCloseReport {
                recovery: None,
                view_error: None,
                view_retry: None,
                record_error: Some("outgoing owner could not persist".into()),
                record_retry: None,
                record_retry_target: None,
                teardown_error: None,
            }),
        };
        let inputs = crate::launch::LegacyLaunchInputs::new(
            State::new(),
            Box::new(probe),
            crate::persist::DocWriter::disabled("fixture owns storage"),
        )
        .with_widget_owned_persistence();
        assert_eq!(
            crate::headless::run_initialized(&crate::headless::Spec::default(), inputs),
            3
        );
        assert_eq!(flushes.get(), 1, "the current owner must still be flushed");
    }
}
