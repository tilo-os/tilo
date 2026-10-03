//! Host-controlled keyboard-shortcut help: shared [`ModalFrame`] chrome around
//! a Notes-specific two-column grid. `?` opens it; Escape, the close control,
//! or a scrim click dismisses it.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::modal_frame::{ModalFrame, ModalLayout, ModalSpec};

use crate::num;

/// Body row size before `TextRenderer` applies the accessibility multiplier.
const ROW_SIZE_TOKEN: fn(&Theme) -> f32 =
    |t| (t.tokens.typography.scale.sm / t.text_scale).max(12.0);
/// Group heading size before `TextRenderer` applies the accessibility multiplier.
const GROUP_SIZE_TOKEN: fn(&Theme) -> f32 =
    |t| (t.tokens.typography.scale.base / t.text_scale).max(14.0);
/// The shortcut grid is wider and taller than a conventional prompt body.
const MODAL_WIDTH: f64 = 640.0;
const BODY_HEIGHT: f64 = 388.0;
/// Row height per shortcut entry — base size × 1.6 for vertical breath.
const ROW_HEIGHT: f64 = 22.0;
/// Compact layouts stack the key below its description. This leaves room for
/// a two-line description at 200% text instead of making two tiny columns
/// collide.
const COMPACT_ROW_HEIGHT: f64 = 56.0;
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
            ("Jump to note", "Cmd-K / Ctrl-K"),
            ("Back / forward note", "Alt-← / Alt-→"),
            ("Move selection up / down", "↑ / ↓"),
            ("Collapse / expand note", "← / →"),
            ("Open shortcuts overlay", "?"),
            ("Close overlay / popover", "Esc"),
        ],
    },
    Group {
        label: "Editing",
        rows: &[
            ("Find in note", "Cmd-F / Ctrl-F"),
            ("New paragraph", "Enter"),
            ("notes-help-outline-indent", "Tab / Shift-Tab"),
            ("notes-help-block-indent", "Ctrl-] / Ctrl-["),
            ("Open block menu", "/"),
            ("Link another note", "[["),
            ("Toggle checklist item", "click"),
            ("Move focused block", "Alt-↑ / Alt-↓"),
            ("notes-help-code-exit", "Ctrl-Enter"),
            ("notes-help-code-before", "Ctrl-Shift-Enter"),
            ("notes-help-code-copy", "Ctrl-Shift-C"),
            ("notes-help-code-indent", "Tab / Shift-Tab"),
            ("notes-help-code-blank", "Shift-Enter"),
            ("notes-help-code-select", "Ctrl-A"),
        ],
    },
    Group {
        label: "Formatting",
        rows: &[
            ("Bold", "**text** or Ctrl-B"),
            ("Italic", "*text* or Ctrl-I"),
            ("Inline code", "`text` or Ctrl-E"),
            ("Heading 1 / 2 / 3", "# / ## / ###"),
            ("Quote", "> "),
            ("notes-help-code", "```python ↵"),
            ("notes-help-bullet", "- Space"),
            ("notes-help-numbered", "1. Space"),
            ("notes-help-checklist", "[ ] Space"),
        ],
    },
    Group {
        label: "Notes",
        rows: &[
            ("New note", "+ button"),
            ("New child of selected", "Cmd-N / Ctrl-N"),
            ("Indent / outdent note", "Tab / Shift-Tab"),
            ("Reorder note up / down", "Alt-↑ / Alt-↓"),
            ("Rename note", "double-click / F2"),
            ("Note actions", "Menu / Shift-F10"),
            ("Link a note", "[[Note]]"),
            ("Embed a note", "![[Note]]"),
            ("Delete note", "Delete"),
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
    /// Local scroll position for the bounded shortcut reference.
    scroll_y: f64,
    max_scroll_y: f64,
    text_scale: f64,
    dirty: bool,
}

impl ShortcutsOverlay {
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

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn set_visible(&mut self, v: bool) {
        if self.visible != v {
            self.visible = v;
            self.scroll_y = 0.0;
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
        let scale = f64::from(theme.text_scale);
        self.text_scale = scale;
        let compact_view = self.viewport.width() < 600.0 * scale;
        let spec = ModalSpec::new(if compact_view {
            "Shortcuts"
        } else {
            "Keyboard shortcuts"
        })
        .body_height(BODY_HEIGHT)
        .max_width(MODAL_WIDTH)
        .close_hovered(close_hovered);
        let layout = ModalFrame::layout(self.viewport, &spec, text_renderer, theme);
        ModalFrame::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);

        // Hint row below the title — small dim line telling the user
        // how to close.
        let hint_size = (tokens.typography.scale.xs / theme.text_scale).max(11.0);
        let hint_y = layout.body.y0 + 2.0 * scale;
        text_renderer.draw_text(
            scene,
            &ui::i18n::I18nManager::lookup(
                if compact_view {
                    "notes-help-close-compact"
                } else {
                    "Press Esc or click outside to close"
                },
                None,
            ),
            hint_size,
            tokens.colors.text_dim,
            transform * Affine::translate((layout.body.x0, hint_y)),
            None,
        );

        // Groups: 2-column grid. Each column gets half the inner width
        // (minus a spacing.s6 gutter).
        let groups_top =
            hint_y + f64::from(hint_size) * scale + f64::from(tokens.spacing.s4) * scale;
        let footer_h = 30.0 * scale;
        let groups_view = Rect::new(
            layout.body.x0,
            groups_top,
            layout.body.x1,
            (layout.body.y1 - footer_h).max(groups_top),
        );
        let compact = layout.body.width() < 520.0 * scale;
        self.max_scroll_y = shortcut_scroll_limit(groups_top, groups_view.y1, theme, compact);
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &groups_view,
        );
        draw_groups(
            scene,
            transform,
            text_renderer,
            theme,
            layout.body.x0,
            layout.body.width(),
            groups_top - self.scroll_y,
            compact,
        );
        scene.pop_layer();
        if self.max_scroll_y > 0.0 {
            let label = if compact_view && self.scroll_y + 0.5 < self.max_scroll_y {
                "notes-help-more-compact"
            } else if compact_view {
                "notes-help-earlier-compact"
            } else if self.scroll_y + 0.5 < self.max_scroll_y {
                "Scroll for more shortcuts ↓"
            } else {
                "Scroll up for earlier shortcuts ↑"
            };
            text_renderer.draw_text(
                scene,
                &ui::i18n::I18nManager::lookup(label, None),
                hint_size,
                tokens.colors.text_dim,
                transform
                    * Affine::translate((layout.body.x0, layout.body.y1 - footer_h + 4.0 * scale)),
                None,
            );
        }
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
            WindowEvent::MouseWheel { delta, .. } => {
                let delta_y = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => {
                        -f64::from(*lines) * ROW_HEIGHT * self.text_scale
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
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(key),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if matches!(
                key,
                NamedKey::ArrowDown
                    | NamedKey::ArrowUp
                    | NamedKey::PageDown
                    | NamedKey::PageUp
                    | NamedKey::Home
                    | NamedKey::End
            ) =>
            {
                let step = ROW_HEIGHT * self.text_scale;
                let page = self
                    .layout
                    .as_ref()
                    .map_or(step * 8.0, |layout| layout.body.height() * 0.8);
                let next = match key {
                    NamedKey::ArrowDown => self.scroll_y + step,
                    NamedKey::ArrowUp => self.scroll_y - step,
                    NamedKey::PageDown => self.scroll_y + page,
                    NamedKey::PageUp => self.scroll_y - page,
                    NamedKey::Home => 0.0,
                    _ => self.max_scroll_y,
                }
                .clamp(0.0, self.max_scroll_y);
                self.dirty |= (next - self.scroll_y).abs() > f64::EPSILON;
                self.scroll_y = next;
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

fn group_block_height(
    row_count: usize,
    group_size: f32,
    _row_size: f32,
    scale: f64,
    compact: bool,
) -> f64 {
    f64::from(group_size) * scale
        + GROUP_HEADING_TO_ROWS_GAP * scale
        + (row_count as f64)
            * if compact {
                COMPACT_ROW_HEIGHT
            } else {
                ROW_HEIGHT
            }
            * scale
}

fn shortcut_scroll_limit(groups_top: f64, groups_bottom: f64, theme: &Theme, compact: bool) -> f64 {
    let scale = f64::from(theme.text_scale);
    let group_size = GROUP_SIZE_TOKEN(theme);
    let row_size = ROW_SIZE_TOKEN(theme);
    let group_height = |index: usize| {
        GROUPS.get(index).map_or(0.0, |group| {
            group_block_height(group.rows.len(), group_size, row_size, scale, compact)
        })
    };
    let gap = f64::from(theme.tokens.spacing.s6) * scale;
    let full_content_h = if compact {
        GROUPS
            .iter()
            .enumerate()
            .map(|(index, _)| group_height(index))
            .sum::<f64>()
            + gap * (GROUPS.len().saturating_sub(1) as f64)
    } else {
        group_height(0).max(group_height(1)) + gap + group_height(2).max(group_height(3))
    };
    (groups_top + full_content_h - groups_bottom).max(0.0)
}

/// Draw every [`GROUPS`] entry in a 2-column grid starting at
/// `(inner_x0, groups_top)`, row-major: `[0, 1]` in row 0, `[2, 3]` in
/// row 1.
#[allow(
    clippy::too_many_lines,
    reason = "one linear renderer keeps the compact and two-column geometry paired with the same canonical shortcut rows"
)]
fn draw_groups(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    inner_x0: f64,
    inner_w: f64,
    groups_top: f64,
    compact: bool,
) {
    let tokens = &theme.tokens;
    let scale = f64::from(theme.text_scale);
    let gutter = f64::from(tokens.spacing.s6) * scale;
    let col_w = if compact {
        inner_w
    } else {
        ((inner_w - gutter) / 2.0).max(0.0)
    };
    let col_left_x = inner_x0;
    let col_right_x = inner_x0 + col_w + gutter;

    let group_size = GROUP_SIZE_TOKEN(theme);
    let row_size = ROW_SIZE_TOKEN(theme);

    // Find the tallest group in row 0 to figure where row 1 starts.
    let column_top_for = |idx: usize| -> f64 {
        if compact {
            return groups_top
                + GROUPS
                    .iter()
                    .take(idx)
                    .map(|group| {
                        group_block_height(group.rows.len(), group_size, row_size, scale, true)
                            + gutter
                    })
                    .sum::<f64>();
        }
        if idx < 2 {
            groups_top
        } else {
            let h0 = GROUPS.first().map_or(0.0, |g| {
                group_block_height(g.rows.len(), group_size, row_size, scale, false)
            });
            let h1 = GROUPS.get(1).map_or(0.0, |g| {
                group_block_height(g.rows.len(), group_size, row_size, scale, false)
            });
            groups_top + h0.max(h1) + f64::from(tokens.spacing.s6) * scale
        }
    };

    for (idx, group) in GROUPS.iter().enumerate() {
        let x = if compact || idx % 2 == 0 {
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

        let rows_top = y + f64::from(group_size) * scale + GROUP_HEADING_TO_ROWS_GAP * scale;
        for (i, (label, key)) in group.rows.iter().enumerate() {
            let row_height = if compact {
                COMPACT_ROW_HEIGHT
            } else {
                ROW_HEIGHT
            };
            let row_y = rows_top + (i as f64) * row_height * scale;
            text_renderer.draw_text(
                scene,
                &ui::i18n::I18nManager::lookup(label, None),
                row_size,
                tokens.colors.text_dim,
                transform * Affine::translate((x, row_y)),
                compact.then_some(num::f32_of_f64(inner_w)),
            );
            // Shortcut chip: right-aligned in the column.
            let (kw, _) = text_renderer.measure(key, row_size, None);
            // Keep the chip inside its column even when a scaled glyph's
            // measured advance lands exactly on the right edge.
            let kx = if compact {
                x + 6.0 * scale
            } else {
                x + col_w - f64::from(kw) - 4.0 * scale
            };
            let key_y = if compact { row_y + 30.0 * scale } else { row_y };
            let chip_pad_x = 6.0 * scale;
            let chip_pad_y = 2.0 * scale;
            let chip = Rect::new(
                kx - chip_pad_x,
                key_y - chip_pad_y,
                kx + f64::from(kw) + chip_pad_x,
                key_y + f64::from(row_size) * scale + chip_pad_y,
            );
            // A kbd chip: one derived hairline step off the frame's ground
            // (`ModalFrame` paints `surfaces.raised`), so the key registers
            // on every preset — `lift(0.72)` landed on the light frame's own
            // colour — with a crisp edge derived from that fill (HIG §9.5).
            let ground = tokens.colors.surfaces.raised;
            let step = ui::surface::edge_on(ground);
            let kbd_fill = ui::tokens::colors::over(ground, step, step.components[3]);
            ui::surface::card(
                scene,
                transform,
                chip,
                f64::from(tokens.radii.sm) * scale,
                kbd_fill,
            );
            text_renderer.draw_text(
                scene,
                key,
                row_size,
                tokens.colors.text_primary,
                transform * Affine::translate((kx, key_y)),
                None,
            );
        }
    }
}

impl Widget for ShortcutsOverlay {
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
        let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../crates/sdk/ui-core/assets/fonts"),
            "en-US",
        );
        let mut text_renderer = TextRenderer::with_font_catalog(&catalog).unwrap();
        o.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut text_renderer,
            &Theme::dark(),
        );
    }

    #[test]
    fn shortcuts_overlay_opens_closes_and_consumes_only_while_visible() {
        eprintln!("phase set_visible_flips_and_marks_dirty");
        {
            let mut o = ShortcutsOverlay::new();
            assert!(!o.is_visible());
            o.mark_drawn();
            assert!(!o.needs_redraw());
            o.set_visible(true);
            assert!(o.is_visible());
            assert!(o.needs_redraw());
        }

        eprintln!("phase escape_closes_overlay");
        {
            let mut o = ShortcutsOverlay::new();
            o.set_visible(true);
            let consumed = o.handle_key(&esc_event(), 1.0);
            assert!(consumed);
            assert!(!o.is_visible());
        }

        eprintln!("phase invisible_overlay_does_not_consume_events");
        {
            let mut o = ShortcutsOverlay::new();
            assert!(!o.handle_key(&esc_event(), 1.0));
        }

        eprintln!("phase shared_modal_hit_testing_preserves_inside_and_dismiss_paths");
        {
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
    }

    #[expect(
        clippy::float_cmp,
        reason = "End and Home assign max_scroll_y and 0.0 exactly; exact equality is the property under test"
    )]
    #[test]
    fn shortcuts_overlay_is_an_accessible_scrollable_dialog_at_high_text() {
        eprintln!("phase visible_overlay_is_an_accessible_focus_trapping_dialog");
        {
            let mut o = ShortcutsOverlay::new();
            o.set_visible(true);
            assert!(o.traps_focus());
            let node = o.accessibility().expect("dialog semantics");
            assert_eq!(node.role, ui::a11y::Role::Dialog);
            assert!(node.state.modal);
            assert!(!node.state.hidden);
        }

        eprintln!("phase keyboard_scrolling_reaches_both_ends_without_closing_help");
        {
            let mut overlay = ShortcutsOverlay::new();
            overlay.set_viewport(Rect::new(0.0, 0.0, 390.0, 844.0));
            overlay.set_visible(true);
            paint(&mut overlay);
            assert!(overlay.max_scroll_y > 0.0);
            for key in [NamedKey::PageDown, NamedKey::End, NamedKey::Home] {
                let mut event = esc_event();
                if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                    event.logical_key = Key::Named(key);
                }
                assert!(overlay.handle_key(&event, 1.0));
                assert!(overlay.is_visible());
                match key {
                    NamedKey::End => assert_eq!(overlay.scroll_y, overlay.max_scroll_y),
                    NamedKey::Home => assert_eq!(overlay.scroll_y, 0.0),
                    _ => assert!(overlay.scroll_y > 0.0),
                }
            }
        }

        eprintln!("phase high_text_shortcuts_are_bounded_and_scrollable_without_partial_rows");
        {
            let theme = Theme::dark().with_text_scale(2.0);
            let groups_top = 180.0;
            let groups_bottom = 560.0;
            let limit = shortcut_scroll_limit(groups_top, groups_bottom, &theme, true);
            assert!(limit > 0.0, "2× shortcut reference needs a scroll range");
            assert!(groups_bottom - groups_top >= ROW_HEIGHT * 2.0);

            let mut overlay = ShortcutsOverlay::new();
            overlay.visible = true;
            overlay.max_scroll_y = limit;
            overlay.text_scale = 2.0;
            overlay.scroll_y = 0.0;
            let next = (overlay.scroll_y + ROW_HEIGHT * overlay.text_scale)
                .clamp(0.0, overlay.max_scroll_y);
            assert!(next > 0.0);
            assert!(next <= overlay.max_scroll_y);
        }
    }
}
