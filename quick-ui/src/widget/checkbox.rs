use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Rect, Point, Stroke, BezPath};
use vello::peniko::Color;
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;

pub struct Checkbox {
    _id: String,
    rect: Rect,
    is_hovered: bool,
    is_checked: bool,
    on_toggle_path: String,
}

impl Checkbox {
    pub fn new(id: &str, on_toggle_path: &str) -> Self {
        Self {
            _id: id.to_string(),
            rect: Rect::ZERO,
            is_hovered: false,
            is_checked: false,
            on_toggle_path: on_toggle_path.to_string(),
        }
    }
}

impl Widget for Checkbox {
    fn sync(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Ok(Some((val, _))) = state.get(root, self.on_toggle_path.as_str()) {
            if let Value::Scalar(s) = val {
                if let automerge::ScalarValue::Boolean(b) = s.as_ref() {
                    self.is_checked = *b;
                }
            }
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree) -> NodeId {
        taffy.new_leaf(Style {
            size: Size {
                width: Dimension::length(24.0),
                height: Dimension::length(24.0),
            },
            ..Default::default()
        }).unwrap()
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

    fn draw(&mut self, scene: &mut Scene, transform: Affine, _text_renderer: &mut TextRenderer) {
        let box_color = if self.is_checked {
            Color::rgb8(0, 120, 215)
        } else if self.is_hovered {
            Color::rgb8(50, 50, 55)
        } else {
            Color::rgb8(35, 35, 40)
        };

        let box_shape = RoundedRect::from_rect(self.rect, 6.0);
        
        // Draw Box
        scene.fill(vello::peniko::Fill::NonZero, transform, box_color, None, &box_shape);
        scene.stroke(&Stroke::new(1.0), transform, Color::rgb8(80, 80, 85), None, &box_shape);

        // Draw Checkmark
        if self.is_checked {
            let mut path = BezPath::new();
            path.move_to(Point::new(self.rect.x0 + 6.0, self.rect.y0 + 12.0));
            path.line_to(Point::new(self.rect.x0 + 10.0, self.rect.y0 + 16.0));
            path.line_to(Point::new(self.rect.x0 + 18.0, self.rect.y0 + 6.0));
            
            scene.stroke(
                &Stroke::new(2.5).with_caps(vello::kurbo::Cap::Round).with_join(vello::kurbo::Join::Round),
                transform,
                Color::rgb8(255, 255, 255),
                None,
                &path
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, state: &mut AutoCommit, root: &ObjId, handled: &mut bool) {
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
                    // CRDT Mutation
                    let new_val = !self.is_checked;
                    let _ = state.put(root, self.on_toggle_path.as_str(), automerge::ScalarValue::Boolean(new_val));
                }
            }
            _ => {}
        }
    }
}