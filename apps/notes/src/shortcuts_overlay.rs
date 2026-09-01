//! Host-controlled keyboard-shortcut help: shared [`ModalFrame`] chrome around
//! a Notes-specific two-column grid. `?` opens it; Escape, the close control,
//! or a scrim click dismisses it.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::modal_frame::{ModalFrame, ModalLayout, ModalSpec};

/// Body row size — matches `typography.scale.sm` for compact density.
const ROW_SIZE_TOKEN: fn(&Theme) -> f32 = |t| t.tokens.typography.scale.sm;
/// Group heading size — one notch above the rows.
const GROUP_SIZE_TOKEN: fn(&Theme) -> f32 = |t| t.tokens.typography.scale.base;
/// The shortcut grid is wider and taller than a conventional prompt body.
const MODAL_WIDTH: f64 = 640.0;
const BODY_HEIGHT: f64 = 388.0;
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
            ("New page", "+ button"),
            ("New child of selected", "Cmd-N / Ctrl-N"),
            ("Reorder page up / down", "Alt-↑ / Alt-↓"),
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
    /// Shared modal geometry from the most recent draw. Events use the exact
    /// same close/card/scrim hit targets that were painted.
    layout: Option<ModalLayout>,
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
            layout: None,
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

    /// Host-driven because the overlay paints above the ordinary taffy tree.
    pub fn set_viewport(&mut self, viewport: Rect) {
        if self.viewport != viewport {
            self.viewport = viewport;
            self.layout = None;
            self.dirty = true;
        }
    }

    fn is_over_close(&self, point: Point) -> bool {
        self.layout
            .as_ref()
            .and_then(|layout| layout.close)
            .is_some_and(|rect| rect.contains(point))
    }

    /// Draw after the main widget tree. No-op when hidden.
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
        let close_hovered = self.is_over_close(self.last_cursor);
        let spec = ModalSpec::new("Keyboard shortcuts")
            .body_height(BODY_HEIGHT)
            .max_width(MODAL_WIDTH)
            .close_hovered(close_hovered);
        let layout = ModalFrame::layout(self.viewport, &spec, text_renderer, theme);
        ModalFrame::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);

        // Hint row below the title — small dim line telling the user
        // how to close.
        let hint_size = tokens.typography.scale.xs;
        let hint_y = layout.body.y0 + 2.0;
        text_renderer.draw_text(
            scene,
            "Press Esc or click outside to close",
            hint_size,
            tokens.colors.text_dim,
            transform * Affine::translate((layout.body.x0, hint_y)),
            None,
        );

        // Groups: 2-column grid. Each column gets half the inner width
        // (minus a spacing.s6 gutter).
        let groups_top = hint_y + f64::from(hint_size) + f64::from(tokens.spacing.s4);
        draw_groups(
            scene,
            transform,
            text_renderer,
            theme,
            layout.body.x0,
            layout.body.width(),
            groups_top,
        );
        self.layout = Some(layout);
    }

    /// Handle a key/mouse event when visible. Returns true if the
    /// event was consumed.
    pub fn handle_key(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        if !self.visible {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let was_over_close = self.is_over_close(self.last_cursor);
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                let is_over_close = self.is_over_close(self.last_cursor);
                self.dirty |= was_over_close != is_over_close;
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
                if self
                    .layout
                    .as_ref()
                    .is_some_and(|layout| layout.dismisses(self.last_cursor))
                {
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
    f64::from(group_size) + GROUP_HEADING_TO_ROWS_GAP + (row_count as f64) * ROW_HEIGHT
}

/// Draw every [`GROUPS`] entry in a 2-column grid starting at
/// `(inner_x0, groups_top)`, row-major: `[0, 1]` in row 0, `[2, 3]` in
/// row 1.
#[allow(
    clippy::too_many_arguments,
    reason = "draw-context bundle, not real parameter coupling"
)]
fn draw_groups(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    inner_x0: f64,
    inner_w: f64,
    groups_top: f64,
) {
    let tokens = &theme.tokens;
    let gutter = f64::from(tokens.spacing.s6);
    let col_w = ((inner_w - gutter) / 2.0).max(0.0);
    let col_left_x = inner_x0;
    let col_right_x = inner_x0 + col_w + gutter;

    let group_size = GROUP_SIZE_TOKEN(theme);
    let row_size = ROW_SIZE_TOKEN(theme);

    // Find the tallest group in row 0 to figure where row 1 starts.
    let column_top_for = |idx: usize| -> f64 {
        if idx < 2 {
            groups_top
        } else {
            let h0 = GROUPS.first().map_or(0.0, |g| {
                group_block_height(g.rows.len(), group_size, row_size)
            });
            let h1 = GROUPS.get(1).map_or(0.0, |g| {
                group_block_height(g.rows.len(), group_size, row_size)
            });
            groups_top + h0.max(h1) + f64::from(tokens.spacing.s6)
        }
    };

    for (idx, group) in GROUPS.iter().enumerate() {
        let x = if idx % 2 == 0 {
            col_left_x
        } else {
            col_right_x
        };
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

        let rows_top = y + f64::from(group_size) + GROUP_HEADING_TO_ROWS_GAP;
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
            let kx = x + col_w - f64::from(kw);
            // Soft chip background — translucent white for a "kbd"
            // affordance without needing per-theme tinting.
            let chip_pad_x = 6.0;
            let chip_pad_y = 2.0;
            let chip = RoundedRect::from_rect(
                Rect::new(
                    kx - chip_pad_x,
                    row_y - chip_pad_y,
                    kx + f64::from(kw) + chip_pad_x,
                    row_y + f64::from(row_size) + chip_pad_y,
                ),
                f64::from(tokens.radii.sm),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                tokens.colors.lift(0.72),
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
        #[allow(
            clippy::unwrap_used,
            reason = "new_leaf on a fresh style cannot fail — see ui::widget::TaffyTreeExt"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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

    fn name(&self) -> &'static str {
        "ShortcutsOverlay"
    }

    fn get_rect(&self) -> Rect {
        self.layout
            .as_ref()
            .map_or(Rect::ZERO, |layout| layout.card)
    }

    fn traps_focus(&self) -> bool {
        self.visible
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Dialog)
                .with_name("Keyboard shortcuts")
                .with_state(AccessState {
                    modal: true,
                    hidden: !self.visible,
                    ..Default::default()
                }),
        )
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

    fn paint(o: &mut ShortcutsOverlay) {
        let mut scene = Scene::new();
        let mut text_renderer = TextRenderer::new();
        o.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut text_renderer,
            &Theme::dark(),
        );
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
    fn shared_modal_hit_testing_preserves_inside_and_dismiss_paths() {
        let mut o = ShortcutsOverlay::new();
        o.set_viewport(Rect::new(0.0, 0.0, 1024.0, 768.0));
        o.set_visible(true);
        paint(&mut o);

        let centre = o.get_rect().center();
        o.handle_key(&cursor_at(centre), 1.0);
        o.handle_key(&left_press(), 1.0);
        assert!(o.is_visible(), "click inside the card must not dismiss");

        let close = o
            .layout
            .as_ref()
            .and_then(|layout| layout.close)
            .expect("shared modal frame supplies a close control");
        o.mark_drawn();
        o.handle_key(&cursor_at(close.center()), 1.0);
        assert!(
            o.needs_redraw(),
            "close hover must schedule its shared face"
        );
        assert!(o.handle_key(&left_press(), 1.0));
        assert!(!o.is_visible());

        o.set_visible(true);
        paint(&mut o);
        o.handle_key(&cursor_at(Point::new(10.0, 10.0)), 1.0);
        assert!(o.handle_key(&left_press(), 1.0));
        assert!(!o.is_visible());
    }

    #[test]
    fn visible_overlay_is_an_accessible_focus_trapping_dialog() {
        let mut o = ShortcutsOverlay::new();
        o.set_visible(true);
        assert!(o.traps_focus());
        let node = o.accessibility().expect("dialog semantics");
        assert_eq!(node.role, ui::a11y::Role::Dialog);
        assert!(node.state.modal);
        assert!(!node.state.hidden);
    }

    #[test]
    fn invisible_overlay_does_not_consume_events() {
        let mut o = ShortcutsOverlay::new();
        assert!(!o.handle_key(&esc_event(), 1.0));
    }
}
