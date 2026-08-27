//! Small leaf controls: vector [`Icon`]s and a [`Switch`] toggle.
//!
//! The toolkit has no icon font and its `Toggle` is CRDT-path bound; Tube
//! wants lightweight, theme-coloured glyphs for the nav rail / buttons and
//! a switch that speaks our [`ActionBus`]. Both draw straight into the
//! Vello scene. Every colour is supplied by the caller from the live
//! [`Theme`].

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::action::{self, Action, ActionBus};

#[derive(Clone, Copy, PartialEq)]
pub enum IconKind {
    Home,
    Fire,
    Subscriptions,
    History,
    Settings,
    Search,
    Like,
    /// Thumbs-down — the Return YouTube Dislike pill on the watch page.
    Dislike,
    Bell,
    Play,
    Pause,
    Check,
    Plus,
    Chevron,
    /// Left-pointing back arrow for the navigation Back affordance.
    Back,
    /// A stack of items — the playlist result badge / playlist page marker.
    Playlist,
    Shield,
    Sparkle,
    Eye,
    Skip,
    /// Hamburger — three horizontal bars. Summons the navigation rail when
    /// it is collapsed to an overlay flap on a narrow window.
    Menu,
}

pub struct Icon {
    kind: IconKind,
    size: f32,
    color: Color,
    rect: Rect,
}

impl Icon {
    pub fn new(kind: IconKind, size: f32, color: Color) -> Self {
        Self {
            kind,
            size,
            color,
            rect: Rect::ZERO,
        }
    }

