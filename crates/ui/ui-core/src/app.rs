use crate::floating_surface::FloatingSurfaceWidgetExt as _;
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
use crate::widget::FullscreenWidgetExt as _;
#[cfg(feature = "native")]
use std::sync::{Arc, mpsc};
use taffy::AvailableSpace;
#[cfg(feature = "native")]
use vello::{Renderer, RendererOptions, peniko::Color};
use vello::{Scene, kurbo::Affine, kurbo::Point};
use winit::event::{ElementState, WindowEvent};
use winit::window::CursorIcon;
#[cfg(feature = "native")]
use winit::{
    application::ApplicationHandler,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Fullscreen, Window, WindowId},
};

use crate::app_core::ResourceInvocation;
use std::cell::Cell;

#[cfg(feature = "native")]
#[path = "profiling/native_input_isolation.rs"]
mod native_input_isolation;

#[cfg(feature = "native")]
#[path = "animation_schedule.rs"]
pub mod animation_schedule;

#[cfg(feature = "model")]
mod action_directory_snapshot;
#[cfg(feature = "model")]
mod bound_document_sync;
mod close_admission;
#[cfg(feature = "model")]
mod follow_ups;
#[cfg(feature = "model")]
mod handoff;
#[cfg(feature = "model")]
mod held_answers;
mod host_commands;
// Pointing at what a report is about: a report goes to people in the workspace, Tilo's model's.
#[cfg(feature = "model")]
mod report_point;
#[cfg(all(feature = "native", test))]
#[cfg(feature = "model")]
mod report_point_tests;
mod window_box;

mod model_lifecycle;
#[cfg(feature = "model")]
mod presences;
mod retirement_assembly;
#[cfg(test)]
mod retirement_lifecycle_tests;
#[cfg(feature = "model")]
mod surface_route;
include!("app/floating.rs");
mod part_bar;
mod runtime;
mod window_bar;
use runtime::{AccessibilityRuntime, ActionRuntime, InputRuntime, LayoutRuntime, WindowRuntime};
#[cfg(feature = "model")]
use runtime::{BrokerReply, HeldOwnInvocation};
#[cfg(feature = "model")]
use runtime::{CollaborationRuntime, ContinuationRuntime, HistoryRuntime, PresenceRuntime};
#[cfg(feature = "model")]
include!("app/continuation.rs");
include!("app/dev_session.rs");
include!("app/dev_library.rs");
include!("app/actions.rs");
include!("app/app_core.rs");
#[cfg(feature = "model")]
include!("app/action_directory.rs");
include!("app/control_availability.rs");
include!("app/accessibility.rs");
include!("app/window_targets.rs");
include!("app/touch_exploration.rs");
#[cfg(feature = "model")]
include!("app/collaboration.rs");
include!("app/persistence.rs");
include!("app/host.rs");
#[cfg(all(feature = "native", any(target_os = "android", test)))]
mod android_text_input;
#[cfg(feature = "native")]
mod native_runner;
#[cfg(feature = "native")]
mod slow_event;
/// How an app's error begins when its window stopped without being closed.
pub const WINDOW_LOST: &str = "the window stopped without being closed";

/// Exit status of an app whose window stopped without being closed: the launcher reports it as a failed
/// launch rather than a clean exit.
pub const WINDOW_LOST_EXIT_CODE: i32 = 70;

mod content_zoom;
pub mod driver;
#[cfg(test)]
mod driver_apps_a_tests;
#[cfg(test)]
mod driver_cards_tests;
#[cfg(test)]
mod driver_chrome_tests;
#[cfg(test)]
mod driver_frames_tests;
#[cfg(test)]
mod driver_host_style_tests;
#[cfg(test)]
mod driver_jobs_tests;
#[cfg(test)]
mod driver_layout_check_tests;
#[cfg(test)]
mod driver_metrics_tests;
#[cfg(test)]
mod driver_owner_tests;
#[cfg(test)]
mod driver_palette_tests;
#[cfg(test)]
mod driver_tests;
#[cfg(test)]
mod driver_uicore_tests;
#[cfg(test)]
mod driver_widget_tests;
#[cfg(test)]
mod driver_window_bar_tests;
mod held_input;
mod input_publication;
#[cfg(feature = "model")]
mod local_input_capture;
#[cfg(test)]
mod session_tests;
mod surface;
#[cfg(all(any(windows, target_os = "linux"), feature = "native"))]
pub(crate) use native_runner::startup::prepare_native_renderer;
pub use surface::{AppSurface, SurfaceDamage};
#[cfg(all(feature = "native", feature = "model"))]
mod embedded;
#[cfg(all(feature = "native", feature = "model"))]
pub use embedded::EmbeddedApp;

#[cfg(feature = "model")]
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

#[cfg(feature = "model")]
const ACTION_DIRECTORY_MAX_RESULTS: usize = ::collaboration::action_directory::MAX_RESULTS;
#[cfg(feature = "model")]
const ACTION_DIRECTORY_MAX_WORK: usize = ::collaboration::action_directory::MAX_WORK;
#[cfg(feature = "model")]
const ACTION_DIRECTORY_MAX_BYTES: usize = ::collaboration::action_directory::MAX_BYTES;
#[cfg(feature = "model")]
const ACTION_DIRECTORY_MAX_DEPTH: u16 = ::collaboration::action_directory::MAX_DEPTH;
#[cfg(feature = "model")]
const ACTION_DIRECTORY_MAX_CURSORS: usize = 256;
const MAX_PENDING_APPLICATION_LAUNCHES: usize = 256;

/// The person at this machine, whom a window acts for unless its host says otherwise: the
/// workspace's identity of them with Tilo's model, else the operating system's account.
pub(crate) fn local_person() -> tilo_actions::MemberId {
    #[cfg(feature = "model")]
    let person = crate::collaboration::local_user_id();
    #[cfg(not(feature = "model"))]
    let person = tilo_actions::MemberId::Person(tilo_actions::PersonId::from(
        crate::process::environment()
            .identity
            .account_name
            .clone()
            .unwrap_or_else(|| "local".to_owned()),
    ));
    person
}

/// Synthetic input for `EventLoopProxy`: each variant becomes the real `WindowEvent`(s) and routes
/// through the same handler, so injected and real-hardware events take the identical code path.
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
    /// Delivered to [`crate::widget::Widget::on_external_event`] rather than translated to a
    /// `WindowEvent` (e.g.
    External(String),
    /// A host command chosen in the operating system chrome (a menu item), entering the same API as
    /// the palette.
    HostCommand(tilo_actions::ControlInvocation),
    /// Text typed into the host's title-bar search field; search continues in the palette.
    HostSearch(String),
    /// Synchronize and redraw after typed state was changed by a worker.
    Wake(crate::wake::WakeEvent),
    /// `tilolang dev` is replacing this process. Flush the document and write
    /// the route and caret, then leave the event loop so the next process can
    /// read them.
    DevSessionStop,
    /// `tilolang dev` rebuilt the app.
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
    /// The system bars, cutout or soft keyboard now cover a different part of the window.
    #[cfg(feature = "native")]
    SafeAreaChanged,
    /// The shell opens one more window of the app in this process ([`crate::window_link`]).
    #[cfg(feature = "native")]
    OpenWindow(crate::window_link::PendingWindow),
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
#[cfg(feature = "model")]
use crate::collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, CollaborationManager, PresencePayload,
};
use crate::focus_navigation::Direction;
use crate::ime::ImeWindowState;
#[cfg(feature = "model")]
use crate::permissions::PermissionContext;
use crate::presence::PresenceCtx;
#[cfg(feature = "model")]
use crate::presence::PresenceKey;
use crate::state::State;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Duration;
use crate::time::Instant;
use crate::widget::Widget;
use taffy::{NodeId, TaffyTree};
use vello::kurbo::Rect as KurboRect;

/// Cached [`crate::process::RunnerDiagnostics::perf_log`] check
/// (`TILO_UI_PERF=1` natively), so the per-frame read is cheap.
#[cfg(feature = "native")]
fn perf_enabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| crate::process::environment().diagnostics.perf_log)
}

/// `TILO_UI_NO_SCROLL_OPT=1` disables the scroll-reposition fast path,
/// forcing a full `compute_layout` on every `layout_dirty` frame — a safety
/// valve if a subtree ever needs a re-solve without reporting
/// `layout_needs_measure`.
fn scroll_opt_disabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        crate::process::environment()
            .diagnostics
            .disable_scroll_fast_path
    })
}

#[cfg(test)]
#[cfg(feature = "model")]
mod continuation_tests {
    use super::continuation_publication;
    use crate::collaboration::{CollabRequest, ContinuationMetadata};

