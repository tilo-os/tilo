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
use crate::presence::{PathSegment, PresenceKey};

pub struct TextInput {
    _id: String,
    path: Vec<PathSegment>,
    rect: Rect,
    is_hovered: bool,
    is_focused: bool,
    text: String,
    cursor_pos: usize,
    on_change_path: String,
    last_mouse_pos: Point,
    char_x_positions: Vec<f64>,
    scroll_x: f64,
    last_edit_time: Option<std::time::Instant>,
}

impl TextInput {
    /// Construct a text input bound to a presence `path` (used as the
    /// widget's `PresenceKey::Widget { path }`). Empty path opts the widget
    /// out of presence rendering.
    ///
    /// Back-compat helpers:
    ///  - `with_id(id)`: attach a debug/element id string.
    ///  - `with_on_change_path(path_str)`: attach the CRDT path that this
    ///    input reads/writes.
    pub fn new(path: Vec<PathSegment>) -> Self {
        Self {
            _id: String::new(),
            path,
            rect: Rect::ZERO,
            is_hovered: false,
            is_focused: false,
            text: String::new(),
            cursor_pos: 0,
            on_change_path: String::new(),
            last_mouse_pos: Point::ZERO,
            char_x_positions: Vec::new(),
            scroll_x: 0.0,
            last_edit_time: None,
        }
    }

    /// Back-compat builder: set the debug/element id.
    pub fn with_id(mut self, id: &str) -> Self {
        self._id = id.to_string();
        self
    }

    /// Back-compat builder: set the CRDT path the widget reads/writes.
    pub fn with_on_change_path(mut self, path: &str) -> Self {
        self.on_change_path = path.to_string();
        self
    }
}

