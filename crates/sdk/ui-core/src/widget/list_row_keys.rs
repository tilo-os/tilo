//! How a focused row answers the keyboard: the arrows arm one of its actions and Enter or
//! Space runs it, Space toggles a leading checkbox, Delete runs its destructive action, and
//! a key a command in its menu declares runs that command.

use winit::event::ElementState;
use winit::event::WindowEvent;
use winit::keyboard::Key;
use winit::keyboard::NamedKey;

use super::Leading;
use super::ListRow;

impl ListRow {
    /// Arrow keys step through the actions of a focused row; Enter or Space runs the armed
    /// one, Escape disarms.
    pub(super) fn handle_key(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let WindowEvent::KeyboardInput { event, .. } = event else {
            return false;
        };
        if !self.activation.state.focused
            || self.activation.state.disabled
            || event.state != ElementState::Pressed
        {
            return false;
        }
        let count = self.shown_actions().len();
        let handled = match &event.logical_key {
            Key::Named(NamedKey::ArrowRight) if count > 0 => {
                self.armed_action = Some(
                    self.armed_action
                        .map_or(0, |i| i.saturating_add(1).min(count.saturating_sub(1))),
                );
                true
            }
            Key::Named(NamedKey::ArrowLeft) if self.armed_action.is_some() => {
                self.armed_action = self.armed_action.and_then(|i| i.checked_sub(1));
                true
            }
            Key::Named(NamedKey::Escape) if self.armed_action.is_some() => {
                self.armed_action = None;
                true
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) if self.armed_action.is_some() => {
                let armed = self.armed_action.unwrap_or_default();
                self.run_action(armed)
            }
            Key::Named(NamedKey::Space)
                if matches!(self.leading, Some(Leading::Checkbox { .. })) =>
            {
                self.toggle_checkbox()
            }
            Key::Named(NamedKey::Delete) => match &mut self.swipe {
                Some(swipe) if swipe.destructive => {
                    swipe.run();
                    true
                }
                _ => false,
            },
            _ => false,
        };
        let handled = handled
            || crate::platform::Shortcut::pressed(
                &event.logical_key,
                ctx.modifiers.state(),
                crate::platform::PlatformConventions::default(),
            )
            .and_then(|pressed| {
                self.shortcuts
                    .iter()
                    .find(|(declared, _)| declared.same_press(&pressed))
                    .map(|(_, run)| run.clone())
            })
            .is_some_and(|run| {
                run();
                true
            });
        if handled {
            self.dirty = true;
            *ctx.handled = true;
        }
        handled
    }
}