    #[test]
    fn vanished_continuations_clear_the_durable_projection() {
        {
            let request = continuation_publication(None, true);
            assert!(matches!(request, Some(CollabRequest::ClearContinuation)));

            assert!(continuation_publication(None, false).is_none());

            let metadata = ContinuationMetadata {
                schema_version: 3,
                bytes: vec![1, 2, 3],
            };
            assert!(matches!(
                continuation_publication(Some(&metadata), false),
                Some(CollabRequest::ReportContinuation {
                    schema_version: 3,
                    metadata,
                }) if metadata == vec![1, 2, 3]
            ));
        }
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

fn word_at(s: &str, byte: usize) -> Option<String> {
    let b = byte.min(s.len());
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

#[derive(Clone, Debug)]
struct DirectedInvocation {
    wire_id: u64,
    cancellation_requested: bool,
}

/// Public-in-crate so the throttle decision is unit-testable without a real event loop.
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

#[cfg(feature = "native")]
fn find_appearance_intent(w: &dyn Widget) -> Option<crate::theme::AppearanceIntent> {
    let _entry = crate::widget::bound_nodes::enter_widget(w).ok()?;
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

#[cfg(feature = "model")]
type PresenceBroadcastSink = Box<dyn Fn(&PresenceKey, &PresencePayload) + Send + Sync>;
type ResolvedResourceInvocation<'a> = (
    Option<serde_json::Map<String, serde_json::Value>>,
    &'a tilo_actions::ResourceSpec,
    &'a tilo_actions::ActionSpec,
);

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResourceCacheKey {
    mutation_generation: u64,
    locale: u64,
}

struct ResourceCache {
    key: ResourceCacheKey,
    declared: Vec<tilo_actions::ResourceSpec>,
    complete: Option<Vec<tilo_actions::ResourceSpec>>,
    /// The layout node each control of `complete` stands in, by its resource id: whose control
    /// it is.
    #[cfg(feature = "model")]
    controls: std::collections::HashMap<String, u64>,
}

struct LocalResourceCache {
    key: ResourceCacheKey,
    permission_generation: u64,
    resources: Option<Vec<tilo_actions::ResourceSpec>>,
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
    /// A high-frequency framework-owned CRDT editing stream with automatic collaboration/history
    /// semantics rather than a discrete action address.
    CollaborativeDocument,
    SemanticAction,
    /// Multiple affordances intentionally invoke the same operation.
    SharedAddress,
    StaleAddress,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlBindingCoverage {
    pub binding: crate::widget::ControlBindingMetadata,
    pub status: ControlBindingStatus,
}

pub use crate::action_locus::{ActionUiRoute, PaintedActionMarker};

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
    #[cfg(feature = "model")]
    retirement: Option<crate::semantic_document_read::retirement::RootRetirementPrelude>,
    #[cfg(all(any(windows, target_os = "linux"), feature = "native"))]
    startup_renderer: Option<native_runner::startup::RendererPreparation>,
    control_domain: crate::ControlDomain,
    wake_scope: Option<crate::wake::WakeScope>,
    state: Box<State>,
    root_widget: Box<dyn Widget>,
    #[cfg(feature = "model")]
    history: Option<crate::history_mode::host::HistoryChrome>,
    #[cfg(feature = "model")]
    #[cfg(any(test, feature = "test-support"))]
    controlled_installed_history_definition:
        Option<&'static crate::history_mode::HistoryDefinition>,
    framework_input: std::rc::Rc<Cell<bool>>,
    #[cfg(feature = "model")]
    history_key_release: bool,
    #[cfg(feature = "model")]
    history_focus: Option<crate::widget::layout::CompositionFocus>,
    text_renderer: TextRenderer,
    palette_source: PaletteSource,
    watch_system_settings: bool,
    layout: LayoutRuntime,
    bindings: crate::widget::bound_nodes::BoundNodeMap,
    #[cfg(feature = "model")]
    component_close_reports: Vec<crate::application::ComponentCloseReport>,
    close_admission: crate::close_admission::Admission,
    /// A window asked for the thing a report is about, until the person points
    /// at it or leaves ([`report_point::Pointing`]).
    #[cfg(feature = "model")]
    report_pointing: Option<report_point::Pointing>,
    /// The send-to box the window stands with no menu around it ([`Self::open_send_box`]).
    window_box: crate::widget::context_menu::ContextMenu,
    needs_sync: bool,
    pub theme: Theme,
    pub debug_mode: bool,
    pub focused_node: Option<NodeId>,
    pub focusable_order: Vec<NodeId>,
    /// The name of the control the keyboard is on ([`Self::answer_name`]),
    /// kept beside [`Self::focused_node`]: a tree rebuilt while the person is
    /// on it replaces every node id, and a name is what finds the same control
    /// again when nothing in the new tree reports itself as focused.
    focus_anchor: Option<String>,
    /// Opt-in: focuses the first focusable widget after initial layout, so the app is keyboard-usable
    /// without a click.
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
    /// Shared settings-file watcher for OS-wide theme changes made in Settings.
    theme_store_stamp: Option<crate::settings_source::SettingsStamp>,
    last_theme_store_check: Instant,
    /// The schedule an "Automatic" theme follows and whether it was night when last applied: sunrise
    /// and sunset re-theme without a file change.
    theme_schedule: Option<(settings_model::day_schedule::Scheduled, bool)>,
    input: InputRuntime,

    #[cfg(feature = "model")]
    focus_history: HistoryRuntime,

    #[cfg(feature = "model")]
    collaboration: CollaborationRuntime,
    #[cfg(feature = "model")]
    local_input_capture: local_input_capture::LocalInputCapture,
    pub presence_ctx: PresenceCtx,
    /// The person this window acts for: who its actions are made by.
    local_author: tilo_actions::MemberId,
    #[cfg(feature = "model")]
    pub permission_ctx: PermissionContext,
    #[cfg(feature = "model")]
    /// [`crate::permissions::shared_revision`] as the last frame painted with it: what widgets paint
    /// follows permissions that can change with nothing in the tree marking it, so a frame draws again
    /// from cleared drawings after it moves.
    painted_permissions: u64,
    /// How many times the theme this window draws with was made again
    /// ([`Self::rebuild_theme`]), and the epoch the last frame drew with.
    /// A theme changes the colour of every pixel the tree paints, which no
    /// widget's damage describes: a frame that sees the epoch move repaints
    /// the whole surface, whatever the tree reports.
    theme_epoch: u64,
    frame_theme_epoch: u64,

    #[cfg(feature = "model")]
    presence: PresenceRuntime,

    accessibility: Box<AccessibilityRuntime>,

    /// Compared against [`crate::widget::Widget::fullscreen_intent`] each frame so a steady desired
    /// state issues one `set_fullscreen` call — see [`fullscreen_request_decision`].
    window: WindowRuntime,
    #[cfg(feature = "model")]
    committed_documents: Vec<(
        crate::collaboration::RouteSpec,
        crate::collaboration::CommittedDocument,
    )>,
    #[cfg(feature = "model")]
    autosave_generation: u64,
    #[cfg(feature = "model")]
    document_routes: crate::history_mode::routes::OwnerDocumentRoutes,

    actions: Box<ActionRuntime>,
    /// The window presents search itself (its palette), rather than a shell hosting the app.
    search_presented: bool,
    native_document_chrome: bool,
    #[cfg(feature = "model")]
    continuation: ContinuationRuntime,
    #[cfg(feature = "model")]
    /// A failed pre-terminal handoff must not re-enter directed effects.
    action_publication_failure: Option<String>,
    profiler: Option<crate::profiling::FrameProfiler>,
    native_input_probe: Option<Box<dyn Fn() -> serde_json::Value>>,

    /// Optional app → shell lifecycle channel for detachable surfaces.
    floating: Box<FloatingRuntime>,
}

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
    #[cfg(feature = "model")]
    collab_manager: CollaborationManager,
    #[cfg(feature = "model")]
    /// How the window takes up what `collab_manager`'s workspace sends it.
    delivery: WorkspaceDelivery,
    configured_theme: ConstructionValue<Theme>,
    text_renderer: ConstructionValue<TextRenderer>,
    palette_source: PaletteSource,
    watch_system_settings: bool,
    debug_mode: fn() -> bool,
    theme_store_stamp: fn() -> Option<crate::settings_source::SettingsStamp>,
    local_author: ConstructionValue<tilo_actions::MemberId>,
    presence_ctx: ConstructionValue<PresenceCtx>,
    screen_reader: fn() -> Option<crate::screen_reader::ScreenReader>,
    /// The person's colour before a host says it: the stored choice where
    /// this process may read the person's settings, `None` elsewhere.
    your_colour: fn() -> Option<crate::tokens::PresenceHue>,
    high_contrast: fn() -> bool,
    reduce_motion: fn() -> bool,
    text_scale: fn() -> f32,
    ui_scale: fn() -> f32,
    #[cfg(feature = "model")]
    pending_continuation_restore: fn() -> Option<crate::collaboration::ContinuationMetadata>,
    profiler: fn() -> Option<crate::profiling::FrameProfiler>,
    floating_surface_client: fn() -> Option<crate::floating_surface::FloatingSurfaceServiceClient>,
    #[cfg(feature = "model")]
    startup_presence_scope: fn() -> Option<PresenceKey>,
    surface_id: ConstructionValue<String>,
}

impl AppConstructionInputs {
    #[cfg(feature = "model")]
    fn native_component(
        inputs: crate::application::ComponentRunnerInputs,
        binding: &crate::bound_dispatch::CallbackBinding,
    ) -> Self {
        Self::component_with_services(
            inputs,
            binding,
            Self::production().with_manager(
                crate::collaboration::native_initial::take_manager()
                    .unwrap_or_else(|| CollaborationManager::for_resource(&binding.checkout)),
            ),
        )
    }

    #[cfg(feature = "model")]
    fn component_with_services(
        inputs: crate::application::ComponentRunnerInputs,
        binding: &crate::bound_dispatch::CallbackBinding,
        mut services: Self,
    ) -> Self {
        services.configured_theme = ConstructionValue::Ready(inputs.theme);
        services.text_renderer = ConstructionValue::Ready(inputs.text_renderer);
        services.palette_source = PaletteSource::Host;
        services.local_author = ConstructionValue::Ready(binding.author.clone());
        let mut presence = services.presence_ctx.resolve();
        presence.add_own_identity(binding.author.clone());
        services.presence_ctx = ConstructionValue::Ready(presence);
        services
    }

    #[cfg(feature = "model")]
    fn component(
        inputs: crate::application::ComponentRunnerInputs,
        binding: &crate::bound_dispatch::CallbackBinding,
    ) -> Self {
        let mut presence = PresenceCtx::empty();
        presence.set_own_identity([binding.author.clone()]);
        Self {
            control_domain: None,
            #[cfg(feature = "model")]
            collab_manager: CollaborationManager::disabled(),
            #[cfg(feature = "model")]
            // A host may put the window on a workspace after it is built.
            delivery: WorkspaceDelivery::LINKED,
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
            #[cfg(feature = "model")]
            pending_continuation_restore: || None,
            profiler: || None,
            floating_surface_client: || None,
            #[cfg(feature = "model")]
            startup_presence_scope: || None,
        }
    }

    /// What a window opened in the workspace is made with: [`Self::production`], joined to the
    /// workspace where Tilo's model is built in.
    fn workspace() -> Self {
        let inputs = Self::production();
        #[cfg(feature = "model")]
        let inputs = inputs.with_manager(CollaborationManager::new());
        inputs
    }

    #[cfg(feature = "model")]
    fn with_manager(mut self, collab_manager: CollaborationManager) -> Self {
        self.collab_manager = collab_manager;
        self
    }

    fn production() -> Self {
        // The screen reader below speaks through the host's services, so a
        // process that never installed any gets the native platform's.
        #[cfg(feature = "native")]
        crate::process::native::install_default_if_unset(None);
        #[cfg(feature = "native")]
        crate::emoji_catalogue::keep_uses_on_the_device();
        Self {
            control_domain: None,
            #[cfg(feature = "model")]
            collab_manager: CollaborationManager::disabled(),
            #[cfg(feature = "model")]
            delivery: WorkspaceDelivery::LINKED,
            configured_theme: ConstructionValue::Lazy(Theme::configured),
            text_renderer: ConstructionValue::Lazy(TextRenderer::new),
            palette_source: PaletteSource::Owned {
                renderer: TextRenderer::new,
                field: crate::widget::SearchField::new,
            },
            watch_system_settings: true,
            debug_mode: || {
                cfg!(debug_assertions) && crate::process::environment().diagnostics.debug_overlay
            },
            theme_store_stamp: crate::settings_source::stamp,
            local_author: ConstructionValue::Lazy(local_person),
            #[cfg(feature = "model")]
            presence_ctx: ConstructionValue::Lazy(App::initial_presence_context),
            #[cfg(not(feature = "model"))]
            presence_ctx: ConstructionValue::Lazy(PresenceCtx::empty),
            screen_reader: || {
                if crate::process::environment().accessibility.screen_reader
                    || crate::screen_reader::settings().enabled
                {
                    Some(App::configured_screen_reader())
                } else {
                    None
                }
            },
            // The colour a person is known by is their workspace identity's: Tilo's model's.
            #[cfg(feature = "model")]
            your_colour: crate::local_identity::own_colour,
            #[cfg(not(feature = "model"))]
            your_colour: || None,
            high_contrast: || crate::process::environment().accessibility.high_contrast,
            reduce_motion: || crate::process::environment().accessibility.reduce_motion,
            text_scale: || {
                crate::process::environment()
                    .accessibility
                    .text_scale
                    .unwrap_or(1.0)
            },
            ui_scale: || {
                crate::process::environment()
                    .accessibility
                    .ui_scale
                    .unwrap_or(1.0)
            },
            #[cfg(feature = "model")]
            pending_continuation_restore: crate::collaboration::continuation_restore_env,
            profiler: crate::profiling::FrameProfiler::from_env,
            floating_surface_client:
                crate::floating_surface::FloatingSurfaceServiceClient::for_window,
            #[cfg(feature = "model")]
            startup_presence_scope: crate::presence::local_workspace_key,
            surface_id: ConstructionValue::Lazy(crate::local_surface_id),
        }
    }

    #[cfg(any(test, all(feature = "native", feature = "test-support")))]
    fn controlled() -> Self {
        Self {
            control_domain: None,
            #[cfg(feature = "model")]
            collab_manager: CollaborationManager::disabled(),
            #[cfg(feature = "model")]
            // A host may put the window on a workspace after it is built.
            delivery: WorkspaceDelivery::LINKED,
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
                crate::permissions::MemberId::person("fixture-user")
            }),
            presence_ctx: ConstructionValue::Lazy(|| {
                let mut presence = PresenceCtx::empty();
                presence.set_own_identity([crate::permissions::MemberId::person("fixture-user")]);
                presence
            }),
            screen_reader: || None,
            your_colour: || None,
            high_contrast: || false,
            reduce_motion: || false,
            text_scale: || 1.0,
            ui_scale: || 1.0,
            #[cfg(feature = "model")]
            pending_continuation_restore: || None,
            profiler: || None,
            floating_surface_client: || None,
            #[cfg(feature = "model")]
            startup_presence_scope: || None,
            surface_id: ConstructionValue::Lazy(|| "fixture-surface".into()),
        }
    }
}

