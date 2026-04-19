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
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
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
            presence_path: Vec::new(),
            permission_scope: None,
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
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
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
    }

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
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);
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