impl Widget for TextInput {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
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
        // Ensure cursor is on char boundary
        while self.cursor_pos > 0 && !self.text.is_char_boundary(self.cursor_pos) {
            self.cursor_pos -= 1;
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size {
                width: Dimension::percent(1.0),
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
        let bg_color = if self.is_focused {
            theme.bg_active
        } else {
            theme.tokens.colors.surfaces.raised
        };

        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
        scene.fill(vello::peniko::Fill::NonZero, transform, bg_color, None, &shape);

        let border_color = if self.is_focused {
            theme.tokens.colors.border_focus
        } else if self.is_hovered {
            theme.tokens.colors.border_standard
        } else {
            theme.tokens.colors.border_standard
        };
        scene.stroke(&Stroke::new(if self.is_focused { 2.0 } else { 1.0 }), transform, border_color, None, &shape);

        let text_size = 16.0;
        let padding_x = theme.padding_standard as f64;

        // Pre-calculate positions for exact caret placement and click resolution
        self.char_x_positions.clear();
        self.char_x_positions.resize(self.text.len() + 1, 0.0);
        for (byte_idx, _) in self.text.char_indices() {
            let (w, _) = text_renderer.measure(&self.text[0..byte_idx], text_size, None);
            self.char_x_positions[byte_idx] = w as f64;
        }
        let (total_w, _) = text_renderer.measure(&self.text, text_size, None);
        self.char_x_positions[self.text.len()] = total_w as f64;

        // Cursor
        let cursor_pos = self.cursor_pos.min(self.text.len());
        let cursor_x_offset = if cursor_pos < self.char_x_positions.len() && self.text.is_char_boundary(cursor_pos) {
            self.char_x_positions[cursor_pos]
        } else {
            let (w, _) = text_renderer.measure(&self.text, text_size, None);
            w as f64
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
            transform * Affine::translate((self.rect.x0 + padding_x - self.scroll_x, text_y)),
            None,
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

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, _modifiers: &winit::event::Modifiers, state: &mut AutoCommit, root: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, _taffy: &TaffyTree<()>) {
        // Safety clamp
        if self.cursor_pos > self.text.len() {
            self.cursor_pos = self.text.len();
        }
        while self.cursor_pos > 0 && !self.text.is_char_boundary(self.cursor_pos) {
            self.cursor_pos -= 1;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical_pos);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }
                self.last_mouse_pos = logical_pos;
            }
            WindowEvent::MouseInput { state: element_state, button: MouseButton::Left, .. } => {
                if self.is_hovered && *element_state == ElementState::Pressed {
                    if !self.is_focused {
                        *focus_target = Some(node_id);
                        *handled = true;
                    }
                    
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
                    
                    if self.cursor_pos != best_idx {
                        self.cursor_pos = best_idx;
                        *handled = true;
                    }
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
                                    
                                    // Optimistic local update
                                    let char_idx = self.text[0..prev_idx].chars().count();
                                    self.text.remove(prev_idx); 
                                    self.cursor_pos = prev_idx;
                                    self.last_edit_time = Some(std::time::Instant::now());
                                    
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
                                // Optimistic local update
                                self.text.insert(self.cursor_pos, ' ');
                                self.last_edit_time = Some(std::time::Instant::now());
                                
                                let _ = state.splice_text(&obj_id, char_idx, 0, " ");
                                self.cursor_pos += 1;
                                *handled = true;
                            }
                            _ => {
                                if let Some(t) = text {
                                    if !t.as_str().chars().all(|c| c.is_control()) {
                                        let char_idx = self.text[0..self.cursor_pos].chars().count();
                                        
                                        // Optimistic local update
                                        let s = t.as_str();
                                        self.text.insert_str(self.cursor_pos, s);
                                        self.last_edit_time = Some(std::time::Instant::now());
                                        
                                        let _ = state.splice_text(&obj_id, char_idx, 0, s);
                                        self.cursor_pos += s.len();
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

    fn name(&self) -> &str { "TextInput" }

    fn test_id(&self) -> Option<&str> {
        if self._id.is_empty() {
            Some("text_input")
        } else {
            Some(self._id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }

    fn needs_commit(&self) -> bool {
        if let Some(t) = self.last_edit_time {
            t.elapsed() >= std::time::Duration::from_secs(1)
        } else {
            false
        }
    }

    fn on_commit(&mut self) {
        self.last_edit_time = None;
    }

    fn get_state(&self) -> Option<String> {
        Some(self.cursor_pos.to_string())
    }

    fn set_state(&mut self, state: &str) {
        if let Ok(pos) = state.parse::<usize>() {
            self.cursor_pos = pos;
        }
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget { path: self.path.clone() })
        }
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
        // Always draw the base input first.
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(key) = self.presence_key() {
            let origin = self.rect.origin();

            // Use a fallback glyph advance (the local caret uses the text
            // renderer's exact measurement; a peer caret without locally-known
            // layout falls back to 8 px/char, which is the same contract the
            // plan specifies for peer rendering).
            let advance = 8.0_f32;

            // Peer carets.
            if let Some(list) = presence.carets.get(&key) {
                for (user, caret) in list.iter() {
                    let x = origin.x as f32 + 4.0 + (*caret as f32) * advance;
                    let y = origin.y as f32 + 4.0;
                    presence.draw_peer_caret(scene, theme, user, (x, y));
                }
            }

            // Peer selections.
            if let Some(list) = presence.selections.get(&key) {
                for (user, range) in list.iter() {
                    let x_start = origin.x + 4.0 + (range.start as f64) * advance as f64;
                    let x_end = origin.x + 4.0 + (range.end as f64) * advance as f64;
                    let rect = Rect::new(
                        x_start,
                        origin.y + 2.0,
                        x_end,
                        origin.y + 22.0,
                    );
                    presence.draw_peer_selection(scene, theme, user, range, &[rect]);
                }
            }
        }

        // Read-only gating: dim the control if the permission scope is denied.
        // Alpha kept aligned with every other widget's denied overlay
        // (button / container / column / row / spacer / stack / text /
        // toggle / slider / checkbox all paint `Color::rgba8(0, 0, 0, 77)`).
        // Pinned by `permission_denied_overlay_alpha_matches_other_widgets`.
        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let r = self.rect;
                let overlay = Self::PERMISSION_DENIED_OVERLAY;
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    Affine::IDENTITY,
                    overlay,
                    None,
                    &r,
                );
            }
        }
    }
}

impl TextInput {
    /// Overlay color used to dim the control when the permission scope
    /// resolves to denied. Held as an associated const so the unit test
    /// below can pin the alpha without poking inside `draw`. Concept's
    /// every-widget convention is `bg-black/30` ≈ 0.30 × 255 ≈ 77.
    pub(crate) const PERMISSION_DENIED_OVERLAY: vello::peniko::Color =
        vello::peniko::Color::rgba8(0, 0, 0, 77);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Polish-sweep #127 pin: the permission-denied dim must paint at the
    /// same alpha across every widget. The shared alpha lives on each
    /// widget's `PERMISSION_DENIED_OVERLAY` const (or as the literal
    /// `Color::rgba8(0, 0, 0, 77)` baked into older draws); a drift here
    /// shows up as a visibly stronger dim on text inputs vs. the
    /// surrounding panel.
    #[test]
    fn permission_denied_overlay_alpha_matches_other_widgets() {
        // The full constant must equal `Color::rgba8(0, 0, 0, 77)` —
        // matching both the byte components AND the call-site every other
        // widget already paints (Button / Container / Column / Row /
        // Spacer / Stack / Text / Toggle / Slider / Checkbox).
        let expected = vello::peniko::Color::rgba8(0, 0, 0, 77);
        assert_eq!(
            TextInput::PERMISSION_DENIED_OVERLAY, expected,
            "TextInput's permission-denied overlay must equal \
             Color::rgba8(0, 0, 0, 77) (≈ bg-black/30) to match every \
             other widget's denied dim"
        );
    }

    #[test]
    fn text_input_test_id_default_is_text_input() {
        let ti = TextInput::new(Vec::new());
        assert_eq!(Widget::test_id(&ti), Some("text_input"));
    }

    #[test]
    fn text_input_test_id_can_be_overridden() {
        let ti = TextInput::new(Vec::new()).with_id("search_box");
        assert_eq!(Widget::test_id(&ti), Some("search_box"));
    }
}