impl App {
    /// Install a read-only workload observer for scheduled native inputs.
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
    #[cfg(feature = "model")]
    pub fn from_component(
        component: crate::application::AppComponent,
        inputs: crate::application::ComponentRunnerInputs,
    ) -> Result<Self, crate::launch::LaunchError> {
        Self::from_component_using(component, inputs, AppConstructionInputs::component)
            .map(|app| *app)
    }

    /// Mount a checked component on an explicitly admitted collaboration manager.
    #[cfg(feature = "model")]
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
        .map(|app| *app)
    }

    /// Mount a standalone native component with the process's existing system services, including
    /// record-scoped collaboration and accessibility.
    #[cfg(feature = "model")]
    pub fn from_component_with_native_services(
        component: crate::application::AppComponent,
        inputs: crate::application::ComponentRunnerInputs,
    ) -> Result<Self, crate::launch::LaunchError> {
        Self::from_component_using(component, inputs, AppConstructionInputs::native_component)
            .map(|app| *app)
    }

    #[cfg(feature = "model")]
    fn from_component_using(
        mut component: crate::application::AppComponent,
        inputs: crate::application::ComponentRunnerInputs,
        construction: impl FnOnce(
            crate::application::ComponentRunnerInputs,
            &crate::bound_dispatch::CallbackBinding,
        ) -> AppConstructionInputs,
    ) -> Result<Box<Self>, crate::launch::LaunchError> {
        let scope =
            crate::collaboration::native_initial::take_component_scope().unwrap_or_default();
        let retirement = component.take_root_retirement();
        let mut assembly = retirement_assembly::ComponentAssemblyOwnership {
            component: None,
            scope: None,
            retirement,
        };
        let scope = assembly.scope.insert(scope);
        let component = assembly.component.insert(component);
        let _entry = scope.enter();
        let handle = scope.handle();
        if let Err(error) = component.mount(move |event| forward_component_wake(&handle, &event)) {
            return Err(component.setup_error(error));
        }
        let binding = component.callback_binding();
        let inputs = construction(inputs, &binding);
        if let Some(route) = inputs.collab_manager.requested_primary_route()
            && !binding.matches_route(route)
        {
            tracing::warn!(
                requested = ?route,
                opened = %binding.checkout.id.as_str(),
                "application binding does not match: the collaboration route asked for is not the record the window opened"
            );
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
        let mut root_ownership = retirement_assembly::AppAssemblyOwnership {
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
        let validation =
            retirement_assembly::ValidationRetirement(&root_ownership.retirement, true);
        if let Some(registration) = &history
            && let Err(error) = registration.validate_root(root.as_ref())
        {
            return Err(root
                .component
                .setup_error(crate::launch::LaunchError::Operation(error.to_string())));
        }
        validation.disarm();
        root_ownership.root = Some(root);
        let mut app = retirement_assembly::assemble(root_ownership);
        app.autofocus_on_structure_change = behaviour.autofocus_on_structure_change;
        app.appearance_reconcile = behaviour.appearance_reconcile;
        app.set_capture_protected(behaviour.capture_protected);
        app.floating.window_record = Some((
            binding.checkout.id.route().to_owned(),
            binding.checkout.kind.as_str().to_owned(),
        ));
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
            app.drop_native_document_chrome();
        }
        Ok(app)
    }

    /// Current SDK transport health, including fail-closed publication errors.
    #[cfg(feature = "model")]
    pub fn collaboration_is_connected(&self) -> bool {
        self.collaboration.manager.is_connected()
    }

    #[cfg(feature = "model")]
    pub(crate) fn native_component_presentation() -> crate::application::ComponentRunnerInputs {
        let accessibility = crate::process::environment().accessibility;
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

    /// The window's root widget: what a host or a test drives the window through when it looks at what
    /// the app mounted.
    pub fn root_widget_mut(&mut self) -> &mut dyn Widget {
        self.root_widget.as_mut()
    }

    /// The document the window shows, as a test reads it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn state_for_test(&mut self) -> &mut State {
        &mut self.state
    }

    /// The text shaping the window did since the last call: `(hits, misses, nanoseconds)`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn take_shape_stats_for_test(&mut self) -> (u64, u64, u128) {
        self.text_renderer.take_shape_stats()
    }

    /// Capture/close embedded components while the host still owns their trees.
    /// Recovery reports stay owned by this host until explicitly transferred.
    pub fn close_components(&mut self) {
        self.mark_semantic_tree_stale();
        #[cfg(feature = "model")]
        {
            self.retire_semantic_root();
        }
        self.control_domain.close();
        #[cfg(feature = "model")]
        {
            self.actions.bound_document_sync = None;
            self.component_close_reports
                .extend(crate::component_lifecycle::close_components(
                    self.root_widget.as_mut(),
                ));
            self.component_close_reports.extend(
                crate::component_lifecycle::take_component_close_reports(self.root_widget.as_mut()),
            );
        }
        self.bindings.clear();
        self.layout.node_map.clear();
    }
    #[cfg(feature = "native")]
    fn close_native_content(&mut self) {
        self.close_components();
        crate::widget::unmount_native_views(self.root_widget.as_mut());
    }

    #[cfg(feature = "model")]
    /// Take what the tree's components reported as they closed.
    #[cfg(feature = "model")]
    fn collect_component_close_reports(&mut self) {
        self.component_close_reports.extend(
            crate::component_lifecycle::take_component_close_reports(self.root_widget.as_mut()),
        );
    }

    #[cfg(feature = "model")]
    pub fn component_close_reports(&self) -> &[crate::application::ComponentCloseReport] {
        &self.component_close_reports
    }
    /// Transfer pending recovery to another owner; ordinary tile destruction cannot lose it.
    #[cfg(feature = "model")]
    pub fn take_component_close_reports(
        &mut self,
    ) -> Vec<crate::application::ComponentCloseReport> {
        self.collect_component_close_reports();
        std::mem::take(&mut self.component_close_reports)
    }

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

    #[cfg(feature = "model")]
    fn initial_presence_context() -> PresenceCtx {
        let mut context = PresenceCtx::empty();
        let own = crate::collaboration::local_user_id();
        context.set_own_identity(std::iter::once(own.clone()));
        if let Some(hue) = crate::local_identity::own_colour() {
            context.set_presence_hue(own, hue);
        }
        context
    }

    /// Detached convenience for explicitly supplied managed roots. Production
    /// launchers construct under their installed wake scope, then install History.
    /// # Errors
    /// Refuses invalid or mismatched managed owner registrations.
    #[cfg(feature = "model")]
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
    #[cfg(all(
        any(test, all(feature = "native", feature = "test-support")),
        feature = "model"
    ))]
    pub fn from_managed_root_with_controlled_inputs(
        state: State,
        root: crate::history_mode::ManagedRoot,
    ) -> Result<Self, crate::history_mode::HistoryRegistrationError> {
        Self::from_managed_root_with_constructor(state, root, |state, widget| {
            let _entry = crate::wake::WakeScope::enter_detached();
            *Self::with_parts(None, state, widget, AppConstructionInputs::controlled())
        })
    }

    #[cfg(feature = "model")]
    fn from_managed_root_with_constructor(
        state: State,
        root: crate::history_mode::ManagedRoot,
        construct: impl FnOnce(State, Box<dyn Widget>) -> Self,
    ) -> Result<Self, crate::history_mode::HistoryRegistrationError> {
        root.history.validate_root(root.widget.as_ref())?;
        let mut app = construct(state, root.widget);
        app.install_history(root.history)?;
        Ok(app)
    }

    /// Registered producer inventory; legacy private ownership remains unqualified.
    pub fn control_coverage(&self) -> crate::ControlCoverage {
        self.control_domain.coverage()
    }

    #[cfg(feature = "model")]
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
        self.layout.walks = runtime::TreeWalks::default();
        if let Some(history) = &mut self.history {
            history.set_control_domain(&self.control_domain);
        }
        #[cfg(any(test, feature = "test-support"))]
        {
            self.controlled_installed_history_definition = Some(controlled_definition);
        }
        // The tree was just synced against the record, which is the state
        // pass a window owes before it first shows: nothing is pending until
        // the model moves from here (or init_layout places focus), so a
        // window whose frame lays the new tree out leaves no pass behind for
        // an idle window to run.
        self.needs_sync = false;
        self.init_layout();
        tilo_startup_metrics::span("history-validate", || self.validate_history_registration())
    }

    /// Construct an explicitly detached app for headless or test execution.
    pub fn new(state: State, root_widget: Box<dyn Widget>) -> Self {
        let _entry = crate::wake::WakeScope::enter_detached();
        *Self::with_parts(None, state, root_widget, AppConstructionInputs::workspace())
    }

    /// Construct an explicitly detached app with a connected collaboration manager for headless or test
    /// execution.
    #[cfg(feature = "model")]
    pub fn with_collaboration_manager(
        state: State,
        root_widget: Box<dyn Widget>,
        collab_manager: CollaborationManager,
    ) -> Self {
        *Self::with_parts(
            None,
            state,
            root_widget,
            AppConstructionInputs::production().with_manager(collab_manager),
        )
    }

    /// Construct a live app whose root factory captures a fresh owner.
    pub fn from_factory(state: State, factory: impl FnOnce() -> Box<dyn Widget>) -> Self {
        Self::from_factory_with_inputs(state, factory, AppConstructionInputs::workspace)
    }

    /// A window showing `factory`'s root that joins no workspace: what a launch shows while the record
    /// it opens arrives with the workspace's join ([`crate::launch::window`]).
    #[cfg(feature = "native")]
    pub(crate) fn without_workspace(factory: impl FnOnce() -> Box<dyn Widget>) -> Self {
        Self::from_factory_with_inputs(State::new(), factory, AppConstructionInputs::production)
    }

    fn from_factory_with_inputs(
        state: State,
        factory: impl FnOnce() -> Box<dyn Widget>,
        inputs: impl FnOnce() -> AppConstructionInputs,
    ) -> Self {
        let scope = crate::wake::WakeScope::new();
        let _entry = scope.enter();
        let domain = crate::control_domain::ControlDomain::outer();
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

    /// Fallible form of [`Self::from_factory`].
    pub fn try_from_factory<E>(
        state: State,
        factory: impl FnOnce() -> Result<Box<dyn Widget>, E>,
    ) -> Result<Self, E> {
        let scope = crate::wake::WakeScope::new();
        let _entry = scope.enter();
        let domain = crate::control_domain::ControlDomain::outer();
        let root = factory()?;
        Ok(Self::from_scoped_parts_with_ownership(
            scope, state, root, domain,
        ))
    }

    pub(crate) fn from_scoped_parts_with_ownership(
        scope: crate::WakeScope,
        state: State,
        root: Box<dyn Widget>,
        domain: crate::ControlDomain,
    ) -> Self {
        let mut inputs = AppConstructionInputs::production();
        #[cfg(feature = "model")]
        {
            inputs = inputs.with_manager(
                crate::collaboration::native_initial::take_manager().unwrap_or_default(),
            );
        }
        inputs.control_domain = Some(domain);
        *Self::with_parts(Some(scope), state, root, inputs)
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
        Self::from_scoped_parts_with_inputs(
            wake_scope,
            state,
            root_widget,
            AppConstructionInputs::workspace,
        )
    }

    fn from_scoped_parts_with_inputs(
        wake_scope: crate::wake::WakeScope,
        state: State,
        root_widget: Box<dyn Widget>,
        inputs: impl FnOnce() -> AppConstructionInputs,
    ) -> Self {
        let _entry = wake_scope.enter();
        *Self::with_parts(Some(wake_scope), state, root_widget, inputs())
    }

    /// An app showing `root_widget` under its own wake scope, as an app embedded in a window is,
    /// with the clock, fonts and settings a test controls.
    #[cfg(any(test, all(feature = "native", feature = "test-support")))]
    pub fn from_scoped_parts_with_controlled_inputs(
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

    /// An app showing `root_widget` over `state`, with the clock, fonts and settings a test
    /// controls.
    #[cfg(any(test, all(feature = "native", feature = "test-support")))]
    pub fn new_with_controlled_inputs(state: State, root_widget: Box<dyn Widget>) -> Self {
        let _entry = crate::wake::WakeScope::enter_detached();
        *Self::with_parts(
            None,
            state,
            root_widget,
            AppConstructionInputs::controlled(),
        )
    }

    /// An app for a surface with no display ([`driver::Driver`]): drawn with the caller's theme and
    /// fonts, as the caller's person, with the process's settings and services left alone.
    pub(crate) fn displayless(
        state: State,
        root_widget: Box<dyn Widget>,
        inputs: driver::DriverInputs,
    ) -> Box<Self> {
        let driver::DriverInputs {
            theme,
            mut text,
            author,
        } = inputs;
        // The theme is composed as the caller hands it over: its scale and its comforts are the
        // window's, not ones the person's settings would put over it.
        let (text_scale, ui_scale, reduce_motion) =
            (theme.text_scale, theme.ui_scale, theme.reduce_motion);
        text.set_text_scale(theme.text_scale);
        text.apply_typography(&theme.tokens.typography);
        let mut presence = PresenceCtx::empty();
        presence.set_own_identity([author.clone()]);
        // A wake scope of its own, as a window's app has: work the app starts, and a source outside
        // it that a host hands the app's wake ([`driver::Driver::wake_handle`]), wakes it, and the
        // frame that wake brings takes the change up.
        let scope = {
            let _outside = crate::wake::WakeScope::enter_detached();
            crate::wake::WakeScope::new()
        };
        let _entry = scope.enter();
        // The window's controls, as a window of its own attaches them: what they bind is what
        // the app publishes and what a reader operates.
        let domain = crate::control_domain::ControlDomain::outer();
        let mut app = Self::with_parts(
            Some(scope),
            state,
            root_widget,
            AppConstructionInputs {
                control_domain: Some(domain),
                #[cfg(feature = "model")]
                collab_manager: CollaborationManager::disabled(),
                #[cfg(feature = "model")]
                delivery: WorkspaceDelivery::UNLINKED,
                configured_theme: ConstructionValue::Ready(theme),
                text_renderer: ConstructionValue::Ready(text),
                palette_source: PaletteSource::Owned {
                    renderer: TextRenderer::new,
                    field: crate::widget::SearchField::new,
                },
                watch_system_settings: false,
                debug_mode: || false,
                theme_store_stamp: || None,
                local_author: ConstructionValue::Ready(author),
                presence_ctx: ConstructionValue::Ready(presence),
                screen_reader: || None,
                your_colour: || None,
                high_contrast: || false,
                reduce_motion: || false,
                text_scale: || 1.0,
                ui_scale: || 1.0,
                #[cfg(feature = "model")]
                pending_continuation_restore: || None,
                profiler: || None,
                floating_surface_client: || None,
                #[cfg(feature = "model")]
                startup_presence_scope: || None,
                surface_id: ConstructionValue::Lazy(|| "displayless-surface".into()),
            },
        );
        // The theme is composed as the caller handed it over: its scale and its comforts stand on
        // the window, where the construction read the person's settings (none here) over it.
        app.accessibility.text_scale = text_scale;
        app.accessibility.ui_scale = ui_scale;
        app.accessibility.a11y.reduce_motion = reduce_motion;
        app.rebuild_theme();
        #[cfg(feature = "model")]
        {
            app.set_permission_ctx(PermissionContext::permissive());
        }
        app
    }

    /// Whether the shared permissions moved since the last frame painted them: the drawings the tree
    /// has were made from the ones before.
    #[cfg_attr(
        not(feature = "native"),
        allow(
            dead_code,
            reason = "read by the native draw and by embedded apps, which this feature set leaves out"
        )
    )]
    /// When Tilo's model next needs the loop to run: a local input it could not yet report
    /// retries then.
    #[cfg_attr(
        not(feature = "native"),
        allow(dead_code, reason = "read by the native event loop")
    )]
    pub(crate) fn model_deadline(&self) -> Option<crate::time::Instant> {
        #[cfg(feature = "model")]
        let at = self.local_input_capture_deadline();
        #[cfg(not(feature = "model"))]
        let at = None;
        at
    }

    pub(crate) fn permissions_moved(&self) -> bool {
        #[cfg(feature = "model")]
        let moved = self.painted_permissions != crate::permissions::shared_revision();
        // Without the model nothing grants or takes away a permission while the window runs.
        #[cfg(not(feature = "model"))]
        let moved = false;
        moved
    }

    /// Whether the theme changed since the last frame drew with it: the frame
    /// that asks repaints its whole surface, because the theme decides the
    /// colour of every pixel the tree paints and no widget reports that in its
    /// damage.
    #[cfg_attr(
        not(feature = "native"),
        allow(
            dead_code,
            reason = "read by the native draw and by hosted surfaces, which this feature set leaves out"
        )
    )]
    pub(crate) fn theme_changed(&self) -> bool {
        self.frame_theme_epoch != self.theme_epoch
    }

    /// Whether the tree was rebuilt or remounted since the last frame drew it:
    /// the frame that asks repaints its whole surface, because a build between
    /// frames (a state tick's, a sync's) leaves every pixel as the tree before
    /// it painted it and no widget reports that.
    #[cfg_attr(
        not(feature = "native"),
        allow(
            dead_code,
            reason = "read by the native draw and by hosted surfaces, which this feature set leaves out"
        )
    )]
    pub(crate) fn rebuilt_since_frame(&self) -> bool {
        self.layout.drawn != (self.layout.rebuilds, self.layout.remounts)
    }

    /// A frame drew the tree as it stands: what came after the last one is
    /// painted, and a rebuild or a theme made again after this frame repaints
    /// whole on its own account.
    pub(crate) fn note_frame_drawn(&mut self) {
        self.frame_theme_epoch = self.theme_epoch;
        self.layout.drawn = (self.layout.rebuilds, self.layout.remounts);
    }

    pub fn enter_wake_scope(&self) -> crate::wake::WakeScopeGuard {
        self.wake_scope.as_ref().map_or_else(
            crate::wake::WakeScope::enter_detached,
            crate::wake::WakeScope::enter,
        )
    }

    #[cfg(feature = "model")]
    fn retire_semantic_root(&self) {
        if let Some(retirement) = &self.retirement {
            retirement.retire();
        }
    }

    fn close_wake_scope(&mut self) {
        #[cfg(feature = "model")]
        {
            self.send_held_answers(true);
        }
        #[cfg(feature = "model")]
        {
            self.retire_semantic_root();
        }
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
    ) -> Box<Self> {
        retirement_assembly::assemble(retirement_assembly::AppAssemblyOwnership {
            #[cfg(feature = "model")]
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
        self.theme_epoch = self.theme_epoch.wrapping_add(1);
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
                        self.accessibility.ui_scale * self.accessibility.zoom.factor(),
                    )
                    .with_reduce_motion(self.accessibility.a11y.reduce_motion);
                // The person scale as the theme took it (the zoom is
                // kept apart, so taking it out never drifts).
                self.accessibility.ui_scale = if self.accessibility.ui_scale.is_finite() {
                    self.accessibility.ui_scale.clamp(0.5, 3.0)
                } else {
                    1.0
                };
                theme
            }
            PaletteSource::Owned { .. } => {
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
                // The owned base is a theme as its caller composed it, so the person's scales are
                // taken as factors over it: a theme handed over already at its own scale is kept
                // as it stands, and one at the plain scale takes the settings whole.
                base.with_component_scales(
                    self.accessibility.text_scale,
                    self.accessibility.ui_scale * self.accessibility.zoom.factor(),
                )
                .with_reduce_motion(self.accessibility.a11y.reduce_motion)
            }
        };
        if crate::window_toolbar::floats() {
            self.theme = self.theme.clone().as_card();
        }
        self.text_renderer
            .apply_typography(&self.theme.tokens.typography);
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

    #[cfg(feature = "model")]
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
            #[cfg(feature = "model")]
            controls: std::collections::HashMap::new(),
        });
        self.actions.local_resource_cache.borrow_mut().take();
        key
    }

    #[cfg(feature = "model")]
    fn ensure_local_control_cache(&self) -> (ResourceCacheKey, u64) {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        fn collect_addresses(
            resources: &[tilo_actions::ResourceSpec],
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

    #[cfg(feature = "model")]
    fn directory_explicit_visibilities(
        &self,
        resources: &[tilo_actions::ResourceSpec],
    ) -> std::collections::HashMap<String, tilo_actions::ActionAddress> {
        fn ids<'a>(resources: &'a [tilo_actions::ResourceSpec], out: &mut Vec<&'a str>) {
            for resource in resources {
                out.push(resource.id.as_str());
                ids(&resource.children, out);
            }
        }
        let mut all = Vec::new();
        ids(resources, &mut all);
        // Every resource of the surface is resolved through the component that published it, one
        // per row of a long collection: its scope is held across the lookups.
        let mut held = crate::bound_dispatch::HeldScope::default();
        let targets: Vec<_> = all
            .iter()
            .map(|id| {
                self.actions
                    .bound_publication
                    .target_within(&tilo_actions::ActionAddress::new(*id, ""), &mut held)
                    .ok()
                    .flatten()
            })
            .collect();
        let wanted: Vec<_> = all
            .iter()
            .zip(&targets)
            .map(|(id, target)| {
                (
                    target.as_ref().map(|target| target.binding.clone()),
                    target
                        .as_ref()
                        .map_or(*id, |target| target.address.address.resource.as_str())
                        .to_owned(),
                )
            })
            .collect();
        let found = crate::bound_dispatch::publication_visibilities(
            self.root_widget.as_ref(),
            &self.state.doc,
            &wanted,
        );
        // Keyed by the resource as this publication names it; the address is
        // the resource's own, as the directory offers and resolves it.
        all.into_iter()
            .zip(found)
            .filter_map(|(id, address)| address.map(|address| (id.to_owned(), address)))
            .collect()
    }

    #[cfg(feature = "model")]
    fn directory_explicit_visibility(
        &self,
        resource: &tilo_actions::ResourceSpec,
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
    }

    #[cfg(feature = "model")]
    /// The descriptor of `action` of `resource`, which the directory names `name`.
    fn directory_descriptor(
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
        name: &str,
    ) -> tilo_actions::ActionDescriptor {
        tilo_actions::ActionDescriptor {
            did: action.did.clone(),
            state: action.state.clone(),
            semantic_identity: resource.semantic_identity.clone(),
            address: tilo_actions::ActionAddress::new(name, &action.id),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            action_label: action.label.clone(),
            keywords: action.keywords.clone(),
            icon: action.icon.clone(),
            description: action.description.clone(),
            locus: action.locus,
            kind: action.kind,
            contract: action.contract.clone(),
            input: match action.input() {
                tilo_actions::ActionInput::Immediate(_)
                | tilo_actions::ActionInput::Deferred { .. } => tilo_actions::InputSpec::None,
                tilo_actions::ActionInput::Prompted {
                    prompt,
                    placeholder,
                    collects,
                } => tilo_actions::InputSpec::Text {
                    prompt: prompt.to_owned(),
                    placeholder: placeholder.to_owned(),
                    collects,
                },
            },
            params: action.params().to_vec(),
            view: tilo_actions::ResourceViewDescriptor::declared_for(resource, &action.id),
        }
    }
}

