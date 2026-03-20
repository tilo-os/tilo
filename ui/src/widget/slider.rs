use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Circle, Rect, Point};
use vello::peniko::Color;
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;

pub struct Slider {
    id: String,
    rect: Rect,
    is_hovered: bool,
    is_dragging: bool,
    value: f64, // 0.0 to 1.0
    on_change_path: String,
}

impl Slider {
    pub fn new(id: &str, on_change_path: &str) -> Self {
        Self {
            id: id.to_string(),
            rect: Rect::ZERO,
            is_hovered: false,
            is_dragging: false,
            value: 0.5,
            on_change_path: on_change_path.to_string(),
        }
    }
}

impl Widget for Slider {
    fn sync(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Ok(Some((val, _))) = state.get(root, self.on_change_path.as_str()) {
            if let Value::Scalar(s) = val {
                match s.as_ref() {
                    automerge::ScalarValue::F64(f) => self.value = *f,
                    automerge::ScalarValue::Int(i) => self.value = *i as f64,
                    automerge::ScalarValue::Uint(u) => self.value = *u as f64,
                    _ => {}
                }
            }
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree) -> NodeId {
        taffy.new_leaf(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(30.0),
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
        // Track
        let track_height = 6.0;
        let track_rect = Rect::new(
            self.rect.x0,
            self.rect.y0 + (self.rect.height() - track_height) / 2.0,
            self.rect.x1,
            self.rect.y0 + (self.rect.height() + track_height) / 2.0,
        );
        let track_shape = RoundedRect::from_rect(track_rect, 3.0);
        scene.fill(vello::peniko::Fill::NonZero, transform, Color::rgb8(45, 45, 50), None, &track_shape);
        scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, Color::rgb8(80, 80, 85), None, &track_shape);

        // Active Track (Blue part on the left)
        let active_track_rect = Rect::new(
            self.rect.x0,
            track_rect.y0,
            self.rect.x0 + self.value * self.rect.width(),
            track_rect.y1,
        );
        let active_track_shape = RoundedRect::from_rect(active_track_rect, 3.0);
        scene.fill(vello::peniko::Fill::NonZero, transform, Color::rgb8(0, 120, 215), None, &active_track_shape);

        // Knob
        let knob_radius = 10.0;
        let knob_x = self.rect.x0 + self.value * self.rect.width();
        let knob_y = self.rect.y0 + self.rect.height() / 2.0;
        let knob_shape = Circle::new(Point::new(knob_x, knob_y), knob_radius);
        
        let knob_color = if self.is_dragging {
            Color::rgb8(240, 240, 240)
        } else if self.is_hovered {
            Color::rgb8(220, 220, 220)
        } else {
            Color::rgb8(200, 200, 200)
        };
        
        scene.fill(vello::peniko::Fill::NonZero, transform, knob_color, None, &knob_shape);
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, state: &mut AutoCommit, root: &ObjId, handled: &mut bool) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.is_hovered = self.rect.contains(p);
                
                if self.is_dragging {
                    let mut new_value = (p.x - self.rect.x0) / self.rect.width();
                    new_value = new_value.clamp(0.0, 1.0);
                    if (new_value - self.value).abs() > 0.001 {
                        self.value = new_value;
                        let _ = state.put(root, self.on_change_path.as_str(), automerge::ScalarValue::F64(self.value));
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput { state: element_state, button: MouseButton::Left, .. } => {
                if self.is_hovered && *element_state == ElementState::Pressed {
                    self.is_dragging = true;
                    *handled = true;
                } else if *element_state == ElementState::Released {
                    self.is_dragging = false;
                }
            }
            _ => {}
        }
    }
}
