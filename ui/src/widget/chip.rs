//! `Chip` — a compact interactive pill for tags, filters, and tokens. Distinct
//! from [`super::Badge`] (a tiny non-interactive count/dot): a chip carries a
//! label and can be **selectable** (a filter that toggles on/off, tinting to
//! the accent when active) and/or **removable** (a trailing "×" that removes a
//! token, e.g. a recipient or an applied filter).
//!
//! Both interactions surface through the toolkit's drain channel —
//! [`Chip::take_toggled`] and [`Chip::take_removed`] — rather than callbacks,
//! so a CRDT-native host reads them each frame and mutates its own model.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

const HEIGHT: f64 = 28.0;
const PAD_X: f64 = 12.0;
const GAP: f64 = 6.0;
const DOT_D: f64 = 8.0;
const X_D: f64 = 14.0;

/// A compact pill: label + optional leading dot + optional trailing "×",
/// selectable and/or removable.
pub struct Chip {
    label: String,
    selectable: bool,
    selected: bool,
    removable: bool,
    dot: Option<Color>,
    toggled: bool,
    removed: bool,
    rect: Rect,
    x_rect: Rect,
    last_cursor: Point,
    label_w: f64,
}

impl Chip {
    /// A plain (informational) chip with a label.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            selectable: false,
            selected: false,
            removable: false,
            dot: None,
            toggled: false,
            removed: false,
            rect: Rect::ZERO,
            x_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            label_w: 0.0,
        }
    }

    /// Make the chip a toggle (a filter). Clicking the body flips selection.
    pub fn selectable(mut self) -> Self {
        self.selectable = true;
        self
    }

    /// Set the initial selected state (implies [`selectable`](Self::selectable)).
    pub fn selected(mut self, on: bool) -> Self {
        self.selectable = true;
        self.selected = on;
        self
    }

    /// Show a trailing "×" that removes the chip when clicked.
    pub fn removable(mut self) -> Self {
        self.removable = true;
        self
    }

    /// Add a small leading colour dot (e.g. a calendar/category colour).
    pub fn with_dot(mut self, color: Color) -> Self {
        self.dot = Some(color);
        self
    }

    /// Whether the chip is currently selected.
    pub fn is_selected(&self) -> bool {
        self.selected
    }

    /// Programmatically set selection (host syncing from its model).
    pub fn set_selected(&mut self, on: bool) {
        self.selected = on;
    }

    /// Drain the "selection toggled" signal (fires once per click on the body
    /// of a selectable chip). Read `is_selected()` for the new value.
    pub fn take_toggled(&mut self) -> bool {
        std::mem::take(&mut self.toggled)
    }

    /// Drain the "× clicked / remove me" signal (fires once).
    pub fn take_removed(&mut self) -> bool {
        std::mem::take(&mut self.removed)
    }

    /// The "×" hit rect (empty until laid out + drawn). Exposed for tests.
    pub fn remove_rect(&self) -> Rect {
        self.x_rect
    }

    /// Intrinsic width for the current label + affordances.
    fn intrinsic_width(&self, text_renderer: &mut TextRenderer, theme: &Theme) -> f64 {
        let (w, _) = text_renderer.measure(&self.label, theme.tokens.typography.scale.sm, None);
        let mut total = PAD_X * 2.0 + w as f64;
        if self.dot.is_some() {
            total += DOT_D + GAP;
        }
        if self.removable {
            total += GAP + X_D;
        }
        total
    }
}

