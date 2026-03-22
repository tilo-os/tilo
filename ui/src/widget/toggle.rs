use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Circle, Rect, Point, Stroke};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

pub struct Toggle {
    _id: String,
    rect: Rect,
    is_hovered: bool,
    is_checked: bool,
    is_focused: bool,
    on_toggle_path: String, // Path in CRDT to mutate
}

impl Toggle {
    pub fn new(id: &str, on_toggle_path: &str) -> Self {
        Self {
            _id: id.to_string(),
            rect: Rect::ZERO,
            is_hovered: false,
            is_checked: false,
            is_focused: false,
            on_toggle_path: on_toggle_path.to_string(),
        }
    }
}

impl Widget for Toggle {
    fn sync(&mut self, state: &AutoCommit, root: &ObjId) {
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
                width: Dimension::length(44.0),
                height: Dimension::length(24.0),
            },
            ..Default::default()
        }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x as f64 + layout.location.x as f64,
            origin.y as f64 + layout.location.y as f64,
            origin.x as f64 + layout.location.x as f64 + layout.size.width as f64,
            origin.y as f64 + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, _text_renderer: &mut TextRenderer, theme: &Theme) {
        let track_color = if self.is_checked {
            theme.status_success
        } else {
            theme.bg_active
        };

        let track_shape = RoundedRect::from_rect(self.rect, 12.0);
        scene.fill(vello::peniko::Fill::NonZero, transform, track_color, None, &track_shape);
        
        let stroke_color = if self.is_focused {
            theme.border_focus
        } else {
            theme.border_standard
        };
        
        scene.stroke(&Stroke::new(if self.is_focused { 2.0 } else { 1.0 }), transform, stroke_color, None, &track_shape);

        // Knob
        let knob_radius = 10.0;
        let knob_x = if self.is_checked {
            self.rect.x1 - knob_radius - 2.0
        } else {
            self.rect.x0 + knob_radius + 2.0
        };
        let knob_y = self.rect.y0 + self.rect.height() / 2.0;
        
        let knob_color = if self.is_hovered {
            theme.text_primary
        } else {
            theme.text_dim
        };
        
        let knob_shape = Circle::new(Point::new(knob_x, knob_y), knob_radius);
        scene.fill(vello::peniko::Fill::NonZero, transform, knob_color, None, &knob_shape);
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, state: &mut AutoCommit, root: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, _taffy: &TaffyTree<()>) {
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

    fn name(&self) -> &str { "Toggle" }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }
}
