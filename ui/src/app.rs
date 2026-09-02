use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{CursorIcon, Fullscreen, Window, WindowId},
};

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
    Wake,
    /// No `WindowEvent` analogue, so accesskit events get their own variant.
    AccessKit(accesskit_winit::WindowEvent),
}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(e: accesskit_winit::Event) -> Self {
        UserEvent::AccessKit(e.window_event)
    }
}
use crate::collaboration::{CollabEvent, CollabRequest, CollaborationManager, PresencePayload};
use crate::gpu::RenderContext;
use crate::permissions::PermissionContext;
use crate::presence::{PresenceCtx, PresenceKey};
use crate::render_thread::{FrameOutcome, RenderRequest, RenderThreadHandle, spawn_render_thread};
use crate::state::State;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use taffy::{AvailableSpace, NodeId, TaffyTree};
use vello::{
    Renderer, RendererOptions, Scene, kurbo::Affine, kurbo::Point, kurbo::Rect as KurboRect,
    peniko::Color,
};

/// Cached `QUICK_UI_PERF=1` check, so the per-frame read costs nothing.
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

#[cfg(test)]
mod animation_pump_tests {
    use super::animation_pump_needed;

    #[test]
    fn reduced_motion_does_not_keep_the_idle_loop_awake() {
        assert!(animation_pump_needed(true, false));
        assert!(!animation_pump_needed(true, true));
        assert!(!animation_pump_needed(false, false));
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
    if let Some(intent) = w.appearance_intent() {
        return Some(intent);
    }
    for child in w.children() {
        if let Some(intent) = find_appearance_intent(child) {
            return Some(intent);
        }
    }
    None
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
type FrameTick = Box<dyn FnMut(&mut PresenceCtx) + Send + Sync>;
type ResolvedResourceInvocation<'a> = (
    Option<serde_json::Map<String, serde_json::Value>>,
    &'a qos_actions::ActionSpec,
);

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

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independently toggled, orthogonal opt-in/state bit with its own doc comment; grouping them into sub-structs would ripple through every read/write site in this file for no behavioural gain"
)]
pub struct App {
    state: State,
    root_widget: Box<dyn Widget>,
    text_renderer: TextRenderer,
    taffy: TaffyTree<()>,
    root_node: Option<NodeId>,
    node_map: std::collections::HashMap<NodeId, *mut dyn Widget>,
    layout_dirty: bool,
    /// Set when a widget reports `take_structure_dirty()`: without a taffy
    /// rebuild before the next layout pass, runtime-spawned widgets (e.g. a
    /// tab-drop split) have no `NodeId` and `apply_layout` skips them.
    structure_dirty: bool,
    needs_sync: bool,
    /// Diffed against `inner_size()` in `about_to_wait` to catch a resize
    /// that arrived without a `Resized` event.
    last_frame_size: Option<winit::dpi::PhysicalSize<u32>>,
    /// `(width, height)` the taffy tree was last solved against — lets the
    /// scroll-reposition fast path skip a full re-solve unless it changed.
    last_layout_px: Option<(u32, u32)>,
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
    modifiers: winit::event::Modifiers,

    interaction: crate::interaction::InteractionEnvironment,
    platform: crate::platform::PlatformConventions,

    nav_gestures: crate::gesture::NavGestureRecognizer,

    // Stable state for Undo/Redo tracking
    last_stable_focus_idx: Option<usize>,
    last_stable_metadata: Option<String>,

    // Collaboration
    collab_manager: CollaborationManager,
    /// Every committed action is attributed to it and Ctrl+Z only reverts
    /// actions carrying it, so undo walks past collaborators' work rather
    /// than through it.
    local_author: crate::permissions::UserId,
    pub presence_ctx: PresenceCtx,
    pub permission_ctx: PermissionContext,

    /// `None` disables outbound broadcast (headless tests, `quick-ui`
    /// examples).
    presence_broadcast_scope: Option<PresenceKey>,
    /// `None` is the ordinary case; the shell sets
    /// [`crate::presence::PointerSpace::Desktop`] for its own chrome, whose
    /// window *is* the desktop other surfaces resolve against. Set rather
    /// than guessed — a wrong guess publishes a position in the wrong space
    /// with nothing to detect it from outside.
    pointer_space: Option<crate::presence::PointerSpace>,
    /// The peer "followed" via a clicked presence indicator; `None` = not
    /// following.
    followed_peer: Option<String>,
    last_pointer_broadcast: Option<Instant>,
    /// Diffed against, to avoid re-sending unchanged presence every frame.
    last_focus_presence: Option<(PresenceKey, crate::collaboration::PresencePayload)>,
    last_location_presence: Option<(PresenceKey, crate::collaboration::PresencePayload)>,
    last_view_presence: Option<crate::view::ViewSubset>,
    /// Fans cursor moves out to Iroh peers too, alongside the Unix-socket
    /// send. Invoked after the throttle gate.
    presence_broadcast_sink: Option<PresenceBroadcastSink>,
    /// Lets the shell drain `NeighbourMsg::PresenceMoved` into
    /// [`PresenceCtx::remote_cursors`] without App knowing about Iroh.
    frame_tick: Option<FrameTick>,

    /// Only calls into winit when it changes, keeping the hot pointer path
    /// cheap.
    current_cursor: CursorIcon,

    tooltip_host: crate::widget::TooltipHost,

    /// `None` makes every narration call site a no-op, so a reader-off
    /// build is byte-for-byte unaffected.
    screen_reader: Option<crate::screen_reader::ScreenReader>,
    /// winit doesn't expose the **Insert** key (the NVDA/Orca reader
    /// modifier) as a [`Modifiers`](winit::event::Modifiers) bit, so
    /// press/release is tracked here for the `Insert + <key>` command layer.
    screen_reader_modifier_down: bool,
    live_region_last: std::collections::HashMap<String, String>,

    /// Tracked to detect the open→close transition and restore focus.
    focus_trap_active: bool,
    focus_before_trap: Option<NodeId>,

    /// Kept separate from the live [`Self::theme`] so disabling high
    /// contrast restores it exactly.
    base_theme: Theme,
    a11y: crate::a11y::A11ySettings,
    text_scale: f32,
    /// Composes on top of the per-output device scale
    /// ([`Theme::with_ui_scale`]) so the result stays crisp at any
    /// fractional scale.
    ui_scale: f32,

    /// Compared against [`crate::widget::Widget::fullscreen_intent`] each
    /// frame so a steady desired state issues one `set_fullscreen` call —
    /// see [`fullscreen_request_decision`].
    fullscreen_requested: bool,
    /// To distinguish app-driven from compositor-driven transitions.
    fullscreen_active: bool,

    autosave: Option<crate::persist::Autosave>,
    /// When present the live document is one record's, named by the root
    /// widget via [`crate::widget::Widget::record_intent`].
    records: Option<crate::record::RecordSet>,
    room_index: Option<automerge::AutoCommit>,

    action_palette: Option<crate::action_palette::ActionPalette>,
    action_registry: qos_actions::ActionRegistry,
    action_transport: Box<dyn qos_actions::ActionTransport>,
    last_published_actions: Option<Vec<qos_actions::ActionDescriptor>>,
    /// Background/streaming operations admitted by the retained widget tree.
    /// They remain here until the widget reports exactly one terminal update.
    directed_invocations: std::collections::HashMap<qos_actions::InvocationId, DirectedInvocation>,
    /// Same lifecycle for actions started from this window rather than by the
    /// broker; terminal state is reflected by the owning widget's UI.
    local_invocations: std::collections::HashSet<qos_actions::InvocationId>,
    semantic_tree: crate::a11y_tree::SemanticTree,

    profiler: Option<crate::profiling::FrameProfiler>,

    /// Wins over the automatic env/package-id/binary-name resolution.
    window_title: Option<String>,
    /// Wins over `QOS_PACKAGE_ID`/binary-name resolution for the Wayland
    /// `xdg_toplevel.app_id`. System surfaces use this as a stable identity.
    window_app_id: Option<String>,

    /// Optional app → shell lifecycle channel for detachable surfaces. It is
    /// discovered from the launcher-provided service fd and absent outside a
    /// qOS session, where apps simply keep their overlays attached.
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
    /// App-local → desktop translation supplied by the shell for each
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

impl App {
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

    pub fn new(state: State, root_widget: Box<dyn Widget>) -> Self {
        Self::with_collaboration_manager(state, root_widget, CollaborationManager::new())
    }

    /// Construct an app with an explicitly connected collaboration manager.
    ///
    /// Launchers normally use [`Self::new`], which discovers the inherited
    /// runtime service. Headless hosts and integration harnesses already own
    /// a connected endpoint, so accepting it here avoids process-global
    /// environment mutation and keeps those callers on the production typed
    /// transport.
    pub fn with_collaboration_manager(
        state: State,
        root_widget: Box<dyn Widget>,
        collab_manager: CollaborationManager,
    ) -> Self {
        let configured_theme = Theme::configured();
        let mut app = Self {
            state,
            root_widget,
            text_renderer: TextRenderer::new(),
            taffy: TaffyTree::new(),
            root_node: None,
            node_map: std::collections::HashMap::new(),
            layout_dirty: true,
            structure_dirty: false,
            nav_gestures: crate::gesture::NavGestureRecognizer::new(),
            needs_sync: true,
            // Finalised below, once base_theme/a11y/text_scale all exist.
            last_frame_size: None,
            last_layout_px: None,
            theme: configured_theme.clone(),
            debug_mode: cfg!(debug_assertions) && std::env::var("QUICK_UI_DEBUG").is_ok(),
            focused_node: None,
            focusable_order: Vec::new(),
            autofocus_first: false,
            autofocus_on_structure_change: false,
            appearance_reconcile: false,
            theme_store_stamp: settings_store::settings_path()
                .and_then(|path| std::fs::metadata(path).ok()?.modified().ok()),
            last_theme_store_check: Instant::now(),
            modifiers: winit::event::Modifiers::default(),
            interaction: crate::interaction::InteractionEnvironment::default(),
            platform: crate::platform::PlatformConventions::default(),
            last_stable_focus_idx: None,
            last_stable_metadata: None,
            collab_manager,
            local_author: crate::collaboration::local_user_id(),
            presence_ctx: Self::initial_presence_context(),
            permission_ctx: PermissionContext::permissive(),
            presence_broadcast_scope: None,
            pointer_space: None,
            followed_peer: None,
            last_pointer_broadcast: None,
            last_focus_presence: None,
            last_location_presence: None,
            last_view_presence: None,
            presence_broadcast_sink: None,
            frame_tick: None,
            current_cursor: CursorIcon::Default,
            tooltip_host: crate::widget::TooltipHost::new(),
            // Auto-enable if the shell mirrored QOS_A11Y_* into this sandbox.
            screen_reader: if Self::screen_reader_env_enabled()
                || Self::persisted_accessibility_bool("screen_reader")
            {
                Some(Self::configured_screen_reader())
            } else {
                None
            },
            screen_reader_modifier_down: false,
            live_region_last: std::collections::HashMap::new(),
            focus_trap_active: false,
            focus_before_trap: None,
            base_theme: configured_theme,
            a11y: crate::a11y::A11ySettings::default(),
            text_scale: 1.0,
            ui_scale: 1.0,
            fullscreen_requested: false,
            fullscreen_active: false,
            autosave: None,
            records: None,
            room_index: None,
            action_palette: None,
            action_registry: qos_actions::ActionRegistry::new(),
            action_transport: Box::new(qos_actions::LocalTransport::new()),
            last_published_actions: None,
            directed_invocations: std::collections::HashMap::new(),
            local_invocations: std::collections::HashSet::new(),
            semantic_tree: crate::a11y_tree::SemanticTree::new(),
            profiler: crate::profiling::FrameProfiler::from_env(),
            window_title: None,
            window_app_id: None,
            floating_surface_client:
                crate::floating_surface::FloatingSurfaceServiceClient::discover(),
            last_floating_presentations: std::collections::HashMap::new(),
            last_active_floating_ids: std::collections::HashSet::new(),
            prepared_floating_presentations: std::collections::HashSet::new(),
            floating_coordinate_offsets: std::collections::HashMap::new(),
            suspended_floating_presentations: std::collections::HashSet::new(),
            floating_host_messages: Vec::new(),
        };

        app.a11y.high_contrast = Self::high_contrast_env_enabled();
        app.a11y.reduce_motion = Self::reduce_motion_env_enabled();
        app.a11y.screen_reader = app.screen_reader.is_some();
        app.text_scale = Self::text_scale_env().unwrap_or(1.0);
        app.ui_scale = Self::ui_scale_env().unwrap_or(1.0);
        app.rebuild_theme();

        // The scope is also what sends `Arrived`, so an app that never set
        // one would never catch up on anything written before it opened.
        // No room means no scope.
        if let Some(scope) = PresenceKey::for_local_workspace() {
            app.set_presence_broadcast_scope(Some(scope));
        }
        app.presence_ctx.set_surface_id(crate::local_surface_id());
        app
    }

    fn rebuild_theme(&mut self) {
        self.theme = self
            .base_theme
            .clone()
            .with_a11y(&self.a11y, self.text_scale, self.ui_scale);
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
            // Outside a qOS session there is no trusted shell chooser to
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
    pub fn action_registry_mut(&mut self) -> &mut qos_actions::ActionRegistry {
        &mut self.action_registry
    }

    pub fn set_action_transport(&mut self, transport: Box<dyn qos_actions::ActionTransport>) {
        self.action_transport = transport;
    }

    fn collect_declared_resources(&self) -> Vec<qos_actions::Resource> {
        let mut resources = Vec::new();
        crate::widget::collect_widget_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &mut resources,
        );
        resources
    }

    fn refresh_control_action_availability(&mut self) {
        fn collect_addresses(
            resources: &[qos_actions::Resource],
            allowed: &mut std::collections::HashSet<qos_actions::ActionAddress>,
        ) {
            for resource in resources {
                allowed.extend(resource.actions.iter().map(|action| {
                    qos_actions::ActionAddress::new(resource.id.as_str(), &action.id)
                }));
                collect_addresses(&resource.children, allowed);
            }
        }

        let resources = self
            .collect_declared_resources()
            .into_iter()
            .filter_map(|resource| self.filter_resource_for_local_actor(resource))
            .collect::<Vec<_>>();
        let mut allowed = std::collections::HashSet::new();
        collect_addresses(&resources, &mut allowed);
        self.root_widget
            .refresh_control_action_availability(&allowed);
    }

    /// The complete machine-driveable surface: domain resources declared by
    /// the app plus ordinary controls projected from its retained semantic
    /// tree. Ctrl+K, scripts, and agents therefore consume one catalogue.
    pub fn collect_resources(&self) -> Vec<qos_actions::Resource> {
        let mut resources = self.collect_declared_resources();
        let mut semantics = crate::a11y_tree::SemanticTree::new();
        semantics.refresh(
            self.root_widget.as_ref(),
            &self.node_map,
            self.focused_node,
            &[],
        );
        resources.extend(semantics.action_resources());
        let mut operation_handlers = self.root_widget.resource_operation_handlers();
        operation_handlers.insert(crate::a11y_tree::SEMANTIC_OPERATION_TYPE.to_string());
        let violations =
            qos_actions::validate_surface(&resources, &self.action_registry, &operation_handlers);
        if violations.is_empty() {
            resources
        } else {
            for violation in violations {
                tracing::error!(
                    address = %violation.address,
                    reason = %violation.reason,
                    "refusing an invalid resource/action surface"
                );
            }
            Vec::new()
        }
    }

    /// Audit every retained control binding against the fresh resource tree.
    /// This is public so development tooling can turn remaining explicit
    /// legacy controls and stale/duplicate action addresses into a coverage
    /// report without inspecting widget implementation details.
    pub fn control_action_coverage(&self) -> Vec<ControlBindingCoverage> {
        let resources = self.collect_resources();
        self.control_action_coverage_for(&resources)
    }

