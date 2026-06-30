//! Root widget for the calculator. A single full-window widget that owns
//! the [`CalcModel`], paints the header + the active mode's screen, and
//! routes all keyboard / pointer input. State is flushed into the shared
//! Automerge document on every change (see [`crate::doc`]).

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey};

use ui::Widget;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::draw::{self, Align};
use crate::graph2d::Graph2dScreen;
use crate::graph3d::Graph3dScreen;
use crate::keypad::{CalcScreen, MEM_KEYS};
use crate::model::{self, CalcModel, Mode, PlotFn};

const TOPBAR_H: f64 = 60.0;

pub struct CalculatorApp {
    model: CalcModel,
    calc: CalcScreen,
    g2: Graph2dScreen,
    g3: Graph3dScreen,

    area: Rect,
    topbar: Rect,
    body: Rect,
    seg_rects: [Rect; 3],
    rad_rect: Rect,
    deg_rect: Rect,

    mouse: Point,
    dragging: bool,
    drag_last: Point,

    seeded: bool,
    model_dirty: bool,
    node: Option<NodeId>,
}

impl CalculatorApp {
    pub fn new() -> Self {
        Self {
            model: CalcModel::seeded(),
            calc: CalcScreen::default(),
            g2: Graph2dScreen::default(),
            g3: Graph3dScreen::default(),
            area: Rect::ZERO,
            topbar: Rect::ZERO,
            body: Rect::ZERO,
            seg_rects: [Rect::ZERO; 3],
            rad_rect: Rect::ZERO,
            deg_rect: Rect::ZERO,
            mouse: Point::ZERO,
            dragging: false,
            drag_last: Point::ZERO,
            seeded: false,
            model_dirty: false,
            node: None,
        }
    }

    /// Recompute the header geometry from `self.area`.
    fn relayout(&mut self) {
        self.topbar = Rect::new(self.area.x0, self.area.y0, self.area.x1, self.area.y0 + TOPBAR_H);
        self.body = Rect::new(self.area.x0, self.area.y0 + TOPBAR_H, self.area.x1, self.area.y1);

        let seg_w = 128.0;
        let seg_h = 36.0;
        let x = self.area.x0 + 196.0;
        let y0 = self.area.y0 + (TOPBAR_H - seg_h) / 2.0;
        for i in 0..3 {
            let sx = x + i as f64 * seg_w;
            self.seg_rects[i] = Rect::new(sx, y0, sx + seg_w, y0 + seg_h);
        }
        let ax1 = self.area.x1 - 24.0;
        let ax0 = ax1 - 124.0;
        self.rad_rect = Rect::new(ax0, y0, ax0 + 62.0, y0 + seg_h);
        self.deg_rect = Rect::new(ax0 + 62.0, y0, ax1, y0 + seg_h);
    }

    fn layout_active(&mut self) {
        match self.model.mode {
            Mode::Calc => self.calc.layout(self.body, &self.model),
            Mode::Graph2d => self.g2.layout(self.body, &self.model),
            Mode::Graph3d => self.g3.layout(self.body),
        }
    }

    fn set_mode(&mut self, m: Mode) {
        if self.model.mode != m {
            self.model.mode = m;
            self.model_dirty = true;
        }
    }

    fn draw_topbar(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, self.topbar, c.surfaces.raised);
        draw::line(
            scene,
            t,
            Point::new(self.topbar.x0, self.topbar.y1),
            Point::new(self.topbar.x1, self.topbar.y1),
            c.border_standard,
            1.0,
        );

        // brand
        let dot = Rect::new(self.area.x0 + 24.0, self.topbar.center().y - 9.0, self.area.x0 + 42.0, self.topbar.center().y + 9.0);
        draw::fill_rrect(scene, t, dot, 5.0, c.accent[5]);
        draw::text_in(scene, tr, t, dot, "=", 16.0, Color::WHITE, Align::Center, TextWeight::Bold);
        let title = Rect::new(self.area.x0 + 52.0, self.topbar.y0, self.area.x0 + 190.0, self.topbar.y1);
        draw::text_in(scene, tr, t, title, "Calculator", 19.0, c.text_primary, Align::Left, TextWeight::Bold);