impl App {
    #[cfg(feature = "model")]
    /// Publish what this window's search finds to the session's persistent
    /// search index, over the certified primary route: only what changed
    /// since the last publication on that route, and nothing while the
    /// source stays at the same generation. The session keeps it after the
    /// app closes, so the app's content stays findable whether or not it
    /// runs.
    ///
    /// Any acknowledged primary route has it, whatever keeps the workspace's
    /// records: the broker files it under the app, workspace and record that
    /// route certifies, so it needs no private window binding, which a workspace
    /// whose records are sealed channels never hands out.
    fn publish_search_index(&mut self) {
        let _phase = tilo_actions::profiling::Phase::start("ui.publish_search_index");
        let Some(state) = crate::search::capability::search_index_state(self.root_widget.as_ref())
        else {
            return;
        };
        let Some(observation) = self.collaboration.manager.primary_route().cloned() else {
            return;
        };
        if self.continuation.search_index_route.as_ref() != Some(&observation) {
            self.continuation.search_index.reset();
            self.continuation.search_index_route = Some(observation.clone());
        }
        if !self.continuation.search_index.wants(state) {
            return;
        }
        let entries = crate::search::capability::search_index_entries(self.root_widget.as_ref());
        let deltas = self.continuation.search_index.publish(state, entries);
        for delta in deltas {
            let Some(delta_json) = delta.encode() else {
                continue;
            };
            self.collaboration
                .manager
                .send(crate::collaboration::CollabRequest::PublishSearchIndex { delta_json });
        }
    }

