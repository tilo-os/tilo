//! [`SegChoice`] — a CRDT-backed segmented (single-choice) selector.
//!
//! `ui::ToggleGroup` keeps its selection on the widget and reports it via a
//! `FnMut(usize)` callback — neither of which survives the installer's
//! per-step widget-tree rebuilds nor reaches the CRDT. `SegChoice` follows
//! the same rising-edge flag pattern as [`crate::disk_row::DiskRow`]: each
//! segment owns a boolean flag path, clicking (or arrowing onto) a segment
//! raises its flag, and the [`crate::installer_app::InstallerApp`] consumes
//! the rising edge on the next `sync` and feeds the resolved index back in
//! as `selected` when it rebuilds. The widget itself is stateless beyond
//! the highlight.
//!
//! It is focusable: Left/Right move the selection while focused; clicks
//! pick a segment directly. All colours come from the [`Theme`].

use std::cell::Cell;
use std::collections::HashMap;

use automerge::{transaction::Transactable, AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

const HEIGHT: f32 = 40.0;
const LABEL_SIZE: f32 = 14.0;

pub struct SegChoice {
    labels: Vec<String>,
    /// Per-segment CRDT flag path; index-aligned with `labels`.
    select_paths: Vec<String>,
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
    pub fn new(labels: Vec<String>, select_paths: Vec<String>, selected: usize) -> Self {
        let n = labels.len();
        Self {
            labels,
            select_paths,
            selected: if n == 0 { 0 } else { selected.min(n - 1) },
            test_id: "filesystem_choice".to_string(),
            rect: Rect::ZERO,
            segment_rects: Vec::new(),
            is_focused: false,
            last_cursor: Cell::new(Point::ZERO),
        }
    }

    /// Override the E2E `test_id` (default `filesystem_choice`).
    pub fn with_test_id(mut self, id: &str) -> Self {
        self.test_id = id.to_string();
        self
    }

    fn fire(&self, idx: usize, state: &mut AutoCommit, root: &ObjId) {
        if let Some(path) = self.select_paths.get(idx) {
            let _ = state.put(root, path.as_str(), true);
        }
    }
}

impl Widget for SegChoice {
    fn sync(&mut self, _state: &mut AutoCommit, _root: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(HEIGHT),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        self.segment_rects.clear();
        let n = self.labels.len();
        if n == 0 {
            return;
        }
        let seg_w = self.rect.width() / n as f64;
        for i in 0..n {
            let sx0 = self.rect.x0 + (i as f64) * seg_w;
            let sx1 = if i == n - 1 {
                self.rect.x1
            } else {
                self.rect.x0 + ((i + 1) as f64) * seg_w
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
            let fill = if i == self.selected {
                theme.accent_primary
            } else {
                theme.bg_primary
            };
            scene.fill(Fill::NonZero, transform, fill, None, seg);
        }
        scene.pop_layer();

        // Interior separators.
        for i in 0..self.labels.len().saturating_sub(1) {
            let x = self.segment_rects[i].x1;
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
        for (i, seg) in self.segment_rects.iter().enumerate() {
            let color = if i == self.selected {
                Color::WHITE
            } else {
                theme.text_primary
            };
            let (tw, th) = text_renderer.measure(&self.labels[i], LABEL_SIZE, None);
            let tx = seg.center().x - tw as f64 / 2.0;
            let ty = seg.y0 + (seg.height() - th as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                &self.labels[i],
                LABEL_SIZE,
                color,
                transform * Affine::translate((tx, ty)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor.set(Point::new(
                    position.x / scale_factor,
                    position.y / scale_factor,
                ));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pos = self.last_cursor.get();
                for (i, seg) in self.segment_rects.iter().enumerate() {
                    if seg.contains(pos) {
                        *focus_target = Some(node_id);
                        *handled = true;
                        if i != self.selected {
                            self.fire(i, state, root);
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
                    NamedKey::ArrowLeft if self.selected > 0 => Some(self.selected - 1),
                    NamedKey::ArrowRight if self.selected + 1 < self.labels.len() => {
                        Some(self.selected + 1)
                    }
                    _ => None,
                };
                if let Some(t) = target {
                    self.fire(t, state, root);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
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