    fn paint(&self, scene: &mut Scene, transform: Affine) {
        let r = self.rect;
        let s = r.width().min(r.height());
        // Inset a touch so strokes don't clip the box edge.
        let pad = s * 0.12;
        let b = Rect::new(r.x0 + pad, r.y0 + pad, r.x0 + s - pad, r.y0 + s - pad);
        let u = |x: f64, y: f64| Point::new(b.x0 + b.width() * x, b.y0 + b.height() * y);
        let sw = (s * 0.09).max(1.4);
        let stroke = Stroke::new(sw).with_caps(vello::kurbo::Cap::Round);
        let line = |scene: &mut Scene, pts: &[(f64, f64)], close: bool| {
            let mut p = BezPath::new();
            for (i, &(x, y)) in pts.iter().enumerate() {
                let pt = u(x, y);
                if i == 0 {
                    p.move_to(pt);
                } else {
                    p.line_to(pt);
                }
            }
            if close {
                p.close_path();
            }
            scene.stroke(&stroke, transform, self.color, None, &p);
        };

        match self.kind {
            IconKind::Home => {
                line(scene, &[(0.1, 0.5), (0.5, 0.12), (0.9, 0.5)], false);
                line(
                    scene,
                    &[(0.2, 0.45), (0.2, 0.9), (0.8, 0.9), (0.8, 0.45)],
                    false,
                );
                line(scene, &[(0.42, 0.9), (0.42, 0.62), (0.58, 0.62), (0.58, 0.9)], false);
            }
            IconKind::Fire => {
                let mut p = BezPath::new();
                p.move_to(u(0.5, 0.1));
                p.curve_to(u(0.85, 0.4), u(0.85, 0.75), u(0.5, 0.92));
                p.curve_to(u(0.15, 0.75), u(0.15, 0.45), u(0.42, 0.3));
                p.curve_to(u(0.4, 0.5), u(0.55, 0.5), u(0.5, 0.1));
                p.close_path();
                scene.fill(Fill::NonZero, transform, self.color, None, &p);
            }
            IconKind::Subscriptions => {
                line(scene, &[(0.15, 0.2), (0.85, 0.2)], false);
                let rr = RoundedRect::new(
                    u(0.15, 0.35).x,
                    u(0.15, 0.35).y,
                    u(0.85, 0.9).x,
                    u(0.85, 0.9).y,
                    sw,
                );
                scene.stroke(&stroke, transform, self.color, None, &rr);
                let mut tri = BezPath::new();
                tri.move_to(u(0.43, 0.5));
                tri.line_to(u(0.43, 0.75));
                tri.line_to(u(0.62, 0.625));
                tri.close_path();
                scene.fill(Fill::NonZero, transform, self.color, None, &tri);
            }
            IconKind::History => {
                scene.stroke(
                    &stroke,
                    transform,
                    self.color,
                    None,
                    &Circle::new(u(0.5, 0.5), b.width() * 0.4),
                );
                line(scene, &[(0.5, 0.3), (0.5, 0.5), (0.68, 0.6)], false);
            }
            IconKind::Settings => {
                // A gear: a body ring + a hollow centre + short trapezoidal
                // teeth straddling the rim (NOT long radial rays, which read
                // as a sun).
                let c = u(0.5, 0.5);
                let r_body = b.width() * 0.26;
                let r_tooth = b.width() * 0.40;
                let n = 8;
                let half = 0.20_f64; // angular half-width of each tooth (rad)
                for k in 0..n {
                    let a = std::f64::consts::TAU * k as f64 / n as f64;
                    let pt = |ang: f64, r: f64| {
                        Point::new(c.x + ang.cos() * r, c.y + ang.sin() * r)
                    };
                    let mut p = BezPath::new();
                    p.move_to(pt(a - half, r_body));
                    p.line_to(pt(a - half, r_tooth));
                    p.line_to(pt(a + half, r_tooth));
                    p.line_to(pt(a + half, r_body));
                    p.close_path();
                    scene.fill(Fill::NonZero, transform, self.color, None, &p);
                }
                scene.stroke(&stroke, transform, self.color, None, &Circle::new(c, r_body));
                scene.stroke(&stroke, transform, self.color, None, &Circle::new(c, b.width() * 0.11));
            }
            IconKind::Search => {
                scene.stroke(
                    &stroke,
                    transform,
                    self.color,
                    None,
                    &Circle::new(u(0.42, 0.42), b.width() * 0.3),
                );
                line(scene, &[(0.63, 0.63), (0.88, 0.88)], false);
            }
            IconKind::Like => {
                let mut p = BezPath::new();
                p.move_to(u(0.3, 0.45));
                p.line_to(u(0.5, 0.15));
                p.curve_to(u(0.62, 0.12), u(0.66, 0.28), u(0.55, 0.42));
                p.line_to(u(0.85, 0.42));
                p.curve_to(u(0.93, 0.42), u(0.93, 0.52), u(0.85, 0.85));
                p.line_to(u(0.4, 0.85));
                p.line_to(u(0.3, 0.78));
                p.close_path();
                scene.fill(Fill::NonZero, transform, self.color, None, &p);
                let base = RoundedRect::new(u(0.12, 0.45).x, u(0.12, 0.45).y, u(0.3, 0.88).x, u(0.3, 0.88).y, sw);
                scene.fill(Fill::NonZero, transform, self.color, None, &base);
            }
            IconKind::Dislike => {
                // The Like glyph flipped vertically (thumb points down).
                let d = |x: f64, y: f64| u(x, 1.0 - y);
                let mut p = BezPath::new();
                p.move_to(d(0.3, 0.45));
                p.line_to(d(0.5, 0.15));
                p.curve_to(d(0.62, 0.12), d(0.66, 0.28), d(0.55, 0.42));
                p.line_to(d(0.85, 0.42));
                p.curve_to(d(0.93, 0.42), d(0.93, 0.52), d(0.85, 0.85));
                p.line_to(d(0.4, 0.85));
                p.line_to(d(0.3, 0.78));
                p.close_path();
                scene.fill(Fill::NonZero, transform, self.color, None, &p);
                let base = RoundedRect::new(d(0.12, 0.45).x, d(0.3, 0.88).y, d(0.3, 0.88).x, d(0.12, 0.45).y, sw);
                scene.fill(Fill::NonZero, transform, self.color, None, &base);
            }
            IconKind::Bell => {
                let mut p = BezPath::new();
                p.move_to(u(0.25, 0.7));
                p.curve_to(u(0.25, 0.35), u(0.3, 0.2), u(0.5, 0.18));
                p.curve_to(u(0.7, 0.2), u(0.75, 0.35), u(0.75, 0.7));
                p.close_path();
                scene.stroke(&stroke, transform, self.color, None, &p);
                line(scene, &[(0.18, 0.7), (0.82, 0.7)], false);
                line(scene, &[(0.43, 0.82), (0.57, 0.82)], false);
            }
            IconKind::Play => {
                let mut tri = BezPath::new();
                tri.move_to(u(0.3, 0.18));
                tri.line_to(u(0.3, 0.82));
                tri.line_to(u(0.85, 0.5));
                tri.close_path();
                scene.fill(Fill::NonZero, transform, self.color, None, &tri);
            }
            IconKind::Pause => {
                scene.fill(Fill::NonZero, transform, self.color, None,
                    &Rect::new(u(0.3, 0.2).x, u(0.3, 0.2).y, u(0.43, 0.8).x, u(0.43, 0.8).y));
                scene.fill(Fill::NonZero, transform, self.color, None,
                    &Rect::new(u(0.57, 0.2).x, u(0.57, 0.2).y, u(0.7, 0.8).x, u(0.7, 0.8).y));
            }
            IconKind::Check => {
                line(scene, &[(0.2, 0.55), (0.42, 0.78), (0.82, 0.25)], false);
            }
            IconKind::Plus => {
                line(scene, &[(0.5, 0.2), (0.5, 0.8)], false);
                line(scene, &[(0.2, 0.5), (0.8, 0.5)], false);
            }
            IconKind::Chevron => {
                line(scene, &[(0.4, 0.25), (0.65, 0.5), (0.4, 0.75)], false);
            }
            IconKind::Back => {
                // Left-pointing arrow: a chevron head + a shaft.
                line(scene, &[(0.45, 0.25), (0.2, 0.5), (0.45, 0.75)], false);
                line(scene, &[(0.2, 0.5), (0.82, 0.5)], false);
            }
            IconKind::Playlist => {
                // Three stacked lines with a small play triangle on the last row.
                line(scene, &[(0.15, 0.28), (0.75, 0.28)], false);
                line(scene, &[(0.15, 0.5), (0.75, 0.5)], false);
                line(scene, &[(0.15, 0.72), (0.5, 0.72)], false);
                let mut tri = BezPath::new();
                tri.move_to(u(0.66, 0.62));
                tri.line_to(u(0.66, 0.82));
                tri.line_to(u(0.85, 0.72));
                tri.close_path();
                scene.fill(vello::peniko::Fill::NonZero, transform, self.color, None, &tri);
            }
            IconKind::Shield => {
                let mut p = BezPath::new();
                p.move_to(u(0.5, 0.12));
                p.line_to(u(0.85, 0.26));
                p.line_to(u(0.85, 0.55));
                p.curve_to(u(0.85, 0.78), u(0.68, 0.88), u(0.5, 0.92));
                p.curve_to(u(0.32, 0.88), u(0.15, 0.78), u(0.15, 0.55));
                p.line_to(u(0.15, 0.26));
                p.close_path();
                scene.stroke(&stroke, transform, self.color, None, &p);
                line(scene, &[(0.36, 0.5), (0.47, 0.62), (0.66, 0.36)], false);
            }
            IconKind::Sparkle => {
                let mut p = BezPath::new();
                p.move_to(u(0.5, 0.1));
                p.line_to(u(0.58, 0.42));
                p.line_to(u(0.9, 0.5));
                p.line_to(u(0.58, 0.58));
                p.line_to(u(0.5, 0.9));
                p.line_to(u(0.42, 0.58));
                p.line_to(u(0.1, 0.5));
                p.line_to(u(0.42, 0.42));
                p.close_path();
                scene.fill(Fill::NonZero, transform, self.color, None, &p);
            }
            IconKind::Eye => {
                let mut p = BezPath::new();
                p.move_to(u(0.12, 0.5));
                p.curve_to(u(0.35, 0.22), u(0.65, 0.22), u(0.88, 0.5));
                p.curve_to(u(0.65, 0.78), u(0.35, 0.78), u(0.12, 0.5));
                p.close_path();
                scene.stroke(&stroke, transform, self.color, None, &p);
                scene.fill(Fill::NonZero, transform, self.color, None, &Circle::new(u(0.5, 0.5), b.width() * 0.12));
            }
            IconKind::Skip => {
                let mut tri = BezPath::new();
                tri.move_to(u(0.2, 0.22));
                tri.line_to(u(0.2, 0.78));
                tri.line_to(u(0.6, 0.5));
                tri.close_path();
                scene.fill(Fill::NonZero, transform, self.color, None, &tri);
                scene.fill(Fill::NonZero, transform, self.color, None,
                    &Rect::new(u(0.66, 0.22).x, u(0.66, 0.22).y, u(0.78, 0.78).x, u(0.78, 0.78).y));
            }
            IconKind::Menu => {
                line(scene, &[(0.18, 0.3), (0.82, 0.3)], false);
                line(scene, &[(0.18, 0.5), (0.82, 0.5)], false);
                line(scene, &[(0.18, 0.7), (0.82, 0.7)], false);
            }
        }
    }
}

