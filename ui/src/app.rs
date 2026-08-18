use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{CursorIcon, Fullscreen, Window, WindowId},
};

/// Synthetic input events that can be injected into a running `App` via
/// `EventLoopProxy`. Each variant is translated back into the exact
/// `WindowEvent`(s) winit would deliver from real hardware, then routed
/// through the SAME `window_event` handler — so injected and hardware
/// events take the IDENTICAL code path. This is the foundation for the
/// hosted-mode `QOS_TEST_CONTROL` socket; the socket-listener thread
/// reads JSON commands and pumps the equivalent `UserEvent`s via the
/// proxy.
///
/// v1 covers mouse input — enough to reproduce the immediate
/// hosted-mode click bug. Keyboard injection is a follow-up (winit's
/// `KeyEvent` constructor isn't pub, so we'd need a forwarder helper).
///
/// Defined in `ui` (not `shell`) because the `App` owns the event loop
/// and proxy; `shell` depends on `ui`, not the other way around.
#[derive(Debug, Clone)]
pub enum UserEvent {
    /// Move the cursor only (no click). For hover tests.
    Hover { x: f64, y: f64 },
    /// Full click sequence at the given coords: CursorMoved → Pressed → Released.
    /// One UserEvent dispatches THREE `WindowEvent`s through the same path
    /// a real click would take.
    Click { x: f64, y: f64 },
    /// Synthesise a `MouseWheel { LineDelta(dx, dy) }` at the cursor's
    /// most-recent position. Used by the hosted-mode test_control socket
    /// so an external script can drive strip-pan / overview-scroll /
    /// wheel-disc gestures without a real mouse.
    Wheel { dx: f32, dy: f32 },
    /// A free-form string payload delivered straight to the root widget's
    /// [`crate::widget::Widget::on_external_event`]. Unlike the pointer /
    /// wheel variants this is NOT translated into a `WindowEvent` — it is
    /// the channel a host app uses to pump out-of-band data (e.g. the qOS
    /// installer streaming `qos-install` progress lines from a worker
    /// thread) back into the App for rendering. The App requests a redraw
    /// + re-sync after dispatch so the new state surfaces next frame.
    External(String),
    /// An accessibility event from the `accesskit_winit::Adapter`, delivered
    /// through the same winit event-loop proxy the synthetic-input variants
    /// use. Carries one of:
    /// - `InitialTreeRequested` — an AT just connected; the App must push a
    ///   full [`accesskit::TreeUpdate`] via `update_if_active`.
    /// - `ActionRequested(req)` — the AT asks us to perform an action on a
    ///   node; the App maps it back to our [`crate::a11y::Action`] and
    ///   dispatches via [`crate::widget::Widget::perform_action`] (falling
    ///   back to a synthetic click through the existing input path).
    /// - `AccessibilityDeactivated` — all ATs disconnected.
    ///
    /// Defined as its own variant (rather than translated to a `WindowEvent`)
    /// because accesskit events have no `WindowEvent` analogue.
    AccessKit(accesskit_winit::WindowEvent),
}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(e: accesskit_winit::Event) -> Self {
        UserEvent::AccessKit(e.window_event)
    }
}
use std::sync::Arc;
use std::time::{Duration, Instant};
use vello::{Scene, Renderer, RendererOptions, util::RenderContext, peniko::Color, kurbo::Affine, kurbo::Rect as KurboRect, kurbo::Point};
use taffy::{TaffyTree, AvailableSpace, NodeId};
use crate::state::State;
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::collaboration::{CollaborationManager, CollabEvent, CollabRequest, PresencePayload};
use crate::presence::{PresenceCtx, PresenceKey};
use crate::permissions::PermissionContext;
use crate::render_thread::{spawn_render_thread, FrameOutcome, RenderRequest, RenderThreadHandle};

/// Cached `QUICK_UI_PERF=1` env-var check. Returns `true` once the env
/// var is non-empty; the lookup happens once and the boolean is cached
/// for the rest of the process. The `RedrawRequested` handler reads
/// this once per frame so the typical (perf-off) hot path pays zero.
fn perf_enabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("QUICK_UI_PERF").map(|s| !s.is_empty()).unwrap_or(false)
    })
}

/// Cached `QUICK_UI_NO_SCROLL_OPT=1` escape hatch. When set, the frame loop
/// always runs a full `compute_layout` on any `layout_dirty` frame (the pre-
/// optimisation behaviour), disabling the scroll-reposition fast path. Kept as
/// a safety valve for the shared toolkit: if a subtree ever repositions in a
/// way that genuinely needs a re-solve without reporting `layout_needs_measure`,
/// this restores the always-solve semantics with one env var.
fn scroll_opt_disabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("QUICK_UI_NO_SCROLL_OPT").map(|s| !s.is_empty()).unwrap_or(false)
    })
}

/// The next UTF-8 char boundary at or after `byte` in `s` (clamped to the
/// length). Used by caret-review to recognise a single-character move.
fn next_boundary(s: &str, byte: usize) -> usize {
    let mut b = byte.min(s.len());
    loop {
        if b >= s.len() {
            return s.len();
        }
        b += 1;
        if s.is_char_boundary(b) {
            return b;
        }
    }
}

/// The previous UTF-8 char boundary before `byte` in `s` (clamped to 0).
fn prev_boundary(s: &str, byte: usize) -> usize {
    let mut b = byte.min(s.len());
    loop {
        if b == 0 {
            return 0;
        }
        b -= 1;
        if s.is_char_boundary(b) {
            return b;
        }
    }
}

/// The whitespace-delimited word the caret at `byte` sits in (or just after),
/// for caret-review word announcements. Returns `None` when the caret is in
/// whitespace / at a gap with no adjacent word.
fn word_at(s: &str, byte: usize) -> Option<String> {
    let b = byte.min(s.len());
    // A word spans the run of non-whitespace bytes covering the caret. When the
    // caret is exactly on a boundary (between two words or at the end), prefer
    // the word immediately to the left so Home/End reads a real word.
    let is_ws = |c: char| c.is_whitespace();
    let on_word = s[b..].chars().next().is_some_and(|c| !is_ws(c));
    let anchor = if on_word {
        b
    } else {
        // Caret not on a word char (gap or end) — back up to the word on the
        // left, if any.
        let left = prev_boundary(s, b);
        if s[left..].chars().next().is_some_and(|c| !is_ws(c)) {
            left
        } else {
            return None;
        }
    };
    // Expand left to the word start.
    let mut start = anchor;
    while start > 0 {
        let prev = prev_boundary(s, start);
        if s[prev..].chars().next().is_some_and(is_ws) {
            break;
        }
        start = prev;
    }
    // Expand right to the word end.
    let mut end = anchor;
    while end < s.len() {
        let next = next_boundary(s, end);
        if s[end..].chars().next().is_some_and(is_ws) {
            break;
        }
        end = next;
    }
    let word = &s[start..end];
    if word.is_empty() { None } else { Some(word.to_string()) }
}

/// Minimum gap between two outbound pointer presence broadcasts, in
/// milliseconds. Winit delivers `CursorMoved` at vsync rate or higher; at
/// 120 Hz the raw event stream is ~8 ms per delta, which would saturate
/// the Unix socket for no visual gain. 33 ms caps the wire rate at ~30 Hz
/// — fast enough that remote cursors read as fluid, slow enough that the
/// collab socket never becomes the bottleneck.
pub(crate) const POINTER_BROADCAST_MIN_GAP_MS: u64 = 33;

/// Pure throttle decision: given the timestamp of the last broadcast and
/// the current instant, return `true` iff a new broadcast is allowed. Kept
/// public-in-crate so the throttle logic can be unit-tested without a
/// real event loop.
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

/// Pure decision for the fullscreen reconcile loop: given what the App last
/// *requested* from the window and what the root widget now *wants*
/// ([`crate::widget::Widget::fullscreen_intent`]), return `Some(new_state)`
/// to push to the window via `set_fullscreen`, or `None` to leave it as-is.
///
/// Kept as a free function so the reconcile decision is unit-testable without
/// a live winit window (real OS fullscreen can't be exercised headlessly).
pub(crate) fn fullscreen_request_decision(
    requested: bool,
    want: Option<bool>,
) -> Option<bool> {
    match want {
        Some(w) if w != requested => Some(w),
        _ => None,
    }
}

/// Depth-first search for the first widget in the tree that expresses an
/// [`crate::widget::Widget::appearance_intent`]. Used by the App's live
/// re-theme path so a settings/appearance pane nested anywhere under the root
/// can drive the running app's global appearance without a back-channel. The
/// first `Some` wins (a settings app has at most one appearance surface open).
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
enum Direction { Up, Down, Left, Right }

pub struct App {
    state: State,
    root_widget: Box<dyn Widget>,
    text_renderer: TextRenderer,
    taffy: TaffyTree<()>,
    root_node: Option<NodeId>,
    node_map: std::collections::HashMap<NodeId, *mut dyn Widget>,
    layout_dirty: bool,
    /// Set when any widget in the tree has reported `take_structure_dirty()`
    /// this tick. Triggers a rebuild of the taffy tree before the next
    /// layout pass — without it, widgets spawned at runtime (e.g. a new
    /// `WindowGroup` from a tab-drop split) have no `NodeId` and are
    /// skipped by `apply_layout`.
    structure_dirty: bool,
    needs_sync: bool,
    /// Physical window size the most recent `RedrawRequested` frame was
    /// built at. `about_to_wait` compares the live `inner_size()` against
    /// this to catch an allocation change that arrived without a `Resized`
    /// event (a compositor reconfigure, or a resize that coalesced away)
    /// and reflow responsive layouts on the next tick instead of waiting
    /// on stray input. `None` until the first frame is drawn.
    last_frame_size: Option<winit::dpi::PhysicalSize<u32>>,
    /// Physical `(width, height)` the taffy tree was last `compute_layout`'d
    /// against. Used by the scroll-reposition fast path: a frame that is only
    /// `layout_dirty` because content scrolled needs `apply_layout` but not a
    /// full re-solve — unless the window size changed since the last solve, in
    /// which case available space changed and we must recompute. `None` until
    /// the first solve.
    last_layout_px: Option<(u32, u32)>,
    pub theme: Theme,
    pub debug_mode: bool,
    pub focused_node: Option<NodeId>,
    pub focusable_order: Vec<NodeId>,
    /// Opt-in: when set, the first focusable widget is focused once the
    /// initial layout is built (before the event loop runs) so the app is
    /// keyboard-usable without a click. Default `false` — existing apps
    /// (shell, terminal, file-explorer) keep "nothing focused until the
    /// user acts". The greeter sets this so the username field is typeable
    /// immediately at the login prompt.
    pub autofocus_first: bool,
    /// Opt-in: when set, every structural rebuild of the widget tree
    /// re-establishes keyboard focus. If the root widget raises
    /// [`crate::widget::Widget::take_focus_reset`] (a wizard advancing to a
    /// new step) focus jumps to the first focusable of the rebuilt tree;
    /// otherwise focus is *preserved* by re-selecting the focusable at the
    /// same ordinal position (so a rebuild triggered by an unrelated state
    /// change — e.g. a validation message appearing — doesn't silently
    /// drop the caret). Default `false`: the shell / greeter keep their
    /// existing rebuild-focus behaviour untouched.
    pub autofocus_on_structure_change: bool,
    /// Opt-in: when set, the App polls the widget tree each tick for a
    /// [`crate::widget::Widget::appearance_intent`] and re-themes live when the
    /// user's global-appearance choice (theme / high-contrast / reduced-motion /
    /// UI-scale / text-scale) changes. Off by default so apps that get their
    /// theme pushed from the host (the shell) pay nothing and the hot path is
    /// untouched; the Settings app turns it on so changing UI scale or text size
    /// re-scales its own window immediately.
    pub appearance_reconcile: bool,
    modifiers: winit::event::Modifiers,

    /// Recognises back/forward navigation gestures (two-finger touchpad
    /// swipe, touchscreen edge pull) from scroll/touch events and hands
    /// the result to the widget tree via [`Widget::on_nav_gesture`].
    nav_gestures: crate::gesture::NavGestureRecognizer,

    // Stable state for Undo/Redo tracking
    last_stable_focus_idx: Option<usize>,
    last_stable_metadata: Option<String>,

    // Collaboration
    collab_manager: CollaborationManager,
    /// Who this process acts as. Every committed action is attributed to it,
    /// and Ctrl+Z only reverts actions carrying it — so in a shared session a
    /// user's undo walks past their collaborators' work instead of through it.
    /// Resolved once from the same environment the broker's `Hello` announces,
    /// so the id here is the id peers see.
    local_author: crate::permissions::UserId,
    pub presence_ctx: PresenceCtx,
    pub permission_ctx: PermissionContext,

    /// Scope under which local pointer moves are broadcast via
    /// [`App::broadcast_presence`] while the event loop runs. The shell
    /// sets this at startup (e.g. to the current workspace) so every
    /// connected peer receives `RemotePresenceUpdate` events under the
    /// same key the shell paints remote cursors with. Leaving it `None`
    /// disables outbound pointer broadcast — useful for headless tests
    /// and the `quick-ui` examples, which have no meaningful scope.
    presence_broadcast_scope: Option<PresenceKey>,
    /// The peer we're currently "following", if any — set by clicking a
    /// peer's presence indicator (their off-screen mist, in particular).
    /// [`App::followed_peer_target`] resolves this to their live pointer so
    /// a host can scroll/recentre toward them. `None` = not following.
    followed_peer: Option<String>,
    /// Timestamp of the last outbound pointer broadcast, used by
    /// [`pointer_broadcast_allowed`] to throttle the wire rate to
    /// ~30 Hz regardless of how fast winit delivers `CursorMoved`.
    last_pointer_broadcast: Option<Instant>,
    /// Last caret/selection presence broadcast for the focused widget,
    /// used to avoid re-sending unchanged presence every frame.
    last_focus_presence:
        Option<(PresenceKey, crate::collaboration::PresencePayload)>,
    /// Last location presence (e.g. current tab/page) broadcast for the
    /// root widget, diffed to avoid re-sending unchanged presence.
    last_location_presence:
        Option<(PresenceKey, crate::collaboration::PresencePayload)>,
    /// Last [`ViewSubset`](crate::view::ViewSubset) broadcast to presence —
    /// the region/resource the local participant is looking at. Diffed so
    /// [`App::broadcast_view_presence`] only sends when the view actually
    /// moves, not every frame.
    last_view_presence: Option<crate::view::ViewSubset>,
    /// Optional out-of-band sink invoked alongside the Unix-socket
    /// `broadcast_presence` send. The shell registers a closure here
    /// when peer-sync is enabled so local cursor moves get fanned out
    /// to libp2p peers in addition to (or instead of) the legacy
    /// `QUICK_COLLAB_SOCK` server. The toolkit invokes the sink after
    /// the throttle gate, so callers receive the same ~30 Hz cadence.
    presence_broadcast_sink: Option<Box<dyn Fn(&PresenceKey, &PresencePayload) + Send + Sync>>,
    /// Optional per-frame callback invoked from `AboutToWait`. The
    /// shell registers a drain closure here when peer-sync is enabled
    /// so received `NeighbourMsg::PresenceMoved` events can be pushed
    /// into [`PresenceCtx::remote_cursors`] without App needing to
    /// know about libp2p. The closure receives `&mut PresenceCtx` so
    /// it can write directly to the overlay surface.
    frame_tick: Option<Box<dyn FnMut(&mut PresenceCtx) + Send + Sync>>,

    /// Last OS cursor shape pushed to the window via `Window::set_cursor`.
    /// `CursorMoved` re-resolves the desired shape from the widget under
    /// the pointer every move, but only calls into winit when this
    /// changes — keeping the hot pointer path cheap and not perturbing
    /// the incremental-render gate.
    current_cursor: CursorIcon,

    /// Central hover-tooltip controller. On every pointer move the App
    /// hit-tests the widget tree (the same `node_map` sweep
    /// [`Self::resolve_cursor`] uses) for the smallest widget under the pointer
    /// that exposes a [`Widget::tooltip`], feeds it here, and — once the dwell
    /// elapses — paints a single themed chip on top of the whole app. Clicks,
    /// scrolls, keys and pointer-leave dismiss it. This is what makes tooltips
    /// a toolkit primitive available to every app with zero per-app wiring.
    tooltip_host: crate::widget::TooltipHost,

    /// Built-in screen reader. `Some` when
    /// [`A11ySettings::screen_reader`](crate::a11y::A11ySettings::screen_reader)
    /// is on (toggled via [`Self::set_screen_reader_enabled`], or auto-enabled
    /// at construction when `QOS_A11Y_SCREEN_READER`/`QOS_SCREEN_READER` is set
    /// in the environment — the env var the shell mirrors into sandboxes).
    /// When `None`, every narration call site short-circuits, so a build
    /// with the reader off pays nothing and is byte-for-byte unaffected.
    ///
    /// The reader owns a `tts` speaker (default backend, falling back to a
    /// silent `NullSpeaker` when no TTS is installed — so enabling it never
    /// panics). Focus narration flows from [`Self::set_focused_node`]; a
    /// dedicated keyboard layer in `window_event` drives the reading-cursor
    /// commands.
    screen_reader: Option<crate::screen_reader::ScreenReader>,
    /// Whether the screen-reader modifier key (the **Insert** key, the
    /// NVDA/Orca convention) is currently held. winit does not expose Insert as
    /// a [`Modifiers`](winit::event::Modifiers) bit, so we track its
    /// press/release ourselves to implement the `Insert + <key>` reader
    /// command layer without colliding with normal navigation (Tab, Alt+arrows,
    /// Ctrl shortcuts). Only meaningful when [`Self::screen_reader`] is `Some`.
    screen_reader_modifier_down: bool,
    /// Last spoken text of every [live region](crate::a11y::AccessNode::live)
    /// seen in the tree, keyed by the region's name (its stable identity across
    /// frames). Each frame the reader compares a live node's current text to its
    /// entry here and announces the change — without moving focus. Only
    /// populated when the reader is active; cleared when it's disabled.
    live_region_last: std::collections::HashMap<String, String>,

    /// Whether a modal currently traps keyboard focus (some widget in the tree
    /// returns [`Widget::traps_focus`] true). While set, `focusable_order` is
    /// restricted to the modal's subtree. Tracked across frames so the App can
    /// detect the open→close transition and restore focus.
    focus_trap_active: bool,
    /// The element that held focus the moment a focus trap appeared. Restored
    /// when the trap goes away (the modal closes), so the keyboard lands back
    /// where the user was. `None` when no trap is active or nothing was focused.
    focus_before_trap: Option<NodeId>,

    /// The base (non-high-contrast) colour theme. Kept separate from the live
    /// [`Self::theme`] so toggling high contrast off restores the user's
    /// original colours rather than a hard-coded default. Text scale and the
    /// reduced-motion flag are re-applied on top of this whenever either
    /// preference changes.
    base_theme: Theme,
    /// Low-vision preferences (high contrast + reduced motion) the active
    /// theme is derived from. The screen-reader flag is mirrored separately by
    /// [`Self::screen_reader`]; this struct's `screen_reader` field is kept in
    /// sync so a single value can be handed back to callers.
    a11y: crate::a11y::A11ySettings,
    /// Global text-size multiplier (`1.0` = default). Folded into the active
    /// theme so every widget that reads its font size from the theme scales
    /// together; layout grows to fit so nothing clips.
    text_scale: f32,
    /// Global UI-scale multiplier (`1.0` = default) — the user "make everything
    /// bigger" knob. Folded into the active theme ([`Theme::with_ui_scale`]) so
    /// spacing, radii, and the type scale all grow together. Composes on top of
    /// the per-output device scale winit reports: widgets lay out in logical px
    /// (already multiplied by this factor via the theme) and the render
    /// transform then rasterises at the device's physical resolution, so the
    /// result stays crisp at any fractional device scale.
    ui_scale: f32,

    /// The fullscreen state the App last *requested* from the window (its
    /// intent). Compared against the root's
    /// [`crate::widget::Widget::fullscreen_intent`] each frame so a steady
    /// desired state only issues a single `set_fullscreen` call — see
    /// [`fullscreen_request_decision`].
    fullscreen_requested: bool,
    /// The last *actual* fullscreen state observed from the window and pushed
    /// to the root via [`crate::widget::Widget::on_fullscreen_changed`].
    /// Distinguishes app-driven from compositor-driven transitions (a global
    /// un-fullscreen keybind flips this without the app asking).
    fullscreen_active: bool,

    /// Background writer for the CRDT document, installed by
    /// [`App::with_autosave`]. `None` means this App does not persist — the
    /// pre-existing behaviour, kept for examples, tests and the shell's
    /// hosted chrome, none of which own a package id.
    autosave: Option<crate::persist::Autosave>,
    /// The records this app has open, installed by [`App::with_records`].
    ///
    /// When present, the live document is *one record's* document rather
    /// than the app's whole state, and the root widget names which one via
    /// [`crate::widget::Widget::record_intent`]. `None` — every app that has
    /// not adopted records — behaves exactly as before.
    records: Option<crate::record::RecordSet>,
    /// The room index that goes with `records`: which records this room
    /// holds. Kept beside the live document rather than inside it, because it
    /// is a fact about the room and not about any one record.
    room_index: Option<automerge::AutoCommit>,

    /// The Ctrl+K action palette, `Some` while open. Scoped to the focused app
    /// instance's open resources (collected from the root widget each time it
    /// opens). See [`Self::open_action_palette`].
    action_palette: Option<crate::action_palette::ActionPalette>,
    /// Decoders for every action type the hosted app registered (via
    /// [`Self::action_registry_mut`]). The palette resolves a chosen row through
    /// this — the same path a remote peer uses to replay an action.
    action_registry: qos_actions::ActionRegistry,
    /// Where dispatched actions are replicated / submitted. Defaults to a
    /// local-only transport; the shell/session swaps in a peer/host transport
    /// via [`Self::set_action_transport`].
    action_transport: Box<dyn qos_actions::ActionTransport>,
    /// The retained accessibility tree — stable node identity + a frame-to-frame
    /// diff over the widget semantics and the app's resource affordances.
    /// Refreshed on demand via [`Self::refresh_semantic_tree`]; read via
    /// [`Self::semantic_tree`]; driven via [`Self::invoke_semantic_action`].
    semantic_tree: crate::a11y_tree::SemanticTree,

    /// Per-pass frame profiler + HUD, or `None` when profiling is off (the
    /// default). Enabled by `QOS_PROFILE` / `QOS_PROFILE_OUT` via
    /// [`crate::profiling::FrameProfiler::from_env`]. While `None`, the
    /// render-loop hooks read no clock and allocate nothing — see the
    /// `RedrawRequested` handler.
    profiler: Option<crate::profiling::FrameProfiler>,

    /// An explicit window title set by the app via
    /// [`App::with_window_title`] / [`App::set_window_title`]. When set it
    /// wins over the automatic resolution (env / package id / binary
    /// name). `None` — the common case — leaves the toolkit to derive the
    /// title, so an app names its window only when it wants something
    /// other than its package name.
    window_title: Option<String>,
}

impl App {
    pub fn new(state: State, root_widget: Box<dyn Widget>) -> Self {
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
            // Theme is finalised right after this struct literal — the env-
            // driven low-vision preferences are folded in once all the
            // fields they depend on (base_theme, a11y, text_scale) exist.
            last_frame_size: None,
            last_layout_px: None,
            theme: Theme::dark(),
            debug_mode: cfg!(debug_assertions) && std::env::var("QUICK_UI_DEBUG").is_ok(),
            focused_node: None,
            focusable_order: Vec::new(),
            autofocus_first: false,
            autofocus_on_structure_change: false,
            appearance_reconcile: false,
            modifiers: winit::event::Modifiers::default(),
            last_stable_focus_idx: None,
            last_stable_metadata: None,
            collab_manager: CollaborationManager::new(),
            local_author: crate::collaboration::local_user_id(),
            presence_ctx: PresenceCtx::empty(),
            permission_ctx: PermissionContext::permissive(),
            presence_broadcast_scope: None,
            followed_peer: None,
            last_pointer_broadcast: None,
            last_focus_presence: None,
            last_location_presence: None,
            last_view_presence: None,
            presence_broadcast_sink: None,
            frame_tick: None,
            current_cursor: CursorIcon::Default,
            tooltip_host: crate::widget::TooltipHost::new(),
            // Auto-enable the screen reader if the environment opts in (the
            // shell mirrors A11ySettings.screen_reader into spawned sandboxes
            // via QOS_A11Y_* env vars). Either var, non-empty, turns it on.
            screen_reader: if Self::screen_reader_env_enabled() {
                Some(crate::screen_reader::ScreenReader::new())
            } else {
                None
            },
            screen_reader_modifier_down: false,
            live_region_last: std::collections::HashMap::new(),
            focus_trap_active: false,
            focus_before_trap: None,
            base_theme: Theme::dark(),
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
            semantic_tree: crate::a11y_tree::SemanticTree::new(),
            profiler: crate::profiling::FrameProfiler::from_env(),
            window_title: None,
        };