        // segmented mode control
        let outer = Rect::new(self.seg_rects[0].x0 - 4.0, self.seg_rects[0].y0 - 4.0, self.seg_rects[2].x1 + 4.0, self.seg_rects[2].y1 + 4.0);
        draw::fill_rrect(scene, t, outer, 10.0, c.neutral[2]);
        let sel = self.model.mode.index();
        for (i, m) in Mode::all().iter().enumerate() {
            let r = self.seg_rects[i];
            if i == sel {
                draw::fill_rrect(scene, t, r, 8.0, c.accent[5]);
            }
            let col = if i == sel { Color::WHITE } else { c.text_dim };
            draw::text_in(scene, tr, t, r, m.label(), 14.0, col, Align::Center, TextWeight::Semibold);
        }

        // DEG / RAD toggle
        let outer2 = Rect::new(self.rad_rect.x0 - 4.0, self.rad_rect.y0 - 4.0, self.deg_rect.x1 + 4.0, self.deg_rect.y1 + 4.0);
        draw::fill_rrect(scene, t, outer2, 10.0, c.neutral[2]);
        if !self.model.deg {
            draw::fill_rrect(scene, t, self.rad_rect, 8.0, c.accent[5]);
        } else {
            draw::fill_rrect(scene, t, self.deg_rect, 8.0, c.accent[5]);
        }
        let rad_col = if !self.model.deg { Color::WHITE } else { c.text_dim };
        let deg_col = if self.model.deg { Color::WHITE } else { c.text_dim };
        draw::text_in(scene, tr, t, self.rad_rect, "RAD", 13.0, rad_col, Align::Center, TextWeight::Semibold);
        draw::text_in(scene, tr, t, self.deg_rect, "DEG", 13.0, deg_col, Align::Center, TextWeight::Semibold);
    }

    // -- pointer routing -----------------------------------------------------

    fn on_press(&mut self, p: Point) {
        // header first
        for i in 0..3 {
            if self.seg_rects[i].contains(p) {
                self.set_mode(Mode::from_index(i));
                return;
            }
        }
        if self.rad_rect.contains(p) {
            self.model.deg = false;
            self.model_dirty = true;
            return;
        }
        if self.deg_rect.contains(p) {
            self.model.deg = true;
            self.model_dirty = true;
            return;
        }

        match self.model.mode {
            Mode::Calc => self.on_press_calc(p),
            Mode::Graph2d => self.on_press_g2(p),
            Mode::Graph3d => {
                if self.g3.canvas_contains(p) {
                    self.dragging = true;
                    self.drag_last = p;
                }
            }
        }
    }

    fn on_press_calc(&mut self, p: Point) {
        if let Some(i) = self.calc.key_at(p) {
            self.calc.pressed_key = Some(i);
            let key = self.calc.keys()[i].clone();
            model::apply_key(&mut self.model, &key);
            self.model_dirty = true;
        } else if let Some(i) = self.calc.mem_at(p) {
            let key = MEM_KEYS[i].clone();
            model::apply_key(&mut self.model, &key);
            self.model_dirty = true;
        } else if self.calc.clear_history_at(p) {
            self.model.history.clear();
            self.model_dirty = true;
        } else if let Some(hi) = self.calc.history_at(p) {
            if let Some(e) = self.model.history.get(hi).cloned() {
                self.model.expr = e.expr;
                self.model.after_equals = false;
                self.model_dirty = true;
            }
        }
    }

    fn on_press_g2(&mut self, p: Point) {
        if self.g2.add_at(p) {
            self.model.funcs.push(PlotFn { expr: String::new(), visible: true });
            self.model.active_fn = self.model.funcs.len() - 1;
            self.model_dirty = true;
        } else if let Some(i) = self.g2.vis_at(p) {
            if let Some(f) = self.model.funcs.get_mut(i) {
                f.visible = !f.visible;
                self.model_dirty = true;
            }
        } else if let Some(i) = self.g2.row_at(p) {
            self.model.active_fn = i;
            self.model_dirty = true;
        } else if self.g2.canvas_contains(p) {
            self.dragging = true;
            self.drag_last = p;
        }
    }

    fn on_drag(&mut self, d: Vec2) {
        match self.model.mode {
            Mode::Graph2d => {
                let s = self.model.view2d.scale;
                self.model.view2d.cx -= d.x / s;
                self.model.view2d.cy += d.y / s;
                self.model_dirty = true;
            }
            Mode::Graph3d => {
                self.model.view3d.yaw += d.x * 0.01;
                self.model.view3d.pitch = (self.model.view3d.pitch + d.y * 0.01).clamp(-1.5, 1.5);
                self.model_dirty = true;
            }
            Mode::Calc => {}
        }
    }

    fn on_wheel(&mut self, step: f64) {
        let factor = (1.0 + step * 0.12).clamp(0.4, 2.5);
        match self.model.mode {
            // Scroll the history list when the pointer is over it. This is
            // ephemeral view state (like hover), so it lives on the screen
            // cache rather than the CRDT.
            Mode::Calc if self.calc.history_viewport().contains(self.mouse) => {
                self.calc.history_scroll = (self.calc.history_scroll - step * 48.0).max(0.0);
            }
            Mode::Graph2d if self.g2.canvas_contains(self.mouse) => {
                let v0 = self.model.view2d;
                let before = self.g2.to_world(self.mouse, &v0);
                let mut v = v0;
                v.scale = (v.scale * factor).clamp(2.0, 8000.0);
                let after = self.g2.to_world(self.mouse, &v);
                v.cx += before.x - after.x;
                v.cy += before.y - after.y;
                self.model.view2d = v;
                self.model_dirty = true;
            }
            Mode::Graph3d if self.g3.canvas_contains(self.mouse) => {
                self.model.view3d.zoom = (self.model.view3d.zoom * factor).clamp(0.25, 8.0);
                self.model_dirty = true;
            }
            _ => {}
        }
    }

    // -- keyboard ------------------------------------------------------------

    fn on_key(&mut self, key: &WKey, ctrl: bool) {
        match key {
            WKey::Named(NamedKey::Enter) => {
                if self.model.mode == Mode::Calc {
                    model::apply_key(&mut self.model, &model::Key::Equals);
                    self.model_dirty = true;
                }
            }
            WKey::Named(NamedKey::Backspace) => {
                match self.model.mode {
                    Mode::Calc => model::apply_key(&mut self.model, &model::Key::Backspace),
                    Mode::Graph2d => {
                        let a = self.model.active_fn;
                        if let Some(f) = self.model.funcs.get_mut(a) {
                            f.expr.pop();
                        }
                    }
                    Mode::Graph3d => {
                        self.model.surface.pop();
                    }
                }
                self.model_dirty = true;
            }
            WKey::Named(NamedKey::Escape) | WKey::Named(NamedKey::Delete) => {
                if self.model.mode == Mode::Calc {
                    model::apply_key(&mut self.model, &model::Key::Clear);
                    self.model_dirty = true;
                }
            }
            WKey::Named(NamedKey::ArrowLeft) => self.arrow(-1.0, 0.0),
            WKey::Named(NamedKey::ArrowRight) => self.arrow(1.0, 0.0),
            WKey::Named(NamedKey::ArrowUp) => self.arrow(0.0, 1.0),
            WKey::Named(NamedKey::ArrowDown) => self.arrow(0.0, -1.0),
            WKey::Character(s) => {
                if ctrl {
                    match s.as_str() {
                        "1" => self.set_mode(Mode::Calc),
                        "2" => self.set_mode(Mode::Graph2d),
                        "3" => self.set_mode(Mode::Graph3d),
                        _ => {}
                    }
                    return;
                }
                for ch in s.chars() {
                    self.type_char(ch);
                }
                self.model_dirty = true;
            }
            _ => {}
        }
    }

    fn type_char(&mut self, ch: char) {
        match self.model.mode {
            Mode::Calc => model::type_char(&mut self.model, ch),
            Mode::Graph2d => {
                if accept_expr_char(ch) {
                    let a = self.model.active_fn;
                    if let Some(f) = self.model.funcs.get_mut(a) {
                        f.expr.push(ch.to_ascii_lowercase());
                    }
                }
            }
            Mode::Graph3d => {
                if accept_expr_char(ch) {
                    self.model.surface.push(ch.to_ascii_lowercase());
                }
            }
        }
    }

    fn arrow(&mut self, dx: f64, dy: f64) {
        match self.model.mode {
            Mode::Graph2d => {
                let step = 48.0 / self.model.view2d.scale;
                self.model.view2d.cx += dx * step;
                self.model.view2d.cy += dy * step;
                self.model_dirty = true;
            }
            Mode::Graph3d => {
                self.model.view3d.yaw += dx * 0.12;
                self.model.view3d.pitch = (self.model.view3d.pitch + dy * 0.12).clamp(-1.5, 1.5);
                self.model_dirty = true;
            }
            Mode::Calc => {}
        }
    }
}