impl Widget for Icon {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: length(self.size),
                    height: length(self.size),
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        _theme: &Theme,
        _p: &PresenceCtx,
    ) {
        self.paint(scene, transform);
    }

    fn handle_event(
        &mut self,
        _e: &WindowEvent,
        _sf: f64,
        _m: &winit::event::Modifiers,
        _s: &mut AutoCommit,
        _p: &ObjId,
        _h: &mut bool,
        _ft: &mut Option<NodeId>,
        _n: NodeId,
        _t: &TaffyTree<()>,
    ) {
    }
    fn name(&self) -> &str {
        "Icon"
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// A pill toggle switch that pushes `action` on click and renders its
/// on/off state from `on`.
pub struct Switch {
    on: bool,
    action: Action,
    bus: ActionBus,
    rect: Rect,
    hovered: bool,
    dirty: bool,
    test_id: Option<String>,
}

impl Switch {
    pub fn new(on: bool, bus: &ActionBus, action: Action) -> Self {
        Self {
            on,
            action,
            bus: bus.clone(),
            rect: Rect::ZERO,
            hovered: false,
            dirty: true,
            test_id: None,
        }
    }

    /// Stable, addressable id so headless click-tests can flip this exact
    /// switch through the real `handle_event` path.
    pub fn id(mut self, id: &str) -> Self {
        self.test_id = Some(id.to_string());
        self
    }
}

impl Widget for Switch {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: length(46.0_f32),
                    height: length(26.0_f32),
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        theme: &Theme,
        _p: &PresenceCtx,
    ) {
        let track = RoundedRect::from_rect(self.rect, self.rect.height() / 2.0);
        let bg = if self.on {
            theme.accent_primary
        } else {
            theme.tokens.colors.neutral.stops[6]
        };
        scene.fill(Fill::NonZero, transform, bg, None, &track);
        if self.hovered {
            scene.stroke(&Stroke::new(2.0), transform, theme.border_focus, None, &track);
        }
        let r = self.rect.height() / 2.0 - 3.0;
        let cx = if self.on {
            self.rect.x1 - r - 3.0
        } else {
            self.rect.x0 + r + 3.0
        };
        scene.fill(
            Fill::NonZero,
            transform,
            Color::WHITE,
            None,
            &Circle::new(Point::new(cx, self.rect.center().y), r),
        );
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _m: &winit::event::Modifiers,
        _s: &mut AutoCommit,
        _p: &ObjId,
        handled: &mut bool,
        _ft: &mut Option<NodeId>,
        _n: NodeId,
        _t: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hov = self.rect.contains(p);
                if hov != self.hovered {
                    self.hovered = hov;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.hovered && !*handled => {
                action::push(&self.bus, self.action.clone());
                *handled = true;
            }
            _ => {}
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
    fn name(&self) -> &str {
        "Switch"
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}
