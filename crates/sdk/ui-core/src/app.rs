use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{CursorIcon, Fullscreen, Window, WindowId},
};

#[cfg(test)]
use std::cell::Cell;

mod bound_record_sync;
mod close_admission;
mod document_history;
pub use document_history::DocumentHistorySnapshot;
include!("app/floating.rs");
mod runtime;
use runtime::{
    AccessibilityRuntime, ActionRuntime, CollaborationRuntime, ContinuationRuntime, HistoryRuntime,
    InputRuntime, LayoutRuntime, PersistenceRuntime, PresenceRuntime, WindowRuntime,
};

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
/// forcing a full `compute_layout` on every `layout_dirty` frame — a safety
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

/// How frequently an otherwise-idle app polls a connected collaboration
/// transport. The shovel currently has no winit wake bridge, so `Wait`
/// forever would strand inbound presence/document events; polling it at the
/// display cadence, however, wakes every built-in app 60 times per second
/// even when nothing changed. Ten hertz keeps remote state interactive while
/// bounding idle CPU independently of the monitor refresh rate.
const COLLAB_IDLE_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdlePumpCadence {
    Wait,
    Deadline(Instant),
}

fn idle_pump_deadline(
    now: Instant,
    visual_work: bool,
    collab_connected: bool,
    scheduled_deadline: Option<Instant>,
) -> IdlePumpCadence {
    let periodic = if visual_work {
        now.checked_add(Duration::from_nanos(16_666_667))
            .unwrap_or(now)
    } else if collab_connected {
        now.checked_add(COLLAB_IDLE_POLL).unwrap_or(now)
    } else if let Some(deadline) = scheduled_deadline {
        return IdlePumpCadence::Deadline(deadline);
    } else {
        return IdlePumpCadence::Wait;
    };
    IdlePumpCadence::Deadline(
        scheduled_deadline.map_or(periodic, |deadline| deadline.min(periodic)),
    )
}

#[cfg(test)]
mod animation_pump_tests {
    use super::{
        COLLAB_IDLE_POLL, IdlePumpCadence, animation_pump_needed, continuation_publication,
        idle_pump_deadline,
    };
    use crate::collaboration::{CollabRequest, ContinuationMetadata};

    #[test]
    fn reduced_motion_does_not_keep_the_idle_loop_awake() {
        assert!(animation_pump_needed(true, false));
        assert!(!animation_pump_needed(true, true));
        assert!(!animation_pump_needed(false, false));
    }

    #[test]
    fn connected_idle_collaboration_does_not_use_the_animation_cadence() {
        let now = std::time::Instant::now();
        assert_eq!(
            idle_pump_deadline(now, false, true, None),
            IdlePumpCadence::Deadline(now + COLLAB_IDLE_POLL)
        );
        assert_eq!(
            idle_pump_deadline(now, false, false, None),
            IdlePumpCadence::Wait
        );
        assert_eq!(
            idle_pump_deadline(now, true, true, None),
            IdlePumpCadence::Deadline(now + std::time::Duration::from_nanos(16_666_667))
        );
    }

    #[test]
    fn an_earlier_widget_deadline_wins_over_periodic_polling() {
        let now = std::time::Instant::now();
        let scheduled = now + std::time::Duration::from_millis(25);
        assert_eq!(
            idle_pump_deadline(now, false, true, Some(scheduled)),
            IdlePumpCadence::Deadline(scheduled)
        );
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
    complete: Vec<tilo_actions::Resource>,
}

struct LocalResourceCache {
    key: ResourceCacheKey,
    permission_generation: u64,
    resources: Vec<tilo_actions::Resource>,
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
pub struct App {
    #[cfg(windows)]
    startup_renderer: Option<RendererPreparation>,
    wake_scope: Option<crate::wake::WakeScope>,
    state: State,
    root_widget: Box<dyn Widget>,
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

    history: HistoryRuntime,

    collaboration: CollaborationRuntime,
    pub presence_ctx: PresenceCtx,
    pub permission_ctx: PermissionContext,

    presence: PresenceRuntime,

    accessibility: AccessibilityRuntime,

    window: WindowRuntime,

    pub(crate) legacy_persistence: crate::launch::LegacyPersistencePolicy,
    persistence: PersistenceRuntime,

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

    #[cfg(test)]
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

    /// Construct an explicitly detached app for headless or test execution.
    ///
    /// This masks any ambient host while App setup runs. It cannot retroactively
    /// change handles already captured while constructing `state` or
    /// `root_widget`. Native launchers must use [`Self::from_factory`],
    /// [`Self::try_from_factory`], or [`Self::from_scoped_parts`].
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
        self.needs_sync = true;
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
            history: HistoryRuntime::new(),
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

include!("app/actions.rs");
include!("app/continuation.rs");

impl App {
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
}

include!("app/accessibility.rs");

impl App {
    /// Reconciles both directions, idempotently: app-driven (push
    /// `fullscreen_intent` to the window on change) and compositor-driven
    /// (read the actual state back and notify the root on a transition, e.g.
    /// a global un-fullscreen keybind or a denied request).
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
}

include!("app/collaboration.rs");

impl App {
    #[doc(hidden)]
    pub fn state_mut_for_test(&mut self) -> &mut crate::state::State {
        self.invalidate_widget_resource_cache();
        &mut self.state
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
        self.history.stable_focus_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.history.stable_metadata = self.focused_node.and_then(|id| {
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
        self.enable_document_history_chrome();
        let event_loop = EventLoop::<UserEvent>::with_user_event().build().unwrap();
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
            floating_source_clear_frames: std::collections::HashMap::new(),
            floating_destination_present_frames: std::collections::HashMap::new(),
            touch_pointer: crate::interaction::TouchPointerEmulator::default(),
            dispatching_touch_pointer: false,
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
/// ([`crate::presence::PointerSpace::Surface`]) — both ends must spell the
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

include!("app/persistence.rs");
include!("app/host.rs");