impl Default for CalculatorApp {
    fn default() -> Self {
        Self::new()
    }
}

fn accept_expr_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || "+-*/^(),. %".contains(ch)
}

impl Widget for CalculatorApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            if crate::doc::is_seeded(state) {
                if let Some(m) = crate::doc::load(state) {
                    self.model = m;
                }
            } else {
                crate::doc::store(state, &self.model);
            }
            self.seeded = true;
        }
    }

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
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.area = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        self.relayout();
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
        draw::fill_rect(scene, t, self.area, c.surfaces.base);
        let body = self.body;
        match self.model.mode {
            Mode::Calc => self.calc.draw(scene, t, tr, theme, &self.model, body),
            Mode::Graph2d => {
                let trace = if self.g2.canvas_contains(self.mouse) {
                    Some(self.mouse)
                } else {
                    None
                };
                self.g2.draw(scene, t, tr, theme, &self.model, body, trace);
            }
            Mode::Graph3d => self.g3.draw(scene, t, tr, theme, &self.model, body),
        }
        self.draw_topbar(scene, t, tr, theme);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        self.relayout();
        self.layout_active();

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.mouse = p;
                match self.model.mode {
                    Mode::Calc => {
                        self.calc.hovered_key = self.calc.key_at(p);
                        self.calc.hovered_mem = self.calc.mem_at(p);
                    }
                    Mode::Graph2d => {
                        self.g2.hovered_row = self.g2.row_at(p);
                    }
                    Mode::Graph3d => {}
                }
                if self.dragging {
                    let d = p - self.drag_last;
                    self.on_drag(d);
                    self.drag_last = p;
                }
                *handled = true;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.on_press(self.mouse);
                *handled = true;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.dragging = false;
                self.calc.pressed_key = None;
                *handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let step = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64,
                    MouseScrollDelta::PixelDelta(pos) => pos.y / 40.0,
                };
                self.on_wheel(step);
                *handled = true;
            }
            WindowEvent::KeyboardInput { event: key_event, .. }
                if key_event.state == ElementState::Pressed =>
            {
                let m = modifiers.state();
                let ctrl = m.control_key() || m.super_key();
                self.on_key(&key_event.logical_key, ctrl);
                *handled = true;
            }
            _ => {}
        }

        if self.model_dirty {
            crate::doc::store(state, &self.model);
            self.model_dirty = false;
        }
    }

    fn name(&self) -> &str {
        "CalculatorApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("calculator_app")
    }

    fn get_rect(&self) -> Rect {
        self.area
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_in_graph2d_with_seeded_content() {
        let app = CalculatorApp::new();
        assert_eq!(app.model.mode, Mode::Graph2d);
        assert!(!app.model.funcs.is_empty());
        assert!(!app.model.history.is_empty());
    }

    #[test]
    fn set_mode_marks_dirty() {
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Calc);
        assert_eq!(app.model.mode, Mode::Calc);
        assert!(app.model_dirty);
    }

    #[test]
    fn ctrl_digit_switches_mode() {
        let mut app = CalculatorApp::new();
        app.on_key(&WKey::Character("3".into()), true);
        assert_eq!(app.model.mode, Mode::Graph3d);
    }
}
