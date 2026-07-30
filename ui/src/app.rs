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
            presence_broadcast_sink: None,
            frame_tick: None,
            current_cursor: CursorIcon::Default,
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
            action_palette: None,
            action_registry: qos_actions::ActionRegistry::new(),
            action_transport: Box::new(qos_actions::LocalTransport::new()),
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
        self.state.commit_action_authored(
            self.last_stable_focus_idx,
            self.last_stable_metadata.clone(),
            Some(self.local_author.clone()),
        );
        self.last_stable_focus_idx = self.focused_node.and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.last_stable_metadata = self.focused_node.and_then(|id| {
            self.node_map.get(&id).and_then(|&w_ptr| unsafe { (*w_ptr).get_state() })
        });

        // Broadcast CRDT state
        let data = self.state.save();
        self.collab_manager.send(CollabRequest::SyncCRDT { data });
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

    /// Offer the current document to the background writer.
    ///
    /// Called once per idle tick. Cheap when nothing changed: comparing two
    /// head vectors, no serialisation and no I/O.
    fn autosave_tick(&mut self) {
        if let Some(saver) = self.autosave.as_mut() {
            saver.request(&mut self.state.doc);
        }
    }

    /// Write out anything still pending and wait for it to land. Called on
    /// event-loop teardown so a `kill` right after the last keystroke still
    /// finds it on disk.
    fn autosave_flush(&mut self) {
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
        // The AccessKit adapter must be created while the window is still
        // hidden (it asserts this), so build the window invisible, create the
        // adapter, then show it.
        let mut window_attributes = Window::default_attributes()
            .with_title("QuickUI App")
            // Draw a normal, draggable title bar (winit's sctk-adwaita CSD)
            // on compositors that don't provide server-side decorations —
            // e.g. GNOME/mutter. Explicit even though it's the winit
            // default, so the intent is unmistakable.
            .with_decorations(true)
            .with_visible(false)
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        // Give the xdg-toplevel a real app_id. Without one the app_id is
        // the empty string, and mutter keys window identity, focus /
        // xdg-activation, and its decoration policy off the app_id — an
        // empty id is the concrete difference from every GTK / other
        // Wayland app (all of which come up focused + decorated). Wayland
        // only; a no-op on other winit backends.
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            window_attributes = window_attributes.with_name("org.qos.app", "");
        }
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());

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

                if app.needs_sync {
                    app.root_widget.sync(&mut app.state.doc, &app.state.root);
                    app.needs_sync = false;
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
                if app.structure_dirty {
                    // Rebuild the taffy tree, re-establishing keyboard focus
                    // AND (for `autofocus_on_structure_change` apps) the
                    // focused widget's caret/selection across the rebuild.
                    app.rebuild_structure();
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
                    }
                    let t1 = std::time::Instant::now();
                    app.root_widget.apply_layout(
                        &app.taffy,
                        root_node,
                        vello::kurbo::Point::new(0.0, 0.0),
                    );
                    t_apply_us = t1.elapsed().as_micros();
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

                // Modal focus-trap: when the tree hosts an open modal
                // overlay it reports its box rect here. While set, the App
                // must NOT run its own global keyboard shortcuts (Tab focus
                // cycling, Alt/arrow directional focus) — the modal owns the
                // keyboard — and an unhandled pointer press is swallowed so
                // clicks can't fall through to widgets behind the modal.
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

                if !handled && modal_rect.is_none() {
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
                        if !app.focusable_order.is_empty() {
                            let current_idx = app.focused_node.and_then(|id| {
                                app.focusable_order.iter().position(|&x| x == id)
                            });
                            let next_idx = match current_idx {
                                Some(idx) => {
                                    if shift {
                                        if idx == 0 {
                                            app.focusable_order.len() - 1
                                        } else {
                                            idx - 1
                                        }
                                    } else {
                                        (idx + 1) % app.focusable_order.len()
                                    }
                                }
                                None => 0,
                            };
                            app.set_focused_node(Some(app.focusable_order[next_idx]));
                            handled = true;
                        }
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
                            (dir, app.modifiers.state().alt_key(), modal_rect.is_none())
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

                if !handled && modal_rect.is_none() {
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
        if app.collab_manager.is_connected() || needs_frame || typing_active {
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
    fn tab_wraps_within_the_trapped_modal() {
        let (mut app, open) = host_app();
        app.init_layout();
        open.set(true);
        app.refresh_focus_order();
        assert_eq!(app.focusable_order.len(), 2);

        let first = app.focusable_order[0];
        let second = app.focusable_order[1];
        app.set_focused_node(Some(first));
        // Emulate the Tab cycle the window-event handler performs over the
        // (now-restricted) focusable_order.
        let next = (app.focusable_order.iter().position(|&x| x == first).unwrap() + 1)
            % app.focusable_order.len();
        app.set_focused_node(Some(app.focusable_order[next]));
        assert_eq!(app.focused_node, Some(second));
        let wrap = (app.focusable_order.iter().position(|&x| x == second).unwrap() + 1)
            % app.focusable_order.len();
        app.set_focused_node(Some(app.focusable_order[wrap]));
        assert_eq!(app.focused_node, Some(first), "Tab from last wraps to first inside the modal");
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
