//! The Linux header bar in a live window. Who decorates is settled once by
//! `xdg-decoration`: where the compositor draws the frame (KDE, X11 window
//! managers) the app keeps it and the in-window History bar; where it
//! leaves decorations to the client (GNOME) the app hides winit's fallback
//! frame and draws [`HeaderBar`] above its content instead.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::platform::wayland::WindowExtWayland;
use winit::window::Window;

use super::super::App;
use super::super::host_commands::HostCommandsKey;
use crate::host_chrome::HostChrome;
use crate::host_chrome::header_bar::{HeaderBar, HeaderRequest, app_draws_frame, resize_edge};
use crate::widget::Widget;

pub(super) enum Decorations {
    /// Waiting for the first configure.
    Unsettled,
    /// The desktop draws the frame, or the Tilo shell owns the chrome.
    Host,
    Bar(Box<LiveHeader>),
}

pub(super) struct LiveHeader {
    bar: HeaderBar,
    tree: TaffyTree<()>,
    nodes: HashMap<NodeId, *mut dyn Widget>,
    /// `None` until the first layout.
    node: Option<NodeId>,
    key: Option<HostCommandsKey>,
    window: Rect,
    pointer: Point,
    /// The bar changed since it was last painted. It reports no widget
    /// damage, so the frame after repaints the whole window.
    repaint: bool,
    /// A fullscreen window has no frame: the bar neither draws, takes
    /// input, nor insets the content until the window leaves fullscreen.
    hidden: bool,
}

/// What the runner still has to do after the bar saw an event.
pub(super) struct Routed {
    pub consumed: bool,
    pub close: bool,
}

impl Decorations {
    /// Settle who decorates, then keep the bar current. Call every frame
    /// before layout: the bar's height is the app's top inset.
    pub(super) fn refresh(&mut self, app: &mut App, window: &Window) {
        if matches!(self, Self::Unsettled) {
            let chrome = HostChrome::current(app.input.platform);
            *self = match app_draws_frame(chrome, window.server_side_decorations()) {
                None => return,
                Some(true) => {
                    window.set_decorations(false);
                    app.prepare_native_presentation(HostChrome::Native);
                    Self::Bar(Box::new(LiveHeader::new(app, window)))
                }
                Some(false) => Self::Host,
            };
        }
        if let Self::Bar(header) = self {
            header.refresh(app, window);
        }
    }

    /// Whether the bar changed since the last frame, which must then repaint
    /// the whole window rather than only the app's own damage.
    pub(super) fn take_repaint(&mut self) -> bool {
        match self {
            Self::Bar(header) => std::mem::take(&mut header.repaint),
            Self::Unsettled | Self::Host => false,
        }
    }

    pub(super) fn draw(&mut self, app: &mut App, scene: &mut Scene, transform: Affine) {
        if let Self::Bar(header) = self
            && !header.hidden
        {
            let (text, theme, presence) = (&mut app.text_renderer, &app.theme, &app.presence_ctx);
            header.bar.draw(scene, transform, text, theme, presence);
            header
                .bar
                .draw_overlay(scene, transform, text, theme, presence);
        }
    }

    pub(super) fn route(&mut self, app: &mut App, window: &Window, event: &WindowEvent) -> Routed {
        match self {
            Self::Bar(header) if !header.hidden => header.route(app, window, event),
            Self::Bar(_) | Self::Unsettled | Self::Host => Routed {
                consumed: false,
                close: false,
            },
        }
    }
}

impl LiveHeader {
    fn new(app: &mut App, window: &Window) -> Self {
        Self {
            bar: HeaderBar::new(&window.title(), &app.host_commands(true)),
            tree: TaffyTree::new(),
            nodes: HashMap::new(),
            node: None,
            key: None,
            window: Rect::ZERO,
            pointer: Point::ORIGIN,
            repaint: true,
            hidden: false,
        }
    }

