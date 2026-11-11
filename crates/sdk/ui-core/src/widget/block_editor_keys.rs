//! The keys the editor answers itself, before the focused line sees them: Tab and the
//! command brackets for a list's depth, and the format menu's heading chords.

use super::{BlockEditor, BlockKind, EventCtx};
use winit::event::WindowEvent;

impl BlockEditor {
    /// Ctrl/Cmd+Alt+1..3 make the selected paragraphs a heading of that level, and
    /// Ctrl/Cmd+Alt+0 body text again: the shortcuts the format menu's rows show. `true` when
    /// the event was one of them.
    pub(super) fn try_heading_shortcut(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::Key;

        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let modifiers = ctx.modifiers.state();
        if !self.is_focused
            || !modifiers.alt_key()
            || modifiers.shift_key()
            || !(modifiers.control_key() || modifiers.super_key())
        {
            return false;
        }
        let Key::Character(key) = logical_key else {
            return false;
        };
        let level = match key.as_str() {
            "0" => None,
            "1" => Some(1),
            "2" => Some(2),
            "3" => Some(3),
            _ => return false,
        };
        self.set_heading(level);
        *ctx.handled = true;
        true
    }

    /// `TextInput` does NOT handle Tab, but intercept anyway so future
    /// `TextInput` changes don't break the contract. Returns `true` when
    /// the caller should stop routing this event any further.
    pub(super) fn try_tab_indent(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::{Key, NamedKey};

        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let Some(focused_idx) = self.focused else {
            return false;
        };
        if !self.is_focused || self.tab_navigation || ctx.modifiers.state().alt_key() {
            return false;
        }
        let command = ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();
        let delta = match logical_key {
            Key::Character(key) if command && key == "]" => 1,
            Key::Character(key) if command && key == "[" => -1,
            Key::Named(NamedKey::Tab)
                if !command
                    && (self.document_selection.is_some()
                        || self.blocks.get(focused_idx).is_some_and(|b| {
                            !matches!(
                                b.kind,
                                BlockKind::CodeBlock { .. } | BlockKind::Table { .. }
                            )
                        })) =>
            {
                if ctx.modifiers.state().shift_key() {
                    -1
                } else {
                    1
                }
            }
            _ => return false,
        };
        self.try_indent_selection(delta);
        *ctx.handled = true;
        true
    }
}