    #[cfg(feature = "model")]
    fn private_continuity_route(&mut self) -> Option<crate::collaboration::RouteSpec> {
        let observation = self.collaboration.manager.primary_route()?.clone();
        let instance = self.collaboration.manager.private_window_instance()?;
        if self.continuation.last_private_route.as_ref() != Some(&observation)
            || self.continuation.private_instance != Some(instance)
        {
            self.continuation.last_published = None;
            self.continuation.publication_initialized = false;
            self.continuation.last_private_route = Some(observation.clone());
            self.continuation.private_instance = Some(instance);
        }
        Some(observation.route().clone())
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
    #[cfg(feature = "native")]
    fn reconcile_fullscreen(&mut self, window: &Window) -> bool {
        let mut changed = false;

        let want = self.root_widget.fullscreen_intent();
        if let Some(new_state) = fullscreen_request_decision(self.window.fullscreen_requested, want)
        {
            if new_state {
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
        // No frame is drawn here, so none waits for a size asked for.
        let _ = self.reconcile_minimum_size(window, false);

        changed
    }

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
        self.layout.request_rebuild();
        true
    }

    /// What the host said about where and for whom this window is shown.
    #[must_use]
    pub fn window_appearance(&self) -> crate::theme::WindowAppearance {
        self.accessibility.window
    }

    /// The shape of this window's surface, laid out over `window` (logical) and drawn with `transform`:
    /// rounded at the radius the shell draws the window with, square while it is fullscreen or drawn
    /// edge to edge.
    #[must_use]
    pub fn window_surface(
        &self,
        window: vello::kurbo::Rect,
        transform: vello::kurbo::Affine,
    ) -> Option<crate::theme::SurfaceShape> {
        let radius = if self.window.fullscreen_active {
            0.0
        } else {
            f64::from(self.accessibility.window.corner_radius?)
        };
        Some(crate::theme::SurfaceShape::rounded(window, radius).transformed(transform))
    }

    /// What a standalone app read from its host OS, through the same API as
    /// a settings pane's intent. The caller applies each host change once, so
    /// a contrast toggle made in the app has until the host changes again.
    #[cfg(feature = "native")]
    pub(crate) fn apply_host_appearance(
        &mut self,
        host: crate::host_appearance::HostAppearance,
    ) -> bool {
        if crate::host_appearance::session_document_owns_appearance() {
            return false;
        }
        // The host's light or dark is also what an appearance the person left open draws with
        // from now on, so the theme applied here and any "in use" the app shows are one reading.
        crate::theme::install_host_scheme(
            host.scheme
                .map(|scheme| scheme == crate::host_appearance::Scheme::Dark),
        );
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
            self.layout.request_rebuild();
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
            self.layout.request_rebuild();
            return true;
        }
        // The debounce spares a settings file a stat on every pass. A source
        // the host provides (the shell's records over the app's channel)
        // says its revision in one atomic read and wakes this loop when it
        // rings, so its change arrives on the wake it caused rather than on
        // some later input after the debounce.
        if crate::settings_source::installed_source().is_some()
            && let Some(due) = now.checked_sub(Duration::from_millis(750))
        {
            self.last_theme_store_check = due;
        }
        self.reconcile_configured_theme_with(
            crate::time::now(),
            crate::settings_source::stamp,
            Self::apply_configured_theme,
        )
    }

    #[cfg(feature = "native")]
    fn reconcile_configured_theme_with(
        &mut self,
        now: Instant,
        stamp: impl FnOnce() -> Option<crate::settings_source::SettingsStamp>,
        apply: impl FnOnce(&mut Self),
    ) -> bool {
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
        self.layout.request_rebuild();
        true
    }

    #[cfg(feature = "native")]
    fn apply_configured_theme(&mut self) {
        crate::format::region::refresh();
        crate::i18n::follow_configured_language();
        self.accessibility.base_theme = self
            .floating
            .host_theme
            .clone()
            .unwrap_or_else(Theme::configured);
        let comforts = Theme::configured_comforts();
        if let Some(on) = comforts.high_contrast {
            self.accessibility.a11y.high_contrast = on;
        }
        if let Some(on) = comforts.reduce_motion {
            self.accessibility.a11y.reduce_motion = on;
        }
        if let Some(scale) = comforts.text_scale {
            self.accessibility.text_scale = scale;
        }
        if let Some(scale) = comforts.ui_scale {
            self.accessibility.ui_scale = scale;
        }
        self.theme_schedule = Theme::automatic_schedule_from(&crate::settings_source::load())
            .map(|schedule| (schedule, schedule.is_night(crate::theme::local_moment())));
        let settings = crate::screen_reader::settings();
        self.set_screen_reader_enabled(
            settings.enabled || crate::process::environment().accessibility.screen_reader,
        );
        if let Some(reader) = self.accessibility.screen_reader.as_mut() {
            reader.configure(&settings);
        }
        self.accessibility.a11y.screen_reader = self.accessibility.screen_reader.is_some();
    }

    fn resolve_cursor(&self, point: Point) -> Option<CursorIcon> {
        #[cfg(feature = "model")]
        if self.report_pointing.is_some() {
            // The window is waiting for the person to point at a thing.
            return Some(CursorIcon::Crosshair);
        }
        self.tree().cursor_pick_at(point).icon()
    }
}

impl App {
    #[cfg(feature = "model")]
    fn collab_event_may_change_action_surface(event: &CollabEvent) -> bool {
        !matches!(
            event,
            CollabEvent::FollowChanged { .. }
                | CollabEvent::RemoteCursorMoved { .. }
                | CollabEvent::RemotePresenceUpdate { .. }
        )
    }
}

/// Hand a component's wake on to the App that runs it, as it was raised: a
/// model-dirty wake stays one, and an event-queue notification (a directory
/// request, a reply) stays one, whose consumer decides whether the model
/// changed. Forwarding every event as model-dirty made the App sync and
/// invalidate its directory on each, so a directory continuation it had just
/// issued was gone before it was asked for, and a hosted app's listing never
/// settled.
pub(crate) fn forward_component_wake(
    handle: &crate::wake::WakeHandle,
    event: &crate::wake::WakeEvent,
) {
    if event.requires_sync() {
        handle.wake();
    } else {
        handle.notify_events();
    }
}

impl App {
    #[doc(hidden)]
    pub fn state_mut_for_test(&mut self) -> &mut crate::state::State {
        self.invalidate_widget_resource_cache();
        &mut self.state
    }

