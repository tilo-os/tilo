//! `ShortcutsOverlay` — fullscreen modal listing every keyboard
//! shortcut, grouped by role. Opened by `?` (Shift+/), closed by
//! `Escape` or a click outside the card.
//!
//! Layout: dim full-viewport backdrop, a centered card (max
//! 640 × 480 px) on the raised surface, four labelled groups arranged
//! in a 2 × 2 grid. Each group lists `(label, shortcut)` rows; the
//! label is left-aligned in `text_primary`, the shortcut is
//! right-aligned in a monospace look (via `font_mono`) on a faint
//! surface chip so it reads as a key cap.
//!
//! State lives entirely outside the CRDT — the overlay's visibility
//! is host-controlled. The widget exposes [`is_visible`] /
//! [`set_visible`] for host wiring and accepts `Escape` /
//! click-outside for self-dismissal (toggles the flag off; the host
//! reads it back via [`is_visible`] on the next tick).

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// Title size for the modal heading. Mirrors `ModalDialog` (it uses
/// `typography.scale.lg`).
const TITLE_SIZE_TOKEN: fn(&Theme) -> f32 = |t| t.tokens.typography.scale.lg;
/// Body row size — matches `typography.scale.sm` for compact density.
const ROW_SIZE_TOKEN: fn(&Theme) -> f32 = |t| t.tokens.typography.scale.sm;
/// Group heading size — one notch above the rows.
const GROUP_SIZE_TOKEN: fn(&Theme) -> f32 = |t| t.tokens.typography.scale.base;
/// Card max size. Sized so all four groups fit at default sm row size
/// (~14 rows total) with comfortable padding.
const CARD_MAX_W: f64 = 640.0;
const CARD_MAX_H: f64 = 480.0;
/// Row height per shortcut entry — base size × 1.6 for vertical breath.
const ROW_HEIGHT: f64 = 22.0;
/// Vertical gap between a group heading and its rows.
const GROUP_HEADING_TO_ROWS_GAP: f64 = 8.0;

/// One group of shortcuts. The label drives the section heading; the
/// rows list each shortcut.
struct Group {
    label: &'static str,
    rows: &'static [(&'static str, &'static str)],
}

/// Canonical list of every shortcut surfaced in the help overlay. The
/// `?` shortcut deliberately includes itself so a user who's just
/// opened the overlay can find their way back out.
const GROUPS: &[Group] = &[
    Group {
        label: "Navigation",
        rows: &[
            ("Jump to page", "Cmd-K / Ctrl-K"),
            ("Move selection up / down", "↑ / ↓"),
            ("Open shortcuts overlay", "?"),
            ("Close overlay / popover", "Esc"),
        ],
    },
    Group {
        label: "Editing",
        rows: &[
            ("New paragraph", "Enter"),
            ("Indent / outdent list", "Tab / Shift-Tab"),
            ("Open block menu", "/"),
            ("Link another page", "[["),
            ("Toggle checklist item", "click"),
        ],
    },
    Group {
        label: "Formatting",
        rows: &[
            ("Bold", "**text** or Ctrl-B"),
            ("Italic", "*text* or Ctrl-I"),
            ("Inline code", "`text` or Ctrl-E"),
            ("Heading 1 / 2 / 3", "# / ## / ###"),
        ],
    },
    Group {
        label: "Pages",
        rows: &[
            ("New child of selected", "Cmd-N / Ctrl-N"),
            ("Rename page", "double-click / F2"),
            ("Delete page", "Delete"),
        ],
    },
];

pub struct ShortcutsOverlay {
    visible: bool,
    /// Full window rect — set by `apply_layout`. Used for the dim
    /// backdrop + to centre the card.
    viewport: Rect,
    /// Card rect (centred sub-region of the viewport).
    card: Rect,
    /// Cached last-cursor position so `MouseInput` (no coords) can
    /// hit-test against the card.
    last_cursor: Point,
    dirty: bool,
}

