//! [`SegChoice`] — a segmented (single-choice) selector over a published
//! action.
//!
//! `ui::ToggleGroup` keeps its selection on the widget, which does not
//! survive the installer's per-step widget-tree rebuilds. `SegChoice` works
//! like [`crate::setup::destination::disk_row::DiskRow`]: each segment owns the [`Control`]
//! choosing it, clicking (or arrowing onto) a segment fires that control,
//! and the [`crate::setup::destination::installer_app::InstallerApp`] runs the action and feeds
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
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ControlSize, SegmentedFace, ToggleGroup, Widget};

use crate::setup::destination::control::Control;

/// The strip's control size: the segmented switch HIG §9.6 sizes for a
/// field.
const SIZE: ControlSize = ControlSize::Regular;

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
}

impl Widget for SegChoice {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let height = ui::num::f32_of_f64(SegmentedFace::height(SIZE, theme));
        taffy.widget_leaf(
            crate::setup::destination::full_width_row(height),
            node_map,
            self,
        )
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
        let labels: Vec<&str> = self.labels.iter().map(String::as_str).collect();
        // The toolkit's segmented strip: a raised track, a concentric thumb
        // on the chosen segment and the theme's ink for it.
        ToggleGroup::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &SegmentedFace::new(&self.segment_rects, &labels, self.selected)
                .control_size(SIZE)
                .focused(self.is_focused),
        );
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
    use crate::setup::destination::control::{Press, Presses};
    use crate::setup::destination::effects::DestinationEffect;

    use ui::synthetic_input::named_key;

    #[test]
    fn every_segment_label_is_painted_in_every_theme() {
        use ui::theme::Preset;

        let labels = vec!["btrfs".into(), "ext4".into()];
        for preset in Preset::ALL {
            let theme = Theme::preset(preset);
            let mut strip = SegChoice::new(labels.clone(), Vec::new(), 0);
            let mut taffy = TaffyTree::new();
            let node = strip.build_layout(&mut taffy, &theme, &mut HashMap::new());
            taffy
                .compute_layout(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(320.0),
                        height: taffy::AvailableSpace::MaxContent,
                    },
                )
                .expect("the strip lays out");
            strip.apply_layout(&taffy, node, Point::ORIGIN);
            let mut scene = Scene::new();
            strip.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::default(),
            );
            assert!(
                scene.encoding().n_paths >= 2,
                "{}: the strip and its thumb are painted",
                preset.id()
            );
            assert_eq!(
                scene.encoding().resources.glyph_runs.len(),
                2,
                "{}: every segment label is painted in its own run",
                preset.id()
            );
        }
    }

    #[test]
    fn arrowing_fires_the_neighbouring_segments_action() {
        let presses = Presses::default();
        let controls = ["btrfs", "ext4", "xfs"]
            .iter()
            .map(|fs| {
                presses.control(Press::new(DestinationEffect::SetFilesystem {
                    filesystem: (*fs).to_owned(),
                }))
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
            assert!(
                press.effect
                    == DestinationEffect::SetFilesystem {
                        filesystem: expected.to_owned()
                    }
            );
        }
        assert_eq!(doc.length(ROOT), 0, "a segment never writes the document");
    }
}
