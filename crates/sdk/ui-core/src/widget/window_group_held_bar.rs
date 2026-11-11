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
    /// The bar's row: the band the window draws above its content, or the row its app draws at
    /// the window's top while it holds the bar. A person moves the window by either.
    pub fn bar_rect(&self) -> vello::kurbo::Rect {
        self.held_bar
            .map_or_else(|| self.bar.bar_rect(), |slot| slot.row)
    }

    /// Whether `p` is in the part of the row that holds the bar which the window keeps for its own
    /// furniture: a press there is the window's controls', never the window's to move by.
    pub(super) fn held_furniture_hit(&self, p: Point) -> bool {
        self.held_bar.is_some() && self.held.trailing.contains(p)
    }

    /// Where the app's own header row stands while it holds the window's bar, and where the
    /// window's controls go; `None` while the window draws its own bar.
    #[cfg(test)]
    pub fn held_bar_for_tests(&self) -> Option<crate::widget::WindowBarSlot> {
        self.held_bar
    }

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
            controls_width: crate::widget::window_header::merged_row_width_in(self.rect.width()),
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
                resize: self.resize(),
                hovered: self.held_hover,
                pressed: self.held_pressed,
                lifts,
            },
            new_tab_hovered: self.held_new_tab_hover,
            keyboard: self
                .held_controls
                .iter()
                .find_map(super::held_controls::HeldControl::keyboard),
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

    /// What a control of the window's furniture does when a person presses it there, as assistive
    /// technology and automation press it by its node. `None` for a target that is not the
    /// furniture's (the window's bar answers those), else whether the action ran: nothing runs
    /// while no row holds the bar, or where the row had no room to place the control.
    pub(super) fn perform_held_furniture_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
    ) -> Option<bool> {
        self.held_bar?;
        let control = match target {
            crate::widget::tabs::NEW_TAB_TARGET => None,
            crate::widget::tabs::WINDOW_MAXIMIZE_TARGET => Some(WindowControl::Maximize),
            crate::widget::tabs::WINDOW_CLOSE_TARGET => Some(WindowControl::Close),
            _ => return None,
        };
        let placed = match control {
            None => self.held.new_tab,
            Some(WindowControl::Maximize) => self.held.controls.maximize,
            Some(WindowControl::Close) => self.held.controls.close,
            Some(WindowControl::Minimize) => return Some(false),
        };
        if !matches!(action, crate::a11y::Action::Activate) || placed.area() <= 0.0 {
            return Some(false);
        }
        match control {
            // The window's own door: the shell decides what a new place holds.
            None => self.pending_new_tab = Some(self.id),
            Some(control) => self.run_held_control(control),
        }
        self.held_dirty = true;
        Some(true)
    }

    /// What the window's maximize control does where the window stands now.
    #[must_use]
    pub fn resize(&self) -> crate::widget::window_controls::Resize {
        crate::widget::window_controls::Resize::of(self.maximized, self.floating)
    }

    /// The window floats over the desk, or stands in its strip: its row says so as it lays it out.
    /// A floating window is a card: it keeps no door to a new tab.
    pub fn set_floating(&mut self, floating: bool) {
        if self.floating != floating {
            self.floating = floating;
            self.held_dirty = true;
            self.bar.set_new_tab(self.opens_tabs && !floating);
        }
    }

    /// Whether the window floats over the desk ([`Self::set_floating`]).
    #[must_use]
    pub fn is_floating(&self) -> bool {
        self.floating
    }

    /// The person asked the floating window back into its strip, once.
    pub fn take_pending_snap(&mut self) -> bool {
        std::mem::take(&mut self.pending_snap)
    }

    /// A press on the window's maximize control: a floating window snaps back into its strip, any
    /// other fills it or takes its width back.
    pub(super) fn press_resize(&mut self) {
        if self.floating {
            self.pending_snap = true;
        } else {
            self.toggle_maximized();
        }
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
            WindowControl::Maximize => self.press_resize(),
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
                let plus = self.new_tab_hit(self.last_cursor);
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
