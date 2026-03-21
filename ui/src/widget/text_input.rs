use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable, ObjType};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Rect, Point, Stroke};
use vello::peniko::{BlendMode};
use winit::event::{WindowEvent, MouseButton, ElementState, KeyEvent};
use winit::keyboard::{Key, NamedKey};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

pub struct TextInput {
    _id: String,
    rect: Rect,
    is_hovered: bool,
    is_focused: bool,
    text: String,
    cursor_pos: usize,
    on_change_path: String,
    last_mouse_pos: Point,
    char_x_positions: Vec<f64>,
    scroll_x: f64,
}

impl TextInput {
    pub fn new(id: &str, on_change_path: &str) -> Self {
        Self {
            _id: id.to_string(),
            rect: Rect::ZERO,
            is_hovered: false,
            is_focused: false,
            text: String::new(),
            cursor_pos: 0,
            on_change_path: on_change_path.to_string(),
            last_mouse_pos: Point::ZERO,
            char_x_positions: Vec::new(),
            scroll_x: 0.0,
        }
    }
}

impl Widget for TextInput {
    fn sync(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Ok(Some((val, obj_id))) = state.get(root, self.on_change_path.as_str()) {
            match val {
                Value::Scalar(s) => {
                    if let automerge::ScalarValue::Str(text) = s.as_ref() {
                        self.text = text.to_string();
                    }
                }
                Value::Object(ObjType::Text) => {
                    if let Ok(text) = state.text(&obj_id) {
                        self.text = text;
                    }
                }
                _ => {}
            }
        }
        if self.cursor_pos > self.text.len() {
            self.cursor_pos = self.text.len();
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree, _theme: &Theme) -> NodeId {
        taffy.new_leaf(Style {
            size: Size {
                width: Dimension::percent(1.0),
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

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme) {
        let bg_color = if self.is_focused {
            theme.bg_active
        } else {
            theme.bg_app
        };

        let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
        scene.fill(vello::peniko::Fill::NonZero, transform, bg_color, None, &shape);
        
        let border_color = if self.is_focused {
            theme.border_focus
        } else if self.is_hovered {
            theme.border_standard
        } else {
            theme.border_standard // Consider theme.border_dim if added
        };
        scene.stroke(&Stroke::new(1.0), transform, border_color, None, &shape);

        let text_size = 16.0;
        let padding_x = theme.padding_standard as f64;

        // Pre-calculate positions for exact caret placement and click resolution
        self.char_x_positions.clear();
        self.char_x_positions.resize(self.text.len() + 1, 0.0);
        for (byte_idx, _) in self.text.char_indices() {
            self.char_x_positions[byte_idx] = text_renderer.measure_width(&self.text[0..byte_idx], text_size) as f64;
        }
        self.char_x_positions[self.text.len()] = text_renderer.measure_width(&self.text, text_size) as f64;

        // Cursor
        let cursor_pos = self.cursor_pos.min(self.text.len());
        let cursor_x_offset = if cursor_pos < self.char_x_positions.len() && self.text.is_char_boundary(cursor_pos) {
            self.char_x_positions[cursor_pos]
        } else {
            text_renderer.measure_width(&self.text, text_size) as f64
        };

        // Adjust scroll
        let visible_width = self.rect.width() - padding_x * 2.0;
        if cursor_x_offset - self.scroll_x < 0.0 {
            self.scroll_x = cursor_x_offset;
        } else if cursor_x_offset - self.scroll_x > visible_width {
            self.scroll_x = cursor_x_offset - visible_width;
        }
        
        // Clip text drawing to the input box shape
        scene.push_layer(BlendMode::default(), 1.0, transform, &shape);

        let text_y = self.rect.y0 + (self.rect.height() - text_size as f64) / 2.0;
        
        text_renderer.draw_text(
            scene,
            &self.text,
            text_size,
            theme.text_primary,
            transform * Affine::translate((self.rect.x0 + padding_x - self.scroll_x, text_y))
        );

        if self.is_focused {
            let cursor_x = self.rect.x0 + padding_x + cursor_x_offset - self.scroll_x;
            let cursor_rect = Rect::new(
                cursor_x,
                self.rect.y0 + (self.rect.height() - text_size as f64) / 2.0,
                cursor_x + 2.0,
                self.rect.y0 + (self.rect.height() + text_size as f64) / 2.0
            );
            scene.fill(vello::peniko::Fill::NonZero, transform, theme.accent_primary, None, &cursor_rect);
        }

        scene.pop_layer();
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, state: &mut AutoCommit, root: &ObjId, handled: &mut bool) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.is_hovered = self.rect.contains(logical_pos);
                self.last_mouse_pos = logical_pos;
            }
            WindowEvent::MouseInput { state: element_state, button: MouseButton::Left, .. } => {
                if self.is_hovered && *element_state == ElementState::Pressed {
                    self.is_focused = true;
                    
                    // Update cursor pos based on click using exact calculated widths and scrolling
                    let padding_x = 10.0;
                    let relative_x = self.last_mouse_pos.x - (self.rect.x0 + padding_x) + self.scroll_x;
                    
                    let mut best_dist = f64::MAX;
                    let mut best_idx = 0;
                    
                    for (i, &x_pos) in self.char_x_positions.iter().enumerate() {
                        if i == self.text.len() || self.text.is_char_boundary(i) {
                            let dist = (relative_x - x_pos).abs();
                            if dist < best_dist {
                                best_dist = dist;
                                best_idx = i;
                            }
                        }
                    }
                    
                    self.cursor_pos = best_idx;
                    *handled = true;
                } else if *element_state == ElementState::Pressed {
                    self.is_focused = false;
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, text, state: element_state, .. }, .. } => {
                if self.is_focused && *element_state == ElementState::Pressed {
                    // Resolve text object ID
                    if let Ok(Some((Value::Object(ObjType::Text), obj_id))) = state.get(root, self.on_change_path.as_str()) {
                        match logical_key {
                            Key::Named(NamedKey::Backspace) => {
                                if self.cursor_pos > 0 {
                                    // Find previous char boundary
                                    let mut prev_idx = self.cursor_pos - 1;
                                    while prev_idx > 0 && !self.text.is_char_boundary(prev_idx) {
                                        prev_idx -= 1;
                                    }
                                    let _char_len = self.cursor_pos - prev_idx;
                                    self.cursor_pos = prev_idx;
                                    
                                    // Automerge splice_text takes char index (not byte index). 
                                    // We need to calculate char index for Automerge.
                                    let char_idx = self.text[0..self.cursor_pos].chars().count();
                                    let _ = state.splice_text(&obj_id, char_idx, 1, "");
                                    *handled = true;
                                }
                            }
                            Key::Named(NamedKey::ArrowLeft) => {
                                if self.cursor_pos > 0 {
                                    let mut prev_idx = self.cursor_pos - 1;
                                    while prev_idx > 0 && !self.text.is_char_boundary(prev_idx) {
                                        prev_idx -= 1;
                                    }
                                    self.cursor_pos = prev_idx;
                                    *handled = true;
                                }
                            }
                            Key::Named(NamedKey::ArrowRight) => {
                                if self.cursor_pos < self.text.len() {
                                    let mut next_idx = self.cursor_pos + 1;
                                    while next_idx < self.text.len() && !self.text.is_char_boundary(next_idx) {
                                        next_idx += 1;
                                    }
                                    self.cursor_pos = next_idx;
                                    *handled = true;
                                }
                            }
                            Key::Named(NamedKey::Space) => {
                                let char_idx = self.text[0..self.cursor_pos].chars().count();
                                let _ = state.splice_text(&obj_id, char_idx, 0, " ");
                                self.cursor_pos += 1;
                                *handled = true;
                            }
                            _ => {
                                if let Some(t) = text {
                                    if !t.as_str().chars().all(|c| c.is_control()) {
                                        let char_idx = self.text[0..self.cursor_pos].chars().count();
                                        let _ = state.splice_text(&obj_id, char_idx, 0, t.as_str());
                                        self.cursor_pos += t.as_str().len();
                                        *handled = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