    /// Replace the framework view inside the windows this process already has.
    ///
    /// Returns whether the running root accepted it. A root that is not the
    /// framework host leaves the current view in place.
    #[cfg(feature = "native")]
    pub(crate) fn install_reloaded_view(&mut self, root: DevRoot) -> bool {
        // Only Tilo's model hosts the framework view.
        #[cfg(not(feature = "model"))]
        let installed = {
            let _ = root;
            false
        };
        #[cfg(feature = "model")]
        let installed = self.install_reloaded_component_view(root);
        installed
    }

    #[cfg(all(feature = "native", feature = "model"))]
    fn install_reloaded_component_view(&mut self, root: DevRoot) -> bool {
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

    /// Lay the root out in a node of the host's own: `host_style` styles it (its size is the
    /// viewport's) and `content_height` lays it out against unbounded height, as a scrolling host
    /// lays the document inside it. A host that gives neither lays the root out at the viewport,
    /// the default.
    pub(crate) fn frame_root_in(&mut self, host_style: Option<taffy::Style>, content_height: bool) {
        self.layout.host_root_style = host_style;
        self.layout.content_height = content_height;
        if self.layout.hosts_root() {
            self.layout.request_rebuild();
        }
    }

    /// Headless and dev-reload startup.
    pub(crate) fn init_layout_with_dev_view(&mut self, dev_view: Option<crate::view::View>) {
        let _entry = self.enter_wake_scope();
        tilo_startup_metrics::span("init-layout-actions", || {
            self.refresh_control_action_availability();
        });
        #[cfg(feature = "model")]
        {
            self.collect_component_close_reports();
        }
        self.bindings.clear();
        self.layout.node_map.clear();
        // The map built now describes the tree as it is: a replacement the
        // widgets reported before is part of it, not a change after it.
        self.take_composition_dirty();
        self.layout.remount = false;
        tilo_startup_metrics::span("init-layout-history", || self.build_history_presentation());
        tilo_startup_metrics::span("init-layout-focus-order", || {
            self.update_focusable_order();
        });
        #[cfg(feature = "model")]
        let dev_view = self.continuation.pending_view.take().or(dev_view);
        if let Some(view) = dev_view {
            <Self as crate::view::FreezeThaw>::restore_view(self, &view);
        }
        if self.autofocus_first && self.focused_node.is_none() {
            if let Some(id) = self.initial_live_focus_target() {
                self.set_focused_node(Some(id));
            }
        }
        #[cfg(feature = "model")]
        {
            self.focus_history.stable_focus_idx = self
                .focused_node
                .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
            self.focus_history.stable_metadata = self.focused_node.and_then(|id| {
                self.inspect_node(id, |widget| crate::widget::undo_points::get_state(widget))
                    .ok()
                    .flatten()
            });
        }
    }

    /// Exposed so headless tests can drive modal open/close without a winit loop.
    #[doc(hidden)]
    pub fn refresh_focus_order(&mut self) {
        let _entry = self.enter_wake_scope();
        self.sync_model(true);
        self.update_focusable_order();
    }

    /// The root's structure flag, and framework History's: it is composed
    /// as a separate root, so its replaced controls are not the app's.
    #[track_caller]
    pub(crate) fn take_composition_dirty(&mut self) -> bool {
        let root = self.take_root_structure_dirty();
        #[cfg(feature = "model")]
        let history = self
            .history
            .as_mut()
            .is_some_and(|history| history.take_structure_dirty());
        #[cfg(not(feature = "model"))]
        let history = false;
        if history {
            self.layout.rebuild_described = false;
        }
        root || history
    }

    /// Whether the root's widgets changed their structure since the last call; a change they do not
    /// describe ([`crate::widget::StructureChange::Rebuild`]) leaves the rebuild that follows
    /// undescribed.
    #[track_caller]
    pub(crate) fn take_root_structure_dirty(&mut self) -> bool {
        let change = self.root_widget.take_structure_change();
        if change == crate::widget::StructureChange::Rebuild {
            self.layout.rebuild_described = false;
            #[cfg(test)]
            {
                self.layout.undescribed_by = Some(std::panic::Location::caller());
            }
        }
        // A widget that replaced children without saying so is found here.
        let stale = self.layout_maps_stale();
        if change != crate::widget::StructureChange::None || stale {
            // The resources and controls declared are the tree's.
            self.invalidate_widget_resource_cache();
        }
        if change == crate::widget::StructureChange::Children && !stale {
            self.layout.remount = true;
            self.layout.layout_dirty = true;
            return false;
        }
        change.rebuilds() || stale
    }

    /// How many times the layout tree was rebuilt, and how many times widgets changed it in place: a
    /// test asks whether a change kept the tree.
    #[doc(hidden)]
    pub fn layout_tree_changes(&self) -> (u64, u64) {
        (self.layout.rebuilds, self.layout.remounts)
    }

    /// When [`Self::autofocus_on_structure_change`] is set, restores the
    /// focused widget's caret/selection only if focus returns to the SAME
    /// focusable ordinal after rebuild (never onto a different field);
    /// without this an unrelated rebuild snaps the caret to offset 0.
    /// [`Widget::take_focus_reset`] instead jumps to the first focusable
    /// with no caret restore, since the field legitimately changed.
    #[doc(hidden)]
    pub fn rebuild_structure(&mut self) {
        self.rebuild_structure_placing_focus();
    }

    fn answer_name(&self, node: NodeId) -> Option<String> {
        let mut name = None;
        // SAFETY: the caller owns this tree; nothing borrows the widget.
        let _ = unsafe {
            crate::widget::bound_nodes::inspect_bound_node(
                &self.bindings,
                &self.layout.node_map,
                node,
                |widget| name = widget.test_id().map(str::to_owned),
            )
        };
        name
    }

    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        let mut answers = false;
        // SAFETY: the caller owns this tree; nothing borrows the widget.
        let _ = unsafe {
            crate::widget::bound_nodes::inspect_bound_node(
                &self.bindings,
                &self.layout.node_map,
                node,
                |widget| answers = widget.test_id() == Some(name),
            )
        };
        answers
    }

    fn unique_answering(&self, name: &str) -> Option<NodeId> {
        let mut answering = self
            .focusable_order
            .iter()
            .copied()
            .filter(|&node| self.answers_to(node, name));
        let single = answering.next()?;
        answering.next().is_none().then_some(single)
    }

