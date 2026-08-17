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
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
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
        use ui::icons::Icon as U;
        // Every glyph comes from the shared `ui::icons::Icon` set so the
        // player, nav rail and result badges match the rest of the OS. Only
        // single closed silhouettes fill cleanly; the rest stroke so open
        // accents (bars, rules) still render.
        let filled = matches!(self.kind, IconKind::Play | IconKind::Pause | IconKind::Fire);
        let glyph = match self.kind {
            IconKind::Home => U::Home,
            IconKind::Fire => U::Flame,
            IconKind::Subscriptions => U::MonitorPlay,
            IconKind::History => U::History,
            IconKind::Settings => U::Settings,
            IconKind::Search => U::Search,
            IconKind::Like => U::ThumbsUp,
            IconKind::Dislike => U::ThumbsDown,
            IconKind::Bell => U::Bell,
            IconKind::Play => U::Play,
            IconKind::Pause => U::Pause,
            IconKind::Check => U::Check,
            IconKind::Plus => U::Plus,
            IconKind::Chevron => U::ChevronRight,
            IconKind::Back => U::ChevronLeft,
            IconKind::Playlist => U::ListVideo,
            IconKind::Shield => U::Shield,
            IconKind::Sparkle => U::Sparkles,
            IconKind::Eye => U::Eye,
            IconKind::Skip => U::SkipForward,
            IconKind::Menu => U::Menu,
        };
        if filled {
            glyph.draw_filled(scene, transform, self.rect, self.color);
        } else {
            let s = self.rect.width().min(self.rect.height());
            let stroke_px = (s * 0.083).max(1.5);
            glyph.draw_with_stroke(scene, transform, self.rect, self.color, stroke_px);
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
                    width: length(46.0),
                    height: length(26.0),
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
            } => {
                if self.hovered && !*handled {
                    action::push(&self.bus, self.action.clone());
                    *handled = true;
                }
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
