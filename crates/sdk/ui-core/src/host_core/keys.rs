//! What a key no widget took does: the host's fallbacks for Tab, arrows,
//! Enter and Space, range steps and the desktop's tab chords, written once
//! for the native and headless app hosts. Each host lends its keyboard
//! state and focus through [`KeyHost`].

use taffy::{NodeId, TaffyTree};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::HostCore;
use crate::focus_navigation::Direction;

/// The app root and what dispatching an event to it needs.
pub(crate) struct RootEvent<'a> {
    pub(crate) root: &'a mut dyn crate::widget::Widget,
    pub(crate) state: &'a mut crate::State,
    pub(crate) modifiers: &'a winit::event::Modifiers,
    pub(crate) taffy: &'a TaffyTree<()>,
    pub(crate) node: NodeId,
    pub(crate) scale_factor: f64,
}

impl RootEvent<'_> {
    /// Hand `event` to the root widget.
    pub(crate) fn dispatch(
        self,
        event: &WindowEvent,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
    ) {
        self.root.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: self.scale_factor,
                modifiers: self.modifiers,
                state: crate::widget::Edits::lend(&mut self.state.doc),
                path: &self.state.root,
                handled,
                focus_target,
                node_id: self.node,
                taffy: self.taffy,
            },
        );
    }
}

/// The named key `event` presses, if any.
fn pressed_named(event: &WindowEvent) -> Option<NamedKey> {
    match event {
        WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(named),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } => Some(*named),
        _ => None,
    }
}

pub(crate) trait KeyHost: HostCore {
    /// The modifier keys held now.
    fn modifiers(&self) -> &winit::event::Modifiers;

    /// The platform whose command modifier and shortcuts apply.
    fn platform(&self) -> crate::platform::PlatformConventions;

    /// Where Tab and arrows move focus, in order.
    fn keyboard_travel_order(&self) -> Vec<NodeId>;

    /// Move keyboard focus to `node`.
    fn move_focus(&mut self, node: Option<NodeId>);

    /// A fallback changed what a laid-out widget shows or holds.
    fn key_fallback_acted(&mut self);

    /// The app root to dispatch to, once it is laid out.
    fn root_event(&mut self) -> Option<RootEvent<'_>>;

    /// Whether History holds the keyboard.
    fn history_captures_input(&self) -> bool {
        self.tree()
            .history
            .is_some_and(|history| history.captures_input())
    }

    /// Whether the host's own focus order goes on past the app, so Tab from
    /// its last control (Shift+Tab from its first) leaves it.
    fn tab_order_continues_in_host(&self) -> bool {
        false
    }

    /// Keys the host answers itself, after Enter and Space and before the
    /// desktop's tab chords. Whether it took `event`.
    fn handle_unconsumed_host_key(&mut self, _event: &WindowEvent) -> bool {
        false
    }

    /// Move focus to the nearest control in `direction`, or to the first
    /// one when nothing holds focus.
    fn navigate_focus(&mut self, direction: Direction) {
        let order = self.keyboard_travel_order();
        let Some(current) = self.tree().focused else {
            if let Some(&first) = order.first() {
                self.move_focus(Some(first));
            }
            return;
        };
        let Ok(current_rect) = self.inspect_node(current, |widget| widget.get_rect()) else {
            return;
        };
        let next = crate::focus_navigation::next(
            (current, current_rect),
            order.iter().filter_map(|&id| {
                self.inspect_node(id, |widget| widget.get_rect())
                    .ok()
                    .map(|rect| (id, rect))
            }),
            direction,
        );
        if let Some(next) = next {
            self.move_focus(Some(next));
        }
    }

    /// Tab (Shift+Tab when `shift`) through the travel order. Whether it
    /// moved focus inside the app.
    fn focus_tab(&mut self, shift: bool) -> bool {
        let order = self.keyboard_travel_order();
        let len = order.len();
        if len == 0 {
            return false;
        }
        let focused = self.tree().focused;
        if self.tab_order_continues_in_host() {
            let at = focused.and_then(|id| order.iter().position(|&n| n == id));
            if at == Some(if shift { 0 } else { len - 1 }) {
                self.move_focus(None);
                return false;
            }
        }
        let Some(next) = crate::focus_navigation::tab_index(&order, focused, shift) else {
            return false;
        };
        if let Some(&id) = order.get(next) {
            self.move_focus(Some(id));
        }
        true
    }

    /// Tab and Shift+Tab move focus, unless the focused widget takes Tab or
    /// `navigation` is fenced by a modal or focus trap. Ctrl+Tab a widget
    /// leaves unhandled is the desktop's next tab (`crate::shell_tabs`),
    /// and Alt and Super chords are the desktop's too.
    fn handle_tab_navigation(&mut self, event: &WindowEvent, navigation: bool) -> bool {
        if !navigation
            || pressed_named(event) != Some(NamedKey::Tab)
            || self.focused_widget_wants_tab_input()
        {
            return false;
        }
        let modifiers = self.modifiers().state();
        if modifiers.control_key() || modifiers.alt_key() || modifiers.super_key() {
            return false;
        }
        self.focus_tab(modifiers.shift_key())
    }

