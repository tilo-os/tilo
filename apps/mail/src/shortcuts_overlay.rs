//! Mail's keyboard reference, presented in the shared modal frame rather than
//! as a bespoke canvas dialog.  The rows below are the single source for the
//! discoverable key map; keep them aligned with `MailApp::global_shortcuts`.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::modal_frame::{ModalFrame, ModalLayout, ModalSpec};

use crate::draw::{fill_round, stroke_round};

const ROWS: &[(&str, &str)] = &[
    ("Open selected message (marks it read)", "Enter"),
    ("Preview selected message (keeps unread)", "P"),
    ("Browse selected message", "↑ / ↓"),
    ("Search mail", "/ or Cmd/Ctrl-F"),
    ("Compose", "C"),
    ("Reply / star / archive", "R / S / E"),
    ("Mark unread / move to Trash", "U / Delete"),
    ("Select messages / select all", "X / Cmd/Ctrl-A"),
    ("Permanently delete selected Trash", "Shift-Delete"),
    ("Refresh current folder", "F5"),
    ("Close this reference", "Esc"),
];

/// A deliberately small, local presentation overlay.  It owns no mailbox
/// state: shortcut activation continues to use the normal app action routes.
pub struct MailShortcutsOverlay {
    visible: bool,
    viewport: Rect,
    layout: Option<ModalLayout>,
    last_cursor: Point,
    scroll_y: f64,
    max_scroll_y: f64,
    text_scale: f64,
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
            text_scale: 1.0,
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
        let scale = f64::from(theme.text_scale);
        self.text_scale = scale;
        let compact = self.viewport.width() < 620.0 * scale;
        let close_hovered = self
            .layout
            .as_ref()
            .and_then(|layout| layout.close)
            .is_some_and(|rect| rect.contains(self.last_cursor));
        let spec = ModalSpec::new(if compact {
            "Shortcuts"
        } else {
            "Keyboard shortcuts"
        })
        .body_height(if compact { 548.0 } else { 338.0 })
        .max_width(700.0)
        .close_hovered(close_hovered);
        let layout = ModalFrame::layout(self.viewport, &spec, tr, theme);
        ModalFrame::paint_backdrop(scene, t, tr, theme, &layout, &spec);

        let colors = &theme.tokens.colors;
        let intro = if compact {
            "Mail’s shortcuts use the same actions as buttons and menus."
        } else {
            "Opening marks mail read. Preview is explicitly read-state neutral."
        };
        tr.draw_text(
            scene,
            intro,
            12.0,
            colors.text_dim,
            t * Affine::translate((layout.body.x0, layout.body.y0 + 2.0 * scale)),
            Some(layout.body.width() as f32),
        );
        let top = layout.body.y0 + 31.0 * scale;
        let row_h = if compact { 43.0 } else { 27.0 } * scale;
        let columns = if compact { 1 } else { 2 };
        let rows_per_column = ROWS.len().div_ceil(columns);
        // A 200%-text phone-width dialog cannot fit all eleven rows. Keep the
        // title/introduction fixed and scroll only the clipped list instead
        // of shrinking the keycaps or painting under the card edge.
        self.max_scroll_y = (top + rows_per_column as f64 * row_h - layout.body.y1).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y);
        let gap = 28.0 * scale;
        let col_w = (layout.body.width() - gap * (columns - 1) as f64) / columns as f64;
        let rows_view = Rect::new(layout.body.x0, top, layout.body.x1, layout.body.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rows_view);
        for (index, (label, key)) in ROWS.iter().enumerate() {
            let column = index / rows_per_column;
            let row = index % rows_per_column;
            let x = layout.body.x0 + column as f64 * (col_w + gap);
            let y = top + row as f64 * row_h - self.scroll_y;
            tr.draw_text(
                scene,
                label,
                12.0,
                colors.text_primary,
                t * Affine::translate((x, y)),
                Some((col_w - 86.0 * scale).max(80.0) as f32),
            );
            let (key_w, _) = tr.measure(key, 11.0, None);
            let chip_w = f64::from(key_w) + 14.0 * scale;
            let chip = Rect::new(
                x + col_w - chip_w,
                y - 3.0 * scale,
                x + col_w,
                y + 17.0 * scale,
            );
            fill_round(scene, t, chip, 6.0 * scale, colors.lift(0.72));
            stroke_round(scene, t, chip, 6.0 * scale, colors.border_standard, 1.0);
            tr.draw_text_weighted(
                scene,
                key,
                11.0,
                colors.text_primary,
                t * Affine::translate((chip.x0 + 7.0 * scale, y)),
                None,
                TextWeight::Medium,
            );
        }
        scene.pop_layer();
        self.layout = Some(layout);
    }

    /// Handle only while visible.  The sheet is intentionally modal: it
    /// prevents an Escape or a character intended to dismiss/help from
    /// reaching Mail’s action router underneath.
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
                    MouseScrollDelta::LineDelta(_, lines) => {
                        -f64::from(*lines) * 36.0 * self.text_scale
                    }
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
            .with_name("Mail keyboard shortcuts")
            .with_description(
                "Shortcut reference. Open marks the selected message read; Preview keeps its unread state.",
            )
            .with_bounds(self.card_rect().unwrap_or(Rect::ZERO))
            .with_state(AccessState::new().modal(true))
            .with_children(
                ROWS
                    .iter()
                    .map(|(label, key)| AccessNode::new(Role::StaticText).with_name(format!("{label}: {key}")))
                    .collect(),
            )
    }
}

impl Default for MailShortcutsOverlay {
    fn default() -> Self {
        Self::new()
    }
}