impl ShortcutsOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            viewport: Rect::ZERO,
            card: Rect::ZERO,
            last_cursor: Point::ZERO,
            dirty: true,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn set_visible(&mut self, v: bool) {
        if self.visible != v {
            self.visible = v;
            self.dirty = true;
        }
    }

    pub fn toggle(&mut self) {
        self.set_visible(!self.visible);
    }

    /// Host-driven layout — the overlay is painted on top of the
    /// existing tree, so we don't get a taffy slot. Call this with
    /// the window rect on every frame the overlay is visible (or in
    /// `apply_layout` if we ever wire it into the taffy tree).
    pub fn set_viewport(&mut self, viewport: Rect) {
        if self.viewport != viewport {
            self.viewport = viewport;
            // Recompute the card rect — capped at our max, never
            // larger than the viewport, centred.
            let w = CARD_MAX_W.min(viewport.width() - 32.0).max(320.0);
            let h = CARD_MAX_H.min(viewport.height() - 32.0).max(240.0);
            let cx = viewport.x0 + (viewport.width() - w) / 2.0;
            let cy = viewport.y0 + (viewport.height() - h) / 2.0;
            self.card = Rect::new(cx, cy, cx + w, cy + h);
            self.dirty = true;
        }
    }

    /// Draw the overlay on top of the host's scene. Call this AFTER
    /// the main widget tree has been drawn. No-op when not visible.
    pub fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.visible || self.viewport.width() <= 0.0 {
            return;
        }
        let tokens = &theme.tokens;

        // Dim backdrop. Matches `ModalDialog`'s 70% black so the visual
        // affordance is consistent across modals in the app.
        scene.fill(
            Fill::NonZero,
            transform,
            Color::BLACK.with_alpha_factor(0.7),
            None,
            &self.viewport,
        );

        // Card.
        let radius = tokens.radii.md as f64;
        let card = RoundedRect::from_rect(self.card, radius);
        scene.fill(Fill::NonZero, transform, tokens.colors.surfaces.raised, None, &card);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_standard,
            None,
            &card,
        );

        let pad = tokens.spacing.s6 as f64;
        let inner_x0 = self.card.x0 + pad;
        let inner_y0 = self.card.y0 + pad;
        let inner_x1 = self.card.x1 - pad;
        let inner_y1 = self.card.y1 - pad;
        let inner_w = (inner_x1 - inner_x0).max(0.0);

        // Title row.
        let title_size = TITLE_SIZE_TOKEN(theme);
        text_renderer.draw_text(
            scene,
            "Keyboard shortcuts",
            title_size,
            tokens.colors.text_primary,
            transform * Affine::translate((inner_x0, inner_y0)),
            None,
        );

        // Hint row below the title — small dim line telling the user
        // how to close.
        let hint_size = tokens.typography.scale.xs;
        let hint_y = inner_y0 + title_size as f64 + 2.0;
        text_renderer.draw_text(
            scene,
            "Press Esc or click outside to close",
            hint_size,
            tokens.colors.text_dim,
            transform * Affine::translate((inner_x0, hint_y)),
            None,
        );

        // Groups: 2-column grid. Each column gets half the inner width
        // (minus a spacing.s6 gutter).
        let gutter = tokens.spacing.s6 as f64;
        let col_w = ((inner_w - gutter) / 2.0).max(0.0);
        let groups_top = hint_y + hint_size as f64 + tokens.spacing.s4 as f64;
        let col_left_x = inner_x0;
        let col_right_x = inner_x0 + col_w + gutter;

        let group_size = GROUP_SIZE_TOKEN(theme);
        let row_size = ROW_SIZE_TOKEN(theme);

        // Place groups in row-major order: [0, 1] in row 0, [2, 3] in row 1.
        let _ = inner_y1;
        let column_top_for = |idx: usize| -> f64 {
            // Find the tallest group in row 0 to figure where row 1 starts.
            if idx < 2 {
                groups_top
            } else {
                let h0 = group_block_height(GROUPS[0].rows.len(), group_size, row_size);
                let h1 = group_block_height(GROUPS[1].rows.len(), group_size, row_size);
                groups_top + h0.max(h1) + tokens.spacing.s6 as f64
            }
        };

        for (idx, group) in GROUPS.iter().enumerate() {
            let x = if idx % 2 == 0 { col_left_x } else { col_right_x };
            let y = column_top_for(idx);

            // Group heading.
            text_renderer.draw_text(
                scene,
                group.label,
                group_size,
                tokens.colors.text_primary,
                transform * Affine::translate((x, y)),
                None,
            );

            let rows_top = y + group_size as f64 + GROUP_HEADING_TO_ROWS_GAP;
            for (i, (label, key)) in group.rows.iter().enumerate() {
                let row_y = rows_top + (i as f64) * ROW_HEIGHT;
                text_renderer.draw_text(
                    scene,
                    label,
                    row_size,
                    tokens.colors.text_dim,
                    transform * Affine::translate((x, row_y)),
                    None,
                );
                // Shortcut chip: right-aligned in the column.
                let (kw, _) = text_renderer.measure(key, row_size, None);
                let kx = x + col_w - kw as f64;
                // Soft chip background — translucent white for a "kbd"
                // affordance without needing per-theme tinting.
                let chip_pad_x = 6.0;
                let chip_pad_y = 2.0;
                let chip = RoundedRect::from_rect(
                    Rect::new(
                        kx - chip_pad_x,
                        row_y - chip_pad_y,
                        kx + kw as f64 + chip_pad_x,
                        row_y + row_size as f64 + chip_pad_y,
                    ),
                    tokens.radii.sm as f64,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    tokens.colors.surface_white_10,
                    None,
                    &chip,
                );
                text_renderer.draw_text(
                    scene,
                    key,
                    row_size,
                    tokens.colors.text_primary,
                    transform * Affine::translate((kx, row_y)),
                    None,
                );
            }
        }
    }

    /// Handle a key/mouse event when visible. Returns true if the
    /// event was consumed.
    pub fn handle_key(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        if !self.visible {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / scale_factor, position.y / scale_factor);
                false
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
                self.set_visible(false);
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if !self.card.contains(self.last_cursor) {
                    // Click outside the card dismisses.
                    self.set_visible(false);
                }
                // Always consume — overlay is blocking.
                true
            }
            // Eat every keystroke so the editor underneath doesn't
            // see it while the overlay is up.
            WindowEvent::KeyboardInput { .. } => true,
            _ => false,
        }
    }
}

