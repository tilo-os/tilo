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
    /// Optional leading vector glyph drawn instead of (icon-only) or
    /// before (icon + label) the text. Lets toolbars render crisp lucide
    /// icons without depending on a Nerd Font being installed.
    icon: Option<crate::icons::Icon>,
    /// Optional fixed (width, height) override; defaults to 120×40.
    size: Option<(f32, f32)>,
    on_click_path: Option<String>, // Path in CRDT to mutate
    /// Optional imperative click handler, fired on release-while-hovered
    /// *in addition to* any `on_click_path` CRDT mutation. Lets app code
    /// that isn't modelling its control flow in the CRDT (e.g. the file
    /// explorer's command sink) react to a press without a message bus.
    on_click: Option<Box<dyn FnMut()>>,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// When `false` the button is disabled: skipped by focus, rejects
    /// input (no `on_click`, no CRDT write, no hover/press feedback) and
    /// renders dimmed. Default `true`.
    enabled: bool,
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
            icon: None,
            size: None,
            on_click_path: on_click_path.map(|s| s.to_string()),
            on_click: None,
            presence_path: Vec::new(),
            permission_scope: None,
            enabled: true,
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

    /// Draw `icon` as a leading vector glyph. With an empty label the
    /// icon is centred (icon-only button); with a label it sits to its
    /// left.
    pub fn with_icon(mut self, icon: crate::icons::Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Override the default 120×40 intrinsic size — e.g. a compact 40×36
    /// square for a toolbar icon button.
    pub fn with_size(mut self, width: f32, height: f32) -> Self {
        self.size = Some((width, height));
        self
    }

    /// Set the enabled state at build time. A disabled button is skipped
    /// by focus, rejects clicks/keys and renders dimmed. Default `true`.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Imperatively flip the enabled state. Marks the scene cache dirty so
    /// the dimmed/normal look is re-emitted on the next frame.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.dirty = true;
        }
    }
}

impl Widget for Button {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        // Button label could be synced from CRDT here
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let (w, h) = self.size.unwrap_or((120.0, 40.0));
        let node = taffy.new_leaf(Style {
            flex_shrink: 0.0,
            size: Size {
                width: Dimension::length(w),
                height: Dimension::length(h),
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
            let inner = out_transform * Affine::translate((dx, dy));
            if self.enabled {
                out_scene.append(&self.cached, Some(inner));
            } else {
                let clip = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
                out_scene.push_layer(vello::peniko::Fill::NonZero, vello::peniko::BlendMode::default(), crate::widget::DISABLED_ALPHA, out_transform, &clip);
                out_scene.append(&self.cached, Some(inner));
                out_scene.pop_layer();
            }
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
        let icon_px = (self.rect.height() - 14.0).clamp(12.0, 22.0);
        match (self.icon, label.is_empty()) {
            (Some(icon), true) => {
                // Icon-only button — centre the glyph.
                let ix = self.rect.center().x - icon_px / 2.0;
                let iy = self.rect.center().y - icon_px / 2.0;
                crate::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(ix, iy, ix + icon_px, iy + icon_px),
                    icon,
                    theme.tokens.colors.text_primary,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            (icon, _) => {
                // Optional leading icon + centred label cluster.
                let (text_width, _) = text_renderer.measure(&label, text_size as f32, None);
                let gap = if icon.is_some() { 6.0 } else { 0.0 };
                let icon_w = if icon.is_some() { icon_px } else { 0.0 };
                let cluster_w = icon_w + gap + text_width as f64;
                let start_x = self.rect.center().x - cluster_w / 2.0;
                if let Some(icon) = icon {
                    let iy = self.rect.center().y - icon_px / 2.0;
                    crate::icons::draw_icon(
                        scene,
                        transform,
                        Rect::new(start_x, iy, start_x + icon_px, iy + icon_px),
                        icon,
                        theme.tokens.colors.text_primary,
                        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                let text_x = start_x + icon_w + gap;
                let text_y = self.rect.y0 + (self.rect.height() - text_size as f64) / 2.0;
                text_renderer.draw_text(
                    scene,
                    &label,
                    text_size as f32,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((text_x, text_y)),
                    None,
                );
            }
        }
        // End of cache build — splice the freshly-emitted local
        // scene into the outer App scene with the host transform.
        // delta = 0 here since cache_rect was just set to self.rect.
        self.cache_rect = self.rect;
        if self.enabled {
            out_scene.append(&self.cached, Some(out_transform));
        } else {
            let clip = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
            out_scene.push_layer(vello::peniko::Fill::NonZero, vello::peniko::BlendMode::default(), crate::widget::DISABLED_ALPHA, out_transform, &clip);
            out_scene.append(&self.cached, Some(out_transform));
            out_scene.pop_layer();
        }
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
        // Disabled: reject all input — no hover/press feedback, no CRDT
        // write, no callback. Return without setting `handled`.
        if !self.enabled { return; }
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

    /// A button is a clickable affordance, so the pointer-finger cursor
    /// while hovered. `_point` is in the same logical space as `rect`.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }
    fn is_enabled(&self) -> bool { self.enabled }

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
                let overlay = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
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
    fn cursor_shape_is_pointer_over_button() {
        // Lay the button out so it has a non-zero hit rect, then assert the
        // pointer-finger cursor is claimed over it and deferred elsewhere.
        let mut b = Button::new("id", "Go", None);
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

        assert_eq!(
            Widget::cursor_shape(&b, Point::new(10.0, 10.0)),
            Some(winit::window::CursorIcon::Pointer),
            "pointer over the button requests the pointing-hand cursor",
        );
        assert_eq!(
            Widget::cursor_shape(&b, Point::new(500.0, 500.0)),
            None,
            "pointer well outside the button defers (no cursor claim)",
        );
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

    #[test]
    fn with_enabled_false_is_reported_by_is_enabled() {
        let b = Button::new("id", "Go", None).with_enabled(false);
        assert!(!Widget::is_enabled(&b));
        let b2 = Button::new("id", "Go", None);
        assert!(Widget::is_enabled(&b2), "default is enabled");
    }

    #[test]
    fn disabled_button_rejects_input() {
        use std::cell::Cell;
        use std::rc::Rc;
        use winit::event::{DeviceId, ElementState, MouseButton};

        let fired = Rc::new(Cell::new(0u32));
        let mut b = {
            let fired = fired.clone();
            Button::new("id", "Go", Some("clicked"))
                .on_click(move || fired.set(fired.get() + 1))
                .with_enabled(false)
        };

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
        let mut focus = None;
        let mut handled = false;

        for ev in [
            WindowEvent::CursorMoved { device_id: device, position: (10.0, 10.0).into() },
            WindowEvent::MouseInput { device_id: device, state: ElementState::Pressed, button: MouseButton::Left },
            WindowEvent::MouseInput { device_id: device, state: ElementState::Released, button: MouseButton::Left },
        ] {
            b.handle_event(&ev, 1.0, &mods, &mut doc, &root, &mut handled, &mut focus, node, &tree);
        }

        assert_eq!(fired.get(), 0, "disabled button must not fire on_click");
        assert!(focus.is_none(), "disabled button must not request focus");
        assert!(!handled, "disabled button must not swallow input");
        assert!(
            doc.get(&root, "clicked").unwrap().is_none(),
            "disabled button must not write the CRDT path"
        );
    }
}