    fn control_action_coverage_for(
        &self,
        resources: &[qos_actions::Resource],
    ) -> Vec<ControlBindingCoverage> {
        let mut seen = std::collections::HashSet::new();
        self.root_widget
            .control_action_bindings()
            .into_iter()
            .map(|binding| {
                let status = match &binding.kind {
                    crate::widget::ControlBindingKind::Action(address) => {
                        if qos_actions::door::resolve(resources, address).is_none() {
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
        encoded: &qos_actions::EncodedAction,
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

    /// Publish the live resource/action tree through the session broker.
    /// Level-triggered at the wire, diffed here so an unchanged frame emits
    /// nothing; publishing an empty list explicitly withdraws a closed tree.
    fn publish_action_surface(&mut self) {
        if !self.collab_manager.is_connected() {
            return;
        }
        let resources = self.collect_resources();
        // Use this exact snapshot for coverage as well as publication. That
        // prevents recursive/repeated resource collection and makes the audit
        // correspond to the catalogue about to cross the wire.
        let binding_coverage = self.control_action_coverage_for(&resources);
        if binding_coverage
            .iter()
            .any(|entry| entry.status == ControlBindingStatus::StaleAddress)
        {
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
                .last_published_actions
                .as_ref()
                .is_some_and(|actions| !actions.is_empty())
            {
                self.collab_manager.send(CollabRequest::PublishActions {
                    app_id: crate::collaboration::local_app_id(),
                    actions: Vec::new(),
                });
                self.last_published_actions = Some(Vec::new());
            }
            return;
        }
        let mut operation_handlers = self.root_widget.resource_operation_handlers();
        operation_handlers.insert(crate::a11y_tree::SEMANTIC_OPERATION_TYPE.to_string());
        let violations =
            qos_actions::validate_surface(&resources, &self.action_registry, &operation_handlers);
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
                .last_published_actions
                .as_ref()
                .is_none_or(Vec::is_empty)
            {
                return;
            }
            self.collab_manager.send(CollabRequest::PublishActions {
                app_id: crate::collaboration::local_app_id(),
                actions: Vec::new(),
            });
            self.last_published_actions = Some(Vec::new());
            return;
        }
        let descriptors = qos_actions::door::survey(&resources);
        if self.last_published_actions.as_ref() == Some(&descriptors) {
            return;
        }
        let actions = descriptors
            .iter()
            .map(|descriptor| crate::collaboration::ExportedAction {
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
                    qos_actions::InputSpec::None => crate::collaboration::ExportedInput::None,
                    qos_actions::InputSpec::Text {
                        prompt,
                        placeholder,
                        collects,
                    } => crate::collaboration::ExportedInput::Text {
                        prompt: prompt.clone(),
                        placeholder: placeholder.clone(),
                        wants_content: *collects == qos_actions::Collects::Content,
                    },
                },
            })
            .collect();
        self.collab_manager.send(CollabRequest::PublishActions {
            app_id: crate::collaboration::local_app_id(),
            actions,
        });
        self.last_published_actions = Some(descriptors);
    }

    fn invoke_published_action(
        &mut self,
        id: u64,
        resource: String,
        action: String,
        argument: Option<&str>,
        actor: &str,
    ) {
        let address = qos_actions::ActionAddress::new(resource, action);
        // Arrival on this directed stream means the broker already performed
        // permission and one-use confirmation-token checks.
        let invocation = qos_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let result = self.invoke_resource_action(&address, argument, actor, true, Some(invocation));
        let update = match result {
            Ok(crate::widget::ResourceOperationStart::Completed(output)) => {
                qos_actions::InvocationUpdate::Succeeded { invocation, output }
            }
            Ok(crate::widget::ResourceOperationStart::Accepted) => {
                self.directed_invocations.insert(
                    invocation,
                    DirectedInvocation {
                        wire_id: id,
                        cancellation_requested: false,
                    },
                );
                qos_actions::InvocationUpdate::Accepted { invocation }
            }
            Err(error) => qos_actions::InvocationUpdate::Failed {
                invocation,
                error: qos_actions::ActionFailure {
                    code: error.code().to_string(),
                    message: error.to_string(),
                    details: None,
                    retryable: false,
                },
            },
        };
        self.collab_manager
            .send(CollabRequest::UpdateInvocation { id, update });
    }

    fn resolve_resource_invocation<'a>(
        resources: &'a [qos_actions::Resource],
        address: &qos_actions::ActionAddress,
        argument: Option<&str>,
        confirmed_by_broker: bool,
    ) -> Result<ResolvedResourceInvocation<'a>, qos_actions::DoorError> {
        let typed_arguments = argument
            .and_then(|value| value.strip_prefix(qos_actions::ARGUMENTS_JSON_PREFIX))
            .map(|json| {
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
                    .map_err(|error| qos_actions::DoorError::InvalidArguments(error.to_string()))
            })
            .transpose()?;
        let resolved = qos_actions::door::resolve(resources, address)
            .ok_or_else(|| qos_actions::DoorError::Unknown(address.clone()))?;
        if !matches!(
            resolved.contract.confirmation,
            qos_actions::ConfirmationPolicy::None
        ) && !confirmed_by_broker
        {
            return Err(qos_actions::DoorError::ConfirmationRequired);
        }
        Ok((typed_arguments, resolved))
    }

    fn encode_resource_action(
        resources: &[qos_actions::Resource],
        address: &qos_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> Result<qos_actions::EncodedAction, qos_actions::DoorError> {
        if let Some(arguments) = typed_arguments {
            qos_actions::door::encode_json(resources, address, arguments)
        } else {
            qos_actions::door::encode(resources, address, argument)
        }
    }

    fn invoke_document_resource_action(
        &mut self,
        resources: &[qos_actions::Resource],
        address: &qos_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<crate::widget::ResourceOperationStart, qos_actions::DoorError> {
        let before = self.state.doc.get_heads();
        let dispatched = if let Some(arguments) = typed_arguments {
            qos_actions::door::invoke_json_confirmed(
                &mut self.state.doc,
                resources,
                address,
                arguments,
                actor.as_bytes(),
                &self.action_registry,
                self.action_transport.as_mut(),
            )?
        } else {
            qos_actions::door::invoke_confirmed(
                &mut self.state.doc,
                resources,
                address,
                argument,
                actor.as_bytes(),
                &self.action_registry,
                self.action_transport.as_mut(),
            )?
        };
        if dispatched.mutated_local() {
            self.state.record_committed_action_authored(
                before,
                None,
                None,
                Some(crate::permissions::UserId(actor.to_string())),
            );
            self.needs_sync = true;
            self.layout_dirty = true;
        }
        Ok(crate::widget::ResourceOperationStart::Completed(
            qos_actions::ActionOutput::default(),
        ))
    }

    fn invoke_resource_action(
        &mut self,
        address: &qos_actions::ActionAddress,
        argument: Option<&str>,
        actor: &str,
        confirmed_by_broker: bool,
        invocation: Option<qos_actions::InvocationId>,
    ) -> Result<crate::widget::ResourceOperationStart, qos_actions::DoorError> {
        let resources = self.collect_resources();
        let resources = if actor == self.local_author.0 {
            resources
                .into_iter()
                .filter_map(|resource| self.filter_resource_for_local_actor(resource))
                .collect()
        } else {
            resources
        };
        let (typed_arguments, resolved) =
            Self::resolve_resource_invocation(&resources, address, argument, confirmed_by_broker)?;
        let kind = resolved.kind;
        if kind == qos_actions::OperationKind::Document {
            return self.invoke_document_resource_action(
                &resources,
                address,
                typed_arguments.as_ref(),
                argument,
                actor,
            );
        }
        let encoded =
            Self::encode_resource_action(&resources, address, typed_arguments.as_ref(), argument)?;
        if encoded.type_id == crate::a11y_tree::SEMANTIC_OPERATION_TYPE {
            let handled = self
                .invoke_semantic_operation_encoded(&encoded)
                .map_err(qos_actions::DoorError::Failed)?;
            if !handled {
                return Err(qos_actions::DoorError::Unknown(address.clone()));
            }
            self.commit_action();
            self.needs_sync = true;
            self.layout_dirty = true;
            return Ok(crate::widget::ResourceOperationStart::Completed(
                qos_actions::ActionOutput::default(),
            ));
        }
        let operation_invocation = invocation.unwrap_or_default();
        let started = self
            .root_widget
            .start_resource_operation(
                operation_invocation,
                kind,
                &encoded.type_id,
                &encoded.payload,
                &self.state.doc,
                &self.state.root,
            )
            .map_err(qos_actions::DoorError::Failed)?;
        let Some(started) = started else {
            return Err(qos_actions::DoorError::Unknown(address.clone()));
        };
        if matches!(started, crate::widget::ResourceOperationStart::Accepted)
            && matches!(
                resolved.contract.execution,
                qos_actions::ExecutionMode::Immediate
            )
        {
            return Err(qos_actions::DoorError::Failed(
                "an immediate operation cannot remain pending".into(),
            ));
        }
        if invocation.is_none()
            && matches!(started, crate::widget::ResourceOperationStart::Accepted)
        {
            self.local_invocations.insert(operation_invocation);
        }
        if matches!(started, crate::widget::ResourceOperationStart::Accepted) {
            // Queued widget work advances from `sync`; admission must
            // therefore schedule a model pass even if no document
            // mutation or input event accompanied the invocation.
            self.needs_sync = true;
        }
        if let crate::widget::ResourceOperationStart::Completed(output) = &started {
            resolved
                .contract
                .result
                .validate(output)
                .map_err(qos_actions::DoorError::Failed)?;
        }
        self.layout_dirty = true;
        Ok(started)
    }

    /// Resolve queued local control intents against the current resource tree
    /// and enter through the same door/lifecycle as every other caller.
    fn dispatch_control_invocations(&mut self) -> usize {
        let mut dispatched = 0_usize;
        while let Some(intent) = self.root_widget.take_control_invocation() {
            let local_resources = self
                .collect_resources()
                .into_iter()
                .filter_map(|resource| self.filter_resource_for_local_actor(resource))
                .collect::<Vec<_>>();
            let Some(resolved) = qos_actions::door::resolve(&local_resources, &intent.address)
            else {
                tracing::warn!(
                    resource = %intent.address.resource,
                    action = %intent.address.action,
                    "action-bound control is unavailable to the local actor"
                );
                continue;
            };
            if !matches!(
                resolved.contract.confirmation,
                qos_actions::ConfirmationPolicy::None
            ) {
                // A bound control never manufactures confirmation. Hand the
                // exact address to the standard palette flow, which owns the
                // explicit confirm/phrase UI and only then enters the door.
                self.open_action_palette();
                if let Some(palette) = self.action_palette.as_mut() {
                    palette.activate_address(&intent.address.resource, &intent.address.action);
                }
                dispatched = dispatched.saturating_add(1);
                continue;
            }
            let json = format!(
                "{}{}",
                qos_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(intent.arguments.clone())
            );
            let actor = self.local_author.0.clone();
            match self.invoke_resource_action(&intent.address, Some(&json), &actor, false, None) {
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
            .directed_invocations
            .values_mut()
            .find(|pending| pending.wire_id == wire_id)
        {
            pending.cancellation_requested = true;
        }
    }

    /// Give retained widgets a non-blocking lifecycle tick and forward every
    /// progress/item/terminal update to the broker. A terminal update removes
    /// the job before it is sent, making terminal delivery at-most-once even
    /// if a widget accidentally returns the same update again.
    fn poll_directed_invocations(&mut self) -> bool {
        let invocations = self
            .directed_invocations
            .iter()
            .map(|(invocation, pending)| (*invocation, *pending))
            .collect::<Vec<_>>();
        let mut changed = false;
        for (invocation, pending) in invocations {
            let Some(mut update) = self.root_widget.poll_resource_operation(
                invocation,
                pending.cancellation_requested,
                &self.state.doc,
                &self.state.root,
            ) else {
                continue;
            };
            if update.invocation() != invocation
                || matches!(update, qos_actions::InvocationUpdate::Accepted { .. })
            {
                update = qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
                        code: "qos.invalid_lifecycle_update".into(),
                        message: "operation handler returned an invalid lifecycle update".into(),
                        details: None,
                        retryable: false,
                    },
                };
            }
            if update.is_terminal() {
                self.directed_invocations.remove(&invocation);
            }
            self.collab_manager.send(CollabRequest::UpdateInvocation {
                id: pending.wire_id,
                update,
            });
            changed = true;
        }
        let local_invocations = self.local_invocations.iter().copied().collect::<Vec<_>>();
        for invocation in local_invocations {
            let Some(update) = self.root_widget.poll_resource_operation(
                invocation,
                false,
                &self.state.doc,
                &self.state.root,
            ) else {
                continue;
            };
            if update.is_terminal() || update.invocation() != invocation {
                self.local_invocations.remove(&invocation);
            }
            changed = true;
        }
        changed
    }

    pub fn open_action_palette(&mut self) {
        let resources = self
            .collect_resources()
            .into_iter()
            .filter_map(|resource| self.filter_resource_for_local_actor(resource))
            .collect::<Vec<_>>();
        let providers = self.root_widget.search_providers();
        let mut palette = crate::action_palette::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.set_providers(providers);
        palette.open();
        self.action_palette = Some(palette);
    }

    fn filter_resource_for_local_actor(
        &self,
        mut resource: qos_actions::Resource,
    ) -> Option<qos_actions::Resource> {
        use collab_types::caps::Verb;
        resource.actions.retain(|action| {
            let verb = match action.contract.permission {
                qos_actions::PermissionRequirement::None => return true,
                qos_actions::PermissionRequirement::Read => Verb::Read,
                qos_actions::PermissionRequirement::Comment => Verb::Comment,
                qos_actions::PermissionRequirement::Write => Verb::Write,
                qos_actions::PermissionRequirement::Invite => Verb::Invite,
                qos_actions::PermissionRequirement::Admin => Verb::Admin,
            };
            self.permission_ctx
                .check(&crate::permissions::PermissionScope::app(
                    crate::collaboration::local_app_id(),
                    verb,
                ))
        });
        resource.children = resource
            .children
            .into_iter()
            .filter_map(|child| self.filter_resource_for_local_actor(child))
            .collect();
        (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
    }

    pub fn close_action_palette(&mut self) {
        self.action_palette = None;
    }

    pub fn action_palette(&self) -> Option<&crate::action_palette::ActionPalette> {
        self.action_palette.as_ref()
    }

    fn action_palette_handle_key(&mut self, event: &WindowEvent) -> bool {
        let Some(palette) = self.action_palette.as_mut() else {
            return false;
        };
        let root = self.state.root.clone();
        let before = self.state.doc.get_heads();
        let consumed = palette.handle_key(event, &self.modifiers, &mut self.state.doc, &root);
        // An operation row may have fired. Document mutations go through the
        // deterministic executor; effects/views route to the owning widget.
        let pending = self
            .action_palette
            .as_mut()
            .and_then(crate::action_palette::ActionPalette::take_operation);
        if let Some(pending) = pending {
            if pending.kind == qos_actions::OperationKind::Document {
                let result = self
                    .action_registry
                    .decode(&pending.encoded)
                    .and_then(|action| {
                        let actor = self.state.doc.get_actor().to_bytes().to_vec();
                        qos_actions::dispatch_by_with_context(
                            &mut self.state.doc,
                            action.as_ref(),
                            pending.encoded,
                            self.action_transport.as_mut(),
                            Some(&actor),
                            pending.context,
                        )
                    });
                match result {
                    Ok(d) if d.mutated_local() => {
                        self.state.record_committed_action(before, None, None);
                        self.needs_sync = true;
                        self.layout_dirty = true;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "action palette dispatch failed"),
                }
            } else {
                let result = if pending.encoded.type_id == crate::a11y_tree::SEMANTIC_OPERATION_TYPE
                {
                    self.invoke_semantic_operation_encoded(&pending.encoded)
                } else {
                    self.root_widget.invoke_resource_operation(
                        pending.kind,
                        &pending.encoded.type_id,
                        &pending.encoded.payload,
                        &self.state.doc,
                        &root,
                    )
                };
                match result {
                    Ok(true) => {
                        self.commit_action();
                        self.layout_dirty = true;
                    }
                    Ok(false) => {
                        tracing::warn!(
                            type_id = %pending.encoded.type_id,
                            "no widget handled exported resource operation"
                        );
                    }
                    Err(error) => tracing::warn!(%error, "resource operation failed"),
                }
            }
        }
        // A content row may have fired: hand its id to the app to navigate,
        // recording an undo point only if it mutated the shared doc.
        let navigation = self
            .action_palette
            .as_mut()
            .and_then(super::action_palette::ActionPalette::take_navigation);
        if let Some(action_id) = navigation {
            let nav_before = self.state.doc.get_heads();
            self.root_widget
                .activate_search_result(&action_id, &mut self.state.doc, &root);
            if self.state.doc.get_heads() != nav_before {
                self.state.record_committed_action(nav_before, None, None);
            }
            self.needs_sync = true;
            self.layout_dirty = true;
        }
        // If the palette closed (Escape, or a non-prompted selection), drop it.
        if self.action_palette.as_ref().is_some_and(|p| !p.is_open()) {
            self.action_palette = None;
        }
        consumed
    }

    fn screen_reader_env_enabled() -> bool {
        for key in ["QOS_A11Y_SCREEN_READER", "QOS_SCREEN_READER"] {
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
        Self::bool_env(&["QOS_A11Y_HIGH_CONTRAST", "QOS_HIGH_CONTRAST"])
    }

    fn reduce_motion_env_enabled() -> bool {
        Self::bool_env(&["QOS_A11Y_REDUCE_MOTION", "QOS_REDUCE_MOTION"])
    }

    /// A bad value is ignored rather than crashing the app at startup.
    fn text_scale_env() -> Option<f32> {
        for key in ["QOS_A11Y_TEXT_SCALE", "QOS_TEXT_SCALE"] {
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
        for key in ["QOS_UI_SCALE", "QOS_A11Y_UI_SCALE"] {
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
        let mut s = self.a11y;
        s.screen_reader = self.screen_reader.is_some();
        s
    }

    pub fn text_scale(&self) -> f32 {
        self.text_scale
    }

    /// Layout grows to fit so text never clips; the value is clamped to a
    /// sane range by the theme.
    pub fn set_text_scale(&mut self, factor: f32) {
        self.text_scale = factor;
        self.rebuild_theme();
        self.layout_dirty = true;
        self.structure_dirty = true;
    }

    pub fn ui_scale(&self) -> f32 {
        self.ui_scale
    }

    pub fn set_ui_scale(&mut self, factor: f32) {
        self.ui_scale = factor;
        self.rebuild_theme();
        self.layout_dirty = true;
        self.structure_dirty = true;
    }

    /// Override the host-detected input environment. Useful for platform
    /// shells and deterministic tests; normal apps do not need to call it.
    pub fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        if self.interaction == environment {
            return;
        }
        self.interaction = environment;
        self.root_widget.set_interaction_environment(environment);
        self.layout_dirty = true;
    }

    pub fn interaction_environment(&self) -> crate::interaction::InteractionEnvironment {
        self.interaction
    }

    /// Override platform HIG conventions for a native shell or deterministic
    /// cross-platform test. Ordinary apps use the host default.
    pub fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        if self.platform == conventions {
            return;
        }
        self.platform = conventions;
        self.root_widget.set_platform_conventions(conventions);
        self.layout_dirty = true;
    }

    pub fn platform_conventions(&self) -> crate::platform::PlatformConventions {
        self.platform
    }

    pub fn high_contrast(&self) -> bool {
        self.a11y.high_contrast
    }

    /// Off restores the captured base theme, not a hard-coded default.
    pub fn set_high_contrast(&mut self, on: bool) {
        if self.a11y.high_contrast == on {
            return;
        }
        self.a11y.high_contrast = on;
        self.rebuild_theme();
        self.layout_dirty = true;
        self.structure_dirty = true;
    }

    pub fn toggle_high_contrast(&mut self) -> bool {
        self.set_high_contrast(!self.a11y.high_contrast);
        self.a11y.high_contrast
    }

    pub fn reduce_motion(&self) -> bool {
        self.a11y.reduce_motion
    }

    pub fn set_reduce_motion(&mut self, on: bool) {
        if self.a11y.reduce_motion == on {
            return;
        }
        self.a11y.reduce_motion = on;
        self.rebuild_theme();
    }

    /// Low-vision preferences are re-applied on top, so changing the base
    /// theme doesn't discard them.
    pub fn set_theme(&mut self, theme: Theme) {
        self.base_theme = theme;
        self.rebuild_theme();
        self.layout_dirty = true;
        self.structure_dirty = true;
    }

    /// Disabling stops in-flight speech and returns every narration call
    /// site to a no-op.
    pub fn set_screen_reader_enabled(&mut self, enabled: bool) {
        match (enabled, self.screen_reader.is_some()) {
            (true, false) => {
                self.screen_reader = Some(Self::configured_screen_reader());
                self.announce_current_focus();
            }
            (false, true) => {
                if let Some(sr) = self.screen_reader.as_mut() {
                    sr.stop();
                }
                self.screen_reader = None;
                self.live_region_last.clear();
            }
            _ => {}
        }
    }

    pub fn screen_reader_enabled(&self) -> bool {
        self.screen_reader.is_some()
    }

    /// Installs a caller-provided reader (e.g. over a test `NullSpeaker` so
    /// headless tests can assert on spoken phrases).
    #[doc(hidden)]
    pub fn set_screen_reader(&mut self, reader: Option<crate::screen_reader::ScreenReader>) {
        self.screen_reader = reader;
    }

    #[doc(hidden)]
    pub fn screen_reader(&self) -> Option<&crate::screen_reader::ScreenReader> {
        self.screen_reader.as_ref()
    }

    fn announce_current_focus(&mut self) {
        let Some(id) = self.focused_node else { return };
        let Some(&w_ptr) = self.node_map.get(&id) else {
            return;
        };
        // SAFETY: same node_map dereference invariant the rest of `App` relies
        // on — pointers are valid for the duration of an event dispatch (the
        // tree is only rebuilt between frames). Mirrors `navigate_focus`.
        let node = unsafe { (*w_ptr).accessibility() };
        if let (Some(node), Some(sr)) = (node, self.screen_reader.as_mut()) {
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

        if self.screen_reader.is_none() {
            return false;
        }
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };

        if let Key::Named(NamedKey::Insert) = ke.logical_key {
            self.screen_reader_modifier_down = ke.state == ElementState::Pressed;
            return false;
        }

        if ke.state != ElementState::Pressed || !self.screen_reader_modifier_down {
            return false;
        }

        // Handled before the named-key arm since winit delivers H/F as
        // character keys, not named keys.
        if let Key::Character(ref c) = ke.logical_key {
            let shift = self.modifiers.state().shift_key();
            match c.to_ascii_lowercase().as_str() {
                "h" => {
                    if let Some(sr) = self.screen_reader.as_mut() {
                        if shift {
                            sr.read_prev_heading(self.root_widget.as_ref());
                        } else {
                            sr.read_next_heading(self.root_widget.as_ref());
                        }
                    }
                    return true;
                }
                "f" => {
                    if let Some(sr) = self.screen_reader.as_mut() {
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
                if let Some(sr) = self.screen_reader.as_mut() {
                    sr.read_all(self.root_widget.as_ref());
                }
                true
            }
            NamedKey::ArrowRight => {
                if let Some(sr) = self.screen_reader.as_mut() {
                    sr.read_next(self.root_widget.as_ref());
                }
                true
            }
            NamedKey::ArrowLeft => {
                if let Some(sr) = self.screen_reader.as_mut() {
                    sr.read_prev(self.root_widget.as_ref());
                }
                true
            }
            NamedKey::ArrowUp => {
                self.announce_current_focus();
                true
            }
            NamedKey::Escape => {
                if let Some(sr) = self.screen_reader.as_mut() {
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

    /// Split from [`Self::reconcile_fullscreen`] so the App→root notification
    /// wiring is unit-testable without a live winit window.
    fn note_actual_fullscreen(&mut self, actual: bool) -> bool {
        if actual == self.fullscreen_active {
            return false;
        }
        self.fullscreen_active = actual;
        self.root_widget.on_fullscreen_changed(actual);
        self.needs_sync = true;
        self.layout_dirty = true;
        true
    }

    /// Level-triggered: a no-op once the user's choice is caught up.
    pub(crate) fn reconcile_appearance(&mut self) -> bool {
        if !self.appearance_reconcile {
            return false;
        }
        let Some(intent) = find_appearance_intent(self.root_widget.as_ref()) else {
            return false;
        };
        let mut changed = false;
        if intent.base_theme.name != self.base_theme.name {
            self.base_theme = intent.base_theme;
            changed = true;
        }
        if intent.high_contrast != self.a11y.high_contrast {
            self.a11y.high_contrast = intent.high_contrast;
            changed = true;
        }
        if intent.reduce_motion != self.a11y.reduce_motion {
            self.a11y.reduce_motion = intent.reduce_motion;
            changed = true;
        }
        if (intent.text_scale - self.text_scale).abs() > f32::EPSILON {
            self.text_scale = intent.text_scale;
            changed = true;
        }
        if (intent.ui_scale - self.ui_scale).abs() > f32::EPSILON {
            self.ui_scale = intent.ui_scale;
            changed = true;
        }
        if changed {
            self.rebuild_theme();
            self.layout_dirty = true;
            self.structure_dirty = true;
        }
        changed
    }

    fn reconcile_configured_theme(&mut self) -> bool {
        if self.last_theme_store_check.elapsed() < Duration::from_millis(750) {
            return false;
        }
        self.last_theme_store_check = Instant::now();
        let stamp = settings_store::settings_path()
            .and_then(|path| std::fs::metadata(path).ok()?.modified().ok());
        if stamp == self.theme_store_stamp {
            return false;
        }
        self.theme_store_stamp = stamp;
        self.base_theme = Theme::configured();
        // Screen reader is a device/personal setting, not a per-application
        // opt-in. Every qOS app watches the same file and changes its real
        // narrator lifecycle when Personal Settings changes it.
        let configured_reader = Self::persisted_accessibility_bool("screen_reader")
            || Self::screen_reader_env_enabled();
        self.set_screen_reader_enabled(configured_reader);
        if let Some(reader) = self.screen_reader.as_mut() {
            Self::configure_screen_reader(reader);
        }
        self.a11y.screen_reader = self.screen_reader.is_some();
        self.rebuild_theme();
        self.layout_dirty = true;
        self.structure_dirty = true;
        true
    }

    /// The smallest-area matching rect wins, so a nested child beats its
    /// enclosing container. Works off `node_map` rather than walking
    /// `children` so it still reaches widgets nested under containers with
    /// private child storage.
    fn resolve_cursor(&self, point: Point) -> Option<CursorIcon> {
        let mut best: Option<(f64, CursorIcon)> = None;
        for &w_ptr in self.node_map.values() {
            // SAFETY: same invariant the rest of `App` relies on — the
            // `node_map` pointers are valid for the duration of an event
            // dispatch (the tree is only rebuilt between frames, never
            // mid-`window_event`). This mirrors the existing `node_map`
            // dereferences in `navigate_focus` / focus handling.
            let w = unsafe { &*w_ptr };
            let rect = w.get_rect();
            if rect.contains(point)
                && let Some(icon) = w.cursor_shape(point)
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
        if !self.interaction.supports_hover() {
            return None;
        }
        let mut best: Option<(f64, vello::kurbo::Rect, String)> = None;
        for &w_ptr in self.node_map.values() {
            // SAFETY: identical invariant to `resolve_cursor` — `node_map`
            // pointers are valid for the duration of an event dispatch.
            let w = unsafe { &*w_ptr };
            let rect = w.get_rect();
            if !rect.contains(point) {
                continue;
            }
            if let Some(tip) = w.tooltip() {
                if tip.is_empty() {
                    continue;
                }
                let area = (rect.width() * rect.height()).max(0.0);
                if best.as_ref().is_none_or(|(a, _, _)| area < *a) {
                    best = Some((area, rect, tip.to_string()));
                }
            }
        }
        best.map(|(_, rect, label)| (rect, label))
    }

    /// Called once per event-loop wakeup, so it must be cheap and
    /// non-blocking.
    pub fn set_frame_tick<F>(&mut self, tick: F)
    where
        F: FnMut(&mut PresenceCtx) + Send + Sync + 'static,
    {
        self.frame_tick = Some(Box::new(tick));
    }

    pub fn set_presence_broadcast_sink<F>(&mut self, sink: F)
    where
        F: Fn(&PresenceKey, &PresencePayload) + Send + Sync + 'static,
    {
        self.presence_broadcast_sink = Some(Box::new(sink));
    }

    pub fn clear_presence_broadcast_sink(&mut self) {
        self.presence_broadcast_sink = None;
    }

    pub fn set_pointer_space(&mut self, space: crate::presence::PointerSpace) {
        self.pointer_space = Some(space);
    }

    fn local_pointer_key(&self) -> PresenceKey {
        match self.pointer_space.clone() {
            Some(space) => PresenceKey::Pointer { space },
            None => PresenceKey::local_surface_pointer(),
        }
    }

    /// Decides *whether* to publish and under what scope, not what space the
    /// pointer is in — see [`Self::set_pointer_space`].
    pub fn set_presence_broadcast_scope(&mut self, key: Option<PresenceKey>) {
        self.presence_broadcast_scope.clone_from(&key);
        // Reset the throttle so a new scope's first move fires immediately.
        self.last_pointer_broadcast = None;
        // Announce arrival so peers already in the room reply with a full
        // snapshot (the late-join handshake).
        if let Some(k) = key {
            if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                crate::diag!(
                    "[app] set scope; collab connected={}; broadcasting Arrived",
                    self.collab_manager.is_connected()
                );
            }
            self.broadcast_presence(k, crate::collaboration::PresencePayload::Arrived);
        }
    }

    pub fn presence_broadcast_scope(&self) -> Option<&PresenceKey> {
        self.presence_broadcast_scope.as_ref()
    }

    pub fn presence_ctx(&self) -> &PresenceCtx {
        &self.presence_ctx
    }
    pub fn presence_ctx_mut(&mut self) -> &mut PresenceCtx {
        &mut self.presence_ctx
    }

    pub fn set_followed_peer(&mut self, user_id: Option<String>) {
        self.followed_peer = user_id;
    }

    pub fn followed_peer(&self) -> Option<&str> {
        self.followed_peer.as_deref()
    }

    pub fn followed_peer_target(&self) -> Option<[f32; 2]> {
        let id = self.followed_peer.as_deref()?;
        self.presence_ctx.peer_pointer(id)
    }

    /// A miss leaves the current follow target untouched.
    pub fn try_follow_click(
        &mut self,
        viewport: vello::kurbo::Rect,
        point: vello::kurbo::Point,
    ) -> bool {
        if let Some(uid) = self.presence_ctx.hit_test_offscreen_mist(viewport, point) {
            self.followed_peer = Some(uid);
            true
        } else {
            false
        }
    }

    pub fn permission_ctx(&self) -> &PermissionContext {
        &self.permission_ctx
    }
    pub fn set_permission_ctx(&mut self, ctx: PermissionContext) {
        self.permission_ctx = ctx;
        self.refresh_control_action_availability();
        self.layout_dirty = true;
    }

    /// The server attributes the broadcast to this user's id automatically,
    /// so callers only specify what changed.
    pub fn broadcast_presence(
        &mut self,
        key: crate::presence::PresenceKey,
        payload: crate::collaboration::PresencePayload,
    ) {
        // Fan out to the optional out-of-band sink first (the shell's Iroh
        // bridge) so a slow socket never delays the network path.
        if let Some(sink) = self.presence_broadcast_sink.as_ref() {
            sink(&key, &payload);
        }
        self.collab_manager
            .send(CollabRequest::Presence { key, payload });
    }

    /// Live-edit path (the late-join handshake still ships a full snapshot).
    fn broadcast_doc_delta(&mut self) {
        if !self.collab_manager.is_connected() {
            return;
        }
        let delta = self.state.save_incremental();
        if !delta.is_empty() {
            if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                crate::diag!("[app] broadcast doc delta {}B", delta.len());
            }
            self.collab_manager
                .send(crate::collaboration::CollabRequest::DocumentIncremental { data: delta });
        }
    }

    fn broadcast_focus_presence(&mut self) {
        use crate::collaboration::PresencePayload;
        let current = self
            .focused_node
            .and_then(|id| self.node_map.get(&id).copied())
            .and_then(|w| unsafe { (*w).presence_payload() });
        if current != self.last_focus_presence {
            // When the caret advances forward under the same scope, publish
            // `Typing` for the inserted run so peers light up a fading
            // highlighter. Backspace/left-arrow and scope changes don't
            // qualify.
            if let (
                Some((new_key, PresencePayload::Caret { pos: new_pos })),
                Some((old_key, PresencePayload::Caret { pos: old_pos })),
            ) = (&current, &self.last_focus_presence)
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
            self.last_focus_presence = current;
        }
    }

    /// Unlike focus presence, this is polled on the root regardless of
    /// focus.
    fn broadcast_location_presence(&mut self) {
        let current = self.root_widget.presence_payload();
        if current != self.last_location_presence {
            if let Some((key, payload)) = current.clone() {
                self.broadcast_presence(key, payload);
            }
            self.last_location_presence = current;
        }
    }

    /// The subset has no `PresencePayload` variant of its own — it rides the
    /// existing presence frame inside a
    /// [`PresenceKey::view`](crate::presence::PresenceKey::view) key with an
    /// inert paired payload; the receiver recognises the key and decodes the
    /// subset instead.
    fn broadcast_view_presence(&mut self) {
        if !self.collab_manager.is_connected() && self.presence_broadcast_sink.is_none() {
            return;
        }
        let view = <Self as crate::view::FreezeThaw>::capture_view(self);
        let subset = crate::view::ViewSubset::from_view(&view);
        if self.last_view_presence.as_ref() == Some(&subset) {
            return;
        }
        self.broadcast_presence(
            crate::presence::PresenceKey::view(&subset),
            // Inert: the receiver dispatches on the key before inspecting
            // the payload; `Arrived` because the SDK transport relays it
            // with the routing key intact.
            crate::collaboration::PresencePayload::Arrived,
        );
        self.last_view_presence = Some(subset);
    }

    /// Following a peer who hasn't published a View yet latches the intent
    /// without moving anything; the viewport snaps to them once their first
    /// View arrives.
    pub fn follow(&mut self, peer_id: impl Into<String>) {
        self.followed_peer = Some(peer_id.into());
        self.apply_follow();
    }

    pub fn unfollow(&mut self) {
        self.followed_peer = None;
    }

    fn apply_follow(&mut self) {
        let Some(id) = self.followed_peer.clone() else {
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
        self.layout_dirty = true;
        self.needs_sync = true;
    }

    /// Lets integration tests drive the collab pump without a real winit
    /// event loop.
    #[doc(hidden)]
    pub fn poll_and_apply_collab_events(&mut self) {
        let events = self.collab_manager.poll();
        for event in events {
            self.apply_collab_event_for_test(event);
        }
    }

    fn apply_signed_document_envelope(&mut self, envelope: &collab_types::SignedOp) {
        let applied = match collab_types::DocumentMutation::decode(&envelope.payload) {
            Ok(collab_types::DocumentMutation::Snapshot(data)) => {
                if let Ok(mut incoming) = State::load(&data) {
                    let _ = self.state.doc.merge(&mut incoming.doc);
                    true
                } else {
                    false
                }
            }
            Ok(collab_types::DocumentMutation::Incremental(data)) => {
                self.state.load_incremental(&data).is_ok()
            }
            Err(error) => {
                tracing::warn!(%error, "collab: broker delivered an untyped signed document op");
                false
            }
        };
        if applied {
            self.needs_sync = true;
            self.layout_dirty = true;
        }
    }

    /// Shared by the per-frame tick and the headless test path. Returns
    /// `true` iff at least one event was processed.
    pub fn pump_collab_presence(&mut self) -> bool {
        let events = self.collab_manager.poll();
        let mut any = false;
        for event in events {
            any = true;
            match event {
                CollabEvent::Context { collaborators, .. } => {
                    self.apply_collab_context(collaborators);
                }
                CollabEvent::FollowChanged { peer_id } => match peer_id {
                    Some(peer_id) => self.follow(peer_id),
                    None => self.unfollow(),
                },
                CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                    if let Some(cursor) = self.state.collab.cursors.get_mut(&user_id) {
                        cursor.x = x;
                        cursor.y = y;
                    }
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
                CollabEvent::Document { envelope, .. } => {
                    // The broker already validated the envelope (signature +
                    // role table + replay defence); the toolkit trusts it.
                    if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                        crate::diag!(
                            "[app] recv document from {} ({}B payload)",
                            "<peer>",
                            envelope.payload.len()
                        );
                    }
                    self.apply_signed_document_envelope(&envelope);
                }
                CollabEvent::ScrollbackBatch {
                    artifact: _,
                    ops,
                    exhausted: _,
                } => {
                    // Broker pre-validated each envelope, trusted the same
                    // way as a live document mutation.
                    if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                        crate::diag!("[app] recv ScrollbackBatch ({} ops)", ops.len());
                    }
                    for envelope in ops {
                        self.apply_signed_document_envelope(&envelope);
                    }
                }
                CollabEvent::Invocation {
                    id,
                    resource,
                    action,
                    argument,
                    actor,
                } => {
                    self.invoke_published_action(id, resource, action, argument.as_deref(), &actor);
                }
                CollabEvent::InvocationCancellation { id } => {
                    self.request_invocation_cancellation(id);
                }
                // The generic catalogue/client responses are consumed by
                // widgets such as Agent through their collaboration event
                // hook; an ordinary application has no local state to apply.
                CollabEvent::ActionCatalogue { .. }
                | CollabEvent::PublishedInvocationResult { .. } => {}
            }
        }
        any
    }

    /// The self row carries the address others use for this participant (so
    /// a locally-derived id is recognised in a room-keyed roster) and the
    /// host-assigned name; it never enters the roster itself.
    fn apply_collab_context(&mut self, collaborators: Vec<crate::collaboration::Collaborator>) {
        if let Some(me) = collaborators.iter().find(|c| c.is_self) {
            self.presence_ctx
                .set_self_participant(crate::permissions::UserId(me.id.clone()), me.name.clone());
        }
        // Filtered to others: letting the self row through would put the
        // local user in every list built from presence.
        for c in collaborators.into_iter().filter(|c| !c.is_self) {
            // Relabel only — the roster doesn't say where a peer's pointer
            // is, so it must not mint one at the origin.
            if let Some(cursor) = self.state.collab.cursors.get_mut(&c.id) {
                cursor.name.clone_from(&c.name);
            }
            self.presence_ctx
                .set_display_name(crate::permissions::UserId(c.id.clone()), c.name.clone());
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
            // Named after the publisher so local Ctrl+Z walks past it
            // instead of through it.
            self.state.record_committed_action_authored(
                before,
                None,
                None,
                Some(crate::permissions::UserId(from_user_id.to_string())),
            );
            self.needs_sync = true;
            self.structure_dirty = true;
        }
    }

    /// Bypasses the collab manager's socket/poll path — for tests using a
    /// `MemoryRelay`.
    #[doc(hidden)]
    pub fn apply_collab_event_for_test(&mut self, event: CollabEvent) {
        match event {
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key,
                payload,
            } => {
                self.apply_remote_presence(user_id, key, payload);
            }
            CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                let entry = self.state.collab.cursors.entry(user_id.clone()).or_insert(
                    crate::state::RemoteCursor {
                        x: 0.0,
                        y: 0.0,
                        color: Color::WHITE,
                        name: String::new(),
                    },
                );
                entry.x = x;
                entry.y = y;
                self.presence_ctx
                    .upsert_remote_cursor(user_id, [x, y], None);
            }
            CollabEvent::Document { envelope, .. } => {
                self.apply_signed_document_envelope(&envelope);
            }
            CollabEvent::FollowChanged { peer_id } => match peer_id {
                Some(peer_id) => self.follow(peer_id),
                None => self.unfollow(),
            },
            CollabEvent::Invocation {
                id,
                resource,
                action,
                argument,
                actor,
            } => self.invoke_published_action(id, resource, action, argument.as_deref(), &actor),
            CollabEvent::InvocationCancellation { id } => {
                self.request_invocation_cancellation(id);
            }
            _ => {}
        }
    }

    pub fn local_author(&self) -> &crate::permissions::UserId {
        &self.local_author
    }

    #[doc(hidden)]
    pub fn state_mut_for_test(&mut self) -> &mut crate::state::State {
        &mut self.state
    }

    fn update_focusable_order(&mut self) {
        fn collect_focusable(
            node_id: NodeId,
            widget: &dyn Widget,
            taffy: &TaffyTree<()>,
            order: &mut Vec<NodeId>,
        ) {
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

        self.focusable_order.clear();
        let Some(root) = self.root_node else { return };

        let trap = find_trap_focusables(root, self.root_widget.as_ref(), &self.taffy);
        let trap_active = trap.is_some();

        if let Some(trapped) = trap {
            self.focusable_order = trapped;
        } else {
            collect_focusable(
                root,
                self.root_widget.as_ref(),
                &self.taffy,
                &mut self.focusable_order,
            );
        }

        match (self.focus_trap_active, trap_active) {
            (false, true) => {
                // Modal just opened: remember prior focus, move into the trap.
                self.focus_before_trap = self.focused_node;
                if let Some(&first) = self.focusable_order.first() {
                    self.set_focused_node(Some(first));
                } else {
                    // No focusable content — drop focus so Tab can't reach
                    // the content behind the backdrop.
                    self.set_focused_node(None);
                }
            }
            (true, false) => {
                // Modal closed: restore only if that element still exists.
                let restore = self
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
        self.focus_trap_active = trap_active;
    }

    /// The decode lives on [`crate::presence::PresenceCtx`] so the
    /// compositor reads presence the same way an app does; what stays here
    /// is what only a document holder can do (follow a peer's viewport,
    /// answer a late joiner with full state).
    fn apply_remote_presence(
        &mut self,
        user_id: String,
        key: crate::presence::PresenceKey,
        payload: crate::collaboration::PresencePayload,
    ) {
        use crate::presence::RemotePresenceEffect;
        match self
            .presence_ctx
            .apply_remote_presence(user_id, key, payload)
        {
            RemotePresenceEffect::None => {}
            RemotePresenceEffect::PeerViewChanged => self.apply_follow(),
            RemotePresenceEffect::PeerArrived => {
                let data = self.state.save();
                if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                    crate::diag!("[app] recv Arrived; replying full state {}B", data.len());
                }
                self.collab_manager
                    .send(crate::collaboration::CollabRequest::DocumentSnapshot { data });
            }
        }
    }

    fn apply_undo_entry(&mut self, entry: crate::state::UndoEntry) {
        if let Some(idx) = entry.focused_node_idx {
            if let Some(&id) = self.focusable_order.get(idx) {
                self.set_focused_node(Some(id));
                if let Some(metadata) = entry.widget_metadata {
                    if let Some(&w_ptr) = self.node_map.get(&id) {
                        unsafe {
                            (*w_ptr).set_state(&metadata);
                        }
                    }
                    self.last_stable_metadata = Some(metadata);
                }
                self.last_stable_focus_idx = Some(idx);
            }
        } else {
            self.set_focused_node(None);
            self.last_stable_focus_idx = None;
            self.last_stable_metadata = None;
        }
        self.layout_dirty = true;
        self.needs_sync = true;
    }

    fn commit_action(&mut self) {
        // Called after every handled event, many of which (e.g. a
        // selection-drag `CursorMoved`) mutate only ephemeral widget state,
        // not the CRDT — `changed` lets those skip persisting/syncing.
        let context = self.root_widget.take_commit_context();
        let changed = self.state.commit_action_authored_with_context(
            self.last_stable_focus_idx,
            self.last_stable_metadata.clone(),
            Some(self.local_author.clone()),
            context,
        );
        self.last_stable_focus_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.last_stable_metadata = self.focused_node.and_then(|id| {
            self.node_map
                .get(&id)
                .and_then(|&w_ptr| unsafe { (*w_ptr).get_state() })
        });

        // Incremental delta, not the whole document — serialising per
        // keystroke scales with document size. `UndoLog::apply_inverse`
        // writes the inverse as new changes, so a delta carries undo/redo
        // like any other edit.
        if changed {
            self.broadcast_doc_delta();
        }
    }

    fn set_focused_node(&mut self, new_id: Option<NodeId>) {
        if self.focused_node != new_id {
            if let Some(old_id) = self.focused_node
                && let Some(&w_ptr) = self.node_map.get(&old_id)
            {
                unsafe {
                    (*w_ptr).set_focus(false);
                }
            }
            self.focused_node = new_id;
            if let Some(id) = new_id
                && let Some(&w_ptr) = self.node_map.get(&id)
            {
                unsafe {
                    (*w_ptr).set_focus(true);
                }
                // Interrupts the previous utterance so rapid arrow/Tab
                // navigation stays responsive.
                if self.screen_reader.is_some() {
                    let node = unsafe { (*w_ptr).accessibility() };
                    if let (Some(node), Some(sr)) = (node, self.screen_reader.as_mut()) {
                        sr.announce_focus(&node);
                    }
                }
            }
            self.layout_dirty = true;
            self.needs_sync = true;
        }
    }

    /// The platform IME contract for the focused widget. A field opts in via
    /// [`Widget::ime_purpose`]; buttons and other focusable controls therefore
    /// never activate `text-input-v3` accidentally.
    fn ime_window_state(&self) -> Option<ImeWindowState> {
        let id = self.focused_node?;
        let &widget = self.node_map.get(&id)?;
        let purpose = unsafe { (*widget).ime_purpose() }?;
        let rect = unsafe { (*widget).ime_cursor_rect() }?;
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
        self.screen_reader.as_ref()?;
        let id = self.focused_node?;
        let &w_ptr = self.node_map.get(&id)?;
        let node = unsafe { (*w_ptr).accessibility() }?;
        Some((id, crate::screen_reader::describe_state(&node)))
    }

    /// Only fires if focus did NOT move — a move is narrated separately by
    /// [`set_focused_node`].
    fn announce_state_change(&mut self, pre: Option<(NodeId, String)>) {
        let Some((pre_id, pre_state)) = pre else {
            return;
        };
        if self.focused_node != Some(pre_id) {
            return;
        }
        if self.screen_reader.is_none() {
            return;
        }
        let now = match self.node_map.get(&pre_id) {
            Some(&w_ptr) => match unsafe { (*w_ptr).accessibility() } {
                Some(node) => crate::screen_reader::describe_state(&node),
                None => return,
            },
            None => return,
        };
        if now.is_empty() || now == pre_state {
            return;
        }
        if let Some(sr) = self.screen_reader.as_mut() {
            sr.announce_value_change(now);
        }
    }

    fn text_review_snapshot(&self) -> Option<(NodeId, usize, String)> {
        self.screen_reader.as_ref()?;
        let id = self.focused_node?;
        let &w_ptr = self.node_map.get(&id)?;
        let (caret, text) = unsafe { (*w_ptr).text_review() }?;
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
        if self.screen_reader.is_none() {
            return;
        }
        let Some(&w_ptr) = self.node_map.get(&pre_id) else {
            return;
        };
        let Some((now_caret, now_text)) = (unsafe { (*w_ptr).text_review() }) else {
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
            if let (Some(ch), Some(sr)) = (ch, self.screen_reader.as_mut()) {
                sr.echo_char(ch);
            }
        } else if let Some(word) = word_at(&now_text, now_caret)
            && let Some(sr) = self.screen_reader.as_mut()
        {
            sr.echo_word(&word);
        }
    }

    /// Announces changed [live regions](crate::a11y::AccessNode::live)
    /// without moving focus.
    fn scan_live_regions(&mut self) {
        fn visit(w: &dyn Widget, out: &mut Vec<(String, crate::a11y::Politeness, String)>) {
            if let Some(node) = w.accessibility()
                && let (Some(politeness), Some(name)) = (node.live, node.name.clone())
            {
                out.push((name, politeness, crate::screen_reader::describe(&node)));
            }
            for child in w.children() {
                visit(child, out);
            }
        }
        if self.screen_reader.is_none() {
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
            let changed = self.live_region_last.get(&key) != Some(&text);
            self.live_region_last.insert(key, text.clone());
            // Clearing a region to empty updates the snapshot silently.
            if changed && !text.is_empty() {
                to_announce.push((politeness, text));
            }
        }
        // Drop snapshots for regions that left the tree, so a region that
        // reappears with the same text announces afresh.
        self.live_region_last
            .retain(|k, _| current_keys.contains(k));

        if let Some(sr) = self.screen_reader.as_mut() {
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
        let current_rect = if let Some(&w_ptr) = self.node_map.get(&current_id) {
            unsafe { (*w_ptr).get_rect() }
        } else {
            return;
        };
        let current_center = current_rect.center();
        let mut best_candidate = None;
        let mut best_score = f64::MAX;
        for &candidate_id in &self.focusable_order {
            if candidate_id == current_id {
                continue;
            }
            if let Some(&candidate_ptr) = self.node_map.get(&candidate_id) {
                let candidate_rect = unsafe { (*candidate_ptr).get_rect() };
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
    /// pointer click-through — gating Tab on it instead either froze focus
    /// inside a dialog or let it escape with no capture.
    fn keyboard_nav_allowed(&self, modal_rect: Option<vello::kurbo::Rect>) -> bool {
        modal_rect.is_none() || self.focus_trap_active
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
    pub fn build_a11y_tree_update(&self, scale: f64) -> accesskit::TreeUpdate {
        let resources = self.collect_declared_resources();
        crate::accesskit_bridge::build_tree_update_with_resources(
            self.root_widget.as_ref(),
            &self.node_map,
            self.focused_node,
            scale,
            &resources,
        )
    }

    fn find_resource_action(
        resources: &[qos_actions::Resource],
        target: accesskit::NodeId,
    ) -> Option<qos_actions::ActionAddress> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|spec| {
                    crate::accesskit_bridge::resource_action_node_id(resource.id.as_str(), &spec.id)
                        == target
                })
                .map(|spec| qos_actions::ActionAddress::new(resource.id.as_str(), &spec.id))
                .or_else(|| Self::find_resource_action(&resource.children, target))
        })
    }

    /// Resolves both retained AccessKit nodes and synthetic semantic children
    /// (virtual rows/cells) to their owning widget. The widget gets first
    /// refusal; synthetic children additionally carry an opaque stable target
    /// produced by that widget. If a retained widget declines, `Action::Focus`
    /// moves keyboard focus and activation returns click coordinates.
    pub fn dispatch_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> Option<(f64, f64)> {
        use crate::a11y::Action;
        use crate::accesskit_bridge::{A11yTarget, resolve_node_id};

        self.refresh_control_action_availability();
        let argument = match action {
            Action::Activate | Action::ToggleCheck => Some(None),
            Action::SetValue(value) => Some(Some(value.as_str())),
            _ => None,
        };
        if let Some(argument) = argument {
            let resources = self.collect_declared_resources();
            if let Some(address) = Self::find_resource_action(&resources, target) {
                let confirmation = qos_actions::door::resolve(&resources, &address)
                    .map(|spec| spec.contract.confirmation.clone())
                    .unwrap_or_default();
                if !matches!(confirmation, qos_actions::ConfirmationPolicy::None) {
                    self.open_action_palette();
                    if let Some(palette) = self.action_palette.as_mut() {
                        palette.activate_address(&address.resource, &address.action);
                    }
                    return None;
                }
                let actor = crate::collaboration::local_user_id().0;
                if self
                    .invoke_resource_action(&address, argument, &actor, false, None)
                    .is_ok()
                {
                    self.needs_sync = true;
                    self.layout_dirty = true;
                }
                return None;
            }
        }

        let resolved = resolve_node_id(self.root_widget.as_ref(), &self.node_map, target)?;
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
        let &w_ptr = self.node_map.get(&taffy_id)?;

        let handled = unsafe {
            if let Some(semantic_target) = semantic_target.as_deref() {
                (*w_ptr).perform_semantic_action(
                    semantic_target,
                    action,
                    &mut self.state.doc,
                    &self.state.root,
                )
            } else if synthetic {
                (*w_ptr).perform_action_at(action, &key_path, &mut self.state.doc, &self.state.root)
            } else {
                (*w_ptr).perform_action(action, &mut self.state.doc, &self.state.root)
            }
        };
        if handled {
            if matches!(action, Action::Focus) {
                self.set_focused_node(Some(taffy_id));
            }
            self.needs_sync = true;
            self.layout_dirty = true;
            return None;
        }

        match action {
            Action::Focus => {
                if synthetic && !ordinal_path.is_empty() {
                    unsafe { (*w_ptr).focus_synthetic_accessibility(&ordinal_path) };
                }
                self.set_focused_node(Some(taffy_id));
                None
            }
            Action::Activate | Action::ToggleCheck => {
                let rect = if synthetic {
                    unsafe { (*w_ptr).synthetic_accessibility_rect(&ordinal_path) }
                        .unwrap_or_else(|| unsafe { (*w_ptr).get_rect() })
                } else {
                    unsafe { (*w_ptr).get_rect() }
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
        let Some(root_node) = self.root_node else {
            return false;
        };
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
            let mut handled = false;
            let mut focus_target = None;
            self.root_widget.handle_event(
                &event,
                1.0,
                &self.modifiers,
                &mut self.state.doc,
                &self.state.root,
                &mut handled,
                &mut focus_target,
                root_node,
                &self.taffy,
            );
            if let Some(id) = focus_target {
                self.set_focused_node(Some(id));
            }
            handled_any |= handled;
        }
        if handled_any {
            self.dispatch_control_invocations();
            self.needs_sync = true;
            self.layout_dirty = true;
        }
        handled_any
    }

    /// Node identity is preserved for everything that recurred, so a
    /// consumer reacts to just the diff.
    pub fn refresh_semantic_tree(&mut self) -> crate::a11y_tree::TreeDelta {
        let resources = self.collect_declared_resources();
        self.semantic_tree.refresh(
            self.root_widget.as_ref(),
            &self.node_map,
            self.focused_node,
            &resources,
        )
    }

    pub fn semantic_tree(&self) -> &crate::a11y_tree::SemanticTree {
        &self.semantic_tree
    }

    /// A [`Verb`](crate::a11y_tree::SemanticActionKind::Verb) routes through
    /// the same [`dispatch_a11y_action`](Self::dispatch_a11y_action) path an
    /// AccessKit `ActionRequest` takes; an
    /// [`App`](crate::a11y_tree::SemanticActionKind::App) affordance
    /// dispatches through [`qos_actions::dispatch`], the same audited path
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
        // Cloned so the mutation below is free to borrow doc/registry/transport.
        let Some(action) = self.semantic_tree.action(id, name).cloned() else {
            return Ok(false);
        };
        match action.kind {
            SemanticActionKind::App(encoded) => self.dispatch_encoded_semantic(encoded),
            SemanticActionKind::AppPrompted { .. } => {
                let arg = arg.unwrap_or("");
                let Some(encoded) = self.semantic_tree.build_prompted(id, name, arg) else {
                    return Ok(false);
                };
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
                    self.semantic_tree.node(id).and_then(|n| {
                        n.widget_taffy_id
                            .map(|taffy| (taffy, n.semantic_target.clone(), n.bounds))
                    })
                else {
                    return Ok(false);
                };
                if let Some(semantic_target) = semantic_target {
                    let taffy_id = NodeId::from(taffy_raw);
                    let Some(&widget) = self.node_map.get(&taffy_id) else {
                        return Ok(false);
                    };
                    let keyed_path = semantic_target
                        .strip_prefix("qos.key-action:")
                        .and_then(|target| target.rsplit_once(':'))
                        .map(|(_, path)| {
                            path.split('/')
                                .map(str::parse::<u64>)
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .transpose()
                        .map_err(|error| format!("invalid keyed semantic action: {error}"))?;
                    let handled = unsafe {
                        if let Some(key_path) = keyed_path {
                            (*widget).perform_action_at(
                                &verb,
                                &key_path,
                                &mut self.state.doc,
                                &self.state.root,
                            )
                        } else {
                            (*widget).perform_semantic_action(
                                &semantic_target,
                                &verb,
                                &mut self.state.doc,
                                &self.state.root,
                            )
                        }
                    };
                    if handled {
                        if matches!(verb, crate::a11y::Action::Focus) {
                            self.set_focused_node(Some(taffy_id));
                        }
                        self.needs_sync = true;
                        self.layout_dirty = true;
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
        encoded: qos_actions::EncodedAction,
    ) -> Result<bool, String> {
        let before = self.state.doc.get_heads();
        let action = self
            .action_registry
            .decode(&encoded)
            .map_err(|e| e.to_string())?;
        let dispatched = qos_actions::dispatch(
            &mut self.state.doc,
            action.as_ref(),
            encoded,
            self.action_transport.as_mut(),
        )
        .map_err(|e| e.to_string())?;
        if dispatched.mutated_local() {
            self.state.record_committed_action(before, None, None);
            self.needs_sync = true;
            self.layout_dirty = true;
        }
        Ok(true)
    }

    /// Lets headless tests stand the App up without spinning a winit loop.
    #[doc(hidden)]
    pub fn init_layout(&mut self) {
        self.refresh_control_action_availability();
        self.node_map.clear();
        self.root_node = Some(self.root_widget.build_layout(
            &mut self.taffy,
            &self.theme,
            &mut self.node_map,
        ));
        self.update_focusable_order();
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
            self.node_map
                .get(&id)
                .and_then(|&w_ptr| unsafe { (*w_ptr).get_state() })
        });
    }

    /// Exposed so headless tests can drive modal open/close without a winit
    /// loop.
    #[doc(hidden)]
    pub fn refresh_focus_order(&mut self) {
        self.root_widget.sync(&mut self.state.doc, &self.state.root);
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
        // Node ids below are about to be invalidated.
        let (prev_focus_idx, prev_focus_state, focus_reset) = if self.autofocus_on_structure_change
        {
            let idx = self
                .focused_node
                .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
            let state = self
                .focused_node
                .and_then(|id| self.node_map.get(&id).copied())
                .and_then(|w| unsafe { (*w).get_state() });
            (idx, state, self.root_widget.take_focus_reset())
        } else {
            (None, None, false)
        };

        self.taffy = TaffyTree::new();
        self.node_map.clear();
        self.root_node = Some(self.root_widget.build_layout(
            &mut self.taffy,
            &self.theme,
            &mut self.node_map,
        ));
        self.structure_dirty = false;
        self.layout_dirty = true;
        self.update_focusable_order();

        // A focus trap already placed focus inside its subtree in
        // `update_focusable_order`; don't let it escape the modal.
        if self.autofocus_on_structure_change && !self.focus_trap_active {
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
                        && let Some(&w) = self.node_map.get(&id)
                    {
                        unsafe {
                            (*w).set_state(state);
                        }
                    }
                }
            }
        }

        // The widgets built above are fresh instances the OLD `sync` never
        // ran against, so without this they draw blank until the next event.
        self.root_widget.sync(&mut self.state.doc, &self.state.root);
        self.needs_sync = false;
    }

    /// Pair with [`crate::persist::open`], which hands back the document the
    /// previous session left plus its writer:
    ///
    /// ```ignore
    /// let (state, autosave) = ui::persist::open("dev.qos.my-app");
    /// ui::App::new(state, my_app::app_root())
    ///     .with_autosave(autosave)
    ///     .run();
    /// ```
    #[must_use]
    pub fn with_autosave(mut self, autosave: crate::persist::Autosave) -> Self {
        self.autosave = Some(autosave);
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

    /// Precedence: explicit override → `QOS_APP_NAME` → package id → binary
    /// name → generic default.
    pub fn resolved_window_title(&self) -> String {
        resolve_window_title(self.window_title.as_deref())
    }

    /// `records` owns per-record documents and persistence; `room_index` is
    /// which records the room holds. Pair with [`App::with_autosave`] only if
    /// the room index itself should persist — records persist through
    /// `records`.
    #[must_use]
    pub fn with_records(
        mut self,
        records: crate::record::RecordSet,
        room_index: automerge::AutoCommit,
    ) -> Self {
        self.records = Some(records);
        self.room_index = Some(room_index);
        self
    }

    pub fn records(&self) -> Option<&crate::record::RecordSet> {
        self.records.as_ref()
    }

    pub fn room_index(&self) -> Option<&automerge::AutoCommit> {
        self.room_index.as_ref()
    }

    /// The record is the collaboration routing key, so a checkout is a
    /// change of address — without re-announcing it, deltas keep publishing
    /// under the previous record's name.
    pub fn record_tick(&mut self) -> bool {
        let Some(set) = self.records.as_mut() else {
            return false;
        };
        let switched = crate::record::tick(
            set,
            &mut self.state,
            self.room_index.as_mut(),
            &*self.root_widget,
        );
        match switched {
            Some(id) => {
                self.collab_manager = CollaborationManager::for_record(&id);
                if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                    crate::diag!(
                        "[app] checked out record {}; collab connected={}",
                        id.as_str(),
                        self.collab_manager.is_connected()
                    );
                }
                self.needs_sync = true;
                self.layout_dirty = true;
                // `Arrived` asks peers already holding the document to reply
                // with a full snapshot, so a window opening a file someone
                // else is editing hears its existing content too.
                if let Some(scope) = self.presence_broadcast_scope.clone() {
                    self.broadcast_presence(scope, crate::collaboration::PresencePayload::Arrived);
                }
                true
            }
            None => false,
        }
    }

    /// With records in play, the App's autosave writes the room index
    /// instead of the live document — sending the live doc to both would put
    /// a record's content in the room index.
    fn autosave_tick(&mut self) {
        match self.records.as_mut() {
            Some(set) => {
                set.tick(&mut self.state.doc);
                if let (Some(saver), Some(room)) =
                    (self.autosave.as_mut(), self.room_index.as_mut())
                {
                    saver.request(room);
                }
            }
            None => {
                if let Some(saver) = self.autosave.as_mut() {
                    saver.request(&mut self.state.doc);
                }
            }
        }
    }

    /// Called on event-loop teardown so a `kill` right after the last
    /// keystroke still finds it on disk.
    fn autosave_flush(&mut self) {
        if let Some(set) = self.records.as_mut() {
            set.flush(&mut self.state.doc);
            if let (Some(saver), Some(room)) = (self.autosave.as_mut(), self.room_index.as_mut()) {
                saver.request(room);
                saver.flush();
            }
            return;
        }
        if let Some(saver) = self.autosave.as_mut() {
            saver.request(&mut self.state.doc);
            saver.flush();
        }
    }

    /// Lets a caller check [`crate::persist::Autosave::last_error`] rather
    /// than assuming the disk accepted the writes.
    pub fn autosave(&self) -> Option<&crate::persist::Autosave> {
        self.autosave.as_ref()
    }

    pub fn run(self) {
        self.run_with_setup(|_| {});
    }

    /// Like [`App::run`] but exposes the winit [`EventLoopProxy`] before the
    /// loop starts, so callers can spawn threads that inject [`UserEvent`]s.
    ///
    /// # Panics
    ///
    /// Panics if the platform windowing system can't give us an event loop,
    /// or if running it fails — there is no app without one, so there is no
    /// meaningful fallback.
    #[allow(
        clippy::unwrap_used,
        reason = "event-loop creation/run failure is unrecoverable at process startup; the public run()/run_with_setup() signature is called from every app's main() and returning Result here would ripple across the whole app surface for a failure with no fallback"
    )]
    pub fn run_with_setup<F>(mut self, setup: F)
    where
        F: FnOnce(EventLoopProxy<UserEvent>),
    {
        let event_loop = EventLoop::<UserEvent>::with_user_event().build().unwrap();
        let proxy = event_loop.create_proxy();
        let _state_wake = crate::local_state::install_event_loop_proxy(proxy.clone());
        // The AccessKit adapter routes events back through this as
        // `UserEvent::AccessKit`.
        let a11y_proxy = proxy.clone();
        setup(proxy);

        self.init_layout();

        let mut handler = AppHandler {
            app: self,
            window: None,
            render: None,
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
        event_loop.run_app(&mut handler).unwrap();
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
mod generic_floating_activity_tests {
    use super::{
        completed_primary_frame_gates, generic_floating_ids_to_dismiss,
        native_floating_logical_size,
    };
    use crate::floating_surface::FloatingSurfacePresentation;
    use std::collections::{HashMap, HashSet};
    use vello::kurbo::Rect;

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
    let app_name = std::env::var("QOS_APP_NAME").ok();
    let package_id = std::env::var("QOS_PACKAGE_ID").ok();
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
    let package_id = std::env::var("QOS_PACKAGE_ID").ok();
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
    "org.qos.app".to_string()
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
        let human = humanize_ident(&bin);
        if !human.is_empty() {
            return human;
        }
    }
    "qOS App".to_string()
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
/// dedicated worker thread (see [`crate::render_thread`]) — this struct
/// keeps only the channel ends to ship frames and recycle scene buffers.
struct AppHandler {
    app: App,
    /// `None` until the first `resumed` — every modern platform gates window
    /// creation behind an `ActiveEventLoop`.
    window: Option<Arc<Window>>,
    /// `None` again in `exiting`, after the worker is joined, so its surface
    /// drops BEFORE the `Window` arc goes away.
    render: Option<RenderThreadHandle>,
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

/// Split out of `AppHandler::handle_redraw_requested` (`clippy::too_many_lines`).
/// Draws the root widget, its overlay, the action palette, and any visible
/// tooltip into `scene`. Returns the transform and logical window rect the
/// rest of the frame (legacy cursors, remote presence, debug outlines, HUD)
/// draws with, plus the draw-pass duration for the perf log line.
fn draw_frame_content(
    app: &mut App,
    scene: &mut Scene,
    width: u32,
    height: u32,
    scale_factor: f32,
    t_draw_start: Option<std::time::Instant>,
) -> (Affine, KurboRect, u128) {
    app.refresh_control_action_availability();
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
    app.root_widget.native_view_frame_start();
    app.root_widget.draw_with_presence(
        scene,
        transform,
        &mut app.text_renderer,
        &app.theme,
        &app.presence_ctx,
        &app.permission_ctx,
    );
    app.presence_ctx.pop_viewport(prev_vp);
    // Without this the incremental dirty gate is a no-op.
    app.root_widget.mark_drawn();

    // Widgets whose visual extent escapes their layout box (an open
    // `Dropdown`, a popover) composite above every later-drawn sibling here.
    // No-op by default.
    app.root_widget.draw_overlay(
        scene,
        transform,
        &mut app.text_renderer,
        &app.theme,
        &app.presence_ctx,
    );

    if let Some(ap) = app.action_palette.as_mut() {
        let win_w = f64::from(width) / f64::from(scale_factor);
        let card_w = (win_w * 0.6).clamp(360.0, 640.0);
        let x0 = ((win_w - card_w) / 2.0).max(0.0);
        let y0 = (f64::from(height) / f64::from(scale_factor) * 0.12).max(24.0);
        ap.layout(&app.theme, Point::new(x0, y0), card_w);
        ap.draw(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    app.tooltip_host.tick(Instant::now());
    if let Some((text, anchor)) = app
        .tooltip_host
        .visible()
        .map(|(label, anchor)| (crate::i18n::I18nManager::lookup(label, None), anchor))
    {
        let win_w = f64::from(width) / f64::from(scale_factor);
        let win_h = f64::from(height) / f64::from(scale_factor);
        let viewport = KurboRect::new(0.0, 0.0, win_w, win_h);
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

    if let Some(p) = app.profiler.as_mut() {
        p.record_since(crate::profiling::Pass::Draw, __prof_draw);
    }
    let t_draw_us = t_draw_start.map_or(0, |t| t.elapsed().as_micros());
    (transform, window_rect, t_draw_us)
}

/// Split out of `AppHandler::handle_redraw_requested` (`clippy::too_many_lines`).
/// Legacy path — `state.collab.cursors` predates `PresenceCtx`; kept for apps
/// that still write into it directly.
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
            Color::WHITE,
            None,
            &path,
        );

        app.text_renderer.draw_text(
            scene,
            &cursor.name,
            10.0,
            Color::WHITE,
            {
                #[allow(clippy::arithmetic_side_effects, reason = "Affine composition/translation is matrix multiplication on f64 fields; it cannot panic or overflow")]
                let name_transform = cursor_transform * Affine::translate((10.0, 20.0));
                name_transform
            },
            None,
        );
    }
}

/// Split out of `AppHandler::handle_redraw_requested` (`clippy::too_many_lines`).
/// Only active in `App::debug_mode` — draws each widget's layout rect and a
/// depth-cycled label over the frame just rendered.
fn draw_debug_outlines(app: &mut App, scene: &mut Scene, transform: Affine) {
    fn draw_debug_recursive(
        widget: &dyn Widget,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        depth: usize,
        cycle: &[Color; 3],
    ) {
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
        for child in widget.children() {
            draw_debug_recursive(
                child,
                scene,
                transform,
                text_renderer,
                depth.saturating_add(1),
                cycle,
            );
        }
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
fn run_layout_pass(
    app: &mut App,
    root_node: NodeId,
    width: u32,
    height: u32,
    scale_factor: f32,
    structure_rebuilt: bool,
) -> Option<(bool, u128, u128)> {
    if !app.layout_dirty {
        return Some((false, 0, 0));
    }
    let mut t_compute_us = 0u128;
    // A frame dirty only because content scrolled needs `apply_layout` to
    // push the offset down, but not a full `compute_layout` — re-solving the
    // whole tree on every scroll frame was the dominant per-frame cost on
    // long lists/grids.
    let size_changed = app.last_layout_px != Some((width, height));
    let must_compute = structure_rebuilt
        || size_changed
        || scroll_opt_disabled()
        || app.root_widget.layout_needs_measure();
    if must_compute {
        let node_map = &app.node_map;
        let text_renderer = &mut app.text_renderer;
        let t0 = std::time::Instant::now();
        let (logical_w, logical_h) =
            crate::scale::logical_size((width, height), f64::from(scale_factor));
        #[allow(
            clippy::cast_possible_truncation,
            reason = "logical layout size is intentionally narrowed to f32 for taffy; identical to today's output"
        )]
        let compute_result = app.taffy.compute_layout_with_measure(
            root_node,
            taffy::Size {
                width: AvailableSpace::Definite(logical_w as f32),
                height: AvailableSpace::Definite(logical_h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&widget_ptr) = node_map.get(&node_id) {
                    unsafe { (*widget_ptr).measure(known, available, text_renderer) }
                } else {
                    taffy::Size::ZERO
                }
            },
        );
        if let Err(err) = compute_result {
            tracing::error!(?err, "compute_layout_with_measure failed; skipping frame");
            return None;
        }
        t_compute_us = t0.elapsed().as_micros();
        app.last_layout_px = Some((width, height));
        if let Some(p) = app.profiler.as_mut() {
            p.record(
                crate::profiling::Pass::BuildLayout,
                std::time::Duration::from_micros(u64::try_from(t_compute_us).unwrap_or(u64::MAX)),
            );
        }
    }
    let t1 = std::time::Instant::now();
    app.root_widget
        .apply_layout(&app.taffy, root_node, vello::kurbo::Point::new(0.0, 0.0));
    let t_apply_us = t1.elapsed().as_micros();
    if let Some(p) = app.profiler.as_mut() {
        p.record(
            crate::profiling::Pass::ApplyLayout,
            std::time::Duration::from_micros(u64::try_from(t_apply_us).unwrap_or(u64::MAX)),
        );
    }
    app.layout_dirty = false;
    if must_compute {
        app.update_focusable_order();
    }
    Some((true, t_compute_us, t_apply_us))
}

fn create_window_render(
    window: &Arc<Window>,
    transparent: bool,
) -> Result<RenderThreadHandle, String> {
    let size = window.inner_size();
    let mut render_cx = RenderContext::new();
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
    // (`render_thread::configure_surface_checked`) — an already-lost
    // surface would otherwise hit wgpu's panicking default handler.
    let configure_scope = device_handle
        .device
        .push_error_scope(vello::wgpu::ErrorFilter::Validation);
    surface
        .surface
        .configure(&device_handle.device, &surface.config);
    if let Some(err) = pollster::block_on(configure_scope.pop()) {
        return Err(format!("could not configure the drawing surface: {err}"));
    }
    let renderer = Renderer::new(
        &device_handle.device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: vello::AaSupport::all(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .map_err(|e| format!("could not create the renderer: {e}"))?;
    // After this, every GPU call happens on its render worker.
    spawn_render_thread(render_cx, surface, renderer)
}

fn begin_window_render_init(
    window: Arc<Window>,
    transparent: bool,
) -> Result<mpsc::Receiver<Result<RenderThreadHandle, String>>, String> {
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("floating-render-init".into())
        .spawn(move || {
            let result = create_window_render(&window, transparent);
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
    /// Called from the first `RedrawRequested` rather than inline in
    /// `resumed`, so the blocking GPU init doesn't starve the loop's first
    /// events. `Err` is returned, not panicked, since every step here can
    /// fail when the compositor exits mid-open.
    fn init_render(&mut self, window: &Arc<Window>) -> Result<(), String> {
        self.render = Some(create_window_render(window, false)?);
        Ok(())
    }

    fn sync_ime(&mut self, window: &Window) {
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
        let mut attributes = Window::default_attributes()
            .with_title(format!("Floating · {id}"))
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_visible(false)
            .with_inner_size(logical_size);
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes = attributes.with_name(
                format!(
                    "{}{}",
                    crate::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX,
                    token
                ),
                "",
            );
        }
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
        let (app, surfaces) = (&mut self.app, &mut self.floating_windows);
        let Some(surface) = surfaces.get_mut(index) else {
            return false;
        };
        if surface.suspended {
            return true;
        }
        if surface.render.is_none() {
            if surface.render_init.is_none() {
                match begin_window_render_init(surface.window.clone(), true) {
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
        let mut followup = false;
        while let Ok(outcome) = render.frame_complete.try_recv() {
            match outcome {
                FrameOutcome::Presented => {}
                FrameOutcome::Reconfigured | FrameOutcome::Skipped => followup = true,
                FrameOutcome::SurfaceGone => return false,
            }
        }
        let Ok(mut scene) = render.scene_pool.recv() else {
            return false;
        };
        scene.reset();
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
                scene: Box::new(scene),
                width: inner.width.max(1),
                height: inner.height.max(1),
                base_color: Color::TRANSPARENT,
            })
            .is_err()
        {
            return false;
        }
        if followup {
            surface.window.request_redraw();
        }
        true
    }

    fn dispatch_floating_event(
        &mut self,
        window: &Arc<Window>,
        desktop_rect: KurboRect,
        event: WindowEvent,
    ) {
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
        let Some(root_node) = app.root_node else {
            return;
        };
        let mut handled = false;
        let mut focus_target = None;
        app.refresh_control_action_availability();
        app.root_widget.handle_event(
            &event,
            scale,
            &app.modifiers,
            &mut app.state.doc,
            &app.state.root,
            &mut handled,
            &mut focus_target,
            root_node,
            &app.taffy,
        );
        if let Some(id) = focus_target {
            app.set_focused_node(Some(id));
        }
        if handled {
            app.commit_action();
            app.needs_sync = true;
            app.layout_dirty |= app.root_widget.is_layout_dirty();
            if let Some(primary) = self.window.as_ref() {
                primary.request_redraw();
            }
            for surface in &self.floating_windows {
                surface.window.request_redraw();
            }
        }
    }

    /// Split out of `window_event`'s `RedrawRequested` arm (`clippy::too_many_lines`).
    /// Body unchanged from the original inline arm — see `window_event` for the
    /// event-loop plumbing this is called from. The layout pass, draw pass,
    /// legacy cursor drawing, and debug-outline drawing are further split
    /// into [`run_layout_pass`], [`draw_frame_content`], [`draw_legacy_cursors`]
    /// and [`draw_debug_outlines`] respectively; what remains is a single
    /// frame's render-thread handshake (acquire scene, draw, send, log) whose
    /// phases share `render`/`app`/`scene`/timing locals and several early
    /// `return`s — further fragmenting it risks changing frame-timing or
    /// error-path behaviour with no clippy/cargo loop available here to
    /// verify against.
    #[allow(clippy::too_many_lines, reason = "see doc comment above")]
    fn handle_redraw_requested(&mut self, event_loop: &ActiveEventLoop, window: &Arc<Window>) {
        if self.render.is_none() {
            qos_startup_metrics::mark("first-redraw-init-render-start");
            if let Err(reason) = self.init_render(window) {
                // Retrying every redraw would only spin.
                crate::diag!("qos: {reason}");
                event_loop.exit();
                return;
            }
            qos_startup_metrics::mark("first-redraw-init-render-done");
        } else {
            qos_startup_metrics::mark("first-paint");
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
                }
                FrameOutcome::Reconfigured | FrameOutcome::Skipped => {
                    needs_followup_redraw = true;
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
        let app = &mut self.app;

        // `recv()` is the backpressure boundary — capacity-2 means
        // at most one frame of overlap (main building N+1 while
        // worker presents N).
        let Ok(mut scene) = render.scene_pool.recv() else {
            tracing::error!("ui-render thread died; closing window");
            event_loop.exit();
            return;
        };
        scene.reset();

        let inner = window.inner_size();
        let width = inner.width;
        let height = inner.height;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "scale factor is intentionally narrowed to f32 for the render pipeline; identical to today's output"
        )]
        let scale_factor = window.scale_factor() as f32;
        app.last_frame_size = Some(inner);

        // Records first: if the root widget opened a different
        // record, sync against that record's document, not the
        // outgoing one's.
        app.record_tick();

        let __prof_frame_start = app.profiler.as_ref().map(|_| std::time::Instant::now());
        // Drain model notifications and advance update-driven widgets before
        // synchronization. A worker's `Wake` event reaches this pass without
        // requiring the app to own a proxy or poll inside its model.
        if app.root_widget.update(&mut app.state.doc, &app.state.root) {
            app.needs_sync = true;
            app.layout_dirty = true;
        }
        if app.needs_sync {
            app.root_widget.sync(&mut app.state.doc, &app.state.root);
            app.needs_sync = false;
        }
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::Sync, __prof_frame_start);
        }

        app.scan_live_regions();

        // E.g. a tab split inserting a new WindowGroup: rebuild the
        // taffy tree from scratch so new widgets get node ids the
        // layout pass below can see.
        app.structure_dirty |= app.root_widget.take_structure_dirty();
        // Captured BEFORE `rebuild_structure` clears it — a fresh
        // taffy tree always needs a full solve regardless of the
        // scroll fast path below.
        let structure_rebuilt = app.structure_dirty;
        let __prof_build = app.profiler.as_ref().map(|_| std::time::Instant::now());
        if app.structure_dirty {
            app.rebuild_structure();
        }
        if structure_rebuilt {
            app.root_widget
                .mount_native_views(&crate::widget::NativeViewHost::live(window, app.platform));
        }
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::BuildLayout, __prof_build);
        }

        let perf = perf_enabled();
        let t_frame_start = if perf {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let Some(root_node) = app.root_node else {
            tracing::error!("root_node unset during a redraw; skipping frame");
            return;
        };
        let Some((t_relayout_branch, t_compute_us, t_apply_us)) = run_layout_pass(
            app,
            root_node,
            width,
            height,
            scale_factor,
            structure_rebuilt,
        ) else {
            return;
        };

        let t_draw_start = if perf {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let (transform, window_rect, t_draw_us) =
            draw_frame_content(app, &mut scene, width, height, scale_factor, t_draw_start);

        draw_legacy_cursors(app, &mut scene, transform);

        // A pointer that arrived on a desktop scope is the
        // compositor's to draw, and the helper skips it — see
        // `presence::PointerOwner`. Only the *painting* has one
        // owner; the shell threads this same context into its own
        // overlay too.
        crate::presence::draw_remote_presence(
            &mut scene,
            transform,
            window_rect,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );

        // Folds in any `handle_event` time banked since last frame.
        if let (Some(p), Some(start)) = (app.profiler.as_mut(), __prof_frame_start) {
            p.commit_frame(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX));
        }

        if app.debug_mode {
            draw_debug_outlines(app, &mut scene, transform);
        }

        // `update_if_active` only builds + sends when an AT is
        // attached, so this is free on the common no-reader path.
        if let Some(adapter) = self.a11y_adapter.as_mut() {
            let app_ref: &App = app;
            let scale = f64::from(scale_factor);
            adapter.update_if_active(|| app_ref.build_a11y_tree_update(scale));
        }

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
            scene: Box::new(scene),
            width,
            height,
            base_color: app.theme.bg_app,
        };
        if render.render_tx.send(req).is_err() {
            tracing::error!("ui-render thread receiver closed; exiting");
            event_loop.exit();
            return;
        }
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
        for surface in &self.floating_windows {
            surface.window.request_redraw();
        }
    }

    /// Split out of `window_event`'s catch-all arm (`clippy::too_many_lines`). Body
    /// unchanged from the original inline arm: a sequential chain of
    /// mutually-exclusive `if !handled && ...` input-routing checks (Ctrl+K,
    /// screen reader, undo/redo, Tab, arrow nav, gesture nav, generic widget
    /// dispatch, Enter/Space activation, cursor/tooltip follow-up) that all
    /// read and write the same `handled`/`focus_target`/`is_undo_redo`
    /// locals in a fixed precedence order. Splitting each check into its own
    /// method is possible but risks silently reordering that precedence with
    /// no clippy/cargo loop available here to verify against.
    #[allow(clippy::too_many_lines, reason = "see doc comment above")]
    fn handle_other_window_event(&mut self, window: &Arc<Window>, other: WindowEvent) {
        let event = other;
        let app = &mut self.app;
        let detected_interaction = if self.dispatching_touch_pointer {
            None
        } else {
            match &event {
                WindowEvent::Touch(_) => Some(crate::interaction::InteractionEnvironment::TOUCH),
                WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. } => {
                    Some(crate::interaction::InteractionEnvironment::POINTER)
                }
                WindowEvent::KeyboardInput {
                    event:
                        winit::event::KeyEvent {
                            state: ElementState::Pressed,
                            ..
                        },
                    ..
                } => Some(app.interaction.with_keyboard_modality()),
                _ => None,
            }
        };
        if let Some(environment) = detected_interaction {
            app.set_interaction_environment(environment);
        }
        let __prof_input = app.profiler.as_ref().map(|_| std::time::Instant::now());
        // RUST_LOG=ui::app=debug: pointer-move spam is intentionally
        // not throttled here — when the bug is "no clicks work", you
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
        if app.presence_broadcast_scope.is_some() {
            match &event {
                WindowEvent::CursorMoved { position, .. } => {
                    let now = Instant::now();
                    if pointer_broadcast_allowed(
                        app.last_pointer_broadcast,
                        now,
                        Duration::from_millis(POINTER_BROADCAST_MIN_GAP_MS),
                    ) {
                        app.last_pointer_broadcast = Some(now);
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
                    app.last_pointer_broadcast = None;
                }
                _ => {}
            }
        }

        let mut handled = false;
        let mut focus_target = None;
        // A synthesized hover-clear may be handled by a control, but it is
        // presentation state and must not create an empty undo boundary.
        let mut is_undo_redo =
            self.dispatching_touch_pointer && matches!(event, WindowEvent::CursorMoved { .. });
        let mut floating_pointer_needs_redraw = false;

        // The palette owns the keyboard while open; Ctrl+K itself is
        // handled below, so let it through.
        if app.action_palette.is_some() {
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
                    && app.platform.command_modifier_down(app.modifiers.state())
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
            && app.platform.command_modifier_down(app.modifiers.state())
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
            let shift = app.modifiers.state().shift_key();
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
                    let entry = match request {
                        crate::undo::UndoRequest::Undo => app.state.undo_as(
                            Some(&app.local_author),
                            current_focus_idx,
                            current_meta,
                        ),
                        crate::undo::UndoRequest::Redo => app.state.redo_as(
                            Some(&app.local_author),
                            current_focus_idx,
                            current_meta,
                        ),
                    };
                    if let Some(entry) = entry {
                        app.apply_undo_entry(entry);
                    }
                }
                is_undo_redo = true;
                handled = true;
            } else {
                match c.as_str() {
                    // A second Ctrl+K closes the palette.
                    "k" | "K" => {
                        if app.action_palette.is_some() {
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

        if !handled
            && app.keyboard_nav_allowed(modal_rect)
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let shift = app.modifiers.state().shift_key();
            handled = app.focus_tab(shift);
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
                app.modifiers.state().alt_key(),
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
                winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled => {
                    GesturePhase::End
                }
            };
            let nav = match &event {
                WindowEvent::MouseWheel { delta, phase, .. } => {
                    let (dx, dy, precise) = match delta {
                        winit::event::MouseScrollDelta::PixelDelta(p) => (p.x, p.y, true),
                        winit::event::MouseScrollDelta::LineDelta(x, y) => {
                            (f64::from(*x), f64::from(*y), false)
                        }
                    };
                    app.nav_gestures
                        .on_scroll(dx, dy, map_phase(*phase), precise)
                }
                WindowEvent::PanGesture { delta, phase, .. } => app.nav_gestures.on_scroll(
                    f64::from(delta.x),
                    f64::from(delta.y),
                    map_phase(*phase),
                    true,
                ),
                WindowEvent::Touch(t) => {
                    let width = f64::from(window.inner_size().width);
                    app.nav_gestures.on_touch(
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

        if !handled && let Some(root_node) = app.root_node {
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
            app.root_widget.handle_event(
                event_for_root,
                scale_factor,
                &app.modifiers,
                &mut app.state.doc,
                &app.state.root,
                &mut handled,
                &mut focus_target,
                root_node,
                &app.taffy,
            );
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
        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key:
                            winit::keyboard::Key::Named(
                                winit::keyboard::NamedKey::Enter | winit::keyboard::NamedKey::Space,
                            ),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            && let Some(fid) = app.focused_node
            && let Some(&w_ptr) = app.node_map.get(&fid)
        {
            let did = unsafe {
                (*w_ptr).perform_action(
                    &crate::a11y::Action::Activate,
                    &mut app.state.doc,
                    &app.state.root,
                )
            };
            if did {
                handled = true;
                app.needs_sync = true;
                app.layout_dirty = true;
            }
        }

        // A focused range control consumes Left/Right; Up/Down and Tab
        // still move focus. Modifiers scale the step:
        //   plain  → one step       Shift → four steps (coarse)
        //   Ctrl   → jump to min/max (Left = min, Right = max)
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
            let ctrl = app.platform.command_modifier_down(app.modifiers.state());
            let shift = app.modifiers.state().shift_key();
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
            if let (Some(act), Some(fid)) = (action, app.focused_node)
                && let Some(&w_ptr) = app.node_map.get(&fid)
            {
                let mut any = false;
                for _ in 0..times {
                    any |= unsafe {
                        (*w_ptr).perform_action(&act, &mut app.state.doc, &app.state.root)
                    };
                }
                if any {
                    handled = true;
                    app.needs_sync = true;
                    app.layout_dirty = true;
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
            app.layout_dirty = true;
            window.request_redraw();
        }

        app.broadcast_focus_presence();
        app.broadcast_location_presence();
        app.broadcast_view_presence();
        app.publish_action_surface();

        if let Some(id) = focus_target {
            app.set_focused_node(Some(id));
            handled = true;
        }

        app.announce_state_change(state_pre);
        app.announce_text_review(review_pre);

        if handled {
            let mut should_commit = !is_undo_redo;
            if let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Character(_),
                        ..
                    },
                ..
            } = event
            {
                should_commit = false;
            }
            if should_commit {
                app.commit_action();
            }
            app.needs_sync = true;
            // Gating every handled event on `needs_redraw()` is
            // wrong: container widgets can mutate their own
            // active-index state without setting a descendant leaf's
            // `dirty` flag, so recursive `needs_redraw` misses it.
            // Only pointer MOVES (the hot 60+ Hz stream) skip a
            // redraw when nothing's dirty.
            let is_pointer_move = matches!(&event, WindowEvent::CursorMoved { .. });
            if !is_pointer_move || app.root_widget.is_layout_dirty() {
                app.layout_dirty = true;
            }
            if !is_pointer_move || floating_pointer_needs_redraw || app.root_widget.needs_redraw() {
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
            if app.current_cursor != desired {
                app.current_cursor = desired;
                window.set_cursor(desired);
            }
            // `on_pointer` keeps the dwell running over the same
            // region and restarts it on a new one.
            let target = app.tooltip_target_at(logical);
            if app.tooltip_host.on_pointer(target, Instant::now()) {
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
        ) && app.tooltip_host.dismiss()
        {
            window.request_redraw();
        }

        if let (Some(start), Some(p)) = (__prof_input, app.profiler.as_mut()) {
            p.record(crate::profiling::Pass::HandleEvent, start.elapsed());
        }
    }
}

impl ApplicationHandler<UserEvent> for AppHandler {
    // SAFETY: the zeroed-DeviceId transmutes below are safe because winit's
    // DeviceId is opaque and the dispatch chain never inspects it (same
    // trick `shell::compositor::native` uses headless).
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let window_id = window.id();
        match event {
            UserEvent::Hover { x, y } => {
                let ev = WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: PhysicalPosition::new(x, y),
                };
                self.window_event(event_loop, window_id, ev);
            }
            UserEvent::Click { x, y } => {
                let moved = WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: PhysicalPosition::new(x, y),
                };
                self.window_event(event_loop, window_id, moved);
                let press = WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                };
                self.window_event(event_loop, window_id, press);
                let release = WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Released,
                    button: MouseButton::Left,
                };
                self.window_event(event_loop, window_id, release);
            }
            UserEvent::Wheel { dx, dy } => {
                let ev = WindowEvent::MouseWheel {
                    device_id: unsafe { std::mem::zeroed() },
                    delta: winit::event::MouseScrollDelta::LineDelta(dx, dy),
                    phase: winit::event::TouchPhase::Moved,
                };
                self.window_event(event_loop, window_id, ev);
            }
            UserEvent::External(payload) => {
                let app = &mut self.app;
                app.root_widget
                    .on_external_event(&payload, &mut app.state.doc, &app.state.root);
                app.needs_sync = true;
                window.request_redraw();
            }
            UserEvent::Wake => {
                self.app.needs_sync = true;
                window.request_redraw();
            }
            UserEvent::AccessKit(ak_event) => {
                use accesskit_winit::WindowEvent as AkWindowEvent;
                match ak_event {
                    AkWindowEvent::InitialTreeRequested => {
                        if let Some(adapter) = self.a11y_adapter.as_mut() {
                            let scale = window.scale_factor();
                            let app_ref: &App = &self.app;
                            adapter.update_if_active(|| app_ref.build_a11y_tree_update(scale));
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
                                self.window_event(event_loop, window_id, moved);
                                let press = WindowEvent::MouseInput {
                                    device_id: unsafe { std::mem::zeroed() },
                                    state: ElementState::Pressed,
                                    button: MouseButton::Left,
                                };
                                self.window_event(event_loop, window_id, press);
                                let release = WindowEvent::MouseInput {
                                    device_id: unsafe { std::mem::zeroed() },
                                    state: ElementState::Released,
                                    button: MouseButton::Left,
                                };
                                self.window_event(event_loop, window_id, release);
                            }
                            window.request_redraw();
                        }
                    }
                    AkWindowEvent::AccessibilityDeactivated => {}
                }
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
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
        let mut window_attributes = Window::default_attributes()
            .with_title(title)
            .with_window_icon(icon.clone())
            // Draggable title bar on compositors with no server-side
            // decorations (e.g. GNOME/mutter); explicit though it's the
            // winit default, so the intent is unmistakable.
            .with_decorations(true)
            .with_visible(false)
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        // See `resolve_app_id`: a shared id collapses per-app compositor
        // lookups.
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            let package_id = std::env::var("QOS_PACKAGE_ID").ok();
            let app_id = resolve_app_id(
                self.app.window_app_id.as_deref().or(package_id.as_deref()),
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .as_deref(),
            );
            window_attributes = window_attributes.with_name(app_id, "");
        }
        // `resumed` is a winit `ApplicationHandler` callback returning `()`,
        // so a `create_window` failure (compositor gone) has nowhere to
        // propagate to; there is no window to run without it either way.
        #[allow(
            clippy::unwrap_used,
            reason = "fixed () trait signature; window-creation failure at startup is unrecoverable"
        )]
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());
        // Some backends only honour the icon post-creation; a no-op on
        // Wayland compositors that source it from the app_id instead.
        window.set_window_icon(icon);

        // Must be created before the window is shown.
        self.a11y_adapter = Some(accesskit_winit::Adapter::with_event_loop_proxy(
            event_loop,
            &window,
            self.a11y_proxy.clone(),
        ));
        self.app
            .root_widget
            .mount_native_views(&crate::widget::NativeViewHost::live(
                &window,
                self.app.platform,
            ));
        window.set_visible(true);

        self.app.layout_dirty = true;
        // GPU init deferred to first `RedrawRequested` (`init_render`): the
        // ~50ms surface+device init here would block `resumed`'s seat
        // wiring — on mutter that intermittently left the toplevel
        // unfocused with dead input on first launch.
        window.request_redraw();
        crate::dnd::set_drag_window(&window);
        self.window = Some(window);
    }

    /// BEFORE the `Window` arc is dropped — the worker's
    /// `RenderSurface<'static>` references it, and wgpu may panic if the
    /// surface outlives the window.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.app.autosave_flush();
        self.app.root_widget.unmount_native_views();
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

    #[allow(
        clippy::too_many_lines,
        reason = "winit requires the complete per-window event dispatch at this callback boundary"
    )]
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
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
                WindowEvent::ModifiersChanged(modifiers) => self.app.modifiers = modifiers,
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
                .set_interaction_environment(crate::interaction::InteractionEnvironment::TOUCH);
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
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_size) => {
                // Handled lazily on the worker via the next `RenderRequest`'s
                // dimensions; main only needs to drive a redraw.
                self.app.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                // The render path reads `window.scale_factor()` each frame,
                // so we just need to invalidate layout and redraw.
                self.app
                    .root_widget
                    .mount_native_views(&crate::widget::NativeViewHost::live(
                        &window,
                        self.app.platform,
                    ));
                self.app.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => self.handle_redraw_requested(event_loop, &window),
            WindowEvent::ModifiersChanged(modifiers) => {
                self.app.modifiers = modifiers;
            }
            other => self.handle_other_window_event(&window, other),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "winit idle work must remain ordered within its single lifecycle callback"
    )]
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else {
            return;
        };
        self.sync_ime(&window);

        if self.app.pump_floating_surface_service() {
            self.app.layout_dirty = true;
            window.request_redraw();
        }
        if self.apply_floating_host_messages(event_loop) {
            window.request_redraw();
        }
        self.sync_native_floating_geometry();
        if self.recover_stalled_floating_windows() {
            window.request_redraw();
        }
        // `RedrawRequested` polls the asynchronous renderer setup. The shell
        // keeps showing its live transfer proxy in the meantime, so these
        // frames are cheap but essential: the first real buffer should take
        // ownership as soon as it is ready, without waiting for fresh input.
        for surface in &self.floating_windows {
            if !surface.suspended && !surface.adopted {
                surface.window.request_redraw();
            }
        }
        if self.app.root_widget.needs_animation_frame() {
            for surface in &self.floating_windows {
                surface.window.request_redraw();
            }
        }

        let app = &mut self.app;

        // Catches a compositor reconfigure that changes `inner_size()`
        // without delivering `Resized`.
        if let Some(prev) = app.last_frame_size
            && window.inner_size() != prev
        {
            app.layout_dirty = true;
            window.request_redraw();
        }
        if app.pump_collab_presence() {
            window.request_redraw();
        }
        if app.poll_directed_invocations() {
            app.needs_sync = true;
            app.layout_dirty = true;
            window.request_redraw();
        }

        // Keep pumping frames while any "just typed" highlighter mark is
        // still fading, so the taper animates instead of freezing.
        let typing_now = Instant::now();
        app.presence_ctx.prune_typing(typing_now);
        let typing_active = app.presence_ctx.has_active_typing(typing_now);
        if typing_active {
            window.request_redraw();
        }

        if app.dispatch_control_invocations() != 0 {
            app.needs_sync = true;
            app.layout_dirty = true;
            window.request_redraw();
        }

        // The button-driven twin of the Ctrl+K palette dispatch: record one
        // undo boundary so Ctrl+Z reverts it.
        if let Some(before) = app.root_widget.take_committed_action()
            && app.state.doc.get_heads() != before
        {
            app.state.record_committed_action(before, None, None);
            app.needs_sync = true;
            window.request_redraw();
        }

        if app.root_widget.needs_commit() {
            app.commit_action();
            app.root_widget.on_commit();
            app.needs_sync = true;
            window.request_redraw();
        }

        // Level-triggered rather than hooked into individual mutations:
        // widgets write to the document from many places.
        app.autosave_tick();

        if app.reconcile_fullscreen(&window) {
            window.request_redraw();
        }

        // Run both: Settings' open Appearance pane may intentionally override
        // the file-level base with its richer live accessibility intent.
        let configured_changed = app.reconcile_configured_theme();
        let appearance_changed = app.reconcile_appearance();
        if configured_changed || appearance_changed {
            app.needs_sync = true;
            window.request_redraw();
        }

        // The active page/view can change during `sync` (e.g. the initial
        // auto-select), which the post-event broadcast above wouldn't catch.
        app.broadcast_location_presence();
        app.broadcast_view_presence();

        // Run here too, not only `RedrawRequested`, since painting isn't
        // guaranteed — a background tab, or a first frame behind a shader
        // compile.
        if app.record_tick() {
            window.request_redraw();
        }

        // Without this a first peer with no input never re-syncs to seed its
        // doc after the grace window, so a late joiner diverges.
        if app.collab_manager.is_connected() && app.root_widget.needs_idle_sync() {
            app.needs_sync = true;
            window.request_redraw();
        }

        app.broadcast_doc_delta();

        // Flag drained by the draw handler, not here — just pump a redraw.
        if app.root_widget.take_structure_dirty() {
            app.structure_dirty = true;
            window.request_redraw();
        }

        let scheduled_deadline = app.root_widget.next_wake_deadline();
        let scheduled_due = scheduled_deadline.is_some_and(|deadline| deadline <= Instant::now());
        if scheduled_due {
            app.needs_sync = true;
            window.request_redraw();
        }

        // A hover tooltip is not motion, so it reveals even under
        // reduce-motion — requested independently of the animation pump.
        let tooltip_pending = app.tooltip_host.needs_frame();
        if tooltip_pending {
            window.request_redraw();
        }

        // Reduced motion lets the loop go idle since animated widgets
        // already paint their resting state.
        let needs_frame = app.root_widget.needs_animation_frame();
        if needs_frame && !app.theme.reduce_motion {
            app.needs_sync = true;
            // Forces `apply_layout` since an animation tick (e.g. momentum
            // fling) can reposition content without an input event.
            app.layout_dirty = true;
            window.request_redraw();
        }

        // The shell typically writes visible state (remote cursor
        // positions) here, so redraw unconditionally when the tick fires.
        if let Some(tick) = app.frame_tick.as_mut() {
            tick(&mut app.presence_ctx);
            window.request_redraw();
        }

        // `Wait` sleeps until real input, zero CPU; escalate to a bounded
        // ~60 Hz tick only while something no winit event surfaces on its
        // own needs draining — a live collab socket, or a root still
        // wanting another frame.
        let awaiting_floating_pixels = self
            .floating_windows
            .iter()
            .any(|surface| !surface.suspended && !surface.adopted);
        let fast_pump = app.collab_manager.is_connected()
            // A widget may keep reporting animation demand until it receives
            // another paint. Under reduced motion we intentionally suppress
            // that paint above, so counting the raw flag here would leave an
            // otherwise-idle app waking at 60 Hz forever.
            || animation_pump_needed(needs_frame, app.theme.reduce_motion)
            || tooltip_pending
            || typing_active
            || awaiting_floating_pixels
            || scheduled_due;
        match (fast_pump, scheduled_deadline) {
            (true, _) => {
                // `checked_add` is non-panicking at the end of Instant's range.
                let next_tick = Instant::now()
                    .checked_add(Duration::from_millis(16))
                    .unwrap_or_else(Instant::now);
                event_loop.set_control_flow(ControlFlow::WaitUntil(next_tick));
            }
            (false, Some(deadline)) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
            (false, None) => event_loop.set_control_flow(ControlFlow::Wait),
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
            Some("My Doc — Notes"),
            Some("Notes"),
            Some("dev.qos.notes"),
            Some("notes"),
        );
        assert_eq!(t, "My Doc — Notes");
    }

    #[test]
    fn app_name_env_resolves_to_the_manifest_name() {
        let t = resolve_title_from(None, Some("Notes"), Some("dev.qos.notes"), Some("notes"));
        assert_eq!(t, "Notes");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn package_id_falls_back_to_humanized_last_segment() {
        let t = resolve_title_from(
            None,
            None,
            Some("dev.qos.file-explorer"),
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
        let t = resolve_title_from(Some("  "), Some(""), Some("dev.qos.notes"), Some("notes"));
        assert_eq!(t, "Notes");
    }

    #[test]
    fn generic_default_is_qos_app_never_quickui_app() {
        let t = resolve_title_from(None, None, None, None);
        assert_eq!(t, "qOS App");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn the_app_id_is_never_the_generic_one_when_anything_identifies_the_app() {
        assert_eq!(
            resolve_app_id(Some("dev.qos.file-explorer"), Some("file-explorer")),
            "dev.qos.file-explorer",
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
            "org.qos.app",
            "the generic id survives only when there is nothing to say"
        );
        assert_ne!(resolve_app_id(None, Some("file-explorer")), "org.qos.app");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn env_path_end_to_end_prefers_app_name() {
        let prev_name = std::env::var_os("QOS_APP_NAME");
        let prev_id = std::env::var_os("QOS_PACKAGE_ID");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe {
            std::env::set_var("QOS_APP_NAME", "Weather");
            std::env::set_var("QOS_PACKAGE_ID", "dev.qos.weather");
        }
        let resolved = resolve_window_title(None);
        // Restore before asserting so a failure doesn't leak env.
        unsafe {
            match prev_name {
                Some(v) => std::env::set_var("QOS_APP_NAME", v),
                None => std::env::remove_var("QOS_APP_NAME"),
            }
            match prev_id {
                Some(v) => std::env::set_var("QOS_PACKAGE_ID", v),
                None => std::env::remove_var("QOS_PACKAGE_ID"),
            }
        }
        assert_eq!(resolved, "Weather");
        assert_ne!(resolved, "QuickUI App");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn env_path_falls_back_to_package_id_without_app_name() {
        let prev_name = std::env::var_os("QOS_APP_NAME");
        let prev_id = std::env::var_os("QOS_PACKAGE_ID");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe {
            std::env::remove_var("QOS_APP_NAME");
            std::env::set_var("QOS_PACKAGE_ID", "dev.qos.calculator");
        }
        let resolved = resolve_window_title(None);
        unsafe {
            match prev_name {
                Some(v) => std::env::set_var("QOS_APP_NAME", v),
                None => std::env::remove_var("QOS_APP_NAME"),
            }
            match prev_id {
                Some(v) => std::env::set_var("QOS_PACKAGE_ID", v),
                None => std::env::remove_var("QOS_PACKAGE_ID"),
            }
        }
        assert_eq!(resolved, "Calculator");
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // These tests assert exact values assigned without computation.
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
        // Exactly at the gap boundary is allowed — strict `>` would
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
        app.last_pointer_broadcast = Some(Instant::now());
        app.set_presence_broadcast_scope(Some(PresenceKey::Workspace { id: "home".into() }));
        assert!(
            app.last_pointer_broadcast.is_none(),
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

        app.state
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
        unsafe { std::env::set_var("QOS_WORKSPACE_ID", room) };
        let app = App::new(State::new(), Box::new(crate::widget::Text::new("t")));
        assert_eq!(
            app.presence_broadcast_scope(),
            Some(&PresenceKey::Workspace {
                id: room.to_string()
            }),
            "a workspace in the environment is a scope, with no app-side call"
        );
        unsafe { std::env::remove_var("QOS_WORKSPACE_ID") };
    }

    /// The nil cluster addresses nobody, so no room means nothing published.
    #[test]
    #[serial_test::serial]
    fn an_app_with_no_workspace_publishes_nothing() {
        // SAFETY: see above.
        unsafe {
            std::env::remove_var("QOS_WORKSPACE_ID");
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
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([0; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            0,
        );
        app.apply_signed_document_envelope(&envelope);

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
        let w = *app.node_map.get(&id)?;
        unsafe { (*w).get_state() }
    }

    #[test]
    fn caret_preserved_across_structure_rebuild() {
        let mut app = App::new(State::new(), Box::new(RebuildHarness::new()));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();

        let id = app.focused_node.expect("input focused at startup");
        unsafe {
            (*(*app.node_map.get(&id).unwrap())).set_state("3");
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
        let mut app = App::new(State::new(), Box::new(RebuildHarness::new()));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();

        let id = app.focused_node.unwrap();
        unsafe {
            (*(*app.node_map.get(&id).unwrap())).set_state("3");
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
        reason = "the Rc<RefCell<_>> is deliberately asserted Send above — this test double \
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
        let mut app = App::new(State::new(), Box::new(TwoButtons::new()));
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
        let mut app = App::new(State::new(), root);
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
            (*(*app.node_map.get(&id).unwrap())).set_state("2");
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
            (*(*app.node_map.get(&id).unwrap())).set_state("0");
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
        let app = App::new(State::new(), Box::new(ModalHost::new(cell.clone())));
        (app, cell)
    }

    #[test]
    fn closed_modal_does_not_trap_focus() {
        let (mut app, _open) = host_app();
        app.init_layout();
        assert!(!app.focus_trap_active, "no trap while the modal is closed");
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

        assert!(app.focus_trap_active, "an open modal traps focus");
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
        assert!(app.focus_trap_active);
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
        assert!(!app.focus_trap_active);
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
        assert!(app.focus_trap_active);
        assert_ne!(app.focused_node, Some(outer), "focus moved into the modal");

        open.set(false);
        app.refresh_focus_order();
        assert!(
            !app.focus_trap_active,
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
        assert!(node.state.modal, "an open modal traps focus → modal flag");
        assert!(!node.state.hidden);
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Exact preference values are part of the test contract.
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
        assert!(a.layout_dirty);
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
        // Swap the *base* theme — the a11y layer must re-apply on top.
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
        assert!(a.layout_dirty);
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
        #[allow(clippy::too_many_arguments)]
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
        // Nord base + high_contrast=true → high-contrast track wins; ui 1.5, text 1.2.
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
        // Spacing grew by the UI factor; type by UI × text.
        assert!((a.theme.tokens.spacing.s3 - base_gap * 1.5).abs() < 1e-1);
        assert!((a.theme.tokens.typography.scale.base - base_type * 1.5 * 1.2).abs() < 1e-1);
        assert!(a.layout_dirty, "a live appearance change re-runs layout");
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

    impl Widget for FsRoot {
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
        fn fullscreen_intent(&self) -> Option<bool> {
            self.want
        }
        fn on_fullscreen_changed(&mut self, active: bool) {
            self.changes.borrow_mut().push(active);
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
        app.layout_dirty = false;
        assert!(app.note_actual_fullscreen(true));
        assert!(app.needs_sync);
        assert!(app.layout_dirty);
    }
}

#[cfg(test)]
mod action_palette_tests {
    use super::*;
    use crate::widget::{Button, Widget};
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ObjId, ROOT, ReadDoc, ScalarValue};
    use qos_actions::{Action, ActionError, ActionSpec, Doc, Resource};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
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

    /// One open resource exporting a [`Flip`] action, plus a searchable
    /// People provider.
    struct Publisher;
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
        fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
            vec![Box::new(PeopleProvider)]
        }
        fn activate_search_result(&mut self, action_id: &str, doc: &mut AutoCommit, _p: &ObjId) {
            doc.put(&ROOT, "navigated", ScalarValue::Str(action_id.into()))
                .unwrap();
        }
    }

    /// winit keeps a `KeyEvent` field `pub(crate)`, so a struct literal
    /// can't build one.
    fn synth_enter() -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Enter);
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    #[test]
    fn ctrl_k_scopes_to_resources_then_dispatch_mutates_doc_and_records_undo() {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);

        let res = app.collect_resources();
        assert_eq!(res.len(), 1, "one open resource");
        assert_eq!(res[0].kind, "test.obj");

        // Scopes the palette to those resources AND folds in the widget's
        // searchable content — one surface, both kinds.
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
        app.layout_dirty = false;

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
        assert!(app.layout_dirty, "dispatch marks the app for relayout");
        assert!(
            app.action_palette().is_none(),
            "palette closed after selection"
        );
    }

    /// A content row does NOT dispatch — it hands its id back through
    /// [`Widget::activate_search_result`] instead.
    #[test]
    fn ctrl_k_content_row_navigates_through_the_app() {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.open_action_palette();
        app.action_palette.as_mut().unwrap().set_query("ada");
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
        app.layout_dirty = false;

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
            app.needs_sync && app.layout_dirty,
            "navigation marks a redraw"
        );
    }

    #[test]
    fn ctrl_k_invokes_a_projected_semantic_view_control() {
        let button = Button::new("drive-me", "Drive me", None);
        let mut app = App::new(State::new(), Box::new(button));
        app.init_layout();
        app.set_focused_node(None);
        assert!(!app.root_widget.is_focused());
        app.open_action_palette();
        app.action_palette.as_mut().unwrap().set_query("focus");

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

    /// Also resets the undo history — the old stacks name changes this
    /// document has never seen.
    fn restore_state(&mut self, bytes: &[u8]) -> Result<(), crate::view::ThawError> {
        let doc = automerge::AutoCommit::load(bytes)
            .map_err(|e| crate::view::ThawError::State(e.to_string()))?;
        self.state.adopt_doc(doc);
        Ok(())
    }

    fn capture_view(&self) -> crate::view::View {
        use crate::view::{FocusView, View, ViewRect};

        let focus = self.focused_node.and_then(|id| {
            let ordinal = self.focusable_order.iter().position(|&x| x == id)?;
            let ephemeral = self
                .node_map
                .get(&id)
                .and_then(|&w| unsafe { (*w).get_state() })
                .unwrap_or_default();
            Some(FocusView { ordinal, ephemeral })
        });

        let open_resource = self
            .root_widget
            .record_intent()
            .map(|c| c.id.as_str().to_string());

        let viewport = self
            .last_frame_size
            .map(|s| ViewRect {
                x: 0.0,
                y: 0.0,
                width: f64::from(s.width),
                height: f64::from(s.height),
            })
            .unwrap_or_default();

        let contrib = self.root_widget.view_state().unwrap_or_default();

        View {
            scroll: contrib.scroll,
            focus,
            open_resource,
            viewport,
            view_mode: contrib.view_mode,
        }
    }

    /// `open_resource` is carried for presence/reboot to reopen the record;
    /// record-navigation itself is the host's and is not driven here.
    fn restore_view(&mut self, view: &crate::view::View) {
        self.root_widget
            .restore_view_state(&crate::view::ViewContribution {
                scroll: view.scroll.clone(),
                view_mode: view.view_mode.clone(),
            });

        if let Some(f) = &view.focus
            && let Some(&id) = self.focusable_order.get(f.ordinal)
        {
            self.set_focused_node(Some(id));
            if !f.ephemeral.is_empty()
                && let Some(&w) = self.node_map.get(&id)
            {
                unsafe {
                    (*w).set_state(&f.ephemeral);
                }
            }
        }

        self.layout_dirty = true;
        self.needs_sync = true;
    }
}

// View presence: exercised on the real broadcast/receive path —
// `broadcast_view_presence` through the presence sink, and
// `apply_collab_event_for_test` through `apply_remote_presence`.
#[cfg(test)]
#[allow(clippy::float_cmp)] // Serialized presence fields are expected to round-trip exactly.
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

    /// No focus field — [`ViewSubset`] excludes the caret.
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
        #[allow(clippy::too_many_arguments)]
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

    /// Asserted on both the frame that leaves the sender (via the broadcast
    /// sink) and the frame the receiver applies.
    #[test]
    fn view_subset_broadcasts_through_real_presence_path() {
        let mut sender = app_with_root(ViewRoot {
            scroll: vec![10.0, 20.0],
            mode: Some("grid".into()),
            resource: Some("file:/report.md".into()),
            rect: KurboRect::ZERO,
        });
        sender.last_frame_size = Some(winit::dpi::PhysicalSize::new(1024, 768));

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
            Some("file:/report.md"),
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
    use qos_actions::{
        Action as QAction, ActionError, ActionSpec, Doc, EncodedAction, ExecutionLocus,
        PermissionRequirement, Resource,
    };
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    enum CounterAction {
        Bump,
        SetLabel(String),
    }
    const COUNTER_TYPE: &str = "test.counter";

    impl QAction for CounterAction {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            match self {
                CounterAction::Bump => {
                    let n = match doc.get(&ROOT, "n") {
                        Ok(Some((v, _))) => v.to_i64().unwrap_or(0),
                        _ => 0,
                    };
                    doc.put(&ROOT, "n", ScalarValue::Int(n.saturating_add(1)))?;
                }
                CounterAction::SetLabel(s) => {
                    doc.put(&ROOT, "label", s.as_str())?;
                }
            }
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            ExecutionLocus::ClientLocal
        }
    }

    /// One open resource exporting an immediate `bump` and a prompted
    /// `set-label`.
    struct CounterCard {
        button: Button,
        destructive: bool,
    }

    fn counter_resource(destructive: bool) -> Resource {
        let bump = ActionSpec::immediate("bump", "Bump", COUNTER_TYPE, &CounterAction::Bump)
            .unwrap()
            .requiring(PermissionRequirement::Write);
        let bump = if destructive {
            bump.destructive()
        } else {
            bump
        };
        Resource::new("test.counter", "test.counter", "Counter")
            .with_action(bump)
            .with_action(ActionSpec::prompted(
                "set-label",
                "Set label",
                "Label",
                "name…",
                ExecutionLocus::ClientLocal,
                |s: &str| {
                    EncodedAction::encode(COUNTER_TYPE, &CounterAction::SetLabel(s.to_string()))
                        .unwrap()
                },
            ))
    }

    impl CounterCard {
        fn new() -> Self {
            Self::with_destructive(false)
        }

        fn with_destructive(destructive: bool) -> Self {
            let resource = counter_resource(destructive);
            let binding = qos_actions::ActionBinding::unit_for(&resource, "bump").unwrap();
            Self {
                button: Button::new("bump", "Bump", None).on_action(binding),
                destructive,
            }
        }
    }
    impl Widget for CounterCard {
        fn sync(&mut self, s: &mut automerge::AutoCommit, p: &automerge::ObjId) {
            self.button.sync(s, p);
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
        fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<Resource> {
            vec![counter_resource(self.destructive)]
        }
    }

    fn app() -> App {
        let mut app = App::new(State::new(), Box::new(CounterCard::new()));
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        app
    }

    fn counter(app: &App) -> i64 {
        match app.state.doc.get(&ROOT, "n") {
            Ok(Some((v, _))) => v.to_i64().unwrap_or(0),
            _ => 0,
        }
    }

    fn activate_bound_button(app: &mut App) {
        let node = app.focusable_order[0];
        let pointer = app.node_map[&node];
        assert!(unsafe {
            (*pointer).perform_action(
                &crate::a11y::Action::Activate,
                &mut app.state.doc,
                &app.state.root,
            )
        });
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
    fn permission_revocation_and_restore_refresh_bound_control_presentation() {
        let mut app = app();
        let node = app.focusable_order[0];
        let pointer = app.node_map[&node];
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
        let mut app = App::new(State::new(), Box::new(CounterCard::with_destructive(true)));
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
        let binding = qos_actions::ActionBinding::unit_for(&resource, "run").unwrap();
        let button = Button::new("run", "Run", None).on_action(binding);
        let root = crate::widget::resource_actions::background(Box::new(button), resource, |_| {
            let (sender, receiver) = std::sync::mpsc::channel();
            sender
                .send(Ok(qos_actions::ActionOutput::default()))
                .unwrap();
            Ok(receiver)
        });
        let mut app = App::new(State::new(), root);
        app.init_layout();
        activate_bound_button(&mut app);
        assert_eq!(app.dispatch_control_invocations(), 1);
        assert_eq!(app.local_invocations.len(), 1, "Accepted is not success");
        assert!(app.poll_directed_invocations());
        assert!(
            app.local_invocations.is_empty(),
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
            &app.node_map,
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
            .find(|resource| resource.kind == "qos.ui.control" && resource.label == "Drive me");
        assert!(resource.is_none_or(|resource| {
            resource
                .actions
                .iter()
                .all(|action| action.id != "activate")
        }));
        assert_eq!(presses.get(), 0);
    }
}
