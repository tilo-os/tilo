use ui::widget::{EventCtx, Widget};
use winit::event::{KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::{BlockEditor, DocumentPosition, DocumentSelection, handle_visual_formula_key};

impl BlockEditor {
    /// For an event that can only type into the focused paragraph (a
    /// character, an IME commit, Backspace or Delete, with no selection
    /// across paragraphs and no find bar), the paragraph's index, the block
    /// count and its id: what must still hold afterwards for the edit to
    /// have stayed in that paragraph.
    pub(super) fn typing_position(
        &self,
        event: &WindowEvent,
        modifiers: Modifiers,
    ) -> Option<(usize, usize, String)> {
        use winit::keyboard::NamedKey;
        let plain = match event {
            WindowEvent::Ime(_) => true,
            WindowEvent::KeyboardInput { event, .. } => {
                let held = modifiers.state();
                !held.control_key()
                    && !held.alt_key()
                    && !held.super_key()
                    && matches!(
                        &event.logical_key,
                        Key::Character(_)
                            | Key::Named(NamedKey::Backspace | NamedKey::Delete | NamedKey::Space)
                    )
            }
            _ => false,
        };
        if !plain || self.document_selection.is_some() || !self.is_focused {
            return None;
        }
        let index = self.focused?;
        let block = self.blocks.get(index)?;
        Some((index, self.blocks.len(), block.id.clone()))
    }

    pub(super) fn handle_event_unscoped(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        use winit::event::{ElementState, MouseButton};

        // The IME owns candidate-selection keys until it commits/cancels.
        // Do not let Enter, Tab, or Backspace become structural document edits.
        if self.is_focused
            && matches!(event, WindowEvent::KeyboardInput { .. })
            && self.is_composing()
        {
            *ctx.handled = true;
            return;
        }

        let changes_selection = match event {
            WindowEvent::Ime(winit::event::Ime::Commit(_)) => true,
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match &event.logical_key {
                    Key::Named(
                        NamedKey::ArrowLeft
                        | NamedKey::ArrowRight
                        | NamedKey::ArrowUp
                        | NamedKey::ArrowDown
                        | NamedKey::Home
                        | NamedKey::End
                        | NamedKey::PageUp
                        | NamedKey::PageDown
                        | NamedKey::Backspace
                        | NamedKey::Delete
                        | NamedKey::Enter
                        | NamedKey::Space,
                    ) => true,
                    Key::Character(_) => {
                        let held = ctx.modifiers.state();
                        !held.control_key() && !held.super_key() && !held.alt_key()
                    }
                    _ => false,
                }
            }
            _ => false,
        };
        if changes_selection {
            self.retained_block_selection = None;
        }

        if self.handle_text_menu_event(event, ctx) {
            return;
        }

        if self.try_paste_image_key(event, ctx) {
            return;
        }

        if self.try_clipboard_key(event, ctx) {
            return;
        }

        if self.handle_opening_code_fence(event, ctx) {
            return;
        }

        if self.handle_link_popover_key(event, ctx) {
            return;
        }

        if self.handle_slash_menu_key(event, ctx) {
            return;
        }

        if self.handle_mention_picker_key(event, ctx) {
            return;
        }

        if let Some(start) = self.visual_formula_focus
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                self.visual_formula_focus = None;
                self.dirty = true;
                *ctx.handled = true;
                return;
            }
            if !matches!(logical_key, Key::Named(NamedKey::Tab)) {
                let chrome = self.formula_chrome.get(start).and_then(|value| *value);
                let changed = self.visual_formulas.get_mut(&start).is_some_and(|visual| {
                    handle_visual_formula_key(&mut visual.editor, logical_key, *ctx.modifiers)
                });
                if changed && let Some(chrome) = chrome {
                    self.commit_visual_formula_edit(start, chrome.end, chrome.descriptor.syntax);
                }
                self.dirty = true;
                *ctx.handled = true;
                return;
            }
        }

        if self.handle_visual_table_event(event, ctx) {
            return;
        }
        if self.try_select_all_shortcut(event, ctx) {
            return;
        }

        if self.handle_code_keyboard(event, ctx) {
            return;
        }

        if let WindowEvent::Ime(winit::event::Ime::Commit(text)) = event
            && self.ime_multiline()
            && text.contains('\n')
            && text.chars().all(|c| c == '\n' || !c.is_control())
        {
            if let Some(index) = self.focused
                && let Some(block) = self.blocks.get_mut(index)
            {
                let (start, end) = block
                    .line
                    .selection_range()
                    .unwrap_or_else(|| (block.line.cursor_pos(), block.line.cursor_pos()));
                block.line.handle_event(
                    &WindowEvent::Ime(winit::event::Ime::Preedit(String::new(), None)),
                    ctx,
                );
                if self.document_selection.is_none() {
                    self.document_selection = Some(DocumentSelection {
                        anchor: DocumentPosition {
                            block: index,
                            byte: start,
                        },
                        cursor: DocumentPosition {
                            block: index,
                            byte: end,
                        },
                    });
                }
            }
            if self.replace_document_selection(text) {
                *ctx.handled = true;
                return;
            }
        }

        if self.handle_document_selection_event(event, *ctx.modifiers) {
            *ctx.handled = true;
            return;
        }

        if self.try_move_focused_block_shortcut(event, ctx) {
            return;
        }

        if self.handle_block_drag_lifecycle(event, ctx) {
            return;
        }

        if self.handle_image_event(event, ctx) {
            return;
        }

        // Reset the anchor on anything but a Ctrl+Shift+Arrow extend, so
        // the next extend latches a fresh one.
        self.maybe_reset_kbd_sel_anchor(event, *ctx.modifiers);

        // Runs BEFORE event routing so the click doesn't fall through to
        // TextInput (which would place the caret at byte 0).
        if self.handle_mouse_press_gestures(event, ctx) {
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } = event
        {
            self.cross_block_drag_start = None;
        }
        self.handle_cursor_moved(event, ctx.scale_factor);

        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == ElementState::Pressed
            && self.is_focused
        {
            if event.logical_key == Key::Named(NamedKey::Escape) {
                self.clear_document_selection();
                self.tab_navigation = true;
                *ctx.handled = true;
                return;
            }
            if !matches!(
                event.logical_key,
                Key::Named(
                    NamedKey::Tab
                        | NamedKey::Shift
                        | NamedKey::Control
                        | NamedKey::Alt
                        | NamedKey::AltGraph
                        | NamedKey::Super
                        | NamedKey::Meta
                )
            ) {
                self.tab_navigation = false;
            }
        }

        if self.try_heading_shortcut(event, ctx) {
            return;
        }

        // TextInput does NOT handle Tab, but intercept anyway so future
        // TextInput changes don't break the contract.
        if self.try_tab_indent(event, ctx) {
            return;
        }

        self.try_open_slash_menu(event);

        // Focus whichever editable block the press lands on first, so the
        // routing below's TextInput click handler sets the caret there.
        self.focus_block_under_mouse(event, ctx);

        let selection_follows = self.document_selection_follows(event, *ctx.modifiers);
        self.route_event_to_focused_block(event, ctx);
        if let Some((anchor, extend)) = selection_follows {
            self.follow_caret_from_document_selection(anchor, extend);
        }
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::Ime(_)
                | WindowEvent::MouseInput { .. }
        ) {
            self.follow_mention_caret();
        }
        self.dirty |= *ctx.handled;
    }
}
