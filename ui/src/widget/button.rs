use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Rect, Point};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};
use crate::permissions::PermissionScope;
use crate::i18n;

pub struct Button {
    _id: String,
    label: String,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
    is_focused: bool,
    on_click_path: Option<String>, // Path in CRDT to mutate
    /// Optional imperative click handler, fired on release-while-hovered
    /// *in addition to* any `on_click_path` CRDT mutation. Lets app code
    /// that isn't modelling its control flow in the CRDT (e.g. the file
    /// explorer's command sink) react to a press without a message bus.
    on_click: Option<Box<dyn FnMut()>>,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// Scene cache. Holds the last emitted body / border / label
    /// primitives at *logical* coordinates; the App's transform
    /// is reapplied on `Scene::append`. See `widget::text` for the
    /// canonical pattern.
    cached: vello::Scene,
    dirty: bool,
    /// Rect the cache was emitted at — compared *by size* to the
    /// current rect; pure translation gets absorbed by the per-
    /// frame append transform without invalidating the cache.
    cache_rect: Rect,
    /// Visual-state snapshot the cache was emitted from. If any
    /// changes we invalidate the cache so the next draw rebuilds.
    last_state: (bool, bool, bool), // (hovered, pressed, focused)
}

impl Button {
    pub fn new(id: &str, label: &str, on_click_path: Option<&str>) -> Self {
        Self {
            _id: id.to_string(),
            label: label.to_string(),
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
            is_focused: false,
            on_click_path: on_click_path.map(|s| s.to_string()),
            on_click: None,
            presence_path: Vec::new(),
            permission_scope: None,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            last_state: (false, false, false),
        }
    }

