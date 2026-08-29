//! A focusable, accessible sidebar list row.
//!
//! The collections tree, saved-request rows, and history rows used to be
//! painted straight into the sidebar and hit-tested from a private
//! `Vec<(Rect, …)>` table, so the keyboard could not reach them and a screen
//! reader could not see them. `NavRow` is the retained replacement: a real
//! [`Widget`] with a [`Role::Button`] accessibility node, keyboard/AT
//! activation, and a themed focus ring. The app hand-positions each row over
//! the sidebar it still paints, exactly as it does for the other shared
//! controls.
//!
//! It preserves the original per-row look — a disclosure caret + count for a
//! collection header, a coloured method tag for a saved request, a method
//! tag + status for a history entry — so the sidebar renders identically
//! while becoming fully navigable.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use crate::model::HttpMethod;
use crate::palette::{method_color, method_short, status_class, status_color};

/// Row height in logical px — matches the sidebar's original `ROW_H`.
pub const ROW_H: f64 = 30.0;

/// Which of the three sidebar row shapes this row paints.
#[derive(Clone)]
pub enum NavKind {
    /// A foldable collection header: disclosure caret + name + request count.
    Collection { expanded: bool, count: usize },
    /// A saved request: coloured method tag + name, highlighted when active.
    Request { method: HttpMethod, selected: bool },
    /// A history entry: coloured method tag + path + coloured status code.
    History { method: HttpMethod, status: u16 },
}

/// A single focusable sidebar row.
pub struct NavRow {
    id: String,
    kind: NavKind,
    /// The visible label (collection name, request name, or history path).
    label: String,
    /// The already-composed screen-reader name (e.g. "GET List users").
    accessible_name: String,
    is_focused: bool,
    rect: Rect,
    /// Last cursor position (logical), tracked from `CursorMoved` so a
    /// left-press can self-gate to the row under the pointer — the container
    /// forwards the whole event stream to every row.
    last_cursor: Point,
    /// Fired on a completed activation — left-click, Enter/Space, or an AT
    /// `Activate` action. The app supplies a closure that pushes the row's
    /// command onto its sink.
    on_activate: Option<Box<dyn FnMut()>>,
}

impl NavRow {
    pub fn new(
        id: impl Into<String>,
        kind: NavKind,
        label: impl Into<String>,
        accessible_name: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            label: label.into(),
            accessible_name: accessible_name.into(),
            is_focused: false,
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            on_activate: None,
        }
    }

    /// Attach the activation handler (mirrors `Button::on_click`).
    pub fn on_activate<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_activate = Some(Box::new(f));
        self
    }

    /// Place the row at an absolute rect — the app positions every row over
    /// the sidebar it paints.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
    }

    fn activate(&mut self) {
        if let Some(cb) = self.on_activate.as_mut() {
            cb();
        }
    }
}

impl Widget for NavRow {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // A leaf node so the row is registered in `node_map` (for focus
        // order + the a11y tree); its taffy geometry is unused because the
        // app hand-positions it via `apply_layout_external`.
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(ROW_H as f32),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
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
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        let r = self.rect;
        let y = r.y0;
        let inner_w = r.width();

        // Focus ring — a rounded outline that reads at a glance and is the
        // one visible cue a hand-painted row never had.
        if self.is_focused {
            let ring = RoundedRect::from_rect(
                Rect::new(r.x0, r.y0 + 1.0, r.x1, r.y1 - 1.0),
                f64::from(theme.tokens.radii.md),
            );
            scene.stroke(&Stroke::new(2.0), t, theme.border_focus, None, &ring);
        }