    fn refresh(&mut self, app: &mut App, window: &Window) {
        let physical = window.inner_size();
        let size =
            crate::scale::logical_size((physical.width, physical.height), window.scale_factor());
        let bounds = Rect::new(0.0, 0.0, size.0, size.1);
        let hidden = window.fullscreen().is_some();
        if hidden != self.hidden {
            self.hidden = hidden;
            self.repaint = true;
            // Lay the bar out again (or not) for the new state.
            self.window = Rect::ZERO;
        }
        if hidden {
            if app.layout.safe_area.top != 0.0 {
                app.layout.safe_area.top = 0.0;
                app.layout.layout_dirty = true;
            }
            return;
        }
        let key = app.host_commands_key();
        let mut rebuilt = false;
        if self.key != Some(key) && self.bar.modal_capture().is_none() {
            self.key = Some(key);
            let (title, commands) = (window.title(), app.host_commands(true));
            // A new bar forgets the pointer: a press would become a move.
            if !self.bar.presents(&title, &commands) {
                self.bar = HeaderBar::new(&title, &commands);
                rebuilt = true;
            }
        }
        if !rebuilt && bounds == self.window {
            return;
        }
        self.repaint = true;
        self.window = bounds;
        self.bar.set_viewport(bounds);
        self.node = self.bar.solve_layout(
            &mut self.tree,
            &mut self.nodes,
            &app.theme,
            &mut app.text_renderer,
            bounds.width(),
        );
        if self.node.is_none() {
            return;
        }
        let area = crate::scale::SafeArea {
            top: self.bar.get_rect().height(),
            ..app.layout.safe_area
        };
        if area != app.layout.safe_area {
            app.layout.safe_area = area;
            app.layout.layout_dirty = true;
        }
    }

    fn route(&mut self, app: &mut App, window: &Window, event: &WindowEvent) -> Routed {
        let mut routed = Routed {
            consumed: false,
            close: false,
        };
        let Some(node) = self.node else {
            return routed;
        };
        if let WindowEvent::CursorMoved { position, .. } = event {
            let scale = window.scale_factor();
            self.pointer = Point::new(position.x / scale, position.y / scale);
        }
        let edge = (!window.is_maximized() && self.bar.modal_capture().is_none())
            .then(|| resize_edge(self.pointer, self.window, &app.theme))
            .flatten();
        match event {
            WindowEvent::CursorMoved { .. } if edge.is_some() => {
                if let Some(edge) = edge {
                    window.set_cursor(winit::window::CursorIcon::from(edge));
                }
                routed.consumed = true;
                return routed;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if edge.is_some() => {
                if let Some(edge) = edge {
                    let _ = window.drag_resize_window(edge);
                }
                routed.consumed = true;
                return routed;
            }
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
                if self.bar.claims(self.pointer) => {}
            // An open History menu has the keyboard: arrows, Enter, Escape.
            WindowEvent::KeyboardInput { .. } if self.bar.modal_capture().is_some() => {}
            _ => return routed,
        }
        let mut handled = false;
        let mut focus = None;
        self.bar.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: window.scale_factor(),
                modifiers: &app.input.modifiers,
                state: crate::widget::Edits::lend(&mut app.state.doc),
                path: &app.state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &self.tree,
            },
        );
        routed.consumed = true;
        self.repaint = true;
        for request in self.bar.take_requests() {
            match request {
                HeaderRequest::Command(invocation) => {
                    app.invoke_host_command(&invocation);
                    app.needs_sync = true;
                    app.layout.layout_dirty = true;
                }
                HeaderRequest::Move => {
                    let _ = window.drag_window();
                }
                HeaderRequest::ToggleMaximize => window.set_maximized(!window.is_maximized()),
                HeaderRequest::Minimize => window.set_minimized(true),
                HeaderRequest::Close => routed.close = true,
            }
        }
        window.request_redraw();
        routed
    }
}