    /// [`Self::rebuild_structure`], keeping the person's place in the tree it builds. A widget
    /// that asks for the keyboard is answered once the tree it asked in is laid out
    /// ([`Self::apply_focus_request`] from the host's layout pass), where a control's request
    /// and the tree it names both stand.
    pub(crate) fn rebuild_structure_placing_focus(&mut self) {
        let _entry = self.enter_wake_scope();
        let _phase = tilo_actions::profiling::Phase::start("ui.rebuild_structure");
        // The header the rebuild builds hears whether the window's bar
        // presents search, so it never shows a second search control.
        self.offer_window_bar_to_header();
        self.root_widget
            .set_interaction_environment(self.input.interaction);
        let reported = self.take_composition_dirty();
        let topology_changed = self.layout.structure_dirty || reported;
        self.layout.frame_rebuilds_described &= self.layout.rebuild_described;
        self.layout.rebuild_described = true;
        let (prev_focus_idx, prev_focus_state, focus_reset) = if self.autofocus_on_structure_change
        {
            let idx = self
                .focused_node
                .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
            let state = if topology_changed {
                None
            } else {
                self.focused_node.and_then(|id| {
                    self.inspect_node(id, |widget| crate::widget::undo_points::get_state(widget))
                        .ok()
                        .flatten()
                })
            };
            (idx, state, self.root_widget.take_focus_reset())
        } else {
            (None, None, false)
        };

        // A modal has the keyboard where the person put it: what it shows
        // changing under their hands, a countdown or a line of status, is not
        // them leaving their place. Its answer is remembered by what it is
        // and, failing that, by where in the answers it stood: a control that
        // was told it lost focus while the tree was replaced still is in
        // the same place after it is built again.
        let trapped_focus = self
            .accessibility
            .focus_trap_active
            .then(|| {
                let at = self
                    .focused_node
                    .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
                // Read from the live tree, never through the layout maps: a
                // rebuild can follow widgets replaced since they were made.
                let identity = crate::widget::layout::CompositionFocus::capture_live(
                    self.root_widget.as_ref(),
                );
                let live = self.focused_node.and_then(|id| self.answer_name(id));
                // The answer the person was on is not in this tree any more,
                // replaced along with everything the sheet drew: the name it
                // went by is all that is left of where they were, and it finds
                // that answer again wherever the new tree put it. While the
                // tree still has the answer, its name at its place
                // decides, as it always has.
                let remembered = live.is_none().then(|| self.focus_anchor.clone()).flatten();
                (at, identity, live, remembered)
            })
            .filter(|(at, identity, live, remembered)| {
                at.is_some() || identity.is_some() || live.is_some() || remembered.is_some()
            });
        // The keyboard was on the answer the trap itself chose, not on one the person put it on:
        // the build below reads the order the modal shows now and seats it there, rather than
        // restoring an answer that was only the first control laid out at the time.
        let trap_entry_provisional = self.accessibility.focus_trap_active
            && self.focused_node == self.accessibility.focus_trap_entry;
        // NodeId values can be reused by the new tree. Clear the old handle
        // without dispatching through its potentially invalid pointer, so a
        // replacement control at the same id still receives set_focus(true).
        self.focused_node = None;
        self.layout.rebuilds = self.layout.rebuilds.wrapping_add(1);
        self.layout.remount = false;
        self.input.pointer_replay.mark_unsettled();
        self.layout.taffy = TaffyTree::new();
        self.layout.host_node = None;
        self.layout.host_child = None;
        self.layout.unsolved = true;
        #[cfg(feature = "model")]
        {
            self.collect_component_close_reports();
        }
        self.bindings.clear();
        self.layout.node_map.clear();
        self.layout.adopted.clear();
        #[cfg(test)]
        let probe = crate::profiling::frame_probe::span("rebuild.build");
        self.build_history_presentation();
        self.root_widget
            .set_interaction_environment(self.input.interaction);
        #[cfg(test)]
        drop(probe);
        self.layout.structure_dirty = false;
        self.layout.layout_dirty = true;
        // A control the build just made may say it holds the keyboard: a form rebuilt around the
        // field the person is typing in builds that field focused. Within a trap the tree's own
        // answer is the app's, and it takes the keyboard once the order the modal shows now is
        // known, rather than the answer restored from before the build.
        // SAFETY: the build above made this exact tree, exclusively owned.
        let retained_focus = self
            .accessibility
            .focus_trap_active
            .then(|| unsafe {
                crate::widget::layout::CompositionFocus::current(
                    &self.layout.node_map,
                    &self.bindings,
                    None,
                )
            })
            .flatten();
        #[cfg(test)]
        let probe = crate::profiling::frame_probe::span("rebuild.focus");
        self.update_focusable_order();
        #[cfg(test)]
        drop(probe);
        if let Some(node) = retained_focus
            && self.focusable_order.contains(&node)
        {
            self.set_focused_node(Some(node));
            self.accessibility.focus_trap_entry = None;
        }
        if let Some((at, identity, live, remembered)) = trapped_focus
            && self.accessibility.focus_trap_active
            && retained_focus.is_none()
            && !trap_entry_provisional
        {
            // The same answer has the keyboard after the rebuild. When the
            // tree no longer reports that answer, a control that still answers
            // to the same name at the same place may take the keyboard: a
            // rebuilt modal can show different answers, and the person's place
            // there is the first one, as it already was by the focus order
            // above.
            // SAFETY: the build above made this exact tree, exclusively owned.
            let restored = identity.and_then(|focus| unsafe {
                focus.restore(
                    &self.layout.node_map,
                    &self.bindings,
                    Some(&self.focusable_order),
                )
            });
            let kept = restored.or_else(|| {
                if let Some(named) = remembered.as_deref() {
                    return self.unique_answering(named);
                }
                let at = at?;
                let node = *self.focusable_order.get(at)?;
                (self.answer_name(node) == live).then_some(node)
            });
            if let Some(node) = kept {
                self.set_focused_node(Some(node));
            }
        }

        if self.autofocus_on_structure_change
            && !self.accessibility.focus_trap_active
            && !self.presents_history()
            && !self.embeds_history()
        {
            // A rebuilt tree can already hold focus: a form that re-creates
            // its controls puts the keyboard back on the field the person was
            // in, by identity. That field is where focus belongs; the old
            // ordinal would move it to whatever now sits in the same slot and
            // leave both fields drawn as focused.
            // SAFETY: the maps describe the tree built above, exclusively owned.
            let retained = if focus_reset {
                None
            } else {
                unsafe {
                    crate::widget::layout::CompositionFocus::current(
                        &self.layout.node_map,
                        &self.bindings,
                        Some(&self.focusable_order),
                    )
                }
            };
            if let Some(node) = retained {
                self.focused_node = Some(node);
            } else if focus_reset {
                clear_stray_focus(self.root_widget.as_mut());
                if let Some(target) = self.tree().default_focus(&self.focusable_order) {
                    self.set_focused_node(Some(target));
                }
            } else if let Some(idx) = prev_focus_idx {
                clear_stray_focus(self.root_widget.as_mut());
                let exact = self.focusable_order.get(idx).copied();
                let target = exact.or_else(|| self.focusable_order.last().copied());
                if let Some(id) = target {
                    self.set_focused_node(Some(id));
                    if exact.is_some()
                        && let Some(state) = prev_focus_state.as_deref()
                    {
                        let _ = self.mutate_node(id, |widget| {
                            crate::widget::undo_points::set_state(widget, state)
                        });
                    }
                }
            }
        }

        // The keyboard is seated once the tree this pass builds stands: a build that puts it on a
        // control of its own (a page moving to the field it asks for) has it there, and the name
        // the person's focus answered to only restores what the tree did not seat.

        // The widgets built above are fresh instances the OLD `sync` never
        // ran against, so without this they draw blank until the next event.
        #[cfg(test)]
        let probe = crate::profiling::frame_probe::span("rebuild.sync");
        self.root_widget.sync(&self.state.doc, &self.state.root);
        #[cfg(test)]
        drop(probe);
        self.needs_sync = false;
        let _ =
            crate::component_lifecycle::take_save_status_presentation(self.root_widget.as_mut());
        // A sync that replaced delegates without saying so is found too.
        if self.root_widget.take_structure_dirty() | self.layout_maps_stale() {
            // Fresh sync can replace delegates. Rebuild before any map access.
            self.layout.request_rebuild();
            self.bindings.clear();
            self.layout.node_map.clear();
        } else if self.focused_node.is_none() {
            // A retained control can keep focus while a modal temporarily
            // owns input. Rebuilding invalidates its NodeId, not that live
            // focus. Recover only the unique current focused path inside
            // the current allowed order; never transfer focus or state to
            // a replacement.
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
        self.window.minimum_width = crate::widget::minimum_window_width(self.root_widget.as_ref());
    }

    /// The narrowest the window's layout works at, in logical pixels, as of the last build of its tree
    /// ([`crate::widget::Widget::minimum_window_width`]).
    pub fn minimum_window_width(&self) -> Option<f64> {
        self.window.minimum_width
    }

    /// `frame` re-reads a fitted size from its motion; between frames, asking again on every
    /// configure would resize the window without drawing at each size. Whether the window asked
    /// for a size it gets later: a layer surface's size is the compositor's answer, and a frame
    /// drawn before it comes is at a size the compositor no longer shows.
    #[cfg(feature = "native")]
    fn reconcile_minimum_size(&mut self, window: &Window, frame: bool) -> bool {
        if frame && self.window.preferred_size.is_some() {
            self.window.preferred_size =
                crate::widget::preferred_window_size(self.root_widget.as_ref());
        }
        if self.window.preferred_size != self.window.preferred_size_stated {
            self.window.preferred_size_stated = self.window.preferred_size;
            let size = self
                .window
                .preferred_size
                .map(|(width, height)| winit::dpi::LogicalSize::new(width.ceil(), height.ceil()));
            window.set_min_inner_size(size);
            window.set_max_inner_size(size);
            if let Some(size) = size {
                let wanted = size.to_physical::<u32>(window.scale_factor());
                return window
                    .request_inner_size(size)
                    .is_some_and(|given| given != wanted);
            }
        }
        if self.window.preferred_size.is_some() {
            return false;
        }
        let wanted = self.window.minimum_width;
        if wanted == self.window.minimum_width_stated {
            return false;
        }
        self.window.minimum_width_stated = wanted;
        window.set_min_inner_size(
            wanted.map(|width| winit::dpi::LogicalSize::new(width.ceil(), 1.0)),
        );
        false
    }

    /// Takes effect when the window is created, so set before [`App::run`].
    #[must_use]
    pub fn with_window_title(mut self, title: impl Into<String>) -> Self {
        self.window.title = Some(title.into());
        self
    }

    /// Name the window by what it shows, for the shell to name it by too: the page, the record, the
    /// place its content says it is ([`crate::component_lifecycle::WindowTitle`]), and the title this
    /// window states where its content says nothing.
    #[cfg(feature = "native")]
    pub(crate) fn refresh_window_title(&mut self, window: &Window) -> bool {
        let shown = crate::component_lifecycle::window_title(self.root_widget.as_ref())
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned);
        let title = shown.unwrap_or_else(|| resolve_window_title(self.window.title.as_deref()));
        if self.window.shown_title.as_deref() == Some(title.as_str()) {
            return false;
        }
        window.set_title(&title);
        self.window.shown_title = Some(title);
        true
    }

    /// Set the stable Wayland application id announced by this window.
    #[must_use]
    pub fn with_window_app_id(mut self, app_id: impl Into<String>) -> Self {
        self.window.app_id = Some(app_id.into());
        self
    }

