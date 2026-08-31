//! [`DiskRow`] — a full-width selectable disk row.
//!
//! `ui::Button` is fixed at 120×40 px, far too narrow for a disk label
//! like `/dev/sda — 238.5G — Samsung SSD 860 EVO`. `DiskRow` is a leaf
//! widget that fills the card width, shows the device on one line and the
//! model on a dimmer second line, and is **focusable** (so keyboard
//! arrow-nav + startup auto-focus reach it). Selecting it — by click or
//! by pressing Space/Enter while focused — sets a CRDT boolean flag the
//! [`crate::installer_app::InstallerApp`] consumes on the next `sync`
//! (rising-edge), exactly like `ui::Button`'s `on_click_path`.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::disks::Disk;

const ROW_HEIGHT: f32 = 54.0;
const PAD: f64 = 12.0;
const NAME_SIZE: f32 = 16.0;
const MODEL_SIZE: f32 = 12.0;

pub struct DiskRow {
    disk: Disk,
    /// CRDT path of the boolean flag raised when this row is chosen.
    select_path: String,
    /// Whether this row is the currently-selected disk (drives the
    /// highlight). Set by the app each time it rebuilds the composition.
    selected: bool,
    rect: Rect,
    is_hovered: bool,
    is_focused: bool,
}

impl DiskRow {
    pub fn new(disk: Disk, select_path: &str, selected: bool) -> Self {
        Self {
            disk,
            select_path: select_path.to_string(),
            selected,
            rect: Rect::ZERO,
            is_hovered: false,
            is_focused: false,
        }
    }

    /// Raise the selection flag — consumed (rising-edge) by the app.
    fn fire(&self, state: &mut AutoCommit, root: &ObjId) {
        let _ = state.put(root, self.select_path.as_str(), true);
    }
}

impl Widget for DiskRow {
    fn sync(&mut self, _state: &mut AutoCommit, _root: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` only errors for a corrupted tree; `Widget::build_layout`
        // returns a bare `NodeId` across every implementor, so surfacing a
        // theoretical error here would mean threading a `Result` through the
        // whole toolkit for a branch that can't be taken.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh style cannot fail")]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(ROW_HEIGHT),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `layout` only errors for a `NodeId` from a different tree, which
        // never happens here since `node` is always the id this same widget
        // received from its own `build_layout` call.
        #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let bg = if self.selected {
            theme.bg_active
        } else if self.is_hovered {
            theme.bg_hover
        } else {
            theme.bg_primary
        };
        let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
        scene.fill(Fill::NonZero, transform, bg, None, &shape);

        let (stroke_w, stroke_color) = if self.selected {
            (2.0, theme.accent_primary)
        } else if self.is_focused {
            (2.0, theme.border_focus)
        } else {
            (1.0, theme.border_standard)
        };
        scene.stroke(
            &Stroke::new(stroke_w),
            transform,
            stroke_color,
            None,
            &shape,
        );

        // Line 1: device path + size. Line 2: model (dim), if present.
        let head = format!("{}    {}", self.disk.name, self.disk.size);
        let y1 = self.rect.y0 + PAD * 0.6;
        text_renderer.draw_text(
            scene,
            &head,
            NAME_SIZE,
            theme.text_primary,
            transform.pre_translate(Vec2::new(self.rect.x0 + PAD, y1)),
            None,
        );
        if !self.disk.model.is_empty() {
            let y2 = y1 + f64::from(NAME_SIZE) + 4.0;
            text_renderer.draw_text(
                scene,
                &self.disk.model,
                MODEL_SIZE,
                theme.text_dim,
                transform.pre_translate(Vec2::new(self.rect.x0 + PAD, y2)),
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
                let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_hovered {
                    *focus_target = Some(node_id);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_hovered {
                    self.fire(state, root);
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                // Space selects when focused. (Enter is reserved for the
                // app-level "advance" handler so it can't double-fire.)
                if matches!(logical_key, Key::Named(NamedKey::Space)) {
                    self.fire(state, root);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "DiskRow"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.disk.name.as_str())
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
