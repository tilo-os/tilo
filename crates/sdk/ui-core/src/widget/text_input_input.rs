//! A text field's input: pointer, keyboard, IME and clipboard, and the
//! field chrome its pointer state draws.

use super::*;

impl TextInput {
    pub(super) fn draw_field_chrome(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        shape: &RoundedRect,
    ) {
        paint_field_chrome(
            scene,
            transform,
            theme,
            shape,
            FieldState {
                hovered: self.is_hovered,
                focused: self.is_focused,
                invalid: self.invalid,
                disabled: self.effectively_disabled(),
                changed_by: self.change.as_ref().map(|(identity, _)| {
                    theme
                        .tokens
                        .colors
                        .presence
                        .hue(crate::tokens::colors::PresenceHue::of_identity(identity))
                }),
            },
        );
    }

    pub(super) fn handle_cursor_moved(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
        // Hover damage updates this field, but siblings still need the same
        // motion to clear/arm their hover state. Only an owned drag consumes it.
        self.update_pointer_position(logical_pos);
        let over_accessory = self
            .accessory_rect()
            .is_some_and(|rect| rect.contains(logical_pos));
        if let Some(accessory) = self.accessory.as_mut() {
            accessory.hovered = over_accessory;
        }

        if !self.is_dragging {
            return;
        }
        *handled = true;
        let raw_position = self.resolve_position(logical_pos);
        let raw = raw_position.byte;
        let (anchor_lo, anchor_hi) = self.drag_anchor_span;
        // Selection always *contains* the original span clicked.
        let (new_anchor, new_cursor) = match self.drag_unit {
            DragUnit::Char => (anchor_lo, raw),
            DragUnit::Word => {
                let (w_start, w_end) = word_boundaries(&self.text, raw);
                extend_span_drag(anchor_lo, anchor_hi, raw, w_start, w_end)
            }
            DragUnit::Line => {
                let (line_start, line_end) = line_boundaries(&self.text, raw);
                extend_span_drag(anchor_lo, anchor_hi, raw, line_start, line_end)
            }
        };
        let affinity = if self.drag_unit == DragUnit::Char {
            raw_position.affinity
        } else {
            TextAffinity::Before
        };
        let cursor = TextPosition::with_affinity(new_cursor, affinity);
        if (self.core.anchor(), self.core.caret_position()) != (new_anchor, cursor) {
            self.core.set_selection_position(new_anchor, cursor);
            *handled = true;
        }
    }

