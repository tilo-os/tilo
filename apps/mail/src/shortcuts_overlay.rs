//! Mail's keyboard reference, presented as the shared modal sheet. The rows
//! below are the single source for the discoverable key map; keep them
//! aligned with `MailApp::global_shortcuts`.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::TextRole;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::InputHint;
use ui::widget::modal_dialog::{self, ModalLayout, ModalSpec};

const ROWS: &[(&str, &[&str])] = &[
    ("Open message", &["Enter"]),
    ("Preview without marking read", &["P"]),
    ("Next message", &["J"]),
    ("Previous message", &["K"]),
    ("Search this folder", &["/"]),
    ("New message", &["C"]),
    ("Reply", &["R"]),
    ("Reply all", &["A"]),
    ("Forward", &["F"]),
    ("Star", &["S"]),
    ("Archive", &["E"]),
    ("Mark read", &["Shift", "I"]),
    ("Mark unread", &["U"]),
    ("Move to Trash", &["#"]),
    ("Select messages", &["X"]),
    ("Select all", &["Ctrl", "A"]),
    ("Delete selected messages permanently", &["Shift", "Delete"]),
    ("Check for new mail", &["F5"]),
    ("Close this list", &["Esc"]),
];

/// A small, local presentation overlay. It owns no mailbox state: every
/// shortcut it lists runs through the app's normal action routes.
pub struct MailShortcutsOverlay {
    visible: bool,
    viewport: Rect,
    layout: Option<ModalLayout>,
    last_cursor: Point,
    scroll_y: f64,
    max_scroll_y: f64,
    line: f64,
    dirty: bool,
}

impl MailShortcutsOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            viewport: Rect::ZERO,
            layout: None,
            last_cursor: Point::ZERO,
            scroll_y: 0.0,
            max_scroll_y: 0.0,
            line: 0.0,
            dirty: true,
        }
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn set_viewport(&mut self, viewport: Rect) {
        if self.viewport != viewport {
            self.viewport = viewport;
            self.layout = None;
            self.dirty = true;
        }
    }

    pub fn show(&mut self) {
        if !self.visible {
            self.visible = true;
            self.scroll_y = 0.0;
            self.dirty = true;
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.visible = false;
            self.dirty = true;
        }
    }

    pub fn card_rect(&self) -> Option<Rect> {
        self.layout.as_ref().map(|layout| layout.card)
    }

    pub fn needs_redraw(&self) -> bool {
        self.dirty
    }

    pub fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    pub fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if !self.visible || self.viewport.width() <= 0.0 {
            return;
        }
        let spacing = &theme.tokens.spacing;
        let label = theme.text(TextRole::Body);
        let (_, line_h) = tr.measure_style("Ag", None, &label);
        let keycap = InputHint::keycap_height(theme);
        let row_h = f64::from(line_h).max(keycap) + f64::from(spacing.s2);
        self.line = row_h;
        let gap = f64::from(spacing.s8);
        let wide = modal_dialog::body_width(self.viewport, f64::from(spacing.s16) * 11.0, theme)
            >= f64::from(spacing.s16) * 9.0;
        let columns = if wide { 2 } else { 1 };
        let rows_per_column = ROWS.len().div_ceil(columns);
        let close_hovered = self
            .layout
            .as_ref()
            .and_then(|layout| layout.close)
            .is_some_and(|rect| rect.contains(self.last_cursor));
        let spec = ModalSpec::new("Keyboard shortcuts")
            .body_height(rows_per_column as f64 * row_h)
            .max_width(f64::from(spacing.s16) * 11.0)
            .close_hovered(close_hovered);
        let layout = modal_dialog::layout(self.viewport, &spec, tr, theme);
        modal_dialog::paint_backdrop(scene, t, tr, theme, &layout, &spec);
        let (ink, _) = modal_dialog::ink(theme);

        let body = layout.body;
        self.max_scroll_y = (rows_per_column as f64 * row_h - body.height()).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y);
        let col_w = (body.width() - gap * (columns - 1) as f64) / columns as f64;
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);
        for (index, (name, keys)) in ROWS.iter().enumerate() {
            let column = index / rows_per_column;
            let row = index % rows_per_column;
            let x = body.x0 + column as f64 * (col_w + gap);
            let y = body.y0 + row as f64 * row_h - self.scroll_y;
            let keys: Vec<String> = keys.iter().map(|key| (*key).to_owned()).collect();
            let keys_w = InputHint::measure_keycap_labels(&keys, tr, theme);
            let text_w = (col_w - keys_w - f64::from(spacing.s3)).max(0.0);
            let shown = tr.elide_to_width_style(name, ui::num::f32_of_f64(text_w), &label);
            let text_top = y + (row_h - f64::from(line_h)) / 2.0;
            tr.draw_style(
                scene,
                &shown,
                ink,
                t * Affine::translate((x, text_top)),
                None,
                &label,
            );
            InputHint::paint_keycap_labels_at(
                scene,
                t,
                Point::new(x + col_w - keys_w, y + (row_h - keycap) / 2.0),
                &keys,
                tr,
                theme,
            );
        }
        scene.pop_layer();
        self.layout = Some(layout);
    }

    /// Handle only while visible. The sheet is modal: an Escape or a key
    /// meant to dismiss it never reaches Mail's actions underneath.
    pub fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        if !self.visible {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let old = self.last_cursor;
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                if old != self.last_cursor {
                    self.dirty = true;
                }
                true
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.hide();
                true
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Character(key),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if key == "?" => {
                self.hide();
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self
                    .layout
                    .as_ref()
                    .is_some_and(|layout| layout.dismisses(self.last_cursor))
                {
                    self.hide();
                }
                true
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let delta_y = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => -f64::from(*lines) * self.line,
                    MouseScrollDelta::PixelDelta(delta) => -delta.y / scale_factor,
                };
                let next = (self.scroll_y + delta_y).clamp(0.0, self.max_scroll_y);
                if (next - self.scroll_y).abs() > f64::EPSILON {
                    self.scroll_y = next;
                    self.dirty = true;
                }
                true
            }
            _ => true,
        }
    }

    pub fn accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        AccessNode::new(Role::Dialog)
            .with_name("Keyboard shortcuts")
            .with_bounds(self.card_rect().unwrap_or(Rect::ZERO))
            .with_state(AccessState::new().modal(true))
            .with_children(
                ROWS.iter()
                    .map(|(label, keys)| {
                        AccessNode::new(Role::StaticText)
                            .with_name(format!("{label}: {}", keys.join(" ")))
                    })
                    .collect(),
            )
    }
}

impl Default for MailShortcutsOverlay {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_names_an_action_in_sentence_case_and_its_keys() {
        for (label, keys) in ROWS {
            assert!(!keys.is_empty(), "{label} has no keys");
            let mut words = label.split_whitespace();
            let first = words.next().unwrap_or_default();
            assert!(first.chars().next().is_some_and(char::is_uppercase));
            assert!(
                words.all(
                    |word| word.chars().next().is_some_and(|c| !c.is_uppercase())
                        || word == "Trash"
                ),
                "{label} is not sentence case"
            );
        }
    }
}
