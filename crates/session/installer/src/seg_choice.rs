//! [`SegChoice`] — a segmented (single-choice) selector over a published
//! action.
//!
//! `ui::ToggleGroup` keeps its selection on the widget, which does not
//! survive the installer's per-step widget-tree rebuilds. `SegChoice` works
//! like [`crate::disk_row::DiskRow`]: each segment owns the [`Control`]
//! choosing it, clicking (or arrowing onto) a segment fires that control,
//! and the [`crate::installer_app::InstallerApp`] runs the action and feeds
//! the resulting index back in as `selected` when it rebuilds. The widget
//! itself is stateless beyond the highlight.
//!
//! It is focusable: Left/Right move the selection while focused; clicks
//! pick a segment directly. All colours come from the [`Theme`].

use std::cell::Cell;
use std::collections::HashMap;
use ui::widget::TaffyTreeExt;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::control::Control;

const HEIGHT: f32 = 40.0;
const LABEL_SIZE: f32 = 14.0;

pub struct SegChoice {
    labels: Vec<String>,
    /// Per-segment control choosing it; index-aligned with `labels`.
    controls: Vec<Control>,
    selected: usize,
    /// Stable name for E2E click targeting (`Widget::test_id`). Defaults to
    /// `filesystem_choice`; override via [`SegChoice::with_test_id`] when the
    /// same widget is reused for another choice (e.g. encryption).
    test_id: String,
    rect: Rect,
    segment_rects: Vec<Rect>,
    is_focused: bool,
    last_cursor: Cell<Point>,
}

impl SegChoice {
    pub fn new(labels: Vec<String>, controls: Vec<Control>, selected: usize) -> Self {
        let n = labels.len();
        Self {
            labels,
            controls,
            selected: if n == 0 {
                0
            } else {
                selected.min(n.saturating_sub(1))
            },
            test_id: "filesystem_choice".to_string(),
            rect: Rect::ZERO,
            segment_rects: Vec::new(),
            is_focused: false,
            last_cursor: Cell::new(Point::ZERO),
        }
    }

    /// Override the E2E `test_id` (default `filesystem_choice`).
    #[must_use]
    pub fn with_test_id(mut self, id: &str) -> Self {
        self.test_id = id.to_string();
        self
    }

    fn fire(&self, idx: usize) {
        if let Some(control) = self.controls.get(idx) {
            control.fire();
        }
    }

    /// The `i`th segment's fill and label ink. The chosen segment takes the
    /// accent with the theme's own label for it: the accent is light in high
    /// contrast and on a soft dark theme, where a white label vanishes.
    fn segment_face(&self, i: usize, theme: &Theme) -> (Color, Color) {
        let colors = &theme.tokens.colors;
        if i == self.selected {
            (colors.intent.accent, colors.intent.on_accent)
        } else {
            (colors.surfaces.raised, theme.text_primary)
        }
    }
}

