use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
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
    pub theme: Theme,
    pub debug_mode: bool,
    pub focused_node: Option<NodeId>,
    pub focusable_order: Vec<NodeId>,
    modifiers: winit::event::Modifiers,
    
    // Stable state for Undo/Redo tracking
    last_stable_focus_idx: Option<usize>,
    last_stable_metadata: Option<String>,

    // Collaboration
    collab_manager: CollaborationManager,
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
}

impl App {
    pub fn new(state: State, root_widget: Box<dyn Widget>) -> Self {
        Self {
            state,
            root_widget,
            text_renderer: TextRenderer::new(),
            taffy: TaffyTree::new(),
            root_node: None,
            node_map: std::collections::HashMap::new(),
            layout_dirty: true,
            structure_dirty: false,
            needs_sync: true,
            theme: Theme::dark(),
            debug_mode: std::env::var("QUICK_UI_DEBUG").is_ok(),
            focused_node: None,
            focusable_order: Vec::new(),
            modifiers: winit::event::Modifiers::default(),
            last_stable_focus_idx: None,
            last_stable_metadata: None,
            collab_manager: CollaborationManager::new(),
            presence_ctx: PresenceCtx::empty(),
            permission_ctx: PermissionContext::permissive(),
            presence_broadcast_scope: None,
            last_pointer_broadcast: None,
            last_focus_presence: None,
            last_location_presence: None,
            presence_broadcast_sink: None,
            frame_tick: None,
        }
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

    /// Poll the focused widget for its caret/selection presence and
    /// broadcast it when it changes since the last send, so peers see the
    /// local caret/selection move in near-real-time. The focused widget
    /// supplies both the key (which sub-element the caret is in) and the
    /// payload via [`Widget::presence_payload`].
    fn broadcast_focus_presence(&mut self) {
        let current = self
            .focused_node
            .and_then(|id| self.node_map.get(&id).copied())
            .and_then(|w| unsafe { (*w).presence_payload() });
        if current != self.last_focus_presence {
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
            match event {
                CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                    if let Some(cursor) = self.state.collab.cursors.get_mut(&user_id) {
                        cursor.x = x;
                        cursor.y = y;
                    }
                    // Mirror the update into PresenceCtx so the shell's
                    // RemoteCursorOverlay (which reads from
                    // `presence_ctx.remote_cursors`, not the legacy
                    // `state.collab.cursors` map) lights up too.
                    self.presence_ctx.upsert_remote_cursor(user_id, [x, y], None);
                }
                CollabEvent::RemotePresenceUpdate { user_id, key, payload } => {
                    self.apply_remote_presence(user_id, key, payload);
                }
                _ => {}
            }
        }
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
            if widget.is_focusable() { order.push(node_id); }
            let children = widget.children();
            if let Ok(child_nodes) = taffy.children(node_id) {
                for (child_widget, &child_node) in children.iter().zip(child_nodes.iter()) {
                    collect_focusable(child_node, *child_widget, taffy, order);
                }
            }
        }
        if let Some(root) = self.root_node {
            collect_focusable(root, self.root_widget.as_ref(), &self.taffy, &mut self.focusable_order);
        }
    }

    fn apply_remote_presence(
        &mut self,
        user_id: String,
        key: crate::presence::PresenceKey,
        payload: crate::collaboration::PresencePayload,
    ) {
        use crate::collaboration::PresencePayload::*;
        use crate::permissions::UserId;
        let uid = UserId(user_id);
        match payload {
            Pointer { x, y } => self.presence_ctx.set_pointer(&key, uid, (x, y)),
            Caret { pos } => self.presence_ctx.set_caret(&key, uid, pos),
            Selection { start, end } => self.presence_ctx.set_selection(
                &key,
                uid,
                crate::presence::SelectionRange { start, end },
            ),
            Arrived => {
                // For Tab keys, route through set_tab_focus so a peer
                // switching tabs moves their dot instead of accumulating
                // attribution on every tab they've ever visited. Non-tab
                // keys fall through to plain register_presence.
                if matches!(key, crate::presence::PresenceKey::Tab { .. }) {
                    self.presence_ctx.set_tab_focus(&key, uid);
                } else {
                    self.presence_ctx.register_presence(&key, uid);
                    // A non-Tab (app/workspace-level) arrival is a peer
                    // announcing themselves — possibly a late joiner with
                    // an empty document. Reply with our full state so they
                    // converge. The broker fans this to all peers; an
                    // already-synced peer merges it idempotently. (Page
                    // switches use Tab keys and never trigger this.)
                    let data = self.state.save();
                    self.collab_manager
                        .send(crate::collaboration::CollabRequest::SyncCRDT { data });
                }
            }
            Left => self.presence_ctx.remove_presence(&key, &uid),
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
        self.state.commit_action(self.last_stable_focus_idx, self.last_stable_metadata.clone());
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
                }
            }
            self.layout_dirty = true;
            self.needs_sync = true;
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
        setup(proxy);

        // Build the taffy tree up-front so widgets that don't depend on a
        // live window (focus order, undo bookkeeping) are wired before the
        // first `resumed` callback.
        self.node_map.clear();
        self.root_node = Some(self.root_widget.build_layout(
            &mut self.taffy,
            &self.theme,
            &mut self.node_map,
        ));
        self.update_focusable_order();
        self.last_stable_focus_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.last_stable_metadata = self.focused_node.and_then(|id| {
            self.node_map
                .get(&id)
                .and_then(|&w_ptr| unsafe { (*w_ptr).get_state() })
        });

        let mut handler = AppHandler {
            app: self,
            window: None,
            render: None,
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
        }
    }


    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // `resumed` can fire more than once (suspend/resume on mobile);
        // after the first init the window/render state stays valid, so
        // subsequent calls are no-ops.
        if self.window.is_some() {
            return;
        }
        let window_attributes = Window::default_attributes()
            .with_title("QuickUI App")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());

        let mut render_cx = RenderContext::new();
        let surface = pollster::block_on(render_cx.create_surface(
            window.clone(),
            window.inner_size().width,
            window.inner_size().height,
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

        // Mark the freshly-built taffy tree dirty so the very first
        // `RedrawRequested` re-runs layout against the actual window
        // size and scale factor.
        self.app.layout_dirty = true;
        window.request_redraw();

        self.window = Some(window);
        // Hand the wgpu state off to the render worker. After this
        // point, the main thread doesn't touch the surface, renderer,
        // or render_cx — every GPU call happens on the `ui-render`
        // thread.
        self.render = Some(spawn_render_thread(render_cx, surface, renderer));
    }

    /// Winit calls this once on event-loop teardown. We use it to
    /// shut the render thread down BEFORE the `Window` arc is
    /// dropped — the worker's `RenderSurface<'static>` references the
    /// window, and wgpu may panic if the surface outlives the window.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
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
            WindowEvent::RedrawRequested => {
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

                if app.needs_sync {
                    app.root_widget.sync(&mut app.state.doc, &app.state.root);
                    app.needs_sync = false;
                }

                // Poll the tree for structural mutations that
                // happened since the last frame (e.g. a tab
                // split that inserted a brand-new WindowGroup
                // into a GroupRow). If any subtree reports
                // dirty, rebuild the taffy tree from scratch so
                // the new widgets get node ids and the layout
                // pass below sees them.
                app.structure_dirty |= app.root_widget.take_structure_dirty();
                if app.structure_dirty {
                    app.taffy = TaffyTree::new();
                    app.node_map.clear();
                    app.root_node = Some(app.root_widget.build_layout(
                        &mut app.taffy,
                        &app.theme,
                        &mut app.node_map,
                    ));
                    app.structure_dirty = false;
                    app.layout_dirty = true;
                    app.update_focusable_order();
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
                    let node_map = &app.node_map;
                    let text_renderer = &mut app.text_renderer;
                    let t0 = std::time::Instant::now();
                    app.taffy
                        .compute_layout_with_measure(
                            root_node,
                            taffy::Size {
                                width: AvailableSpace::Definite((width as f32) / scale_factor),
                                height: AvailableSpace::Definite((height as f32) / scale_factor),
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
                    let t1 = std::time::Instant::now();
                    app.root_widget.apply_layout(
                        &app.taffy,
                        root_node,
                        vello::kurbo::Point::new(0.0, 0.0),
                    );
                    t_apply_us = t1.elapsed().as_micros();
                    app.layout_dirty = false;
                    app.update_focusable_order();
                }

                let t_draw_start = if perf { Some(std::time::Instant::now()) } else { None };
                let transform = Affine::scale(scale_factor as f64);
                // Initialise the spatial-culling viewport to the
                // entire window in *logical* coords (the App's
                // transform applies a scale_factor at composite
                // time, so widget rects live in logical space).
                // Scrollables push a tighter inner viewport during
                // their child recursion. Restored to `None` after
                // draw so the bench's offscreen renderer paths
                // aren't accidentally bound to a stale viewport.
                let window_rect = vello::kurbo::Rect::new(
                    0.0,
                    0.0,
                    (width as f64) / (scale_factor as f64),
                    (height as f64) / (scale_factor as f64),
                );
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
                let t_draw_us = t_draw_start.map(|t| t.elapsed().as_micros()).unwrap_or(0);

                // Draw Remote Cursors
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

                // Draw remote pointers delivered over the app collab
                // socket (the broker path): these land in
                // `PresenceCtx::pointers` keyed by the broadcast scope,
                // NOT the legacy `state.collab.cursors` map. Render each
                // as a round, color-coded cursor with a name label.
                for peers in app.presence_ctx.pointers.values() {
                    for (uid, pos) in peers {
                        app.presence_ctx.draw_peer_pointer(
                            &mut scene,
                            &app.theme,
                            transform,
                            uid,
                            *pos,
                        );
                        app.text_renderer.draw_text(
                            &mut scene,
                            &uid.0,
                            10.0,
                            Color::WHITE,
                            transform
                                * Affine::translate((
                                    pos.0 as f64 + 11.0,
                                    pos.1 as f64 + 4.0,
                                )),
                            None,
                        );
                    }
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
                    eprintln!(
                        "perf frame: main_total={:>5}us layout={}({}+{}us) draw={:>5}us send_to_worker={:>5}us",
                        t_send_us,
                        if t_relayout_branch { "Y" } else { "n" },
                        t_compute_us,
                        t_apply_us,
                        t_draw_us,
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
                                if let Some(e) =
                                    app.state.redo(current_focus_idx, current_meta.clone())
                                {
                                    app.apply_undo_entry(e);
                                }
                                is_undo_redo = true;
                                handled = true;
                            }
                            "z" | "Z" => {
                                if let Some(e) =
                                    app.state.undo(current_focus_idx, current_meta.clone())
                                {
                                    app.apply_undo_entry(e);
                                }
                                is_undo_redo = true;
                                handled = true;
                            }
                            "y" | "Y" => {
                                if let Some(e) =
                                    app.state.redo(current_focus_idx, current_meta.clone())
                                {
                                    app.apply_undo_entry(e);
                                }
                                is_undo_redo = true;
                                handled = true;
                            }
                            _ => {}
                        }
                    }
                }

                if !handled {
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
                        if let (Some(d), true) = (dir, app.modifiers.state().alt_key()) {
                            app.navigate_focus(d);
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
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.as_ref().cloned() else {
            return;
        };
        let app = &mut self.app;
        // Poll collaboration events
        for event in app.collab_manager.poll() {
            match event {
                CollabEvent::Context { collaborators, .. } => {
                    // Update collaborators
                    // We'll need a better way to map this, but for now:
                    for c in collaborators {
                        app.state.collab.cursors.entry(c.id.clone()).or_insert(
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
                        // announced friendly name (later cursor
                        // moves will preserve that label because
                        // they pass display_name=None).
                        app.presence_ctx.upsert_remote_cursor(
                            c.id.clone(),
                            [0.0, 0.0],
                            Some(&c.name),
                        );
                    }
                }
                CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                    if let Some(cursor) = app.state.collab.cursors.get_mut(&user_id) {
                        cursor.x = x;
                        cursor.y = y;
                        window.request_redraw();
                    }
                    // Always mirror into PresenceCtx — a peer
                    // can send cursor moves before we've seen
                    // their Context entry, and the overlay still
                    // needs to paint them.
                    app.presence_ctx.upsert_remote_cursor(user_id, [x, y], None);
                }
                CollabEvent::CRDTUpdate { data } => {
                    // MERGE incoming full state into our doc rather than
                    // replacing it — a replace would clobber our own
                    // concurrent edits. Merge is the correct CRDT join and
                    // is what makes bidirectional editing converge.
                    if let Ok(mut incoming) = State::load(&data) {
                        let _ = app.state.doc.merge(&mut incoming.doc);
                        app.needs_sync = true;
                        window.request_redraw();
                    }
                }
                CollabEvent::CRDTIncremental { data } => {
                    // Merge the incremental patch into the local
                    // CRDT. If the bytes are malformed we keep the
                    // local doc unchanged (best-effort) — a noisy
                    // peer can't brick our state.
                    let _ = app.state.load_incremental(&data);
                    app.needs_sync = true;
                    window.request_redraw();
                }
                CollabEvent::RemotePresenceUpdate {
                    user_id,
                    key,
                    payload,
                } => {
                    app.apply_remote_presence(user_id, key, payload);
                    window.request_redraw();
                }
            }
        }

        if app.root_widget.needs_commit() {
            app.commit_action();
            app.root_widget.on_commit();
            app.needs_sync = true;
            window.request_redraw();
        }

        // Re-broadcast location presence here too: the active page can
        // change during `sync` (e.g. the initial auto-select), which the
        // post-event broadcast above wouldn't catch. Diffed, so it only
        // sends on an actual change.
        app.broadcast_location_presence();

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

        // Pump the animation loop: while any widget reports an
        // in-flight frame-driven transition (e.g. the shell's
        // overview crossfade), keep scheduling redraws so the
        // animation progresses until it settles. Without this,
        // a single post-click redraw renders the first eased
        // frame at t≈0 and then stalls — the overview would
        // appear to "not work" because the fade never plays.
        if app.root_widget.needs_animation_frame() {
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
