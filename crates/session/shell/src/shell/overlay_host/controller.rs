//! Game-controller intents on the shell side.
//!
//! Desktop-layout intents run the same host operations the keyboard
//! shortcuts do. Intents aimed at whatever has focus — an app or an open
//! shell surface — come back as the key a keyboard user would press, so the
//! compositor delivers them through the ordinary seat and every app gets
//! controller navigation without knowing controllers exist.

use crate::compositor::controller_nav::{NavDirection, NavIntent};
use crate::shell::chrome_state::SearchScope;
use crate::shell::search::ResourceKind;

use super::{ShellOverlayHost, ShortcutGuideFocus};

/// The key a controller intent stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerKey {
    /// Directional focus: Alt+arrow in an app (the toolkit's 2D focus
    /// move), a bare arrow inside an open shell surface.
    Move(NavDirection),
    /// Enter.
    Activate,
    /// Escape.
    Back,
}

/// Where a controller intent ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerDelivery {
    /// The shell performed it.
    Handled,
    /// Deliver this key to the focused surface.
    Key(ControllerKey),
}

impl ShellOverlayHost {
    /// Adopt the compositor's input environment, repainting when it changed
    /// so hints and the guide switch to the devices in use.
    pub fn apply_input_environment(&mut self, environment: ui::InteractionEnvironment) {
        if environment != self.interaction {
            ui::widget::Widget::set_interaction_environment(self, environment);
            self.bump_chrome();
        }
    }

    /// Perform `intent`, or name the key that performs it on the focused
    /// surface.
    pub fn apply_controller_intent(&mut self, intent: NavIntent) -> ControllerDelivery {
        match intent {
            NavIntent::Move(direction) => {
                return ControllerDelivery::Key(ControllerKey::Move(direction));
            }
            NavIntent::Activate => return ControllerDelivery::Key(ControllerKey::Activate),
            NavIntent::Back => return ControllerDelivery::Key(ControllerKey::Back),
            NavIntent::Actions => self.toggle_focused_app_actions(),
            NavIntent::Search => self.open_palette_with_scope(SearchScope::All),
            NavIntent::Help => self.toggle_controller_guide(),
            NavIntent::Overview => {
                if let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
                    st.overlays.overview = !st.overlays.overview;
                    st.bump();
                }
            }
            NavIntent::QuickSettings => {
                if let Some(chrome) = self.chrome.clone() {
                    chrome.borrow_mut().toggle_quick_settings();
                }
            }
            NavIntent::PreviousTab => {
                let _ = self.cycle_focused_tab(-1);
            }
            NavIntent::NextTab => {
                let _ = self.cycle_focused_tab(1);
            }
            NavIntent::PreviousWorkspace | NavIntent::NextWorkspace => {
                let direction = if intent == NavIntent::PreviousWorkspace {
                    -1
                } else {
                    1
                };
                if let Some(chrome) = self.chrome.clone() {
                    let _ = chrome.borrow_mut().switch_workspace(direction);
                }
            }
            NavIntent::FocusPreviousWindow => {
                let _ = self.step_focus_along_strip(-1);
            }
            NavIntent::FocusNextWindow => {
                let _ = self.step_focus_along_strip(1);
            }
            NavIntent::PreviousStrip | NavIntent::NextStrip => {
                let direction = if intent == NavIntent::PreviousStrip {
                    -1
                } else {
                    1
                };
                if let Some(chrome) = self.chrome.clone() {
                    let _ = chrome.borrow_mut().switch_strip(direction);
                }
            }
            NavIntent::MoveGroupLeft => {
                let _ = self.reorder_focused_group(-1);
            }
            NavIntent::MoveGroupRight => {
                let _ = self.reorder_focused_group(1);
            }
        }
        ControllerDelivery::Handled
    }

    /// The focused app's typed actions — the same registry entries search,
    /// automation and agents see — in the palette, narrowed to that app.
    fn toggle_focused_app_actions(&mut self) {
        if self.palette_visible {
            self.palette_visible = false;
            self.palette.close();
            return;
        }
        // Resolve the scope against the tab that has focus now, not a
        // stale pointer from before the last controller move.
        self.harvest_active_tab_writeback();
        self.open_palette_with_scope(SearchScope::ActiveApp);
        self.active_kind_filter = vec![ResourceKind::Action];
    }

    /// Start/Menu opens the guide on its Done button, so A closes it and B
    /// backs out — both through the guide's own keyboard handling.
    fn toggle_controller_guide(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let open = {
            let mut st = chrome.borrow_mut();
            st.overlays.shortcut_help = !st.overlays.shortcut_help;
            st.bump();
            st.overlays.shortcut_help
        };
        self.shortcut_rebind = None;
        self.shortcut_rebind_message = None;
        self.shortcut_guide_focus = open.then_some(ShortcutGuideFocus::Done);
    }
}
