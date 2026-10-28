//! The pane a connected session draws in: the remote desktop's framebuffer or
//! its terminal grid, in the rect the window gives it, and the pointer and
//! keyboard it forwards into that remote.

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Fill, ImageBrush};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{ModifiersState, PhysicalKey};

use ui::TextRole;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{TaffyTreeExt, Widget};

use crate::session::SessionStatus;
use crate::vnc::VncInput;

/// A connected session: the remote terminal grid or desktop, painted from
/// the transport's own state, with the pointer and keyboard forwarded to the
/// remote machine while the pane has focus.
pub(crate) struct SessionPane {
    session: SharedSession,
    rect: Rect,
    focused: bool,
    /// How far one wheel notch scrolls, from the theme the pane was last
    /// laid out with: a trackpad clicks a VNC desktop's wheel this often.
    wheel_line: f64,
}

use super::keys::{ssh_key_bytes, vnc_keysym};
use super::{LiveSession, SampleSurface, SessionCore, SharedSession};

impl SessionPane {
    pub(super) fn new(session: SharedSession) -> Self {
        Self {
            session,
            rect: Rect::ZERO,
            focused: true,
            wheel_line: f64::from(ui::tokens::metrics::Metrics::BASE.wheel_line),
        }
    }

    /// Paint the remote framebuffer, letterboxed to fit or at its own size.
    fn draw_framebuffer(
        core: &mut SessionCore,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        surface: Rect,
    ) {
        let media = &theme.tokens.colors.media;
        scene.fill(Fill::NonZero, transform, media.letterbox, None, &surface);
        let Some(img) = core.fb_image.clone() else {
            // Connected, but no frame decoded yet.
            let style = theme.text(TextRole::Body);
            let label = "Waiting for the first screen update";
            let (w, h) = text_renderer.measure_style(label, None, &style);
            text_renderer.draw_style(
                scene,
                label,
                media.ink_dim,
                transform
                    * Affine::translate((
                        surface.center().x - f64::from(w) / 2.0,
                        surface.center().y - f64::from(h) / 2.0,
                    )),
                None,
                &style,
            );
            return;
        };
        let iw = f64::from(img.width.max(1));
        let ih = f64::from(img.height.max(1));
        let (w, h) = if core.scale_to_fit {
            let scale = (surface.width() / iw).min(surface.height() / ih);
            (iw * scale, ih * scale)
        } else {
            (iw.min(surface.width()), ih.min(surface.height()))
        };
        let canvas = Rect::from_center_size(surface.center(), (w, h));
        core.fb_canvas = Some(canvas);
        let sx = canvas.width() / iw;
        let sy = canvas.height() / ih;
        let brush_transform =
            Affine::translate((canvas.x0, canvas.y0)) * Affine::scale_non_uniform(sx, sy);
        scene.fill(
            Fill::NonZero,
            transform,
            &ImageBrush::new(img),
            Some(brush_transform),
            &canvas,
        );
    }

    pub(super) fn terminal_grid(core: &SessionCore) -> Option<&crate::vt::VtGrid> {
        match &core.live {
            Some(LiveSession::Ssh(s)) => Some(s.grid()),
            Some(LiveSession::Sample(SampleSurface::Terminal(grid))) => Some(grid),
            _ => None,
        }
    }

