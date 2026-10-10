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
use crate::render_thread::{FrameOutcome, RenderRequest};
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
mod handoff;
mod history_actions;
mod host_commands;

/// The broker-facing History door, which host chrome presents as its own.
#[cfg(any(feature = "native", test))]
pub(crate) const HISTORY_ACTION_RESOURCE: &str = history_actions::RESOURCE;
#[cfg(test)]
mod history_actions_tests;
mod model_lifecycle;
mod participants;
mod published_invocations;
mod retirement_assembly;
#[cfg(test)]
mod retirement_lifecycle_tests;
pub use document_history::DocumentHistorySnapshot;
include!("app/floating.rs");
mod runtime;
use runtime::{
    AccessibilityRuntime, ActionRuntime, BrokerReply, CollaborationRuntime, ContinuationRuntime,
    HistoryRuntime, InputRuntime, LayoutRuntime, PersistenceRuntime, PresenceRuntime,
    WindowRuntime,
};
include!("app/continuation.rs");
include!("app/dev_session.rs");
include!("app/dev_library.rs");
include!("app/actions.rs");
include!("app/accessibility.rs");
include!("app/collaboration.rs");
include!("app/persistence.rs");
include!("app/host.rs");
#[cfg(all(feature = "native", any(target_os = "android", test)))]
mod android_text_input;
#[cfg(feature = "native")]
mod native_runner;
/// How an app's error begins when its window stopped without being closed.
/// The shell recognises it in a failed launch's output.
pub const WINDOW_LOST: &str = "the window stopped without being closed";

/// Exit status of an app whose window stopped without being closed: the
/// launcher reports it as a failed launch rather than a clean exit.
pub const WINDOW_LOST_EXIT_CODE: i32 = 70;

#[cfg(feature = "native")]
mod input_publication;
#[cfg(feature = "native")]
mod surface;
#[cfg(all(any(windows, target_os = "linux"), feature = "native"))]
pub(crate) use native_runner::prepare_native_renderer;
#[cfg(feature = "native")]
pub use surface::AppSurface;
#[cfg(feature = "native")]
mod embedded;
#[cfg(feature = "native")]
pub use embedded::EmbeddedApp;

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
    /// A host command chosen in the operating system's own chrome (a menu
    /// item), entering the same door as the palette.
    HostCommand(tilo_actions::ControlInvocation),
    /// Text typed into the host's title-bar search field; search continues
    /// in the palette.
    HostSearch(String),
    /// Synchronize and redraw after typed state was changed by a worker.
    /// Created automatically by [`crate::LocalStateWriter`].
    Wake(crate::wake::WakeEvent),
    /// `tilo-app dev` is replacing this process. Flush the document and write
    /// the route and caret, then leave the event loop so the next process can
    /// read them.
    DevSessionStop,
    /// `tilo-app dev` rebuilt the app. Swap the contents of the windows this
    /// process already has. Do not open or close any of them.
    DevSessionReload,
    /// No `WindowEvent` analogue, so accesskit events get their own variant.
    #[cfg(feature = "native")]
    AccessKit {
        window_id: winit::window::WindowId,
        event: accesskit_winit::WindowEvent,
    },
    /// The soft keyboard queued edits for the focused field.
    #[cfg(all(feature = "native", target_os = "android"))]
    PlatformTextInput,
}

#[cfg(feature = "native")]
impl From<accesskit_winit::Event> for UserEvent {
    fn from(e: accesskit_winit::Event) -> Self {
        UserEvent::AccessKit {
            window_id: e.window_id,
            event: e.window_event,
        }
    }
}
use crate::collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, CollaborationManager, PresencePayload,
};
use crate::focus_navigation::Direction;
#[cfg(feature = "native")]
use crate::ime::ImeWindowState;
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
/// (`TILO_UI_PERF=1` natively), so the per-frame read is cheap.
#[cfg(feature = "native")]
fn perf_enabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| crate::host::environment().diagnostics.perf_log)
}

/// `TILO_UI_NO_SCROLL_OPT=1` disables the scroll-reposition fast path,
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
    fn animation_pump_sleeps_for_reduced_motion_and_clears_vanished_continuations() {
        // Case `reduced_motion_does_not_keep_the_idle_loop_awake`.
        {
            assert!(animation_pump_needed(true, false));
            assert!(!animation_pump_needed(true, true));
            assert!(!animation_pump_needed(false, false));
        }

        // Case `continuation_disappearance_clears_the_durable_projection`.
        {
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

#[derive(Clone, Debug)]
struct DirectedInvocation {
    wire_id: u64,
    cancellation_requested: bool,
    authorization: Option<crate::invocation_scope::ResourceInvocationScope>,
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

pub use crate::action_locus::ActionUiRoute;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independently toggled, orthogonal opt-in/state bit with its own doc comment; grouping them into sub-structs would ripple through every read/write site in this file for no behavioural gain"
)]
#[cfg_attr(
    not(feature = "native"),
    expect(
        dead_code,
        reason = "the theme-store watch and frame profiler are read only by the native event-loop runner, which shared app code feeds"
    )
)]
pub struct App {
    retirement: Option<crate::semantic_document_read::retirement::RootRetirementPrelude>,
    #[cfg(all(any(windows, target_os = "linux"), feature = "native"))]
    startup_renderer: Option<native_runner::RendererPreparation>,
    control_domain: crate::ControlDomain,
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
    theme_store_stamp: Option<settings_store::SettingsStamp>,
    last_theme_store_check: Instant,
    /// The schedule an "Automatic" theme follows and whether it was night
    /// when last applied: sunrise and sunset re-theme without a file change.
    theme_schedule: Option<(settings_store::day_schedule::Scheduled, bool)>,
    input: InputRuntime,

    focus_history: HistoryRuntime,

    collaboration: CollaborationRuntime,
    native_room_route: Option<crate::collaboration::RouteSpec>,
    pub presence_ctx: PresenceCtx,
    pub permission_ctx: PermissionContext,

    presence: PresenceRuntime,

    accessibility: AccessibilityRuntime,

    /// Compared against [`crate::widget::Widget::fullscreen_intent`] each
    /// frame so a steady desired state issues one `set_fullscreen` call —
    /// see [`fullscreen_request_decision`].
    window: WindowRuntime,
    committed_documents: Vec<(
        crate::collaboration::RouteSpec,
        crate::collaboration::CommittedDocument,
    )>,
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
    /// A failed pre-terminal handoff must not re-enter directed effects.
    action_publication_failure: Option<String>,
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
    Taken,
}
impl<T> ConstructionValue<T> {
    fn resolve_in_place(&mut self) -> T {
        std::mem::replace(self, Self::Taken).resolve()
    }