impl Widget for SegChoice {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(crate::full_width_row(HEIGHT), node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        self.segment_rects.clear();
        let n = self.labels.len();
        if n == 0 {
            return;
        }
        let seg_w = self.rect.width() / ui::num::f64_of_usize(n);
        for i in 0..n {
            let sx0 = self.rect.x0 + ui::num::f64_of_usize(i) * seg_w;
            let sx1 = if i == n.saturating_sub(1) {
                self.rect.x1
            } else {
                self.rect.x0 + ui::num::f64_of_usize(i.saturating_add(1)) * seg_w
            };
            self.segment_rects
                .push(Rect::new(sx0, self.rect.y0, sx1, self.rect.y1));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.labels.is_empty() || self.segment_rects.len() != self.labels.len() {
            return;
        }
        let radius = theme.radius_standard;
        let outer = RoundedRect::from_rect(self.rect, radius);

        // Per-segment fills, clipped to the rounded outer shape.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &outer);
        for (i, seg) in self.segment_rects.iter().enumerate() {
            let (fill, _) = self.segment_face(i, theme);
            scene.fill(Fill::NonZero, transform, fill, None, seg);
        }
        scene.pop_layer();

        // Interior separators.
        for seg in self
            .segment_rects
            .iter()
            .take(self.labels.len().saturating_sub(1))
        {
            let x = seg.x1;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &Line::new(Point::new(x, self.rect.y0), Point::new(x, self.rect.y1)),
            );
        }

        // Outer border (focus ring when focused).
        let (bw, bc) = if self.is_focused {
            (2.0, theme.border_focus)
        } else {
            (1.0, theme.border_standard)
        };
        scene.stroke(&Stroke::new(bw), transform, bc, None, &outer);

        // Labels.
        for (i, (seg, label)) in self
            .segment_rects
            .iter()
            .zip(self.labels.iter())
            .enumerate()
        {
            let (_, color) = self.segment_face(i, theme);
            let (tw, th) = text_renderer.measure(label, LABEL_SIZE, None);
            let tx = seg.center().x - f64::from(tw) / 2.0;
            let ty = seg.y0 + (seg.height() - f64::from(th)) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                LABEL_SIZE,
                color,
                transform.pre_translate(Vec2::new(tx, ty)),
                None,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor.set(ctx.logical(position));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pos = self.last_cursor.get();
                for (i, seg) in self.segment_rects.iter().enumerate() {
                    if seg.contains(pos) {
                        *ctx.focus_target = Some(ctx.node_id);
                        *ctx.handled = true;
                        if i != self.selected {
                            self.fire(i);
                        }
                        break;
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(key),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                // Left/Right move the selection; consume so the App's
                // directional focus-nav fallback doesn't also fire.
                let target = match key {
                    NamedKey::ArrowLeft if self.selected > 0 => {
                        Some(self.selected.saturating_sub(1))
                    }
                    NamedKey::ArrowRight if self.selected.saturating_add(1) < self.labels.len() => {
                        Some(self.selected.saturating_add(1))
                    }
                    _ => None,
                };
                if let Some(t) = target {
                    self.fire(t);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "SegChoice"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }
}

impl ui::widget::controls::ActionControls for SegChoice {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        // Every segment runs the same action with its own argument.
        self.controls
            .first()
            .map(|control| control.metadata(&self.test_id))
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use automerge::{AutoCommit, ROOT, ReadDoc};

    use super::*;
    use crate::control::{Press, Presses};

    use ui::synthetic_input::named_key;

    #[test]
    fn every_segment_label_reads_on_its_fill_in_every_theme() {
        use ui::theme::Preset;
        use ui::tokens::colors::ON_FILL_MIN_CONTRAST;

        let labels = vec!["btrfs".into(), "ext4".into()];
        let choice = SegChoice::new(labels, Vec::new(), 0);
        for preset in Preset::ALL {
            let theme = Theme::preset(preset);
            for i in 0..2 {
                let (fill, ink) = choice.segment_face(i, &theme);
                let ratio = ui::a11y::contrast_ratio(ink, fill);
                assert!(
                    ratio >= ON_FILL_MIN_CONTRAST,
                    "{}: segment {i} label reads at {ratio:.2}:1 on its fill",
                    preset.id()
                );
            }
        }
    }

    #[test]
    fn arrowing_fires_the_neighbouring_segments_action() {
        let presses = Presses::default();
        let controls = ["btrfs", "ext4", "xfs"]
            .iter()
            .map(|fs| {
                presses.control(Press::new(
                    "installer.options",
                    "set-filesystem",
                    serde_json::json!({ "filesystem": fs }),
                ))
            })
            .collect();
        let labels = vec!["btrfs".into(), "ext4".into(), "xfs".into()];
        let mut choice = SegChoice::new(labels, controls, 1);
        choice.set_focus(true);
        let mut doc = AutoCommit::new();
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = choice.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let mut keys = ui::testing::Dispatch::new()
            .doc(&mut doc)
            .layout(&taffy, node);
        for (key, expected) in [
            (NamedKey::ArrowRight, "xfs"),
            (NamedKey::ArrowLeft, "btrfs"),
        ] {
            let handled = keys.send(&mut choice, &named_key(key)).handled;
            assert!(handled);
            let press = presses.take().expect("a segment action");
            assert_eq!(press.address.action, "set-filesystem");
            assert_eq!(press.args["filesystem"], expected);
        }
        assert_eq!(doc.length(ROOT), 0, "a segment never writes the document");
    }
}
