//! The window's bar held by the row its app draws.
//!
//! A window shows one bar. A header row that can stand as the bar's slots draws none of its own
//! and the strip draws the bar ([`TabBar`]); a row that cannot (it holds a catch-up, a popover)
//! draws itself at the window's top and is the window's bar: the window draws none above it,
//! the row fills the window, and the window's controls stand at the row's trailing end, where
//! they close, restore and maximize the window as the bar's own do.
//! [`crate::widget::window_header::content_bar`] is that answer, which the frames of the shell's
//! preset and the frame the layout check stages read too.

use super::{WindowGroup, merged_controls_width};
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::window_controls::{WindowControl, WindowControls};
use vello::kurbo::Point;
use winit::event::{ElementState, WindowEvent};

impl WindowGroup {
    /// Offer the window's bar to the active app's own header row, and read whether that row is the
    /// window's bar: a row that cannot stand as the bar's slots (it holds a catch-up, a popover)
    /// draws itself at the window's top, and the window's controls stand at its trailing end, as
    /// a window of the shell's frames draws it. A row that stands as the slots draws none, and
    /// the window keeps its bar.
    ///
    /// A window of several places shows them as its bar, and a fullscreen window's bar is the
    /// desktop's: neither hands its bar to the app.
    pub(super) fn offer_bar_to_app(
        &mut self,
        theme: &Theme,
    ) -> Option<crate::widget::WindowBarSlot> {
        let active = self.bar.active_id();
        // Nothing of the app's stands in the bar: a shell whose row can be the bar's slots declares
        // them there, and another part of the window declares them of its own, and the bar is
        // theirs then, not the row's.
        let declares_nothing = self.bar.toolbar(active).is_none();
        let hands_its_bar = declares_nothing
            && self.bar.tabs().len() <= 1
            && !self.external_navigation
            && !self.fullscreen_presentation;
        let host = self
            .hosts
            .iter_mut()
            .find(|(id, _)| *id == active)
            .map(|(_, host)| host)?;
        let app = host.app_mut()?;
        let app = app as &mut dyn Widget;
        let bar = hands_its_bar.then(|| crate::widget::WindowBar {
            controls_width: merged_controls_width(theme),
            revealed: true,
            top_inset: 0.0,
            lists_actions: false,
        });
        crate::widget::window_header::offer_window_bar(app, bar);
        crate::widget::window_header::content_bar(app)
    }

    /// Read where the active app's header row stands, after layout: the row it draws at the
    /// window's top is the window's bar while it holds it.
    pub(super) fn read_held_bar(&mut self) -> Option<crate::widget::WindowBarSlot> {
        let active = self.bar.active_id();
        let host = self
            .hosts
            .iter_mut()
            .find(|(id, _)| *id == active)
            .map(|(_, host)| host)?;
        let app = host.app_mut()?;
        crate::widget::window_header::content_bar(app as &dyn Widget)
    }

    /// The window's controls where they stand while its app's header row holds the bar.
    pub(super) fn lay_out_held_controls(&mut self) -> WindowControls {
        let controls = self.held_bar.map_or_else(WindowControls::default, |slot| {
            WindowControls::lay_out(slot.controls)
        });
        self.held_controls = controls;
        controls
    }

    /// Run a press on the window's controls in a held bar's row: close ends the window, restore
    /// and maximize change its place, as the bar's own controls do.
    pub(super) fn run_held_control(&mut self, control: WindowControl) {
        match control {
            WindowControl::Close => {
                let id = self.bar.active_id();
                if self.bar.is_held(id) {
                    return;
                }
                let started = self.close_started();
                self.closing_hosts.entry(id).or_insert(started);
                if !self.pending_close_commit.contains(&id) {
                    self.pending_close_commit.push(id);
                }
                if self.bar.tabs().is_empty() {
                    self.start_closing();
                }
            }
            WindowControl::Maximize => {
                self.begin_rect_spring_from_current();
                self.maximized = !self.maximized;
                self.style_dirty = true;
                self.pending_maximize_change = Some(self.maximized);
            }
            // The strip closes and restores; the desktop's window menu minimizes.
            WindowControl::Minimize => {}
        }
    }

    /// What the pointer did with the window's controls in a held bar's row. Returns whether it
    /// was theirs, so nothing under it hears the event.
    pub(super) fn held_control_input(&mut self, event: &WindowEvent, scale: f64) -> bool {
        if self.held_bar.is_none() || self.held_controls.pill.area() <= 0.0 {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = Point::new(
                    position.x / scale.max(f64::MIN_POSITIVE),
                    position.y / scale.max(f64::MIN_POSITIVE),
                );
                let hover = self.held_controls.hit(point);
                if hover != self.held_hover {
                    self.held_hover = hover;
                    self.held_dirty = true;
                }
                self.held_controls.pill.contains(point)
            }
            WindowEvent::CursorLeft { .. } => {
                let had = self.held_hover.take().is_some();
                if had {
                    self.held_dirty = true;
                }
                false
            }
            WindowEvent::MouseInput {
                state,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let over = self.held_controls.hit(self.last_cursor);
                match state {
                    ElementState::Pressed => {
                        self.held_pressed = over;
                        if over.is_some() {
                            self.held_dirty = true;
                        }
                        over.is_some()
                    }
                    ElementState::Released => {
                        let pressed = self.held_pressed.take();
                        if pressed.is_some() && pressed == over {
                            self.held_dirty = true;
                            if let Some(control) = pressed {
                                self.run_held_control(control);
                            }
                        }
                        over.is_some() || pressed.is_some()
                    }
                }
            }
            _ => false,
        }
    }
}