impl Default for ShortcutsOverlay {
    fn default() -> Self {
        Self::new()
    }
}

fn group_block_height(row_count: usize, group_size: f32, _row_size: f32) -> f64 {
    group_size as f64
        + GROUP_HEADING_TO_ROWS_GAP
        + (row_count as f64) * ROW_HEIGHT
}

impl Widget for ShortcutsOverlay {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Reserve a zero-height leaf — the overlay never participates
        // in the layout pass, it just needs to be addressable from the
        // host's draw chain. (Same pattern `link_popover` follows.)
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {
        // Host calls `set_viewport` instead.
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Use `draw_overlay` from the host so the overlay paints on
        // top of everything else. A no-op here keeps the trait happy.
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Use `handle_key` from the host.
    }

    fn name(&self) -> &str {
        "ShortcutsOverlay"
    }

    fn get_rect(&self) -> Rect {
        self.card
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::DeviceId;
    use winit::keyboard::{KeyLocation, PhysicalKey};

    fn synth_device() -> DeviceId {
        // SAFETY: opaque `DeviceId`; the all-zero pattern is the
        // same trick used in `modal_dialog::tests`.
        unsafe { std::mem::zeroed() }
    }

    fn esc_event() -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Escape);
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    #[test]
    fn set_visible_flips_and_marks_dirty() {
        let mut o = ShortcutsOverlay::new();
        assert!(!o.is_visible());
        o.mark_drawn();
        assert!(!o.needs_redraw());
        o.set_visible(true);
        assert!(o.is_visible());
        assert!(o.needs_redraw());
    }

    #[test]
    fn escape_closes_overlay() {
        let mut o = ShortcutsOverlay::new();
        o.set_visible(true);
        let consumed = o.handle_key(&esc_event(), 1.0);
        assert!(consumed);
        assert!(!o.is_visible());
    }

    #[test]
    fn click_outside_card_dismisses() {
        let mut o = ShortcutsOverlay::new();
        o.set_viewport(Rect::new(0.0, 0.0, 1024.0, 768.0));
        o.set_visible(true);
        // (10, 10) is outside the centered card (~ 192..832 × 144..624).
        o.handle_key(&cursor_at(Point::new(10.0, 10.0)), 1.0);
        let consumed = o.handle_key(&left_press(), 1.0);
        assert!(consumed);
        assert!(!o.is_visible());
    }

    #[test]
    fn click_inside_card_does_not_dismiss() {
        let mut o = ShortcutsOverlay::new();
        o.set_viewport(Rect::new(0.0, 0.0, 1024.0, 768.0));
        o.set_visible(true);
        let centre = o.card.center();
        o.handle_key(&cursor_at(centre), 1.0);
        o.handle_key(&left_press(), 1.0);
        assert!(o.is_visible(), "click inside the card must not dismiss");
    }

    #[test]
    fn invisible_overlay_does_not_consume_events() {
        let mut o = ShortcutsOverlay::new();
        assert!(!o.handle_key(&esc_event(), 1.0));
    }
}
