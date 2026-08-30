//! A movable + resizable in-app overlay window: draggable title bar,
//! edge/corner resize grips, on-screen clamping that keeps the title bar
//! reachable. Genuinely modal while open via
//! [`crate::widget::Widget::modal_capture`] — it consumes every event
//! inside its rect and swallows out-of-rect clicks without closing (use the
//! title-bar close affordance or an explicit host action to dismiss).
//!
//! It does NOT draw a close button or toolbar — that's the host's concern,
//! drawn into the content. It owns only the frame chrome and the
//! move/resize/clamp mechanics.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

pub const TITLE_BAR_HEIGHT: f64 = 38.0;
pub const RESIZE_GRIP: f64 = 8.0;
pub const MIN_WIDTH: f64 = 280.0;
pub const MIN_HEIGHT: f64 = 200.0;

/// No top edge / top corner — the title bar owns the top of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Right,
    Bottom,
    BottomRight,
    Left,
    BottomLeft,
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    /// `grab` = cursor offset from window origin.
    Move { grab: Point },
    /// `start` = window rect at press, `from` = cursor at press.
    Resize {
        edge: Edge,
        start: Rect,
        from: Point,
    },
}

pub struct FloatingWindow<C: Widget> {
    title: String,
    content: C,
    open: bool,
    /// Screen coords — the source of truth for placement.
    rect: Rect,
    /// Typically the app content area.
    bounds: Rect,
    /// `true` once positioned this open, so a re-layout doesn't recentre
    /// the window after the user has moved it.
    placed: bool,
    drag: Option<Drag>,
    last_cursor: Point,
    /// Retained so `handle_event` can forward events into the content
    /// with a live node.
    content_taffy: Option<TaffyTree<()>>,
    content_node: Option<NodeId>,
    /// Used on first placement, before any user resize.
    initial_size: (f64, f64),
}

impl<C: Widget> FloatingWindow<C> {
    /// Starts closed.
    pub fn new(title: impl Into<String>, content: C) -> Self {
        Self {
            title: title.into(),
            content,
            open: false,
            rect: Rect::ZERO,
            bounds: Rect::ZERO,
            placed: false,
            drag: None,
            last_cursor: Point::ZERO,
            content_taffy: None,
            content_node: None,
            initial_size: (560.0, 460.0),
        }
    }

