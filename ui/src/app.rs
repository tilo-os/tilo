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
use std::sync::Arc;
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

/// `None` when the caret is in whitespace / at a gap with no adjacent word.
fn word_at(s: &str, byte: usize) -> Option<String> {
    let b = byte.min(s.len());
    // On a boundary (between words or at the end), prefer the word to the
    // left so Home/End reads a real word.
    let is_ws = |c: char| c.is_whitespace();
    let on_word = s[b..].chars().next().is_some_and(|c| !is_ws(c));
    let anchor = if on_word {
        b
    } else {
        let left = prev_boundary(s, b);
        if s[left..].chars().next().is_some_and(|c| !is_ws(c)) {
            left
        } else {
            return None;
        }
    };
    let mut start = anchor;
    while start > 0 {
        let prev = prev_boundary(s, start);
        if s[prev..].chars().next().is_some_and(is_ws) {
            break;
        }
        start = prev;
    }
    let mut end = anchor;
    while end < s.len() {
        let next = next_boundary(s, end);
        if s[end..].chars().next().is_some_and(is_ws) {
            break;
        }
        end = next;
    }
    let word = &s[start..end];
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
    modifiers: winit::event::Modifiers,

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
    /// The undo status last handed to the widget tree, and the history
    /// revision it was derived from.
    ///
    /// Cached rather than recomputed per frame because deriving the summaries
    /// reads `AutoCommit::diff`, which **closes the open transaction** — doing
    /// that mid-typing would commit a half-typed splice out from under the
    /// pending `commit_action`, and the undo entry that action was about to
    /// push would silently not exist. Keyed on the revision, which changes only
    /// when a stack does, so the doc is touched only at moments the host has
    /// just closed the transaction itself.
    undo_status: crate::undo::UndoStatus,
    undo_status_rev: Option<u64>,
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
    semantic_tree: crate::a11y_tree::SemanticTree,

    profiler: Option<crate::profiling::FrameProfiler>,

    /// Wins over the automatic env/package-id/binary-name resolution.
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
            // Finalised below, once base_theme/a11y/text_scale all exist.
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
            undo_status: crate::undo::UndoStatus::empty(),
            undo_status_rev: None,
            presence_ctx: {
                // Own identity, in preference order: the handle announced on
                // `Hello`, then the fallback app id. Anything else is
                // somebody else and gets painted.
                let mut ctx = PresenceCtx::empty();
                ctx.set_own_identity(
                    crate::collaboration::presence_handle_env()
                        .into_iter()
                        .chain(std::iter::once(crate::collaboration::local_user_id().0)),
                );
                ctx
            },
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

    /// Lets the hosted app register its action types, e.g.
    /// `app.action_registry_mut().register::<TubeAction>("tube.action")`.
    pub fn action_registry_mut(&mut self) -> &mut qos_actions::ActionRegistry {
        &mut self.action_registry
    }

    pub fn set_action_transport(&mut self, transport: Box<dyn qos_actions::ActionTransport>) {
        self.action_transport = transport;
    }

    pub fn collect_resources(&self) -> Vec<qos_actions::Resource> {
        self.root_widget.resources(&self.state.doc)
    }

    pub fn open_action_palette(&mut self) {
        let resources = self.collect_resources();
        let providers = self.root_widget.search_providers();
        let mut palette = crate::action_palette::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.set_providers(providers);
        palette.open();
        self.action_palette = Some(palette);
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
        // An action row may have fired: drain + dispatch through qos_actions.
        let dispatched = self.action_palette.as_mut().and_then(|p| {
            p.take_dispatch(
                &mut self.state.doc,
                &self.action_registry,
                self.action_transport.as_mut(),
            )
        });
        if let Some(result) = dispatched {
            match result {
                Ok(d) if d.mutated_local() => {
                    self.record_action_boundary(before);
                    self.needs_sync = true;
                    self.layout_dirty = true;
                }
                Ok(_) => {} // submitted to host; no local mutation
                Err(e) => tracing::warn!(error = %e, "action palette dispatch failed"),
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
                self.record_action_boundary(nav_before);
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
        if self.screen_reader.is_none() {
            return false;
        }
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        use winit::event::ElementState;
        use winit::keyboard::{Key, NamedKey};

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
        self.presence_broadcast_scope = key.clone();
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
                .send(crate::collaboration::CollabRequest::SyncIncremental { data: delta });
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

    /// Shared by the per-frame tick and the headless test path. Returns
    /// `true` iff at least one event was processed.
    pub fn pump_collab_presence(&mut self) -> bool {
        let events = self.collab_manager.poll();
        let mut any = false;
        for event in events {
            any = true;
            match event {
                CollabEvent::Context { collaborators, .. } => {
                    // The self row carries the address others use for this
                    // participant (so a locally-derived id is recognised in
                    // a room-keyed roster) and the host-assigned name; it
                    // never enters the roster itself.
                    if let Some(me) = collaborators.iter().find(|c| c.is_self) {
                        self.presence_ctx.set_self_participant(
                            crate::permissions::UserId(me.id.clone()),
                            me.name.clone(),
                        );
                    }
                    // Filtered to others: letting the self row through would
                    // put the local user in every list built from presence.
                    for c in collaborators.into_iter().filter(|c| !c.is_self) {
                        // Relabel only — the roster doesn't say where a peer's
                        // pointer is, so it must not mint one at the origin.
                        if let Some(cursor) = self.state.collab.cursors.get_mut(&c.id) {
                            cursor.name = c.name.clone();
                        }
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
                    // Every app in the room is handed every action; the one
                    // whose registry knows the type applies it, so the owner
                    // stays the only writer. `before` MUST be read ahead of
                    // apply: a baseline left behind the incoming change means
                    // the user's next Ctrl+Z reverts someone else's edit along
                    // with their own keystroke.
                    let before = self.state.doc.get_heads();
                    let applied =
                        self.root_widget
                            .apply_room_action(&mut self.state.doc, &type_id, &payload);
                    if applied {
                        // Named after the publisher so local Ctrl+Z walks
                        // past it instead of through it.
                        self.state.record_committed_action_authored(
                            before,
                            None,
                            None,
                            Some(crate::permissions::UserId(from_user_id.clone())),
                        );
                        self.needs_sync = true;
                        self.structure_dirty = true;
                    }
                }
                CollabEvent::CRDTUpdate { data } => {
                    // MERGE rather than replace — replacing clobbers our own
                    // concurrent edits.
                    if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                        crate::diag!("[app] recv CRDTUpdate {}B", data.len());
                    }
                    if let Ok(mut incoming) = State::load(&data) {
                        let _ = self.state.doc.merge(&mut incoming.doc);
                        self.needs_sync = true;
                        // A remote change can grow the widget tree, so force
                        // a relayout — else a stale layout indexes out of
                        // bounds.
                        self.layout_dirty = true;
                    }
                }
                CollabEvent::CRDTIncremental { data } => {
                    if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                        crate::diag!("[app] recv CRDTIncremental {}B", data.len());
                    }
                    let _ = self.state.load_incremental(&data);
                    self.needs_sync = true;
                    self.layout_dirty = true;
                }
                CollabEvent::SignedCRDTIncremental { envelope, .. } => {
                    // The broker already validated the envelope (signature +
                    // role table + replay defence); the toolkit trusts it.
                    if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                        crate::diag!(
                            "[app] recv SignedCRDTIncremental from {} ({}B payload)",
                            "<peer>",
                            envelope.payload.len()
                        );
                    }
                    let _ = self.state.load_incremental(&envelope.payload);
                    self.needs_sync = true;
                    self.layout_dirty = true;
                }
                CollabEvent::ScrollbackBatch {
                    artifact: _,
                    ops,
                    exhausted: _,
                } => {
                    // Broker pre-validated each envelope, trusted the same
                    // way as `SignedCRDTIncremental`.
                    if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                        crate::diag!("[app] recv ScrollbackBatch ({} ops)", ops.len());
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
            CollabEvent::CRDTIncremental { data } => {
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
                let _ = self.state.load_incremental(&envelope.payload);
                self.needs_sync = true;
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
        self.focusable_order.clear();
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
                    .send(crate::collaboration::CollabRequest::SyncCRDT { data });
            }
        }
    }

    /// Undo the local user's most recent action. **The one undo door.**
    ///
    /// Ctrl+Z calls this, and so does a widget that asked for an undo through
    /// [`Widget::take_undo_request`](crate::widget::Widget::take_undo_request).
    /// Keeping the shortcut and every on-screen control on one method is what
    /// stops them drifting: an Undo button cannot pick a different author
    /// scope, forget to restore the caret, or skip the redo bookkeeping,
    /// because it does not reimplement any of it.
    ///
    /// Returns `true` iff something was actually undone.
    pub fn perform_undo(&mut self) -> bool {
        let (focus_idx, meta) = self.undo_focus_snapshot();
        let Some(entry) = self
            .state
            .undo_as(Some(&self.local_author), focus_idx, meta)
        else {
            return false;
        };
        self.apply_undo_entry(entry);
        self.refresh_undo_status();
        true
    }

    /// Redo the local user's most recently undone action — the mirror of
    /// [`perform_undo`](Self::perform_undo), and equally the only redo door.
    pub fn perform_redo(&mut self) -> bool {
        let (focus_idx, meta) = self.undo_focus_snapshot();
        let Some(entry) = self
            .state
            .redo_as(Some(&self.local_author), focus_idx, meta)
        else {
            return false;
        };
        self.apply_undo_entry(entry);
        self.refresh_undo_status();
        true
    }

    /// Where the caret is *right now*, for the opposite stack's entry.
    ///
    /// Captured before undo/redo runs so round-tripping restores the caret
    /// rather than rehydrating the text at offset 0. `get_state` is the
    /// non-mutating read half of the get/set pair, so widgets that don't opt
    /// in are safe.
    fn undo_focus_snapshot(&self) -> (Option<usize>, Option<String>) {
        let meta = self.root_widget.get_state();
        let idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|x| *x == id));
        (idx, meta)
    }

    /// What the history can do for the local user right now.
    ///
    /// The same value the widget tree is given, so a test can assert that a
    /// control's disabled state and the host agree without reaching into the
    /// stacks.
    pub fn undo_status(&self) -> &crate::undo::UndoStatus {
        &self.undo_status
    }

    /// Recompute the cached status if — and only if — the history moved.
    ///
    /// Cheap and safe to call whenever the host has just finished writing to
    /// the document: when the revision is unchanged this touches neither the
    /// doc nor the widget tree. See the field docs for why it must not run
    /// while a transaction is open.
    fn refresh_undo_status(&mut self) {
        let rev = self.state.history_revision();
        if self.undo_status_rev == Some(rev) {
            return;
        }
        self.undo_status_rev = Some(rev);
        self.undo_status = self.state.undo_status(Some(&self.local_author));
    }

    /// Hand the current [`UndoStatus`](crate::undo::UndoStatus) to every
    /// laid-out widget.
    ///
    /// Sweeps `node_map` rather than walking `children`, so a control reaches
    /// the status at any depth and behind containers that keep private child
    /// storage — the same reason [`Self::resolve_cursor`] works that way.
    pub fn publish_undo_status(&mut self) {
        self.refresh_undo_status();
        let status = self.undo_status.clone();
        for &w_ptr in self.node_map.values() {
            // SAFETY: the `node_map` dereference invariant the rest of `App`
            // relies on — the pointers are valid for the duration of an event
            // dispatch; the tree is only rebuilt between frames.
            let w = unsafe { &mut *w_ptr };
            w.set_undo_status(&status);
        }
    }

    /// Turn any `qos_actions` action the widget tree dispatched during the last
    /// event into one undo boundary. Called from the idle tick; returns whether
    /// an entry was recorded.
    ///
    /// Delegates to [`undo::record_dispatched_action`](crate::undo::record_dispatched_action),
    /// which the headless [`Driver`](crate::headless::Driver) calls too — so a
    /// scenario and a user get the same history from the same interaction.
    pub fn record_dispatched_actions(&mut self) -> bool {
        let root: *mut dyn crate::widget::Widget = self.root_widget.as_mut();
        crate::undo::record_dispatched_action(
            &mut self.state,
            root,
            &self.node_map,
            &self.local_author,
        )
    }

    /// Record one undo boundary for an action this host has just dispatched
    /// itself (Ctrl+K palette, a search-result navigation that mutated the
    /// document), bracketed by `before`.
    ///
    /// The host's own dispatches attribute the same way a widget's do — see
    /// [`record_dispatched_actions`](Self::record_dispatched_actions) — so
    /// whether an action arrived via the palette or via a toolbar button makes
    /// no difference to whether Ctrl+Z can revert it.
    fn record_action_boundary(&mut self, before: Vec<automerge::ChangeHash>) {
        let author = self.local_author.clone();
        self.state
            .record_committed_action_authored(before, None, None, Some(author));
    }

    /// Perform any undo/redo a widget asked for since the last drain.
    ///
    /// Returns `true` if anything happened, so the caller can repaint. Each
    /// request goes through [`perform_undo`](Self::perform_undo) /
    /// [`perform_redo`](Self::perform_redo) — a control never touches the
    /// stacks itself.
    pub fn drain_undo_requests(&mut self) -> bool {
        // Collect first: performing an undo mutates the tree's state, and the
        // sweep must not be holding raw child pointers while that happens.
        let mut requests = Vec::new();
        for &w_ptr in self.node_map.values() {
            // SAFETY: as in `publish_undo_status`.
            let w = unsafe { &mut *w_ptr };
            if let Some(req) = w.take_undo_request() {
                requests.push(req);
            }
        }
        let mut acted = false;
        for req in requests {
            acted |= match req {
                crate::undo::UndoRequest::Undo => self.perform_undo(),
                crate::undo::UndoRequest::Redo => self.perform_redo(),
            };
        }
        acted
    }

    fn apply_undo_entry(&mut self, entry: crate::state::UndoEntry) {
        if let Some(idx) = entry.focused_node_idx {
            if idx < self.focusable_order.len() {
                let id = self.focusable_order[idx];
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
        let changed = self.state.commit_action_authored(
            self.last_stable_focus_idx,
            self.last_stable_metadata.clone(),
            Some(self.local_author.clone()),
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
            let ch = now_text[now_caret..]
                .chars()
                .next()
                .or_else(|| now_text[..now_caret].chars().next_back());
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
        if self.screen_reader.is_none() {
            return;
        }
        // A live region without an accessible name can't be tracked stably
        // across frames, so it's skipped.
        let mut seen: Vec<(String, crate::a11y::Politeness, String)> = Vec::new();
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
        let current_id = if let Some(id) = self.focused_node {
            id
        } else {
            if !self.focusable_order.is_empty() {
                self.set_focused_node(Some(self.focusable_order[0]));
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

    /// `scale` is the window scale factor so bounds land in physical pixels
    /// (what AT-SPI expects).
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
    /// `target` is resolved back through
    /// [`crate::accesskit_bridge::resolve_node_id`], which distinguishes the
    /// three id spaces the bridge emits: the synthetic window root, a laid-out
    /// widget, and a **synthetic node** a widget described without a backing
    /// widget of its own (a table row, a clip on a timeline track). Whichever it
    /// is, the owning widget gets first refusal via
    /// [`Widget::perform_action_at`](crate::widget::Widget::perform_action_at),
    /// which receives the key path naming the element the action was aimed at
    /// (empty for the widget itself) — so a canvas app can act on the one clip
    /// the user was on rather than on the whole canvas.
    ///
    /// If the widget declines, the generic fallback runs: [`Action::Focus`]
    /// moves keyboard focus to the owning widget, and any other verb synthesises
    /// a click at its centre (returned as coordinates so the caller can inject a
    /// [`UserEvent::Click`]). That fallback is necessarily **widget-granular** —
    /// it has no way to reach inside a canvas — so a widget with synthetic nodes
    /// that wants them operable must implement `perform_action_at`.
    ///
    /// An id that names no node in the current frame (a reference an AT held
    /// across a structural change) is dropped: retargeting it at whatever now
    /// occupies that slot would act on the wrong element.
    ///
    /// Returns `Some((x, y))` window-logical coordinates if the caller should
    /// synthesise a click there, else `None` (handled in place, or nothing to do).
    pub fn dispatch_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> Option<(f64, f64)> {
        use crate::a11y::Action;
        use crate::accesskit_bridge::A11yTarget;

        let resolved = crate::accesskit_bridge::resolve_node_id(
            self.root_widget.as_ref(),
            &self.node_map,
            target,
        )?;
        let (taffy_id, key_path): (NodeId, Vec<u64>) = match resolved {
            // The window root has no widget behind it.
            A11yTarget::Root => return None,
            A11yTarget::Widget(id) => (id, Vec::new()),
            A11yTarget::Synthetic { owner, key_path } => (owner, key_path),
        };
        self.dispatch_a11y_action_to(taffy_id, &key_path, action)
    }

    /// Perform an accessibility action on an already-resolved target: the widget
    /// at `taffy_id`, or the synthetic element inside it named by `key_path`
    /// (empty for the widget itself).
    ///
    /// This is the single place a verb is applied, shared by every route that
    /// can produce one — an AccessKit `ActionRequest`
    /// ([`dispatch_a11y_action`](Self::dispatch_a11y_action)), a named
    /// affordance on the retained semantic tree
    /// ([`invoke_semantic_action`](Self::invoke_semantic_action)), and the
    /// keyboard ([`perform_action_on_focused`](Self::perform_action_on_focused)).
    /// Keeping them on one path is what stops a screen reader, an agent, and a
    /// key press from disagreeing about what "activate this" does.
    ///
    /// Returns `Some((x, y))` if the caller should synthesise a click there.
    pub fn dispatch_a11y_action_to(
        &mut self,
        taffy_id: NodeId,
        key_path: &[u64],
        action: &crate::a11y::Action,
    ) -> Option<(f64, f64)> {
        use crate::a11y::Action;
        let &w_ptr = self.node_map.get(&taffy_id)?;

        // Operating an element inside a widget means two things at once: the
        // keyboard goes to the owning widget, and the widget puts its own
        // cursor on that element. Do the first half here so a widget only has
        // to implement the second — otherwise "focus row 4" would either move
        // the keyboard without moving the cursor, or the reverse, depending on
        // whether the widget claimed the action.
        //
        // This covers every verb, not just Focus: clicking a control focuses it
        // as well as pressing it, and an assistive technology activating one
        // must leave the reader on what it just operated rather than back at
        // the window root.
        if !key_path.is_empty() {
            self.set_focused_node(Some(taffy_id));
        }

        // Widget gets first refusal, told which element was targeted.
        let handled = unsafe {
            (*w_ptr).perform_action_at(action, key_path, &mut self.state.doc, &self.state.root)
        };
        if handled {
            self.needs_sync = true;
            self.layout_dirty = true;
            return None;
        }

        match action {
            Action::Focus => {
                self.set_focused_node(Some(taffy_id));
                None
            }
            Action::Activate | Action::ToggleCheck => {
                let rect = unsafe { (*w_ptr).get_rect() };
                let c = rect.center();
                Some((c.x, c.y))
            }
            _ => None,
        }
    }

    /// Perform an accessibility [`Action`](crate::a11y::Action) on whatever the
    /// keyboard is currently pointing at — the focused widget, or, when that
    /// widget nominates one, the **active descendant** inside it.
    ///
    /// This is the keyboard's half of the routing
    /// [`dispatch_a11y_action`](Self::dispatch_a11y_action) does for an
    /// assistive technology, and it deliberately resolves the target the same
    /// way ([`crate::accesskit_bridge::active_descendant_path`]): pressing Enter
    /// must activate the element the reader just announced, not the canvas
    /// containing it. A widget that nominates nothing gets an empty key path and
    /// behaves exactly as it did before.
    ///
    /// Returns `true` if the widget consumed the action.
    pub fn perform_action_on_focused(&mut self, action: &crate::a11y::Action) -> bool {
        let Some(fid) = self.focused_node else {
            return false;
        };
        let Some(&w_ptr) = self.node_map.get(&fid) else {
            return false;
        };
        let key_path = unsafe { (*w_ptr).accessibility() }
            .and_then(|n| crate::accesskit_bridge::active_descendant_path(&n))
            .unwrap_or_default();
        let did = unsafe {
            (*w_ptr).perform_action_at(action, &key_path, &mut self.state.doc, &self.state.root)
        };
        if did {
            self.needs_sync = true;
            self.layout_dirty = true;
        }
        did
    }

    /// Node identity is preserved for everything that recurred, so a
    /// consumer reacts to just the diff.
    pub fn refresh_semantic_tree(&mut self) -> crate::a11y_tree::TreeDelta {
        let resources = self.collect_resources();
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
                // Two kinds of node carry verbs: a laid-out widget, addressed by
                // its taffy id, and a synthetic element inside one, addressed by
                // its owner plus a key path. Before, only the first could be
                // driven — a canvas app's rows and clips reported affordances
                // that silently did nothing when invoked.
                let Some(node) = self.semantic_tree.node(id) else {
                    return Ok(false);
                };
                let (taffy_raw, key_path) = match (&node.synthetic_ref, node.widget_taffy_id) {
                    (Some(sref), _) => (sref.owner_taffy_id, sref.key_path.clone()),
                    (None, Some(raw)) => (raw, Vec::new()),
                    // A root or resource node has no widget to drive.
                    (None, None) => return Ok(false),
                };
                // Give the widget first refusal (its `perform_action_at`) and let
                // the generic focus fallback run. A `Some((x, y))` result means the
                // widget self-activates only via a pointer click, which needs the
                // windowed input path (the winit handler synthesises it for an
                // AccessKit `ActionRequest`); such widgets should implement
                // `perform_action_at`. We surface the verb as dispatched either way.
                let _pending_click =
                    self.dispatch_a11y_action_to(NodeId::from(taffy_raw), &key_path, &verb);
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
            self.record_action_boundary(before);
            self.needs_sync = true;
            self.layout_dirty = true;
        }
        Ok(true)
    }

    /// Lets headless tests stand the App up without spinning a winit loop.
    #[doc(hidden)]
    pub fn init_layout(&mut self) {
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
    pub fn with_autosave(mut self, autosave: crate::persist::Autosave) -> Self {
        self.autosave = Some(autosave);
        self
    }

    /// Takes effect when the window is created, so set before [`App::run`].
    pub fn with_window_title(mut self, title: impl Into<String>) -> Self {
        self.window_title = Some(title.into());
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
    pub fn run_with_setup<F>(mut self, setup: F)
    where
        F: FnOnce(EventLoopProxy<UserEvent>),
    {
        let event_loop = EventLoop::<UserEvent>::with_user_event().build().unwrap();
        let proxy = event_loop.create_proxy();
        // The AccessKit adapter routes events back through this as
        // `UserEvent::AccessKit`.
        let a11y_proxy = proxy.clone();
        setup(proxy);

        self.init_layout();

        let mut handler = AppHandler {
            app: self,
            window: None,
            render: None,
            a11y_adapter: None,
            a11y_proxy,
            ime_allowed: false,
            ime_cursor_rect: None,
        };
        event_loop.run_app(&mut handler).unwrap();
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
    /// `None` until `resumed` creates the window (must be built while
    /// hidden); lazy, so the hot path pays nothing with no screen reader
    /// running.
    a11y_adapter: Option<accesskit_winit::Adapter>,
    a11y_proxy: EventLoopProxy<UserEvent>,
    /// Cached so a steady frame does not issue platform text-input requests.
    ime_allowed: bool,
    ime_cursor_rect: Option<vello::kurbo::Rect>,
}

impl AppHandler {
    /// Called from the first `RedrawRequested` rather than inline in
    /// `resumed`, so the blocking GPU init doesn't starve the loop's first
    /// events. `Err` is returned, not panicked, since every step here can
    /// fail when the compositor exits mid-open.
    fn init_render(&mut self, window: &Arc<Window>) -> Result<(), String> {
        let size = window.inner_size();
        let mut render_cx = RenderContext::new();
        let surface = pollster::block_on(render_cx.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            vello::wgpu::PresentMode::AutoVsync,
        ))
        .map_err(|e| format!("could not create a drawing surface: {e}"))?;
        let device_handle = &render_cx.devices[surface.dev_id];
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
        // After this, every GPU call happens on the `ui-render` thread.
        self.render = Some(spawn_render_thread(render_cx, surface, renderer));
        Ok(())
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
            let app_id = resolve_app_id(
                std::env::var("QOS_PACKAGE_ID").ok().as_deref(),
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .as_deref(),
            );
            window_attributes = window_attributes.with_name(app_id, "");
        }
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
                self.app.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if self.render.is_none() {
                    qos_startup_metrics::mark("first-redraw-init-render-start");
                    if let Err(reason) = self.init_render(&window) {
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
                let ime_allowed_cache = &mut self.ime_allowed;
                let ime_rect_cache = &mut self.ime_cursor_rect;
                let app = &mut self.app;
                // A `Reconfigured` or `Skipped` previous frame re-requests a
                // redraw so the user doesn't see a stale image.
                let mut needs_followup_redraw = false;
                let mut surface_gone = false;
                while let Ok(outcome) = render.frame_complete.try_recv() {
                    match outcome {
                        FrameOutcome::Presented => {}
                        FrameOutcome::Reconfigured | FrameOutcome::Skipped => {
                            needs_followup_redraw = true;
                        }
                        FrameOutcome::SurfaceGone => {
                            surface_gone = true;
                        }
                    }
                }
                if surface_gone {
                    tracing::info!("drawing surface is gone; closing the window");
                    event_loop.exit();
                    return;
                }

                // `recv()` is the backpressure boundary — capacity-2 means
                // at most one frame of overlap (main building N+1 while
                // worker presents N).
                let mut scene = if let Ok(s) = render.scene_pool.recv() {
                    s
                } else {
                    tracing::error!("ui-render thread died; closing window");
                    event_loop.exit();
                    return;
                };
                scene.reset();

                let inner = window.inner_size();
                let width = inner.width;
                let height = inner.height;
                let scale_factor = window.scale_factor() as f32;
                app.last_frame_size = Some(inner);

                // Records first: if the root widget opened a different
                // record, sync against that record's document, not the
                // outgoing one's.
                app.record_tick();

                let __prof_frame_start = app.profiler.as_ref().map(|_| std::time::Instant::now());
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
                if let Some(p) = app.profiler.as_mut() {
                    p.record_since(crate::profiling::Pass::BuildLayout, __prof_build);
                }

                let perf = perf_enabled();
                let t_frame_start = if perf {
                    Some(std::time::Instant::now())
                } else {
                    None
                };
                let mut t_compute_us = 0u128;
                let mut t_apply_us = 0u128;
                let mut t_relayout_branch = false;

                let root_node = app.root_node.unwrap();
                if app.layout_dirty {
                    t_relayout_branch = true;
                    // A frame dirty only because content scrolled needs
                    // `apply_layout` to push the offset down, but not a full
                    // `compute_layout` — re-solving the whole tree on every
                    // scroll frame was the dominant per-frame cost on long
                    // lists/grids.
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
                        app.taffy
                            .compute_layout_with_measure(
                                root_node,
                                taffy::Size {
                                    width: AvailableSpace::Definite(logical_w as f32),
                                    height: AvailableSpace::Definite(logical_h as f32),
                                },
                                |known, available, node_id, _, _| {
                                    if let Some(&widget_ptr) = node_map.get(&node_id) {
                                        unsafe {
                                            (*widget_ptr).measure(known, available, text_renderer)
                                        }
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

                // Hand the widget tree the live undo status, after layout has
                // settled (so a control built this frame is in `node_map`) and
                // before draw and the accessibility export — the disabled state
                // an AT is told about has to be the one that gets painted.
                app.publish_undo_status();

                let t_draw_start = if perf {
                    Some(std::time::Instant::now())
                } else {
                    None
                };
                // Widget rects are logical; this transform rasterises at the
                // physical resolution, so the frame stays crisp at any
                // fractional scale.
                let transform = crate::scale::composite_transform(f64::from(scale_factor));
                let (vp_w, vp_h) =
                    crate::scale::logical_size((width, height), f64::from(scale_factor));
                let window_rect = vello::kurbo::Rect::new(0.0, 0.0, vp_w, vp_h);
                let prev_vp = app.presence_ctx.push_viewport(window_rect);
                // No GPU damage clip: vello allocates a fresh `target_texture`
                // per frame, so clipping out a static region would just paint
                // background instead of preserving the last frame.
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
                // Without this the incremental dirty gate is a no-op.
                app.root_widget.mark_drawn();

                // Widgets whose visual extent escapes their layout box (an
                // open `Dropdown`, a popover) composite above every
                // later-drawn sibling here. No-op by default.
                app.root_widget.draw_overlay(
                    &mut scene,
                    transform,
                    &mut app.text_renderer,
                    &app.theme,
                    &app.presence_ctx,
                );

                // Text widgets publish the caret rectangle after shaping, so
                // the platform IME candidate window follows the exact glyph
                // position (including wrapping, scrolling, and RTL). A
                // focused non-text control returns `None`, disabling IME.
                let ime_rect = app.focused_node.and_then(|id| {
                    app.node_map
                        .get(&id)
                        .and_then(|&widget| unsafe { (*widget).ime_cursor_rect() })
                });
                let ime_allowed = ime_rect.is_some();
                if *ime_allowed_cache != ime_allowed {
                    window.set_ime_allowed(ime_allowed);
                    *ime_allowed_cache = ime_allowed;
                }
                if ime_rect != *ime_rect_cache {
                    if let Some(rect) = ime_rect {
                        window.set_ime_cursor_area(
                            winit::dpi::LogicalPosition::new(rect.x0.max(0.0), rect.y0.max(0.0)),
                            winit::dpi::LogicalSize::new(
                                rect.width().max(1.0),
                                rect.height().max(1.0),
                            ),
                        );
                    }
                    *ime_rect_cache = ime_rect;
                }

                if let Some(ap) = app.action_palette.as_mut() {
                    let win_w = f64::from(width) / f64::from(scale_factor);
                    let card_w = (win_w * 0.6).clamp(360.0, 640.0);
                    let x0 = ((win_w - card_w) / 2.0).max(0.0);
                    let y0 = (f64::from(height) / f64::from(scale_factor) * 0.12).max(24.0);
                    ap.layout(&app.theme, Point::new(x0, y0), card_w);
                    ap.draw(
                        &mut scene,
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

                if let Some(p) = app.profiler.as_mut() {
                    p.record_since(crate::profiling::Pass::Draw, __prof_draw);
                }
                let t_draw_us = t_draw_start.map_or(0, |t| t.elapsed().as_micros());

                // Legacy path — `state.collab.cursors` predates PresenceCtx;
                // kept for apps that still write into it directly.
                for cursor in app.state.collab.cursors.values() {
                    let mut path = vello::kurbo::BezPath::new();
                    path.move_to(Point::new(0.0, 0.0));
                    path.line_to(Point::new(14.0, 14.0));
                    path.line_to(Point::new(6.0, 16.0));
                    path.line_to(Point::new(0.0, 22.0));
                    path.close_path();

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
                        &mut scene,
                        &cursor.name,
                        10.0,
                        Color::WHITE,
                        cursor_transform * Affine::translate((10.0, 20.0)),
                        None,
                    );
                }

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
                        scene.stroke(
                            &vello::kurbo::Stroke::new(1.5),
                            transform,
                            color,
                            None,
                            &rect,
                        );
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
                let mut is_undo_redo = false;

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
                            && app.modifiers.state().control_key()
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
                                logical_key:
                                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::F12),
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
                    && app.modifiers.state().control_key()
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
                    // The history bindings resolve through `undo::shortcut`,
                    // the one table both this host and the headless driver
                    // read, and are performed through `perform_undo` /
                    // `perform_redo` — the same methods an on-screen Undo
                    // control reaches. One binding, one implementation.
                    //
                    // Those methods snapshot the caret (`undo_focus_snapshot`)
                    // before touching a stack; otherwise round-tripping
                    // undo/redo loses caret position.
                    if let Some(req) = crate::undo::shortcut(c.as_str(), true, shift) {
                        match req {
                            crate::undo::UndoRequest::Undo => app.perform_undo(),
                            crate::undo::UndoRequest::Redo => app.perform_redo(),
                        };
                        is_undo_redo = true;
                        handled = true;
                    }
                    match c.as_str() {
                        // Ctrl+K opens the action palette scoped to the focused
                        // app instance's open resources. A second Ctrl+K
                        // (palette already open) closes it.
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

                if !handled
                    && app.keyboard_nav_allowed(modal_rect)
                    && let WindowEvent::KeyboardInput {
                        event:
                            winit::event::KeyEvent {
                                logical_key:
                                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
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
                if !handled {
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
                        WindowEvent::MouseInput {
                            state: winit::event::ElementState::Pressed,
                            ..
                        }
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
                                        winit::keyboard::NamedKey::Enter
                                        | winit::keyboard::NamedKey::Space,
                                    ),
                                state: winit::event::ElementState::Pressed,
                                ..
                            },
                        ..
                    } = event
                    && app.perform_action_on_focused(&crate::a11y::Action::Activate)
                {
                    handled = true;
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
                    if let Some(act) = action {
                        let mut any = false;
                        for _ in 0..times {
                            any |= app.perform_action_on_focused(&act);
                        }
                        if any {
                            handled = true;
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

                app.broadcast_focus_presence();
                app.broadcast_location_presence();
                app.broadcast_view_presence();

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
                    if !is_pointer_move || app.root_widget.needs_redraw() {
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
        let Some(window) = self.window.clone() else {
            return;
        };
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

        // Keep pumping frames while any "just typed" highlighter mark is
        // still fading, so the taper animates instead of freezing.
        let typing_now = Instant::now();
        app.presence_ctx.prune_typing(typing_now);
        let typing_active = app.presence_ctx.has_active_typing(typing_now);
        if typing_active {
            window.request_redraw();
        }

        // An affordance may have dispatched a `qos_actions` action out-of-band
        // during the last event (the button-driven twin of the Ctrl+K palette).
        // Record one undo boundary bracketing it, so Ctrl+Z reverts it.
        if app.record_dispatched_actions() {
            app.needs_sync = true;
            window.request_redraw();
        }

        if app.root_widget.needs_commit() {
            app.commit_action();
            app.root_widget.on_commit();
            app.needs_sync = true;
            window.request_redraw();
        }

        // An Undo/Redo control the user activated during the last event. Runs
        // after the commit block above so an action performed in the same tick
        // is already on the stack and is what gets reverted. Both verbs take
        // the same route Ctrl+Z does.
        if app.drain_undo_requests() {
            app.needs_sync = true;
            window.request_redraw();
        }
        // Re-derive the status now, at a point where the host has just closed
        // the transaction itself. A no-op unless a stack actually moved.
        app.refresh_undo_status();

        // Level-triggered rather than hooked into individual mutations:
        // widgets write to the document from many places.
        app.autosave_tick();

        if app.reconcile_fullscreen(&window) {
            window.request_redraw();
        }

        if app.reconcile_appearance() {
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
        if app.collab_manager.is_connected() || needs_frame || tooltip_pending || typing_active {
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(16),
            ));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
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
            let m = match doc.get(&ROOT, "m") {
                Ok(Some((automerge::Value::Object(ObjType::Map), id))) => id,
                _ => panic!("m exists"),
            };
            doc.put(&m, "a", 10).unwrap();
        }

        let mut peer = automerge::AutoCommit::load(&base_bytes).unwrap();
        let pm = match peer.get(&ROOT, "m") {
            Ok(Some((automerge::Value::Object(ObjType::Map), id))) => id,
            _ => panic!("m exists"),
        };
        peer.put(&pm, "b", 20).unwrap();
        let incoming = peer.save();

        app.apply_collab_event_for_test(CollabEvent::CRDTUpdate { data: incoming });

        // A replace would drop "a".
        let doc = &mut app.state_mut_for_test().doc;
        let m = match doc.get(&ROOT, "m") {
            Ok(Some((automerge::Value::Object(ObjType::Map), id))) => id,
            _ => panic!("m exists"),
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
            self.kids
                .iter_mut()
                .map(|c| &mut **c as &mut (dyn Widget + '_))
                .collect()
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
        let ak_id = crate::accesskit_bridge::widget_ak_id(&app.node_map, second).unwrap();
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
        let ak_id = crate::accesskit_bridge::widget_ak_id(&app.node_map, second).unwrap();
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
            self.kids
                .iter_mut()
                .map(|c| &mut **c as &mut (dyn Widget + '_))
                .collect()
        }
        fn name(&self) -> &str {
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
        fn name(&self) -> &str {
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
    use crate::widget::Widget;
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
        let mut app = app_with_root(ViewRoot::new());

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
        Action as QAction, ActionError, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
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

    /// One open resource exporting an immediate `bump` and a prompted
    /// `set-label`.
    struct CounterCard {
        button: Button,
    }
    impl CounterCard {
        fn new() -> Self {
            Self {
                button: Button::new("bump", "Bump", None),
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
            vec![
                Resource::new("test.counter", "test.counter", "Counter")
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
                            EncodedAction::encode(
                                COUNTER_TYPE,
                                &CounterAction::SetLabel(s.to_string()),
                            )
                            .unwrap()
                        },
                    )),
            ]
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
}

/// End-to-end proof that a widget's **synthetic** accessibility nodes — the
/// ones a canvas-style widget describes without any child widget behind them —
/// are reachable by an assistive technology, not merely describable to one.
///
/// Every assertion here is made against what the real bridge emits
/// ([`App::build_a11y_tree_update`]) and against what the real dispatch path
/// does ([`App::dispatch_a11y_action`]), so nothing passes because a hand-built
/// node list happened to look right.
#[cfg(test)]
mod synthetic_node_dispatch_tests {
    use super::*;
    use crate::a11y::{Action, Role};
    use crate::state::State;
    use crate::widget::Widget;
    use crate::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use taffy::{NodeId, TaffyTree};
    use vello::kurbo::Rect as KurboRect;
    use winit::event::WindowEvent;

    #[derive(Clone)]
    struct Person {
        name: &'static str,
    }

    /// A three-row selectable table, plus the recorder its `on_activate` writes
    /// the activated row index into.
    fn table_app() -> (App, Rc<RefCell<Vec<usize>>>) {
        let fired: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = fired.clone();
        let mut table: DataTable<Person> = DataTable::new(vec![
            ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)),
            ColumnDef::new("role", "Role", ColumnWidth::Flex(1.0)),
        ])
        .selectable(true)
        .on_activate(move |i| sink.borrow_mut().push(i));
        table.set_rows(vec![
            Person { name: "Ada" },
            Person { name: "Grace" },
            Person { name: "Katherine" },
        ]);

        let mut app = App::new(State::new(), Box::new(table));
        app.init_layout();
        (app, fired)
    }

    /// The id the bridge emitted for the row at 0-based `index`. Rows carry no
    /// label, so they are located by their exported 1-based collection position
    /// — the same handle a reader announces as "row 2 of 3".
    fn row_id(update: &accesskit::TreeUpdate, index: usize) -> accesskit::NodeId {
        update
            .nodes
            .iter()
            .find(|(_, n)| {
                n.role() == accesskit::Role::Row && n.position_in_set() == Some(index + 1)
            })
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("no exported node for row {index}"))
    }

    /// A canvas widget in the shape the video editor and `sheets` use: it paints
    /// its own contents, has no child widgets, describes its elements as
    /// synthetic nodes keyed by identity, and implements **only**
    /// `perform_action_at`.
    struct Timeline {
        clips: Vec<&'static str>,
        cursor: usize,
        focused: bool,
        activated: Rc<RefCell<Vec<String>>>,
    }

    impl Timeline {
        fn new(activated: Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                clips: vec!["intro", "interview", "outro"],
                cursor: 1,
                focused: false,
                activated,
            }
        }
    }

    impl Widget for Timeline {
        fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &crate::theme::Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, self as *mut dyn Widget);
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: vello::kurbo::Point) {}
        fn draw(
            &mut self,
            _sc: &mut vello::Scene,
            _tr: vello::kurbo::Affine,
            _txt: &mut crate::text_render::TextRenderer,
            _th: &crate::theme::Theme,
            _pr: &crate::presence::PresenceCtx,
        ) {
        }
        #[allow(clippy::too_many_arguments)]
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
        fn name(&self) -> &str {
            "Timeline"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::new(0.0, 0.0, 400.0, 100.0)
        }
        fn is_focusable(&self) -> bool {
            true
        }
        fn set_focus(&mut self, f: bool) {
            self.focused = f;
        }
        fn is_focused(&self) -> bool {
            self.focused
        }

        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            use crate::a11y::{AccessNode, AccessState, Role};
            Some(
                AccessNode::new(Role::Canvas)
                    .with_name("Timeline")
                    .with_state(AccessState::new().focusable(true).focused(self.focused))
                    .with_children(
                        self.clips
                            .iter()
                            .enumerate()
                            .map(|(i, id)| {
                                AccessNode::new(Role::ListItem)
                                    .with_name(*id)
                                    // Identity-keyed: a clip keeps its id when
                                    // it is dragged to another position.
                                    .with_hashed_key(*id)
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .focused(i == self.cursor),
                                    )
                            })
                            .collect(),
                    ),
            )
        }

        fn perform_action_at(
            &mut self,
            action: &crate::a11y::Action,
            key_path: &[u64],
            _state: &mut automerge::AutoCommit,
            _path: &automerge::ObjId,
        ) -> bool {
            use crate::a11y::{Action, hash_key};
            let Some(&key) = key_path.first() else {
                return false;
            };
            // Recover the clip from its hashed id — a stale key matches nothing
            // and is dropped rather than guessed at.
            let Some(index) = self.clips.iter().position(|id| hash_key(id) == key) else {
                return false;
            };
            match action {
                Action::Focus => {
                    self.cursor = index;
                    true
                }
                Action::Activate => {
                    self.cursor = index;
                    self.activated.borrow_mut().push(self.clips[index].into());
                    true
                }
                _ => false,
            }
        }
    }

    #[test]
    fn keyboard_activation_reaches_the_clip_the_reader_is_on() {
        // A canvas that implements only the routed hook must still be operable
        // from the keyboard, aimed at the same element the reader announced.
        let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mut app = App::new(State::new(), Box::new(Timeline::new(activated.clone())));
        app.autofocus_first = true;
        app.init_layout();

        // The reader is on "interview" (the widget's cursor).
        let update = app.build_a11y_tree_update(1.0);
        let (_, focused) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == update.focus)
            .expect("focus names a node in the update");
        assert_eq!(focused.label(), Some("interview"));

        // Enter activates exactly that clip, not the canvas.
        assert!(app.perform_action_on_focused(&Action::Activate));
        assert_eq!(*activated.borrow(), vec!["interview".to_string()]);
    }

    #[test]
    fn an_assistive_technology_and_the_keyboard_agree_on_the_target() {
        // Drive the cursor with an AT focus request, then press Enter: the two
        // routes must land on the same clip, or Enter activates something other
        // than what was just announced.
        let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mut app = App::new(State::new(), Box::new(Timeline::new(activated.clone())));
        app.autofocus_first = true;
        app.init_layout();

        let update = app.build_a11y_tree_update(1.0);
        let outro = update
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some("outro"))
            .map(|(id, _)| *id)
            .expect("the clip reached the tree");

        app.dispatch_a11y_action(outro, &Action::Focus);
        let after = app.build_a11y_tree_update(1.0);
        assert_eq!(after.focus, outro, "the AT moved the reader onto outro");

        assert!(app.perform_action_on_focused(&Action::Activate));
        assert_eq!(
            *activated.borrow(),
            vec!["outro".to_string()],
            "the keyboard activated the clip the reader was on"
        );
    }

    #[test]
    fn a_clip_keeps_its_identity_when_the_track_is_reordered() {
        // The whole point of an identity key: an id an AT is holding must follow
        // the clip, not the slot it happened to occupy.
        let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mut app = App::new(State::new(), Box::new(Timeline::new(activated.clone())));
        app.init_layout();

        let before = app.build_a11y_tree_update(1.0);
        let outro = before
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some("outro"))
            .map(|(id, _)| *id)
            .unwrap();

        // Drag "outro" to the front.
        {
            let w = app.node_map.values().next().copied().unwrap();
            let t = unsafe { &mut *(w as *mut Timeline) };
            t.clips = vec!["outro", "intro", "interview"];
        }

        let after = app.build_a11y_tree_update(1.0);
        assert_eq!(
            after
                .nodes
                .iter()
                .find(|(_, n)| n.label() == Some("outro"))
                .map(|(id, _)| *id),
            Some(outro),
            "the clip kept its id across the move"
        );
        // And the id an AT was holding still activates that same clip.
        app.dispatch_a11y_action(outro, &Action::Activate);
        assert_eq!(*activated.borrow(), vec!["outro".to_string()]);
    }

    // ---- The retained semantic tree (agent / CLI / headless route) --------

    /// Find the semantic node for a clip by its accessible name.
    fn semantic_clip(app: &App, name: &str) -> crate::a11y_tree::SemanticId {
        app.semantic_tree()
            .find_by_name(name)
            .map(|n| n.id.clone())
            .unwrap_or_else(|| panic!("no semantic node named {name}"))
    }

    #[test]
    fn a_synthetic_semantic_node_carries_how_to_reach_it() {
        let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mut app = App::new(State::new(), Box::new(Timeline::new(activated)));
        app.init_layout();
        app.refresh_semantic_tree();

        let owner = app
            .node_map
            .keys()
            .next()
            .copied()
            .map(u64::from)
            .expect("the timeline was laid out");

        let id = semantic_clip(&app, "outro");
        let node = app.semantic_tree().node(&id).unwrap();
        let sref = node
            .synthetic_ref
            .as_ref()
            .expect("a synthetic node must say how to reach it, or it cannot be driven");
        assert_eq!(sref.owner_taffy_id, owner);
        assert_eq!(
            sref.key_path,
            vec![crate::a11y::hash_key(&"outro")],
            "the path is the clip's own identity key"
        );
        assert_eq!(
            node.widget_taffy_id, None,
            "it is not a widget, so it must not claim a widget id"
        );

        // The owning widget, by contrast, is addressed by its taffy id alone.
        let canvas = app.semantic_tree().find_by_name("Timeline").unwrap();
        assert_eq!(canvas.widget_taffy_id, Some(owner));
        assert!(canvas.synthetic_ref.is_none());
    }

    #[test]
    fn invoking_a_verb_on_a_synthetic_node_drives_that_element() {
        // The gap this closes: the node advertised an "activate" affordance and
        // invoking it silently did nothing.
        let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mut app = App::new(State::new(), Box::new(Timeline::new(activated.clone())));
        app.init_layout();
        app.refresh_semantic_tree();

        let id = semantic_clip(&app, "outro");
        assert!(
            app.semantic_tree()
                .node(&id)
                .unwrap()
                .action("activate")
                .is_some(),
            "the clip advertises an activate affordance"
        );

        assert_eq!(app.invoke_semantic_action(&id, "activate", None), Ok(true));
        assert_eq!(
            *activated.borrow(),
            vec!["outro".to_string()],
            "invoking the affordance must reach that clip"
        );
    }

    #[test]
    fn focusing_a_synthetic_node_by_name_moves_the_exported_focus() {
        // The semantic route and the AccessKit route must agree: focusing "intro"
        // by name has to put the reader on the same node an AT would land on.
        let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mut app = App::new(State::new(), Box::new(Timeline::new(activated)));
        app.init_layout();
        app.refresh_semantic_tree();

        let id = semantic_clip(&app, "intro");
        assert_eq!(app.invoke_semantic_action(&id, "focus", None), Ok(true));

        let update = app.build_a11y_tree_update(1.0);
        let (_, focused) = update
            .nodes
            .iter()
            .find(|(nid, _)| *nid == update.focus)
            .expect("focus names a node in the update");
        assert_eq!(
            focused.label(),
            Some("intro"),
            "the semantic route must move the reader's cursor like the AT route does"
        );
    }

    #[test]
    fn a_synthetic_semantic_id_follows_its_element_across_a_reorder() {
        // An agent that resolved "outro" in one frame and invokes it in the next
        // must still hit that clip, not whichever one took its slot.
        let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mut app = App::new(State::new(), Box::new(Timeline::new(activated.clone())));
        app.init_layout();
        app.refresh_semantic_tree();
        let before = semantic_clip(&app, "outro");

        {
            let w = app.node_map.values().next().copied().unwrap();
            let t = unsafe { &mut *(w as *mut Timeline) };
            t.clips = vec!["outro", "intro", "interview"];
        }
        app.refresh_semantic_tree();

        assert_eq!(
            semantic_clip(&app, "outro"),
            before,
            "the semantic id belongs to the clip, not to the slot"
        );
        assert_eq!(
            app.invoke_semantic_action(&before, "activate", None),
            Ok(true)
        );
        assert_eq!(*activated.borrow(), vec!["outro".to_string()]);
    }

    #[test]
    fn a_table_rows_semantic_id_is_its_position() {
        // A row has no identity of its own, so it is keyed by position — "row 2"
        // is what the user means — and that path is what reaches the table.
        let (mut app, fired) = table_app();
        app.refresh_semantic_tree();

        let row = app
            .semantic_tree()
            .iter()
            .find(|n| n.role == Role::Row)
            .map(|n| n.id.clone())
            .expect("rows reach the semantic tree");
        let sref = app
            .semantic_tree()
            .node(&row)
            .unwrap()
            .synthetic_ref
            .clone()
            .expect("a row must be reachable");
        assert_eq!(sref.key_path.len(), 1, "a row is one level below the table");

        let index = sref.key_path[0] as usize;
        assert_eq!(app.invoke_semantic_action(&row, "activate", None), Ok(true));
        assert_eq!(
            *fired.borrow(),
            vec![index],
            "the affordance activated the row its key path named"
        );
    }

    #[test]
    fn a_node_with_nothing_behind_it_reports_that_it_was_not_driven() {
        // The window root carries no widget and no synthetic ref. Invoking a verb
        // there must answer "no", not pretend it did something.
        let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mut app = App::new(State::new(), Box::new(Timeline::new(activated)));
        app.init_layout();
        app.refresh_semantic_tree();

        let root = app.semantic_tree().root().clone();
        let node = app.semantic_tree().node(&root).unwrap();
        assert!(node.widget_taffy_id.is_none() && node.synthetic_ref.is_none());
        assert_eq!(
            app.invoke_semantic_action(&root, "activate", None),
            Ok(false)
        );
    }

    #[test]
    fn every_row_reaches_the_tree_as_a_focusable_node() {
        let (app, _fired) = table_app();
        let update = app.build_a11y_tree_update(1.0);

        let rows: Vec<_> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == accesskit::Role::Row)
            .collect();
        assert_eq!(rows.len(), 3, "one exported node per row");
        for (id, node) in &rows {
            assert!(
                node.supports_action(accesskit::Action::Focus),
                "a row an AT cannot focus is a row it cannot navigate to"
            );
            assert!(node.supports_action(accesskit::Action::Click));
            assert!(
                crate::accesskit_bridge::is_synthetic_id(*id),
                "rows have no widget of their own, so they live in the synthetic id space"
            );
        }
    }

    #[test]
    fn focusing_a_row_moves_the_readers_cursor_onto_that_row() {
        // The gap this closes: before, focus could only ever land on the whole
        // table, so a reader user arrowing through rows heard "table" each time.
        let (mut app, _fired) = table_app();
        let before = app.build_a11y_tree_update(1.0);
        let second = row_id(&before, 1);
        assert_ne!(before.focus, second, "nothing is focused yet");

        app.dispatch_a11y_action(second, &Action::Focus);

        let after = app.build_a11y_tree_update(1.0);
        assert_eq!(
            after.focus, second,
            "the tree's focus must be the row the AT asked for, not its owning table"
        );
        // ...and the keyboard went to the table, so typing still reaches it.
        let table_taffy = app
            .node_map
            .iter()
            .find(|(_, ptr)| {
                let w = unsafe { &***ptr };
                w.accessibility()
                    .map(|n| n.role == Role::Table)
                    .unwrap_or(false)
            })
            .map(|(&id, _)| id)
            .expect("the table was laid out");
        assert_eq!(
            app.focused_node,
            Some(table_taffy),
            "focusing a descendant must also put the keyboard on its owner"
        );
        // The focused node must exist in the update it belongs to — AccessKit
        // panics the consuming AT otherwise.
        assert!(after.nodes.iter().any(|(id, _)| *id == after.focus));
    }

    #[test]
    fn moving_the_cursor_moves_the_exported_focus_with_it() {
        let (mut app, _fired) = table_app();
        for index in [2usize, 0, 1] {
            let update = app.build_a11y_tree_update(1.0);
            let target = row_id(&update, index);
            app.dispatch_a11y_action(target, &Action::Focus);
            let after = app.build_a11y_tree_update(1.0);
            assert_eq!(
                after.focus, target,
                "focus should follow the AT to row {index}"
            );
            let (_, node) = after
                .nodes
                .iter()
                .find(|(id, _)| *id == after.focus)
                .unwrap();
            assert_eq!(
                node.position_in_set(),
                Some(index + 1),
                "and the focused node should be that very row"
            );
        }
    }

    #[test]
    fn activating_a_row_activates_that_row_and_not_the_whole_table() {
        // Without per-node routing the host could only synthesise a click at the
        // table's centre, which activates whatever row happens to sit there.
        let (mut app, fired) = table_app();
        let update = app.build_a11y_tree_update(1.0);

        app.dispatch_a11y_action(row_id(&update, 2), &Action::Activate);
        assert_eq!(
            *fired.borrow(),
            vec![2],
            "the activation must carry the row the AT named"
        );

        app.dispatch_a11y_action(row_id(&update, 0), &Action::Activate);
        assert_eq!(*fired.borrow(), vec![2, 0]);
    }

    #[test]
    fn activating_a_cell_activates_its_row() {
        // A cell is addressed as [row, column]; the table has no independent
        // per-cell behaviour, so the row is what gets activated.
        let (mut app, fired) = table_app();
        let update = app.build_a11y_tree_update(1.0);
        let row1 = row_id(&update, 1);
        let (_, row_node) = update.nodes.iter().find(|(id, _)| *id == row1).unwrap();
        let cell = *row_node.children().last().expect("the row exports cells");

        app.dispatch_a11y_action(cell, &Action::Activate);
        assert_eq!(*fired.borrow(), vec![1]);
    }

    #[test]
    fn a_stale_row_reference_does_nothing_rather_than_hitting_the_wrong_row() {
        // An AT can hold a node id across a change. Acting on "whatever is at
        // that index now" is how the wrong record gets deleted.
        let (mut app, fired) = table_app();
        let update = app.build_a11y_tree_update(1.0);
        let real = row_id(&update, 0);

        // Same id space, no such node.
        let stale = accesskit::NodeId(real.0 ^ 0x5555_5555);
        assert!(crate::accesskit_bridge::is_synthetic_id(stale));
        assert_eq!(app.dispatch_a11y_action(stale, &Action::Activate), None);
        assert!(
            fired.borrow().is_empty(),
            "an unresolvable id must activate nothing at all"
        );
        assert_eq!(app.focused_node, None, "and must not move focus either");
    }
}

/// The Undo control, proved end to end against the host.
///
/// Every assertion here runs through what production runs: the real
/// `perform_undo` the Ctrl+Z handler calls, the real `drain_undo_requests` the
/// idle tick calls, and the real AccessKit tree the bridge exports. Nothing is
/// asserted against a hand-built status or a stubbed widget list.
#[cfg(test)]
mod undo_control_tests {
    use super::*;
    use crate::a11y::Action;
    use crate::state::State;
    use crate::widget::{Row, UndoButton, Widget};
    use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};

    /// Build the tree an app would: an Undo button and a Redo button in a row.
    ///
    /// `Row` does not forward `set_undo_status` or `take_undo_request` — it has
    /// no idea they exist — so a button that works here proves the host reaches
    /// nested widgets through `node_map` rather than through container
    /// cooperation.
    fn app_with_history_controls() -> App {
        let row = Row::new()
            .push(Box::new(UndoButton::undo()))
            .push(Box::new(UndoButton::redo()));
        let mut app = App::new(State::new(), Box::new(row));
        app.init_layout();
        app
    }

    /// One real edit, committed the way the host commits one.
    fn type_action(app: &mut App, text: &str) {
        let obj = app
            .state
            .doc
            .put_object(&ROOT, "body", ObjType::Text)
            .unwrap();
        app.state.doc.splice_text(&obj, 0, 0, text).unwrap();
        app.commit_action();
    }

    fn body(app: &App) -> String {
        match app.state.doc.get(&ROOT, "body") {
            Ok(Some((_, obj))) => app.state.doc.text(&obj).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// The exported AccessKit node for the control labelled `name`.
    fn exported(app: &App, name: &str) -> accesskit::Node {
        let update = app.build_a11y_tree_update(1.0);
        update
            .nodes
            .iter()
            .find(|(_, n)| n.label().as_deref() == Some(name))
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| panic!("no exported node labelled {name}"))
    }

    fn exported_id(app: &App, name: &str) -> accesskit::NodeId {
        let update = app.build_a11y_tree_update(1.0);
        update
            .nodes
            .iter()
            .find(|(_, n)| n.label().as_deref() == Some(name))
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("no exported node labelled {name}"))
    }

    /// What an assistive technology is told must be what the host would
    /// actually do. Asserted against the exported tree on one side and against
    /// `perform_undo`'s own return on the other, so a mismatch cannot hide.
    #[test]
    fn the_exported_disabled_state_matches_whether_undo_would_do_anything() {
        let mut app = app_with_history_controls();
        app.publish_undo_status();

        assert!(
            exported(&app, "Undo").is_disabled(),
            "an empty history must export as disabled"
        );
        assert_eq!(
            exported(&app, "Undo").description().as_deref(),
            Some("Nothing to undo"),
            "and must not describe an action that is not there"
        );
        assert!(exported(&app, "Redo").is_disabled());

        type_action(&mut app, "hello");
        app.publish_undo_status();

        assert!(
            !exported(&app, "Undo").is_disabled(),
            "an action on the stack must export as enabled"
        );
        assert_eq!(
            exported(&app, "Undo").description().as_deref(),
            Some("Undo: +5 -0 chars")
        );
        assert!(
            exported(&app, "Redo").is_disabled(),
            "nothing has been undone, so Redo stays disabled"
        );

        // The claim is now checked against the act itself.
        assert!(app.perform_undo(), "the tree promised an undo; do it");
        assert_eq!(body(&app), "");
        app.publish_undo_status();
        assert!(
            exported(&app, "Undo").is_disabled(),
            "the stack is empty again"
        );
        assert!(
            !exported(&app, "Redo").is_disabled(),
            "and the action is now redoable"
        );
    }

    /// A disabled Undo control must not be reachable by Tab. `focusable_order`
    /// is the live list the host tabs through, so this is the real traversal.
    #[test]
    fn a_dead_undo_control_is_skipped_by_keyboard_focus() {
        let mut app = app_with_history_controls();
        app.publish_undo_status();
        app.update_focusable_order();
        assert!(
            app.focusable_order.is_empty(),
            "nothing to undo or redo, so neither control takes focus"
        );

        type_action(&mut app, "hello");
        app.publish_undo_status();
        app.update_focusable_order();
        assert_eq!(
            app.focusable_order.len(),
            1,
            "exactly the Undo control becomes focusable"
        );
    }

    /// The point of the whole API: a control drives the *same* undo the
    /// shortcut does.
    ///
    /// Two identical apps, one edit each. One is undone through
    /// `perform_undo` — the method the Ctrl+Z handler calls. The other is
    /// undone by an assistive technology activating the button, which produces
    /// a request the host drains. The documents must end up identical.
    #[test]
    fn activating_the_control_and_the_shortcut_reach_the_same_undo() {
        let mut via_shortcut = app_with_history_controls();
        type_action(&mut via_shortcut, "hello");
        let before = body(&via_shortcut);

        let mut via_control = app_with_history_controls();
        type_action(&mut via_control, "hello");
        assert_eq!(body(&via_control), before, "same starting point");

        // The shortcut's route.
        assert!(via_shortcut.perform_undo());

        // The control's route: publish, activate through the a11y dispatch
        // path, then the idle tick's drain.
        via_control.publish_undo_status();
        let undo_id = exported_id(&via_control, "Undo");
        via_control.dispatch_a11y_action(undo_id, &Action::Activate);
        assert!(
            via_control.drain_undo_requests(),
            "the activation must have produced an undo for the host to perform"
        );

        assert_eq!(
            body(&via_control),
            body(&via_shortcut),
            "the control and the shortcut leave the document in the same state"
        );
        assert_eq!(body(&via_control), "", "and that state is pre-edit");

        // Redo through the control lands where redo through the shortcut does.
        via_shortcut.perform_redo();
        via_control.publish_undo_status();
        let redo_id = exported_id(&via_control, "Redo");
        via_control.dispatch_a11y_action(redo_id, &Action::Activate);
        assert!(via_control.drain_undo_requests());
        assert_eq!(body(&via_control), body(&via_shortcut));
        assert_eq!(body(&via_control), "hello");
    }

    /// Every pending request in the live tree, drained the way the host does.
    fn pending_requests(app: &mut App) -> Vec<crate::undo::UndoRequest> {
        let mut out = Vec::new();
        for &w_ptr in app.node_map.values() {
            let w = unsafe { &mut *w_ptr };
            if let Some(req) = w.take_undo_request() {
                out.push(req);
            }
        }
        out
    }

    /// Activating a control the host has said is unavailable must do nothing —
    /// not silently undo something else.
    ///
    /// Asserted at the widget boundary, not only on the document: the host's
    /// own `redo_as` would decline an impossible verb anyway, so a test that
    /// checked only the document would pass with the control's refusal removed
    /// and prove nothing about it.
    #[test]
    fn activating_an_unavailable_control_produces_no_request_at_all() {
        let mut app = app_with_history_controls();
        type_action(&mut app, "hello");
        app.publish_undo_status();

        // Redo is unavailable: nothing has been undone.
        let redo_id = exported_id(&app, "Redo");
        app.dispatch_a11y_action(redo_id, &Action::Activate);
        assert_eq!(
            pending_requests(&mut app),
            Vec::new(),
            "an unavailable verb must not even reach the host as a request"
        );
        assert!(!app.drain_undo_requests());
        assert_eq!(body(&app), "hello", "and the document is untouched");

        // The same activation on the same control, once the verb is real,
        // does produce a request — so the assertion above is about
        // availability and not about the activation route being inert.
        assert!(app.perform_undo());
        app.publish_undo_status();
        let redo_id = exported_id(&app, "Redo");
        app.dispatch_a11y_action(redo_id, &Action::Activate);
        assert_eq!(
            pending_requests(&mut app),
            vec![crate::undo::UndoRequest::Redo]
        );
    }

    /// The status must reach a control nested behind a container that knows
    /// nothing about undo, and must reach one that only appeared this frame.
    #[test]
    fn the_status_reaches_controls_the_container_never_forwards_to() {
        let mut app = app_with_history_controls();
        type_action(&mut app, "hi");
        app.publish_undo_status();

        // Read it back off the live widgets, not off the App's cache.
        let mut seen = Vec::new();
        for &w_ptr in app.node_map.values() {
            let w = unsafe { &*w_ptr };
            if let Some(node) = w.accessibility() {
                if let Some(name) = node.name.as_deref() {
                    if name == "Undo" || name == "Redo" {
                        seen.push((name.to_string(), node.state.disabled));
                    }
                }
            }
        }
        seen.sort();
        assert_eq!(
            seen,
            vec![("Redo".to_string(), true), ("Undo".to_string(), false)],
            "both nested controls received the status Row never forwarded"
        );
    }

    /// Re-deriving the summaries reads `AutoCommit::diff`, which closes the
    /// open transaction. If the host did that on a frame where nothing had
    /// changed, a half-typed edit would be committed out from under the
    /// pending `commit_action` and its undo entry would never exist.
    #[test]
    fn publishing_between_keystrokes_does_not_split_the_pending_edit() {
        let mut app = app_with_history_controls();

        // A first action, so the history has something to describe. Without
        // one there is no span to diff and the frames below would touch the
        // document at all — the hazard only exists once a summary is derivable.
        type_action(&mut app, "hello");
        app.publish_undo_status();
        assert_eq!(
            exported(&app, "Undo").description().as_deref(),
            Some("Undo: +5 -0 chars"),
            "the status really is being derived, so a diff really would run"
        );

        // Now a second edit in progress: written, not yet committed — exactly
        // the state a text field is in between keystrokes.
        let obj = match app.state.doc.get(&ROOT, "body") {
            Ok(Some((_, obj))) => obj,
            _ => panic!("body must exist"),
        };
        app.state.doc.splice_text(&obj, 5, 0, " wor").unwrap();
        app.publish_undo_status();
        app.state.doc.splice_text(&obj, 9, 0, "ld").unwrap();
        app.publish_undo_status();

        // The widget signals the boundary, as a text field does.
        app.commit_action();
        assert_eq!(body(&app), "hello world");

        app.publish_undo_status();
        assert!(app.perform_undo(), "the second edit must be undoable");
        assert_eq!(
            body(&app),
            "hello",
            "one undo reverts the second edit and only that — a frame that \
             published between the keystrokes must not have committed part of \
             it as an untracked change"
        );
        assert!(app.perform_undo(), "the first edit is still undoable");
        assert_eq!(body(&app), "");
    }
}

/// The live host's half of the dispatched-action boundary, proved against the
/// same [`undo::record_dispatched_action`](crate::undo::record_dispatched_action)
/// the headless [`Driver`](crate::headless::Driver) calls.
///
/// The driver's tests prove a scenario sees the boundary. These prove the App
/// records it the same way, so an app cannot pass headlessly and then behave
/// differently in front of a user.
#[cfg(test)]
mod dispatched_action_tests {
    use super::*;
    use crate::state::State;
    use crate::widget::Widget;
    use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
    use qos_actions::{ActionError, Doc, ExecutionLocus};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use vello::kurbo::Rect;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Bump {
        to: i64,
    }

    const BUMP: &str = "test.bump";

    impl qos_actions::Action for Bump {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            doc.put(&ROOT, "count", ScalarValue::Int(self.to))?;
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            ExecutionLocus::ClientLocal
        }
    }

    /// An app in the migrated shape: its affordance dispatches a `qos_actions`
    /// action and reports the bracket. No `needs_commit` — that is the path this
    /// covers and the other module does not.
    #[derive(Default)]
    struct ActionApp {
        actions: crate::action_host::ActionHost,
        /// A bracket captured by an affordance that then dispatched nothing.
        armed: Option<Vec<automerge::ChangeHash>>,
    }

    impl ActionApp {
        /// What a toolbar button's click handler does.
        fn press(&mut self, doc: &mut automerge::AutoCommit, to: i64) {
            self.actions.dispatch(doc, BUMP, &Bump { to });
        }

        /// An affordance that captured the boundary and then changed nothing —
        /// a guarded control, or an action whose `apply` is a no-op against the
        /// document as it stands.
        fn arm_without_dispatching(&mut self, doc: &mut automerge::AutoCommit) {
            self.armed = Some(doc.get_heads());
        }
    }

    impl Widget for ActionApp {
        fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &crate::theme::Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let n = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(n, self as *mut dyn Widget);
            n
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: vello::kurbo::Point) {}
        fn draw(
            &mut self,
            _sc: &mut Scene,
            _tr: vello::kurbo::Affine,
            _tx: &mut crate::text_render::TextRenderer,
            _th: &crate::theme::Theme,
            _pr: &crate::presence::PresenceCtx,
        ) {
        }
        #[allow(clippy::too_many_arguments)]
        fn handle_event(
            &mut self,
            _e: &winit::event::WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut automerge::AutoCommit,
            _p: &automerge::ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _n: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &str {
            "ActionApp"
        }
        fn get_rect(&self) -> Rect {
            Rect::new(0.0, 0.0, 100.0, 40.0)
        }
        fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
            self.actions.take_committed().or_else(|| self.armed.take())
        }
    }

    fn count(app: &App) -> Option<i64> {
        app.state
            .doc
            .get(&ROOT, "count")
            .ok()
            .flatten()
            .and_then(|(v, _)| v.to_i64())
    }

    /// Press an affordance that dispatches an action, and Ctrl+Z reverts it.
    ///
    /// Both halves are the production ones: `record_dispatched_actions` is what
    /// the idle tick calls, and `perform_undo` is what the Ctrl+Z handler
    /// calls — including its author scoping, which is what an unattributed
    /// entry silently fails.
    #[test]
    fn an_action_dispatched_by_an_affordance_is_undoable_by_the_local_user() {
        let mut app = App::new(State::new(), Box::<ActionApp>::default());
        app.init_layout();

        // Reach the app widget the way the host does, and press its affordance.
        let root: *mut dyn Widget = app.root_widget.as_mut();
        unsafe { (*root.cast::<ActionApp>()).press(&mut app.state.doc, 7) };
        assert_eq!(count(&app), Some(7), "the action reached the document");

        assert!(
            app.record_dispatched_actions(),
            "the idle tick must turn the dispatch into a boundary"
        );
        app.publish_undo_status();
        assert!(
            app.undo_status().can_undo,
            "and the boundary must belong to the local user, or every Undo \
             control in the app paints dark over a real edit"
        );

        assert!(app.perform_undo(), "Ctrl+Z reverts the dispatched action");
        assert_eq!(count(&app), None, "the document is back where it started");
    }

    /// A dispatched action and a directly-committed edit land on one stack in
    /// the order they happened, and undo walks back through both.
    ///
    /// This is what author scoping breaks when a boundary is unattributed: the
    /// anonymous entry is skipped, so the first Ctrl+Z reaches *past* the newer
    /// action and reverts the older edit instead.
    #[test]
    fn undo_walks_dispatched_and_committed_edits_in_the_order_they_happened() {
        let mut app = App::new(State::new(), Box::<ActionApp>::default());
        app.init_layout();

        // An ordinary committed edit first.
        app.state
            .doc
            .put(&ROOT, "label", ScalarValue::Str("first".into()))
            .unwrap();
        app.commit_action();

        // Then a dispatched action.
        let root: *mut dyn Widget = app.root_widget.as_mut();
        unsafe { (*root.cast::<ActionApp>()).press(&mut app.state.doc, 7) };
        assert!(app.record_dispatched_actions());

        assert!(app.perform_undo(), "the newest entry is the action");
        assert_eq!(count(&app), None, "so the action is what came off first");
        assert_eq!(
            app.state
                .doc
                .get(&ROOT, "label")
                .ok()
                .flatten()
                .map(|(v, _)| v.to_string()),
            Some("\"first\"".to_string()),
            "the older committed edit is untouched by the first undo"
        );

        assert!(app.perform_undo(), "the committed edit is still undoable");
        assert!(app.state.doc.get(&ROOT, "label").unwrap().is_none());
    }

    /// A bracket whose actions turned out to change nothing is drained but not
    /// recorded, and the host is told so.
    ///
    /// The return value is not cosmetic: the idle tick reads it to decide
    /// whether to mark the document dirty for sync and ask for a repaint.
    /// Reporting a boundary that was never pushed makes the host broadcast and
    /// redraw for an edit that does not exist.
    #[test]
    fn a_bracket_whose_actions_changed_nothing_records_no_boundary() {
        let mut app = App::new(State::new(), Box::<ActionApp>::default());
        app.init_layout();

        // A real dispatched edit first, so there is a genuine entry that an
        // empty one could be confused with.
        let root: *mut dyn Widget = app.root_widget.as_mut();
        unsafe { (*root.cast::<ActionApp>()).press(&mut app.state.doc, 7) };
        assert!(app.record_dispatched_actions());

        // Now an affordance that bracketed and then changed nothing.
        unsafe { (*root.cast::<ActionApp>()).arm_without_dispatching(&mut app.state.doc) };
        assert!(
            !app.record_dispatched_actions(),
            "no document change means no boundary — and the host must not be \
             told to sync and repaint for one"
        );

        assert!(
            app.perform_undo(),
            "the real edit is still the top of the stack"
        );
        assert_eq!(count(&app), None);
        assert!(
            !app.perform_undo(),
            "and the empty bracket left no second entry behind"
        );
    }

    /// The idle tick is level-triggered and runs on every event. It must not
    /// push an entry when nothing was dispatched, or a Ctrl+Z would be
    /// swallowed by an empty boundary.
    #[test]
    fn a_tick_with_no_dispatch_records_nothing() {
        let mut app = App::new(State::new(), Box::<ActionApp>::default());
        app.init_layout();
        assert!(!app.record_dispatched_actions());

        let root: *mut dyn Widget = app.root_widget.as_mut();
        unsafe { (*root.cast::<ActionApp>()).press(&mut app.state.doc, 7) };
        assert!(app.record_dispatched_actions());
        // Rising edge: the following ticks find nothing left.
        assert!(!app.record_dispatched_actions());
        assert!(!app.record_dispatched_actions());

        assert!(app.perform_undo());
        assert_eq!(count(&app), None);
        assert!(
            !app.perform_undo(),
            "one dispatch left exactly one entry behind"
        );
    }
}
