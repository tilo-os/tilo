use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Rect, Point, Stroke, BezPath};
use vello::peniko::Color;
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};
use crate::permissions::PermissionScope;

pub struct Checkbox {
    _id: String,
    rect: Rect,
    is_hovered: bool,
    is_checked: bool,
    is_focused: bool,
    on_toggle_path: String,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// Optional override for `Widget::test_id()`. Defaults to `"checkbox"`.
    id_override: Option<String>,
}

impl Checkbox {
    pub fn new(id: &str, on_toggle_path: &str) -> Self {
        Self {
            _id: id.to_string(),
            rect: Rect::ZERO,
            is_hovered: false,
            is_checked: false,
            is_focused: false,
            on_toggle_path: on_toggle_path.to_string(),
            presence_path: Vec::new(),
            permission_scope: None,
            id_override: None,
        }
    }

    /// Opt this widget into presence tracking by binding it to a CRDT-style path.
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this widget behind a permission scope.
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Override the value returned from `Widget::test_id()`. Without a
    /// custom id the checkbox reports `"checkbox"`; callers with multiple
    /// checkboxes in the same surface can disambiguate them, e.g.
    /// `with_id("remember_me")`.
    pub fn with_id(mut self, id: &str) -> Self {
        self.id_override = Some(id.to_string());
        self
    }
}

impl Widget for Checkbox {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Ok(Some((val, _))) = state.get(root, self.on_toggle_path.as_str()) {
            if let Value::Scalar(s) = val {
                if let automerge::ScalarValue::Boolean(b) = s.as_ref() {
                    self.is_checked = *b;
                }
            }
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size {
                width: Dimension::length(24.0),
                height: Dimension::length(24.0),
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

    fn draw(&mut self, scene: &mut Scene, transform: Affine, _text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
        let box_color = if self.is_checked {
            // Checked = active fill, not hover — concept uses indigo-500
            // (`bg-indigo-500`) for the on-state and reserves indigo-600
            // for the hover shade.
            theme.accent_primary
        } else if self.is_hovered {
            theme.bg_hover
        } else {
            theme.bg_active
        };

        let box_shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);

        // Draw Box
        scene.fill(vello::peniko::Fill::NonZero, transform, box_color, None, &box_shape);

        let stroke_color = if self.is_focused {
            theme.border_focus
        } else {
            theme.tokens.colors.border_standard
        };

        scene.stroke(&Stroke::new(if self.is_focused { 2.0 } else { 1.0 }), transform, stroke_color, None, &box_shape);

        // Draw Checkmark
        if self.is_checked {
            let mut path = BezPath::new();
            path.move_to(Point::new(self.rect.x0 + 6.0, self.rect.y0 + 12.0));
            path.line_to(Point::new(self.rect.x0 + 10.0, self.rect.y0 + 16.0));
            path.line_to(Point::new(self.rect.x0 + 18.0, self.rect.y0 + 6.0));
            
            scene.stroke(
                &Stroke::new(2.5).with_caps(vello::kurbo::Cap::Round).with_join(vello::kurbo::Join::Round),
                transform,
                Color::WHITE,
                None,
                &path
            );
        }
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
                if self.is_hovered && *element_state == ElementState::Pressed {
                    *handled = true;
                    *focus_target = Some(node_id);
                    // CRDT Mutation
                    let new_val = !self.is_checked;
                    let _ = state.put(root, self.on_toggle_path.as_str(), automerge::ScalarValue::Boolean(new_val));
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "Checkbox" }

    fn test_id(&self) -> Option<&str> {
        Some(self.id_override.as_deref().unwrap_or("checkbox"))
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
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let overlay = Color::rgba8(0, 0, 0, 77);
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
    fn checkbox_test_id_default_is_checkbox() {
        let c = Checkbox::new("any", "path");
        assert_eq!(Widget::test_id(&c), Some("checkbox"));
    }

    #[test]
    fn checkbox_test_id_can_be_overridden() {
        let c = Checkbox::new("any", "path").with_id("remember_me");
        assert_eq!(Widget::test_id(&c), Some("remember_me"));
    }
}