    #[expect(
        clippy::unreachable,
        reason = "each construction value is resolved exactly once during App assembly"
    )]
    fn resolve(self) -> T {
        match self {
            Self::Lazy(produce) => produce(),
            Self::Ready(value) => value,
            Self::Taken => unreachable!("construction value already resolved"),
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
    control_domain: Option<crate::ControlDomain>,
    collab_manager: CollaborationManager,
    configured_theme: ConstructionValue<Theme>,
    text_renderer: ConstructionValue<TextRenderer>,
    palette_source: PaletteSource,
    watch_system_settings: bool,
    debug_mode: fn() -> bool,
    theme_store_stamp: fn() -> Option<settings_store::SettingsStamp>,
    local_author: ConstructionValue<crate::permissions::UserId>,
    presence_ctx: ConstructionValue<PresenceCtx>,
    screen_reader: fn() -> Option<crate::screen_reader::ScreenReader>,
    /// The person's colour before a host says it: the stored choice where
    /// this process may read the person's settings, `None` elsewhere.
    your_colour: fn() -> Option<crate::tokens::PresenceHue>,
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
            Self::production(
                crate::collaboration::native_initial::take_manager()
                    .unwrap_or_else(|| CollaborationManager::for_record(&binding.checkout)),
            ),
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
            control_domain: None,
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
            your_colour: || None,
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
            control_domain: None,
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
            theme_store_stamp: settings_store::stamp,
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
            your_colour: App::stored_your_colour,
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
            control_domain: None,
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
            your_colour: || None,
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

    /// Mount a checked component on an explicitly admitted collaboration manager.
    /// The manager retains the certified SDK route; no ambient identity or service
    /// lookup is added. Component authority and record custody remain unchanged.
    pub fn from_component_with_collaboration(
        component: crate::application::AppComponent,
        inputs: crate::application::ComponentRunnerInputs,
        collaboration: CollaborationManager,
    ) -> Result<Self, crate::launch::LaunchError> {
        Self::from_component_using(component, inputs, |inputs, binding| {
            let mut construction = AppConstructionInputs::component(inputs, binding);
            construction.collab_manager = collaboration;
            construction
        })
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
        construction: impl FnOnce(
            crate::application::ComponentRunnerInputs,
            &crate::bound_dispatch::CallbackBinding,
        ) -> AppConstructionInputs,
    ) -> Result<Self, crate::launch::LaunchError> {
        let scope =
            crate::collaboration::native_initial::take_component_scope().unwrap_or_default();
        let retirement = component.take_root_retirement();
        let mut assembly = retirement_assembly::ComponentAssemblyCustody {
            component: None,
            scope: None,
            retirement,
        };
        let scope = assembly.scope.insert(scope);
        let component = assembly.component.insert(component);
        let _entry = scope.enter();
        let handle = scope.handle();
        if let Err(error) = component.mount(move |_| handle.wake()) {
            return Err(component.setup_error(error));
        }
        let binding = component.callback_binding();
        let inputs = construction(inputs, &binding);
        // The manager may still be connecting; the route it announced is the
        // only one the broker can acknowledge, so check that one now.
        if let Some(route) = inputs.collab_manager.requested_primary_route()
            && !binding.matches_route(route)
        {
            return Err(component.setup_error(crate::launch::LaunchError::BindingMismatch));
        }
        let history = component.history.take();
        let behaviour = component.behaviour;
        let input_surface_id = (behaviour.window == crate::application::WindowRole::InputSurface)
            .then(|| component.app_id().to_owned());
        let Some(component) = assembly.component.take() else {
            return Err(crate::launch::LaunchError::Operation(
                "component assembly no longer owns its component".to_owned(),
            ));
        };
        let mut root = Box::new(crate::widget::BoundAppWidget::new(component));
        let mut root_custody = retirement_assembly::AppAssemblyCustody {
            retirement: assembly.retirement.take(),
            root: None,
            wake: assembly.scope.take(),
            state: Some(State::new()),
            control: None,
            inputs: Some(inputs),
            configured_theme: None,
            screen_reader: None,
            values: Default::default(),
        };
        // Validation can call app code; keep a negative guard declared after the
        // concrete root so unwind retires before that root's destructor.
        let validation = retirement_assembly::ValidationRetirement(&root_custody.retirement, true);
        if let Some(registration) = &history
            && let Err(error) = registration.validate_root(root.as_ref())
        {
            return Err(root
                .component
                .setup_error(crate::launch::LaunchError::Operation(error.to_string())));
        }
        validation.disarm();
        root_custody.root = Some(root);
        let mut app = retirement_assembly::assemble(root_custody);
        app.autofocus_on_structure_change = behaviour.autofocus_on_structure_change;
        app.appearance_reconcile = behaviour.appearance_reconcile;
        app.set_capture_protected(behaviour.capture_protected);
        if let Some(app_id) = input_surface_id {
            app.window.app_id = Some(app_id);
        }
        if let Some(registration) = history
            && let Err(error) = app.install_history(registration)
        {
            app.close_components();
            return Err(crate::launch::LaunchError::ComponentSetup {
                source: Box::new(crate::launch::LaunchError::Operation(error.to_string())),
                record: Box::new(binding.checkout),
                recovery: crate::launch::LaunchRecovery::new(
                    Some(binding.records),
                    app.take_component_close_reports(),
                ),
            });
        }
        if behaviour.window == crate::application::WindowRole::InputSurface {
            app = app.without_native_document_chrome();
        }
        Ok(app)
    }

    /// Current SDK transport health, including fail-closed publication errors.
    pub fn collaboration_is_connected(&self) -> bool {
        self.collaboration.manager.is_connected()
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
        text_renderer.apply_typography(&theme.tokens.typography);
        crate::application::ComponentRunnerInputs {
            theme,
            text_renderer,
        }
    }

    /// Capture/close embedded components while the host still owns their trees.
    /// Recovery reports stay owned by this host until explicitly transferred.
    pub fn close_components(&mut self) {
        self.retire_semantic_root();
        self.control_domain.close();
        self.actions.bound_record_sync = None;
        self.component_close_reports
            .extend(crate::component_lifecycle::close_components(
                self.root_widget.as_mut(),
            ));
        self.component_close_reports.extend(
            crate::component_lifecycle::take_component_close_reports(self.root_widget.as_mut()),
        );
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
        self.component_close_reports.extend(
            crate::component_lifecycle::take_component_close_reports(self.root_widget.as_mut()),
        );
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
                action,
                &mut self.state,
                crate::bound_dispatch::SemanticCaller {
                    authority: &self.permission_ctx,
                    author: &author,
                },
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

    /// Where a [`Widget::take_focus_reset`] puts focus: the control the new
    /// content marks as its default (so Enter takes the encouraged action),
    /// otherwise the first focusable.
    fn reset_focus_target(&self) -> Option<taffy::NodeId> {
        self.focusable_order
            .iter()
            .copied()
            .find(|&id| {
                self.inspect_node(id, |widget| widget.is_default_keyboard_focus())
                    .unwrap_or(false)
            })
            .or_else(|| self.focusable_order.first().copied())
    }

    /// Apply the native host's fallback for an Enter/Space press that the
    /// focused widget did not consume during normal event dispatch.
    fn initial_live_focus_target(&self) -> Option<taffy::NodeId> {
        self.focusable_order
            .iter()
            .copied()
            .find(|&id| {
                self.inspect_node(id, |widget| widget.name() == "Terminal")
                    .unwrap_or(false)
            })
            .or_else(|| self.focusable_order.first().copied())
    }

    fn handle_unconsumed_activation_key(&mut self, event: &WindowEvent, handled: &mut bool) {
        if *handled
            || self.input.modifiers.state().super_key()
            || self.input.modifiers.state().shift_key()
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
        if self.root_widget.name() == "Terminal"
            && self
                .history
                .as_ref()
                .is_none_or(|history| !history.captures_input())
            && let Some(root) = self.layout.root_node
        {
            let mut focus = None;
            self.root_widget.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &self.input.modifiers,
                    state: crate::widget::Edits::lend(&mut self.state.doc),
                    path: &self.state.root,
                    handled,
                    focus_target: &mut focus,
                    node_id: root,
                    taffy: &self.layout.taffy,
                },
            );
            if *handled {
                self.needs_sync = true;
                self.layout.layout_dirty = true;
                return;
            }
        }
        let Some(focused) = self.focused_node else {
            return;
        };
        let activated = self
            .dispatch_node(focused, |widget, doc, root| {
                widget.perform_action(
                    &crate::a11y::Action::Activate,
                    &mut crate::widget::Edits::lend(doc),
                    root,
                )
            })
            .unwrap_or(false);
        if activated {
            *handled = true;
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
    }

    /// The desktop's tab chords (Ctrl+T, Ctrl+Shift+T, Ctrl+W; Cmd on macOS)
    /// when no widget consumed the press: tabs are the shell's, so every app
    /// answers them the same way through [`crate::shell_tabs`].
    #[cfg(feature = "native")]
    fn handle_unconsumed_tab_chord(&self, event: &WindowEvent, handled: &mut bool) {
        if *handled {
            return;
        }
        let WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key,
                    state: winit::event::ElementState::Pressed,
                    repeat: false,
                    ..
                },
            ..
        } = event
        else {
            return;
        };
        let modifiers = self.input.modifiers.state();
        let command = self.input.platform.command_modifier_down(modifiers);
        // Ctrl and Super together are a different chord on every platform.
        let other = modifiers.alt_key() || (modifiers.control_key() && modifiers.super_key());
        if let Some(request) =
            crate::shell_tabs::chord_request(logical_key, command, modifiers.shift_key(), other)
        {
            crate::shell_tabs::request(request);
            *handled = true;
        }
    }

    /// Let the focused widget see directional input first. Only an unconsumed
    /// arrow may move host focus, using the same modal/focus-trap fence as Tab.
    #[cfg(feature = "native")]
    fn handle_unconsumed_directional_key(
        &mut self,
        event: &WindowEvent,
        modal_rect: Option<KurboRect>,
        require_alt: bool,
        handled: &mut bool,
    ) {
        if *handled
            || (require_alt && !self.input.modifiers.state().alt_key())
            || !self.keyboard_nav_allowed(modal_rect)
        {
            return;
        }
        let WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Named(named),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return;
        };
        let Some(direction) = Direction::from_key(*named) else {
            return;
        };
        self.navigate_focus(direction);
        *handled = true;
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
        Self::from_managed_root_with_constructor(state, root, Self::new)
    }

    /// Explicit fixture inputs, through the same validation and installation
    /// boundary as the public managed constructor.
    /// # Errors
    /// Refuses invalid or mismatched managed owner registrations.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_managed_root_with_controlled_inputs(
        state: State,
        root: crate::history_mode::ManagedRoot,
    ) -> Result<Self, crate::history_mode::HistoryRegistrationError> {
        Self::from_managed_root_with_constructor(state, root, |state, widget| {
            let _entry = crate::wake::WakeScope::enter_detached();
            Self::with_parts(None, state, widget, AppConstructionInputs::controlled())
        })
    }

    fn from_managed_root_with_constructor(
        state: State,
        root: crate::history_mode::ManagedRoot,
        construct: impl FnOnce(State, Box<dyn Widget>) -> Self,
    ) -> Result<Self, crate::history_mode::HistoryRegistrationError> {
        // Reject custody before evaluating any production or fixture inputs.
        root.history.validate_root(root.widget.as_ref())?;
        let mut app = construct(state, root.widget);
        app.install_history(root.history)?;
        Ok(app)
    }

    /// Registered producer inventory; legacy private custody remains unqualified.
    pub fn control_coverage(&self) -> crate::ControlCoverage {
        self.control_domain.coverage()
    }

    pub(crate) fn install_history(
        &mut self,
        registration: crate::history_mode::HistoryRegistration,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        let _entry = self.enter_wake_scope();
        self.root_widget.sync(&self.state.doc, &self.state.root);
        registration.validate_root(self.root_widget.as_ref())?;
        #[cfg(any(test, feature = "test-support"))]
        let controlled_definition = registration.definition;
        let host = tilo_startup_metrics::span("history-install", || {
            crate::history_mode::host::HistoryHost::new(registration, &self.state.doc)
        })?;
        self.history = Some(Box::new(host));
        if let Some(history) = &mut self.history {
            history.set_control_domain(&self.control_domain);
        }
        #[cfg(any(test, feature = "test-support"))]
        {
            self.controlled_installed_history_definition = Some(controlled_definition);
        }
        self.init_layout();
        tilo_startup_metrics::span("history-validate", || self.validate_history_registration())
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
        let domain = crate::control_custody::ControlDomain::outer();
        let root = factory();
        Self::from_scoped_parts_with_inputs(scope, state, root, || {
            let mut inputs = inputs();
            inputs.control_domain = Some(domain);
            inputs
        })
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
        let domain = crate::control_custody::ControlDomain::outer();
        let root = factory()?;
        Ok(Self::from_scoped_parts_with_custody(
            scope, state, root, domain,
        ))
    }

    pub(crate) fn from_scoped_parts_with_custody(
        scope: crate::WakeScope,
        state: State,
        root: Box<dyn Widget>,
        domain: crate::ControlDomain,
    ) -> Self {
        let manager = crate::collaboration::native_initial::take_manager().unwrap_or_default();
        let mut inputs = AppConstructionInputs::production(manager);
        inputs.control_domain = Some(domain);
        Self::with_parts(Some(scope), state, root, inputs)
    }

    #[cfg(test)]
    pub(crate) fn from_scoped_parts_with_controlled_custody(
        scope: crate::WakeScope,
        state: State,
        root: Box<dyn Widget>,
        domain: crate::ControlDomain,
    ) -> Self {
        let mut inputs = AppConstructionInputs::controlled();
        inputs.control_domain = Some(domain);
        Self::with_parts(Some(scope), state, root, inputs)
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
            // An ephemeral identity wears the default colour, not the
            // person's.
            inputs.your_colour = || None;
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

    #[cfg(test)]
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

    fn retire_semantic_root(&self) {
        if let Some(retirement) = &self.retirement {
            retirement.retire();
        }
    }

    fn close_wake_scope(&mut self) {
        self.retire_semantic_root();
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
        retirement_assembly::assemble(retirement_assembly::AppAssemblyCustody {
            retirement: None,
            root: Some(root_widget),
            wake: wake_scope,
            state: Some(state),
            control: None,
            inputs: Some(inputs),
            configured_theme: None,
            screen_reader: None,
            values: Default::default(),
        })
    }

    fn rebuild_theme(&mut self) {
        self.theme = match self.palette_source {
            PaletteSource::Host => {
                self.text_renderer
                    .set_text_scale(self.accessibility.text_scale);
                self.accessibility.text_scale = self.text_renderer.text_scale();
                let base = self
                    .accessibility
                    .base_theme
                    .clone()
                    .in_window(&self.accessibility.window);
                let base = if self.accessibility.a11y.high_contrast {
                    base.with_high_contrast()
                } else {
                    base
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
            PaletteSource::Owned { .. } => self
                .accessibility
                .base_theme
                .clone()
                .in_window(&self.accessibility.window)
                .with_a11y(
                    &self.accessibility.a11y,
                    self.accessibility.text_scale,
                    self.accessibility.ui_scale,
                ),
        };
        self.text_renderer
            .apply_typography(&self.theme.tokens.typography);
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
        tilo_startup_metrics::startup_mark("actions-declared-done");
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
            semantic_identity: resource.semantic_identity.clone(),
            address: tilo_actions::ActionAddress::new(resource.id.as_str(), &action.id),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            action_label: action.label.clone(),
            description: action.description.clone(),
            locus: action.locus,
            kind: action.kind,
            contract: action.contract.clone(),
            input: match &action.input {
                tilo_actions::ActionInput::Immediate(_)
                | tilo_actions::ActionInput::Deferred { .. } => tilo_actions::InputSpec::None,
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
            view: tilo_actions::ResourceViewDescriptor::declared_for(resource, &action.id),
        }
    }
}

impl App {
    /// Publish private source descriptors over the authenticated local app
    /// broker. Public cross-device state is derived by the shell and remains
    /// path/URL-free; this report never enters the room document.
    fn publish_playback_handoff(&mut self) {
        let Some(route) = self.private_continuity_route() else {
            return;
        };
        let now = crate::time::now();
        if self
            .last_playback_publish
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(500))
        {
            return;
        }
        let app_id = route.app_id;
        let workspace_id = route.cluster_id.to_string();
        let reports = media::handoff::playback_reports(&app_id, &workspace_id);
        if self.last_published_playback.as_ref() == Some(&reports) {
            return;
        }
        if reports.is_empty() && self.last_published_playback.is_none() {
            self.last_published_playback = Some(reports);
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

    fn private_continuity_route(&mut self) -> Option<crate::collaboration::RouteSpec> {
        let witness = self.collaboration.manager.primary_route()?.clone();
        let instance = self.collaboration.manager.private_window_instance()?;
        if self.continuation.last_private_route.as_ref() != Some(&witness)
            || self.continuation.private_instance != Some(instance)
        {
            if self
                .continuation
                .last_private_route
                .as_ref()
                .is_some_and(|last| last.route != witness.route)
                || self.continuation.private_instance != Some(instance)
            {
                self.continuation.owner_view = Default::default();
            } else {
                self.continuation.owner_view.pending = None;
                self.continuation.owner_view.last_generation = 0;
            }
            self.continuation.last_published = None;
            self.continuation.publication_initialized = false;
            self.last_published_playback = None;
            self.last_playback_publish = None;
            self.continuation.last_private_route = Some(witness.clone());
            self.continuation.private_instance = Some(instance);
        }
        Some(witness.route)
    }
}

/// Reduce a winit monitor to what output selection ranks on. Whether a
/// display is built in comes from its connector name (`eDP-1`), which
/// Wayland compositors advertise as the `wl_output` name; its physical size
/// from `wl_output.geometry`. Platforms that expose neither rank on pixels.
#[cfg(feature = "native")]
pub(crate) fn monitor_info(
    index: usize,
    monitor: &winit::monitor::MonitorHandle,
    primary: bool,
) -> crate::fullscreen::MonitorInfo {
    #[cfg(target_os = "linux")]
    let physical_mm = {
        use winit::platform::wayland::MonitorHandleExtWayland;
        monitor.physical_size_mm()
    };
    #[cfg(not(target_os = "linux"))]
    let physical_mm = None;
    crate::fullscreen::MonitorInfo {
        index: u32::try_from(index).unwrap_or(u32::MAX),
        width: monitor.size().width,
        height: monitor.size().height,
        primary,
        scale: monitor.scale_factor(),
        builtin: monitor
            .name()
            .is_some_and(|name| crate::fullscreen::connector_is_builtin(&name)),
        physical_mm,
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
                    .map(|(i, m)| monitor_info(i, m, primary.as_ref() == Some(m)))
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
        self.apply_appearance_intent(intent)
    }

    /// The person's stored colour (`identity.colour`) in the settings
    /// document this process reads, if they chose one. The shell's
    /// appearance message replaces it with the colour it resolved.
    fn stored_your_colour() -> Option<crate::tokens::PresenceHue> {
        crate::tokens::PresenceHue::chosen_in(&serde_json::Value::Object(settings_store::load()))
    }

    /// Where and for whom this window is shown, as the shell said it over
    /// the shell-ui channel: the workspace's temperature and the person's
    /// colour. A window the shell never told stays neutral and keeps the
    /// colour it started with. Returns whether the theme changed.
    pub fn set_window_appearance(&mut self, appearance: crate::theme::WindowAppearance) -> bool {
        if appearance == self.accessibility.window {
            return false;
        }
        self.accessibility.window = appearance;
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.structure_dirty = true;
        true
    }

    /// What the host said about where and for whom this window is shown.
    #[must_use]
    pub fn window_appearance(&self) -> crate::theme::WindowAppearance {
        self.accessibility.window
    }

    /// What a standalone app read from its host OS, through the same door as
    /// a settings pane's intent. The caller applies each host change once, so
    /// a contrast toggle made in the app holds until the host changes again.
    #[cfg(feature = "native")]
    pub(crate) fn apply_host_appearance(
        &mut self,
        host: crate::host_appearance::HostAppearance,
    ) -> bool {
        if crate::host_appearance::session_document_owns_appearance() {
            return false;
        }
        let _entry = self.enter_wake_scope();
        let mut base_theme = crate::host_appearance::theme(host);
        base_theme.caret_width = self.accessibility.base_theme.caret_width;
        self.apply_appearance_intent(crate::theme::AppearanceIntent {
            base_theme,
            high_contrast: host.high_contrast,
            reduce_motion: host.reduce_motion,
            text_scale: self.accessibility.text_scale,
            ui_scale: self.accessibility.ui_scale,
        })
    }

    #[cfg(feature = "native")]
    fn apply_appearance_intent(&mut self, intent: crate::theme::AppearanceIntent) -> bool {
        let mut changed = false;
        let base = &self.accessibility.base_theme;
        // Same name, other temperature or other person: still a new theme.
        if intent.base_theme.name != base.name
            || intent.base_theme.tokens.colors.surfaces.content
                != base.tokens.colors.surfaces.content
            || intent.base_theme.border_focus != base.border_focus
        {
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
        let now = crate::time::now();
        if self.watch_system_settings
            && now.saturating_duration_since(self.last_theme_store_check)
                >= Duration::from_millis(750)
            && self.theme_schedule.is_some_and(|(schedule, night)| {
                schedule.is_night(crate::theme::local_moment()) != night
            })
        {
            self.last_theme_store_check = now;
            self.apply_configured_theme();
            self.rebuild_theme();
            self.layout.layout_dirty = true;
            self.layout.structure_dirty = true;
            return true;
        }
        self.reconcile_configured_theme_with(
            crate::time::now(),
            settings_store::stamp,
            Self::apply_configured_theme,
        )
    }

    #[cfg(feature = "native")]
    fn reconcile_configured_theme_with(
        &mut self,
        now: Instant,
        stamp: impl FnOnce() -> Option<settings_store::SettingsStamp>,
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
        self.accessibility.base_theme = self
            .floating
            .host_theme
            .clone()
            .unwrap_or_else(Theme::configured);
        self.theme_schedule =
            Theme::automatic_schedule_from(settings_store::settings_path().as_deref())
                .map(|schedule| (schedule, schedule.is_night(crate::theme::local_moment())));
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

    /// Whether a laid-out widget claims raw per-finger touch at `point`
    /// (logical coords); see [`Widget::claims_raw_touch`].
    #[cfg(feature = "native")]
    fn claims_raw_touch_at(&self, point: Point) -> bool {
        self.layout.node_map.keys().any(|&node| {
            self.inspect_node(node, |widget| {
                widget.get_rect().contains(point) && widget.claims_raw_touch(point)
            })
            .unwrap_or(false)
        })
    }

    /// Mirrors [`Self::resolve_cursor`]: smallest-area widget under `point`
    /// exposing a non-empty [`Widget::tooltip`], so a nested icon button
    /// beats its enclosing toolbar.
    #[cfg(feature = "native")]
    fn tooltip_target_at(&self, point: Point) -> Option<(vello::kurbo::Rect, String)> {
        self.tooltip_placed_at(point)
            .map(|(rect, label, _)| (rect, label))
    }

    /// [`Self::tooltip_target_at`] with the side of its control the
    /// tooltip prefers.
    #[cfg(feature = "native")]
    pub(crate) fn tooltip_placed_at(
        &self,
        point: Point,
    ) -> Option<(
        vello::kurbo::Rect,
        String,
        crate::widget::tooltip::TooltipSide,
    )> {
        if !self.input.interaction.supports_hover() {
            return None;
        }
        let mut best: Option<(
            f64,
            vello::kurbo::Rect,
            String,
            crate::widget::tooltip::TooltipSide,
        )> = None;
        for &node in self.layout.node_map.keys() {
            let Ok(target) = self.inspect_node(node, |widget| {
                widget
                    .tooltip_target_at(point)
                    .map(|(rect, tip)| (rect, tip, widget.tooltip_side()))
            }) else {
                continue;
            };
            if let Some((rect, tip, side)) = target {
                let area = (rect.width() * rect.height()).max(0.0);
                if best.as_ref().is_none_or(|(a, _, _, _)| area < *a) {
                    best = Some((area, rect, tip, side));
                }
            }
        }
        best.map(|(_, rect, label, side)| (rect, label, side))
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

    /// Replace the framework view inside the windows this process already has.
    ///
    /// Returns whether the running root accepted it. A root that is not the
    /// framework host leaves the current view in place.
    #[cfg(feature = "native")]
    pub(crate) fn install_reloaded_view(&mut self, root: DevRoot) -> bool {
        let Some(bound) = self
            .root_widget
            .as_any_mut()
            .and_then(|widget| widget.downcast_mut::<crate::widget::BoundAppWidget>())
        else {
            return false;
        };
        bound.component.install_reloaded(root);
        self.needs_sync = true;
        true
    }

    /// Lets headless tests stand the App up without spinning a winit loop.
    #[doc(hidden)]
    pub fn init_layout(&mut self) {
        let dev_view = self.consume_dev_checkpoint();
        self.init_layout_with_dev_view(dev_view);
    }

    /// Headless and dev-reload startup. `dev_view` is the presentation captured
    /// from the previous process; shell continuation still wins when both exist.
    pub(crate) fn init_layout_with_dev_view(&mut self, dev_view: Option<crate::view::View>) {
        let _entry = self.enter_wake_scope();
        // Startup marks name each step, so a slow first layout says which one.
        tilo_startup_metrics::span("init-layout-actions", || {
            self.refresh_control_action_availability();
        });
        self.component_close_reports.extend(
            crate::component_lifecycle::take_component_close_reports(self.root_widget.as_mut()),
        );
        self.bindings.clear();
        self.layout.node_map.clear();
        // The map built now describes the tree as it is: a replacement the
        // widgets reported before is part of it, not a change after it.
        self.take_composition_dirty();
        tilo_startup_metrics::span("init-layout-history", || self.build_history_presentation());
        tilo_startup_metrics::span("init-layout-focus-order", || {
            self.update_focusable_order();
        });
        if let Some(view) = self.continuation.pending_view.take() {
            <Self as crate::view::FreezeThaw>::restore_view(self, &view);
        } else if let Some(view) = dev_view {
            <Self as crate::view::FreezeThaw>::restore_view(self, &view);
        }
        if self.autofocus_first && self.focused_node.is_none() {
            if let Some(id) = self.initial_live_focus_target() {
                self.set_focused_node(Some(id));
            }
        }
        self.focus_history.stable_focus_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.focus_history.stable_metadata = self.focused_node.and_then(|id| {
            self.inspect_node(id, |widget| crate::undo::get_state(widget))
                .ok()
                .flatten()
        });
    }

    /// Exposed so headless tests can drive modal open/close without a winit
    /// loop.
    #[doc(hidden)]
    pub fn refresh_focus_order(&mut self) {
        let _entry = self.enter_wake_scope();
        self.sync_model(true);
        self.update_focusable_order();
    }

    /// The root's structure flag, and framework History's: it is composed
    /// as a separate root, so its replaced controls are not the app's.
    pub(crate) fn take_composition_dirty(&mut self) -> bool {
        let root = self.root_widget.take_structure_dirty();
        let history = self
            .history
            .as_mut()
            .is_some_and(|history| history.take_structure_dirty());
        root || history
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
        // Always take the widgets' report, so it is not mistaken later for a
        // change after the map this rebuild makes.
        let reported = self.take_composition_dirty();
        let topology_changed = self.layout.structure_dirty || reported;
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
                    self.inspect_node(id, |widget| crate::undo::get_state(widget))
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
        self.component_close_reports.extend(
            crate::component_lifecycle::take_component_close_reports(self.root_widget.as_mut()),
        );
        self.bindings.clear();
        self.layout.node_map.clear();
        self.build_history_presentation();
        self.layout.structure_dirty = false;
        self.layout.layout_dirty = true;
        self.update_focusable_order();

        // A focus trap already placed focus inside its subtree in
        // `update_focusable_order`; don't let it escape the modal.
        if self.autofocus_on_structure_change
            && !self.accessibility.focus_trap_active
            && self.history.is_none()
            && !crate::history_mode::embedded::has_surfaces(self.root_widget.as_ref())
        {
            if focus_reset {
                if let Some(target) = self.reset_focus_target() {
                    self.set_focused_node(Some(target));
                }
            } else if let Some(idx) = prev_focus_idx {
                let exact = self.focusable_order.get(idx).copied();
                let target = exact.or_else(|| self.focusable_order.last().copied());
                if let Some(id) = target {
                    self.set_focused_node(Some(id));
                    if exact.is_some()
                        && let Some(state) = prev_focus_state.as_deref()
                    {
                        let _ =
                            self.mutate_node(id, |widget| crate::undo::set_state(widget, state));
                    }
                }
            }
        }

        // The widgets built above are fresh instances the OLD `sync` never
        // ran against, so without this they draw blank until the next event.
        self.root_widget.sync(&self.state.doc, &self.state.root);
        self.needs_sync = false;
        if self.root_widget.take_structure_dirty() {
            // Fresh sync can replace delegates. Rebuild before any map access.
            self.layout.structure_dirty = true;
            self.bindings.clear();
            self.layout.node_map.clear();
        } else if self.focused_node.is_none() {
            // A retained control can keep focus while a modal temporarily owns
            // input. Rebuilding invalidates its NodeId, not that live focus.
            // Recover only the unique current focused path inside the current
            // allowed order; never transfer focus or state to a replacement.
            // SAFETY: sync retained this exact exclusively owned topology.
            self.focused_node = unsafe {
                crate::widget::layout::CompositionFocus::current(
                    &self.layout.node_map,
                    &self.bindings,
                    Some(&self.focusable_order),
                )
            };
        }
        self.invalidate_widget_resource_cache();
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

    /// The Wayland application id the window announces when it is not the
    /// process's own package id: an input surface's, or one set with
    /// [`Self::with_window_app_id`].
    pub fn window_app_id(&self) -> Option<&str> {
        self.window.app_id.as_deref()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.retire_semantic_root();
        self.control_domain.close();
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
            device_id: winit::event::DeviceId::dummy(),
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
    fn floating_activity_pumps_and_proxies_until_the_exact_primary_frame() {
        // Case `primary_surface_pumps_only_until_its_first_presented_frame`.
        {
            assert!(!primary_startup_pump_needed(0, 0));
            assert!(primary_startup_pump_needed(1, 0));
            assert!(primary_startup_pump_needed(3, 0));
            assert!(!primary_startup_pump_needed(3, 1));
        }

        // Case `generic_pip_activity_edge_dismisses_without_touching_native_surfaces`.
        {
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

        // Case `native_float_allocation_follows_the_complete_soft_chrome`.
        {
            let compact = native_floating_logical_size(Rect::new(80.0, 40.0, 456.0, 292.0));
            let expanded = native_floating_logical_size(Rect::new(80.0, 40.0, 456.0, 352.0));
            assert_eq!((compact.width, compact.height), (376.0, 252.0));
            assert_eq!((expanded.width, expanded.height), (376.0, 312.0));
        }

        // Case `ownership_proxy_waits_for_the_exact_primary_frame_to_present`.
        {
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
}

/// See [`resolve_title_from`] for the precedence.
#[cfg(feature = "native")]
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
#[cfg(feature = "native")]
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

#[cfg(feature = "native")]
fn humanize_ident(ident: &str) -> String {
    ident
        .split(|c: char| c == '-' || c == '_' || c == '.' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .map(crate::format::capitalize_first)
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
pub(crate) mod framework_history_tests;

#[cfg(test)]
#[path = "app/person_record_tests.rs"]
mod person_record_tests;

#[path = "app/history.rs"]
mod framework_history;

mod committed_document;

#[path = "app/document_routes.rs"]
mod document_routes;

#[cfg(test)]
#[path = "app/focus_selection_tests.rs"]
mod focus_selection_tests;

#[cfg(test)]
#[cfg(feature = "local-daemons")]
#[path = "app/local_privacy_tests.rs"]
mod local_privacy_tests;

#[cfg(test)]
#[path = "app/participant_invocation_tests.rs"]
mod participant_invocation_tests;

#[cfg(test)]
#[path = "app/large_document_tests.rs"]
mod large_document_tests;

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
    use std::collections::HashMap;

    struct OwnerProbe {
        label: &'static str,
        report: Option<crate::application::ComponentCloseReport>,
    }

    impl Widget for OwnerProbe {
        crate::provides!(dyn crate::component_lifecycle::ComponentLifecycle);

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
            doc: &mut crate::widget::Edits<'_>,
            path: &crate::widget::ModelPath,
        ) -> bool {
            let (doc, _) = crate::doc::document(doc, path);
            if target != "owner:named" {
                return false;
            }
            match action {
                crate::a11y::Action::Activate => {
                    doc.document_mut()
                        .put(ROOT, "selected", format!("{} named", self.label))
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
            doc: &mut crate::widget::Edits<'_>,
            path: &crate::widget::ModelPath,
        ) -> bool {
            let (doc, _) = crate::doc::document(doc, path);
            if keys != [7] || !matches!(action, crate::a11y::Action::Activate) {
                return false;
            }
            doc.document_mut()
                .put(ROOT, "selected", format!("{} keyed", self.label))
                .unwrap();
            true
        }
    }

    impl crate::component_lifecycle::ComponentLifecycle for OwnerProbe {
        fn take_component_close_reports(
            &mut self,
        ) -> Vec<crate::application::ComponentCloseReport> {
            self.report.take().into_iter().collect()
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
            })));
        tile.bind_record(store.clone(), checkout.clone());
        tile
    }

    #[test]
    fn legacy_owner_routing_resolves_current_aliases_and_rejects_drained_errors() {
        // Case `semantic_targets_resolve_current_legacy_aliases_across_disjoint_roots`.
        {
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
    }
}

#[cfg(test)]
#[path = "app/legacy_resource_tests.rs"]
mod legacy_resource_tests;

#[cfg(test)]
mod private_continuity_route_tests {
    use super::*;
    use crate::collaboration::{CollabEvent, CollabRequest, PeerTransport, RouteSpec};
    use std::cell::RefCell;
    use std::rc::Rc;

    struct AcknowledgingPeer {
        connected: bool,
        acknowledged: Option<RouteSpec>,
        requests: Rc<RefCell<Vec<CollabRequest>>>,
    }

    impl PeerTransport for AcknowledgingPeer {
        fn is_connected(&self) -> bool {
            self.connected
        }

        fn send(&mut self, request: CollabRequest) -> Result<(), String> {
            if let CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                ..
            } = &request
            {
                self.acknowledged = Some(RouteSpec {
                    app_id: app_id.clone(),
                    cluster_id: *cluster_id,
                    record: record.clone(),
                });
            }
            self.requests.borrow_mut().push(request);
            Ok(())
        }

        fn poll(&mut self) -> Vec<CollabEvent> {
            Vec::new()
        }

        fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
            self.acknowledged.take().map(Ok)
        }
    }

    fn app_for_acknowledged_route(route: RouteSpec) -> (App, Rc<RefCell<Vec<CollabRequest>>>) {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut manager = CollaborationManager::with_peer_transport(AcknowledgingPeer {
            connected: true,
            acknowledged: None,
            requests: requests.clone(),
        });
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id,
            cluster_id: route.cluster_id,
            record: route.record,
            record_kind: None,
            handle: None,
        }));
        manager.poll();
        requests.borrow_mut().clear();
        let app = App::with_collaboration_manager(
            State::new(),
            Box::new(crate::widget::Text::new("continuity route fixture")),
            manager,
        );
        (app, requests)
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn private_reports_use_each_acknowledged_continuation_route() {
        let previous_app = std::env::var_os("TILO_PACKAGE_ID");
        let previous_room = std::env::var_os("TILO_WORKSPACE_ID");
        let ambient_room = uuid::Uuid::new_v4();
        // SAFETY: guarded by the process_env serial group and restored below.
        unsafe {
            std::env::set_var("TILO_PACKAGE_ID", "ambient.app");
            std::env::set_var("TILO_WORKSPACE_ID", ambient_room.to_string());
        }
        let playback = media::handoff::AppPlaybackHandle::new();
        playback.publish(media::handoff::AppPlayback {
            resource_uri: "music://track/route-fixture",
            title: "Route fixture",
            position_ms: 7,
            duration_ms: Some(30),
            playing: true,
            has_video: false,
            audible: true,
            controller_bus_name: None,
        });

        let routes = [
            RouteSpec::record("dev.tilo.notes", uuid::Uuid::new_v4(), "continuation:7"),
            RouteSpec::record("dev.tilo.browser", uuid::Uuid::new_v4(), "continuation:91"),
        ];
        let observed = routes
            .iter()
            .cloned()
            .map(|route| {
                let (_app, requests) = app_for_acknowledged_route(route);
                requests.borrow().clone()
            })
            .collect::<Vec<_>>();
        drop(playback);

        // Restore before asserting so a failure does not leak process state.
        unsafe {
            match previous_app {
                Some(value) => std::env::set_var("TILO_PACKAGE_ID", value),
                None => std::env::remove_var("TILO_PACKAGE_ID"),
            }
            match previous_room {
                Some(value) => std::env::set_var("TILO_WORKSPACE_ID", value),
                None => std::env::remove_var("TILO_WORKSPACE_ID"),
            }
        }

        for (requests, expected) in observed.iter().zip(routes.iter()) {
            let reported_apps = requests
                .iter()
                .filter_map(|request| match request {
                    CollabRequest::ReportContinuation { app_id, .. }
                    | CollabRequest::ClearContinuation { app_id } => Some(app_id.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(reported_apps, vec![expected.app_id.as_str()]);
            let playback = requests
                .iter()
                .find_map(|request| match request {
                    CollabRequest::ReportPlayback {
                        app_id,
                        reports_json,
                    } => Some((app_id, reports_json)),
                    _ => None,
                })
                .expect("eligible route reports live playback");
            assert_eq!(playback.0, &expected.app_id);
            let reports = serde_json::from_slice::<Vec<media::handoff::PlaybackReport>>(playback.1)
                .expect("playback report JSON");
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].session.app_id, expected.app_id);
            assert_eq!(reports[0].workspace_id, expected.cluster_id.to_string());
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn initial_empty_playback_is_suppressed() {
        let route = RouteSpec::record("dev.tilo.music", uuid::Uuid::new_v4(), "continuation:8");
        let (_app, requests) = app_for_acknowledged_route(route);
        assert!(
            requests
                .borrow()
                .iter()
                .all(|request| !matches!(request, CollabRequest::ReportPlayback { .. }))
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn authenticated_document_route_emits_no_private_continuity_requests() {
        let route = RouteSpec::record("so.tilo.calculator", uuid::Uuid::new_v4(), "tab:0013");
        let (_app, requests) = app_for_acknowledged_route(route);
        assert!(requests.borrow().iter().all(|request| !matches!(
            request,
            CollabRequest::ReportContinuation { .. }
                | CollabRequest::ClearContinuation { .. }
                | CollabRequest::ReportPlayback { .. }
        )));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn route_acknowledgement_does_not_lose_the_first_unchanged_projection() {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let manager = CollaborationManager::with_peer_transport(AcknowledgingPeer {
            connected: true,
            acknowledged: None,
            requests: requests.clone(),
        });
        let mut app = App::with_collaboration_manager(
            State::new(),
            Box::new(crate::widget::Text::new("delayed route fixture")),
            manager,
        );
        assert!(app.continuation.last_published.is_none());
        assert!(app.last_published_playback.is_none());
        assert!(app.last_playback_publish.is_none());

        let route = RouteSpec::record("dev.tilo.notes", uuid::Uuid::new_v4(), "continuation:42");
        assert!(app.collaboration.manager.send(CollabRequest::Hello {
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id,
            record: route.record.clone(),
            record_kind: None,
            handle: None,
        }));
        app.collaboration.manager.poll();
        requests.borrow_mut().clear();
        app.publish_continuation_metadata();
        app.publish_playback_handoff();

        assert!(requests.borrow().iter().any(|request| matches!(
            request,
            CollabRequest::ReportContinuation { app_id, .. } if app_id == &route.app_id
        )));
        assert!(
            requests
                .borrow()
                .iter()
                .all(|request| !matches!(request, CollabRequest::ReportPlayback { .. }))
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_new_acknowledgement_epoch_republishes_unchanged_continuity() {
        let route = RouteSpec::record("dev.tilo.notes", uuid::Uuid::new_v4(), "continuation:42");
        let (mut app, requests) = app_for_acknowledged_route(route.clone());
        requests.borrow_mut().clear();

        assert!(app.collaboration.manager.send(CollabRequest::Hello {
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id,
            record: route.record.clone(),
            record_kind: None,
            handle: None,
        }));
        app.collaboration.manager.poll();
        requests.borrow_mut().clear();
        app.publish_continuation_metadata();

        assert!(requests.borrow().iter().any(|request| matches!(
            request,
            CollabRequest::ReportContinuation { app_id, .. } if app_id == &route.app_id
        )));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn eligible_nonempty_to_empty_playback_publishes_a_clear() {
        let route = RouteSpec::record("dev.tilo.music", uuid::Uuid::new_v4(), "continuation:8");
        let (mut app, requests) = app_for_acknowledged_route(route.clone());
        app.last_published_playback = Some(vec![media::handoff::PlaybackReport {
            session: media::handoff::PlaybackSession {
                id: "dev.tilo.music:media".into(),
                app_id: route.app_id.clone(),
                title: "Previous".into(),
                content_id: format!("blake3:{}", "0".repeat(64)),
                position_ms: 0,
                duration_ms: None,
                playing: false,
                has_video: false,
            },
            workspace_id: route.cluster_id.to_string(),
            source: media::handoff::HandoffSource::AppResource {
                uri: "music://track/previous".into(),
            },
            controller_bus_name: None,
            accepted_handoff_request: None,
        }]);
        app.last_playback_publish = None;
        requests.borrow_mut().clear();

        app.publish_playback_handoff();

        let clears = requests
            .borrow()
            .iter()
            .filter_map(|request| match request {
                CollabRequest::ReportPlayback {
                    app_id,
                    reports_json,
                } => Some((app_id.clone(), reports_json.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(clears.len(), 1);
        assert_eq!(clears[0].0, route.app_id);
        assert_eq!(
            serde_json::from_slice::<Vec<media::handoff::PlaybackReport>>(&clears[0].1).unwrap(),
            Vec::new()
        );
    }
}

#[cfg(test)]
pub(crate) mod ordinary_confirmation_tests {
    include!("app/deferred_invocation_tests.rs");
    use super::*;
    use crate::history_mode::*;
    use std::{cell::RefCell, rc::Rc};
    use winit::keyboard::{Key, NamedKey};
    pub(crate) struct Fixture {
        child: Box<dyn Widget>,
        pub resources: Rc<RefCell<Vec<tilo_actions::Resource>>>,
        pub ledger: Rc<RefCell<Vec<serde_json::Value>>>,
        output: Rc<RefCell<Option<crate::ControlOutbox>>>,
    }
    impl Widget for Fixture {
        crate::provides!(
            dyn crate::widget::controls::ActionControls,
            dyn crate::resources::PublishesResources
        );

        crate::testing::delegate_widget_methods!(
            child: sync,
            build_layout,
            apply_layout,
            draw,
            handle_event,
            get_rect
        );
        fn name(&self) -> &str {
            "Ordinary confirmation fixture"
        }
        fn children(&self) -> Vec<&dyn crate::widget::Widget> {
            vec![self.child.as_ref()]
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn crate::widget::Widget + '_)> {
            vec![self.child.as_mut()]
        }
        fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
            let mut handlers = self.child.resource_operation_handlers();
            handlers.insert("fixture.control".into());
            handlers
        }
        fn start_operation(
            &mut self,
            op: &crate::widget::ResourceOperation<'_>,
            doc: &mut crate::widget::Edits<'_>,
            path: &automerge::ObjId,
        ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
            let Some(request) = op
                .unscoped()
                .filter(|request| request.type_id == "fixture.control")
            else {
                return self.child.start_operation(
                    op,
                    &mut crate::widget::Edits::lend(doc.document_mut()),
                    path,
                );
            };
            self.ledger
                .borrow_mut()
                .push(serde_json::from_slice(request.payload).unwrap());
            Ok(Some(crate::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        }
    }

    impl crate::resources::PublishesResources for Fixture {
        fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
            self.resources.borrow().clone()
        }
    }

    impl crate::widget::controls::ActionControls for Fixture {
        fn install_control_custody(
            &mut self,
            domain: &crate::ControlDomain,
            attachment: crate::control_custody::CustodyAttachment,
        ) {
            crate::widget::controls::install_control_custody(
                self.child.as_mut(),
                domain,
                attachment,
            );
            if self.output.borrow().is_none() {
                *self.output.borrow_mut() =
                    Some(domain.producer(crate::ControlProducerDiscipline::Fifo { limit: 64 }));
            }
        }
    }
    static HISTORY: HistoryDefinition = HistoryDefinition {
        app_id: "ordinary-confirmation-fixture",
        scopes: &[HistoryScopeDefinition {
            key: "document",
            title: "Confirmation fixture",
            presentation: HistoryPresentation::Domain {
                create: crate::history_mode::test_preview,
            },
            content_schema: Some(&crate::state::owned_restore_tests::SCHEMA),
        }],
    };
    fn fixture_widget(
        policy: tilo_actions::ConfirmationPolicy,
        source: std::sync::Arc<std::sync::Mutex<serde_json::Map<String, serde_json::Value>>>,
    ) -> (
        Fixture,
        Rc<RefCell<Vec<serde_json::Value>>>,
        Rc<RefCell<Vec<tilo_actions::Resource>>>,
    ) {
        use tilo_actions::*;
        let mut spec = ActionSpec::typed_operation(
            "run",
            "Run fixture",
            "Payload",
            OperationKind::Effect,
            "fixture.control",
            vec![ActionParam::required("payload", ParamType::Text, "Payload")],
            |args| EncodedAction::encode_operation("fixture.control", &args.to_json()).unwrap(),
        );
        spec.contract.confirmation = policy;
        let resource = Resource::new("ordinary:fixture", "fixture", "Fixture").with_action(spec);
        let binding = ActionBinding::new(&resource, &resource.actions[0], move |()| {
            source.lock().unwrap().clone()
        })
        .unwrap();
        let resources = Rc::new(RefCell::new(vec![resource]));
        let ledger = Rc::new(RefCell::new(vec![]));
        let widget = Fixture {
            child: Box::new(
                crate::widget::Button::new("ordinary-emitter", "Emit request").on_action(binding),
            ),
            resources: resources.clone(),
            ledger: ledger.clone(),
            output: Rc::new(RefCell::new(None)),
        };
        (widget, ledger, resources)
    }
    fn managed(widget: Box<dyn Widget>) -> ManagedRoot {
        let registration = HistoryRegistration::new(
            &HISTORY,
            vec![HistorySourceRegistration::HostDocument {
                scope: "document",
                resource: HistoryResource::new(
                    "ordinary:fixture",
                    "lifetime-confirmation",
                    HISTORY.app_id,
                    crate::RecordId::for_tab(990),
                ),
            }],
        )
        .unwrap();
        ManagedRoot::new(widget, registration)
    }
    fn source() -> std::sync::Arc<std::sync::Mutex<serde_json::Map<String, serde_json::Value>>> {
        std::sync::Arc::new(std::sync::Mutex::new(
            serde_json::json!({"payload":"original"})
                .as_object()
                .unwrap()
                .clone(),
        ))
    }
    pub(crate) fn fixture(
        policy: tilo_actions::ConfirmationPolicy,
    ) -> (
        ManagedRoot,
        Rc<RefCell<Vec<serde_json::Value>>>,
        Rc<RefCell<Vec<tilo_actions::Resource>>>,
    ) {
        let (widget, ledger, resources) = fixture_widget(policy, source());
        (managed(Box::new(widget)), ledger, resources)
    }
    crate::testing::test_app_widget!(Fixture, "Confirmation fixture");
    enum OwnerGuard {
        Component {
            binding: crate::bound_dispatch::CallbackBinding,
            _host: crate::application_test_support::ProbeHost,
        },
        Legacy {
            owner: crate::history_mode::HistoryOwner,
            _directory: tempfile::TempDir,
        },
    }
    impl OwnerGuard {
        fn close(&self) {
            match self {
                Self::Component { binding, .. } => binding.closing.set(true),
                Self::Legacy { owner, .. } => {
                    let crate::history_mode::owner::OwnerKind::Legacy { lifetime, .. } = &owner.0
                    else {
                        unreachable!()
                    };
                    lifetime.upgrade().unwrap().active.set(false);
                }
            }
        }
        fn managed(&self, widget: Box<dyn Widget>) -> ManagedRoot {
            static COMPONENT_HISTORY: HistoryDefinition = HistoryDefinition {
                app_id: crate::application_test_support::APP,
                scopes: HISTORY.scopes,
            };
            let (definition, owner) = match self {
                Self::Component { binding, .. } => {
                    (&COMPONENT_HISTORY, HistoryOwner::component(binding.clone()))
                }
                Self::Legacy { owner, .. } => (&HISTORY, owner.clone()),
            };
            ManagedRoot::new(
                widget,
                HistoryRegistration::new(
                    definition,
                    vec![
                        HistorySourceRegistration::owned("document", definition.app_id, owner)
                            .unwrap(),
                    ],
                )
                .unwrap(),
            )
        }
        fn revoke(&self) {
            if let Self::Component { binding, .. } = self {
                *binding.permissions.borrow_mut() =
                    crate::permissions::PermissionContext::stranger("fixture");
            }
        }
    }
    fn owned(widget: Fixture, component: bool) -> (Box<dyn Widget>, OwnerGuard) {
        use crate::application_test_support::*;
        if component {
            let host = ProbeHost::new();
            let definition = runner_probe_definition();
            let target = crate::application::LaunchTarget::New;
            let mut instance = definition
                .instantiate(
                    host.prepare_with_permissions(definition, 991, &target, runner_permissions())
                        .unwrap(),
                    target,
                    crate::application::RestoreInput::None,
                )
                .unwrap();
            instance.root = Some(Box::new(widget));
            instance.mount(|_| {}).unwrap();
            let binding = instance.callback_binding();
            (
                Box::new(crate::widget::BoundAppWidget::new(instance)),
                OwnerGuard::Component {
                    binding,
                    _host: host,
                },
            )
        } else {
            let directory = tempfile::tempdir().unwrap();
            let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
            let store = owners.open(HISTORY.app_id, "legacy").unwrap();
            let checkout = crate::RecordCheckout::new(
                crate::RecordId::for_tab(991),
                crate::RecordKind::new("note"),
            );
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|_| Ok(()))
                .unwrap();
            let mut tile =
                crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(widget)));
            tile.bind_record(store, checkout);
            let owner = tile.history_owner().unwrap();
            (
                Box::new(tile),
                OwnerGuard::Legacy {
                    owner,
                    _directory: directory,
                },
            )
        }
    }
    pub(crate) fn layout(app: &mut App) {
        // Match native redraw: capture the pending structural change before
        // rebuilding clears it; unchanged trees must use the normal fast path.
        app.layout.structure_dirty |= app.layout.root_node.is_none()
            || app.layout.node_map.is_empty()
            || app.root_widget.take_structure_dirty();
        let structure_rebuilt = app.layout.structure_dirty;
        if structure_rebuilt {
            app.rebuild_structure();
        }
        let node = app.layout.root_node.unwrap();
        run_layout_pass(app, node, 900, 650, 1.0, structure_rebuilt, None).unwrap();
        draw_frame_content(app, &mut Scene::new(), 900, 650, 1.0, None);
        complete_native_frame_content(app, |_| {});
    }
    pub(crate) fn named(app: &mut App, label: &str) -> accesskit::NodeId {
        app.build_a11y_tree_update(1.0)
            .nodes
            .into_iter()
            .find(|(_, n)| n.label() == Some(label))
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("missing published {label}"))
    }
    enum Host {
        Native(Box<App>),
        Driver(Box<crate::headless::Driver>),
    }
    impl Host {
        fn new(native: bool, root: ManagedRoot) -> Self {
            if native {
                let mut app =
                    App::from_managed_root_with_controlled_inputs(State::new(), root).unwrap();
                layout(&mut app);
                Self::Native(Box::new(app))
            } else {
                Self::Driver(Box::new(
                    crate::headless::Driver::from_managed_root_with_controlled_inputs(
                        State::new(),
                        root,
                        (900, 650),
                        Theme::dark(),
                        crate::font_catalog::fixture_renderer(),
                        crate::permissions::UserId("fixture".into()),
                    )
                    .unwrap(),
                ))
            }
        }
        fn act(&mut self, label: &str, action: crate::a11y::Action) {
            match self {
                Self::Native(app) => {
                    let target = named(app, label);
                    app.dispatch_a11y_action(target, &action);
                    app.controlled_local_idle_for_test();
                    layout(app);
                }
                Self::Driver(driver) => {
                    if label == "Emit request" {
                        driver
                            .perform_semantic_named("ordinary-emitter", None, &action)
                            .unwrap();
                    } else if label == "History" {
                        let id = driver.a11y_node_named(label).unwrap();
                        driver.perform_a11y_action(id, &action).unwrap();
                    } else {
                        let target = Self::published_target(driver, label);
                        driver
                            .perform_semantic_named("action-palette", Some(&target), &action)
                            .unwrap();
                    }
                }
            }
        }
        fn published_target(driver: &crate::headless::Driver, label: &str) -> String {
            fn access(node: &crate::a11y::AccessNode, label: &str) -> Option<String> {
                if node.name.as_deref() == Some(label) {
                    return node.action_target.clone();
                }
                node.children.iter().find_map(|n| access(n, label))
            }
            fn widget(node: &crate::headless::InspectionNode, label: &str) -> Option<String> {
                node.semantics
                    .as_ref()
                    .and_then(|n| access(n, label))
                    .or_else(|| node.children.iter().find_map(|n| widget(n, label)))
            }
            widget(&driver.inspection().root, label)
                .unwrap_or_else(|| panic!("missing inspection target {label}"))
        }
        fn activate(&mut self, label: &str) {
            self.act(label, crate::a11y::Action::Activate);
        }
        fn value(&mut self, value: &str) {
            self.act("Confirmation", crate::a11y::Action::SetValue(value.into()));
        }
        fn frame(&mut self) {
            match self {
                Self::Native(app) => {
                    app.bindings.clear();
                    app.layout.node_map.clear();
                    app.focused_node = None;
                    app.root_widget.sync(&app.state.doc, &app.state.root);
                    layout(app)
                }
                Self::Driver(driver) => driver.relayout(),
            }
        }
        fn key(&mut self, key: &str) {
            let event = crate::synthetic_input::key_input(
                &Key::Character(key.into()),
                ElementState::Pressed,
            );
            match self {
                Self::Native(app) => {
                    assert!(app.action_palette_handle_key(&event));
                    layout(app)
                }
                Self::Driver(driver) => driver.send(&event),
            }
        }
        fn focused_label(&mut self) -> Option<String> {
            match self {
                Self::Native(app) => {
                    let tree = app.build_a11y_tree_update(1.0);
                    tree.nodes
                        .iter()
                        .find(|(id, _)| *id == tree.focus)
                        .and_then(|(_, node)| node.label().map(str::to_owned))
                }
                Self::Driver(driver) => driver
                    .a11y_focus()
                    .and_then(|(_, node)| node.label().map(str::to_owned)),
            }
        }
        fn submit_id(&mut self) -> accesskit::NodeId {
            match self {
                Self::Native(app) => named(app, "Confirm"),
                Self::Driver(driver) => driver.a11y_node_named("Confirm").unwrap(),
            }
        }
        fn stale(&mut self, id: accesskit::NodeId) {
            match self {
                Self::Native(app) => {
                    app.dispatch_a11y_action(id, &crate::a11y::Action::Activate);
                    layout(app);
                }
                Self::Driver(driver) => {
                    assert!(
                        driver
                            .perform_a11y_action(id, &crate::a11y::Action::Activate)
                            .is_err()
                    );
                }
            }
        }
        fn revoke(&mut self) {
            let denied = crate::permissions::PermissionContext::stranger("fixture");
            match self {
                Self::Native(app) => app.set_permission_ctx(denied),
                Self::Driver(driver) => driver.set_permission_ctx(denied),
            }
        }
    }
    #[test]
    fn ordinary_confirmation_native_events_keep_bounded_measured_tree() {
        let (root, ledger, _) = fixture(tilo_actions::ConfirmationPolicy::Confirm);
        let mut host = Host::new(true, root);
        let expected_rect = match &host {
            Host::Native(app) => {
                assert!(
                    !app.root_widget.layout_needs_measure(),
                    "retained Button is clean"
                );
                app.root_widget.get_rect()
            }
            Host::Driver(_) => unreachable!(),
        };
        assert!(expected_rect.width() > 0.0 && expected_rect.height() > 0.0);
        host.activate("Emit request");
        let Host::Native(app) = &mut host else {
            unreachable!()
        };
        let baseline_nodes = app.layout.taffy.total_node_count();
        let mut samples = vec![(baseline_nodes, app.root_widget.get_rect())];
        for index in 0..12 {
            let event = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(300.0 + f64::from(index), 220.0),
            };
            assert!(app.action_palette_handle_event(&event, 1.0));
            layout(app);
            samples.push((
                app.layout.taffy.total_node_count(),
                app.root_widget.get_rect(),
            ));
            let field = named(app, "Confirmation");
            app.dispatch_a11y_action(
                field,
                &crate::a11y::Action::SetValue(format!("draft {index}")),
            );
            app.controlled_local_idle_for_test();
            layout(app);
            samples.push((
                app.layout.taffy.total_node_count(),
                app.root_widget.get_rect(),
            ));
        }
        assert!(ledger.borrow().is_empty());
        let bounded = samples.iter().all(|(nodes, _)| *nodes <= baseline_nodes);
        let measured = samples.iter().all(|(_, rect)| *rect == expected_rect);
        assert!(
            bounded && measured,
            "unchanged-background events must retain bounded solved geometry: initial nodes={baseline_nodes}, rect={expected_rect:?}, samples={samples:?}"
        );
    }

    fn published_confirmation_instruction(host: &mut Host) -> String {
        match host {
            Host::Native(app) => app
                .build_a11y_tree_update(1.0)
                .nodes
                .into_iter()
                .find(|(_, node)| node.label() == Some("Confirmation"))
                .and_then(|(_, node)| node.description().map(str::to_owned))
                .expect("native published field describes how to confirm"),
            Host::Driver(driver) => {
                fn access(node: &crate::a11y::AccessNode) -> Option<String> {
                    if node.name.as_deref() == Some("Confirmation") {
                        return node.description.clone();
                    }
                    node.children.iter().find_map(access)
                }
                fn widget(node: &crate::headless::InspectionNode) -> Option<String> {
                    node.semantics
                        .as_ref()
                        .and_then(access)
                        .or_else(|| node.children.iter().find_map(widget))
                }
                widget(&driver.inspection().root)
                    .expect("Driver published field describes how to confirm")
            }
        }
    }
    fn confirm_using_published_instruction(native: bool) {
        for policy in [
            tilo_actions::ConfirmationPolicy::Confirm,
            tilo_actions::ConfirmationPolicy::Phrase {
                phrase: "APPLY EXACT".into(),
            },
        ] {
            let (root, ledger, _) = fixture(policy);
            let mut host = Host::new(native, root);
            host.activate("Emit request");
            assert!(ledger.borrow().is_empty());
            let instruction = published_confirmation_instruction(&mut host);
            let value = instruction
                .strip_prefix("Type ")
                .and_then(|s| s.strip_suffix(" to confirm"))
                .filter(|s| !s.is_empty())
                .expect("published instruction contains a discoverable confirmation phrase");
            let stale = host.submit_id();
            host.value(value);
            host.activate("Confirm");
            assert_eq!(
                &*ledger.borrow(),
                &[serde_json::json!({"payload":"original"})]
            );
            host.stale(stale);
            assert_eq!(ledger.borrow().len(), 1);
        }
    }
    #[test]
    fn ordinary_confirmation_native_instruction_is_independently_operable() {
        confirm_using_published_instruction(true);
    }
    #[test]
    fn ordinary_confirmation_driver_instruction_is_independently_operable() {
        confirm_using_published_instruction(false);
    }

    #[test]
    fn ordinary_confirmation_no_resolver_confirm_phrase_once_through_published_surface() {
        for native in [true, false] {
            for phrase in ["CONFIRM", "APPLY EXACT"] {
                let policy = if phrase == "CONFIRM" {
                    tilo_actions::ConfirmationPolicy::Confirm
                } else {
                    tilo_actions::ConfirmationPolicy::Phrase {
                        phrase: phrase.into(),
                    }
                };
                let (root, ledger, _) = fixture(policy);
                let mut host = Host::new(native, root);
                host.activate("Emit request");
                assert!(ledger.borrow().is_empty());
                let stale = host.submit_id();
                host.value("wrong");
                host.activate("Confirm");
                assert!(ledger.borrow().is_empty());
                host.value(phrase);
                host.activate("Confirm");
                assert_eq!(
                    &*ledger.borrow(),
                    &[serde_json::json!({"payload":"original"})]
                );
                host.stale(stale);
                assert_eq!(ledger.borrow().len(), 1);
            }
        }
    }
    #[test]
    fn ordinary_confirmation_cancel_and_successor_reject_stale_submit() {
        for native in [true, false] {
            let (root, ledger, _) = fixture(tilo_actions::ConfirmationPolicy::Confirm);
            let mut host = Host::new(native, root);
            host.activate("Emit request");
            let stale = host.submit_id();
            host.value("CONFIRM");
            host.activate("Cancel");
            host.activate("Emit request");
            host.value("CONFIRM");
            host.stale(stale);
            assert!(ledger.borrow().is_empty());
            host.activate("Confirm");
            assert_eq!(ledger.borrow().len(), 1);
        }
    }
    #[test]
    fn ordinary_confirmation_declared_mutations_and_revocation_refuse() {
        use tilo_actions::*;
        for native in [true, false] {
            for mutation in [
                "encoding",
                "contract",
                "policy",
                "schema",
                "unavailable",
                "duplicate-resource",
                "duplicate-action",
                "revoked",
                "label",
                "kind",
            ] {
                let (root, ledger, resources) = fixture(ConfirmationPolicy::Confirm);
                let mut host = Host::new(native, root);
                host.activate("Emit request");
                {
                    let mut resources = resources.borrow_mut();
                    let spec = &mut resources[0].actions[0];
                    match mutation {
                        "encoding" => {
                            let old = spec.contract.clone();
                            *spec = ActionSpec::typed_operation(
                                "run",
                                "Run fixture",
                                "Payload",
                                OperationKind::Effect,
                                "fixture.control",
                                vec![ActionParam::required("payload", ParamType::Text, "Payload")],
                                |_| {
                                    EncodedAction::encode_operation(
                                        "fixture.control",
                                        &serde_json::json!({"payload":"changed"}),
                                    )
                                    .unwrap()
                                },
                            )
                            .with_contract(old);
                        }
                        "contract" => spec.contract.risk = RiskLevel::Privileged,
                        "policy" => {
                            spec.contract.confirmation = ConfirmationPolicy::Phrase {
                                phrase: "OTHER".into(),
                            }
                        }
                        "schema" => {
                            let old = spec.contract.clone();
                            *spec = ActionSpec::typed_operation(
                                "run",
                                "Run fixture",
                                "Payload",
                                OperationKind::Effect,
                                "fixture.control",
                                vec![ActionParam::required(
                                    "payload",
                                    ParamType::Integer,
                                    "Payload",
                                )],
                                |args| {
                                    EncodedAction::encode_operation(
                                        "fixture.control",
                                        &args.to_json(),
                                    )
                                    .unwrap()
                                },
                            )
                            .with_contract(old);
                        }
                        "unavailable" => {
                            spec.contract.availability = Availability::Unavailable {
                                code: "gone".into(),
                                reason: "Gone".into(),
                            }
                        }
                        "duplicate-resource" => {
                            let duplicate = resources[0].clone();
                            resources.push(duplicate);
                        }
                        "duplicate-action" => {
                            let duplicate = resources[0].actions[0].clone();
                            resources[0].actions.push(duplicate);
                        }
                        "label" => spec.label = "Different reviewed meaning".into(),
                        "kind" => spec.kind = OperationKind::View,
                        "revoked" => {}
                        _ => unreachable!(),
                    }
                }
                if mutation == "revoked" {
                    host.revoke();
                }
                host.value("CONFIRM");
                host.activate("Confirm");
                assert!(ledger.borrow().is_empty(), "{native}:{mutation}");
            }
        }
    }
    #[test]
    fn ordinary_confirmation_optional_exact_evidence_change_or_loss_refuses() {
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };
        use tilo_actions::*;
        for native in [true, false] {
            for later in [0, 2] {
                for handler in [false, true] {
                    let (root, ledger, resources) = fixture(ConfirmationPolicy::Confirm);
                    let epoch = Arc::new(AtomicU64::new(1));
                    let captured = epoch.clone();
                    let original = resources.borrow()[0].actions[0].clone();
                    resources.borrow_mut()[0].actions[0] =
                        original.with_continuation_identity(move |query| {
                            let epoch = captured.load(Ordering::SeqCst);
                            if epoch == 0 {
                                return None;
                            }
                            matches!(query, ContinuationQuery::Exact(_)).then(|| {
                                ContinuationEvidence::Exact {
                                    target: ContinuationKey::new(
                                        "alias-target",
                                        [if handler {
                                            1u64.to_le_bytes()
                                        } else {
                                            epoch.to_le_bytes()
                                        }],
                                    ),
                                    handler: ContinuationKey::new(
                                        "handler",
                                        [if handler {
                                            epoch.to_le_bytes()
                                        } else {
                                            1u64.to_le_bytes()
                                        }],
                                    ),
                                }
                            })
                        });
                    let mut host = Host::new(native, root);
                    host.activate("Emit request");
                    epoch.store(later, Ordering::SeqCst);
                    host.value("CONFIRM");
                    host.activate("Confirm");
                    assert!(ledger.borrow().is_empty());
                }
            }
        }
    }
    #[test]
    fn ordinary_confirmation_history_clears_old_confirmation_and_fresh_no_resolver_works() {
        for native in [true, false] {
            let (root, ledger, _) = fixture(tilo_actions::ConfirmationPolicy::Confirm);
            let mut host = Host::new(native, root);
            host.activate("Emit request");
            host.value("CONFIRM");
            let stale = host.submit_id();
            host.activate("History");
            assert!(ledger.borrow().is_empty());
            host.activate("History");
            assert!(ledger.borrow().is_empty());
            host.stale(stale);
            host.value("CONFIRM");
            host.activate("Confirm");
            assert!(ledger.borrow().is_empty());
            host.activate("Cancel");
            host.activate("Emit request");
            host.value("CONFIRM");
            host.activate("Confirm");
            assert_eq!(ledger.borrow().len(), 1);
        }
    }
    #[test]
    fn ordinary_confirmation_copies_emitted_json_and_types_with_fixture_fonts() {
        for native in [true, false] {
            let input = source();
            let (widget, ledger, _) =
                fixture_widget(tilo_actions::ConfirmationPolicy::Confirm, input.clone());
            let mut host = Host::new(native, managed(Box::new(widget)));
            host.activate("Emit request");
            input
                .lock()
                .unwrap()
                .insert("payload".into(), serde_json::json!("successor-selection"));
            host.key("CONFIRM");
            host.activate("Confirm");
            assert_eq!(
                &*ledger.borrow(),
                &[serde_json::json!({"payload":"original"})]
            );
        }
    }
    #[test]
    fn ordinary_confirmation_component_legacy_owner_permission_and_duplicate_matrix() {
        for native in [true, false] {
            for component in [true, false] {
                for case in ["positive", "closed", "revoked", "duplicate"] {
                    let (widget, ledger, resources) =
                        fixture_widget(tilo_actions::ConfirmationPolicy::Confirm, source());
                    let (root, guard) = owned(widget, component);
                    let mut host = Host::new(native, guard.managed(root));
                    host.activate("Emit request");
                    match case {
                        "closed" => guard.close(),
                        "revoked" => {
                            if component {
                                guard.revoke()
                            } else {
                                host.revoke()
                            }
                        }
                        "duplicate" => {
                            let duplicate = resources.borrow()[0].clone();
                            resources.borrow_mut().push(duplicate);
                        }
                        _ => {}
                    }
                    host.value("CONFIRM");
                    host.activate("Confirm");
                    assert_eq!(
                        ledger.borrow().len(),
                        usize::from(case == "positive"),
                        "{native}:{component}:{case}"
                    );
                }
            }
        }
    }
    #[test]
    fn ordinary_confirmation_same_address_successor_cannot_receive_original_request() {
        for native in [true, false] {
            for component in [true, false] {
                let (widget, old_ledger, _) =
                    fixture_widget(tilo_actions::ConfirmationPolicy::Confirm, source());
                let (old, old_guard) = owned(widget, component);
                let (widget, new_ledger, _) =
                    fixture_widget(tilo_actions::ConfirmationPolicy::Confirm, source());
                let (new, new_guard) = owned(widget, component);
                let mut values = [Some(old), Some(new)];
                let changed = Rc::new(std::cell::Cell::new(false));
                let projection = changed.clone();
                let mut model = crate::widget::ModelView::from_document(
                    move |_, _| projection.get(),
                    move |changed| values[usize::from(*changed)].take().unwrap(),
                );
                model.sync(&automerge::AutoCommit::new(), &automerge::ROOT);
                let mut host = Host::new(native, old_guard.managed(Box::new(model)));
                host.activate("Emit request");
                host.value("CONFIRM");
                let stale = host.submit_id();
                changed.set(true);
                host.frame();
                match &mut host {
                    Host::Native(app) => {
                        app.dispatch_a11y_action(stale, &crate::a11y::Action::Activate);
                        layout(app)
                    }
                    Host::Driver(driver) => {
                        let _ = driver.perform_a11y_action(stale, &crate::a11y::Action::Activate);
                    }
                }
                assert!(old_ledger.borrow().is_empty());
                assert!(new_ledger.borrow().is_empty());
                drop((old_guard, new_guard));
            }
        }
    }
    #[test]
    fn ordinary_confirmation_native_immediate_and_text_use_original_representation() {
        use tilo_actions::*;
        for text in [false, true] {
            let (root, ledger, resources) = fixture(ConfirmationPolicy::Confirm);
            let mut spec = if text {
                ActionSpec::prompted_operation(
                    "run",
                    "Run fixture",
                    "Enter original text",
                    "Original",
                    OperationKind::Effect,
                    "fixture.control",
                    |text| {
                        EncodedAction::encode_operation(
                            "fixture.control",
                            &serde_json::json!({"prompted_argument":text}),
                        )
                        .unwrap()
                    },
                )
            } else {
                ActionSpec::operation(
                    "run",
                    "Run fixture",
                    OperationKind::Effect,
                    "fixture.control",
                    &serde_json::json!({"immediate":"original"}),
                )
                .unwrap()
            };
            spec.contract.confirmation = ConfirmationPolicy::Confirm;
            resources.borrow_mut()[0].actions[0] = spec;
            let mut app =
                App::from_managed_root_with_controlled_inputs(State::new(), root).unwrap();
            layout(&mut app);
            app.dispatch_a11y_action(
                crate::accesskit_bridge::resource_action_node_id("ordinary:fixture", "run"),
                &crate::a11y::Action::Activate,
            );
            layout(&mut app);
            if text {
                let named_action_target = named(&mut app, "Search");
                app.dispatch_a11y_action(
                    named_action_target,
                    &crate::a11y::Action::SetValue("not-a-value-field".into()),
                );
                layout(&mut app);
                app.action_palette_handle_key(&crate::synthetic_input::key_input(
                    &Key::Named(NamedKey::Enter),
                    ElementState::Pressed,
                ));
                layout(&mut app);
            }
            assert!(ledger.borrow().is_empty());
            let named_action_target = named(&mut app, "Confirmation");
            app.dispatch_a11y_action(
                named_action_target,
                &crate::a11y::Action::SetValue("CONFIRM".into()),
            );
            layout(&mut app);
            let named_action_target = named(&mut app, "Confirm");
            app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
            layout(&mut app);
            assert_eq!(
                &*ledger.borrow(),
                &[if text {
                    serde_json::json!({"prompted_argument":"not-a-value-field"})
                } else {
                    serde_json::json!({"immediate":"original"})
                }]
            );
        }
    }
    #[test]
    fn ordinary_confirmation_history_preserves_original_snapshot_and_exact_continuation() {
        use tilo_actions::*;
        for native in [true, false] {
            for changed in [false, true] {
                let (root, ledger, resources) = fixture(ConfirmationPolicy::Confirm);
                let original = resources.borrow()[0].actions[0].clone();
                resources.borrow_mut()[0].actions[0] = original.with_continuation_identity(|q| {
                    matches!(q, ContinuationQuery::Exact(_)).then(|| ContinuationEvidence::Exact {
                        target: ContinuationKey::new("target", [b"original"]),
                        handler: ContinuationKey::new("handler", [b"v1"]),
                    })
                });
                let mut host = Host::new(native, root);
                host.activate("Emit request");
                host.value("CONFIRM");
                host.activate("History");
                if changed {
                    resources.borrow_mut()[0].actions[0].label = "Changed during History".into();
                }
                host.activate("History");
                assert!(ledger.borrow().is_empty());
                host.value("CONFIRM");
                host.activate("Confirm");
                assert_eq!(
                    ledger.borrow().len(),
                    usize::from(!changed),
                    "{native}:{changed}"
                );
            }
        }
    }
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct FixtureEdit;
    impl tilo_actions::Action for FixtureEdit {
        fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
            use automerge::transaction::Transactable;
            doc.put(automerge::ROOT, "confirmed-document", true)?;
            Ok(())
        }
    }
    #[test]
    fn ordinary_confirmation_document_uses_authored_undo_completion() {
        use tilo_actions::*;
        for native in [true, false] {
            let (root, _, resources) = fixture(ConfirmationPolicy::Confirm);
            let mut spec = ActionSpec::typed(
                "run",
                "Run fixture",
                "Payload",
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required("payload", ParamType::Text, "Payload")],
                |_| EncodedAction::encode("fixture.edit", &FixtureEdit).unwrap(),
            );
            spec.contract.confirmation = ConfirmationPolicy::Confirm;
            resources.borrow_mut()[0].actions[0] = spec;
            let mut host = Host::new(native, root);
            match &mut host {
                Host::Native(app) => app
                    .action_registry_mut()
                    .register::<FixtureEdit>("fixture.edit"),
                Host::Driver(driver) => driver
                    .action_registry_mut()
                    .register::<FixtureEdit>("fixture.edit"),
            }
            fn state(host: &mut Host) -> &mut State {
                match host {
                    Host::Native(app) => &mut app.state,
                    Host::Driver(driver) => driver.state_mut(),
                }
            }
            use automerge::ReadDoc;
            assert!(state(&mut host).peek_last_undo().is_none());
            let before = state(&mut host).doc.get_heads();
            host.activate("Emit request");
            host.value("CONFIRM");
            host.activate("Confirm");
            let author = match &host {
                Host::Native(app) => app.collaboration.local_author.clone(),
                Host::Driver(_) => crate::permissions::UserId("fixture".into()),
            };
            let state = state(&mut host);
            assert_eq!(
                state
                    .doc
                    .get(automerge::ROOT, "confirmed-document")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_bool(),
                Some(true)
            );
            let entry = state.peek_last_undo().unwrap();
            assert_eq!(entry.before, before);
            assert_ne!(entry.after, entry.before);
            assert_eq!(entry.author.as_ref(), Some(&author));
            let committed = state.doc.get_heads();
            assert!(state.undo_as(Some(&author), None, None, None).is_some());
            assert!(
                state
                    .doc
                    .get(automerge::ROOT, "confirmed-document")
                    .unwrap()
                    .is_none()
            );
            assert_ne!(state.doc.get_heads(), committed);
            assert!(
                state.peek_last_undo().is_none(),
                "exactly one authored undo boundary"
            );
        }
    }
    #[test]
    fn ordinary_confirmation_modal_focus_tracks_actual_field() {
        for native in [true, false] {
            let (root, _, _) = fixture(tilo_actions::ConfirmationPolicy::Confirm);
            let mut host = Host::new(native, root);
            host.activate("Emit request");
            assert_eq!(host.focused_label().as_deref(), Some("Confirmation"));
        }
    }
    #[test]
    fn ordinary_confirmation_pointer_and_keyboard_controls_use_shared_transition() {
        for native in [true, false] {
            for cancel in [false, true] {
                let (root, ledger, _) = fixture(tilo_actions::ConfirmationPolicy::Confirm);
                let mut host = Host::new(native, root);
                host.activate("Emit request");
                host.value("CONFIRM");
                let label = if cancel { "Cancel" } else { "Confirm" };
                let nodes = match &mut host {
                    Host::Native(app) => app.build_a11y_tree_update(1.0).nodes,
                    Host::Driver(driver) => driver.a11y_nodes(),
                };
                let bounds = nodes
                    .iter()
                    .find(|(_, node)| node.label() == Some(label))
                    .unwrap()
                    .1
                    .bounds()
                    .unwrap();
                assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
                let scale = if native { 2.0 } else { 1.0 };
                let event = WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(
                        (bounds.x0 + bounds.x1) * 0.5 * scale,
                        (bounds.y0 + bounds.y1) * 0.5 * scale,
                    ),
                };
                let mut send = |event: &WindowEvent| match &mut host {
                    Host::Native(app) => {
                        assert!(app.action_palette_handle_event(event, scale));
                        layout(app)
                    }
                    Host::Driver(driver) => driver.send(event),
                };
                send(&event);
                for state in [ElementState::Pressed, ElementState::Released] {
                    send(&WindowEvent::MouseInput {
                        device_id: winit::event::DeviceId::dummy(),
                        state,
                        button: winit::event::MouseButton::Left,
                    });
                }
                assert_eq!(ledger.borrow().len(), usize::from(!cancel));
            }
        }
        for native in [true, false] {
            let (root, ledger, _) = fixture(tilo_actions::ConfirmationPolicy::Confirm);
            let mut host = Host::new(native, root);
            host.activate("Emit request");
            host.value("CONFIRM");
            for key in [NamedKey::Tab, NamedKey::Tab] {
                let event =
                    crate::synthetic_input::key_input(&Key::Named(key), ElementState::Pressed);
                match &mut host {
                    Host::Native(app) => {
                        assert!(app.action_palette_handle_event(&event, 1.0));
                        layout(app)
                    }
                    Host::Driver(driver) => driver.send(&event),
                }
            }
            assert_eq!(host.focused_label().as_deref(), Some("Cancel"));
            let event = crate::synthetic_input::key_input(
                &Key::Named(NamedKey::Enter),
                ElementState::Pressed,
            );
            match &mut host {
                Host::Native(app) => {
                    assert!(app.action_palette_handle_event(&event, 1.0));
                    layout(app)
                }
                Host::Driver(driver) => driver.send(&event),
            }
            assert!(ledger.borrow().is_empty());
        }
    }
    #[test]
    fn ordinary_confirmation_history_preserves_e_suppresses_h_and_fresh_f_is_ordinary() {
        use tilo_actions::*;
        for native in [true, false] {
            for owner in [0, 1, 2] {
                let input = source();
                let (widget, ledger, resources) =
                    fixture_widget(ConfirmationPolicy::Confirm, input.clone());
                let output = widget.output.clone();
                let spec = resources.borrow()[0].actions[0].clone();
                resources.borrow_mut()[0].actions[0] = spec.with_continuation_identity(|query| {
                    matches!(query, ContinuationQuery::Exact(_)).then(|| {
                        ContinuationEvidence::Exact {
                            target: ContinuationKey::new("target", [b"original"]),
                            handler: ContinuationKey::new("handler", [b"original"]),
                        }
                    })
                });
                let (root, guard) = if owner == 0 {
                    (managed(Box::new(widget)), None)
                } else {
                    let (root, guard) = owned(widget, owner == 1);
                    (guard.managed(root), Some(guard))
                };
                let mut host = Host::new(native, root);
                host.activate("Emit request");
                host.value("CONFIRM");
                let stale = host.submit_id();
                host.activate("History");
                assert_eq!(
                    output.borrow().as_ref().unwrap().emit(ControlInvocation {
                        address: ActionAddress::new("ordinary:fixture", "run"),
                        arguments: serde_json::json!({"payload":"H"})
                            .as_object()
                            .unwrap()
                            .clone()
                    }),
                    Ok(crate::control_custody::LocalEmission::Suppressed)
                );
                input
                    .lock()
                    .unwrap()
                    .insert("payload".into(), serde_json::json!("F"));
                host.activate("History");
                assert!(ledger.borrow().is_empty());
                host.stale(stale);
                host.activate("Confirm");
                assert!(ledger.borrow().is_empty(), "old typed evidence was cleared");
                host.value("CONFIRM");
                host.activate("Confirm");
                assert_eq!(
                    &*ledger.borrow(),
                    &[serde_json::json!({"payload":"original"})]
                );
                // Fresh action deliberately has no History resolver.
                let mut fresh = resources.borrow()[0].actions[0].clone();
                fresh = fresh.with_continuation_identity(|_| None);
                resources.borrow_mut()[0].actions[0] = fresh;
                host.activate("Emit request");
                host.value("CONFIRM");
                host.activate("Confirm");
                assert_eq!(
                    &*ledger.borrow(),
                    &[
                        serde_json::json!({"payload":"original"}),
                        serde_json::json!({"payload":"F"})
                    ]
                );
                drop(guard);
            }
        }
    }
    #[test]
    fn ordinary_confirmation_none_and_later_optional_evidence_remain_compatible() {
        use tilo_actions::*;
        for native in [true, false] {
            let (root, ledger, _) = fixture(ConfirmationPolicy::None);
            let mut host = Host::new(native, root);
            host.activate("Emit request");
            assert_eq!(ledger.borrow().len(), 1);
            let (root, ledger, resources) = fixture(ConfirmationPolicy::Confirm);
            let mut host = Host::new(native, root);
            host.activate("Emit request");
            let spec = resources.borrow()[0].actions[0].clone();
            resources.borrow_mut()[0].actions[0] = spec.with_continuation_identity(|_| {
                Some(ContinuationEvidence::Exact {
                    target: ContinuationKey::new("later", [b"target"]),
                    handler: ContinuationKey::new("later", [b"handler"]),
                })
            });
            host.value("CONFIRM");
            host.activate("Confirm");
            assert_eq!(ledger.borrow().len(), 1);
        }
    }
    #[test]
    fn ordinary_confirmation_cancel_restores_retained_background_focus() {
        for native in [true, false] {
            let (root, ledger, _) = fixture(tilo_actions::ConfirmationPolicy::Confirm);
            let mut host = Host::new(native, root);
            host.act("Emit request", crate::a11y::Action::Focus);
            assert_eq!(host.focused_label().as_deref(), Some("Emit request"));
            host.activate("Emit request");
            assert_eq!(host.focused_label().as_deref(), Some("Confirmation"));
            if let Host::Native(app) = &host {
                assert!(app.actions.ordinary_palette_return_focus.is_some());
            }
            host.activate("Cancel");
            assert_eq!(host.focused_label().as_deref(), Some("Emit request"));
            if let Host::Native(app) = &host {
                assert!(app.actions.ordinary_palette_return_focus.is_none());
            }
            assert!(ledger.borrow().is_empty());
        }
    }
    #[test]
    fn ordinary_confirmation_cancel_does_not_focus_a_removed_opener() {
        let (root, ledger, _) = fixture(tilo_actions::ConfirmationPolicy::Confirm);
        let mut host = Host::new(true, root);
        host.act("Emit request", crate::a11y::Action::Focus);
        assert_eq!(host.focused_label().as_deref(), Some("Emit request"));
        host.activate("Emit request");
        assert_eq!(host.focused_label().as_deref(), Some("Confirmation"));
        let Host::Native(app) = &mut host else {
            unreachable!()
        };
        assert!(
            app.actions.ordinary_palette_return_focus.is_some(),
            "the modal actually retained the opener identity before removal"
        );
        // Retire the old custody map before removing its retained owner.
        app.bindings.clear();
        app.layout.node_map.clear();
        app.focused_node = None;
        app.root_widget = Box::new(crate::widget::Button::new(
            "replacement-control",
            "Unrelated control",
        ));
        app.layout.structure_dirty = true;
        layout(app);
        host.activate("Cancel");
        let Host::Native(app) = &mut host else {
            unreachable!()
        };
        let published = app.build_a11y_tree_update(1.0);
        assert!(
            published
                .nodes
                .iter()
                .any(|(_, node)| { node.label() == Some("Unrelated control") })
        );
        assert!(
            published
                .nodes
                .iter()
                .all(|(_, node)| { node.label() != Some("Emit request") })
        );
        assert!(
            app.focused_node.is_none(),
            "removed focus must not transfer"
        );
        assert!(
            ledger.borrow().is_empty(),
            "cancellation executes no action"
        );
    }

    #[test]
    fn native_control_coverage_admits_only_declared_app_and_host_actions() {
        use tilo_actions::{ActionBinding, ActionSpec, OperationKind, Resource};
        let (widget, _, _) = fixture_widget(tilo_actions::ConfirmationPolicy::None, source());
        let missing_binding = |resource_id: &str| {
            let resource = Resource::new(resource_id, "fixture.coverage", "Missing action")
                .with_action(
                    ActionSpec::operation(
                        "nonexistent",
                        "Missing action",
                        OperationKind::View,
                        "fixture.coverage",
                        &(),
                    )
                    .unwrap(),
                );
            ActionBinding::unit_for(&resource, "nonexistent").unwrap()
        };
        let controls = crate::widget::Column::new()
            .push(Box::new(widget))
            .push(Box::new(
                crate::widget::Button::new("coverage-search", "Search").on_action(
                    crate::search::open_search_binding(&crate::search::SearchRequest::this_app()),
                ),
            ))
            .push(Box::new(
                crate::standalone_history_host::history_control_button(
                    "coverage-history",
                    crate::widget::window_header::HistoryControl::Closed,
                ),
            ))
            .push(Box::new(
                crate::widget::Button::new("coverage-missing-app", "Missing app action")
                    .on_action(missing_binding("ordinary:fixture")),
            ))
            .push(Box::new(
                crate::widget::Button::new("coverage-missing-host", "Missing host action")
                    .on_action(missing_binding("tilo.host:search")),
            ));
        let mut app = App::from_managed_root_with_controlled_inputs(
            State::new(),
            managed(Box::new(controls)),
        )
        .unwrap();
        layout(&mut app);
        assert!(app.history.is_some(), "the real History owner is installed");
        let coverage = app.control_action_coverage();
        for (control, expected) in [
            ("ordinary-emitter", ControlBindingStatus::Resolved),
            ("coverage-search", ControlBindingStatus::Resolved),
            ("coverage-history", ControlBindingStatus::Resolved),
            ("coverage-missing-app", ControlBindingStatus::StaleAddress),
            ("coverage-missing-host", ControlBindingStatus::StaleAddress),
        ] {
            let entry = coverage
                .iter()
                .find(|entry| entry.binding.control_id == control)
                .unwrap_or_else(|| panic!("missing retained binding {control}"));
            assert_eq!(entry.status, expected, "binding {control}");
        }
    }

    #[test]
    fn ordinary_confirmation_native_closed_or_replaced_outer_domain_refuses() {
        for replacement in [false, true] {
            let (root, ledger, _) = fixture(tilo_actions::ConfirmationPolicy::Confirm);
            let mut host = Host::new(true, root);
            host.activate("Emit request");
            host.value("CONFIRM");
            let Host::Native(app) = &mut host else {
                unreachable!()
            };
            let old = app.control_domain.clone();
            if replacement {
                app.control_domain = crate::ControlDomain::new();
            } else {
                old.close();
            }
            host.activate("Confirm");
            assert!(ledger.borrow().is_empty());
        }
    }
    #[test]
    fn ordinary_confirmation_native_publishes_actual_prompt() {
        let (root, ledger, _) = fixture(tilo_actions::ConfirmationPolicy::Confirm);
        let mut app = App::from_managed_root_with_controlled_inputs(State::new(), root).unwrap();
        layout(&mut app);
        let named_action_target = named(&mut app, "Emit request");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        app.controlled_local_idle_for_test();
        layout(&mut app);
        assert!(ledger.borrow().is_empty());
        named(&mut app, "Confirmation");
        named(&mut app, "Confirm");
        named(&mut app, "Cancel");
    }
    #[test]
    fn ordinary_confirmation_driver_publishes_actual_prompt() {
        let (root, ledger, _) = fixture(tilo_actions::ConfirmationPolicy::Confirm);
        let mut driver = crate::headless::Driver::from_managed_root_with_controlled_inputs(
            State::new(),
            root,
            (900, 650),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        )
        .unwrap();
        driver
            .perform_semantic_named("ordinary-emitter", None, &crate::a11y::Action::Activate)
            .unwrap();
        assert!(ledger.borrow().is_empty());
        assert!(
            driver.a11y_node_named("Confirmation").is_some(),
            "actual palette field must be published"
        );
        assert!(driver.a11y_node_named("Confirm").is_some());
        assert!(driver.a11y_node_named("Cancel").is_some());
    }
}

#[cfg(test)]
mod legacy_control_feedback_tests {
    use super::*;
    #[test]
    fn native_queued_finalizer_feedback_stays_with_originating_legacy_owner() {
        use crate::headless::legacy_control_feedback_tests::{owner, register};
        for fail in [false, true] {
            let (_nested_owner, nested, nested_seen) = owner(99051, false, None);
            let (_owner, tile, seen) = owner(99050, fail, Some(nested));
            let (_sibling_owner, sibling, sibling_seen) = owner(99052, false, None);
            let root = crate::widget::Column::new()
                .push(Box::new(tile))
                .push(Box::new(sibling));
            let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
            register(app.action_registry_mut());
            seen.begin();
            app.controlled_local_idle_for_test();
            app.controlled_local_idle_for_test();
            assert_eq!(
                *seen.results.borrow(),
                vec![("begin".into(), true), ("finalize".into(), !fail)]
            );
            assert!(
                nested_seen.results.borrow().is_empty(),
                "nested owner cannot inherit callback"
            );
            assert!(
                sibling_seen.results.borrow().is_empty(),
                "same-address sibling cannot inherit callback"
            );
        }
    }
}