        match &self.kind {
            NavKind::Collection { expanded, count } => {
                let caret = if *expanded { "\u{25BE}" } else { "\u{25B8}" };
                tr.draw_text(
                    scene,
                    caret,
                    12.0,
                    c.text_dim,
                    t * Affine::translate((r.x0 + 6.0, y + 7.0)),
                    None,
                );
                tr.draw_text_weighted(
                    scene,
                    &self.label,
                    theme.tokens.typography.scale.base,
                    c.text_primary,
                    t * Affine::translate((r.x0 + 24.0, y + 6.0)),
                    Some((inner_w - 60.0) as f32),
                    TextWeight::Semibold,
                );
                let cs = count.to_string();
                let (cw, _) = tr.measure(&cs, 11.0, None);
                tr.draw_text(
                    scene,
                    &cs,
                    11.0,
                    c.text_dim,
                    t * Affine::translate((r.x1 - f64::from(cw) - 6.0, y + 8.0)),
                    None,
                );
            }
            NavKind::Request { method, selected } => {
                if *selected {
                    let shape = RoundedRect::from_rect(
                        Rect::new(r.x0, r.y0 + 2.0, r.x1, r.y1 - 2.0),
                        f64::from(theme.tokens.radii.md),
                    );
                    scene.fill(
                        Fill::NonZero,
                        t,
                        theme.accent_primary.with_alpha_factor(0.18),
                        None,
                        &shape,
                    );
                }
                tr.draw_text_weighted(
                    scene,
                    method_short(*method),
                    10.0,
                    method_color(theme, *method),
                    t * Affine::translate((r.x0 + 28.0, y + 9.0)),
                    None,
                    TextWeight::Bold,
                );
                tr.draw_text(
                    scene,
                    &self.label,
                    theme.tokens.typography.scale.sm,
                    if *selected {
                        c.text_primary
                    } else {
                        c.text_dim
                    },
                    t * Affine::translate((r.x0 + 64.0, y + 8.0)),
                    Some((inner_w - 72.0) as f32),
                );
            }
            NavKind::History { method, status } => {
                tr.draw_text_weighted(
                    scene,
                    method_short(*method),
                    10.0,
                    method_color(theme, *method),
                    t * Affine::translate((r.x0 + 6.0, y + 9.0)),
                    None,
                    TextWeight::Bold,
                );
                tr.draw_text(
                    scene,
                    &self.label,
                    theme.tokens.typography.scale.sm,
                    c.text_dim,
                    t * Affine::translate((r.x0 + 46.0, y + 8.0)),
                    Some((inner_w - 90.0) as f32),
                );
                let st = status.to_string();
                let (sw, _) = tr.measure(&st, 11.0, None);
                tr.draw_text_weighted(
                    scene,
                    &st,
                    11.0,
                    status_color(theme, status_class(*status)),
                    t * Affine::translate((r.x1 - f64::from(sw) - 6.0, y + 8.0)),
                    None,
                    TextWeight::Semibold,
                );
            }
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        if *handled {
            return;
        }
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.last_cursor) => {
                // `MouseInput` carries no position, so gate on the cursor we
                // tracked from `CursorMoved` — only the row under the pointer
                // activates.
                *focus_target = Some(node_id);
                self.activate();
                *handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                let is_activate = matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space))
                    || matches!(key, Key::Character(s) if s.as_str() == " ");
                if is_activate {
                    self.activate();
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "NavRow"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then_some(winit::window::CursorIcon::Pointer)
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

    fn accessibility(&self) -> Option<AccessNode> {
        let mut state = AccessState::new().focusable(true).focused(self.is_focused);
        match &self.kind {
            NavKind::Collection { expanded, .. } => {
                state = state.expanded(*expanded);
            }
            NavKind::Request { selected, .. } => {
                state = state.selected(*selected);
            }
            NavKind::History { .. } => {}
        }
        Some(
            AccessNode::new(Role::Button)
                .with_name(self.accessible_name.clone())
                .with_state(state),
        )
    }

    fn perform_action(&mut self, action: &Action, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        if matches!(action, Action::Activate) {
            self.activate();
            true
        } else {
            false
        }
    }
}

// A tiny unused helper kept to document that a caret glyph is a triangle;
// `draw` uses the unicode caret directly for parity with the original.
#[allow(dead_code)]
fn draw_caret(scene: &mut Scene, t: Affine, center: Point, color: Color, expanded: bool) {
    let mut p = BezPath::new();
    let (cx, cy) = (center.x, center.y);
    if expanded {
        p.move_to((cx - 3.0, cy - 2.0));
        p.line_to((cx + 3.0, cy - 2.0));
        p.line_to((cx, cy + 3.0));
    } else {
        p.move_to((cx - 2.0, cy - 3.0));
        p.line_to((cx + 3.0, cy));
        p.line_to((cx - 2.0, cy + 3.0));
    }
    p.close_path();
    scene.fill(Fill::NonZero, t, color, None, &p);
}
