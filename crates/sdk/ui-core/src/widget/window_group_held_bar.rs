//! The window's bar held by the row its app draws.
//!
//! A window shows one bar. A header row that can stand as the bar's slots draws none of its own
//! and the strip draws the bar ([`TabBar`](crate::widget::TabBar)); a row that cannot (it holds a
//! catch-up, a popover) draws itself at the window's top and is the window's bar: the window draws
//! none above it, the row fills the window, and the window's own furniture stands at the row's
//! trailing end — its close/restore controls, its search and its new-tab door
//! ([`RowFurniture`](crate::widget::window_header::RowFurniture)). A person moves the window by the
//! row it holds, as by the bar's own band.
//! [`crate::widget::window_header::content_bar`] is that answer, which the frames of the shell's
//! preset and the frame the layout check stages read too.

use super::WindowGroup;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::window_controls::{ControlsState, WindowControl};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, MouseButton, WindowEvent};

impl WindowGroup {
    /// Offer the window's bar to the active app's own header row, and read whether that row is the
    /// window's bar: a row that cannot stand as the bar's slots (it holds a catch-up, a popover)
    /// draws itself at the window's top, and the window keeps its furniture at the row's trailing
    /// end, as a window of the shell's frames draws it. A row that stands as the slots draws none,
    /// and the window keeps its bar.
    ///
    /// A window of several places shows them as its bar, and a fullscreen window's bar is the
    /// desktop's: neither hands its bar to the app.
    pub(super) fn offer_bar_to_app(&mut self) -> Option<crate::widget::WindowBarSlot> {
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
            controls_width: crate::widget::window_header::merged_row_width(),
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

    /// Paint the window's furniture in the row its app draws, over the app's own drawing: its
    /// close/restore controls, its new-tab door and the window's search, as the bar shows them.
    pub(super) fn draw_held_furniture(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        if self.held_bar.is_none() {
            return;
        }
        let lifts = self
            .held_hover
            .or(self.held_pressed)
            .map_or([0.0; 3], |control| {
                let mut lifts = [0.0; 3];
                lifts[control.index()] = 1.0;
                lifts
            });
        let state = crate::widget::window_header::RowFurnitureState {
            controls: ControlsState {
                maximized: self.maximized,
                hovered: self.held_hover,
                pressed: self.held_pressed,
                lifts,
            },
            new_tab_hovered: self.held_new_tab_hover,
        };
        self.held.paint(
            scene,
            transform,
            tr,
            theme,
            presence,
            &mut self.search,
            state,
        );
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

    /// What the pointer did in the row its app draws, where the row is the window's bar: the
    /// window's furniture there is the window's, and a right press anywhere on the row opens the
    /// window's menu, as one on the bar does. Returns whether it was the window's, so nothing
    /// under it hears the event.
    pub(super) fn held_row_input(&mut self, event: &WindowEvent, scale: f64) -> bool {
        if self.held_bar.is_none() {
            return false;
        }
        let held = self.held;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = Point::new(
                    position.x / scale.max(f64::MIN_POSITIVE),
                    position.y / scale.max(f64::MIN_POSITIVE),
                );
                let hover = held.controls.hit(point);
                if hover != self.held_hover {
                    self.held_hover = hover;
                    self.held_dirty = true;
                }
                let plus = held.new_tab.area() > 0.0 && held.new_tab.contains(point);
                if plus != self.held_new_tab_hover {
                    self.held_new_tab_hover = plus;
                    self.held_dirty = true;
                }
                held.controls.pill.contains(point) || plus
            }
            WindowEvent::CursorLeft { .. } => {
                let had = self.held_hover.take().is_some()
                    || std::mem::take(&mut self.held_new_tab_hover);
                if had {
                    self.held_dirty = true;
                }
                false
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Right,
                ..
            } if *state == ElementState::Pressed => {
                if !self.held.row.contains(self.last_cursor) {
                    return false;
                }
                self.bar.open_context_menu(self.last_cursor);
                true
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let over = held.controls.hit(self.last_cursor);
                let plus = self.held_new_tab_hit(self.last_cursor);
                match state {
                    ElementState::Pressed => {
                        self.held_pressed = over;
                        self.held_new_tab_pressed = plus;
                        if over.is_some() || plus {
                            self.held_dirty = true;
                        }
                        over.is_some() || plus
                    }
                    ElementState::Released => {
                        let pressed = self.held_pressed.take();
                        let had_plus = std::mem::take(&mut self.held_new_tab_pressed);
                        let ran = pressed.is_some() && pressed == over;
                        if ran {
                            self.held_dirty = true;
                            if let Some(control) = pressed {
                                self.run_held_control(control);
                            }
                        }
                        if had_plus && plus {
                            // The window's own door: the shell decides what a new place holds.
                            self.pending_new_tab = Some(self.id);
                            self.held_dirty = true;
                        }
                        over.is_some() || plus || pressed.is_some() || had_plus
                    }
                }
            }
            _ => false,
        }
    }
}