    /// Enter or Space no widget consumed activates the focused control, as a
    /// click would. A terminal root sees the key itself first. Releases
    /// never activate.
    fn handle_unconsumed_activation_key(&mut self, event: &WindowEvent, handled: &mut bool) {
        let modifiers = self.modifiers().state();
        if *handled
            || modifiers.super_key()
            || modifiers.shift_key()
            || !matches!(
                pressed_named(event),
                Some(NamedKey::Enter | NamedKey::Space)
            )
        {
            return;
        }
        if self.tree().root.name() == "Terminal"
            && !self.history_captures_input()
            && let Some(root) = self.root_event()
        {
            root.dispatch(event, handled, &mut None);
            if *handled {
                self.key_fallback_acted();
                return;
            }
        }
        let Some(focused) = self.tree().focused else {
            return;
        };
        if self
            .perform_node_action(focused, &crate::a11y::Action::Activate)
            .unwrap_or(false)
        {
            *handled = true;
            self.key_fallback_acted();
        }
    }

    /// The desktop's tab chords (Ctrl+T, Ctrl+Shift+T, Ctrl+W; Cmd on macOS)
    /// when no widget consumed the press: tabs are the shell's, so every app
    /// answers them the same way through [`crate::shell_tabs`].
    fn handle_unconsumed_tab_chord(&self, event: &WindowEvent, handled: &mut bool) {
        if *handled {
            return;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    repeat: false,
                    ..
                },
            ..
        } = event
        else {
            return;
        };
        let modifiers = self.modifiers().state();
        let command = self.platform().command_modifier_down(modifiers);
        // Ctrl and Super together are a different chord on every platform.
        let other = modifiers.alt_key() || (modifiers.control_key() && modifiers.super_key());
        if let Some(request) =
            crate::shell_tabs::chord_request(logical_key, command, modifiers.shift_key(), other)
        {
            crate::shell_tabs::request(request);
            *handled = true;
        }
    }

    /// A focused range control steps on Left and Right no widget consumed:
    /// one step, four with Shift, to its ends with the command modifier.
    /// Other widgets ignore the action.
    fn handle_unconsumed_range_key(&mut self, event: &WindowEvent, handled: &mut bool) {
        use crate::a11y::Action;
        if *handled {
            return;
        }
        let Some(named) = pressed_named(event) else {
            return;
        };
        let modifiers = self.modifiers().state();
        let command = self.platform().command_modifier_down(modifiers);
        let times = if modifiers.shift_key() { 4 } else { 1 };
        let (action, times) = match named {
            NamedKey::ArrowLeft if command => (Action::SetValue("0".into()), 1),
            NamedKey::ArrowRight if command => (Action::SetValue("100".into()), 1),
            NamedKey::ArrowLeft => (Action::Decrement, times),
            NamedKey::ArrowRight => (Action::Increment, times),
            _ => return,
        };
        let Some(focused) = self.tree().focused else {
            return;
        };
        let mut any = false;
        for _ in 0..times {
            any |= self.perform_node_action(focused, &action).unwrap_or(false);
        }
        if any {
            *handled = true;
            self.key_fallback_acted();
        }
    }

    /// An arrow no widget consumed moves focus (only with Alt when
    /// `require_alt`), behind the same modal and trap fence as Tab.
    fn handle_unconsumed_directional_key(
        &mut self,
        event: &WindowEvent,
        navigation: bool,
        require_alt: bool,
        handled: &mut bool,
    ) {
        if *handled || !navigation || (require_alt && !self.modifiers().state().alt_key()) {
            return;
        }
        let Some(direction) = pressed_named(event).and_then(Direction::from_key) else {
            return;
        };
        self.navigate_focus(direction);
        *handled = true;
    }

    /// Every fallback, in order, for a key the widgets left unhandled.
    /// Alt+Arrow runs after widget dispatch, so editors take platform word
    /// and selection shortcuts first, and before the plain range step.
    /// `tab_chords` is false where the window itself answers them.
    fn handle_unconsumed_key(
        &mut self,
        event: &WindowEvent,
        navigation: bool,
        tab_chords: bool,
        handled: &mut bool,
    ) {
        self.handle_unconsumed_directional_key(event, navigation, true, handled);
        self.handle_unconsumed_activation_key(event, handled);
        if !*handled && self.handle_unconsumed_host_key(event) {
            *handled = true;
        }
        if tab_chords {
            self.handle_unconsumed_tab_chord(event, handled);
        }
        self.handle_unconsumed_range_key(event, handled);
        self.handle_unconsumed_directional_key(event, navigation, false, handled);
    }
}
