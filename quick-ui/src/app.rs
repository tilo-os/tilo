use winit::{
    event::{Event, WindowEvent},
    event_loop::EventLoop,
    window::Window,
};
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
        }
    }

    /// Set (or clear) the presence key under which local pointer moves
    /// are broadcast during the event loop. The shell calls this after
    /// resolving the active workspace so remote peers see the cursor
    /// attributed to the right scope. Passing `None` disables outbound
    /// pointer broadcast.
    pub fn set_presence_broadcast_scope(&mut self, key: Option<PresenceKey>) {
        self.presence_broadcast_scope = key;
        // Reset the throttle so a new scope's first pointer move fires
        // immediately rather than waiting on the stale last-send gate.
        self.last_pointer_broadcast = None;
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
        self.collab_manager
            .send(CollabRequest::Presence { key, payload });
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
                if let Ok(new_state) = crate::state::State::load(&data) {
                    self.state.doc = new_state.doc;
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

    pub fn run(mut self) {
        let event_loop = EventLoop::new().unwrap();
        let window_attributes = Window::default_attributes()
            .with_title("QuickUI App")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());
        let mut render_cx = RenderContext::new();
        let mut surface = pollster::block_on(render_cx.create_surface(window.clone(), window.inner_size().width, window.inner_size().height, vello::wgpu::PresentMode::AutoVsync)).unwrap();
        let device_handle = &render_cx.devices[surface.dev_id];
        surface.config.format = surface.config.format.add_srgb_suffix();
        surface.surface.configure(&device_handle.device, &surface.config);
        let mut renderer = Renderer::new(&device_handle.device, RendererOptions { surface_format: Some(surface.config.format), use_cpu: false, antialiasing_support: vello::AaSupport::all(), num_init_threads: None }).unwrap();
        let mut scene = Scene::new();

        self.node_map.clear();
        self.root_node = Some(self.root_widget.build_layout(&mut self.taffy, &self.theme, &mut self.node_map));
        self.update_focusable_order();
        
        self.last_stable_focus_idx = self.focused_node.and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.last_stable_metadata = self.focused_node.and_then(|id| {
            self.node_map.get(&id).and_then(|&w_ptr| unsafe { (*w_ptr).get_state() })
        });

        event_loop.run(move |event, elwt| {
            match event {
                Event::WindowEvent { event, window_id } if window_id == window.id() => {
                    match event {
                        WindowEvent::CloseRequested => elwt.exit(),
                        WindowEvent::Resized(size) => { render_cx.resize_surface(&mut surface, size.width, size.height); self.layout_dirty = true; window.request_redraw(); }
                        WindowEvent::RedrawRequested => {
                            let width = surface.config.width;
                            let height = surface.config.height;
                            scene.reset();
                            let scale_factor = window.scale_factor() as f32;

                            if self.needs_sync { self.root_widget.sync(&mut self.state.doc, &self.state.root); self.needs_sync = false; }

                            // Poll the tree for structural mutations that
                            // happened since the last frame (e.g. a tab
                            // split that inserted a brand-new WindowGroup
                            // into a GroupRow). If any subtree reports
                            // dirty, rebuild the taffy tree from scratch so
                            // the new widgets get node ids and the layout
                            // pass below sees them.
                            self.structure_dirty |= self.root_widget.take_structure_dirty();
                            if self.structure_dirty {
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
                            }

                            let root_node = self.root_node.unwrap();
                            if self.layout_dirty {
                                let node_map = &self.node_map;
                                let text_renderer = &mut self.text_renderer;
                                self.taffy.compute_layout_with_measure(root_node, taffy::Size { width: AvailableSpace::Definite((width as f32) / scale_factor), height: AvailableSpace::Definite((height as f32) / scale_factor) },
                                    |known, available, node_id, _, _| {
                                        if let Some(&widget_ptr) = node_map.get(&node_id) { unsafe { (*widget_ptr).measure(known, available, text_renderer) } }
                                        else { taffy::Size::ZERO }
                                    }).unwrap();
                                self.root_widget.apply_layout(&self.taffy, root_node, vello::kurbo::Point::new(0.0, 0.0));
                                self.layout_dirty = false;
                                self.update_focusable_order();
                            }
                            
                            let transform = Affine::scale(scale_factor as f64);
                            self.root_widget.draw_with_presence(
                                &mut scene,
                                transform,
                                &mut self.text_renderer,
                                &self.theme,
                                &self.presence_ctx,
                                &self.permission_ctx,
                            );
                            
                            // Draw Remote Cursors
                            for (_, cursor) in &self.state.collab.cursors {
                                let mut path = vello::kurbo::BezPath::new();
                                path.move_to(Point::new(0.0, 0.0));
                                path.line_to(Point::new(14.0, 14.0));
                                path.line_to(Point::new(6.0, 16.0));
                                path.line_to(Point::new(0.0, 22.0));
                                path.close_path();

                                let cursor_transform = transform * Affine::translate((cursor.x as f64, cursor.y as f64));
                                scene.fill(vello::peniko::Fill::NonZero, cursor_transform, cursor.color.clone(), None, &path);
                                scene.stroke(&vello::kurbo::Stroke::new(1.5), cursor_transform, Color::WHITE, None, &path);
                                
                                self.text_renderer.draw_text(&mut scene, &cursor.name, 10.0, Color::WHITE, cursor_transform * Affine::translate((10.0, 20.0)), None);
                            }

                            if self.debug_mode {
                                let debug_outline_cycle = [
                                    self.theme.tokens.colors.semantic.warning,
                                    self.theme.tokens.colors.semantic.info,
                                    self.theme.tokens.colors.semantic.success,
                                ];
                                fn draw_debug_recursive(widget: &dyn Widget, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, depth: usize, cycle: &[Color; 3]) {
                                    let rect = widget.get_rect();
                                    let color = cycle[depth % 3];
                                    scene.stroke(&vello::kurbo::Stroke::new(1.5), transform, color, None, &rect);
                                    let label = format!("{}: {}", widget.name(), depth);
                                    let text_y = (rect.y0 + 12.0).min(rect.y1);
                                    let bg_rect = KurboRect::new(rect.x0, rect.y0, rect.x0 + 100.0, rect.y0 + 14.0);
                                    scene.fill(vello::peniko::Fill::NonZero, transform, Color::rgba8(0,0,0,180), None, &bg_rect);
                                    text_renderer.draw_text(scene, &label, 10.0, color, transform * Affine::translate((rect.x0 + 2.0, text_y)), None);
                                    for child in widget.children() { draw_debug_recursive(child, scene, transform, text_renderer, depth + 1, cycle); }
                                }
                                draw_debug_recursive(self.root_widget.as_ref(), &mut scene, transform, &mut self.text_renderer, 0, &debug_outline_cycle);
                            }

                            let surface_texture = surface.surface.get_current_texture().expect("failed to get surface texture");
                            let device_handle = &render_cx.devices[surface.dev_id];
                            renderer.render_to_surface(&device_handle.device, &device_handle.queue, &scene, &surface_texture, &vello::RenderParams { base_color: self.theme.bg_app, width, height, antialiasing_method: vello::AaConfig::Area }).unwrap();
                            surface_texture.present();
                            device_handle.device.poll(vello::wgpu::Maintain::Poll);
                        }
                        WindowEvent::ModifiersChanged(modifiers) => { self.modifiers = modifiers; }
                        _ => {
                            // Outbound pointer-presence broadcast: if the
                            // shell has declared a presence scope, relay
                            // every pointer move (throttled to ~30 Hz) so
                            // peers can render this user's cursor on the
                            // same scope. CursorLeft clears the peer-side
                            // pointer via the `Left` payload so their
                            // cursor stops rendering when we wander out
                            // of the window.
                            if let Some(scope) = self.presence_broadcast_scope.clone() {
                                match &event {
                                    WindowEvent::CursorMoved { position, .. } => {
                                        let now = Instant::now();
                                        if pointer_broadcast_allowed(
                                            self.last_pointer_broadcast,
                                            now,
                                            Duration::from_millis(POINTER_BROADCAST_MIN_GAP_MS),
                                        ) {
                                            self.last_pointer_broadcast = Some(now);
                                            self.broadcast_presence(
                                                scope.clone(),
                                                PresencePayload::Pointer {
                                                    x: position.x as f32,
                                                    y: position.y as f32,
                                                },
                                            );
                                        }
                                    }
                                    WindowEvent::CursorLeft { .. } => {
                                        self.broadcast_presence(scope.clone(), PresencePayload::Left);
                                        self.last_pointer_broadcast = None;
                                    }
                                    _ => {}
                                }
                            }

                            let mut handled = false;
                            let mut focus_target = None;
                            let mut is_undo_redo = false;

                            if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::F12), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                self.debug_mode = !self.debug_mode; handled = true;
                            }

                            if !handled && self.modifiers.state().control_key() {
                                if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Character(ref c), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                    let shift = self.modifiers.state().shift_key();
                                    // Capture the *current* focus + widget metadata so the
                                    // opposite-stack entry carries where the caret is right
                                    // now — otherwise round-tripping undo/redo loses caret
                                    // position. `get_state()` is the non-mutating read half
                                    // of the get/set pair (default returns None, so widgets
                                    // that don't opt in are safe).
                                    let current_meta = self.root_widget.get_state();
                                    let current_focus_idx = self.focused_node.and_then(|id|
                                        self.focusable_order.iter().position(|x| *x == id)
                                    );
                                    match c.as_str() {
                                        // Ctrl+Shift+Z is the Linux/Mac-native redo binding
                                        // and winit delivers it as character "Z" while shift is
                                        // held, so check the modifier directly rather than
                                        // relying on the capital-letter cue.
                                        "z" | "Z" if shift => { if let Some(e) = self.state.redo(current_focus_idx, current_meta.clone()) { self.apply_undo_entry(e); } is_undo_redo = true; handled = true; }
                                        "z" | "Z" => { if let Some(e) = self.state.undo(current_focus_idx, current_meta.clone()) { self.apply_undo_entry(e); } is_undo_redo = true; handled = true; }
                                        "y" | "Y" => { if let Some(e) = self.state.redo(current_focus_idx, current_meta.clone()) { self.apply_undo_entry(e); } is_undo_redo = true; handled = true; }
                                        _ => {}
                                    }
                                }
                            }

                            if !handled {
                                if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                    let shift = self.modifiers.state().shift_key();
                                    if !self.focusable_order.is_empty() {
                                        let current_idx = self.focused_node.and_then(|id| self.focusable_order.iter().position(|&x| x == id));
                                        let next_idx = match current_idx {
                                            Some(idx) => if shift { if idx == 0 { self.focusable_order.len() - 1 } else { idx - 1 } } else { (idx + 1) % self.focusable_order.len() },
                                            None => 0,
                                        };
                                        self.set_focused_node(Some(self.focusable_order[next_idx])); handled = true;
                                    }
                                }
                            }

                            if !handled {
                                if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(logical_key), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                    let dir = match logical_key {
                                        winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up),
                                        winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                                        winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left),
                                        winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                                        _ => None,
                                    };
                                    if let (Some(d), true) = (dir, self.modifiers.state().alt_key()) { self.navigate_focus(d); handled = true; }
                                }
                            }

                            if !handled {
                                let scale_factor = window.scale_factor();
                                let root_node = self.root_node.unwrap();
                                self.root_widget.handle_event(&event, scale_factor, &self.modifiers, &mut self.state.doc, &self.state.root, &mut handled, &mut focus_target, root_node, &self.taffy);
                            }

                            if !handled {
                                if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(logical_key), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                    let dir = match logical_key {
                                        winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up),
                                        winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                                        winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left),
                                        winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                                        _ => None,
                                    };
                                    if let Some(d) = dir { self.navigate_focus(d); handled = true; }
                                }
                            }

                            if let Some(id) = focus_target { self.set_focused_node(Some(id)); handled = true; }

                            if handled {
                                let mut should_commit = !is_undo_redo;
                                if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Character(_), .. }, .. } = event { should_commit = false; }
                                if should_commit { self.commit_action(); }
                                self.needs_sync = true; self.layout_dirty = true; window.request_redraw();
                            }
                        }
                    }
                }
                Event::AboutToWait => {
                    // Poll collaboration events
                    for event in self.collab_manager.poll() {
                        match event {
                            CollabEvent::Context { collaborators, .. } => {
                                // Update collaborators
                                // We'll need a better way to map this, but for now:
                                for c in collaborators {
                                    self.state.collab.cursors.entry(c.id.clone()).or_insert(crate::state::RemoteCursor {
                                        x: 0.0, y: 0.0,
                                        color: Color::rgb(c.color[0] as f64, c.color[1] as f64, c.color[2] as f64),
                                        name: c.name.clone(),
                                    });
                                    // Seed the overlay-side presence so peer
                                    // labels come through with the server-
                                    // announced friendly name (later cursor
                                    // moves will preserve that label because
                                    // they pass display_name=None).
                                    self.presence_ctx.upsert_remote_cursor(
                                        c.id.clone(),
                                        [0.0, 0.0],
                                        Some(&c.name),
                                    );
                                }
                            }
                            CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                                if let Some(cursor) = self.state.collab.cursors.get_mut(&user_id) {
                                    cursor.x = x;
                                    cursor.y = y;
                                    window.request_redraw();
                                }
                                // Always mirror into PresenceCtx — a peer
                                // can send cursor moves before we've seen
                                // their Context entry, and the overlay still
                                // needs to paint them.
                                self.presence_ctx.upsert_remote_cursor(user_id, [x, y], None);
                            }
                            CollabEvent::CRDTUpdate { data } => {
                                if let Ok(new_state) = State::load(&data) {
                                    self.state.doc = new_state.doc;
                                    self.needs_sync = true;
                                    window.request_redraw();
                                }
                            }
                            CollabEvent::CRDTIncremental { data } => {
                                // Merge the incremental patch into the local
                                // CRDT. If the bytes are malformed we keep the
                                // local doc unchanged (best-effort) — a noisy
                                // peer can't brick our state.
                                let _ = self.state.load_incremental(&data);
                                self.needs_sync = true;
                                window.request_redraw();
                            }
                            CollabEvent::RemotePresenceUpdate { user_id, key, payload } => {
                                self.apply_remote_presence(user_id, key, payload);
                                window.request_redraw();
                            }
                        }
                    }

                    if self.root_widget.needs_commit() {
                        self.commit_action();
                        self.root_widget.on_commit();
                        self.needs_sync = true;
                        window.request_redraw();
                    }

                    // If a widget mutated its child composition between
                    // frames (e.g. GroupRow split a tab into a new group
                    // during the last event dispatch), flag the app so the
                    // next RedrawRequested rebuilds the taffy tree before
                    // laying out. We don't drain the flag here — the draw
                    // handler needs to see it — just pump a redraw so the
                    // rebuild happens promptly.
                    if self.root_widget.take_structure_dirty() {
                        self.structure_dirty = true;
                        window.request_redraw();
                    }

                    // Pump the animation loop: while any widget reports an
                    // in-flight frame-driven transition (e.g. the shell's
                    // overview crossfade), keep scheduling redraws so the
                    // animation progresses until it settles. Without this,
                    // a single post-click redraw renders the first eased
                    // frame at t≈0 and then stalls — the overview would
                    // appear to "not work" because the fade never plays.
                    if self.root_widget.needs_animation_frame() {
                        window.request_redraw();
                    }
                }
                _ => {}
            }
        }).unwrap();
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