    /// Closing clears the placement so the next open re-centres.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        if !open {
            self.placed = false;
        }
        self
    }

    /// Non-builder form of [`Self::open`].
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
        if !open {
            self.placed = false;
        }
    }

    pub fn with_initial_size(mut self, w: f64, h: f64) -> Self {
        self.initial_size = (w, h);
        self
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn window_rect(&self) -> Rect {
        self.rect
    }

    /// Host can suppress hover-driven work while dragging.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn content_mut(&mut self) -> &mut C {
        &mut self.content
    }

    /// Centres on first placement, then clamps on every call so a window
    /// stays reachable when the bounds shrink (e.g. the app pane resized).
    pub fn layout_within(&mut self, bounds: Rect) {
        self.bounds = bounds;
        if !self.placed && self.open {
            let (mut w, mut h) = self.initial_size;
            w = w
                .min(bounds.width() - 32.0)
                .max(MIN_WIDTH.min(bounds.width()));
            h = h
                .min(bounds.height() - 32.0)
                .max(MIN_HEIGHT.min(bounds.height()));
            let cx = f64::midpoint(bounds.x0, bounds.x1);
            let y0 = (bounds.y0 + (bounds.height() - h) / 2.0).max(bounds.y0 + 8.0);
            self.rect = Rect::new(cx - w / 2.0, y0, cx + w / 2.0, y0 + h);
            self.placed = true;
        }
        self.clamp_into_bounds();
    }

    fn content_area(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0 + TITLE_BAR_HEIGHT,
            self.rect.x1,
            self.rect.y1,
        )
    }

    fn clamp_into_bounds(&mut self) {
        let b = self.bounds;
        if b.width() <= 0.0 || b.height() <= 0.0 {
            return;
        }
        let r = self.rect;
        let w = r.width().min(b.width()).max(MIN_WIDTH.min(b.width()));
        let h = r.height().min(b.height()).max(MIN_HEIGHT.min(b.height()));
        let mut x0 = r.x0.clamp(b.x0, (b.x1 - w).max(b.x0));
        let mut y0 = r.y0.clamp(b.y0, (b.y1 - h).max(b.y0));
        // Always keep ≥80px of the title bar horizontally reachable.
        x0 = x0.min(b.x1 - 80.0);
        y0 = y0.max(b.y0);
        self.rect = Rect::new(x0, y0, x0 + w, y0 + h);
    }

    fn resize_edge_at(&self, pt: Point) -> Option<Edge> {
        let r = self.rect;
        let near_r = (pt.x - r.x1).abs() <= RESIZE_GRIP;
        let near_l = (pt.x - r.x0).abs() <= RESIZE_GRIP;
        let near_b = (pt.y - r.y1).abs() <= RESIZE_GRIP;
        match (near_l, near_r, near_b) {
            (_, true, true) => Some(Edge::BottomRight),
            (true, _, true) => Some(Edge::BottomLeft),
            (_, true, _) => Some(Edge::Right),
            (true, _, _) if pt.y > r.y0 + TITLE_BAR_HEIGHT => Some(Edge::Left),
            (_, _, true) => Some(Edge::Bottom),
            _ => None,
        }
    }

    fn title_bar_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + TITLE_BAR_HEIGHT,
        )
    }

    fn apply_drag(&mut self, drag: Drag, pt: Point) {
        match drag {
            Drag::Move { grab } => {
                let w = self.rect.width();
                let h = self.rect.height();
                let x0 = pt.x - grab.x;
                let y0 = pt.y - grab.y;
                self.rect = Rect::new(x0, y0, x0 + w, y0 + h);
                self.clamp_into_bounds();
            }
            Drag::Resize { edge, start, from } => {
                let dx = pt.x - from.x;
                let dy = pt.y - from.y;
                let mut r = start;
                match edge {
                    Edge::Right => r.x1 = (start.x1 + dx).max(start.x0 + MIN_WIDTH),
                    Edge::Bottom => r.y1 = (start.y1 + dy).max(start.y0 + MIN_HEIGHT),
                    Edge::BottomRight => {
                        r.x1 = (start.x1 + dx).max(start.x0 + MIN_WIDTH);
                        r.y1 = (start.y1 + dy).max(start.y0 + MIN_HEIGHT);
                    }
                    Edge::Left => r.x0 = (start.x0 + dx).min(start.x1 - MIN_WIDTH),
                    Edge::BottomLeft => {
                        r.x0 = (start.x0 + dx).min(start.x1 - MIN_WIDTH);
                        r.y1 = (start.y1 + dy).max(start.y0 + MIN_HEIGHT);
                    }
                }
                self.rect = r;
                self.clamp_into_bounds();
            }
        }
    }

    /// Isolated taffy tree, retained so events can route in. Mirrors
    /// `ModalDialog`.
    fn place_content(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let inner = self.content_area();
        if inner.width() <= 0.0 || inner.height() <= 0.0 {
            return;
        }
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = self.content.build_layout(&mut taffy, theme, &mut map);
        let _ = taffy.compute_layout_with_measure(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(inner.width() as f32),
                height: taffy::AvailableSpace::Definite(inner.height() as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, text_renderer) }
                } else {
                    Size::ZERO
                }
            },
        );
        self.content
            .apply_layout(&taffy, node, Point::new(inner.x0, inner.y0));
        self.content_taffy = Some(taffy);
        self.content_node = Some(node);
    }

    fn forward_to_content(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
    ) {
        let (Some(taffy), Some(node)) = (self.content_taffy.as_ref(), self.content_node) else {
            return;
        };
        let mut inner_handled = false;
        self.content.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            &mut inner_handled,
            focus_target,
            node,
            taffy,
        );
    }
}