    /// Opt this widget into presence tracking by binding it to a CRDT-style path.
    /// An empty path leaves presence opt-out (the default).
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this widget behind a permission scope. When the scope is denied
    /// the widget renders dimmed and rejects input.
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Attach an imperative click handler. Fires once per completed
    /// click (left-button release while the cursor is still over the
    /// button), in addition to any `on_click_path` CRDT mutation.
    pub fn on_click<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

impl Widget for Button {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        // Button label could be synced from CRDT here
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size {
                width: Dimension::length(120.0),
                height: Dimension::length(40.0),
            },
            ..Default::default()
        }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x as f64 + layout.location.x as f64,
            origin.y as f64 + layout.location.y as f64,
            origin.x as f64 + layout.location.x as f64 + layout.size.width as f64,
            origin.y as f64 + layout.location.y as f64 + layout.size.height as f64,
        );
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        if size_changed {
            self.dirty = true;
        }
    }

    fn draw(&mut self, out_scene: &mut Scene, out_transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
        let current_state = (self.is_hovered, self.is_pressed, self.is_focused);
        if current_state != self.last_state {
            self.dirty = true;
            self.last_state = current_state;
        }
        let needs_emit = self.dirty || self.cache_rect == Rect::ZERO;
        if !needs_emit {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            out_scene.append(&self.cached, Some(out_transform * Affine::translate((dx, dy))));
            return;
        }
        // Re-emit at current absolute coords. cache_rect tracks
        // *where* the cache was emitted so subsequent frames can
        // translate without re-encoding.
        self.cached.reset();
        let scene = &mut self.cached;
        let transform = Affine::IDENTITY;
        let (bg_color, border_color) = if self.is_pressed {
            (theme.bg_active, theme.tokens.colors.border_standard)
        } else if self.is_hovered {
            (theme.bg_hover, theme.tokens.colors.border_standard)
        } else {
            (theme.tokens.colors.surfaces.raised, theme.tokens.colors.border_standard)
        };

        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            bg_color,
            None,
            &shape,
        );

        let stroke_color = if self.is_focused {
            theme.border_focus
        } else {
            border_color
        };

        scene.stroke(
            &vello::kurbo::Stroke::new(if self.is_focused { 2.0 } else { 1.0 }),
            transform,
            stroke_color,
            None,
            &shape,
        );

        let text_size = 14.0;
        let label = i18n!(&self.label);
        let (text_width, _text_height) = text_renderer.measure(&label, text_size as f32, None);
        let text_x = self.rect.center().x - (text_width as f64 / 2.0);
        let text_y = self.rect.y0 + (self.rect.height() - text_size as f64) / 2.0;

        text_renderer.draw_text(
            scene,
            &label,
            text_size as f32,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((text_x, text_y)),
            None,
        );
        // End of cache build — splice the freshly-emitted local
        // scene into the outer App scene with the host transform.
        // delta = 0 here since cache_rect was just set to self.rect.
        self.cache_rect = self.rect;
        out_scene.append(&self.cached, Some(out_transform));
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || (self.is_hovered, self.is_pressed, self.is_focused) != self.last_state
    }

    fn mark_drawn(&mut self) { self.dirty = false; }

    /// Button has a fixed 120×40 intrinsic size that doesn't change
    /// between frames — its layout only needs to be computed once.
    /// The `Rect::ZERO` sentinel keeps the first frame's
    /// `compute_layout` running so `apply_layout` can populate the
    /// rect; after that it stays clean (pure translation handled
    /// via the cache's append transform, no taffy involvement).
    fn is_layout_dirty(&self) -> bool { self.cache_rect == Rect::ZERO }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, _modifiers: &winit::event::Modifiers, state: &mut AutoCommit, root: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, _taffy: &TaffyTree<()>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical_pos);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: element_state, button: MouseButton::Left, .. } => {
                if self.is_hovered {
                    let pressed = *element_state == ElementState::Pressed;
                    if pressed != self.is_pressed {
                        self.is_pressed = pressed;
                        *handled = true;

                        if self.is_pressed {
                            *focus_target = Some(node_id);
                        }

                        // CRDT Mutation
                        if !self.is_pressed {
                            if let Some(path) = &self.on_click_path {
                                match state.get(root, path.as_str()) {
                                    Ok(Some((Value::Scalar(s), _))) => {
                                        match s.as_ref() {
                                            automerge::ScalarValue::Counter(_) => {
                                                let _ = state.increment(root, path.as_str(), 1);
                                            }
                                            automerge::ScalarValue::Boolean(_) => {
                                                let _ = state.put(root, path.as_str(), true);
                                            }
                                            _ => {
                                                // Default to setting true if it's some other scalar
                                                let _ = state.put(root, path.as_str(), true);
                                            }
                                        }
                                    }
                                    _ => {
                                        // If it doesn't exist, we'll try to put a boolean true
                                        let _ = state.put(root, path.as_str(), true);
                                    }
                                }
                            }
                            // Imperative handler fires alongside the CRDT path.
                            if let Some(cb) = &mut self.on_click {
                                cb();
                            }
                        }
                    }
                } else if self.is_pressed {
                    self.is_pressed = false;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "Button" }

    fn test_id(&self) -> Option<&str> {
        Some(self.label.as_str())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget { path: self.presence_path.clone() })
        }
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) { return; }
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let overlay = vello::peniko::Color::from_rgba8(0, 0, 0, 77);
                scene.fill(vello::peniko::Fill::NonZero, transform, overlay, None, &self.rect);
            }
        }

        if let Some(key) = self.presence_key() {
            if let Some(peers) = presence.here.get(&key) {
                if let Some(user) = peers.first() {
                    presence.draw_peer_focus(scene, theme, user, self.rect);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Widget;

    #[test]
    fn button_test_id_returns_label() {
        let b = Button::new("id", "Save", None);
        assert_eq!(Widget::test_id(&b), Some("Save"));
    }

    #[test]
    fn on_click_callback_fires_on_release_over_button() {
        use std::cell::Cell;
        use std::rc::Rc;
        use winit::event::{DeviceId, ElementState, MouseButton};

        let fired = Rc::new(Cell::new(0u32));
        let mut b = {
            let fired = fired.clone();
            Button::new("id", "Go", None).on_click(move || fired.set(fired.get() + 1))
        };

        // Lay the button out so it has a non-zero hit rect.
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = b.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(200.0),
                height: taffy::AvailableSpace::Definite(60.0),
            },
        )
        .unwrap();
        b.apply_layout(&tree, node, Point::ZERO);

        let device: DeviceId = unsafe { std::mem::zeroed() };
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut dispatch = |b: &mut Button, ev: WindowEvent| {
            let mut handled = false;
            let mut focus = None;
            b.handle_event(&ev, 1.0, &mods, &mut doc, &root, &mut handled, &mut focus, node, &tree);
        };

        // Hover, press, release — the callback fires once on release.
        dispatch(&mut b, WindowEvent::CursorMoved { device_id: device, position: (10.0, 10.0).into() });
        dispatch(&mut b, WindowEvent::MouseInput { device_id: device, state: ElementState::Pressed, button: MouseButton::Left });
        assert_eq!(fired.get(), 0, "not fired until release");
        dispatch(&mut b, WindowEvent::MouseInput { device_id: device, state: ElementState::Released, button: MouseButton::Left });
        assert_eq!(fired.get(), 1, "fired once on release over the button");
    }
}
