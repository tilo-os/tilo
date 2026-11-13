//! The window's bar held by the row its app draws.
use super::WindowGroup;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::window_controls::{ControlsState, WindowControl};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, MouseButton, WindowEvent};

impl WindowGroup {
    pub fn bar_rect(&self) -> vello::kurbo::Rect {
        self.held_bar
            .map_or_else(|| self.bar.bar_rect(), |slot| slot.row)
    }

    /// Whether `p` is in the part of the row that holds the bar which the window keeps for its own
    /// furniture: a press there is the window's controls', never the window's to move by.
    pub(super) fn held_furniture_hit(&self, p: Point) -> bool {
        self.held_bar.is_some() && self.held.trailing.contains(p)
    }

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
            None => self.pending_new_tab = Some(self.id),
            Some(control) => self.run_held_control(control),
        }
        self.held_dirty = true;
        Some(true)
    }

    #[must_use]
    pub fn resize(&self) -> crate::widget::window_controls::Resize {
        crate::widget::window_controls::Resize::of_window(
            self.maximized,
            self.floating,
            self.collapsed,
        )
    }

    pub fn set_collapsed(&mut self, collapsed: bool) {
        let collapsed = collapsed && self.floating;
        if self.collapsed != collapsed {
            self.collapsed = collapsed;
            self.held_dirty = true;
            self.bar.set_resize(self.resize());
        }
    }

    pub fn set_floating(&mut self, floating: bool) {
        if self.floating != floating {
            self.floating = floating;
            self.held_dirty = true;
            self.bar.set_new_tab(self.opens_tabs && !floating);
            self.bar.set_card(floating);
            self.bar.set_resize(self.resize());
            self.bar
                .set_search_control((!floating).then_some(super::SEARCH_CONTROL_ROOM));
        }
    }

    #[must_use]
    pub fn is_floating(&self) -> bool {
        self.floating
    }

    pub fn take_pending_collapse(&mut self) -> bool {
        self.bar.take_collapse()
    }

    pub fn take_pending_snap(&mut self) -> bool {
        std::mem::take(&mut self.pending_snap)
    }

    pub(super) fn press_resize(&mut self) {
        if self.floating {
            self.pending_snap = true;
        } else {
            self.toggle_maximized();
        }
    }

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
            WindowControl::Minimize => {}
        }
    }

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