impl<C: Widget + 'static> Widget for FloatingWindow<C> {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Occupies the whole viewport so it can clamp + paint anywhere.
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        let viewport = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        // Default when the host hasn't supplied tighter bounds via `layout_within`.
        if self.bounds.width() <= 0.0 {
            self.bounds = viewport;
        }
        let bounds = self.bounds;
        self.layout_within(bounds);
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        self.place_content(&theme, &mut tr);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        let radius = f64::from(theme.tokens.radii.lg);

        let shadow = Rect::new(
            self.rect.x0 + 2.0,
            self.rect.y0 + 4.0,
            self.rect.x1 + 2.0,
            self.rect.y1 + 6.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 90),
            None,
            &RoundedRect::from_rect(shadow, radius),
        );

        let body = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &body,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &body,
        );

        let title_band = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + TITLE_BAR_HEIGHT,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base.with_alpha_factor(0.6),
            None,
            &title_band,
        );
        let title_size = theme.tokens.typography.scale.base;
        let tx = self.rect.x0 + f64::from(theme.tokens.spacing.s4);
        let ty = self.rect.y0 + (TITLE_BAR_HEIGHT - f64::from(title_size)) / 2.0;
        text_renderer.draw_text(
            scene,
            &self.title,
            title_size,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((tx, ty)),
            None,
        );

        let inner = self.content_area();
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &inner,
        );
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
        scene.pop_layer();

        let g = Rect::new(
            self.rect.x1 - 14.0,
            self.rect.y1 - 14.0,
            self.rect.x1 - 4.0,
            self.rect.y1 - 4.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(g, 1.0),
        );
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if !self.open {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = pt;
                if let Some(drag) = self.drag {
                    self.apply_drag(drag, pt);
                    self.place_content(&Theme::dark(), &mut TextRenderer::new());
                    *handled = true;
                    return;
                }
                // A move outside must not reach the content (the modal
                // boundary), but this arms embedded fields' hover flag.
                if self.content_area().contains(pt) {
                    self.forward_to_content(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        focus_target,
                    );
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pt = self.last_cursor;
                // Tested first so a press exactly on the (half-open) border
                // — which `Rect::contains` reports as outside — still
                // starts a resize.
                if let Some(edge) = self.resize_edge_at(pt) {
                    self.drag = Some(Drag::Resize {
                        edge,
                        start: self.rect,
                        from: pt,
                    });
                    *handled = true;
                    return;
                }
                if self.title_bar_rect().contains(pt) {
                    self.drag = Some(Drag::Move {
                        grab: Point::new(pt.x - self.rect.x0, pt.y - self.rect.y0),
                    });
                    *handled = true;
                    return;
                }
                if !self.rect.contains(pt) {
                    // Outside the window and not on a resize grip: swallow
                    // (modal) but don't close.
                    *handled = true;
                    return;
                }
                self.forward_to_content(event, scale_factor, modifiers, state, path, focus_target);
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                if self.drag.is_some() {
                    self.drag = None;
                }
                *handled = true;
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
                self.set_open(false);
                *handled = true;
            }
            WindowEvent::KeyboardInput { .. } => {
                self.forward_to_content(event, scale_factor, modifiers, state, path, focus_target);
                *handled = true;
            }
            _ => {}
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.content]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.content]
    }

    fn name(&self) -> &'static str {
        "FloatingWindow"
    }

    fn modal_capture(&self) -> Option<Rect> {
        if self.open { Some(self.rect) } else { None }
    }

    /// Without this the frame silently accepts a drag with no hover
    /// feedback. Content sits in an isolated inner tree the host's cursor
    /// sweep can't reach, so a point over the body defers to the default
    /// arrow.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        if !self.open {
            return None;
        }
        let edge_cursor = |e: Edge| match e {
            Edge::Right | Edge::Left => CursorIcon::EwResize,
            Edge::Bottom => CursorIcon::NsResize,
            Edge::BottomRight => CursorIcon::NwseResize,
            Edge::BottomLeft => CursorIcon::NeswResize,
        };
        // Owns the cursor regardless of the exact point — the pointer
        // can slip a few px off the grip while the button is held.
        match self.drag {
            Some(Drag::Move { .. }) => return Some(CursorIcon::Grabbing),
            Some(Drag::Resize { edge, .. }) => return Some(edge_cursor(edge)),
            None => {}
        }
        if let Some(edge) = self.resize_edge_at(point) {
            return Some(edge_cursor(edge));
        }
        if self.title_bar_rect().contains(point) {
            return Some(CursorIcon::Grab);
        }
        None
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_encoding::DrawTag;

    /// A leaf content widget that records forwarded events.
    #[derive(Default)]
    struct Probe {
        events: std::rc::Rc<std::cell::Cell<usize>>,
        rect: Rect,
    }
    impl Widget for Probe {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let n = taffy.leaf_node(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            });
            node_map.insert(n, self as *mut dyn Widget);
            n
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).copied().unwrap_or_default();
            self.rect = Rect::new(
                origin.x,
                origin.y,
                origin.x + l.size.width as f64,
                origin.y + l.size.height as f64,
            );
        }
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
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
            self.events.set(self.events.get() + 1);
        }
        fn name(&self) -> &str {
            "Probe"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    fn laid_out(open: bool) -> (FloatingWindow<Probe>, std::rc::Rc<std::cell::Cell<usize>>) {
        let events = std::rc::Rc::new(std::cell::Cell::new(0));
        let probe = Probe {
            events: events.clone(),
            rect: Rect::ZERO,
        };
        let mut w = FloatingWindow::new("Compose", probe).open(open);
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = w.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(1000.0),
                    height: taffy::AvailableSpace::Definite(800.0),
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
        (w, events)
    }

    fn synth_device() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
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
    fn left_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }
    fn dispatch(w: &mut FloatingWindow<Probe>, e: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let mut handled = false;
        let mut ft = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        w.handle_event(
            e,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        handled
    }

    #[test]
    fn opens_centred_within_bounds() {
        let (w, _e) = laid_out(true);
        let r = w.window_rect();
        assert!(
            (r.center().x - 500.0).abs() < 2.0,
            "centred horizontally: {r:?}"
        );
        assert!(r.x0 >= 0.0 && r.x1 <= 1000.0, "within bounds: {r:?}");
    }

    #[test]
    fn title_bar_drag_moves_window() {
        let (mut w, _e) = laid_out(true);
        let start = w.window_rect();
        let grab = Point::new(start.x0 + 40.0, start.y0 + 10.0);
        dispatch(&mut w, &cursor_at(grab));
        assert!(dispatch(&mut w, &left_press()), "title press consumed");
        dispatch(
            &mut w,
            &cursor_at(Point::new(grab.x + 120.0, grab.y + 40.0)),
        );
        dispatch(&mut w, &left_release());
        let moved = w.window_rect();
        assert!(
            (moved.x0 - (start.x0 + 120.0)).abs() < 2.0,
            "moved right by 120: {moved:?}"
        );
        assert!(
            (moved.y0 - (start.y0 + 40.0)).abs() < 2.0,
            "moved down by 40: {moved:?}"
        );
    }

    #[test]
    fn bottom_right_resize_grows_window() {
        let (mut w, _e) = laid_out(true);
        let start = w.window_rect();
        let corner = Point::new(start.x1, start.y1);
        dispatch(&mut w, &cursor_at(corner));
        assert!(dispatch(&mut w, &left_press()), "corner press consumed");
        dispatch(
            &mut w,
            &cursor_at(Point::new(corner.x + 60.0, corner.y + 50.0)),
        );
        dispatch(&mut w, &left_release());
        let grown = w.window_rect();
        assert!(
            grown.width() > start.width() + 50.0,
            "wider: {} -> {}",
            start.width(),
            grown.width()
        );
        assert!(grown.height() > start.height() + 40.0, "taller");
    }

    #[test]
    fn resize_respects_min_size() {
        let (mut w, _e) = laid_out(true);
        let start = w.window_rect();
        let corner = Point::new(start.x1, start.y1);
        dispatch(&mut w, &cursor_at(corner));
        dispatch(&mut w, &left_press());
        dispatch(
            &mut w,
            &cursor_at(Point::new(start.x0 + 10.0, start.y0 + 10.0)),
        );
        dispatch(&mut w, &left_release());
        let r = w.window_rect();
        assert!(
            r.width() >= MIN_WIDTH - 0.5,
            "clamped to MIN_WIDTH: {}",
            r.width()
        );
        assert!(
            r.height() >= MIN_HEIGHT - 0.5,
            "clamped to MIN_HEIGHT: {}",
            r.height()
        );
    }

    #[test]
    fn off_screen_drag_is_clamped_to_stay_reachable() {
        // Viewport bounds are 0..1000 x 0..800 (see `laid_out`).
        let (mut w, _e) = laid_out(true);
        let start = w.window_rect();

        // Fling the window far past the bottom-right corner.
        let grab = Point::new(start.x0 + 40.0, start.y0 + 10.0);
        dispatch(&mut w, &cursor_at(grab));
        dispatch(&mut w, &left_press());
        dispatch(
            &mut w,
            &cursor_at(Point::new(grab.x + 5000.0, grab.y + 5000.0)),
        );
        dispatch(&mut w, &left_release());
        let r = w.window_rect();
        assert!(
            r.x0 >= 0.0 && r.y0 >= 0.0,
            "origin stays on screen after a huge fling: {r:?}"
        );
        assert!(
            r.x0 <= 1000.0 - 80.0,
            "at least 80px of the window's left stays horizontally reachable: {r:?}"
        );
        assert!(
            r.y0 <= 800.0,
            "the title bar stays within the viewport: {r:?}"
        );
        assert!(
            r.x1 <= 1000.0 + 0.5 && r.y1 <= 800.0 + 0.5,
            "the window is clamped inside the viewport, not off the bottom-right: {r:?}"
        );

        // Fling it far past the top-left (negative) corner.
        let grab2 = Point::new(r.x0 + 40.0, r.y0 + 10.0);
        dispatch(&mut w, &cursor_at(grab2));
        dispatch(&mut w, &left_press());
        dispatch(
            &mut w,
            &cursor_at(Point::new(grab2.x - 5000.0, grab2.y - 5000.0)),
        );
        dispatch(&mut w, &left_release());
        let r2 = w.window_rect();
        assert!(
            r2.x0 >= -0.5 && r2.y0 >= -0.5,
            "clamped to the top-left corner, not off screen: {r2:?}"
        );
    }

    #[test]
    fn cursor_shape_hints_move_and_resize() {
        use winit::window::CursorIcon;
        let (w, _e) = laid_out(true);
        let r = w.window_rect();
        // Over the title bar → a grab hand (the window is draggable).
        let title = Point::new(r.center().x, r.y0 + TITLE_BAR_HEIGHT / 2.0);
        assert_eq!(Widget::cursor_shape(&w, title), Some(CursorIcon::Grab));
        // Right / bottom edges + bottom-right corner → the matching resize cursors.
        assert_eq!(
            Widget::cursor_shape(&w, Point::new(r.x1, r.center().y)),
            Some(CursorIcon::EwResize)
        );
        assert_eq!(
            Widget::cursor_shape(&w, Point::new(r.center().x, r.y1)),
            Some(CursorIcon::NsResize)
        );
        assert_eq!(
            Widget::cursor_shape(&w, Point::new(r.x1, r.y1)),
            Some(CursorIcon::NwseResize)
        );
        // Deep in the content body → no claim (defers to the default arrow).
        assert_eq!(Widget::cursor_shape(&w, w.content_area().center()), None);
    }

    #[test]
    fn cursor_shape_follows_an_active_drag() {
        use winit::window::CursorIcon;
        let (mut w, _e) = laid_out(true);
        let start = w.window_rect();
        let grab = Point::new(start.x0 + 40.0, start.y0 + 10.0);
        dispatch(&mut w, &cursor_at(grab));
        dispatch(&mut w, &left_press());
        dispatch(&mut w, &cursor_at(Point::new(grab.x + 30.0, grab.y + 30.0)));
        assert_eq!(
            Widget::cursor_shape(&w, Point::new(5.0, 5.0)),
            Some(CursorIcon::Grabbing),
            "a move drag keeps the grabbing cursor even off the title bar"
        );
    }

    #[test]
    fn outside_click_is_swallowed_not_passed_through() {
        let (mut w, events) = laid_out(true);
        // Click far outside the window.
        dispatch(&mut w, &cursor_at(Point::new(5.0, 5.0)));
        let handled = dispatch(&mut w, &left_press());
        assert!(handled, "outside click must be swallowed");
        assert!(w.is_open(), "outside click must NOT close the window");
        assert_eq!(events.get(), 0, "outside click must not forward to content");
    }

    #[test]
    fn inbox_content_press_forwards_to_content() {
        let (mut w, events) = laid_out(true);
        // Press inside the content area (below the title bar, away from grips).
        let c = w.content_area().center();
        dispatch(&mut w, &cursor_at(c));
        let before = events.get();
        let handled = dispatch(&mut w, &left_press());
        assert!(handled, "in-box press consumed");
        assert!(
            events.get() > before,
            "in-box content press forwarded to content"
        );
    }

    #[test]
    fn modal_capture_reflects_open() {
        let (w, _e) = laid_out(true);
        assert!(Widget::modal_capture(&w).is_some());
        let (closed, _e) = laid_out(false);
        assert!(Widget::modal_capture(&closed).is_none());
    }

    #[test]
    fn draws_chrome_when_open() {
        let (mut w, _e) = laid_out(true);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(
            fills >= 3,
            "open window draws shadow+body+title chrome, got {fills}"
        );
    }
}