    /// Paint the SSH terminal grid, straight from the remote PTY output held
    /// in the [`crate::vt::VtGrid`], on the window's content ground in the
    /// theme's terminal palette.
    fn draw_terminal(
        grid: &crate::vt::VtGrid,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        surface: Rect,
    ) {
        let palette = &theme.tokens.terminal_palette;
        let (lines, cursor) = (grid.lines(), grid.cursor());
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &surface,
        );
        let regular = theme.text(TextRole::Code);
        let bold = regular.clone().strong();
        let (cell_w, line_h) = text_renderer.measure_style("M", None, &regular);
        let (cell_w, line_h) = (f64::from(cell_w), f64::from(line_h));
        let pad = f64::from(theme.tokens.spacing.s4);
        for (row, line) in lines.iter().enumerate() {
            let y = surface.y0 + pad + row as f64 * line_h;
            // Draw cells run by run (consecutive cells of one style).
            let mut x = surface.x0 + pad;
            let mut i = 0;
            while let Some(cell) = line.cells.get(i) {
                let style = (cell.color, cell.bold);
                let start = i;
                while line
                    .cells
                    .get(i)
                    .is_some_and(|c| (c.color, c.bold) == style)
                {
                    i += 1;
                }
                let run: String = line
                    .cells
                    .get(start..i)
                    .unwrap_or_default()
                    .iter()
                    .map(|c| c.ch)
                    .collect();
                let color = style.0.map_or(palette.fg_default, |idx| {
                    palette
                        .ansi
                        .get(idx.min(15))
                        .copied()
                        .unwrap_or(palette.fg_default)
                });
                text_renderer.draw_style(
                    scene,
                    &run,
                    color,
                    transform * Affine::translate((x, y)),
                    None,
                    if style.1 { &bold } else { &regular },
                );
                x += run.chars().count() as f64 * cell_w;
            }
        }
        // Block caret at the remote cursor, in the person's own colour.
        let (cx, cy) = cursor;
        let caret = Rect::from_origin_size(
            (
                surface.x0 + pad + cx as f64 * cell_w,
                surface.y0 + pad + cy as f64 * line_h,
            ),
            (cell_w, line_h),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.intent.selection_tint,
            None,
            &caret,
        );
        scene.pop_layer();
    }

    fn handle_pointer(&self, event: &WindowEvent, scale_factor: f64) -> bool {
        let mut core = self.session.borrow_mut();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                if p == core.pointer {
                    return false;
                }
                core.pointer = p;
                core.forward_vnc_pointer(p);
                if let Some((x, y)) = core.tilo_pointer_target(p) {
                    core.queue_tilo_input(runtime_collab::WorkspaceControlInput::PointerMotion {
                        x,
                        y,
                    });
                }
                false
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let p = core.pointer;
                core.forward_vnc_wheel(p, delta, scale_factor, self.wheel_line)
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let p = core.pointer;
                if core.tilo_pointer_target(p).is_some() {
                    let evdev_button = match button {
                        MouseButton::Left => Some(272),
                        MouseButton::Right => Some(273),
                        MouseButton::Middle => Some(274),
                        _ => None,
                    };
                    if let Some(evdev_button) = evdev_button {
                        if *state == ElementState::Pressed && evdev_button == 272 {
                            core.queue_tilo_input(
                                runtime_collab::WorkspaceControlInput::KeyboardFocus {
                                    focused: true,
                                },
                            );
                        }
                        core.queue_tilo_input(
                            runtime_collab::WorkspaceControlInput::PointerButton {
                                button: evdev_button,
                                pressed: *state == ElementState::Pressed,
                            },
                        );
                    }
                    return true;
                }
                if core.vnc_pointer_target(p).is_some() {
                    let bit = match button {
                        MouseButton::Left => 1,
                        MouseButton::Middle => 2,
                        MouseButton::Right => 4,
                        _ => 0,
                    };
                    match state {
                        ElementState::Pressed => core.vnc_buttons |= bit,
                        ElementState::Released => core.vnc_buttons &= !bit,
                    }
                    core.forward_vnc_pointer(p);
                    return true;
                }
                self.rect.contains(p)
            }
            _ => false,
        }
    }

    pub(super) fn handle_key(&self, event: &KeyEvent, mods: ModifiersState) -> bool {
        let mut core = self.session.borrow_mut();
        let tilo_connected = matches!(
            core.live.as_ref(),
            Some(LiveSession::Tilo(session)) if session.status == SessionStatus::Connected
        );
        let pressed = event.state == ElementState::Pressed;
        if tilo_connected {
            if let PhysicalKey::Code(code) = event.physical_key
                && let Some(keycode) = ui::evdev::from_key_code(code)
            {
                // A release goes only for a key still held there: one the
                // window took on its way here was released already.
                if let Some(keycode) =
                    core.track_key(event.physical_key, pressed.then_some(keycode))
                {
                    core.queue_tilo_input(runtime_collab::WorkspaceControlInput::Key {
                        keycode,
                        pressed,
                        modifiers: 0,
                    });
                }
                return true;
            }
            return false;
        }
        if core.vnc_session().is_some() {
            // Down on the press and up on the release, modifiers included,
            // so a held Ctrl or Shift reaches the remote desktop with the key
            // it modifies.
            let Some(key) = core.track_key(
                event.physical_key,
                pressed.then(|| vnc_keysym(&event.logical_key)).flatten(),
            ) else {
                return false;
            };
            if let Some(s) = core.vnc_session() {
                let _ = s.send_input(VncInput::Key { down: pressed, key });
            }
            core.changed = true;
            return true;
        }
        if !pressed {
            return false;
        }
        let text = event.text.as_ref().map(winit::keyboard::SmolStr::as_str);
        let Some(LiveSession::Ssh(s)) = &core.live else {
            return false;
        };
        if s.status() != SessionStatus::Connected {
            return false;
        }
        let Some(bytes) = ssh_key_bytes(&event.logical_key, text, mods) else {
            return false;
        };
        // Interactive key events are best-effort when the bounded queue is
        // full; the resource action path reports that condition exactly to
        // agents.
        let _ = s.send_input(bytes);
        core.changed = true;
        true
    }
}

impl Widget for SessionPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.wheel_line = f64::from(theme.tokens.metrics.wheel_line);
        let style = Style {
            flex_grow: 1.0,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let mut core = self.session.borrow_mut();
        if let Some(grid) = Self::terminal_grid(&core) {
            Self::draw_terminal(grid, scene, transform, text_renderer, theme, self.rect);
        } else {
            Self::draw_framebuffer(&mut core, scene, transform, text_renderer, theme, self.rect);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let handled = match event {
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. } => {
                if matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        ..
                    }
                ) {
                    let focused = self.rect.contains(self.session.borrow().pointer);
                    self.set_focus(focused);
                }
                self.handle_pointer(event, ctx.scale_factor)
            }
            WindowEvent::KeyboardInput { event, .. } if self.focused => {
                self.handle_key(event, ctx.modifiers.state())
            }
            // The window lost the keyboard: what is held here is released
            // somewhere else.
            WindowEvent::Focused(false) => {
                self.session.borrow_mut().release_held_keys();
                false
            }
            _ => false,
        };
        if handled {
            *ctx.handled = true;
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn is_default_keyboard_focus(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if !focused {
            self.session.borrow_mut().release_held_keys();
        }
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn wants_tab_input(&self) -> bool {
        true
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let core = self.session.borrow();
        let name = if Self::terminal_grid(&core).is_some() {
            "Remote terminal"
        } else {
            "Remote desktop"
        };
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_name(name)
                .with_bounds(self.rect)
                .with_state(
                    ui::a11y::AccessState::new()
                        .focusable(true)
                        .focused(self.focused),
                ),
        )
    }

    fn needs_redraw(&self) -> bool {
        self.session.borrow().changed
    }

    fn name(&self) -> &'static str {
        "SessionPane"
    }
}
