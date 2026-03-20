use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Rect, Point};
use vello::peniko::Color;
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;

pub struct Button {
    id: String,
    label: String,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
    on_click_path: Option<String>, // Path in CRDT to mutate
}

impl Button {
    pub fn new(id: &str, label: &str, on_click_path: Option<&str>) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
            on_click_path: on_click_path.map(|s| s.to_string()),
        }
    }
}

impl Widget for Button {
    fn sync(&mut self, _state: &AutoCommit, _path: &ObjId) {
        // Button label could be synced from CRDT here
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree) -> NodeId {
        taffy.new_leaf(Style {
            size: Size {
                width: Dimension::length(120.0),
                height: Dimension::length(40.0),
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

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer) {
        let (bg_color, border_color) = if self.is_pressed {
            (Color::rgb8(35, 35, 40), Color::rgb8(60, 60, 65))
        } else if self.is_hovered {
            (Color::rgb8(55, 55, 60), Color::rgb8(90, 90, 95))
        } else {
            (Color::rgb8(45, 45, 50), Color::rgb8(80, 80, 85))
        };

        let shape = RoundedRect::from_rect(self.rect, 4.0); // Match text input rounding (4.0)
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            bg_color,
            None,
            &shape,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            border_color,
            None,
            &shape,
        );
        
        let text_size = 14.0;
        let text_x = self.rect.center().x - (self.label.len() as f64 * 4.0); // Naive centering
        let text_y = self.rect.y0 + (self.rect.height() - text_size as f64) / 2.0;
        
        text_renderer.draw_text(
            scene,
            &self.label,
            text_size,
            Color::WHITE,
            transform * Affine::translate((text_x, text_y))
        );
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
                if self.is_hovered {
                    let pressed = *element_state == ElementState::Pressed;
                    if pressed != self.is_pressed {
                        self.is_pressed = pressed;
                        *handled = true;

                        // CRDT Mutation
                        if !self.is_pressed {
                            if let Some(path) = &self.on_click_path {
                                // Super hacky CRDT mutation for the counter example
                                if let Ok(Some((val, _prop_id))) = state.get(root, path.as_str()) {
                                    if let Value::Scalar(s) = val {
                                        if let automerge::ScalarValue::Counter(_c) = s.as_ref() {
                                            let _ = state.increment(root, path.as_str(), 1);
                                        }
                                    }
                                } else {
                                    let _ = state.put(root, path.as_str(), automerge::ScalarValue::Counter(1.into()));
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
}