    /// The Wayland application id the window announces when it is not the process package id: an input
    /// surface's, or one set with [`Self::with_window_app_id`].
    pub fn window_app_id(&self) -> Option<&str> {
        self.window.app_id.as_deref()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        #[cfg(feature = "model")]
        {
            self.retire_semantic_root();
        }
        self.control_domain.close();
        self.bindings.clear();
        self.layout.node_map.clear();
        self.close_wake_scope();
        let _entry = self.enter_wake_scope();
        let _closing = crate::motion::closing_window();
        let root = std::mem::replace(
            &mut self.root_widget,
            Box::new(crate::widget::Container::new()),
        );
        drop(root);
    }
}

fn clear_stray_focus(widget: &mut dyn crate::widget::Widget) {
    if widget.is_focusable() && widget.is_focused() {
        widget.set_focus(false);
    }
    for child in widget.children_mut() {
        clear_stray_focus(child);
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
fn completed_primary_frame_checks(
    checks: &std::collections::HashMap<String, Option<u64>>,
    last_presented: u64,
) -> Vec<String> {
    checks
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
        let pointer = crate::synthetic_input::cursor_moved(10.0, 20.0);
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

#[cfg(test)]
mod generic_floating_activity_tests {
    use super::{
        completed_primary_frame_checks, generic_floating_ids_to_dismiss,
        native_floating_logical_size,
    };
    use crate::floating_surface::FloatingSurfacePresentation;
    use std::collections::{HashMap, HashSet};
    use vello::kurbo::Rect;

    #[test]
    fn floating_activity_pumps_and_proxies_until_the_exact_primary_frame() {
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
                    input_rect: None,
                    returning_to_app: false,
                },
            )]);
            let suspended = HashSet::from(["navigation:route".to_string()]);
            assert_eq!(
                generic_floating_ids_to_dismiss(&previous, &current, &native, &suspended),
                vec!["browser.media-pip".to_string()]
            );
        }

        {
            let compact = native_floating_logical_size(Rect::new(80.0, 40.0, 456.0, 292.0));
            let expanded = native_floating_logical_size(Rect::new(80.0, 40.0, 456.0, 352.0));
            assert_eq!((compact.width, compact.height), (376.0, 252.0));
            assert_eq!((expanded.width, expanded.height), (376.0, 312.0));
        }

        {
            let checks = HashMap::from([
                ("waiting".to_string(), None),
                ("next".to_string(), Some(12)),
                ("later".to_string(), Some(13)),
            ]);
            assert!(completed_primary_frame_checks(&checks, 11).is_empty());
            assert_eq!(
                completed_primary_frame_checks(&checks, 12),
                vec!["next".to_string()]
            );
        }
    }
}

#[cfg(feature = "native")]
fn resolve_window_title(explicit: Option<&str>) -> String {
    let environment = crate::process::environment();
    let app_name = environment.identity.app_name.clone();
    let app_id = environment.identity.app_id.clone();
    let binary = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    resolve_title_from(
        explicit,
        app_name.as_deref(),
        app_id.as_deref(),
        binary.as_deref(),
    )
}

/// Public because it is half of a protocol: what a pointer published in this
/// surface's content coordinates is scoped to
/// ([`crate::presence::PointerSpace::Surface`]) — both ends must spell the
/// surface the same way by calling this same function.
pub fn local_surface_id() -> String {
    let app_id = crate::process::environment().identity.app_id.clone();
    let binary = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    resolve_app_id(app_id.as_deref(), binary.as_deref())
}

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
mod window_targets_tests;

#[cfg(test)]
mod sideways_wheel_tests;

#[cfg(test)]
mod motion_source_tests;

#[cfg(all(test, feature = "native"))]
mod held_input_tests;

#[cfg(all(test, feature = "native", unix))]
mod press_after_input_tests;

#[cfg(test)]
mod manual_composition_tests;

#[cfg(test)]
mod number_input_layout_tests;

#[cfg(test)]
mod committed_document_tests;
#[cfg(test)]
mod context_menu_input_tests;
#[cfg(test)]
#[path = "app/history_tests.rs"]
pub(crate) mod framework_history_tests;
#[cfg(test)]
mod tree_record_tests;

#[cfg(test)]
mod person_resource_tests;

#[path = "app/history.rs"]
mod framework_history;

#[cfg(feature = "model")]
mod committed_document;

#[cfg(feature = "model")]
mod document_routes;

#[cfg(test)]
mod focus_selection_tests;

#[cfg(test)]
mod presence_invocation_tests;

#[cfg(test)]
mod large_document_tests;

#[cfg(all(test, feature = "native"))]
mod scroll_cost_tests;

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
mod private_continuity_route_tests;

#[cfg(test)]
pub(crate) mod control_fixture_tests {
    include!("app/deferred_invocation_tests.rs");
    include!("app/state_tick_tests.rs");
    use super::*;
    use crate::history_mode::*;
    use std::{cell::RefCell, rc::Rc};
    pub(crate) struct Fixture {
        child: Box<dyn Widget>,
        pub resources: Rc<RefCell<Vec<tilo_actions::ResourceSpec>>>,
        pub log: Rc<RefCell<Vec<serde_json::Value>>>,
        output: Rc<RefCell<Option<crate::ControlOutbox>>>,
    }
    impl Widget for Fixture {
        crate::provides!(
            dyn crate::widget::controls::ActionControls,
            dyn crate::widget::resources::PublishesResources,
            dyn crate::widget::ResourceOperations
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
    }

    impl crate::widget::ResourceOperations for Fixture {
        fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
            let mut handlers = crate::widget::resource_operation_handlers(&*self.child);
            handlers.insert("fixture.control".into());
            handlers
        }

        fn start_operation(
            &mut self,
            op: &crate::widget::ResourceOperation<'_>,
            doc: &mut crate::widget::Edits<'_>,
            path: &automerge::ObjId,
        ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
            let request = op.request();
            if request.type_id != "fixture.control" {
                return crate::widget::start_operation(
                    &mut *self.child,
                    op,
                    &mut crate::widget::Edits::lend(doc.document_mut()),
                    path,
                );
            }
            self.log
                .borrow_mut()
                .push(serde_json::from_slice(request.payload).unwrap());
            Ok(Some(crate::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        }
    }

    impl crate::widget::resources::PublishesResources for Fixture {
        fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
            self.resources.borrow().clone()
        }
    }

    impl crate::widget::controls::ActionControls for Fixture {
        fn attach_controls(
            &mut self,
            domain: &crate::ControlDomain,
            attachment: crate::control_domain::DomainAttachment,
        ) {
            crate::widget::controls::attach_controls(self.child.as_mut(), domain, attachment);
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
        }],
    };
    fn fixture_widget(
        policy: tilo_actions::ConfirmationPolicy,
        source: std::sync::Arc<std::sync::Mutex<serde_json::Map<String, serde_json::Value>>>,
    ) -> (
        Fixture,
        Rc<RefCell<Vec<serde_json::Value>>>,
        Rc<RefCell<Vec<tilo_actions::ResourceSpec>>>,
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
        spec.type_mut().contract.confirmation = policy;
        let resource =
            ResourceSpec::new("ordinary:fixture", "fixture", "Fixture").with_action(spec);
        let binding = ActionBinding::new(&resource, &resource.actions[0], move |()| {
            source.lock().unwrap().clone()
        })
        .unwrap();
        let resources = Rc::new(RefCell::new(vec![resource]));
        let log = Rc::new(RefCell::new(vec![]));
        let widget = Fixture {
            child: Box::new(
                crate::widget::Button::new("ordinary-emitter", "Emit request").on_action(binding),
            ),
            resources: resources.clone(),
            log: log.clone(),
            output: Rc::new(RefCell::new(None)),
        };
        (widget, log, resources)
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
                    crate::DocumentId::for_tab(990),
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
        Rc<RefCell<Vec<tilo_actions::ResourceSpec>>>,
    ) {
        let (widget, log, resources) = fixture_widget(policy, source());
        (managed(Box::new(widget)), log, resources)
    }
    crate::testing::test_app_widget!(Fixture, "Confirmation fixture");
    struct OwnerGuard {
        binding: crate::bound_dispatch::CallbackBinding,
        _host: crate::application_test_support::ProbeHarness,
    }
    impl OwnerGuard {
        fn managed(&self, widget: Box<dyn Widget>) -> ManagedRoot {
            static COMPONENT_HISTORY: HistoryDefinition = HistoryDefinition {
                app_id: crate::application_test_support::APP,
                scopes: HISTORY.scopes,
            };
            let (definition, owner) = (
                &COMPONENT_HISTORY,
                HistoryOwner::component(self.binding.clone()),
            );
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
    }
    fn owned(widget: Fixture) -> (Box<dyn Widget>, OwnerGuard) {
        use crate::application_test_support::*;
        let host = ProbeHarness::new();
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
            OwnerGuard {
                binding,
                _host: host,
            },
        )
    }
    pub(crate) fn layout(app: &mut App) {
        app.update_native_model();
        app.sync_model(false);
        // Match native redraw: capture the pending structural change before
        // rebuilding clears it; unchanged trees must use the normal fast path.
        app.layout.structure_dirty |= app.layout.root_node.is_none()
            || app.layout.node_map.is_empty()
            || app.take_root_structure_dirty();
        let structure_rebuilt = app.layout.structure_dirty;
        if structure_rebuilt {
            app.rebuild_structure();
        }
        let node = app.layout.root_node.unwrap();
        run_layout_pass(app, node, 900, 650, 1.0, structure_rebuilt, None).unwrap();
        draw_frame_content(app, &mut Scene::new(), 900, 650, 1.0, None);
        complete_native_frame_content(app, |_| {});
    }

    #[test]
    fn native_control_coverage_admits_only_declared_app_and_host_actions() {
        use tilo_actions::{ActionBinding, ActionSpec, OperationKind, ResourceSpec};
        let (widget, _, _) = fixture_widget(tilo_actions::ConfirmationPolicy::None, source());
        let missing_binding = |resource_id: &str| {
            let resource = ResourceSpec::new(resource_id, "fixture.coverage", "Missing action")
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
                    crate::widget::search_request::open_search_binding(
                        &crate::widget::search_request::SearchRequest::this_app(),
                    ),
                ),
            ))
            .push(Box::new(crate::history_mode::api::history_control_button(
                "coverage-history",
                crate::widget::window_header::HistoryControl::Closed,
            )))
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
}