    pub(super) fn handle_right_click(
        &mut self,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        if !self.is_focused {
            *focus_target = Some(node_id);
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "pointer coordinates are bounded on-screen positions, never near f32's range"
        )]
        let pos = [self.last_mouse_pos.x as f32, self.last_mouse_pos.y as f32];
        self.open_context_menu(pos);
        *handled = true;
    }

    fn click_place_caret(&mut self, pos: usize, affinity: TextAffinity, modifiers: Modifiers) {
        let extend = modifiers.state().shift_key();
        self.core
            .place_position(TextPosition::with_affinity(pos, affinity), extend);
        let anchor = self.core.anchor();
        self.drag_unit = DragUnit::Char;
        self.drag_anchor_span = (anchor, anchor);
        self.is_dragging = true;
    }

    /// Arms a word-granularity drag so dragging beyond it extends by whole words.
    fn click_select_word(&mut self, pos: usize) {
        self.core.select_word(&self.text, pos);
        self.drag_unit = DragUnit::Word;
        self.drag_anchor_span = (self.core.anchor(), self.core.caret());
        self.is_dragging = true;
        self.publish_primary_selection();
    }

    fn click_select_line(&mut self, pos: usize) {
        self.core.select_line(&self.text, pos);
        self.drag_unit = DragUnit::Line;
        self.drag_anchor_span = (self.core.anchor(), self.core.caret());
        self.is_dragging = true;
        self.publish_primary_selection();
    }

    pub(super) fn handle_left_press(
        &mut self,
        modifiers: Modifiers,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        if !self.is_focused {
            *focus_target = Some(node_id);
        }

        let intent = self
            .drag_select
            .press(self.last_mouse_pos, modifiers.state().shift_key());
        let position = self.resolve_position(self.last_mouse_pos);
        let pos = position.byte;
        match intent.granularity {
            Granularity::Char => self.click_place_caret(pos, position.affinity, modifiers),
            Granularity::Word => self.click_select_word(pos),
            Granularity::Line if self.multiline => self.click_select_line(pos),
            // A single-line field has exactly one line, so this is select-all.
            Granularity::Line => self.click_select_line(pos),
        }
        *handled = true;
    }

    pub(super) fn handle_left_click(
        &mut self,
        element_state: ElementState,
        modifiers: Modifiers,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        // A left press while the menu is open is routed to it, never to caret placement.
        if self.context_menu.is_open() {
            if element_state == ElementState::Pressed {
                let point = [self.last_mouse_pos.x, self.last_mouse_pos.y];
                if let Some(idx) = self.context_menu.item_at(point) {
                    self.dispatch_context_action(idx);
                } else {
                    self.context_menu.close();
                }
                *handled = true;
            }
            return;
        }
        match element_state {
            // The tap the toolkit synthesises when a finger lifts: that
            // finger's own lift, which follows, presses once
            // (`text_input_touch.rs`), and a second press would count as a
            // double click.
            ElementState::Pressed if self.active_touch.is_some() => *handled = true,
            ElementState::Pressed if self.is_hovered && self.press_accessory() => {
                *handled = true;
            }
            ElementState::Pressed if self.is_hovered => {
                self.handle_left_press(modifiers, focus_target, node_id, handled);
            }
            ElementState::Released if self.is_dragging => {
                self.end_drag();
                *handled = true;
            }
            _ => {}
        }
    }

    /// Linux primary selection is intentionally available only for ordinary
    /// text. A password must never leave its protected widget merely because
    /// the user dragged across its bullets; middle click may still paste a
    /// primary selection *into* a secret field, matching GTK password inputs.
    pub(super) fn publish_primary_selection(&self) {
        if let Some(selected) = self.shareable_selection() {
            clipboard_copy_primary(selected);
        }
    }

    /// The selection as it may leave the field — for the clipboard, the
    /// primary selection or a host's text action. Never a secret's.
    pub(super) fn shareable_selection(&self) -> Option<&str> {
        (!self.holds_secret())
            .then(|| self.selected_str())
            .flatten()
    }

    pub(super) fn handle_middle_click(
        &mut self,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        let primary = clipboard_paste_primary();
        self.handle_middle_click_with_primary(primary.as_deref(), focus_target, node_id, handled);
    }

    pub(super) fn handle_middle_click_with_primary(
        &mut self,
        primary: Option<&str>,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        if !self.is_focused {
            *focus_target = Some(node_id);
        }
        let position = self.resolve_position(self.last_mouse_pos);
        self.core.set_caret_position(position);
        self.goal_x = None;
        if let Some(paste) = primary {
            let cleaned = self.clean_pasted_text(paste);
            if !cleaned.is_empty() {
                self.insert_str(&cleaned);
            }
        }
        *handled = true;
    }

    /// One step left or right in visual order through the shaped layout,
    /// when the field has one (bidi runs move the way they read).
    fn visual_step(&self, forward: bool) -> Option<TextPosition> {
        if (self.multiline && !self.multiline_shaped) || self.masked() {
            return None;
        }
        let layout = self.single_layout.as_ref()?;
        let target =
            self.layout_to_text_position(layout.visual_motion_position(
                self.text_to_layout_position(self.caret_position()),
                forward,
            ));
        Some(TextPosition::with_affinity(
            target.byte.min(self.text.len()),
            target.affinity,
        ))
    }

    fn handle_arrow(&mut self, forward: bool, shift: bool, ctrl: bool, handled: &mut bool) {
        let motion = match (forward, ctrl) {
            (false, false) => Motion::Left,
            (true, false) => Motion::Right,
            (false, true) => Motion::WordLeft,
            (true, true) => Motion::WordRight,
        };
        match (!ctrl).then(|| self.visual_step(forward)).flatten() {
            Some(target) => self.core.motion_to(motion, target, shift),
            None => self.core.motion(&self.text, motion, shift),
        }
        self.goal_x = None;
        *handled = true;
    }

    fn handle_home(&mut self, shift: bool, ctrl: bool, handled: &mut bool) {
        let target = if ctrl {
            0
        } else if self.multiline_shaped
            && let Some(layout) = &self.single_layout
        {
            let (home, _) = layout.visual_line_edges(self.text_to_layout_byte(self.core.caret()));
            self.layout_to_text_byte(home)
        } else if self.multiline && !self.lines.is_empty() {
            let (i, _) = self.locate_ml(self.core.caret());
            self.lines.get(i).map_or(0, |r| r.start)
        } else {
            0
        };
        self.set_cursor(target, shift);
        *handled = true;
    }

    fn handle_end(&mut self, shift: bool, ctrl: bool, handled: &mut bool) {
        let target = if ctrl {
            self.text.len()
        } else if self.multiline_shaped
            && let Some(layout) = &self.single_layout
        {
            let (_, end) = layout.visual_line_edges(self.text_to_layout_byte(self.core.caret()));
            self.layout_to_text_byte(end)
        } else if self.multiline && !self.lines.is_empty() {
            let (i, _) = self.locate_ml(self.core.caret());
            self.lines.get(i).map_or(self.text.len(), |r| r.end)
        } else {
            self.text.len()
        };
        self.set_cursor(target, shift);
        *handled = true;
    }

    fn handle_enter_key(&mut self, shift: bool, handled: &mut bool) {
        let correction = (self.enter_submits || self.multiline)
            .then(|| self.autocorrect_previous_word())
            .flatten();
        if self.enter_submits && !shift {
            // What is submitted is the corrected word.
            if let Some(correction) = correction {
                self.apply_autocorrection(correction);
            }
            self.fire_submit();
            *handled = true;
        } else if self.multiline {
            self.insert_str("\n");
            if let Some(correction) = correction {
                self.apply_autocorrection(correction);
            }
            *handled = true;
        }
    }

    /// Only reached when `self.tab_spaces.is_some()` — otherwise the host keeps Tab for focus traversal.
    fn handle_tab_key(&mut self, shift: bool, handled: &mut bool) {
        let width = self.tab_spaces.unwrap_or(0);
        if shift {
            self.outdent(width);
        } else if width == 0 {
            self.insert_str("\t");
        } else {
            let spaces = " ".repeat(width);
            self.insert_str(&spaces);
        }
        *handled = true;
    }

    pub(super) fn handle_backspace_key(&mut self, ctrl: bool, handled: &mut bool) {
        if !ctrl && self.undo_last_autocorrection(true) {
            *handled = true;
            return;
        }
        self.last_autocorrection = None;
        let edit = self.core.delete_backward(&self.text, ctrl);
        self.apply_edit(&edit);
        *handled = true;
    }

    fn handle_delete_key(&mut self, ctrl: bool, handled: &mut bool) {
        let edit = self.core.delete_forward(&self.text, ctrl);
        self.apply_edit(&edit);
        *handled = true;
    }

    fn handle_ctrl_shortcut(&mut self, c: &str, handled: &mut bool) {
        let lc = c.to_lowercase();
        match lc.as_str() {
            "a" => {
                self.do_select_all();
                *handled = true;
            }
            "c" => {
                self.do_copy();
                *handled = true;
            }
            "x" => {
                self.do_cut();
                *handled = true;
            }
            "v" => {
                self.do_paste();
                *handled = true;
            }
            _ => {}
        }
    }

    pub(super) fn handle_keyboard_input(
        &mut self,
        logical_key: &Key,
        text: Option<&str>,
        element_state: ElementState,
        modifiers: Modifiers,
        handled: &mut bool,
    ) {
        if element_state != ElementState::Pressed {
            return;
        }
        if !self.is_focused {
            return;
        }
        let shift = modifiers.state().shift_key();
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        if self.read_only && !Self::read_only_key(logical_key, ctrl) {
            return;
        }

        // An open candidate list owns its keys before editing does: the
        // caret stays put while Up and Down arm a row, and Enter or Tab
        // accepts it instead of submitting or leaving the field.
        if !ctrl && !modifiers.state().alt_key() && !self.is_composing() {
            match self.completion.handle_key(logical_key) {
                CompletionKey::Handled => {
                    *handled = true;
                    return;
                }
                CompletionKey::Accept(pick) => {
                    *handled = true;
                    self.accept_completion(pick);
                    return;
                }
                CompletionKey::Pass => {}
            }
        }

        match logical_key {
            Key::Named(NamedKey::ArrowLeft) => self.handle_arrow(false, shift, ctrl, handled),
            Key::Named(NamedKey::ArrowRight) => self.handle_arrow(true, shift, ctrl, handled),
            Key::Named(NamedKey::ArrowUp) if self.multiline => {
                self.move_vertical(false, shift);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) if self.multiline => {
                self.move_vertical(true, shift);
                *handled = true;
            }
            Key::Named(NamedKey::Home) => self.handle_home(shift, ctrl, handled),
            Key::Named(NamedKey::End) => self.handle_end(shift, ctrl, handled),
            Key::Named(NamedKey::Enter) => self.handle_enter_key(shift, handled),
            Key::Named(NamedKey::Tab) if self.tab_spaces.is_some() => {
                self.handle_tab_key(shift, handled);
            }
            Key::Named(NamedKey::Backspace) => {
                self.handle_backspace_key(ctrl, handled);
            }
            Key::Named(NamedKey::Delete) => self.handle_delete_key(ctrl, handled),
            Key::Named(NamedKey::Space) => {
                self.insert_word_boundary(" ");
                *handled = true;
            }
            Key::Character(c) if ctrl => {
                self.handle_ctrl_shortcut(c.as_str(), handled);
            }
            _ => {
                if let Some(t) = text
                    && !t.chars().all(char::is_control)
                {
                    if t.chars().next().is_some_and(is_autocorrect_boundary) {
                        self.insert_word_boundary(t);
                    } else {
                        self.insert_str(t);
                    }
                    *handled = true;
                }
            }
        }
    }

    /// The keys a read-only field takes: moving and selecting, select all and
    /// copy. The rest are the app's.
    fn read_only_key(key: &Key, ctrl: bool) -> bool {
        match key {
            Key::Named(
                NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::Home
                | NamedKey::End,
            ) => true,
            Key::Character(c) if ctrl => matches!(c.to_lowercase().as_str(), "a" | "c"),
            _ => false,
        }
    }

    pub(super) fn handle_ime_commit(&mut self, t: &str, handled: &mut bool) {
        if self.read_only {
            return;
        }
        if !self.is_focused {
            return;
        }
        let trailing_boundary = t
            .char_indices()
            .next_back()
            .filter(|&(_, boundary)| is_autocorrect_boundary(boundary));
        if self.multiline && t.contains('\n') && t.chars().all(|c| c == '\n' || !c.is_control()) {
            let edit = self.core.commit(t);
            self.apply_insert(&edit);
            *handled = true;
        } else if t.chars().all(char::is_control) {
            self.core.clear_preedit();
        } else if let Some((boundary_at, boundary)) = trailing_boundary {
            // A committed word plus its boundary autocorrects like typing.
            self.core.clear_preedit();
            if let Some(prefix) = t.get(..boundary_at)
                && !prefix.is_empty()
            {
                self.insert_str(prefix);
            }
            let mut encoded = [0; 4];
            self.insert_word_boundary(boundary.encode_utf8(&mut encoded));
            *handled = true;
        } else {
            let edit = self.core.commit(t);
            self.apply_insert(&edit);
            *handled = true;
        }
    }

    pub(super) fn handle_ime_preedit(
        &mut self,
        t: &str,
        cursor: Option<(usize, usize)>,
        handled: &mut bool,
    ) {
        if self.read_only {
            return;
        }
        if !self.is_focused {
            return;
        }
        self.core.set_preedit(t, cursor);
        *handled = true;
    }
}