        // Seed the low-vision preferences from the environment so a sandbox
        // the shell launched with `QOS_A11Y_*` set comes up already themed —
        // matching how the screen reader auto-enables above.
        app.a11y.high_contrast = Self::high_contrast_env_enabled();
        app.a11y.reduce_motion = Self::reduce_motion_env_enabled();
        app.a11y.screen_reader = app.screen_reader.is_some();
        app.text_scale = Self::text_scale_env().unwrap_or(1.0);
        app.ui_scale = Self::ui_scale_env().unwrap_or(1.0);
        app.rebuild_theme();
        app
    }

    /// Re-derive the live [`Self::theme`] from the base colour theme, the
    /// current low-vision settings, and the text-scale factor. Called whenever
    /// any of those inputs change so widgets always draw against a theme that
    /// reflects every active preference.
    fn rebuild_theme(&mut self) {
        self.theme = self
            .base_theme
            .clone()
            .with_a11y(&self.a11y, self.text_scale, self.ui_scale);
    }

    /// Mutable access to the action registry so the hosted app can register its
    /// action types (`app.action_registry_mut().register::<TubeAction>("tube.action")`).
    /// Both the palette (local execution) and remote replay decode through it.
    pub fn action_registry_mut(&mut self) -> &mut qos_actions::ActionRegistry {
        &mut self.action_registry
    }

    /// Swap in a peer/host-aware transport for dispatched actions. The default
    /// is a local-only transport (`ClientLocal` replicate is a no-op sink,
    /// `ServerPreferred` falls back to local execution).
    pub fn set_action_transport(&mut self, transport: Box<dyn qos_actions::ActionTransport>) {
        self.action_transport = transport;
    }

    /// The resource-collection hook: the tree of open resources the focused app
    /// instance (the root widget) currently publishes. Called each time the
    /// action palette opens (and drivable directly in tests).
    pub fn collect_resources(&self) -> Vec<qos_actions::Resource> {
        self.root_widget.resources(&self.state.doc)
    }

    /// Open the Ctrl+K action palette, scoped to the focused instance's open
    /// resources + their exported actions. No-op-safe to call repeatedly.
    pub fn open_action_palette(&mut self) {
        let resources = self.collect_resources();
        let providers = self.root_widget.search_providers();
        let mut palette = crate::action_palette::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.set_providers(providers);
        palette.open();
        self.action_palette = Some(palette);
    }

    /// Close + drop the action palette.
    pub fn close_action_palette(&mut self) {
        self.action_palette = None;
    }

    /// Read access to the action palette (for hosts/tests that inspect it).
    pub fn action_palette(&self) -> Option<&crate::action_palette::ActionPalette> {
        self.action_palette.as_ref()
    }

    /// Route a keyboard event to the open action palette. Returns whether the
    /// event was consumed. On a selection it dispatches the chosen action
    /// against the CRDT doc (honoring its execution locus), records it on the
    /// undo stack, and re-syncs the widget tree; Escape closes the palette.
    fn action_palette_handle_key(&mut self, event: &WindowEvent) -> bool {
        let Some(palette) = self.action_palette.as_mut() else {
            return false;
        };
        let root = self.state.root.clone();
        let before = self.state.doc.get_heads();
        let consumed = palette.handle_key(
            event,
            &self.modifiers,
            &mut self.state.doc,
            &root,
        );
        // An action row may have fired: drain + dispatch it through qos_actions.
        let dispatched = self
            .action_palette
            .as_mut()
            .and_then(|p| p.take_dispatch(&mut self.state.doc, &self.action_registry, self.action_transport.as_mut()));
        if let Some(result) = dispatched {
            match result {
                Ok(d) if d.mutated_local() => {
                    self.state.record_committed_action(before, None, None);
                    self.needs_sync = true;
                    self.layout_dirty = true;
                }
                Ok(_) => {} // submitted to host; no local mutation
                Err(e) => tracing::warn!(error = %e, "action palette dispatch failed"),
            }
        }
        // A content row may have fired: hand its id to the app to navigate. This
        // is presentation, not an action — record an undo point only if the
        // navigation genuinely mutated the shared doc.
        let navigation = self.action_palette.as_mut().and_then(|p| p.take_navigation());
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
        if self
            .action_palette
            .as_ref()
            .map(|p| !p.is_open())
            .unwrap_or(false)
        {
            self.action_palette = None;
        }
        consumed
    }

    /// Whether the environment opts the screen reader on at startup. Reads
    /// `QOS_A11Y_SCREEN_READER` (the sandbox-mirrored var) and the shorter
    /// `QOS_SCREEN_READER` alias; either, non-empty and not `"0"`/`"false"`,
    /// enables it.
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

    /// Whether a `QOS_A11Y_*` env var (and its short alias) is set to a
    /// truthy value: non-empty, not `"0"`, not `"false"` (case-insensitive).
    /// The single gate the boolean low-vision env flags share.
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

    /// Whether the environment opts high contrast on at startup
    /// (`QOS_A11Y_HIGH_CONTRAST` / `QOS_HIGH_CONTRAST`).
    fn high_contrast_env_enabled() -> bool {
        Self::bool_env(&["QOS_A11Y_HIGH_CONTRAST", "QOS_HIGH_CONTRAST"])
    }

    /// Whether the environment opts reduced motion on at startup
    /// (`QOS_A11Y_REDUCE_MOTION` / `QOS_REDUCE_MOTION`).
    fn reduce_motion_env_enabled() -> bool {
        Self::bool_env(&["QOS_A11Y_REDUCE_MOTION", "QOS_REDUCE_MOTION"])
    }

    /// Text-scale factor from the environment (`QOS_A11Y_TEXT_SCALE` /
    /// `QOS_TEXT_SCALE`), or `None` if unset / unparseable. A bad value is
    /// ignored rather than crashing the app at startup.
    fn text_scale_env() -> Option<f32> {
        for key in ["QOS_A11Y_TEXT_SCALE", "QOS_TEXT_SCALE"] {
            if let Ok(v) = std::env::var(key) {
                if let Ok(f) = v.trim().parse::<f32>() {
                    if f.is_finite() && f > 0.0 {
                        return Some(f);
                    }
                }
            }
        }
        None
    }

    /// Global UI-scale factor from the environment (`QOS_UI_SCALE`), or `None`
    /// if unset / unparseable. The shell mirrors the user's UI-scale preference
    /// into spawned sandboxes via this var so an app comes up already scaled,
    /// matching how [`Self::text_scale_env`] handles the text-size preference.
    fn ui_scale_env() -> Option<f32> {
        for key in ["QOS_UI_SCALE", "QOS_A11Y_UI_SCALE"] {
            if let Ok(v) = std::env::var(key) {
                if let Ok(f) = v.trim().parse::<f32>() {
                    if f.is_finite() && f > 0.0 {
                        return Some(f);
                    }
                }
            }
        }
        None
    }

    /// The active low-vision preferences (high contrast + reduced motion +
    /// screen reader). The screen-reader flag tracks
    /// [`Self::screen_reader_enabled`].
    pub fn a11y_settings(&self) -> crate::a11y::A11ySettings {
        let mut s = self.a11y;
        s.screen_reader = self.screen_reader.is_some();
        s
    }

    /// The global text-size multiplier currently in effect (`1.0` = default).
    pub fn text_scale(&self) -> f32 {
        self.text_scale
    }

    /// Set the global text-size multiplier and re-theme. Every widget that
    /// reads its font size from the theme scales together on the next frame,
    /// and layout grows to fit so text never clips. The value is clamped to a
    /// sane range by the theme. Used by the shell when the user changes the
    /// text-size preference, and by examples/tests via `QOS_A11Y_TEXT_SCALE`.
    pub fn set_text_scale(&mut self, factor: f32) {
        self.text_scale = factor;
        self.rebuild_theme();
        self.layout_dirty = true;
        self.structure_dirty = true;
    }

    /// The global UI-scale multiplier currently in effect (`1.0` = default).
    /// This is the user "make everything bigger" knob; it composes with the
    /// per-output device scale the compositor reports.
    pub fn ui_scale(&self) -> f32 {
        self.ui_scale
    }

    /// Set the global UI-scale multiplier and re-theme. Spacing, radii, and the
    /// type scale all grow together on the next frame ([`Theme::with_ui_scale`]),
    /// and layout is marked dirty so the whole interface re-flows live. Used by
    /// the shell when the user changes the UI-scale preference, and by
    /// examples/tests via `QOS_UI_SCALE`.
    pub fn set_ui_scale(&mut self, factor: f32) {
        self.ui_scale = factor;
        self.rebuild_theme();
        self.layout_dirty = true;
        self.structure_dirty = true;
    }

    /// Whether the high-contrast colour track is currently active.
    pub fn high_contrast(&self) -> bool {
        self.a11y.high_contrast
    }

    /// Turn the high-contrast colour track on or off at runtime and re-theme.
    /// Off restores the user's original colours (the base theme captured at
    /// construction / via [`Self::set_theme`]), not a hard-coded default. The
    /// text-scale factor and reduced-motion flag are preserved across the swap.
    pub fn set_high_contrast(&mut self, on: bool) {
        if self.a11y.high_contrast == on {
            return;
        }
        self.a11y.high_contrast = on;
        self.rebuild_theme();
        self.layout_dirty = true;
        self.structure_dirty = true;
    }

    /// Flip the high-contrast colour track and return the new state.
    pub fn toggle_high_contrast(&mut self) -> bool {
        self.set_high_contrast(!self.a11y.high_contrast);
        self.a11y.high_contrast
    }

    /// Whether reduced motion is currently in effect.
    pub fn reduce_motion(&self) -> bool {
        self.a11y.reduce_motion
    }

    /// Turn reduced motion on or off at runtime and re-theme. Animated
    /// widgets consult the theme's reduced-motion flag and snap to their
    /// resting state when it is set; the App also stops pumping the perpetual
    /// animation-frame loop.
    pub fn set_reduce_motion(&mut self, on: bool) {
        if self.a11y.reduce_motion == on {
            return;
        }
        self.a11y.reduce_motion = on;
        self.rebuild_theme();
    }

    /// Replace the base colour theme (the shell sets this when the user picks
    /// a theme). The high-contrast track, text scale, and reduced-motion flag
    /// are re-applied on top, so changing the base theme doesn't discard the
    /// user's low-vision preferences.
    pub fn set_theme(&mut self, theme: Theme) {
        self.base_theme = theme;
        self.rebuild_theme();
        self.layout_dirty = true;
        self.structure_dirty = true;
    }

    /// Enable or disable the built-in screen reader at runtime. The shell calls
    /// this when the user toggles
    /// [`A11ySettings::screen_reader`](crate::a11y::A11ySettings::screen_reader).
    ///
    /// Enabling constructs a [`ScreenReader`](crate::screen_reader::ScreenReader)
    /// over the default `tts` speaker (never panics; falls back to a silent
    /// `NullSpeaker` when no TTS stack is present) and immediately announces the
    /// currently-focused widget so the user gets oriented. Disabling stops any
    /// in-flight speech and drops the reader, returning every narration call
    /// site to a no-op.
    pub fn set_screen_reader_enabled(&mut self, enabled: bool) {
        match (enabled, self.screen_reader.is_some()) {
            (true, false) => {
                self.screen_reader = Some(crate::screen_reader::ScreenReader::new());
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

    /// Whether the built-in screen reader is currently active.
    pub fn screen_reader_enabled(&self) -> bool {
        self.screen_reader.is_some()
    }

    /// Install a caller-provided screen reader (e.g. one over a test
    /// `NullSpeaker`, so a headless test can assert on the spoken phrases).
    /// Passing `None` disables narration. Used by the headless tests and by a
    /// host that wants a custom speech backend.
    #[doc(hidden)]
    pub fn set_screen_reader(&mut self, reader: Option<crate::screen_reader::ScreenReader>) {
        self.screen_reader = reader;
    }

    /// Borrow the active screen reader, if any (tests inspect the recorded
    /// utterances of an injected reader).
    #[doc(hidden)]
    pub fn screen_reader(&self) -> Option<&crate::screen_reader::ScreenReader> {
        self.screen_reader.as_ref()
    }

    /// Announce the currently-focused widget through the screen reader, if both
    /// the reader is active and a widget is focused. No-op otherwise. Called on
    /// focus changes ([`Self::set_focused_node`]) and when the reader is first
    /// enabled.
    fn announce_current_focus(&mut self) {
        let Some(id) = self.focused_node else { return };
        let Some(&w_ptr) = self.node_map.get(&id) else { return };
        // SAFETY: same node_map dereference invariant the rest of `App` relies
        // on — pointers are valid for the duration of an event dispatch (the
        // tree is only rebuilt between frames). Mirrors `navigate_focus`.
        let node = unsafe { (*w_ptr).accessibility() };
        if let (Some(node), Some(sr)) = (node, self.screen_reader.as_mut()) {
            sr.announce_focus(&node);
        }
    }

    /// The built-in screen reader's keyboard command layer.
    ///
    /// Returns `true` if the event was a reader command and was consumed (so
    /// the caller skips normal dispatch). A no-op returning `false` when the
    /// reader is disabled, so non-reader builds are unaffected.
    ///
    /// **Bindings** (the **Insert** key is the reader modifier — the long-
    /// standing NVDA/Orca convention; tracked via [`Self::screen_reader_modifier_down`]
    /// since winit has no Insert modifier bit). These deliberately avoid Tab
    /// (focus order), Alt+arrows / bare arrows (directional focus) and Ctrl+keys
    /// (undo / app shortcuts):
    ///
    /// | Keys | Action |
    /// |---|---|
    /// | `Insert + Down`  | **Read all** — speak the whole reading order ("say all") |
    /// | `Insert + Right` | **Read next** — step the reading cursor forward and speak it |
    /// | `Insert + Left`  | **Read previous** — step the reading cursor back and speak it |
    /// | `Insert + Up`    | **Where am I** — re-announce the currently-focused widget |
    /// | `Insert + H`     | **Next heading** — jump the reading cursor to the next heading |
    /// | `Insert + Shift + H` | **Previous heading** — jump to the previous heading |
    /// | `Insert + F`     | **Next form field** — jump to the next interactive control |
    /// | `Insert + Shift + F` | **Previous form field** — jump to the previous control |
    /// | `Insert + Escape`| **Stop** — silence speech and clear the queue |
    ///
    /// The `Insert` key itself is swallowed (so a stray Insert never types) but
    /// reported as *not* a command (returns `false`) so it doesn't suppress a
    /// genuine following keypress's normal handling.
    fn handle_screen_reader_key(&mut self, event: &WindowEvent) -> bool {
        // Disabled → never intercept anything.
        if self.screen_reader.is_none() {
            return false;
        }
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        use winit::event::ElementState;
        use winit::keyboard::{Key, NamedKey};

        // Track the Insert modifier press/release.
        if let Key::Named(NamedKey::Insert) = ke.logical_key {
            self.screen_reader_modifier_down = ke.state == ElementState::Pressed;
            // Swallow the Insert key so it never produces text, but report it as
            // not-a-command so the next key dispatches normally.
            return false;
        }

        // Only act on presses while the reader modifier is held.
        if ke.state != ElementState::Pressed || !self.screen_reader_modifier_down {
            return false;
        }

        // Quick navigation by element type: jump the reading cursor to the
        // next/previous heading (H) or form field (F). Shift reverses
        // direction. winit delivers these as character keys, so handle them
        // before the named-key arm. The reading direction is the only thing
        // Shift changes here, so we read it off the live modifier state.
        if let Key::Character(ref c) = ke.logical_key {
            let shift = self.modifiers.state().shift_key();
            // Split-borrow: `root_widget` and `screen_reader` are disjoint
            // fields, so pass the root in inline (mirrors the arrow-key arms).
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
                // Split-borrow: root_widget and screen_reader are disjoint
                // fields, so borrow each independently.
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

    /// Reconcile the root widget's desired OS-fullscreen state with the live
    /// window, and notify the root of the *actual* state.
    ///
    /// Two directions, both idempotent:
    ///   1. **App-driven** — if the root's
    ///      [`crate::widget::Widget::fullscreen_intent`] differs from what we
    ///      last requested, push it to the window via `set_fullscreen`. On
    ///      Wayland this sends `xdg_toplevel.set_fullscreen` to the compositor;
    ///      on every other winit backend it goes to the host WM. Either way the
    ///      app becomes a true borderless full-output surface.
    ///   2. **Compositor-driven** — read back the window's actual fullscreen
    ///      state and, on a transition, call the root's
    ///      [`crate::widget::Widget::on_fullscreen_changed`]. This is how a
    ///      global un-fullscreen keybind (or a denied request) reaches the app.
    ///
    /// Returns `true` when anything changed and a redraw/re-sync is warranted.
    /// Called once per event-loop wakeup from `about_to_wait`.
    fn reconcile_fullscreen(&mut self, window: &Window) -> bool {
        let mut changed = false;

        // (1) App-driven: honor the root's desired state.
        let want = self.root_widget.fullscreen_intent();
        if let Some(new_state) = fullscreen_request_decision(self.fullscreen_requested, want) {
            if new_state {
                // The root names *which* output it wants (e.g. the largest
                // monitor for a presentation / immersive video). Enumerate the
                // live monitors, pick the target via the shared selection logic,
                // and fullscreen onto that specific output. When the host
                // exposes no monitors (the current qOS compositor) the list is
                // empty and `select_output` returns `None`, so we fall back to
                // `Borderless(None)` — still a genuine full-output surface, just
                // compositor-placed.
                let params = self.root_widget.fullscreen_params();
                let monitors: Vec<winit::monitor::MonitorHandle> =
                    window.available_monitors().collect();
                let primary = window.primary_monitor();
                let infos: Vec<crate::fullscreen::MonitorInfo> = monitors
                    .iter()
                    .enumerate()
                    .map(|(i, m)| crate::fullscreen::MonitorInfo {
                        index: i as u32,
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

        // (2) Compositor-driven: observe the actual state and notify the root
        // on a transition. `window.fullscreen()` reflects the compositor's
        // most recent configure on Wayland (and the WM state elsewhere), so it
        // catches a fullscreen we never asked for as well as a grant of one we
        // did.
        let actual = window.fullscreen().is_some();
        changed |= self.note_actual_fullscreen(actual);

        changed
    }

    /// Record the actual OS-fullscreen state and, on a transition, notify the
    /// root widget via [`crate::widget::Widget::on_fullscreen_changed`].
    /// Returns `true` iff the state changed. Split out from
    /// [`Self::reconcile_fullscreen`] so the App→root notification wiring is
    /// unit-testable without a live winit window.
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

    /// Reconcile the widget tree's desired global appearance with the live
    /// theme. When [`Self::appearance_reconcile`] is enabled, the App walks the
    /// tree for an [`crate::widget::Widget::appearance_intent`] and applies any
    /// field that differs from what is currently in effect — the base colour
    /// theme, the high-contrast / reduced-motion tracks, the global UI scale,
    /// and the text scale — then re-themes and marks layout dirty so the whole
    /// interface re-scales/re-colours on the next frame. Level-triggered and
    /// idempotent: a no-op on the common frame where the user's choice is
    /// unchanged. Returns `true` iff anything changed.
    ///
    /// Kept split out (and driven off the shared field state) so the reconcile
    /// decision is unit-testable without a live winit window.
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

    /// Hit-test the widget tree at `point` (logical coords) and return the
    /// OS cursor shape the most-specific widget under the pointer wants, or
    /// `None` if nothing claims one.
    ///
    /// Cheap: a single pass over `node_map` with an `O(1)`
    /// [`Widget::cursor_shape`] per widget (the default returns `None`;
    /// only opted-in leaves do real work). The smallest-area matching rect
    /// wins, so a deeply-nested child (a text field, a resize handle) beats
    /// its enclosing container. This deliberately works off `node_map`
    /// rather than walking `children` so it still reaches interactive
    /// widgets nested under containers that keep private child storage
    /// (e.g. the shell's `GroupRow` / `WindowGroup`).
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
            if rect.contains(point) {
                if let Some(icon) = w.cursor_shape(point) {
                    let area = (rect.width() * rect.height()).max(0.0);
                    if best.is_none_or(|(a, _)| area < a) {
                        best = Some((area, icon));
                    }
                }
            }
        }
        best.map(|(_, icon)| icon)
    }

    /// The hover-tooltip `(anchor_rect, label)` for the widget under `point`,
    /// or `None`. Mirrors [`Self::resolve_cursor`]: a single sweep over
    /// `node_map`, picking the smallest-area widget under the pointer that
    /// exposes a non-empty [`Widget::tooltip`] — so a nested icon button beats
    /// its enclosing toolbar. The label is the raw string the widget returns;
    /// the caller resolves i18n at render/announce time.
    fn tooltip_target_at(&self, point: Point) -> Option<(vello::kurbo::Rect, String)> {
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

    /// Register a per-frame callback invoked from `AboutToWait`. The
    /// shell uses this to drain received `PresenceMoved` events from
    /// libp2p and write them into `PresenceCtx::remote_cursors`. The
    /// callback runs once per event-loop wakeup; it must therefore be
    /// cheap and non-blocking.
    pub fn set_frame_tick<F>(&mut self, tick: F)
    where
        F: FnMut(&mut PresenceCtx) + Send + Sync + 'static,
    {
        self.frame_tick = Some(Box::new(tick));
    }

    /// Register a sink invoked alongside [`Self::broadcast_presence`].
    ///
    /// When set, every call to `broadcast_presence` (including the
    /// throttled `CursorMoved` path inside `App::run`) also dispatches
    /// the `(key, payload)` pair to `sink`. The shell uses this hook to
    /// republish presence updates over libp2p — see
    /// `quick_shell::shell::network::ShellNetwork::publish_presence`.
    /// Passing a fresh sink replaces any previous registration; pass
    /// `None`-equivalent (an empty sink) is not supported, use
    /// [`Self::clear_presence_broadcast_sink`] instead.
    pub fn set_presence_broadcast_sink<F>(&mut self, sink: F)
    where
        F: Fn(&PresenceKey, &PresencePayload) + Send + Sync + 'static,
    {
        self.presence_broadcast_sink = Some(Box::new(sink));
    }

    /// Drop any previously-registered presence broadcast sink. Calls to
    /// `broadcast_presence` after this fall back to the Unix-socket-only
    /// path.
    pub fn clear_presence_broadcast_sink(&mut self) {
        self.presence_broadcast_sink = None;
    }

    /// Set (or clear) the presence key under which local pointer moves
    /// are broadcast during the event loop. The shell calls this after
    /// resolving the active workspace so remote peers see the cursor
    /// attributed to the right scope. Passing `None` disables outbound
    /// pointer broadcast.
    pub fn set_presence_broadcast_scope(&mut self, key: Option<PresenceKey>) {
        self.presence_broadcast_scope = key.clone();
        // Reset the throttle so a new scope's first pointer move fires
        // immediately rather than waiting on the stale last-send gate.
        self.last_pointer_broadcast = None;
        // Announce arrival under the new scope so peers already in the
        // room reply with a full document snapshot — this is the
        // app-side half of the late-join convergence handshake. A `None`
        // scope (teardown / headless) announces nothing.
        if let Some(k) = key {
            if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                eprintln!(
                    "[app] set scope; collab connected={}; broadcasting Arrived",
                    self.collab_manager.is_connected()
                );
            }
            self.broadcast_presence(
                k,
                crate::collaboration::PresencePayload::Arrived,
            );
        }
    }

    /// Returns the currently active presence-broadcast scope, if any.
    /// Exposed for tests that assert the scope wiring.
    pub fn presence_broadcast_scope(&self) -> Option<&PresenceKey> {
        self.presence_broadcast_scope.as_ref()
    }

    pub fn presence_ctx(&self) -> &PresenceCtx { &self.presence_ctx }
    pub fn presence_ctx_mut(&mut self) -> &mut PresenceCtx { &mut self.presence_ctx }

    /// Start (or stop, with `None`) following a peer. While set,
    /// [`Self::followed_peer_target`] resolves to their live pointer so a
    /// host can keep them in view.
    pub fn set_followed_peer(&mut self, user_id: Option<String>) {
        self.followed_peer = user_id;
    }

    /// The peer currently being followed, if any.
    pub fn followed_peer(&self) -> Option<&str> {
        self.followed_peer.as_deref()
    }

    /// The followed peer's live pointer position in scene space, if we're
    /// following someone and have a position for them. `None` when not
    /// following, or the peer has no known pointer (e.g. just left).
    pub fn followed_peer_target(&self) -> Option<[f32; 2]> {
        let id = self.followed_peer.as_deref()?;
        self.presence_ctx.peer_pointer(id)
    }

    /// Try to follow whatever peer's presence indicator sits under `point`
    /// (their off-screen mist band) for the given `viewport`. Returns
    /// `true` and latches [`Self::followed_peer`] when a peer is hit; a
    /// miss leaves the current follow target untouched and returns `false`.
    /// This is the seam a host click handler calls to make a peer's mist
    /// clickable — "jump to where Alice went".
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

    pub fn permission_ctx(&self) -> &PermissionContext { &self.permission_ctx }
    pub fn set_permission_ctx(&mut self, ctx: PermissionContext) {
        self.permission_ctx = ctx;
    }

    /// Broadcast the local user's presence to every connected peer via
    /// the collaboration manager. This is the outbound counterpart to
    /// `apply_remote_presence`: callers — the shell, or any widget that
    /// has meaningful local presence (text input cursor, tab focus) —
    /// invoke this when state changes, and peers receive the event
    /// through `CollabEvent::RemotePresenceUpdate`.
    ///
    /// The server (or `MemoryRelay::pump` in tests) attributes the
    /// broadcast to *this* user's id automatically, so callers only
    /// specify what changed, not who.
    pub fn broadcast_presence(
        &mut self,
        key: crate::presence::PresenceKey,
        payload: crate::collaboration::PresencePayload,
    ) {
        // Fan out to the optional out-of-band sink first — this is how
        // the shell's libp2p bridge republishes presence to LAN peers.
        // The sink fires before the Unix-socket send so a slow socket
        // never delays the network path.
        if let Some(sink) = self.presence_broadcast_sink.as_ref() {
            sink(&key, &payload);
        }
        self.collab_manager
            .send(CollabRequest::Presence { key, payload });
    }

    /// Broadcast the document changes made since the last call as an
    /// incremental CRDT delta, so peers see local edits in near-real-time.
    /// This is the live-edit path (the late-join handshake still ships a
    /// full snapshot). Cheap when nothing changed — `save_incremental`
    /// returns empty and we skip the send.
    fn broadcast_doc_delta(&mut self) {
        if !self.collab_manager.is_connected() {
            return;
        }
        let delta = self.state.save_incremental();
        if !delta.is_empty() {
            if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                eprintln!("[app] broadcast doc delta {}B", delta.len());
            }
            self.collab_manager
                .send(crate::collaboration::CollabRequest::SyncIncremental { data: delta });
        }
    }

    /// Poll the focused widget for its caret/selection presence and
    /// broadcast it when it changes since the last send, so peers see the
    /// local caret/selection move in near-real-time. The focused widget
    /// supplies both the key (which sub-element the caret is in) and the
    /// payload via [`Widget::presence_payload`].
    fn broadcast_focus_presence(&mut self) {
        use crate::collaboration::PresencePayload;
        let current = self
            .focused_node
            .and_then(|id| self.node_map.get(&id).copied())
            .and_then(|w| unsafe { (*w).presence_payload() });
        if current != self.last_focus_presence {
            // Typing detection: when the local caret advances forward under
            // the same scope, the user inserted text — publish a `Typing`
            // event for the just-inserted run so peers light up a fading
            // highlighter over what we wrote. Backspace / left-arrow (caret
            // moves back) and scope changes don't qualify. This rides the
            // real caret presence stream, so it needs no per-widget hook.
            if let (
                Some((new_key, PresencePayload::Caret { pos: new_pos })),
                Some((old_key, PresencePayload::Caret { pos: old_pos })),
            ) = (&current, &self.last_focus_presence)
            {
                if new_key == old_key && new_pos > old_pos {
                    self.broadcast_presence(
                        new_key.clone(),
                        PresencePayload::Typing { start: *old_pos, end: *new_pos },
                    );
                }
            }
            if let Some((key, payload)) = current.clone() {
                self.broadcast_presence(key, payload);
            }
            self.last_focus_presence = current;
        }
    }

    /// Poll the ROOT widget for its location presence (e.g. which page /
    /// tab it's viewing) and broadcast the diff, so peers can render
    /// "indicative mist" on the navigational element leading here. Unlike
    /// focus presence, this is polled on the root regardless of focus.
    fn broadcast_location_presence(&mut self) {
        let current = self.root_widget.presence_payload();
        if current != self.last_location_presence {
            if let Some((key, payload)) = current.clone() {
                self.broadcast_presence(key, payload);
            }
            self.last_location_presence = current;
        }
    }

    /// Broadcast the local participant's [`ViewSubset`](crate::view::ViewSubset)
    /// — scroll / open resource / viewport / view mode — so peers know not
    /// just where the local cursor is (that rides the caret/selection
    /// presence stream) but *what region and resource* the participant is
    /// looking at. Diffed against the last send, so it only fires when the
    /// view actually moves.
    ///
    /// The subset has no `PresencePayload` variant of its own, so it rides
    /// the existing presence frame carried inside a
    /// [`PresenceKey::view`](crate::presence::PresenceKey::view) key; the
    /// paired payload is inert — the receiver recognises the key and stores
    /// the decoded subset per peer (see [`App::apply_remote_presence`]). No
    /// new wire variant, no change to the collab transport.
    ///
    /// Skipped entirely when nothing is listening (no live broker and no
    /// out-of-band sink), so the single-user path pays nothing.
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
            // Inert carrier: the receiver dispatches on the `View` key
            // before the payload is ever inspected. `Arrived` is chosen
            // because the SDK presence transport relays it with the routing
            // key intact, which is what actually ferries the subset.
            crate::collaboration::PresencePayload::Arrived,
        );
        self.last_view_presence = Some(subset);
    }

    /// Start following `peer_id`: bind the local viewport to that peer's
    /// broadcast [`ViewSubset`](crate::view::ViewSubset) so the local view
    /// scrolls/pans to track where they are looking. Applies immediately if
    /// the peer has already published a View; following a peer who is not
    /// present (no broadcast View yet) latches the intent but moves nothing,
    /// and the viewport snaps to them the moment their first View arrives.
    ///
    /// A device is just another participant here — there is no special-casing
    /// of the user's own devices. Following is opt-in and reversible via
    /// [`Self::unfollow`].
    pub fn follow(&mut self, peer_id: impl Into<String>) {
        self.followed_peer = Some(peer_id.into());
        self.apply_follow();
    }

    /// Stop following whoever was being followed, releasing the local
    /// viewport. Subsequent View broadcasts from that peer no longer move
    /// the local view. A no-op when not currently following.
    pub fn unfollow(&mut self) {
        self.followed_peer = None;
    }

    /// Bind the local viewport to the followed peer's broadcast View, if
    /// any. Hands the peer's scroll / view-mode to the root widget so the
    /// local content pans to match where they are looking. A no-op when not
    /// following, or when the followed peer has published no View yet — the
    /// "follow someone who isn't here" case, which must not move the view.
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
                view_mode: view.view_mode.clone(),
            });
        self.layout_dirty = true;
        self.needs_sync = true;
    }

    /// Poll the collaboration manager and apply any pending events
    /// (remote cursor moves, remote presence updates). This is the headless
    /// equivalent of the `Event::AboutToWait` branch in [`App::run`] and is
    /// exposed so integration tests can drive the collab pump without
    /// a real winit event loop.
    #[doc(hidden)]
    pub fn poll_and_apply_collab_events(&mut self) {
        let events = self.collab_manager.poll();
        for event in events {
            self.apply_collab_event_for_test(event);
        }
    }

    /// Drain every pending [`CollabEvent`] from the collaboration
    /// manager and route the payload into [`Self::presence_ctx`]:
    /// remote cursor moves stamp the `remote_cursors` overlay vec,
    /// arrivals / departures update the `here` map, caret /
    /// selection / pointer updates land in their respective maps,
    /// and a CRDT incremental merge is applied so peer mutations
    /// become visible at the next sync. Returns `true` iff at least
    /// one event was processed; the caller can use that to decide
    /// whether the window needs a fresh redraw.
    ///
    /// This is the **single shared entry point** the per-frame tick
    /// and the headless test path both go through. Keeping the
    /// dispatch in one place means a future protocol addition
    /// (e.g. a new payload variant) doesn't have to be added to
    /// two near-identical match arms.
    pub fn pump_collab_presence(&mut self) -> bool {
        let events = self.collab_manager.poll();
        let mut any = false;
        for event in events {
            any = true;
            match event {
                CollabEvent::Context { collaborators, .. } => {
                    for c in collaborators {
                        self.state.collab.cursors.entry(c.id.clone()).or_insert(
                            crate::state::RemoteCursor {
                                x: 0.0,
                                y: 0.0,
                                color: crate::color_compat::rgb(
                                    c.color[0] as f64,
                                    c.color[1] as f64,
                                    c.color[2] as f64,
                                ),
                                name: c.name.clone(),
                            },
                        );
                        // Seed the overlay-side presence so peer
                        // labels come through with the server-
                        // announced friendly name; later cursor
                        // moves preserve the label because they
                        // pass display_name=None.
                        self.presence_ctx
                            .upsert_remote_cursor(c.id.clone(), [0.0, 0.0], Some(&c.name));
                        // Seed the roster so caret chips and mist tags for
                        // this peer show the friendly name, not the raw id.
                        self.presence_ctx.set_display_name(
                            crate::permissions::UserId(c.id.clone()),
                            c.name.clone(),
                        );
                    }
                }
                CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                    if let Some(cursor) = self.state.collab.cursors.get_mut(&user_id) {
                        cursor.x = x;
                        cursor.y = y;
                    }
                    self.presence_ctx
                        .upsert_remote_cursor(user_id, [x, y], None);
                }
                CollabEvent::RemotePresenceUpdate { user_id, key, payload } => {
                    self.apply_remote_presence(user_id, key, payload);
                }
                CollabEvent::CRDTUpdate { data } => {
                    // MERGE incoming full state rather than replacing the
                    // doc — a replace clobbers our own concurrent edits.
                    if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                        eprintln!("[app] recv CRDTUpdate {}B", data.len());
                    }
                    if let Ok(mut incoming) = State::load(&data) {
                        let _ = self.state.doc.merge(&mut incoming.doc);
                        self.needs_sync = true;
                        // A remote change can grow the widget tree (new
                        // page/block), so force a relayout before the next
                        // draw — else a stale layout indexes out of bounds.
                        self.layout_dirty = true;
                    }
                }
                CollabEvent::CRDTIncremental { data } => {
                    let _ = self.state.load_incremental(&data);
                    self.needs_sync = true;
                    self.layout_dirty = true;
                }
                CollabEvent::SignedCRDTIncremental { envelope, .. } => {
                    // The broker has already validated the envelope (signature
                    // check + role table lookup + replay defence) before
                    // emitting this event to us. The toolkit trusts that and
                    // applies the embedded payload to the local Automerge doc
                    // via the same incremental-merge path the legacy unsigned
                    // variant uses. Phase 7 (MLS encryption) layers payload
                    // decryption between extraction and apply; the envelope
                    // metadata stays cleartext.
                    if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                        eprintln!(
                            "[app] recv SignedCRDTIncremental from {} ({}B payload)",
                            "<peer>",
                            envelope.payload.len()
                        );
                    }
                    let _ = self.state.load_incremental(&envelope.payload);
                    self.needs_sync = true;
                    self.layout_dirty = true;
                }
                CollabEvent::ScrollbackBatch { artifact: _, ops, exhausted: _ } => {
                    // Phase 6: apply older ops in seq order. The broker
                    // pre-validated each envelope before sticking it in
                    // its in-memory log; we trust the contents the same
                    // way `SignedCRDTIncremental` does. Apps that need
                    // a "Loading older edits…" spinner read the
                    // `exhausted` flag separately; the toolkit's
                    // default surface is just to apply.
                    if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                        eprintln!("[app] recv ScrollbackBatch ({} ops)", ops.len());
                    }
                    for envelope in ops {
                        let _ = self.state.load_incremental(&envelope.payload);
                    }
                    self.needs_sync = true;
                    self.layout_dirty = true;
                }
            }
        }
        any
    }

    /// Test-only hook: apply a single `CollabEvent` directly, bypassing the
    /// collab manager's socket/poll path. Integration tests that construct
    /// a `MemoryRelay` can use this to inject remote presence / cursor
    /// updates into the app's state without wiring a full transport.
    #[doc(hidden)]
    pub fn apply_collab_event_for_test(&mut self, event: CollabEvent) {
        match event {
            CollabEvent::RemotePresenceUpdate { user_id, key, payload } => {
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
                // Also drive the PresenceCtx overlay path so integration
                // tests can assert on remote cursors via the public presence
                // surface instead of the legacy cursors HashMap.
                self.presence_ctx.upsert_remote_cursor(user_id, [x, y], None);
            }
            CollabEvent::CRDTIncremental { data } => {
                // Mirror the production path so tests can exercise the
                // incremental-merge wiring without a real event loop.
                let _ = self.state.load_incremental(&data);
                self.needs_sync = true;
            }
            CollabEvent::CRDTUpdate { data } => {
                // MERGE (not replace) — see the production handler in
                // `run`'s about_to_wait loop for the rationale.
                if let Ok(mut incoming) = crate::state::State::load(&data) {
                    let _ = self.state.doc.merge(&mut incoming.doc);
                    self.needs_sync = true;
                }
            }
            CollabEvent::SignedCRDTIncremental { envelope, .. } => {
                // Validation has happened on the broker side; the
                // toolkit trusts the envelope at this point and feeds
                // the payload into the same incremental-merge path the
                // legacy variant uses.
                let _ = self.state.load_incremental(&envelope.payload);
                self.needs_sync = true;
            }
            _ => {}
        }
    }

    /// Test-only accessor: a mutable reference to the app's CRDT state so
    /// integration tests can stage mutations (e.g. type into a text input)
    /// and then serialize the resulting deltas via `save_incremental` to
    /// feed the peer relay.
    #[doc(hidden)]
    pub fn state_mut_for_test(&mut self) -> &mut crate::state::State {
        &mut self.state
    }

    fn update_focusable_order(&mut self) {
        self.focusable_order.clear();
        fn collect_focusable(node_id: NodeId, widget: &dyn Widget, taffy: &TaffyTree<()>, order: &mut Vec<NodeId>) {
            // Disabled widgets are skipped by keyboard focus traversal.
            if widget.is_focusable() && widget.is_enabled() { order.push(node_id); }
            let children = widget.children();
            if let Ok(child_nodes) = taffy.children(node_id) {
                for (child_widget, &child_node) in children.iter().zip(child_nodes.iter()) {
                    collect_focusable(child_node, *child_widget, taffy, order);
                }
            }
        }
        // Locate the focusables of an open modal's subtree, if one is up. The
        // deepest trapping widget wins (a modal opened from within a modal).
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

        let Some(root) = self.root_node else { return };

        let trap = find_trap_focusables(root, self.root_widget.as_ref(), &self.taffy);
        let trap_active = trap.is_some();

        if let Some(trapped) = trap {
            // Confine keyboard focus to the modal's focusables.
            self.focusable_order = trapped;
        } else {
            collect_focusable(root, self.root_widget.as_ref(), &self.taffy, &mut self.focusable_order);
        }

        // Manage the trap open/close transition for focus restore.
        match (self.focus_trap_active, trap_active) {
            (false, true) => {
                // A modal just opened: remember where focus was, then move it
                // into the modal so Tab cycles inside it and the dialog (or its
                // first field) is announced.
                self.focus_before_trap = self.focused_node;
                if let Some(&first) = self.focusable_order.first() {
                    self.set_focused_node(Some(first));
                } else {
                    // Modal has no focusable content — drop focus so Tab can't
                    // reach the content behind the backdrop.
                    self.set_focused_node(None);
                }
            }
            (true, false) => {
                // The modal closed: return focus to where the user was, as long
                // as that element still exists in the rebuilt tree.
                let restore = self
                    .focus_before_trap
                    .take()
                    .filter(|id| self.focusable_order.contains(id));
                self.set_focused_node(restore);
            }
            (true, true) => {
                // Still trapped: if focus drifted outside the modal (a rebuild
                // dropped the focused element), pull it back to the first member.
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

    fn apply_remote_presence(
        &mut self,
        user_id: String,
        key: crate::presence::PresenceKey,
        payload: crate::collaboration::PresencePayload,
    ) {
        use crate::collaboration::PresencePayload::*;
        use crate::permissions::UserId;

        // A View broadcast rides inside the routing key, not the payload
        // (the subset has no `PresencePayload` variant). Recognise it here,
        // store the peer's region/resource, and — if we're following them —
        // move the local viewport to match. The paired payload is inert and
        // deliberately never reaches the match below.
        if let Some(subset) = key.as_view() {
            self.presence_ctx
                .set_peer_view(UserId(user_id), subset);
            self.apply_follow();
            return;
        }

        match payload {
            Pointer { x, y } => {
                // Populate BOTH presence stores: `pointers` (keyed by
                // scope, read by the shell's per-workspace overlay and by
                // `pointer_for`) and the screen-space `remote_cursors` vec
                // the shared `draw_remote_cursors` helper renders. Only the
                // latter is painted by the App, so there's no double-draw.
                let uid = UserId(user_id.clone());
                self.presence_ctx.set_pointer(&key, uid, (x, y));
                self.presence_ctx
                    .upsert_remote_cursor(user_id, [x, y], None);
            }
            Caret { pos } => {
                // Feed BOTH presence stores: the in-app BlockEditor reads
                // `carets` (set_caret); the cross-process overlay reads
                // the `remote_caret` map (set_remote_caret).
                let uid = UserId(user_id.clone());
                self.presence_ctx.set_caret(&key, uid, pos);
                self.presence_ctx.set_remote_caret(user_id, key, pos);
            }
            Selection { start, end } => {
                let uid = UserId(user_id.clone());
                self.presence_ctx.set_selection(
                    &key,
                    uid,
                    crate::presence::SelectionRange { start, end },
                );
                self.presence_ctx
                    .set_remote_selection(user_id, key, start, end);
            }
            Typing { start, end } => {
                // A peer inserted `start..end` under `key`: record a fading
                // textmarker swipe in their colour. `Instant::now()` births
                // the mark; the render fades it over `TYPING_FADE`, and the
                // per-frame `prune_typing` reclaims it.
                self.presence_ctx.record_peer_typing(
                    crate::permissions::UserId(user_id),
                    key,
                    crate::presence::SelectionRange { start, end },
                    Instant::now(),
                );
            }
            Arrived => {
                let uid = UserId(user_id.clone());
                // Register in #280's cross-process peer registry too.
                self.presence_ctx
                    .register_peer_at(user_id.clone(), key.clone());
                // Tab keys move the peer's dot between tabs/pages; non-Tab
                // (app/workspace) arrivals are (possibly late) joiners, so
                // reply with our full state to converge. The broker fans
                // this to all peers; an already-synced peer merges it
                // idempotently. (Page switches use Tab keys, never this.)
                if matches!(key, crate::presence::PresenceKey::Tab { .. }) {
                    self.presence_ctx.set_tab_focus(&key, uid);
                } else {
                    self.presence_ctx.register_presence(&key, uid);
                    let data = self.state.save();
                    if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                        eprintln!(
                            "[app] recv Arrived; replying full state {}B",
                            data.len()
                        );
                    }
                    self.collab_manager
                        .send(crate::collaboration::CollabRequest::SyncCRDT { data });
                }
            }
            Left => {
                // `Left` is broadcast on `WindowEvent::CursorLeft` —
                // the peer's mouse pointer left their window viewport,
                // not the peer themselves. Their caret + selection
                // should KEEP rendering because they're still editing,
                // they've just moved their mouse elsewhere. Wiping
                // them via `unregister_peer` here was the
                // "remote caret disappears a few seconds after it was
                // placed" bug — any mouse-leave on the publisher side
                // tore down the entire presence record on every
                // subscriber.
                //
                // True peer disconnect arrives via the socket close
                // path (broker drops the peer from its fan-out
                // table); subscribers only need to clear pointer-
                // related state here.
                let uid = UserId(user_id.clone());
                for list in self.presence_ctx.pointers.values_mut() {
                    list.retain(|(u, _)| u != &uid);
                }
                self.presence_ctx.remove_remote_cursor(&user_id);
                let _ = key;
            }
        }
    }

    fn apply_undo_entry(&mut self, entry: crate::state::UndoEntry) {
        if let Some(idx) = entry.focused_node_idx {
            if idx < self.focusable_order.len() {
                let id = self.focusable_order[idx];
                self.set_focused_node(Some(id));
                if let Some(metadata) = entry.widget_metadata {
                    if let Some(&w_ptr) = self.node_map.get(&id) {
                        unsafe { (*w_ptr).set_state(&metadata); }
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
        // Stamp the action with who performed it. Without this every entry on
        // the undo stack is anonymous, and `undo_for_author` — the thing that
        // stops one collaborator reverting another's work — has nothing to
        // match on.
        //
        // `commit_action_authored` reports whether the transaction actually
        // changed the document. The App calls `commit_action` after *every*
        // handled event, and many handled events mutate only ephemeral widget
        // state, not the CRDT — a text-selection drag in the terminal is the
        // motivating case: it fires a `CursorMoved` per frame, each marked
        // handled, with nothing committed. Serialising the whole document
        // (`state.save()`) and broadcasting it on each of those was a per-frame
        // cost proportional to the document size, which is what made selection
        // feel laggy over a large scrollback. When nothing changed there is
        // nothing to persist or sync, so skip both.
        let changed = self.state.commit_action_authored(
            self.last_stable_focus_idx,
            self.last_stable_metadata.clone(),
            Some(self.local_author.clone()),
        );
        self.last_stable_focus_idx = self.focused_node.and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.last_stable_metadata = self.focused_node.and_then(|id| {
            self.node_map.get(&id).and_then(|&w_ptr| unsafe { (*w_ptr).get_state() })
        });

        // Broadcast CRDT state — only when the commit produced a real change.
        if changed {
            let data = self.state.save();
            self.collab_manager.send(CollabRequest::SyncCRDT { data });
        }
    }

    fn set_focused_node(&mut self, new_id: Option<NodeId>) {
        if self.focused_node != new_id {
            if let Some(old_id) = self.focused_node {
                if let Some(&w_ptr) = self.node_map.get(&old_id) {
                    unsafe { (*w_ptr).set_focus(false); }
                }
            }
            self.focused_node = new_id;
            if let Some(id) = new_id {
                if let Some(&w_ptr) = self.node_map.get(&id) {
                    unsafe { (*w_ptr).set_focus(true); }
                    // Screen-reader focus narration. Pull the newly-focused
                    // widget's semantics and speak it, INTERRUPTING the previous
                    // utterance so rapid arrow/Tab navigation stays responsive
                    // (the reader's queue coalesces a focus storm to the latest).
                    // No-op when the reader is disabled — the hot focus path
                    // pays only a single `Option::is_some`-style branch.
                    if self.screen_reader.is_some() {
                        let node = unsafe { (*w_ptr).accessibility() };
                        if let (Some(node), Some(sr)) =
                            (node, self.screen_reader.as_mut())
                        {
                            sr.announce_focus(&node);
                        }
                    }
                }
            }
            self.layout_dirty = true;
            self.needs_sync = true;
        }
    }

    /// Snapshot the focused widget's concise value+state string. Captured
    /// *before* an event is dispatched so [`announce_state_change`] can detect
    /// whether the same control's state changed (mouse click, Space/Enter, a
    /// slider step) and speak the new state. Cheap no-op when the reader is off.
    fn focused_state_snapshot(&self) -> Option<(NodeId, String)> {
        self.screen_reader.as_ref()?;
        let id = self.focused_node?;
        let &w_ptr = self.node_map.get(&id)?;
        let node = unsafe { (*w_ptr).accessibility() }?;
        Some((id, crate::screen_reader::describe_state(&node)))
    }

    /// If the focused control's state changed since `pre` (and focus did NOT
    /// move — a focus move is already narrated by [`set_focused_node`]), speak
    /// just the new state ("checked", "on", "75 percent"). Covers both mouse
    /// and keyboard activation, so the reader always confirms the result.
    fn announce_state_change(&mut self, pre: Option<(NodeId, String)>) {
        let Some((pre_id, pre_state)) = pre else { return };
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

    /// Snapshot the focused widget's caret-review state `(caret_byte, text)`
    /// *before* an event is dispatched, so [`announce_text_review`] can tell a
    /// caret move (arrow / Home / End over existing content) from an edit. Cheap
    /// no-op when the reader is off or the focused widget isn't an editable text
    /// field.
    fn text_review_snapshot(&self) -> Option<(NodeId, usize, String)> {
        self.screen_reader.as_ref()?;
        let id = self.focused_node?;
        let &w_ptr = self.node_map.get(&id)?;
        let (caret, text) = unsafe { (*w_ptr).text_review() }?;
        Some((id, caret, text))
    }

    /// If the focused text field's caret moved over **unchanged** text since
    /// `pre`, announce what the caret landed on: the single character for a
    /// one-step move (arrow keys), or the word for a longer jump (Home/End,
    /// word-wise move). Typing (which changes the text) is left to the widget's
    /// own echo, so this never double-speaks an edit. No-op when the reader is
    /// off, focus moved, or the field isn't editable text.
    fn announce_text_review(&mut self, pre: Option<(NodeId, usize, String)>) {
        let Some((pre_id, pre_caret, pre_text)) = pre else { return };
        if self.focused_node != Some(pre_id) {
            return;
        }
        if self.screen_reader.is_none() {
            return;
        }
        let Some(&w_ptr) = self.node_map.get(&pre_id) else { return };
        let Some((now_caret, now_text)) = (unsafe { (*w_ptr).text_review() }) else {
            return;
        };
        // Only a pure caret move over identical text is a "review"; an edit
        // changed the text and is echoed by the widget itself.
        if now_text != pre_text || now_caret == pre_caret {
            return;
        }
        // One char-boundary step → speak the character; a longer jump → speak
        // the word the caret now sits in.
        let single_step = next_boundary(&now_text, pre_caret) == now_caret
            || prev_boundary(&now_text, pre_caret) == now_caret;
        if single_step {
            // The character the caret is now on (to its right). At the very end
            // there's nothing to the right, so fall back to the char to the left.
            let ch = now_text[now_caret..]
                .chars()
                .next()
                .or_else(|| now_text[..now_caret].chars().next_back());
            if let (Some(ch), Some(sr)) = (ch, self.screen_reader.as_mut()) {
                sr.echo_char(ch);
            }
        } else if let Some(word) = word_at(&now_text, now_caret) {
            if let Some(sr) = self.screen_reader.as_mut() {
                sr.echo_word(&word);
            }
        }
    }

    /// Scan the tree for [live regions](crate::a11y::AccessNode::live) whose
    /// spoken text changed since the last frame and announce each change —
    /// **without moving focus**. A toast appearing, a status line updating, a
    /// progress step all narrate themselves this way. Politeness picks the
    /// priority: assertive interrupts; polite waits for a pause. Run once per
    /// frame after `sync`, so the text reflects the freshly-pulled CRDT state.
    /// A cheap no-op when the reader is off.
    fn scan_live_regions(&mut self) {
        if self.screen_reader.is_none() {
            return;
        }
        // Collect (identity, politeness, current text) for every live region,
        // then diff against the stored snapshot. The identity is the region's
        // accessible name — a live region without a name can't be tracked
        // stably across frames, so it's skipped.
        let mut seen: Vec<(String, crate::a11y::Politeness, String)> = Vec::new();
        fn visit(
            w: &dyn Widget,
            out: &mut Vec<(String, crate::a11y::Politeness, String)>,
        ) {
            if let Some(node) = w.accessibility() {
                if let (Some(politeness), Some(name)) = (node.live, node.name.clone()) {
                    out.push((name, politeness, crate::screen_reader::describe(&node)));
                }
            }
            for child in w.children() {
                visit(child, out);
            }
        }
        visit(self.root_widget.as_ref(), &mut seen);

        let mut current_keys = std::collections::HashSet::new();
        let mut to_announce: Vec<(crate::a11y::Politeness, String)> = Vec::new();
        for (key, politeness, text) in seen {
            current_keys.insert(key.clone());
            let changed = self.live_region_last.get(&key) != Some(&text);
            // Record the new text first so we never re-announce the same value.
            self.live_region_last.insert(key, text.clone());
            // Announce only meaningful (non-empty) text. Clearing a region to
            // empty updates the snapshot silently.
            if changed && !text.is_empty() {
                to_announce.push((politeness, text));
            }
        }
        // Drop snapshots for regions that left the tree, so a region that
        // reappears with the same text announces afresh.
        self.live_region_last.retain(|k, _| current_keys.contains(k));

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
        let current_id = match self.focused_node {
            Some(id) => id,
            None => { if !self.focusable_order.is_empty() { self.set_focused_node(Some(self.focusable_order[0])); } return; }
        };
        let current_rect = if let Some(&w_ptr) = self.node_map.get(&current_id) { unsafe { (*w_ptr).get_rect() } } else { return; };
        let current_center = current_rect.center();
        let mut best_candidate = None;
        let mut best_score = f64::MAX;
        for &candidate_id in &self.focusable_order {
            if candidate_id == current_id { continue; }
            if let Some(&candidate_ptr) = self.node_map.get(&candidate_id) {
                let candidate_rect = unsafe { (*candidate_ptr).get_rect() };
                let candidate_center = candidate_rect.center();
                let vector = vello::kurbo::Vec2::new(candidate_center.x - current_center.x, candidate_center.y - current_center.y);
                let (primary_dist, secondary_dist, in_direction) = match direction {
                    Direction::Up => (-vector.y, vector.x.abs(), vector.y < -1.0),
                    Direction::Down => (vector.y, vector.x.abs(), vector.y > 1.0),
                    Direction::Left => (-vector.x, vector.y.abs(), vector.x < -1.0),
                    Direction::Right => (vector.x, vector.y.abs(), vector.x > 1.0),
                };
                if in_direction {
                    let score = primary_dist + (secondary_dist * 2.0);
                    if score < best_score { best_score = score; best_candidate = Some(candidate_id); }
                }
            }
        }
        if let Some(new_id) = best_candidate { self.set_focused_node(Some(new_id)); }
    }

    /// Paint the **shared** keyboard-focus ring around the focused widget, on
    /// top of the composed widget tree. This is the toolkit-wide safety net that
    /// makes keyboard focus visible for *every* focusable widget: a control that
    /// does not paint its own indicator (see
    /// [`Widget::draws_own_focus_ring`](crate::widget::Widget::draws_own_focus_ring))
    /// still gets an unmistakable themed ring, so an app makes a control
    /// keyboard-navigable with nothing more than `is_focusable() -> true` and a
    /// correct `get_rect()`.
    ///
    /// Draws exactly one stroke (`theme.border_focus`) when there is a focused
    /// node whose widget declines to draw its own ring; a no-op otherwise (a
    /// widget that renders its own ring, or no focus at all). Called from the
    /// production draw pass right after the widget tree is encoded, and driven
    /// directly by the focus-ring tests — so both exercise the same path.
    pub(crate) fn compose_focus_ring(&self, scene: &mut Scene, transform: Affine) {
        let Some(fid) = self.focused_node else { return };
        let Some(&w_ptr) = self.node_map.get(&fid) else { return };
        // SAFETY: `node_map` pointers are valid for the lifetime of the current
        // tree; this read mirrors every other `node_map` deref in the App.
        let (draws_own, rect) = unsafe { ((*w_ptr).draws_own_focus_ring(), (*w_ptr).get_rect()) };
        if draws_own {
            return;
        }
        // Skip a degenerate (never-laid-out) rect so an off-screen or zero-size
        // focusable doesn't stroke a dot at the origin.
        if rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }
        // A 2px ring, rounded to the theme's small radius, outset by 1px so it
        // hugs the control's outer edge without clipping its content — the same
        // weight the shared widgets use for their own rings.
        let radius = self.theme.tokens.radii.sm as f64;
        let ring = vello::kurbo::RoundedRect::from_rect(rect.inflate(1.0, 1.0), radius);
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            self.theme.border_focus,
            None,
            &ring,
        );
    }

    /// Synthesise a full pointer click (CursorMoved -> Pressed -> Released) at
    /// `center` (window-**logical** coordinates), routed through the widget
    /// tree's [`Widget::handle_event`](crate::widget::Widget::handle_event) —
    /// the identical path a real or injected mouse click takes. Used as the
    /// keyboard-activation fallback when a focused control declines
    /// [`Action::Activate`](crate::a11y::Action::Activate): its existing mouse
    /// handler (hover + press/release) drives the activation, so a custom
    /// clickable needs no `perform_action` to be operable from the keyboard.
    ///
    /// `scale` converts the logical centre to the physical position
    /// `handle_event` expects (it divides back by the scale factor). Returns
    /// whether any of the synthesised events was consumed.
    fn synth_activation_click(&mut self, center: Point, scale: f64) -> bool {
        let Some(root_node) = self.root_node else { return false };
        let pos = winit::dpi::PhysicalPosition::new(center.x * scale, center.y * scale);
        let seq = [
            WindowEvent::CursorMoved { device_id: unsafe { std::mem::zeroed() }, position: pos },
            WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
            WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
            },
        ];
        let mut any = false;
        for ev in seq {
            let mut handled = false;
            let mut focus_target = None;
            self.root_widget.handle_event(
                &ev,
                scale,
                &self.modifiers,
                &mut self.state.doc,
                &self.state.root,
                &mut handled,
                &mut focus_target,
                root_node,
                &self.taffy,
            );
            any |= handled;
        }
        if any {
            self.needs_sync = true;
            self.layout_dirty = true;
        }
        any
    }

    /// Whether the App's own keyboard focus navigation (Tab cycling and
    /// directional arrows) may run this frame.
    ///
    /// It runs when nothing is capturing the pointer as a modal, **or** when a
    /// focus trap is active. In the trapped case [`focusable_order`] is already
    /// scoped to the trap's subtree (see [`Self::update_focusable_order`]), so
    /// navigation stays *inside* the modal and cannot walk out to the content
    /// behind the scrim. Gating on the focus-trap signal — not `modal_capture`
    /// — is deliberate: `modal_capture` governs only pointer click-through, so
    /// gating Tab on it froze focus inside a dialog (the OK/Cancel buttons
    /// couldn't be cycled) while a trap that set `traps_focus` without
    /// `modal_capture` let Tab escape entirely.
    ///
    /// [`focusable_order`]: Self::focusable_order
    fn keyboard_nav_allowed(&self, modal_rect: Option<vello::kurbo::Rect>) -> bool {
        modal_rect.is_none() || self.focus_trap_active
    }

    /// Advance keyboard focus for a Tab (`shift == false`) / Shift+Tab
    /// (`shift == true`) press: step through [`focusable_order`] wrapping
    /// last↔first. When a modal traps focus that order is scoped to the modal,
    /// so the cycle stays within it. Returns `true` when focus moved (there was
    /// at least one focusable), so the caller marks the event handled.
    ///
    /// This is the production Tab path — the window-event loop calls it and the
    /// focus-trap tests drive it directly, rather than re-implementing the wrap.
    ///
    /// [`focusable_order`]: Self::focusable_order
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
                    if idx == 0 { len - 1 } else { idx - 1 }
                } else {
                    (idx + 1) % len
                }
            }
            None => 0,
        };
        self.set_focused_node(Some(self.focusable_order[next_idx]));
        true
    }

    /// Build a fresh [`accesskit::TreeUpdate`] from the current widget tree,
    /// node map, and focus. Pure delegation to
    /// [`crate::accesskit_bridge::build_tree_update`]; kept on `App` so the
    /// adapter's `update_if_active` closure can build the tree from the App's
    /// live state. `scale` is the window scale factor so bounds land in
    /// physical pixels (what AT-SPI expects).
    pub fn build_a11y_tree_update(&self, scale: f64) -> accesskit::TreeUpdate {
        crate::accesskit_bridge::build_tree_update(
            self.root_widget.as_ref(),
            &self.node_map,
            self.focused_node,
            scale,
        )
    }

    /// Dispatch an accessibility action (already mapped to our
    /// [`crate::a11y::Action`]) at the AccessKit node `target`.
    ///
    /// The `target` is an [`accesskit::NodeId`] derived from a `taffy::NodeId`
    /// (`taffy_id + 1`, with `0` reserved for the synthetic window root — see
    /// [`crate::accesskit_bridge`]). We invert that to find the live widget in
    /// `node_map` and:
    ///   1. give the widget first refusal via
    ///      [`Widget::perform_action`](crate::widget::Widget::perform_action);
    ///   2. if it declines, handle [`Action::Focus`] by moving keyboard focus,
    ///      and any other verb by synthesising a click through the existing
    ///      input path (return the click coordinates so the caller can inject a
    ///      [`UserEvent::Click`]).
    ///
    /// Returns `Some((x, y))` window-logical coordinates if the caller should
    /// synthesise a click there (the generic activation fallback), else `None`
    /// (the action was fully handled in-place).
    pub fn dispatch_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> Option<(f64, f64)> {
        use crate::a11y::Action;
        let raw = target.0;
        // Reverse the bridge's id scheme: window root (0) has no widget.
        if raw == 0 {
            return None;
        }
        let taffy_id = NodeId::from(raw - 1);
        let Some(&w_ptr) = self.node_map.get(&taffy_id) else {
            return None;
        };

        // 1. Widget gets first refusal.
        let handled = unsafe {
            (*w_ptr).perform_action(action, &mut self.state.doc, &self.state.root)
        };
        if handled {
            self.needs_sync = true;
            self.layout_dirty = true;
            return None;
        }

        // 2. Generic fallbacks.
        match action {
            Action::Focus => {
                self.set_focused_node(Some(taffy_id));
                None
            }
            Action::Activate | Action::ToggleCheck => {
                // Synthesise a click at the widget's centre through the same
                // path real pointer input takes.
                let rect = unsafe { (*w_ptr).get_rect() };
                let c = rect.center();
                Some((c.x, c.y))
            }
            _ => None,
        }
    }

    /// Rebuild the [retained semantic tree](crate::a11y_tree::SemanticTree) from
    /// the current widget tree, node map, focus, and the focused app's open
    /// resources, diffing it against the previous frame.
    ///
    /// Returns the [`TreeDelta`](crate::a11y_tree::TreeDelta): what nodes were
    /// added / removed / had their content change since the last refresh. Node
    /// identity is preserved for everything that recurred, so a consumer (an
    /// AT-SPI incremental push, a screen reader, an AI-agent introspecting the
    /// UI) can react to just the diff rather than re-reading a whole new tree.
    pub fn refresh_semantic_tree(&mut self) -> crate::a11y_tree::TreeDelta {
        let resources = self.collect_resources();
        self.semantic_tree.refresh(
            self.root_widget.as_ref(),
            &self.node_map,
            self.focused_node,
            &resources,
        )
    }

    /// Read access to the retained semantic tree (last
    /// [`refresh`](Self::refresh_semantic_tree)ed state).
    pub fn semantic_tree(&self) -> &crate::a11y_tree::SemanticTree {
        &self.semantic_tree
    }

    /// Invoke a named affordance on a semantic node — the operate-the-tree
    /// counterpart to reading it. This is how an assistive technology, a voice
    /// command, or an AI agent drives the UI *by name*.
    ///
    /// * A [`Verb`](crate::a11y_tree::SemanticActionKind::Verb) (focus, activate,
    ///   increment, …) routes through the same
    ///   [`dispatch_a11y_action`](Self::dispatch_a11y_action) path an AccessKit
    ///   `ActionRequest` takes — [`Widget::perform_action`](crate::widget::Widget::perform_action)
    ///   first, then the generic focus/click fallback.
    /// * An [`App`](crate::a11y_tree::SemanticActionKind::App) or
    ///   [`AppPrompted`](crate::a11y_tree::SemanticActionKind::AppPrompted)
    ///   affordance dispatches the resource's **exact
    ///   [`EncodedAction`](qos_actions::EncodedAction)** through
    ///   [`qos_actions::dispatch`] — the *identical* audited path the Ctrl+K
    ///   palette and the on-screen control use — and records one undo boundary.
    ///   `arg` supplies the free-text argument a prompted action needs (ignored
    ///   otherwise).
    ///
    /// Returns `Ok(true)` if a matching affordance was found and driven,
    /// `Ok(false)` if the node or action name was unknown, and `Err` if a
    /// resolved app action failed to decode/dispatch. Call
    /// [`refresh_semantic_tree`](Self::refresh_semantic_tree) first so the ids
    /// are current.
    pub fn invoke_semantic_action(
        &mut self,
        id: &crate::a11y_tree::SemanticId,
        name: &str,
        arg: Option<&str>,
    ) -> Result<bool, String> {
        use crate::a11y_tree::SemanticActionKind;
        // Resolve the affordance out of the (immutably-borrowed) tree first, so
        // the mutation below is free to borrow the doc / registry / transport.
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
                let Some(taffy_raw) = self.semantic_tree.node(id).and_then(|n| n.widget_taffy_id)
                else {
                    return Ok(false);
                };
                let ak = accesskit::NodeId(taffy_raw + 1);
                // Give the widget first refusal (its `perform_action`) and let the
                // generic focus fallback run. A `Some((x, y))` result means the
                // widget self-activates only via a pointer click, which needs the
                // windowed input path (the winit handler synthesises it for an
                // AccessKit `ActionRequest`); such widgets should implement
                // `perform_action`. We surface the verb as dispatched either way.
                let _pending_click = self.dispatch_a11y_action(ak, &verb);
                Ok(true)
            }
        }
    }

    /// Dispatch a resolved [`EncodedAction`](qos_actions::EncodedAction) through
    /// the qOS action model — decode via the app-registered registry, dispatch
    /// via [`qos_actions::dispatch`] honouring its locus, and (when it mutated
    /// the local doc) record one undo boundary. Byte-for-byte the same path as a
    /// Ctrl+K palette selection ([`Self::action_palette_handle_key`]).
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

    /// Build the initial taffy tree, seed the focus order, and (when
    /// [`Self::autofocus_first`] is set) hand the keyboard to the first
    /// focusable widget. Factored out of [`Self::run_with_setup`] so the
    /// headless tests can stand the App up without spinning a winit loop.
    #[doc(hidden)]
    pub fn init_layout(&mut self) {
        self.node_map.clear();
        self.root_node = Some(self.root_widget.build_layout(
            &mut self.taffy,
            &self.theme,
            &mut self.node_map,
        ));
        self.update_focusable_order();
        // Opt-in startup focus: hand the keyboard to the first focusable
        // widget so the app is typeable without a click (used by the
        // greeter so the username field is ready at the login prompt).
        if self.autofocus_first && self.focused_node.is_none() {
            if let Some(&first) = self.focusable_order.first() {
                self.set_focused_node(Some(first));
            }
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

    /// Compute and apply layout for the current tree at a logical
    /// `(width, height)`, then refresh the focus order — the same
    /// `compute_layout_with_measure` -> `apply_layout` -> `update_focusable_order`
    /// pass the per-frame draw loop runs, minus the GPU. Exposed (doc-hidden) so
    /// headless tests can give widgets real [`Widget::get_rect`] bounds (needed
    /// for directional focus and the shared focus ring) without a winit loop.
    #[doc(hidden)]
    pub fn layout_for_test(&mut self, width: f64, height: f64) {
        let root_node = self.root_node.expect("init_layout must run before layout_for_test");
        let node_map = &self.node_map;
        let text_renderer = &mut self.text_renderer;
        self.taffy
            .compute_layout_with_measure(
                root_node,
                taffy::Size {
                    width: AvailableSpace::Definite(width as f32),
                    height: AvailableSpace::Definite(height as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&widget_ptr) = node_map.get(&node_id) {
                        unsafe { (*widget_ptr).measure(known, available, text_renderer) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
        self.root_widget.apply_layout(&self.taffy, root_node, Point::new(0.0, 0.0));
        self.update_focusable_order();
    }

    /// Compose one frame into a fresh [`Scene`] exactly as the redraw loop does
    /// — draw the widget tree, then paint the shared focus ring
    /// ([`Self::compose_focus_ring`]) — minus the GPU present. Exposed
    /// (doc-hidden) so a `--shots` capture or a test can rasterise a real frame,
    /// focus ring included, through an [`OffscreenRenderer`](crate::offscreen::OffscreenRenderer).
    #[doc(hidden)]
    pub fn render_to_scene_for_test(&mut self) -> Scene {
        let mut scene = Scene::new();
        self.root_widget.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut self.text_renderer,
            &self.theme,
            &self.presence_ctx,
            &self.permission_ctx,
        );
        self.root_widget.mark_drawn();
        self.compose_focus_ring(&mut scene, Affine::IDENTITY);
        scene
    }

    /// Render one offscreen frame of `root` to a PNG at `out` under `theme`,
    /// with a control focused and the **shared focus ring** painted around it —
    /// the byte-for-byte ring the live compositor draws, via the real `App`
    /// frame path (layout -> draw tree -> [`Self::compose_focus_ring`]).
    ///
    /// `focus_name` picks which focusable to land on: `Some(name)` focuses the
    /// first focusable whose [`Widget::name`](crate::widget::Widget::name)
    /// matches (e.g. a custom app row), `None` focuses the first focusable in
    /// tree order. Used by the system apps' `--shots` focus captures to prove
    /// keyboard focus is visible in both light and dark themes.
    pub fn render_focus_shot(
        root: Box<dyn Widget>,
        viewport: (u32, u32),
        theme: Theme,
        focus_name: Option<&str>,
        out: &std::path::Path,
    ) {
        let (w, h) = viewport;
        let mut app = App::new(State::new(), root);
        app.set_theme(theme);
        app.init_layout();
        app.layout_for_test(w as f64, h as f64);

        let target = match focus_name {
            Some(name) => app.focusable_order.iter().copied().find(|id| {
                app.node_map
                    .get(id)
                    .map(|&p| unsafe { (*p).name() == name })
                    .unwrap_or(false)
            }),
            None => app.focusable_order.first().copied(),
        };
        app.set_focused_node(target);

        let scene = app.render_to_scene_for_test();
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut renderer = crate::offscreen::OffscreenRenderer::new(w, h);
        renderer.render_png(&scene, vello::peniko::Color::TRANSPARENT, out);
    }

    /// Re-run the per-frame focus pass: pull widget state via `sync` (so a
    /// modal's open flag updates) then re-evaluate the focusable order and any
    /// focus trap. This is what the event loop does after handling input that
    /// changed widget state without rebuilding the tree; exposed (doc-hidden)
    /// so headless tests can drive modal open/close without a winit loop.
    #[doc(hidden)]
    pub fn refresh_focus_order(&mut self) {
        self.root_widget.sync(&mut self.state.doc, &self.state.root);
        self.update_focusable_order();
    }

    /// Move keyboard focus to the first focusable widget whose
    /// [`Widget::test_id`](crate::widget::Widget::test_id) matches `id`,
    /// returning whether one was found. The real `set_focused_node` path is
    /// private; this is the click-free way a headless test lands focus on a
    /// named control (a settings sidebar row, say) before exercising a rebuild.
    #[doc(hidden)]
    pub fn focus_test_id(&mut self, id: &str) -> bool {
        let target = self.focusable_order.iter().copied().find(|n| {
            self.node_map
                .get(n)
                .map(|&p| unsafe { (*p).test_id() } == Some(id))
                .unwrap_or(false)
        });
        if let Some(t) = target {
            self.set_focused_node(Some(t));
            true
        } else {
            false
        }
    }

    /// The [`Widget::test_id`](crate::widget::Widget::test_id) of the
    /// currently keyboard-focused widget, if any — so a headless test can name
    /// which control the focus ring lands on after a rebuild.
    #[doc(hidden)]
    pub fn focused_test_id(&self) -> Option<String> {
        let id = self.focused_node?;
        let &p = self.node_map.get(&id)?;
        unsafe { (*p).test_id() }.map(|s| s.to_string())
    }

    /// The ephemeral [`Widget::get_state`](crate::widget::Widget::get_state) of
    /// the currently keyboard-focused widget — for a sidebar row this reports
    /// `"active"`/`"inactive"`, so a test can assert focus landed on the
    /// *selected* row (focus == selection).
    #[doc(hidden)]
    pub fn focused_widget_state(&self) -> Option<String> {
        let id = self.focused_node?;
        let &p = self.node_map.get(&id)?;
        unsafe { (*p).get_state() }
    }

    /// Run one production frame at logical `(width, height)`: `sync`, adopt any
    /// structural rebuild the sync requested, then lay out and refresh the focus
    /// order — the exact order (and thus the exact focus-restore behaviour) the
    /// winit event loop runs. Lets a headless test drive the real
    /// selection-rebuild path a click triggers without spinning a window.
    #[doc(hidden)]
    pub fn run_frame_for_test(&mut self, width: f64, height: f64) {
        self.root_widget.sync(&mut self.state.doc, &self.state.root);
        self.structure_dirty |= self.root_widget.take_structure_dirty();
        if self.structure_dirty {
            self.rebuild_structure();
        }
        self.layout_for_test(width, height);
    }

    /// Rebuild the taffy tree from the current widget tree after a
    /// structural mutation, re-establishing keyboard focus.
    ///
    /// When [`Self::autofocus_on_structure_change`] is set this *also*
    /// preserves the focused widget's **ephemeral** caret / selection state
    /// across the rebuild. The previously-focused widget's [`Widget::get_state`]
    /// is captured BEFORE the tree is torn down; after the rebuild, provided
    /// focus lands back on the SAME focusable ordinal (so we never paste a
    /// caret onto a different field), [`Widget::set_state`] restores it on the
    /// new focused widget. This mirrors the undo path
    /// ([`Self::apply_undo_entry`]), which already round-trips widget metadata
    /// — without it a rebuild triggered by an unrelated state change (a
    /// validation line appearing, a sibling status row) silently snaps the
    /// caret back to offset 0.
    ///
    /// If the root raised [`Widget::take_focus_reset`] (a wizard advancing to
    /// a new step, or — in the greeter — an auth-phase transition that changes
    /// which fields are focusable) focus jumps to the first focusable of the
    /// rebuilt tree and no caret is restored, because the focused field
    /// legitimately changed.
    ///
    /// Exposed (doc-hidden) so the headless tests can exercise focus + caret
    /// continuity without a real event loop.
    #[doc(hidden)]
    pub fn rebuild_structure(&mut self) {
        // Capture pre-rebuild focus ordinal, the focused widget's ephemeral
        // state, and the root's one-shot focus-reset request BEFORE the tree
        // is torn down — the focusable node ids are about to be invalidated.
        let (prev_focus_idx, prev_focus_state, focus_reset) =
            if self.autofocus_on_structure_change {
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

        // A focus trap (open modal) already placed focus inside its subtree in
        // `update_focusable_order`; the structure-change autofocus must not
        // override that and let focus escape the modal.
        if self.autofocus_on_structure_change && !self.focus_trap_active {
            if focus_reset {
                // Step / phase transition: hand focus to the first field of
                // the freshly-built tree. No caret restore — the focused
                // field deliberately changed.
                if let Some(&first) = self.focusable_order.first() {
                    self.set_focused_node(Some(first));
                }
            } else if let Some(idx) = prev_focus_idx {
                // Incidental rebuild (e.g. a status line appeared): keep focus
                // on the same ordinal so the caret doesn't vanish mid-edit.
                // We only restore the caret/selection when the SAME ordinal
                // still exists in the rebuilt tree — a shifted ordinal means
                // the focusable set changed and we can't prove it's the same
                // logical field, so we clamp focus but skip the caret paste.
                let exact = self.focusable_order.get(idx).copied();
                let target = exact.or_else(|| self.focusable_order.last().copied());
                if let Some(id) = target {
                    self.set_focused_node(Some(id));
                    if exact.is_some() {
                        if let Some(state) = prev_focus_state.as_deref() {
                            if let Some(&w) = self.node_map.get(&id) {
                                unsafe { (*w).set_state(state); }
                            }
                        }
                    }
                }
            }
        }

        // The widgets built above are *fresh* instances (apps that rebuild
        // their composition — the greeter, the installer wizard — recreate
        // their children in `build_layout`). The frame's `sync` ran against
        // the OLD tree, so without this the new widgets would draw blank
        // until the next event. Re-sync now so they pull their CRDT state
        // this frame; this also clamps any caret restored just above against
        // the now-populated text. Cheap, and a no-op for widgets that don't
        // read state in `sync`.
        self.root_widget.sync(&mut self.state.doc, &self.state.root);
        self.needs_sync = false;
    }

    /// Persist this App's CRDT document through `autosave`.
    ///
    /// Pair with [`crate::persist::open`], which hands back the document the
    /// previous session left behind plus the writer for it:
    ///
    /// ```ignore
    /// let (state, autosave) = ui::persist::open("dev.qos.my-app");
    /// ui::App::new(state, my_app::app_root())
    ///     .with_autosave(autosave)
    ///     .run();
    /// ```
    ///
    /// From here on every change the document takes is written back —
    /// debounced, atomically, and incrementally — and flushed when the event
    /// loop exits. Nothing else in the App changes; widgets keep mutating the
    /// same `AutoCommit` they always did.
    pub fn with_autosave(mut self, autosave: crate::persist::Autosave) -> Self {
        self.autosave = Some(autosave);
        self
    }

    /// Set the window title explicitly, overriding the automatic
    /// resolution (env / package id / binary name). Builder form, for an
    /// app that wants a title other than its package name — most don't,
    /// and get their real name for free. Takes effect when the window is
    /// created, so set it before [`App::run`].
    pub fn with_window_title(mut self, title: impl Into<String>) -> Self {
        self.window_title = Some(title.into());
        self
    }

    /// Set the window title explicitly at runtime. Same precedence as
    /// [`App::with_window_title`]; applied when the window is next
    /// created. `None` clears the override and restores automatic
    /// resolution.
    pub fn set_window_title(&mut self, title: Option<String>) {
        self.window_title = title;
    }

    /// The title this App would give its window right now, resolved
    /// through the same precedence the window uses (explicit override →
    /// `QOS_APP_NAME` → package id → binary name → generic default).
    /// Exposed so the resolution is observable in tests and by a host
    /// that mirrors the title elsewhere.
    pub fn resolved_window_title(&self) -> String {
        resolve_window_title(self.window_title.as_deref())
    }

    /// Make this App's live document a **record's** document.
    ///
    /// `records` owns the per-record documents and their persistence;
    /// `room_index` is the room's list of which records it holds — normally
    /// the document [`crate::persist::open`] hands back, since that document
    /// already is per-room state. The root widget names the record it is on
    /// via [`crate::widget::Widget::record_intent`] and the App checks it out
    /// before the next `sync`.
    ///
    /// Pair it with [`App::with_autosave`] only if the room index itself
    /// should be persisted — the records persist through `records`, not
    /// through the App's autosave, so the two are not alternatives.
    pub fn with_records(
        mut self,
        records: crate::record::RecordSet,
        room_index: automerge::AutoCommit,
    ) -> Self {
        self.records = Some(records);
        self.room_index = Some(room_index);
        self
    }

    /// The records this App has open, if it adopted them.
    pub fn records(&self) -> Option<&crate::record::RecordSet> {
        self.records.as_ref()
    }

    /// The room index this App writes record references into.
    pub fn room_index(&self) -> Option<&automerge::AutoCommit> {
        self.room_index.as_ref()
    }

    /// Check out the record the root widget names, if it differs from the
    /// live one.
    ///
    /// Runs once per frame, before `sync`, so a widget that opened a file
    /// during the previous frame is editing that file's own document by the
    /// time it next reads state. On a change the collaboration channel is
    /// re-announced: the record is the routing key, so a checkout is a change
    /// of address, and continuing to publish this document's deltas under the
    /// previous record's name is what "opening a second file merged the
    /// first" was made of.
    ///
    /// Returns whether the live document changed.
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
                // The freshly-adopted document has not been through `sync`,
                // and the widget's caches still describe the outgoing one.
                self.needs_sync = true;
                self.layout_dirty = true;
                true
            }
            None => false,
        }
    }

    /// Offer the current document to the background writer.
    ///
    /// Called once per idle tick. Cheap when nothing changed: comparing two
    /// head vectors, no serialisation and no I/O.
    fn autosave_tick(&mut self) {
        // With records in play the live document belongs to a record and is
        // written through that record's own store; the App's autosave (when
        // installed alongside) writes the room index instead. Sending the
        // live document to both would put a record's content into the room
        // index file.
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

    /// Write out anything still pending and wait for it to land. Called on
    /// event-loop teardown so a `kill` right after the last keystroke still
    /// finds it on disk.
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

    /// The writer installed by [`App::with_autosave`], if any. Exposed so a
    /// caller can check [`crate::persist::Autosave::last_error`] rather than
    /// assuming the disk accepted the writes.
    pub fn autosave(&self) -> Option<&crate::persist::Autosave> {
        self.autosave.as_ref()
    }

    pub fn run(self) {
        self.run_with_setup(|_| {});
    }

    /// Like [`App::run`] but exposes the winit [`EventLoopProxy`] before
    /// the loop starts running so callers (e.g. the shell's hosted-mode
    /// `QOS_TEST_CONTROL` socket listener) can spawn background threads
    /// that inject [`UserEvent`]s into the App. The closure runs once,
    /// after the event loop is created and before `run_app`. Any thread
    /// the closure spawns may move the proxy and `send_event(_)` from
    /// any thread; the App processes each via `ApplicationHandler::
    /// user_event`, translating to the equivalent `WindowEvent` and
    /// routing through the SAME dispatch path as hardware events.
    pub fn run_with_setup<F>(mut self, setup: F)
    where
        F: FnOnce(EventLoopProxy<UserEvent>),
    {
        let event_loop = EventLoop::<UserEvent>::with_user_event()
            .build()
            .unwrap();
        let proxy = event_loop.create_proxy();
        // Keep a clone for the AccessKit adapter (it routes initial-tree /
        // action-request / deactivation events back through this same proxy as
        // `UserEvent::AccessKit`).
        let a11y_proxy = proxy.clone();
        setup(proxy);

        // Build the taffy tree up-front so widgets that don't depend on a
        // live window (focus order, undo bookkeeping) are wired before the
        // first `resumed` callback.
        self.init_layout();

        let mut handler = AppHandler {
            app: self,
            window: None,
            render: None,
            a11y_adapter: None,
            a11y_proxy,
        };
        event_loop.run_app(&mut handler).unwrap();
    }
}

/// Resolve the human-readable window title from the process environment,
/// given any explicit override the app set. Reads `QOS_APP_NAME` (the
/// launcher exports the manifest `[package].name`), `QOS_PACKAGE_ID`
/// (the app_id — `dev.qos.notes`), and the running binary's file name.
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

/// Pure title resolution, so the precedence is unit-testable without
/// touching the process environment. In order, the first non-blank
/// candidate wins:
///
/// 1. `explicit` — a title the app set via [`App::with_window_title`].
/// 2. `app_name` — `QOS_APP_NAME`, the manifest's `[package].name`.
/// 3. the last dot-segment of `package_id` (`QOS_PACKAGE_ID`),
///    humanized: `dev.qos.notes` → `Notes`.
/// 4. `binary`, humanized: a dev run with no launcher env gets
///    `file-explorer` → `File Explorer`.
/// 5. a generic `qOS App` — never the old `QuickUI App` placeholder.
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

/// Turn a machine identifier into a human title: split on `-`, `_`, `.`,
/// and whitespace, drop the empties, upper-case each word's first
/// character, and join with a single space. `file-explorer` →
/// `File Explorer`, `notes` → `Notes`.
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

/// Winit 0.30 `ApplicationHandler` driver for [`App`]. The previous
/// closure-based event loop captured `self`, the window, the surface and
/// the renderer; the new pattern hoists that state into a struct so winit
/// can call back into us with `&mut self`.
///
/// As of the Item-4 render-thread refactor (2026-05-23 perf plan), the
/// wgpu `RenderContext`, `RenderSurface`, and `vello::Renderer` all live
/// on a dedicated worker thread — see [`crate::render_thread`]. The
/// `AppHandler` keeps only the channel ends needed to ship frames to the
/// worker and recycle scene buffers; the `Scene` buffer itself moves
/// between main and worker per frame via a bounded pool channel.
struct AppHandler {
    app: App,
    /// `None` until winit fires the first `resumed` event. On platforms
    /// that gate window creation behind an `ActiveEventLoop` (every modern
    /// platform), creating the window any earlier would panic.
    window: Option<Arc<Window>>,
    /// The render-thread plumbing (channels + join handle). `None` until
    /// the first `resumed` fires; set to `None` again in `exiting` after
    /// the worker is joined so its surface is dropped BEFORE the
    /// `Window` arc goes away.
    render: Option<RenderThreadHandle>,
    /// AccessKit ↔ winit adapter. `None` until `resumed` creates the
    /// window (the adapter must be created while the window is hidden, then the
    /// window is shown). On Linux this pulls in `accesskit_unix`, which
    /// publishes our exported tree over AT-SPI2 so Orca / braille work. The
    /// adapter is **lazy**: `update_if_active` is a no-op until an AT actually
    /// connects, so the hot path pays nothing when no screen reader is running.
    /// Guarded so headless / no-window code paths never require it.
    a11y_adapter: Option<accesskit_winit::Adapter>,
    /// Proxy used to construct the adapter (it delivers accesskit events back
    /// through the winit event loop as [`UserEvent::AccessKit`]).
    a11y_proxy: EventLoopProxy<UserEvent>,
}

impl AppHandler {
    /// Lazily initialise the wgpu surface + renderer and spawn the render
    /// worker. Called once, from the first `RedrawRequested`, so the heavy
    /// blocking GPU init runs AFTER the winit event loop has processed the
    /// initial `configure` and wired the seat — not inline in `resumed`,
    /// where it would starve those first events and race first-launch focus.
    fn init_render(&mut self, window: &Arc<Window>) {
        let size = window.inner_size();
        let mut render_cx = RenderContext::new();
        let surface = pollster::block_on(render_cx.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            vello::wgpu::PresentMode::AutoVsync,
        ))
        .unwrap();
        let device_handle = &render_cx.devices[surface.dev_id];
        // vello 0.9's `create_surface` builds the blitter pipeline against
        // the format it chose; flipping to the sRGB suffix here would
        // panic on the format mismatch in the first blit.
        surface.surface.configure(&device_handle.device, &surface.config);
        let renderer = Renderer::new(
            &device_handle.device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::all(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .unwrap();
        // Hand the wgpu state off to the render worker. After this point
        // the main thread doesn't touch the surface, renderer, or
        // render_cx — every GPU call happens on the `ui-render` thread.
        self.render = Some(spawn_render_thread(render_cx, surface, renderer));
    }
}

impl ApplicationHandler<UserEvent> for AppHandler {
    /// External-input translator: every variant becomes the WindowEvent
    /// winit would have delivered, then routes back through
    /// `Self::window_event` so injected and hardware events take the
    /// IDENTICAL code path (same dispatch, same handlers, same redraw
    /// gate). The unsafe transmute of a zeroed DeviceId is the same
    /// trick `shell::compositor::native` uses for the headless event
    /// path — winit's DeviceId is opaque and the dispatch chain doesn't
    /// inspect it.
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let Some(window) = self.window.as_ref().cloned() else { return };
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
                // Out-of-band payload: hand straight to the root widget,
                // then drive a re-sync + redraw so its mutation paints.
                // (Used by the qOS installer to stream backend progress
                // from a worker thread into its status log.)
                let app = &mut self.app;
                app.root_widget
                    .on_external_event(&payload, &mut app.state.doc, &app.state.root);
                app.needs_sync = true;
                window.request_redraw();
            }
            UserEvent::AccessKit(ak_event) => {
                use accesskit_winit::WindowEvent as AkWindowEvent;
                match ak_event {
                    AkWindowEvent::InitialTreeRequested => {
                        // An AT just connected. Push the full tree so the
                        // adapter has something to publish immediately.
                        if let Some(adapter) = self.a11y_adapter.as_mut() {
                            let scale = window.scale_factor();
                            let app_ref: &App = &self.app;
                            adapter.update_if_active(|| app_ref.build_a11y_tree_update(scale));
                        }
                        window.request_redraw();
                    }
                    AkWindowEvent::ActionRequested(req) => {
                        // Map the AccessKit action (+ data) back to ours and
                        // dispatch it to the target widget. A `Some((x, y))`
                        // result means "synthesise a click here" — we route it
                        // through the same path as a real / injected click.
                        if let Some(action) = crate::accesskit_bridge::accesskit_action_to_ours(
                            req.action,
                            req.data.as_ref(),
                        ) {
                            let click = self.app.dispatch_a11y_action(req.target_node, &action);
                            if let Some((x, y)) = click {
                                // Reuse the synthetic-click path (CursorMoved →
                                // Pressed → Released) so the activation takes the
                                // identical route a pointer click would.
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
                    AkWindowEvent::AccessibilityDeactivated => {
                        // All ATs gone — nothing to do; `update_if_active`
                        // becomes a no-op until the next activation.
                    }
                }
            }
        }
    }


    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // `resumed` can fire more than once (suspend/resume on mobile);
        // after the first init the window/render state stays valid, so
        // subsequent calls are no-ops.
        if self.window.is_some() {
            return;
        }
        // Attach the clipboard to this app's own Wayland connection, before
        // any window exists — a copy has to work from the first frame, and the
        // backend only needs the display, not a surface. Sharing the app's
        // connection is required, not incidental: the compositor only accepts
        // `set_selection` from the connection that owns the focused surface.
        // No-ops off Wayland, and no-ops when a host (the compositor, for its
        // in-process widgets) already installed one.
        #[cfg(target_os = "linux")]
        {
            crate::clipboard::install_wayland_backend_from_display(event_loop);
        }
        // The AccessKit adapter must be created while the window is still
        // hidden (it asserts this), so build the window invisible, create the
        // adapter, then show it.
        // The human-readable window title. Resolved through the app's
        // real name (an explicit override, then `QOS_APP_NAME` from the
        // manifest, then the package id, then the binary name) rather
        // than a toolkit placeholder — so every window's titlebar / task
        // entry reads as the app, not "QuickUI App".
        let title = resolve_window_title(self.app.window_title.as_deref());
        // The window icon: the app's own art when it ships some (via
        // `QOS_APP_ICON`), otherwise a generated qOS default. Resolved
        // once and used both as a creation attribute and via
        // `set_window_icon`, so whichever the backend honours takes it.
        let icon = crate::window_icon::resolve();
        let mut window_attributes = Window::default_attributes()
            .with_title(title)
            .with_window_icon(icon.clone())
            // Draw a normal, draggable title bar (winit's sctk-adwaita CSD)
            // on compositors that don't provide server-side decorations —
            // e.g. GNOME/mutter. Explicit even though it's the winit
            // default, so the intent is unmistakable.
            .with_decorations(true)
            .with_visible(false)
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        // Give the xdg-toplevel a real app_id, and make it *this app's*.
        //
        // Without one the app_id is the empty string, and mutter keys
        // window identity, focus / xdg-activation, and its decoration
        // policy off the app_id — an empty id is the concrete difference
        // from every GTK / other Wayland app (all of which come up
        // focused + decorated).
        //
        // A single shared id is nearly as bad, and on qOS it is worse.
        // The compositor has no other handle on which window belongs to
        // which launch: it labels the tab from the app_id, matches a
        // launch's placement note by it, and answers "is this document
        // already open" by it. With every app announcing `org.qos.app`
        // those all resolve to the same thing — every tab is called
        // "app", no placement note is ever claimed, and a document that
        // is open cannot be recognised as open. `QOS_PACKAGE_ID` is the
        // identity the launcher already puts in the sandbox for exactly
        // this reason (see `crate::persist`, `crate::record`), so it is
        // the id to announce. Outside a sandbox there is no package, and
        // the generic id is the honest answer.
        //
        // Wayland only; a no-op on other winit backends.
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            let app_id = std::env::var("QOS_PACKAGE_ID")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "org.qos.app".to_string());
            window_attributes = window_attributes.with_name(app_id, "");
        }
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());
        // Also set the icon post-creation: some backends only honour it
        // this way, not as a creation attribute. A no-op on Wayland
        // compositors that source the icon from the app_id instead.
        window.set_window_icon(icon);

        // Accessibility: wire the AccessKit winit adapter so the exported
        // tree reaches AT-SPI2 (Orca, braille). Events (initial-tree request,
        // action requests, deactivation) are delivered back through our event
        // loop proxy as `UserEvent::AccessKit`. This is intentionally created
        // before the window is shown.
        self.a11y_adapter = Some(accesskit_winit::Adapter::with_event_loop_proxy(
            event_loop,
            &window,
            self.a11y_proxy.clone(),
        ));
        window.set_visible(true);

        // Mark the freshly-built taffy tree dirty so the very first
        // `RedrawRequested` re-runs layout against the actual window
        // size and scale factor.
        self.app.layout_dirty = true;
        // Drive the first frame. GPU init (create_surface / renderer /
        // render-worker spawn) is DEFERRED to that first `RedrawRequested`
        // — see `init_render` + `window_event`. Doing the ~50 ms
        // `pollster::block_on` surface+device init inline here would block
        // the winit Wayland event loop during `resumed`, starving the
        // initial xdg `configure` and the wl_seat wiring; on mutter that
        // intermittently left the toplevel unfocused with dead input on
        // first launch ("works on the second try"). Deferring lets the
        // event loop complete the configure + seat handshake first.
        window.request_redraw();
        self.window = Some(window);
    }

    /// Winit calls this once on event-loop teardown. We use it to
    /// shut the render thread down BEFORE the `Window` arc is
    /// dropped — the worker's `RenderSurface<'static>` references the
    /// window, and wgpu may panic if the surface outlives the window.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Last chance to get the user's final edit onto the disk — after
        // this the process is on its way out.
        self.app.autosave_flush();
        if let Some(mut render) = self.render.take() {
            render.shutdown_and_join();
            // `render` (and the worker-owned surface inside it) is
            // now dropped. Only after this is it safe to let
            // `self.window` go.
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_ref().cloned() else {
            return;
        };
        if window.id() != window_id {
            return;
        }
        // Accessibility: let the AccessKit adapter observe every window event
        // before the app handles it (it tracks focus / window state for the
        // AT-SPI bridge). No-op when no AT is attached.
        if let Some(adapter) = self.a11y_adapter.as_mut() {
            adapter.process_event(&window, &event);
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_size) => {
                // Resize is handled lazily on the worker: the
                // `RenderRequest` carries the latest dimensions and
                // the worker calls `resize_surface` when they
                // differ from the live surface config. Main only
                // needs to drive a redraw — the next frame the
                // worker pulls will see the new size on `window
                // .inner_size()`.
                self.app.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                // The display's fractional scale factor changed (moved to
                // a HiDPI monitor, or the compositor sent a new
                // `wp_fractional_scale.preferred_scale`). winit delivers a
                // fractional `f64` here; the render path reads the live
                // `window.scale_factor()` each frame and composites the
                // whole scene through `Affine::scale(scale_factor)`
                // (physical-pixel surface, logical-pixel layout), so a
                // 1.25/1.5/1.75× factor renders crisp rather than being
                // rounded to an integer. We only need to invalidate layout
                // and redraw so the new factor is picked up — winit resizes
                // the surface to keep the logical size stable, and the
                // worker reconfigures it on the next frame.
                self.app.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                // Deferred one-time GPU init on the first frame (see
                // `init_render`): the surface + renderer are built here,
                // after the event loop has processed the initial configure
                // and wired the seat, rather than inline in `resumed`.
                if self.render.is_none() {
                    qos_startup_metrics::mark("first-redraw-init-render-start");
                    self.init_render(&window);
                    qos_startup_metrics::mark("first-redraw-init-render-done");
                } else {
                    // Every subsequent redraw hits the "first-paint"
                    // mark only the first time — subsequent frames are
                    // uninteresting for the cold-start measurement.
                    // A boolean guard is cheaper than a lock; the mark
                    // itself is a no-op past the first call because the
                    // metrics module records every push and downstream
                    // consumers just look at the first occurrence.
                    qos_startup_metrics::mark("first-paint");
                }
                let Some(render) = self.render.as_mut() else {
                    return;
                };
                let app = &mut self.app;
                // Drain any pending FrameOutcomes from the worker
                // BEFORE building this frame. If the previous
                // frame was `Reconfigured` or `Skipped`, we'll
                // re-request a redraw after this one so the user
                // doesn't see a stale image.
                let mut needs_followup_redraw = false;
                while let Ok(outcome) = render.frame_complete.try_recv() {
                    match outcome {
                        FrameOutcome::Presented => {}
                        FrameOutcome::Reconfigured | FrameOutcome::Skipped => {
                            needs_followup_redraw = true;
                        }
                    }
                }

                // Pull a recycled Scene buffer from the worker's
                // pool. The pool is seeded with two Scenes at
                // spawn time so the first frame doesn't block;
                // afterwards this `recv()` is the backpressure
                // boundary — if the worker is still draining the
                // previous frame, main blocks here until it
                // catches up. Capacity-2 pool means at most one
                // frame of overlap (main building N+1 while
                // worker presents N).
                let mut scene = match render.scene_pool.recv() {
                    Ok(s) => s,
                    Err(_) => {
                        tracing::error!("ui-render thread died; closing window");
                        event_loop.exit();
                        return;
                    }
                };
                scene.reset();

                // Pull the current window dimensions directly
                // from the window — the worker only learns about
                // resizes via the dims carried in this request.
                let inner = window.inner_size();
                let width = inner.width;
                let height = inner.height;
                let scale_factor = window.scale_factor() as f32;
                // Record the size this frame is built at so `about_to_wait`
                // can notice a later allocation change (a resize that never
                // produced a `Resized` event, or one that coalesced) and
                // reflow without waiting on stray input.
                app.last_frame_size = Some(inner);

                // Records first: if the root widget opened a different
                // record, the document it is about to sync against is that
                // record's, not the outgoing one's.
                app.record_tick();

                // Frame profiler (gated). `__prof_frame_start` is `Some` only
                // when `QOS_PROFILE`/`QOS_PROFILE_OUT` installed a profiler, so
                // the disabled path reads no clock. It marks the start of both
                // the `sync` pass and the whole-frame total committed below.
                let __prof_frame_start =
                    app.profiler.as_ref().map(|_| std::time::Instant::now());
                if app.needs_sync {
                    app.root_widget.sync(&mut app.state.doc, &app.state.root);
                    app.needs_sync = false;
                }
                if let Some(p) = app.profiler.as_mut() {
                    p.record_since(crate::profiling::Pass::Sync, __prof_frame_start);
                }

                // Live regions: after the tree reflects the freshly-synced
                // state, announce any live region whose spoken text changed —
                // a new toast, an updated status line — without moving focus.
                app.scan_live_regions();

                // Poll the tree for structural mutations that
                // happened since the last frame (e.g. a tab
                // split that inserted a brand-new WindowGroup
                // into a GroupRow). If any subtree reports
                // dirty, rebuild the taffy tree from scratch so
                // the new widgets get node ids and the layout
                // pass below sees them.
                app.structure_dirty |= app.root_widget.take_structure_dirty();
                // Capture BEFORE `rebuild_structure` clears it: a fresh taffy
                // tree always needs a full solve regardless of the scroll fast
                // path below.
                let structure_rebuilt = app.structure_dirty;
                let __prof_build = app.profiler.as_ref().map(|_| std::time::Instant::now());
                if app.structure_dirty {
                    // Rebuild the taffy tree, re-establishing keyboard focus
                    // AND (for `autofocus_on_structure_change` apps) the
                    // focused widget's caret/selection across the rebuild.
                    app.rebuild_structure();
                }
                // Widget-tree (re)build is part of the build_layout pass; the
                // taffy `compute_layout` below adds to the same pass.
                if let Some(p) = app.profiler.as_mut() {
                    p.record_since(crate::profiling::Pass::BuildLayout, __prof_build);
                }

                // Phase timing (gated). `QUICK_UI_PERF=1` enables a
                // per-frame stderr breakdown of compute_layout /
                // apply_layout / draw_widget_tree / render_to_surface /
                // present. The flag is checked once and cached in a
                // static so we don't hit env vars on the hot path.
                let perf = perf_enabled();
                let t_frame_start = if perf { Some(std::time::Instant::now()) } else { None };
                let mut t_compute_us = 0u128;
                let mut t_apply_us = 0u128;
                let mut t_relayout_branch = false;

                let root_node = app.root_node.unwrap();
                if app.layout_dirty {
                    t_relayout_branch = true;
                    // Scroll-reposition fast path: a frame that is `layout_dirty`
                    // only because content scrolled needs `apply_layout` to push
                    // the new offset down to the children, but NOT a full taffy
                    // `compute_layout` — the intrinsic sizes and relative boxes
                    // are unchanged. Re-solving the whole content tree on every
                    // scroll frame was the dominant per-frame cost on long lists
                    // and grids. We still force a full solve whenever the tree
                    // was just rebuilt, the window changed size (available space
                    // differs), or any widget genuinely needs re-measuring
                    // (`layout_needs_measure`). `QUICK_UI_NO_SCROLL_OPT=1`
                    // disables the fast path (always solve) as a safety escape.
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
                            crate::scale::logical_size((width, height), scale_factor as f64);
                        app.taffy
                            .compute_layout_with_measure(
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
                            )
                            .unwrap();
                        t_compute_us = t0.elapsed().as_micros();
                        app.last_layout_px = Some((width, height));
                        if let Some(p) = app.profiler.as_mut() {
                            p.record(
                                crate::profiling::Pass::BuildLayout,
                                std::time::Duration::from_micros(t_compute_us as u64),
                            );
                        }
                    }
                    let t1 = std::time::Instant::now();
                    app.root_widget.apply_layout(
                        &app.taffy,
                        root_node,
                        vello::kurbo::Point::new(0.0, 0.0),
                    );
                    t_apply_us = t1.elapsed().as_micros();
                    if let Some(p) = app.profiler.as_mut() {
                        p.record(
                            crate::profiling::Pass::ApplyLayout,
                            std::time::Duration::from_micros(t_apply_us as u64),
                        );
                    }
                    app.layout_dirty = false;
                    if must_compute {
                        app.update_focusable_order();
                    }
                }

                let t_draw_start = if perf { Some(std::time::Instant::now()) } else { None };
                // Composite the whole logical scene at the per-output device
                // scale. Widget rects are logical; Vello rasterises glyphs and
                // strokes at the physical resolution this transform implies, so
                // the frame is crisp at any fractional scale (see `crate::scale`).
                let transform = crate::scale::composite_transform(scale_factor as f64);
                // Initialise the spatial-culling viewport to the
                // entire window in *logical* coords (the App's
                // transform applies a scale_factor at composite
                // time, so widget rects live in logical space).
                // Scrollables push a tighter inner viewport during
                // their child recursion. Restored to `None` after
                // draw so the bench's offscreen renderer paths
                // aren't accidentally bound to a stale viewport.
                let (vp_w, vp_h) = crate::scale::logical_size((width, height), scale_factor as f64);
                let window_rect = vello::kurbo::Rect::new(0.0, 0.0, vp_w, vp_h);
                let prev_vp = app.presence_ctx.push_viewport(window_rect);
                // No GPU damage clip: vello allocates a fresh `target_texture`
                // per frame, so any `scene.push_layer` clipping out the static
                // region simply paints those tiles as background instead of
                // preserving last frame. A correct damage path would need to
                // retain + re-blit the previous target texture — deferred.
                let __prof_draw = app.profiler.as_ref().map(|_| std::time::Instant::now());
                app.root_widget.draw_with_presence(
                    &mut scene,
                    transform,
                    &mut app.text_renderer,
                    &app.theme,
                    &app.presence_ctx,
                    &app.permission_ctx,
                );
                app.presence_ctx.pop_viewport(prev_vp);
                // Mark every visited widget as drawn — clears their
                // per-widget `dirty` flag so `needs_redraw` returns
                // false on the next frame unless an event mutates
                // them. Without this the incremental gate is a no-op:
                // widgets stay "dirty" forever.
                app.root_widget.mark_drawn();

                // Shared keyboard-focus ring: paint a themed ring around the
                // focused widget unless it draws its own indicator. Runs after
                // the tree so the ring composites on top of the focused
                // control, and before the overlay/palette passes so an open
                // dropdown or modal still paints above it.
                app.compose_focus_ring(&mut scene, transform);

                // Top-layer overlay pass: widgets whose visual extent
                // escapes their layout box (an open `Dropdown` list, a
                // popover) paint here so they composite ABOVE every
                // later-drawn sibling instead of being occluded by it.
                // Default is a no-op recursion, so this is free for the
                // shell / greeter and every widget that doesn't opt in.
                app.root_widget.draw_overlay(
                    &mut scene,
                    transform,
                    &mut app.text_renderer,
                    &app.theme,
                    &app.presence_ctx,
                );

                // The Ctrl+K action palette paints last so it composites above
                // the whole app. Centered near the top, sized to its content.
                if let Some(ap) = app.action_palette.as_mut() {
                    let win_w = (width as f64) / (scale_factor as f64);
                    let card_w = (win_w * 0.6).clamp(360.0, 640.0);
                    let x0 = ((win_w - card_w) / 2.0).max(0.0);
                    let y0 = (height as f64 / (scale_factor as f64) * 0.12).max(24.0);
                    ap.layout(&app.theme, Point::new(x0, y0), card_w);
                    ap.draw(
                        &mut scene,
                        transform,
                        &mut app.text_renderer,
                        &app.theme,
                        &app.presence_ctx,
                    );
                }
                // Central hover tooltip — painted last so the single chip
                // composites above the whole app (widget tree, overlays and the
                // action palette). Tick the wall clock, and if a tooltip has
                // dwelled long enough to reveal, resolve its i18n label and draw
                // one themed chip anchored to the hovered control, flipped and
                // clamped to stay inside the window.
                app.tooltip_host.tick(Instant::now());
                if let Some((text, anchor)) = app
                    .tooltip_host
                    .visible()
                    .map(|(label, anchor)| (crate::i18n::I18nManager::lookup(label, None), anchor))
                {
                    let win_w = (width as f64) / (scale_factor as f64);
                    let win_h = (height as f64) / (scale_factor as f64);
                    let viewport = vello::kurbo::Rect::new(0.0, 0.0, win_w, win_h);
                    crate::widget::tooltip::draw_hover_tooltip(
                        &mut scene,
                        transform,
                        &mut app.text_renderer,
                        &app.theme,
                        anchor,
                        &text,
                        viewport,
                    );
                }

                // The `draw` pass covers the whole widget tree, the top-layer
                // overlay, and the action palette — everything encoded into the
                // scene above (but NOT the profiling HUD, which is drawn after
                // the timing is committed so it never inflates its own numbers).
                if let Some(p) = app.profiler.as_mut() {
                    p.record_since(crate::profiling::Pass::Draw, __prof_draw);
                }
                let t_draw_us = t_draw_start.map(|t| t.elapsed().as_micros()).unwrap_or(0);

                // Draw Remote Cursors (legacy path — `state.collab.cursors`
                // is the pre-PresenceCtx map; kept for compat with apps
                // that still write into it directly).
                for (_, cursor) in &app.state.collab.cursors {
                    let mut path = vello::kurbo::BezPath::new();
                    path.move_to(Point::new(0.0, 0.0));
                    path.line_to(Point::new(14.0, 14.0));
                    path.line_to(Point::new(6.0, 16.0));
                    path.line_to(Point::new(0.0, 22.0));
                    path.close_path();

                    let cursor_transform =
                        transform * Affine::translate((cursor.x as f64, cursor.y as f64));
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        cursor_transform,
                        cursor.color.clone(),
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
                        &mut scene,
                        &cursor.name,
                        10.0,
                        Color::WHITE,
                        cursor_transform * Affine::translate((10.0, 20.0)),
                        None,
                    );
                }

                // Remote peer presence (both collab-socket and libp2p
                // sources land in `remote_cursors`): one shared helper
                // paints each on-screen peer as a round, color-coded dot +
                // name pill, and each OFF-screen peer as a colored edge
                // mist pointing the way they drifted, so nobody vanishes.
                // `pointers` is kept populated for the shell overlay but
                // not painted here, so no peer is drawn twice.
                crate::presence::draw_remote_presence(
                    &mut scene,
                    transform,
                    window_rect,
                    &mut app.text_renderer,
                    &app.theme,
                    &app.presence_ctx,
                );

                // Commit this frame to the profiler: the total is the measured
                // wall time of the whole frame build (so it includes the small
                // amount of work between passes), and any `handle_event` time
                // banked since the last frame is folded in here.
                if let (Some(p), Some(start)) = (app.profiler.as_mut(), __prof_frame_start) {
                    p.commit_frame(start.elapsed().as_micros() as u64);
                }

                if app.debug_mode {
                    let debug_outline_cycle = [
                        app.theme.tokens.colors.semantic.warning,
                        app.theme.tokens.colors.semantic.info,
                        app.theme.tokens.colors.semantic.success,
                    ];
                    fn draw_debug_recursive(
                        widget: &dyn Widget,
                        scene: &mut Scene,
                        transform: Affine,
                        text_renderer: &mut TextRenderer,
                        depth: usize,
                        cycle: &[Color; 3],
                    ) {
                        let rect = widget.get_rect();
                        let color = cycle[depth % 3];
                        scene.stroke(&vello::kurbo::Stroke::new(1.5), transform, color, None, &rect);
                        let label = format!("{}: {}", widget.name(), depth);
                        let text_y = (rect.y0 + 12.0).min(rect.y1);
                        let bg_rect =
                            KurboRect::new(rect.x0, rect.y0, rect.x0 + 100.0, rect.y0 + 14.0);
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
                            transform * Affine::translate((rect.x0 + 2.0, text_y)),
                            None,
                        );
                        for child in widget.children() {
                            draw_debug_recursive(
                                child,
                                scene,
                                transform,
                                text_renderer,
                                depth + 1,
                                cycle,
                            );
                        }
                    }
                    draw_debug_recursive(
                        app.root_widget.as_ref(),
                        &mut scene,
                        transform,
                        &mut app.text_renderer,
                        0,
                        &debug_outline_cycle,
                    );
                }

                // Accessibility: push the freshly-laid-out tree to the
                // AccessKit adapter. `update_if_active` only builds + sends the
                // `TreeUpdate` when an AT is actually attached, so this is free
                // (a single bool check) on the common no-screen-reader path.
                // Bounds are emitted in physical pixels (scale-multiplied) to
                // match what the AT-SPI bridge expects from the window.
                if let Some(adapter) = self.a11y_adapter.as_mut() {
                    let app_ref: &App = app;
                    let scale = scale_factor as f64;
                    adapter.update_if_active(|| app_ref.build_a11y_tree_update(scale));
                }

                // Hand the encoded Scene to the render worker. The
                // seven GPU steps that used to live here (encode →
                // get_current_texture → encoder → blit → submit →
                // present → poll) now run on the `ui-render` thread;
                // main returns from RedrawRequested as soon as the
                // channel send completes.
                //
                // The worker pushes the consumed `Scene` straight
                // back into `scene_pool` for the next frame, so
                // there is no per-frame `done` channel and no
                // extra forwarder thread.
                //
                // Profiling HUD: painted last so it composites on top of the
                // whole app, and AFTER the frame timing was committed above so
                // its own cost never enters the numbers it reports.
                if app.profiler.as_ref().is_some_and(|p| p.hud_enabled()) {
                    let (hud_w, hud_h) =
                        crate::scale::logical_size((width, height), scale_factor as f64);
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
                    scene,
                    width,
                    height,
                    base_color: app.theme.bg_app,
                };
                if render.render_tx.send(req).is_err() {
                    tracing::error!("ui-render thread receiver closed; exiting");
                    event_loop.exit();
                    return;
                }

                let t_send_us = t_frame_start.map(|t| t.elapsed().as_micros()).unwrap_or(0);
                if let Some(_start) = t_frame_start {
                    // Attribute the draw phase: how much of it was
                    // cosmic-text shaping (and how many labels hit vs.
                    // missed the shaped-glyph cache) vs. Vello scene
                    // encoding. On a static frame this should read
                    // `shape=0us hit=N miss=0` — every label reused.
                    let (shape_hits, shape_misses, shape_ns) =
                        app.text_renderer.take_shape_stats();
                    eprintln!(
                        "perf frame: main_total={:>5}us layout={}({}+{}us) draw={:>5}us (shape={:>4}us hit={} miss={}) send_to_worker={:>5}us",
                        t_send_us,
                        if t_relayout_branch { "Y" } else { "n" },
                        t_compute_us,
                        t_apply_us,
                        t_draw_us,
                        shape_ns / 1000,
                        shape_hits,
                        shape_misses,
                        t_send_us,
                    );
                }

                if needs_followup_redraw {
                    window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.app.modifiers = modifiers;
            }
            other => {
                let event = other;
                let app = &mut self.app;
                // Profiler `handle_event` pass: time the whole input-dispatch
                // arm (this covers pointer, keyboard, and wheel — every event
                // that reaches the widget tree). Banked per event and folded
                // into the next frame's commit. `None`, and no clock read, when
                // profiling is off. This arm has no early `return`, so the
                // single record at its tail runs for every input event.
                let __prof_input = app.profiler.as_ref().map(|_| std::time::Instant::now());
                // DEBUGABILITY (RUST_LOG=ui::app=debug): trace every
                // mouse/keyboard event hitting the App so a real
                // click reproducer (xdotool, ydotool, the live
                // window) can be diagnosed without source-reading.
                // Pointer-move spam is intentionally NOT throttled
                // here — when the bug is "no clicks work", you want
                // to see whether moves arrive at all.
                match &event {
                    WindowEvent::MouseInput { state, button, .. } => {
                        tracing::debug!(?state, ?button, "App::window_event MouseInput");
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        tracing::trace!(x = position.x, y = position.y, "App::window_event CursorMoved");
                    }
                    WindowEvent::KeyboardInput { event: ke, .. } => {
                        tracing::debug!(key = ?ke.logical_key, state = ?ke.state, "App::window_event KeyboardInput");
                    }
                    _ => {}
                }
                // Outbound pointer-presence broadcast: if the
                // shell has declared a presence scope, relay
                // every pointer move (throttled to ~30 Hz) so
                // peers can render this user's cursor on the
                // same scope. CursorLeft clears the peer-side
                // pointer via the `Left` payload so their
                // cursor stops rendering when we wander out
                // of the window.
                if let Some(scope) = app.presence_broadcast_scope.clone() {
                    match &event {
                        WindowEvent::CursorMoved { position, .. } => {
                            let now = Instant::now();
                            if pointer_broadcast_allowed(
                                app.last_pointer_broadcast,
                                now,
                                Duration::from_millis(POINTER_BROADCAST_MIN_GAP_MS),
                            ) {
                                app.last_pointer_broadcast = Some(now);
                                app.broadcast_presence(
                                    scope.clone(),
                                    PresencePayload::Pointer {
                                        x: position.x as f32,
                                        y: position.y as f32,
                                    },
                                );
                            }
                        }
                        WindowEvent::CursorLeft { .. } => {
                            app.broadcast_presence(scope.clone(), PresencePayload::Left);
                            app.last_pointer_broadcast = None;
                        }
                        _ => {}
                    }
                }

                let mut handled = false;
                let mut focus_target = None;
                let mut is_undo_redo = false;

                // The Ctrl+K action palette, while open, owns the keyboard: it
                // consumes keys (typing filters, Enter dispatches the selected
                // action, Escape closes) before anything reaches the app behind.
                // Ctrl+K itself is handled below (toggle), so let it through.
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
                            && app.modifiers.state().control_key()
                    );
                    if !is_ctrl_k
                        && matches!(event, WindowEvent::KeyboardInput { .. })
                        && app.action_palette_handle_key(&event)
                    {
                        handled = true;
                    }
                }

                // Screen-reader command layer: `Insert + <key>` drives the
                // reading cursor / say-all / stop without colliding with normal
                // navigation. Runs FIRST so it wins over Tab/arrow handling, and
                // is a no-op (returns false) when the reader is disabled. When
                // it consumes a command we still want a redraw (the narration
                // may have moved a visible reading highlight in the future), so
                // mark handled and let the tail of this branch request it.
                if app.handle_screen_reader_key(&event) {
                    handled = true;
                    is_undo_redo = true; // suppress an undo commit for a read cmd
                }

                // snapshot the focused control's state BEFORE dispatch, so
                // we can announce a resulting change (checked / on / value) —
                // whether it came from a mouse click or keyboard activation.
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

                // Snapshot the focused text field's caret BEFORE dispatch, so we
                // can announce the char/word a caret move (arrow / Home / End)
                // lands on once the widget has processed the key. Only keyboard
                // events can move a caret over existing content.
                let review_pre = if matches!(event, WindowEvent::KeyboardInput { .. }) {
                    app.text_review_snapshot()
                } else {
                    None
                };

                // Modal capture: when the tree hosts an open modal overlay it
                // reports its box rect here. `modal_rect` governs pointer
                // click-through only — an unhandled press inside the modal's
                // lifetime is swallowed so clicks can't fall through to widgets
                // behind it. Keyboard focus *containment* is a separate concern
                // driven by `traps_focus()` / `focus_trap_active`: while a trap
                // is active `focusable_order` is scoped to the modal, and
                // `keyboard_nav_allowed` lets Tab/arrow navigation run *within*
                // that scope (so a dialog's buttons stay reachable) instead of
                // being switched off wholesale.
                let modal_rect = app.root_widget.modal_capture();

                if !handled {
                if let WindowEvent::KeyboardInput {
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
                }

                if !handled && app.modifiers.state().control_key() {
                    if let WindowEvent::KeyboardInput {
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
                        // Capture the *current* focus + widget metadata so the
                        // opposite-stack entry carries where the caret is right
                        // now — otherwise round-tripping undo/redo loses caret
                        // position. `get_state()` is the non-mutating read half
                        // of the get/set pair (default returns None, so widgets
                        // that don't opt in are safe).
                        let current_meta = app.root_widget.get_state();
                        let current_focus_idx = app
                            .focused_node
                            .and_then(|id| app.focusable_order.iter().position(|x| *x == id));
                        match c.as_str() {
                            // Ctrl+Shift+Z is the Linux/Mac-native redo binding
                            // and winit delivers it as character "Z" while shift is
                            // held, so check the modifier directly rather than
                            // relying on the capital-letter cue.
                            "z" | "Z" if shift => {
                                if let Some(e) = app.state.redo_as(
                                    Some(&app.local_author),
                                    current_focus_idx,
                                    current_meta.clone(),
                                ) {
                                    app.apply_undo_entry(e);
                                }
                                is_undo_redo = true;
                                handled = true;
                            }
                            "z" | "Z" => {
                                if let Some(e) = app.state.undo_as(
                                    Some(&app.local_author),
                                    current_focus_idx,
                                    current_meta.clone(),
                                ) {
                                    app.apply_undo_entry(e);
                                }
                                is_undo_redo = true;
                                handled = true;
                            }
                            "y" | "Y" => {
                                if let Some(e) = app.state.redo_as(
                                    Some(&app.local_author),
                                    current_focus_idx,
                                    current_meta.clone(),
                                ) {
                                    app.apply_undo_entry(e);
                                }
                                is_undo_redo = true;
                                handled = true;
                            }
                            // Ctrl+K opens the action palette scoped to the
                            // focused app instance's open resources. A second
                            // Ctrl+K (palette already open) closes it.
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

                if !handled && app.keyboard_nav_allowed(modal_rect) {
                    if let WindowEvent::KeyboardInput {
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
                        // Cycles `focusable_order`, which is scoped to the modal
                        // subtree while a trap is active — so Tab stays inside a
                        // dialog and wraps last↔first there.
                        handled = app.focus_tab(shift);
                    }
                }

                if !handled {
                    if let WindowEvent::KeyboardInput {
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
                        if let (Some(d), true, true) =
                            (dir, app.modifiers.state().alt_key(), app.keyboard_nav_allowed(modal_rect))
                        {
                            app.navigate_focus(d);
                            handled = true;
                        }
                    }
                }

                // Global back/forward navigation gestures: two-finger
                // horizontal swipe (precise touchpad scroll) and
                // touchscreen edge pulls. Recognised here so every app
                // gets them; delivered to the widget tree via
                // `on_nav_gesture` (the WebView, a wizard, a pane stack —
                // whatever owns history — consumes it).
                if !handled {
                    use crate::gesture::GesturePhase;
                    let map_phase = |p: winit::event::TouchPhase| match p {
                        winit::event::TouchPhase::Started => GesturePhase::Begin,
                        winit::event::TouchPhase::Moved => GesturePhase::Update,
                        winit::event::TouchPhase::Ended
                        | winit::event::TouchPhase::Cancelled => GesturePhase::End,
                    };
                    let nav = match &event {
                        WindowEvent::MouseWheel { delta, phase, .. } => {
                            let (dx, dy, precise) = match delta {
                                winit::event::MouseScrollDelta::PixelDelta(p) => (p.x, p.y, true),
                                winit::event::MouseScrollDelta::LineDelta(x, y) => {
                                    (*x as f64, *y as f64, false)
                                }
                            };
                            app.nav_gestures.on_scroll(dx, dy, map_phase(*phase), precise)
                        }
                        WindowEvent::PanGesture { delta, phase, .. } => app
                            .nav_gestures
                            .on_scroll(delta.x as f64, delta.y as f64, map_phase(*phase), true),
                        WindowEvent::Touch(t) => {
                            let width = window.inner_size().width as f64;
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
                        let consumed = app.root_widget.on_nav_gesture(
                            dir,
                            &mut app.state.doc,
                            &app.state.root,
                        );
                        if consumed {
                            app.needs_sync = true;
                            window.request_redraw();
                            handled = true;
                        }
                    }
                }

                if !handled {
                    let scale_factor = window.scale_factor();
                    let root_node = app.root_node.unwrap();
                    let was_mouse_click = matches!(
                        event,
                        WindowEvent::MouseInput { state: winit::event::ElementState::Pressed, .. }
                    );
                    app.root_widget.handle_event(
                        &event,
                        scale_factor,
                        &app.modifiers,
                        &mut app.state.doc,
                        &app.state.root,
                        &mut handled,
                        &mut focus_target,
                        root_node,
                        &app.taffy,
                    );
                    if was_mouse_click {
                        tracing::debug!(
                            handled,
                            needs_redraw = app.root_widget.needs_redraw(),
                            is_layout_dirty = app.root_widget.is_layout_dirty(),
                            "root_widget.handle_event(MouseInput Pressed) returned"
                        );
                    }
                    // Click-through swallow: with a modal active, any pointer
                    // press the modal didn't already consume (e.g. a click on
                    // the dimmed backdrop outside the box, before the modal's
                    // own out-of-box dismiss has been wired) is marked handled
                    // so it can't reach widgets painted behind the scrim.
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

                // Keyboard activation: if no widget consumed Enter/Space (text
                // inputs do, so they keep typing), activate the focused control
                // via its `Activate` action — the same mutation a mouse click
                // performs. Buttons fire, checkboxes/switches flip. The state
                // change is spoken below by `announce_state_change`.
                //
                // If the focused widget declines the action (its
                // `perform_action` returns the default `false` — e.g. an
                // app-level custom clickable that only wires up mouse handling),
                // fall back to synthesising a real pointer click at its centre
                // through the same `handle_event` path a mouse takes
                // (CursorMoved -> Pressed -> Released). That makes *any*
                // focusable clickable keyboard-activatable without per-app
                // `perform_action` code — the mouse handler it already has is
                // enough.
                if !handled {
                    if let WindowEvent::KeyboardInput {
                        event:
                            winit::event::KeyEvent {
                                logical_key:
                                    winit::keyboard::Key::Named(
                                        winit::keyboard::NamedKey::Enter
                                        | winit::keyboard::NamedKey::Space,
                                    ),
                                state: winit::event::ElementState::Pressed,
                                ..
                            },
                        ..
                    } = event
                    {
                        if let Some(fid) = app.focused_node {
                            if let Some(&w_ptr) = app.node_map.get(&fid) {
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
                                } else {
                                    // Generic fallback: synthesise a click at the
                                    // focused control's centre. Mirrors the a11y
                                    // `Activate` fallback (`dispatch_a11y_action`)
                                    // and the injected-click path.
                                    let center = unsafe { (*w_ptr).get_rect().center() };
                                    handled = app.synth_activation_click(center, window.scale_factor());
                                }
                            }
                        }
                    }
                }

                // A focused range control (e.g. a slider) consumes Left/Right
                // to adjust its value; Up/Down and Tab still move focus, so you
                // can always navigate away. Modifiers scale the step:
                //   plain  → one step       Shift → four steps (coarse)
                //   Ctrl   → jump to min/max (Left = min, Right = max)
                // Other widgets ignore the action, so their Left/Right falls
                // through to directional navigation below. The resulting value
                // is spoken by `announce_state_change`.
                if !handled {
                    if let WindowEvent::KeyboardInput {
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
                        let ctrl = app.modifiers.state().control_key();
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
                        if let (Some(act), Some(fid)) = (action, app.focused_node) {
                            if let Some(&w_ptr) = app.node_map.get(&fid) {
                                let mut any = false;
                                for _ in 0..times {
                                    any |= unsafe {
                                        (*w_ptr).perform_action(
                                            &act,
                                            &mut app.state.doc,
                                            &app.state.root,
                                        )
                                    };
                                }
                                if any {
                                    handled = true;
                                    app.needs_sync = true;
                                    app.layout_dirty = true;
                                }
                            }
                        }
                    }
                }

                if !handled && app.keyboard_nav_allowed(modal_rect) {
                    if let WindowEvent::KeyboardInput {
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
                }

                // Broadcast the focused widget's caret/selection presence
                // and the root widget's location presence (diffed) so
                // peers can render the remote caret + indicative mist.
                // Cheap: no-ops when nothing changed or no socket is bound.
                app.broadcast_focus_presence();
                app.broadcast_location_presence();
                app.broadcast_view_presence();

                if let Some(id) = focus_target {
                    app.set_focused_node(Some(id));
                    handled = true;
                }

                // if the dispatch changed the *still-focused* control's
                // state (a checkbox the mouse toggled, a switch flipped by
                // Space), speak the new state. When focus moved instead,
                // `set_focused_node` already narrated it, and this is a no-op.
                app.announce_state_change(state_pre);

                // If the caret moved over unchanged text (review, not typing),
                // speak the character / word it landed on.
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
                    // Incremental-redraw gate (Unit 2 of the 2026-05-23
                    // perf plan), narrowed to its actual target: hover
                    // spam. The original "gate everything on
                    // `needs_redraw()`" was wrong because container
                    // widgets like Tabs / GroupRow / TabBar mutate
                    // their own active-index state without setting
                    // any descendant leaf widget's `dirty` flag —
                    // so the tree's recursive `needs_redraw` returned
                    // false and CLICKS WERE INVISIBLE on hosted-mode.
                    // (Headless was unaffected because the FrameBuilder
                    // dispatch path doesn't go through this gate.)
                    //
                    // The conservative gate now: pointer MOVES (the
                    // 60+ Hz stream the perf plan cared about) still
                    // skip a redraw when nothing reports dirty; every
                    // other handled event (MouseInput, KeyboardInput,
                    // …) requests a redraw unconditionally. This
                    // preserves the hover-spam win and restores
                    // click-to-paint.
                    let is_pointer_move = matches!(&event, WindowEvent::CursorMoved { .. });
                    if !is_pointer_move || app.root_widget.is_layout_dirty() {
                        app.layout_dirty = true;
                    }
                    if !is_pointer_move || app.root_widget.needs_redraw() {
                        window.request_redraw();
                    }
                }

                // OS cursor shape. After dispatching the move, hit-test the
                // widget tree at the pointer and adopt the cursor the most-
                // specific widget under it asks for — an I-beam over a text
                // field, an east-west / north-south arrow over a split or
                // resize handle, a pointer over a button. Resolved on every
                // `CursorMoved` (not just `handled` ones, so the shape stays
                // correct when the pointer crosses into a widget that doesn't
                // flip its own hover), but we only call winit's `set_cursor`
                // when the shape actually changes so the per-move path stays
                // cheap and the incremental-render gate is left untouched.
                if let WindowEvent::CursorMoved { position, .. } = &event {
                    let scale = window.scale_factor();
                    let logical = Point::new(position.x / scale, position.y / scale);
                    let desired = app
                        .resolve_cursor(logical)
                        .unwrap_or(CursorIcon::Default);
                    if app.current_cursor != desired {
                        app.current_cursor = desired;
                        window.set_cursor(desired);
                    }
                    // Retarget the central hover tooltip. `on_pointer` keeps the
                    // dwell running when the pointer stays over the same region
                    // and restarts it on a new one; a redraw is requested so the
                    // reveal/clear lands promptly (the frame pump in
                    // `about_to_wait` carries the dwell across to the reveal).
                    let target = app.tooltip_target_at(logical);
                    if app.tooltip_host.on_pointer(target, Instant::now()) {
                        window.request_redraw();
                    }
                }

                // Any click, scroll, key or pointer-leave dismisses a shown or
                // pending tooltip — the user has moved on from "what is this?".
                if matches!(
                    &event,
                    WindowEvent::MouseInput { state: ElementState::Pressed, .. }
                        | WindowEvent::MouseWheel { .. }
                        | WindowEvent::KeyboardInput { .. }
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
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.as_ref().cloned() else {
            return;
        };
        let app = &mut self.app;

        // Reflow on an allocation change even when no other input arrives.
        // The event-driven loop only re-runs layout on a user event or a
        // `Resized`, so a compositor reconfigure that changes `inner_size()`
        // without delivering `Resized` (or a resize that coalesced away)
        // would leave responsive content sized to the previous frame until
        // the user happens to move the mouse. Compare against the size the
        // last frame was built at and, only when it actually differs, mark
        // layout dirty and request one redraw — the redraw refreshes
        // `last_frame_size`, so this self-coalesces to a single reflow per
        // distinct size rather than a per-tick repaint.
        if let Some(prev) = app.last_frame_size {
            if window.inner_size() != prev {
                app.layout_dirty = true;
                window.request_redraw();
            }
        }
        // Drain every pending CollabEvent into PresenceCtx /
        // state.collab via the single shared dispatch. If any event
        // came through, kick a redraw so the freshly-stamped remote
        // cursor / caret / selection appears on the next frame.
        if app.pump_collab_presence() {
            window.request_redraw();
        }

        // Fade out peer "just typed" highlighter marks: reclaim any that
        // have fully decayed, and keep pumping frames while any are still
        // fading so the taper animates instead of freezing between events.
        let typing_now = Instant::now();
        app.presence_ctx.prune_typing(typing_now);
        let typing_active = app.presence_ctx.has_active_typing(typing_now);
        if typing_active {
            window.request_redraw();
        }

        // An affordance may have dispatched a `qos_actions` action out-of-band
        // during the last event (the button-driven twin of the Ctrl+K palette).
        // Record one undo boundary bracketing it, so Ctrl+Z reverts it — the same
        // `record_committed_action` path the palette dispatch takes.
        if let Some(before) = app.root_widget.take_committed_action() {
            if app.state.doc.get_heads() != before {
                app.state.record_committed_action(before, None, None);
                app.needs_sync = true;
                window.request_redraw();
            }
        }

        if app.root_widget.needs_commit() {
            app.commit_action();
            app.root_widget.on_commit();
            app.needs_sync = true;
            window.request_redraw();
        }

        // Persist whatever the frame just changed. Level-triggered rather
        // than hooked into individual mutations: widgets write to the
        // document from many places (text splices, drag drops, collab
        // merges) and comparing heads once per idle tick catches all of
        // them without every call site having to remember.
        app.autosave_tick();

        // Reconcile the root's desired OS-fullscreen state with the window
        // (and observe compositor-driven fullscreen changes). Level-triggered
        // and idempotent — a no-op on the common frame where nothing changed.
        if app.reconcile_fullscreen(&window) {
            window.request_redraw();
        }

        // Reconcile a live global-appearance change (theme / high-contrast /
        // reduced-motion / UI-scale / text-scale) surfaced by an appearance
        // pane, so the running app re-scales/re-colours immediately. Opt-in via
        // `appearance_reconcile`; a cheap no-op otherwise. When it applies a
        // change it marks layout + structure dirty, so pump a redraw.
        if app.reconcile_appearance() {
            app.needs_sync = true;
            window.request_redraw();
        }

        // Re-broadcast location presence here too: the active page can
        // change during `sync` (e.g. the initial auto-select), which the
        // post-event broadcast above wouldn't catch. Diffed, so it only
        // sends on an actual change.
        app.broadcast_location_presence();
        // Same for the participant's View (scroll / open resource / mode):
        // `sync` can move it (e.g. the initial auto-select), so re-broadcast
        // the diffed subset here too.
        app.broadcast_view_presence();

        // Keep syncing on idle frames while a collab app is still seeding
        // or adopting the shared document. Without this a first peer with
        // no input never re-runs `sync` to seed its doc after the grace
        // window, so a late joiner receives an empty reply and diverges.
        // Stops as soon as the widget reports it's converged.
        if app.collab_manager.is_connected() && app.root_widget.needs_idle_sync() {
            app.needs_sync = true;
            window.request_redraw();
        }

        // Broadcast any local document edits (and the initial seed) as an
        // incremental delta so peers converge in near-real-time. Runs each
        // ~16ms tick while connected; a no-op when nothing changed.
        app.broadcast_doc_delta();

        // If a widget mutated its child composition between
        // frames (e.g. GroupRow split a tab into a new group
        // during the last event dispatch), flag the app so the
        // next RedrawRequested rebuilds the taffy tree before
        // laying out. We don't drain the flag here — the draw
        // handler needs to see it — just pump a redraw so the
        // rebuild happens promptly.
        if app.root_widget.take_structure_dirty() {
            app.structure_dirty = true;
            window.request_redraw();
        }

        // Pump the animation / async-work loop: the root reports it needs
        // another frame either because a widget is mid-transition (e.g. the
        // shell's overview crossfade) or because it has a background worker
        // — a network fetch, a file load — whose result it folds in on the
        // next `sync`. Give it that sync opportunity and a redraw so the
        // fresh state surfaces this frame; without the sync, a worker that
        // delivered into the CRDT/state would stay invisible until unrelated
        // input arrived. The bounded `WaitUntil` chosen at the end of this
        // method keeps waking the loop at ~60 Hz while `needs_frame` holds,
        // so results drain promptly, then it drops back to `Wait` the moment
        // the widget stops asking — no idle spin.
        //
        // Reduced motion stops the perpetual pump: widgets that respect it
        // already paint their resting state (a still spinner, a static bar),
        // so there is nothing left to animate and we let the loop go idle.
        // A hover tooltip counting toward its reveal needs frames pumped until
        // the dwell elapses. Unlike animation, a tooltip is not motion, so it
        // reveals even under reduce-motion — request a redraw independently.
        let tooltip_pending = app.tooltip_host.needs_frame();
        if tooltip_pending {
            window.request_redraw();
        }

        let needs_frame = app.root_widget.needs_animation_frame();
        if needs_frame && !app.theme.reduce_motion {
            app.needs_sync = true;
            // An animation tick may reposition content without any input event
            // (a Scrollable auto-repeating a held track-page, a momentum
            // fling), so force `apply_layout` this frame — the sync/draw pass
            // alone would leave the moved offset unreflected in the children's
            // resolved rects. The scroll fast path keeps this to a cheap
            // reposition (no `compute_layout`) unless something needs measuring.
            app.layout_dirty = true;
            window.request_redraw();
        }

        // Pump the optional shell-registered frame tick so
        // out-of-band sources (libp2p presence, palette
        // command queue) can drain into the overlay state
        // once per wakeup. We always request a redraw when
        // the tick fires because the shell typically writes
        // visible state (remote cursor positions) — without
        // the redraw a remote cursor would not animate.
        if let Some(tick) = app.frame_tick.as_mut() {
            tick(&mut app.presence_ctx);
            window.request_redraw();
        }

        // Choose the loop's idle cadence for this wakeup. The steady state —
        // nothing animating, no collab socket, no pending async work — is
        // event-driven `Wait`: the loop sleeps until real input arrives and
        // burns no CPU, so the `QUICK_UI_PERF` idle frame count stays ~0.
        // We escalate to a bounded ~60 Hz `WaitUntil` tick only while there
        // is work that no winit event will surface on its own: a live collab
        // socket whose peer edits/presence must be drained, or a root that
        // still needs another frame (animation or async worker). Dropping
        // straight back to `Wait` the instant both clear is what keeps an
        // idle app from spinning.
        if app.collab_manager.is_connected() || needs_frame || tooltip_pending || typing_active {
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(16),
            ));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

/// Framework helper: run `dispatch` only if `widget`'s permission scope (if any)
/// passes `ctx.check`. Returns true if dispatched, false if denied.
///
/// Widgets that expose a `permission_scope()` are gated at the framework level:
/// mutating events (clicks, text input, wheel, etc.) should be routed through
/// this helper so that denied interactions short-circuit cleanly without the
/// widget ever observing the event. Callers can additionally consult
/// `ctx.check_with_denied_feedback(&scope)` to surface a toast/flash on denial.
#[cfg(test)]
mod window_title_tests {
    use super::*;

    #[test]
    fn humanize_ident_titlecases_and_splits() {
        assert_eq!(humanize_ident("notes"), "Notes");
        assert_eq!(humanize_ident("file-explorer"), "File Explorer");
        assert_eq!(humanize_ident("word-processor"), "Word Processor");
        assert_eq!(humanize_ident("db_client"), "Db Client");
        // The last dot-segment case a package id feeds in.
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
        // `QOS_APP_NAME` carries the manifest `[package].name`; it is the
        // primary source once no explicit override is set.
        let t = resolve_title_from(None, Some("Notes"), Some("dev.qos.notes"), Some("notes"));
        assert_eq!(t, "Notes");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn package_id_falls_back_to_humanized_last_segment() {
        // No manifest name: the app_id's last segment, humanized.
        let t = resolve_title_from(None, None, Some("dev.qos.file-explorer"), Some("file-explorer"));
        assert_eq!(t, "File Explorer");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn binary_name_is_the_dev_run_fallback() {
        // A dev run outside the launcher: no env at all, so the binary
        // name stands in — never the old placeholder.
        let t = resolve_title_from(None, None, None, Some("terminal"));
        assert_eq!(t, "Terminal");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn blank_candidates_are_skipped_not_used() {
        // Empty / whitespace env values must not win — they fall through
        // to the next source, and never to "QuickUI App" while a real
        // one exists.
        let t = resolve_title_from(Some("  "), Some(""), Some("dev.qos.notes"), Some("notes"));
        assert_eq!(t, "Notes");
    }

    #[test]
    fn generic_default_is_qos_app_never_quickui_app() {
        // The last resort, when literally nothing identifies the app, is
        // the qOS default — the string the whole change exists to kill is
        // never produced.
        let t = resolve_title_from(None, None, None, None);
        assert_eq!(t, "qOS App");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn env_path_end_to_end_prefers_app_name() {
        // Prove the real env-reading entry point, not just the pure core:
        // with `QOS_APP_NAME` set the window title is the manifest name.
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
mod pointer_broadcast_tests {
    use super::*;

    #[test]
    fn pointer_broadcast_allowed_first_call_always_passes() {
        // With no prior broadcast, the throttle must let the first move
        // through. Otherwise a peer's first pointer event after startup
        // would drop silently.
        let now = Instant::now();
        assert!(pointer_broadcast_allowed(None, now, Duration::from_millis(33)));
    }

    #[test]
    fn pointer_broadcast_allowed_blocks_inside_gap() {
        // Two broadcasts spaced closer than the minimum gap: the second
        // must be denied so the wire rate stays capped.
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
        // A broadcast exactly at the gap boundary is allowed — strict >
        // would accumulate single-ms drift under a sawtooth wakeup schedule.
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
        // A new scope should fire immediately rather than waiting on the
        // old scope's last-send gate. Verified by observing that
        // `last_pointer_broadcast` is cleared when the scope changes.
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        app.last_pointer_broadcast = Some(Instant::now());
        app.set_presence_broadcast_scope(Some(PresenceKey::Workspace {
            id: "home".into(),
        }));
        assert!(
            app.last_pointer_broadcast.is_none(),
            "scope change must clear the throttle so the next move broadcasts immediately"
        );
        assert_eq!(
            app.presence_broadcast_scope(),
            Some(&PresenceKey::Workspace { id: "home".into() })
        );
    }

    #[test]
    fn default_scope_is_none_so_broadcast_is_opt_in() {
        // A fresh App must not broadcast pointer presence by default —
        // the headless tests and `quick-ui` examples rely on that.
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let app = App::new(state, root);
        assert!(app.presence_broadcast_scope().is_none());
    }

    #[test]
    fn setting_scope_announces_arrival() {
        // The app-side half of the late-join handshake: setting a
        // presence scope must announce `Arrived` so peers already in the
        // room reply with full state.
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
        let observed = captured.lock().unwrap();
        assert!(
            observed
                .iter()
                .any(|(k, p)| *k == key && matches!(p, PresencePayload::Arrived)),
            "setting a scope must announce Arrival, got {observed:?}"
        );
    }

    #[test]
    fn crdt_update_merges_not_replaces() {
        use automerge::transaction::Transactable;
        use automerge::{ObjType, ReadDoc, ROOT};
        // Shared base with a map "m" holding "x" — peers share this base
        // via the late-join handshake, so merge is conflict-free.
        let mut base = automerge::AutoCommit::new();
        let m = base.put_object(&ROOT, "m", ObjType::Map).unwrap();
        base.put(&m, "x", 1).unwrap();
        let base_bytes = base.save();

        // App from base; add a local edit m["a"].
        let app_state = State::load(&base_bytes).unwrap();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(app_state, root);
        {
            let doc = &mut app.state_mut_for_test().doc;
            let m = match doc.get(&ROOT, "m") {
                Ok(Some((automerge::Value::Object(ObjType::Map), id))) => id,
                _ => panic!("m exists"),
            };
            doc.put(&m, "a", 10).unwrap();
        }

        // A remote peer (same base) adds m["b"] and sends full state.
        let mut peer = automerge::AutoCommit::load(&base_bytes).unwrap();
        let pm = match peer.get(&ROOT, "m") {
            Ok(Some((automerge::Value::Object(ObjType::Map), id))) => id,
            _ => panic!("m exists"),
        };
        peer.put(&pm, "b", 20).unwrap();
        let incoming = peer.save();

        app.apply_collab_event_for_test(CollabEvent::CRDTUpdate { data: incoming });

        // Both edits must survive the merge (a replace would drop "a").
        let doc = &mut app.state_mut_for_test().doc;
        let m = match doc.get(&ROOT, "m") {
            Ok(Some((automerge::Value::Object(ObjType::Map), id))) => id,
            _ => panic!("m exists"),
        };
        assert!(doc.get(&m, "a").unwrap().is_some(), "local edit survives merge");
        assert!(doc.get(&m, "b").unwrap().is_some(), "remote edit merged in");
        assert!(doc.get(&m, "x").unwrap().is_some(), "base value present");
    }

    #[test]
    fn presence_broadcast_sink_receives_both_key_and_payload() {
        // The shell-side libp2p bridge subscribes via
        // `set_presence_broadcast_sink`. Verify that a registered sink
        // observes every (key, payload) pair passed through
        // `broadcast_presence`, with both halves of the tuple intact.
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
        // After clearing, the sink must not receive subsequent broadcasts.
        // Guards against the bridge accidentally double-publishing once
        // peer-sync is torn down.
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
        assert_eq!(*count.lock().unwrap(), 1, "sink fires before clear, never after");
    }
}

#[cfg(test)]
mod rebuild_focus_tests {
    use super::*;
    use crate::widget::{Text, TextInput};
    use automerge::{AutoCommit, ObjId};
    use vello::kurbo::Rect;

    /// A minimal container that *recreates* its children on every
    /// `build_layout` — exactly like the greeter's `build_composition` and
    /// the installer's wizard steps. The recreation is what makes caret
    /// continuity non-trivial: the focused `TextInput` is a brand-new
    /// instance after a rebuild, so its caret would reset to the new
    /// instance's default unless the App restores it via `get_state` /
    /// `set_state`.
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
            // Fresh focusable input each rebuild. `with_initial_value` parks
            // the caret at the END ("hello" -> 5), so a restored caret of 3
            // is distinguishable from both the reset-to-0 bug and the new
            // instance's own default.
            kids.push(Box::new(TextInput::local().with_initial_value("hello")));
            if self.show_status {
                // Non-focusable sibling appears — the rebuild trigger, and a
                // child the focus order must ignore.
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
            node_map.insert(node, self as *mut dyn Widget);
            node
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let layout = taffy.layout(node).unwrap();
            self.rect = Rect::new(
                origin.x + layout.location.x as f64,
                origin.y + layout.location.y as f64,
                origin.x + layout.location.x as f64 + layout.size.width as f64,
                origin.y + layout.location.y as f64 + layout.size.height as f64,
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
            self.children.iter().map(|c| c.as_ref()).collect()
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
        fn name(&self) -> &str {
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

    /// The core fix: typing into a field, then a sibling status line appears
    /// (a structural rebuild), must NOT snap the caret back to 0 — the App
    /// restores the focused widget's ephemeral state across the rebuild.
    #[test]
    fn caret_preserved_across_structure_rebuild() {
        let mut app = App::new(State::new(), Box::new(RebuildHarness::new()));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();

        // Startup auto-focus landed on the (sole) input. Park the caret at 3.
        let id = app.focused_node.expect("input focused at startup");
        unsafe { (*(*app.node_map.get(&id).unwrap())).set_state("3"); }
        assert_eq!(caret(&app).as_deref(), Some("3"));

        // A sibling status line appears -> structural rebuild (recreates the
        // input as a fresh instance whose own caret default is 5).
        let mut tmp = AutoCommit::new();
        app.root_widget
            .on_external_event("show_status", &mut tmp, &automerge::ROOT);
        app.rebuild_structure();

        // Focus stayed on the recreated input, caret restored to 3 (not 0,
        // not the new instance's end-of-text default 5).
        assert!(app.focused_node.is_some(), "focus preserved across rebuild");
        assert_eq!(
            caret(&app).as_deref(),
            Some("3"),
            "caret offset must be preserved across the rebuild"
        );
        // Prove a real rebuild happened with a new sibling, and that the
        // non-focusable sibling did not change the focusable count (which is
        // what makes the same-ordinal guard hold).
        assert_eq!(app.root_widget.children().len(), 2, "status sibling appeared");
        assert_eq!(app.focusable_order.len(), 1, "status is non-focusable");
    }

    /// The guard's other half: when the root raises `take_focus_reset` (a
    /// wizard step / auth-phase transition), focus jumps to the first
    /// focusable and the old caret is deliberately NOT pasted — the focused
    /// field changed.
    #[test]
    fn focus_reset_does_not_restore_caret() {
        let mut app = App::new(State::new(), Box::new(RebuildHarness::new()));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();

        let id = app.focused_node.unwrap();
        unsafe { (*(*app.node_map.get(&id).unwrap())).set_state("3"); }

        let mut tmp = AutoCommit::new();
        app.root_widget
            .on_external_event("reset", &mut tmp, &automerge::ROOT);
        app.rebuild_structure();

        // Reset path: focus on the first focusable, caret is the new
        // instance's own default (end of "hello" = 5), NOT the preserved 3.
        assert_eq!(
            caret(&app).as_deref(),
            Some("5"),
            "focus_reset must not paste the old caret onto the new field"
        );
    }
}

pub fn dispatch_gated_event<W: crate::widget::Widget + ?Sized>(
    ctx: &crate::permissions::PermissionContext,
    widget: &W,
    dispatch: impl FnOnce(),
) -> bool {
    if let Some(scope) = widget.permission_scope() {
        if !ctx.check(&scope) {
            return false;
        }
    }
    dispatch();
    true
}

/// App ↔ screen-reader wiring. Proves the App narrates focus changes
/// through an injected reader (over a recording speaker), toggles the reader
/// on/off, and that a reader-off App is unaffected. The focus-narration path
/// (`set_focused_node`) is private, so these tests drive it through the public
/// `init_layout` (autofocus) + the public a11y action dispatch / setter API.
#[cfg(test)]
mod screen_reader_tests {
    use super::*;
    use crate::screen_reader::ScreenReader;
    use crate::widget::{Button, Widget};
    use tts::{NullSpeaker, Speaker, Utterance};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    // A recording speaker we can read back after moving a clone into the reader.
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
    // Test-only: single-threaded, so the !Send Rc is fine.
    unsafe impl Send for SharedRecorder {}

    // A container of two focusable buttons that lays them out so directional
    // focus has somewhere to go.
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
            let node = taffy.new_with_children(taffy::Style::default(), &kids).unwrap();
            node_map.insert(node, self as *mut dyn Widget);
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
            self.kids.iter().map(|c| c.as_ref()).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.kids.iter_mut().map(|c| &mut **c as &mut (dyn Widget + '_)).collect()
        }
        fn name(&self) -> &str {
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
        // With the reader installed and autofocus on, building the layout
        // focuses the first button and the reader speaks it.
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
        // Move focus to the next focusable (the second button). set_focused_node
        // is private; drive it via the public a11y Focus action on the 2nd node,
        // which routes through set_focused_node (the same path Tab/arrow use).
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
        assert!(rec.0.borrow().stop_count() >= 1, "disabling stops in-flight speech");
        // Re-focus after disable: nothing new is spoken (reader is gone).
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
        // A plain App (no env, no setter) has no reader, so focus is silent.
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
        // handle_screen_reader_key must return false (not intercept anything)
        // when the reader is off — proving normal navigation is never stolen.
        // winit's KeyEvent has no public constructor, but the disabled-guard is
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
        // Even with the reader ON, a non-keyboard event is not a command.
        let (mut app, _rec) = app_with_reader();
        app.init_layout();
        assert!(app.screen_reader_enabled());
        let ev = WindowEvent::CursorLeft {
            device_id: unsafe { std::mem::zeroed() },
        };
        assert!(!app.handle_screen_reader_key(&ev));
    }

    // ----- Live regions -----

    /// A widget exposing one live region whose spoken value is mutable, so a
    /// test can change it between frames and prove the App announces the change.
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
            node_map.insert(node, self as *mut dyn Widget);
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
        fn name(&self) -> &str {
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

        // Seed the snapshot with the first scan, then forget what was recorded
        // so we assert purely on the *change* below.
        app.scan_live_regions();
        rec.0.borrow_mut().reset();

        // A change appears in the live region (e.g. a toast text).
        *val.borrow_mut() = "File saved".to_string();
        app.scan_live_regions();
        assert_eq!(
            rec.0.borrow().last(),
            Some("Notifications, group, File saved"),
            "the changed live text is announced (name, role, value)"
        );
        // Focus was never moved by the announcement.
        assert_eq!(app.focused_node, focus_before, "live region must not move focus");

        // Re-scanning with the SAME text announces nothing further.
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
        let last = recref.utterances().last().expect("an utterance was recorded");
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
        let last = recref.utterances().last().expect("an utterance was recorded");
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
        // Must not panic and must record nothing (no reader).
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
        // "abc" with caret parked at end (3). Snapshot, move caret left to 2,
        // then announce — the char now under the caret is 'c'.
        let (mut app, rec, id) = app_with_focused_input("abc");
        let pre = app.text_review_snapshot();
        assert!(pre.is_some(), "focused text input yields a review snapshot");
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        // Move the caret one char left (no edit).
        unsafe { (*(*app.node_map.get(&id).unwrap())).set_state("2"); }
        app.announce_text_review(pre);
        assert_eq!(rec.0.borrow().last(), Some("c"), "single-step move echoes the char");
    }

    #[test]
    fn caret_jump_to_start_announces_the_word() {
        // "hello world", caret at end. Jump to start (0): a multi-step move, so
        // the word under the caret ("hello") is announced.
        let (mut app, rec, id) = app_with_focused_input("hello world");
        let pre = app.text_review_snapshot();
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        unsafe { (*(*app.node_map.get(&id).unwrap())).set_state("0"); }
        app.announce_text_review(pre);
        assert_eq!(rec.0.borrow().last(), Some("hello"), "a long jump echoes the word");
    }

    #[test]
    fn no_caret_move_announces_nothing() {
        // Caret unchanged between snapshot and announce → silence.
        let (mut app, rec, _id) = app_with_focused_input("abc");
        let pre = app.text_review_snapshot();
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        app.announce_text_review(pre);
        assert!(rec.0.borrow().spoken().is_empty(), "no move, no announcement");
    }

    #[test]
    fn text_review_snapshot_is_none_for_non_text_widget() {
        // A button (no text_review) yields no snapshot, so review never fires.
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

/// Modal focus-trap wiring. Proves that while an open modal is in the tree the
/// App confines keyboard focus to the modal's subtree (so Tab can't reach the
/// content behind the backdrop), and restores the previously-focused element
/// when the modal closes. The modal's open flag is a state change (no
/// structural rebuild), so node ids stay stable and `refresh_focus_order` —
/// the per-frame focus pass — re-evaluates the trap.
#[cfg(test)]
mod focus_trap_tests {
    use super::*;
    use crate::widget::modal_dialog::ModalDialog;
    use crate::widget::{Button, Widget};
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::rc::Rc;

    /// A tiny container of two focusable buttons used as the modal body.
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
            let node = taffy.new_with_children(taffy::Style::default(), &kids).unwrap();
            node_map.insert(node, self as *mut dyn Widget);
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
            self.kids.iter().map(|c| c.as_ref()).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.kids.iter_mut().map(|c| &mut **c as &mut (dyn Widget + '_)).collect()
        }
        fn name(&self) -> &str {
            "two-kids"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
    }

    /// A root with two always-present outer buttons plus a modal whose body is
    /// two more buttons. A shared `want_open` cell flips the modal's state in
    /// `sync` — modelling a host that opens/closes the dialog in response to a
    /// click, without rebuilding the widget tree.
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
            // Reflect the requested open state before building so the trap query
            // sees it on the freshly-built tree.
            self.modal.set_open(self.want_open.get());
            let mut kids: Vec<_> = self
                .outer
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            kids.push(self.modal.build_layout(taffy, theme, node_map));
            let node = taffy.new_with_children(taffy::Style::default(), &kids).unwrap();
            node_map.insert(node, self as *mut dyn Widget);
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
            let mut v: Vec<&dyn Widget> = self.outer.iter().map(|c| c.as_ref()).collect();
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
        fn name(&self) -> &str {
            "modal-host"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
    }

    /// Stand up an App over the modal host and return it plus the open-flag
    /// cell. The tree is built closed; flipping the cell + calling
    /// `refresh_focus_order` models the per-frame re-evaluation.
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
        // Outer buttons plus the (closed) modal body are all reachable.
        assert!(
            app.focusable_order.len() >= 3,
            "all focusables reachable when nothing traps"
        );
    }

    #[test]
    fn open_modal_confines_focus_order_to_its_subtree() {
        let (mut app, open) = host_app();
        app.init_layout();
        let full = app.focusable_order.len();

        // Open the modal (a state flip) and re-run the per-frame focus pass.
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
        let modal_set: std::collections::HashSet<_> =
            app.focusable_order.iter().copied().collect();
        let first = app.focusable_order[0];
        let second = app.focusable_order[1];

        // The window-event loop gates Tab on `keyboard_nav_allowed`. With a
        // trap active it must be allowed EVEN while a modal captures the pointer
        // — otherwise the dialog's buttons would be unreachable by keyboard.
        assert!(
            app.keyboard_nav_allowed(Some(KurboRect::new(0.0, 0.0, 10.0, 10.0))),
            "an active focus trap allows Tab to run (scoped to the modal)"
        );

        // Drive the SAME `focus_tab` the event loop calls. Every landing must
        // stay inside the modal's two focusables and wrap last↔first — Tab can
        // never reach the content behind the scrim.
        app.set_focused_node(Some(first));
        assert!(app.focus_tab(false), "Tab moves focus");
        assert_eq!(app.focused_node, Some(second), "Tab advances within the modal");
        assert!(modal_set.contains(&second));
        assert!(app.focus_tab(false));
        assert_eq!(app.focused_node, Some(first), "Tab from the last wraps to the first");
        // Shift+Tab steps backward and is equally contained.
        assert!(app.focus_tab(true));
        assert_eq!(app.focused_node, Some(second), "Shift+Tab from the first wraps to the last");
        assert!(
            modal_set.contains(&app.focused_node.unwrap()),
            "Shift+Tab stays inside the modal subtree"
        );
    }

    #[test]
    fn pointer_capture_without_a_trap_suppresses_app_tab() {
        // A surface that captures the pointer (FloatingWindow, context menu)
        // but does NOT trap keyboard focus must switch OFF the App's own Tab /
        // arrow navigation so its own key handling wins — the click-through
        // swallow is what `modal_capture` is for, not focus containment.
        let (mut app, _open) = host_app();
        app.init_layout();
        assert!(!app.focus_trap_active);
        assert!(app.keyboard_nav_allowed(None), "with no modal, Tab runs normally");
        assert!(
            !app.keyboard_nav_allowed(Some(KurboRect::new(0.0, 0.0, 10.0, 10.0))),
            "a pointer-capturing surface with no focus trap suppresses App-level Tab"
        );
    }

    #[test]
    fn closing_modal_restores_prior_focus() {
        let (mut app, open) = host_app();
        app.init_layout();
        // Focus an outer button before opening the modal.
        let outer = app.focusable_order[0];
        app.set_focused_node(Some(outer));

        // Open: focus moves into the modal, prior focus is remembered.
        open.set(true);
        app.refresh_focus_order();
        assert!(app.focus_trap_active);
        assert_ne!(app.focused_node, Some(outer), "focus moved into the modal");

        // Close: focus returns to the outer button it left.
        open.set(false);
        app.refresh_focus_order();
        assert!(!app.focus_trap_active, "trap released when the modal closes");
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
        // The live theme starts on the plain (non-high-contrast) track.
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

        // Toggling back restores the original colours.
        let now_off = a.toggle_high_contrast();
        assert!(!now_off);
        assert!(!a.high_contrast());
        let restored = contrast_ratio(a.theme.text_primary, a.theme.bg_app);
        assert!((restored - plain_ratio).abs() < 1e-6, "off restores base colours");
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
        assert_eq!(a.text_scale(), 1.5, "scale preference is preserved across the swap");
        assert!((a.theme.tokens.typography.scale.base - hc_base * 1.5).abs() < 1e-3);
        assert_eq!(a.theme.name, "High Contrast");
    }

    #[test]
    fn reduce_motion_setter_threads_onto_the_theme() {
        let mut a = app();
        assert!(!a.theme.reduce_motion);
        a.set_reduce_motion(true);
        assert!(a.reduce_motion());
        assert!(a.theme.reduce_motion, "the flag reaches the theme widgets draw against");
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
        assert!((a.theme.tokens.spacing.s3 - base_gap * 1.5).abs() < 1e-3, "spacing grows");
        assert!((a.theme.tokens.radii.md - base_radius * 1.5).abs() < 1e-3, "radii grow");
        assert!((a.theme.tokens.typography.scale.base - base_type * 1.5).abs() < 1e-3, "type grows");
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
        assert_eq!(a.ui_scale(), 1.5, "UI-scale preference is preserved across the swap");
        assert!((a.theme.tokens.spacing.s3 - hc_gap * 1.5).abs() < 1e-2);
        assert_eq!(a.theme.name, "High Contrast");
    }

    /// A minimal root that expresses a fixed [`AppearanceIntent`], so the App's
    /// live re-theme reconcile can be exercised without a window.
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
            node_map.insert(node, self as *mut dyn Widget);
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
        ) {}
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
        ) {}
        fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
            Some(self.intent.clone())
        }
        fn name(&self) -> &str { "AppearanceRoot" }
        fn get_rect(&self) -> vello::kurbo::Rect { vello::kurbo::Rect::ZERO }
    }

    fn intent(theme: Theme, ui_scale: f32, text_scale: f32, high_contrast: bool) -> crate::theme::AppearanceIntent {
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
        let root = AppearanceRoot { intent: intent(Theme::nord(), 1.5, 1.3, true) };
        let mut a = App::new(State::new(), Box::new(root));
        // Default: appearance_reconcile is off, so the intent is ignored.
        assert!(!a.reconcile_appearance());
        assert_eq!(a.theme.name, "Dark");
        assert_eq!(a.ui_scale(), 1.0);
    }

    #[test]
    fn appearance_reconcile_applies_scale_theme_and_contrast_live() {
        let base_gap = Theme::high_contrast().tokens.spacing.s3;
        let base_type = Theme::high_contrast().tokens.typography.scale.base;
        // Nord base + high_contrast=true → high-contrast track wins; ui 1.5, text 1.2.
        let root = AppearanceRoot { intent: intent(Theme::nord(), 1.5, 1.2, true) };
        let mut a = App::new(State::new(), Box::new(root));
        a.appearance_reconcile = true;
        assert!(a.reconcile_appearance(), "first reconcile applies the intent");
        assert_eq!(a.ui_scale(), 1.5);
        assert_eq!(a.text_scale(), 1.2);
        assert!(a.high_contrast());
        assert_eq!(a.theme.name, "High Contrast", "high_contrast overrides the Nord base");
        // Spacing grew by the UI factor; type by UI × text.
        assert!((a.theme.tokens.spacing.s3 - base_gap * 1.5).abs() < 1e-1);
        assert!((a.theme.tokens.typography.scale.base - base_type * 1.5 * 1.2).abs() < 1e-1);
        assert!(a.layout_dirty, "a live appearance change re-runs layout");
        // Steady intent → second reconcile is a no-op (level-triggered).
        assert!(!a.reconcile_appearance(), "unchanged intent does not re-theme");
    }
}

#[cfg(test)]
mod fullscreen_tests {
    use super::*;
    use automerge::{AutoCommit, ObjId};
    use vello::kurbo::Rect;

    use std::cell::RefCell;
    use std::rc::Rc;

    /// A minimal root widget that exposes a settable fullscreen *intent* and
    /// records the actual-state notifications it receives (into a shared cell
    /// the test can inspect), so the App's fullscreen reconcile wiring can be
    /// exercised without a live window.
    struct FsRoot {
        want: Option<bool>,
        rect: Rect,
        changes: Rc<RefCell<Vec<bool>>>,
    }

    impl FsRoot {
        fn new(want: Option<bool>) -> (Self, Rc<RefCell<Vec<bool>>>) {
            let changes = Rc::new(RefCell::new(Vec::new()));
            (
                Self { want, rect: Rect::ZERO, changes: changes.clone() },
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
            node_map.insert(node, self as *mut dyn Widget);
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
        fn name(&self) -> &str {
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

    /// The pure decision only fires `set_fullscreen` on an actual change of
    /// the requested state — a steady desire is idempotent.
    #[test]
    fn decision_truth_table() {
        // No opinion → never touch the window.
        assert_eq!(fullscreen_request_decision(false, None), None);
        assert_eq!(fullscreen_request_decision(true, None), None);
        // Want matches what we already requested → no-op (idempotent).
        assert_eq!(fullscreen_request_decision(false, Some(false)), None);
        assert_eq!(fullscreen_request_decision(true, Some(true)), None);
        // Want differs → issue the new state exactly once.
        assert_eq!(fullscreen_request_decision(false, Some(true)), Some(true));
        assert_eq!(fullscreen_request_decision(true, Some(false)), Some(false));
    }

    /// A compositor-driven transition (fullscreen granted, then exited) reaches
    /// the root via `on_fullscreen_changed`, exactly once per edge; a repeated
    /// same-state observation is swallowed.
    #[test]
    fn actual_state_transitions_notify_root_once_per_edge() {
        let (root, changes) = FsRoot::new(None);
        let mut app = App::new(State::new(), Box::new(root));

        // Rising edge.
        assert!(app.note_actual_fullscreen(true));
        // Same state again → no notification.
        assert!(!app.note_actual_fullscreen(true));
        // Falling edge.
        assert!(app.note_actual_fullscreen(false));

        assert_eq!(*changes.borrow(), vec![true, false]);
        assert!(!app.fullscreen_active);
    }

    /// `note_actual_fullscreen` marks the app for a re-sync + relayout so the
    /// widget tree can switch to its presentation layout on the next frame.
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
    use crate::widget::Widget;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, ROOT};
    use qos_actions::{Action, ActionError, ActionSpec, Doc, Resource};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use winit::event::{ElementState, KeyEvent};
    use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

    /// A minimal replayable action: set a boolean flag under a root key.
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

    /// A searchable-content source folded into the unified palette — stands in
    /// for an app's contacts / files provider.
    struct PeopleProvider;
    impl crate::search::SearchProvider for PeopleProvider {
        fn name(&self) -> &str {
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

    /// A root widget that publishes one open resource exporting one immediate
    /// [`Flip`] action, plus a searchable People provider — i.e. exactly what an
    /// app instance offers the unified Ctrl+K (actions + findable content).
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
            node_map.insert(node, self as *mut dyn Widget);
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
        fn name(&self) -> &str {
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
            // Navigation writes the chosen id into the doc so the test can prove
            // the app was reached through the real palette path.
            doc.put(&ROOT, "navigated", ScalarValue::Str(action_id.into())).unwrap();
        }
    }

    /// Synthesise a pressed `Enter` key event. Mirrors the `mem::zeroed`
    /// workaround the palette's own tests use — winit 0.30 keeps a `KeyEvent`
    /// field `pub(crate)`, so a struct literal can't build one.
    fn synth_enter() -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Enter);
        ev.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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

    /// The full `ui::App` Ctrl+K path: the palette scopes to the focused root
    /// widget's published resources, and choosing one dispatches its action
    /// against the CRDT (honouring its locus), records it on the undo stack, and
    /// marks the app for re-sync — proving the wiring, not just the pieces.
    #[test]
    fn ctrl_k_scopes_to_resources_then_dispatch_mutates_doc_and_records_undo() {
        let mut app = App::new(State::new(), Box::new(Publisher));
        // The app registers its action decoders — the same path a remote peer
        // uses to replay a dispatched action.
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);

        // The resource-collection hook reflects what the focused widget publishes.
        let res = app.collect_resources();
        assert_eq!(res.len(), 1, "one open resource");
        assert_eq!(res[0].kind, "test.obj");

        // Opening Ctrl+K scopes the palette to those resources AND folds in the
        // widget's searchable content — one surface, both kinds.
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

        // Preconditions: flag unset, nothing recorded to undo.
        assert!(app.state.doc.get(&ROOT, "flipped").unwrap().is_none());
        assert!(app.state.peek_last_undo().is_none());
        app.needs_sync = false;
        app.layout_dirty = false;

        // Enter selects the top row (the action) and dispatches it, the real path.
        let consumed = app.action_palette_handle_key(&synth_enter());
        assert!(consumed, "the open palette consumed the Enter key");

        // The action mutated the shared CRDT...
        let flipped = app
            .state
            .doc
            .get(&ROOT, "flipped")
            .unwrap()
            .and_then(|(v, _)| v.to_bool());
        assert_eq!(flipped, Some(true), "dispatched action set the flag in the CRDT");
        // ...it is recorded as an undo point...
        assert!(
            app.state.peek_last_undo().is_some(),
            "the dispatched action is undoable"
        );
        // ...the app is marked for re-sync + relayout so the widget tree adopts it...
        assert!(app.needs_sync, "dispatch marks the app for re-sync");
        assert!(app.layout_dirty, "dispatch marks the app for relayout");
        // ...and a non-prompted selection closes + drops the palette.
        assert!(app.action_palette().is_none(), "palette closed after selection");
    }

    /// The other half of the unified surface: a **content** row (from a folded-in
    /// [`SearchProvider`](crate::search::SearchProvider)) does NOT dispatch — it
    /// hands its opaque id back through [`Widget::activate_search_result`] so the
    /// app navigates. Proves the App wiring for the second outcome channel.
    #[test]
    fn ctrl_k_content_row_navigates_through_the_app() {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.open_action_palette();
        // Narrow to a single content hit (the action row does not match "ada").
        app.action_palette.as_mut().unwrap().set_query("ada");
        let labels = app.action_palette().unwrap().result_labels();
        assert!(labels.iter().any(|l| l == "Ada Lovelace"), "content hit present: {labels:?}");
        assert!(
            !labels.iter().any(|l| l.contains("Flip it")),
            "the action row is filtered out by the query: {labels:?}"
        );

        assert!(app.state.doc.get(&ROOT, "navigated").unwrap().is_none());
        app.needs_sync = false;
        app.layout_dirty = false;

        // Enter on the content row navigates (not dispatches).
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
        assert!(app.action_palette().is_none(), "navigation closes the palette");
        assert!(app.needs_sync && app.layout_dirty, "navigation marks a redraw");
    }
}

// ---------------------------------------------------------------------------
// Freeze / thaw: the App is (de)serializable as a whole — its State document
// plus its View — through one override seam. See `crate::view` for the design.
// Kept as a small, self-contained impl block so the bulk of `app.rs` is
// untouched.
// ---------------------------------------------------------------------------
impl crate::view::FreezeThaw for App {
    /// Default state serialization for a CRDT app: `doc.save()`. `AutoCommit`'s
    /// save takes `&mut self` to flush its open transaction; freeze is `&self`
    /// and a cold path (memory eviction / reboot), so we save a clone rather
    /// than widen the signature.
    fn serialize_state(&self) -> Vec<u8> {
        self.state.doc.clone().save()
    }

    /// Default state restore for a CRDT app: load the document and adopt it as
    /// the live one (which also resets the undo history, as a document swap
    /// must — the old stacks name changes this document has never seen).
    fn restore_state(&mut self, bytes: &[u8]) -> Result<(), crate::view::ThawError> {
        let doc = automerge::AutoCommit::load(bytes)
            .map_err(|e| crate::view::ThawError::State(e.to_string()))?;
        self.state.adopt_doc(doc);
        Ok(())
    }

    /// Capture the live presentation into a [`View`](crate::view::View): the
    /// focused widget's caret/selection (through the same ephemeral
    /// `get_state` mechanism the undo path uses), the open resource, the
    /// viewport, and the root widget's scroll/view-mode contribution.
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
                width: s.width as f64,
                height: s.height as f64,
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

    /// Restore a [`View`](crate::view::View) onto the live app: hand the
    /// scroll/view-mode fields back to the root widget, then move focus to the
    /// captured ordinal and replay the focused widget's ephemeral caret via
    /// `set_state`. `open_resource` is carried for presence/reboot to reopen the
    /// record; the record-navigation wiring is the host's and is not driven
    /// here.
    fn restore_view(&mut self, view: &crate::view::View) {
        self.root_widget
            .restore_view_state(&crate::view::ViewContribution {
                scroll: view.scroll.clone(),
                view_mode: view.view_mode.clone(),
            });

        if let Some(f) = &view.focus {
            if let Some(&id) = self.focusable_order.get(f.ordinal) {
                self.set_focused_node(Some(id));
                if !f.ephemeral.is_empty() {
                    if let Some(&w) = self.node_map.get(&id) {
                        unsafe {
                            (*w).set_state(&f.ephemeral);
                        }
                    }
                }
            }
        }

        self.layout_dirty = true;
        self.needs_sync = true;
    }
}

// ---------------------------------------------------------------------------
// View presence: broadcast the local participant's View subset, render peers'
// Views, and attach/follow. Exercised on the real broadcast/receive path —
// `broadcast_view_presence` through the presence sink, and
// `apply_collab_event_for_test` through `apply_remote_presence`.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod view_presence_tests {
    use super::*;
    use crate::record::{RecordCheckout, RecordId, RecordKind};
    use crate::state::State;
    use crate::view::{FreezeThaw, ViewContribution, ViewSubset};
    use crate::widget::Widget;
    use crate::PresenceCtx;
    use crate::TextRenderer;
    use crate::Theme;
    use automerge::{AutoCommit, ObjId};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use taffy::TaffyTree;
    use vello::kurbo::{Affine, Point, Rect as KurboRect};
    use vello::Scene;
    use winit::event::WindowEvent;

    /// A root that carries exactly the presentation a [`ViewSubset`] projects:
    /// a scroll offset + view mode (via `view_state` / `restore_view_state`)
    /// and an open resource (via `record_intent`). No focus needed — the
    /// subset deliberately excludes the caret.
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
            node_map.insert(node, self as *mut dyn Widget);
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
        fn name(&self) -> &str {
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

    /// The View a participant is looking at — scroll, open resource, viewport,
    /// view mode — is broadcast through the *real* presence path and lands in
    /// a second peer's `PresenceCtx` as that peer's View, distinct from a
    /// cursor point. Asserted on the frame that actually leaves the sender
    /// (captured through the broadcast sink) and on the frame the receiver
    /// applies (through `apply_remote_presence`).
    #[test]
    fn view_subset_broadcasts_through_real_presence_path() {
        // Sender: scrolled, in "grid" mode, with a resource open, laid out to
        // a real viewport.
        let mut sender = app_with_root(ViewRoot {
            scroll: vec![10.0, 20.0],
            mode: Some("grid".into()),
            resource: Some("file:/report.md".into()),
            rect: KurboRect::ZERO,
        });
        sender.last_frame_size = Some(winit::dpi::PhysicalSize::new(1024, 768));

        // Capture what actually leaves the sender via the real broadcast sink
        // (the same seam the shell's libp2p bridge subscribes to).
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
        assert_eq!(sent.view_mode.as_deref(), Some("grid"), "view mode rode the frame");
        assert_eq!(sent.viewport.width, 1024.0, "viewport width rode the frame");
        assert_eq!(sent.viewport.height, 768.0, "viewport height rode the frame");

        // A second, unrelated participant applies that exact frame through the
        // real receive path and now knows where the sender is looking.
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
        assert_eq!(peer_view, &sent, "the receiver stores the peer's exact View subset");

        // The inert carrier payload must NOT have been processed as a real
        // Arrived — no phantom cursor, no presence registration under the
        // View key.
        assert!(
            receiver.presence_ctx().remote_cursors.is_empty(),
            "a View frame must not register a phantom cursor"
        );
    }

    /// Delivering a peer's View to the receiver, then `follow`ing them, moves
    /// the local viewport to match their broadcast View; a later View from the
    /// followed peer keeps tracking; `unfollow` releases so subsequent Views
    /// no longer move the local view.
    #[test]
    fn follow_binds_local_viewport_and_unfollow_releases() {
        let mut app = app_with_root(ViewRoot::new());

        // Helper: deliver a peer View through the real receive path.
        fn deliver(app: &mut App, peer: &str, scroll: Vec<f64>, mode: &str) {
            let subset = ViewSubset {
                scroll,
                open_resource: None,
                viewport: Default::default(),
                view_mode: Some(mode.into()),
            };
            app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
                user_id: peer.into(),
                key: PresenceKey::view(&subset),
                payload: PresencePayload::Arrived,
            });
        }

        // Peer bob is looking at scroll 100 / "list". Before following, the
        // local view has not moved.
        deliver(&mut app, "bob", vec![100.0], "list");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            Vec::<f64>::new(),
            "receiving a peer's View must not move the local view on its own"
        );

        // Follow bob: the local viewport binds to his View.
        app.follow("bob");
        let v = <App as FreezeThaw>::capture_view(&app);
        assert_eq!(v.scroll, vec![100.0], "follow moved the local view to the peer's scroll");
        assert_eq!(v.view_mode.as_deref(), Some("list"), "follow adopted the peer's view mode");

        // Bob scrolls on; while followed, the local view keeps tracking.
        deliver(&mut app, "bob", vec![250.0], "board");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            vec![250.0],
            "a followed peer's later View keeps moving the local view"
        );

        // Unfollow releases: a further View from bob no longer moves us.
        app.unfollow();
        deliver(&mut app, "bob", vec![999.0], "grid");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            vec![250.0],
            "after unfollow the local view is released and stops tracking"
        );
    }

    /// Following a peer who is not present (has broadcast no View) latches the
    /// intent but moves nothing — no panic, no spurious scroll.
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
        assert_eq!(v.scroll, vec![7.0], "following an absent peer leaves the local view untouched");
        assert_eq!(app.followed_peer(), Some("ghost"), "the follow intent is still latched");
    }
}

/// The retained semantic tree, driven through the full `ui::App`: refreshing it,
/// reading node identity/state, and invoking a node's affordance so it dispatches
/// the *same* `EncodedAction` the on-screen control does.
#[cfg(test)]
mod semantic_tree_app_tests {
    use super::*;
    use crate::a11y_tree::{SemanticActionKind, SemanticId};
    use crate::state::State;
    use crate::widget::{Button, Widget};
    use automerge::transaction::Transactable;
    use automerge::{ReadDoc, ScalarValue, Value, ROOT};
    use qos_actions::{
        Action as QAction, ActionError, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    };
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    /// A minimal real action: bump a counter, or set a label. Deterministic —
    /// the executor's contract. Registered under [`COUNTER_TYPE`].
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
                    doc.put(&ROOT, "n", ScalarValue::Int(n + 1))?;
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

    /// A leaf widget that publishes one open resource ("Counter") exporting an
    /// immediate `bump` and a prompted `set-label` — the on-screen control's
    /// resource+action model, exactly as a real app does.
    struct CounterCard {
        button: Button,
    }
    impl CounterCard {
        fn new() -> Self {
            Self { button: Button::new("bump", "Bump", None) }
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
            let node = taffy.new_with_children(taffy::Style::default(), &[kid]).unwrap();
            node_map.insert(node, self as *mut dyn Widget);
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
        fn name(&self) -> &str {
            "CounterCard"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<Resource> {
            vec![Resource::new("test.counter", "test.counter", "Counter")
                .with_action(
                    ActionSpec::immediate("bump", "Bump", COUNTER_TYPE, &CounterAction::Bump)
                        .unwrap(),
                )
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
                ))]
        }
    }

    fn app() -> App {
        let mut app = App::new(State::new(), Box::new(CounterCard::new()));
        app.action_registry_mut().register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        app
    }

    fn counter(app: &App) -> i64 {
        match app.state.doc.get(&ROOT, "n") {
            Ok(Some((v, _))) => v.to_i64().unwrap_or(0),
            _ => 0,
        }
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
        assert!(node.action("bump").is_some(), "the resource's action is an affordance");
        assert!(node.action("set-label").is_some());
        // The root owns it (resources hang off the window root).
        let root = tree.node(tree.root()).unwrap();
        assert!(root.children.contains(&SemanticId("r:test.counter".into())));
    }

    #[test]
    fn identity_is_stable_across_refreshes() {
        let mut app = app();
        app.refresh_semantic_tree();
        let ids_before: Vec<SemanticId> = app.semantic_tree().iter().map(|n| n.id.clone()).collect();
        // A refresh with nothing changed yields no delta and identical ids.
        let delta = app.refresh_semantic_tree();
        assert!(delta.is_empty(), "a stable frame yields no delta, got {delta:?}");
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

        // The on-screen control dispatches through ui::ActionHost — the exact
        // path a real app's affordance takes. It must build the byte-identical
        // EncodedAction the semantic node exposes.
        let mut host = crate::action_host::ActionHost::new();
        let mut host_doc = automerge::AutoCommit::new();
        host.dispatch(&mut host_doc, COUNTER_TYPE, &CounterAction::Bump);
        let from_affordance = host.last_dispatched().expect("the affordance dispatched").clone();

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

        // Invoke by name — the operate-the-tree path an AT / agent uses.
        assert!(app.invoke_semantic_action(&id, "bump", None).unwrap());
        assert_eq!(counter(&app), 1, "the invoked action mutated the shared doc");
        // Each invocation dispatches the real action again.
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
        assert!(app.invoke_semantic_action(&id, "set-label", Some("hello")).unwrap());
        assert_eq!(label(&app), "hello", "the prompted argument reached the built action");
    }

    #[test]
    fn invoke_matches_a_direct_affordance_dispatch_byte_for_byte() {
        // Two docs from the same start: one driven by the semantic-tree invoke,
        // one by a direct ActionHost affordance dispatch. They must converge to
        // the identical value — proving the a11y path and the UI path are one.
        let mut app = app();
        app.refresh_semantic_tree();
        app.invoke_semantic_action(&SemanticId("r:test.counter".into()), "bump", None).unwrap();

        let mut host = crate::action_host::ActionHost::new();
        let mut host_doc = automerge::AutoCommit::new();
        host.dispatch(&mut host_doc, COUNTER_TYPE, &CounterAction::Bump);

        let via_a11y = counter(&app);
        let via_affordance = match host_doc.get(&ROOT, "n") {
            Ok(Some((v, _))) => v.to_i64().unwrap_or(0),
            _ => 0,
        };
        assert_eq!(via_a11y, via_affordance, "both paths produce the same result");
    }
}

#[cfg(test)]
mod keynav_focus_tests {
    //! End-to-end proof of the shared keyboard-navigation contract on the real
    //! `App` paths: Tab/Shift+Tab cycle + wrap the focus order, arrow keys move
    //! focus directionally, Enter/Space activate a focused control (via the
    //! synth-click fallback), and the shared focus ring is painted for a
    //! focusable that draws none of its own — and suppressed for one that does.
    use super::*;
    use std::collections::HashMap;
    use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value};
    use automerge::transaction::Transactable;
    use vello_encoding::DrawTag;

    const ROW_W: f32 = 300.0;
    const ROW_H: f32 = 44.0;

    /// An app-style custom clickable leaf — the pattern a system app writes for
    /// a list row / card. It is `is_focusable`, reports a real `get_rect`, wires
    /// up only *mouse* handling (writes a CRDT key on a release while hovered,
    /// exactly like the settings `SidebarRow`), and implements **no**
    /// `perform_action`. When `self_ring` is false it also paints no focus ring
    /// and leaves `is_focused` at its default `false`, so the host's shared ring
    /// and synth-click activation are what make it keyboard-operable — with zero
    /// extra per-app code. When `self_ring` is true it tracks + reports its own
    /// focus, standing in for a widget that renders its own indicator.
    struct ClickRow {
        key: &'static str,
        rect: KurboRect,
        hovered: bool,
        pressed: bool,
        self_ring: bool,
        focused: bool,
    }
    impl ClickRow {
        fn new(key: &'static str) -> Self {
            Self { key, rect: KurboRect::ZERO, hovered: false, pressed: false, self_ring: false, focused: false }
        }
        fn self_ringing(key: &'static str) -> Self {
            Self { self_ring: true, ..Self::new(key) }
        }
    }
    impl Widget for ClickRow {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
            let node = taffy
                .new_leaf(taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(ROW_W),
                        height: taffy::Dimension::length(ROW_H),
                    },
                    ..Default::default()
                })
                .unwrap();
            node_map.insert(node, self as *mut dyn Widget);
            node
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).unwrap();
            self.rect = KurboRect::new(
                origin.x + l.location.x as f64,
                origin.y + l.location.y as f64,
                origin.x + l.location.x as f64 + l.size.width as f64,
                origin.y + l.location.y as f64 + l.size.height as f64,
            );
        }
        fn draw(&mut self, _s: &mut Scene, _t: Affine, _tr: &mut TextRenderer, _th: &Theme, _p: &PresenceCtx) {}
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            scale_factor: f64,
            _m: &winit::event::Modifiers,
            state: &mut AutoCommit,
            root: &ObjId,
            handled: &mut bool,
            _ft: &mut Option<NodeId>,
            _n: NodeId,
            _t: &TaffyTree<()>,
        ) {
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
                    self.hovered = self.rect.contains(logical);
                }
                WindowEvent::MouseInput { state: es, button: MouseButton::Left, .. } => {
                    if self.hovered {
                        let pressed = *es == ElementState::Pressed;
                        if pressed {
                            self.pressed = true;
                            *handled = true;
                        } else if self.pressed {
                            self.pressed = false;
                            *handled = true;
                            let _ = state.put(root, self.key, true);
                        }
                    }
                }
                _ => {}
            }
        }
        fn name(&self) -> &str { "click-row" }
        fn get_rect(&self) -> KurboRect { self.rect }
        fn is_focusable(&self) -> bool { true }
        fn set_focus(&mut self, f: bool) { self.focused = f; }
        fn is_focused(&self) -> bool { self.self_ring && self.focused }
    }

    /// A vertical stack of focusable rows with a real flex layout, so the focus
    /// order is well-defined and directional navigation has somewhere to go.
    struct Board { kids: Vec<Box<dyn Widget>> }
    impl Board {
        fn new(kids: Vec<Box<dyn Widget>>) -> Self { Self { kids } }
    }
    impl Widget for Board {
        fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) {
            for c in &mut self.kids { c.sync(s, p); }
        }
        fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
            let kids: Vec<_> = self.kids.iter_mut().map(|c| c.build_layout(taffy, theme, node_map)).collect();
            let node = taffy
                .new_with_children(
                    taffy::Style {
                        display: taffy::Display::Flex,
                        flex_direction: taffy::FlexDirection::Column,
                        align_items: Some(taffy::AlignItems::FlexStart),
                        ..Default::default()
                    },
                    &kids,
                )
                .unwrap();
            node_map.insert(node, self as *mut dyn Widget);
            node
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).unwrap();
            let new_origin = Point::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64);
            let children = taffy.children(node).unwrap();
            for (child, cnode) in self.kids.iter_mut().zip(children.iter()) {
                child.apply_layout(taffy, *cnode, new_origin);
            }
        }
        fn draw(&mut self, s: &mut Scene, t: Affine, tr: &mut TextRenderer, th: &Theme, p: &PresenceCtx) {
            for c in &mut self.kids { c.draw(s, t, tr, th, p); }
        }
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            scale_factor: f64,
            modifiers: &winit::event::Modifiers,
            state: &mut AutoCommit,
            path: &ObjId,
            handled: &mut bool,
            focus_target: &mut Option<NodeId>,
            node_id: NodeId,
            taffy: &TaffyTree<()>,
        ) {
            let child_nodes = taffy.children(node_id).unwrap();
            for (child, &cnode) in self.kids.iter_mut().zip(child_nodes.iter()) {
                child.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, cnode, taffy);
                if *handled { break; }
            }
        }
        fn children(&self) -> Vec<&dyn Widget> { self.kids.iter().map(|c| c.as_ref()).collect() }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.kids.iter_mut().map(|c| &mut **c as &mut (dyn Widget + '_)).collect()
        }
        fn name(&self) -> &str { "board" }
        fn get_rect(&self) -> KurboRect { KurboRect::ZERO }
    }

    fn app_with_rows(kids: Vec<Box<dyn Widget>>) -> App {
        let mut app = App::new(State::new(), Box::new(Board::new(kids)));
        app.init_layout();
        app.layout_for_test(800.0, 600.0);
        app
    }

    /// Count the solid-colour draw ops the shared focus-ring pass emits — one
    /// stroke per painted ring. Asserts on the production `compose_focus_ring`.
    fn ring_color_ops(app: &App) -> usize {
        let mut scene = Scene::new();
        app.compose_focus_ring(&mut scene, Affine::IDENTITY);
        scene.encoding().draw_tags.iter().filter(|t| **t == DrawTag::COLOR).count()
    }

    fn crdt_flag(app: &App, key: &str) -> bool {
        match app.state.doc.get(&app.state.root, key) {
            Ok(Some((Value::Scalar(s), _))) => matches!(s.as_ref(), ScalarValue::Boolean(true)),
            _ => false,
        }
    }

    #[test]
    fn tab_advances_through_focusable_order_and_wraps() {
        let mut app = app_with_rows(vec![
            Box::new(ClickRow::new("a")),
            Box::new(ClickRow::new("b")),
            Box::new(ClickRow::new("c")),
        ]);
        assert_eq!(app.focusable_order.len(), 3, "all three rows are focusable");
        app.set_focused_node(Some(app.focusable_order[0]));

        assert!(app.focus_tab(false));
        assert_eq!(app.focused_node, Some(app.focusable_order[1]), "Tab -> 2nd");
        assert!(app.focus_tab(false));
        assert_eq!(app.focused_node, Some(app.focusable_order[2]), "Tab -> 3rd");
        assert!(app.focus_tab(false));
        assert_eq!(app.focused_node, Some(app.focusable_order[0]), "Tab wraps last -> first");
    }

    #[test]
    fn shift_tab_retreats_through_focusable_order_and_wraps() {
        let mut app = app_with_rows(vec![
            Box::new(ClickRow::new("a")),
            Box::new(ClickRow::new("b")),
            Box::new(ClickRow::new("c")),
        ]);
        app.set_focused_node(Some(app.focusable_order[0]));

        assert!(app.focus_tab(true));
        assert_eq!(app.focused_node, Some(app.focusable_order[2]), "Shift+Tab wraps first -> last");
        assert!(app.focus_tab(true));
        assert_eq!(app.focused_node, Some(app.focusable_order[1]), "Shift+Tab -> 2nd");
        assert!(app.focus_tab(true));
        assert_eq!(app.focused_node, Some(app.focusable_order[0]), "Shift+Tab -> 1st");
    }

    #[test]
    fn arrow_keys_move_focus_directionally() {
        let mut app = app_with_rows(vec![
            Box::new(ClickRow::new("a")),
            Box::new(ClickRow::new("b")),
            Box::new(ClickRow::new("c")),
        ]);
        app.set_focused_node(Some(app.focusable_order[0]));

        app.navigate_focus(Direction::Down);
        assert_eq!(app.focused_node, Some(app.focusable_order[1]), "Down -> next row below");
        app.navigate_focus(Direction::Down);
        assert_eq!(app.focused_node, Some(app.focusable_order[2]), "Down -> third row");
        app.navigate_focus(Direction::Up);
        assert_eq!(app.focused_node, Some(app.focusable_order[1]), "Up -> row above");
    }

    #[test]
    fn shared_focus_ring_drawn_for_focusable_without_own_ring() {
        let mut app = app_with_rows(vec![Box::new(ClickRow::new("a")), Box::new(ClickRow::new("b"))]);
        app.set_focused_node(Some(app.focusable_order[0]));
        assert_eq!(
            ring_color_ops(&app),
            1,
            "a focusable that paints no ring of its own gets exactly one shared focus ring",
        );
    }

    #[test]
    fn shared_focus_ring_suppressed_when_widget_draws_its_own() {
        let mut app = app_with_rows(vec![Box::new(ClickRow::self_ringing("a"))]);
        app.set_focused_node(Some(app.focusable_order[0]));
        assert!(
            unsafe { (*(*app.node_map.get(&app.focused_node.unwrap()).unwrap())).draws_own_focus_ring() },
            "the self-ring row reports it draws its own indicator while focused",
        );
        assert_eq!(
            ring_color_ops(&app),
            0,
            "the host must not double-ring a widget that draws its own focus indicator",
        );
    }

    #[test]
    fn no_shared_focus_ring_when_nothing_is_focused() {
        let app = app_with_rows(vec![Box::new(ClickRow::new("a"))]);
        assert_eq!(app.focused_node, None, "no autofocus in this fixture");
        assert_eq!(ring_color_ops(&app), 0, "no focus -> no ring");
    }

    #[test]
    fn enter_activates_custom_clickable_via_synthesized_click() {
        let mut app = app_with_rows(vec![
            Box::new(ClickRow::new("a")),
            Box::new(ClickRow::new("b")),
        ]);
        // Focus the second row and take the exact steps the Enter/Space handler
        // does: the widget declines `Activate` (default `perform_action`), so
        // the host synthesises a click at its centre.
        let fid = app.focusable_order[1];
        app.set_focused_node(Some(fid));
        let (declined, center) = {
            let w = *app.node_map.get(&fid).unwrap();
            let declined = !unsafe {
                (*w).perform_action(&crate::a11y::Action::Activate, &mut app.state.doc, &app.state.root)
            };
            (declined, unsafe { (*w).get_rect().center() })
        };
        assert!(declined, "the custom row has no perform_action, so Activate falls through");

        let consumed = app.synth_activation_click(center, 1.0);
        assert!(consumed, "the synthesised click reaches the focused row's mouse handler");
        assert!(crdt_flag(&app, "b"), "Enter activated the focused row (2nd), writing its key");
        assert!(!crdt_flag(&app, "a"), "activation did not leak to the unfocused row");
    }
}