impl Widget for Chip {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Approximate width from a fixed advance; measure() refines it.
        let approx = PAD_X * 2.0
            + self.label.chars().count() as f64 * (theme.tokens.typography.scale.sm as f64 * 0.6)
            + if self.dot.is_some() { DOT_D + GAP } else { 0.0 }
            + if self.removable { GAP + X_D } else { 0.0 };
        let node = taffy
            .new_leaf(Style {
                flex_shrink: 0.0,
                size: taffy::Size { width: length(approx as f32), height: length(HEIGHT as f32) },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn measure(
        &mut self,
        _k: taffy::Size<Option<f32>>,
        _a: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        taffy::Size {
            width: self.intrinsic_width(text_renderer, &Theme::dark()) as f32,
            height: HEIGHT as f32,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
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
        if self.rect.width() <= 0.0 {
            return;
        }
        let accent = theme.accent_primary;
        let selected = self.selectable && self.selected;

        // Pill surface.
        let pill = RoundedRect::from_rect(self.rect, theme.tokens.radii.full as f64);
        let bg = if selected {
            accent.with_alpha_factor(0.18)
        } else {
            theme.tokens.colors.surface_white_5
        };
        let border = if selected { accent } else { theme.border_standard };
        scene.fill(Fill::NonZero, transform, bg, None, &pill);
        scene.stroke(&Stroke::new(1.0), transform, border, None, &pill);

        let mut x = self.rect.x0 + PAD_X;
        let cy = self.rect.center().y;

        // Leading dot.
        if let Some(dot) = self.dot {
            scene.fill(
                Fill::NonZero,
                transform,
                dot,
                None,
                &Circle::new(Point::new(x + DOT_D / 2.0, cy), DOT_D / 2.0),
            );
            x += DOT_D + GAP;
        }

        // Label.
        let size = theme.tokens.typography.scale.sm;
        let (lw, _lh) = text_renderer.measure(&self.label, size, None);
        self.label_w = lw as f64;
        let label_col = if selected { accent } else { theme.text_primary };
        // Optically centre the label so it sits level with the leading dot /
        // trailing "×" and the pill middle, not ~0.2 em low (shared helper).
        let label_y = crate::text_render::centered_text_y(self.rect.y0, self.rect.height(), size);
        text_renderer.draw_text(
            scene,
            &self.label,
            size,
            label_col,
            transform * Affine::translate((x, label_y)),
            None,
        );
        x += lw as f64;

        // Trailing "×".
        if self.removable {
            x += GAP;
            let xr = Rect::from_center_size(Point::new(x + X_D / 2.0, cy), (X_D, X_D));
            self.x_rect = xr;
            let hovered = xr.contains(self.last_cursor);
            let col = if hovered { theme.text_primary } else { theme.text_dim };
            Icon::X.draw_with_stroke(scene, transform, xr, col, 1.6);
        } else {
            self.x_rect = Rect::ZERO;
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
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.removable && self.x_rect.contains(self.last_cursor) {
                    self.removed = true;
                    *handled = true;
                } else if self.selectable && self.rect.contains(self.last_cursor) {
                    self.selected = !self.selected;
                    self.toggled = true;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if (self.selectable || self.removable) && self.rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn name(&self) -> &str { "Chip" }

    fn get_rect(&self) -> Rect { self.rect }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // A selectable chip is a toggle button; a plain chip is static text.
        if self.selectable {
            Some(
                AccessNode::new(Role::ToggleButton)
                    .with_name(self.label.clone())
                    .with_state(AccessState::new().focusable(true).selected(self.selected).checked(self.selected)),
            )
        } else {
            Some(AccessNode::new(Role::StaticText).with_value(self.label.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay_out(w: &mut dyn Widget) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = w.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(300.0),
                    height: taffy::AvailableSpace::Definite(60.0),
                },
                |k, a, id, _, _| {
                    node_map.get(&id).map(|&p| unsafe { (*p).measure(k, a, &mut tr) }).unwrap_or(taffy::Size::ZERO)
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
    }

    fn draw(w: &mut dyn Widget) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    fn synth() -> winit::event::DeviceId { unsafe { std::mem::zeroed() } }
    fn cursor(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved { device_id: synth(), position: winit::dpi::PhysicalPosition::new(p.x, p.y) }
    }
    fn press() -> WindowEvent {
        WindowEvent::MouseInput { device_id: synth(), state: ElementState::Pressed, button: MouseButton::Left }
    }
    fn dispatch(w: &mut dyn Widget, e: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut ft = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        w.handle_event(e, 1.0, &winit::event::Modifiers::default(), &mut doc, &root, &mut handled, &mut ft, node, &taffy);
        handled
    }

    #[test]
    fn selectable_chip_toggles_on_body_click() {
        let mut c = Chip::new("Rust").selectable();
        lay_out(&mut c);
        draw(&mut c);
        assert!(!c.is_selected());
        let center = c.get_rect().center();
        dispatch(&mut c, &cursor(center));
        assert!(dispatch(&mut c, &press()), "body click on a selectable chip is consumed");
        assert!(c.is_selected(), "click selects");
        assert!(c.take_toggled(), "toggle signal fires");
        assert!(!c.take_toggled(), "toggle drains after one read");
        // Click again → deselect.
        dispatch(&mut c, &press());
        assert!(!c.is_selected(), "second click deselects");
    }

    #[test]
    fn plain_chip_does_not_toggle() {
        let mut c = Chip::new("Label");
        lay_out(&mut c);
        draw(&mut c);
        let center = c.get_rect().center();
        dispatch(&mut c, &cursor(center));
        assert!(!dispatch(&mut c, &press()), "a plain chip ignores clicks");
        assert!(!c.take_toggled());
    }

    #[test]
    fn removable_chip_fires_remove_on_x_click() {
        let mut c = Chip::new("alice@example.com").removable();
        lay_out(&mut c);
        draw(&mut c);
        let x = c.remove_rect().center();
        assert!(x.x > 0.0, "× rect resolves after draw");
        dispatch(&mut c, &cursor(x));
        assert!(dispatch(&mut c, &press()), "× click is consumed");
        assert!(c.take_removed(), "remove signal fires");
        assert!(!c.take_removed(), "remove drains after one read");
    }

    #[test]
    fn removable_and_selectable_x_wins_over_toggle() {
        // Clicking the × of a chip that is also selectable removes, doesn't toggle.
        let mut c = Chip::new("tag").selectable().removable();
        lay_out(&mut c);
        draw(&mut c);
        let x = c.remove_rect().center();
        dispatch(&mut c, &cursor(x));
        dispatch(&mut c, &press());
        assert!(c.take_removed(), "× removes");
        assert!(!c.take_toggled(), "× does not also toggle selection");
        assert!(!c.is_selected());
    }

    #[test]
    fn accessibility_toggle_vs_static() {
        use crate::a11y::Role;
        let sel = Chip::new("Filter").selected(true);
        let node = Widget::accessibility(&sel).unwrap();
        assert_eq!(node.role, Role::ToggleButton);
        assert_eq!(node.state.checked, Some(true), "a selected chip reports checked");
        assert!(node.state.focusable);

        let plain = Chip::new("tag");
        assert_eq!(Widget::accessibility(&plain).unwrap().role, Role::StaticText);
    }
}
