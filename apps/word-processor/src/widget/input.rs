//! Pointer, keyboard and drop input: routing a window event to the body, the
//! panels, the ruler or the toolbar, and the editing keys and shortcuts.

use super::image_layout::{anchor as image_anchor, free_position as image_free_position};
use crate::effects::WordProcessorEffect;
use crate::num;
use crate::text_offsets::{byte_to_char, char_to_byte};
use crate::word_processor::Caret;
use crate::zoom_control::{MAX as ZOOM_MAX, MIN as ZOOM_MIN};
use automerge::AutoCommit;
use automerge::transaction::Transactable;
use tilo_doc::model::Region;
use ui::gesture::Granularity;
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::{
    DocumentAutocorrection, DragKind, Editor, EditorEventCtx, GuideDrag, KeyPressEffect,
    LayoutMode, ObjectDrag,
};

impl Editor {
    // A dropped file carries no coordinates: seat the caret under the last pointer, so the
    // image lands where it was released.
    fn handle_dropped_file(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        path: &std::path::Path,
        handled: &mut bool,
    ) {
        if self.rect.contains(self.pointer) {
            let over_body = self.pointer.y >= self.rect.y0
                && self.pointer.y <= self.rect.y1 - self.layout_bar_height
                && !(self.view.styles_panel_visible && self.panel_rect.contains(self.pointer))
                && !self.outline.contains(self.pointer);
            if over_body {
                self.caret_from_pointer(self.pointer_to_content(self.pointer), false);
            }
        }
        if self.insert_image_from_path(state, path) {
            *handled = true;
        }
    }

    pub(super) fn handle_mouse_wheel(
        &mut self,
        delta: &MouseScrollDelta,
        scale_factor: f64,
        ctrl: bool,
        handled: &mut bool,
    ) {
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 48.0,
            MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
        };
        if self.styles_panel_visible() && self.panel_body.contains(self.pointer) {
            let next = (self.panel_scroll - dy).clamp(0.0, self.panel_max_scroll);
            if next.to_bits() != self.panel_scroll.to_bits() {
                self.panel_scroll = next;
                self.lifecycle.dirty = true;
            }
            *handled = true;
            return;
        }
        if ctrl {
            // Ctrl + wheel zooms a step per notch; a hosted window offers the same gesture first.
            let step = if dy > 0.0 {
                ui::content_zoom::ZoomStep::In
            } else {
                ui::content_zoom::ZoomStep::Out
            };
            self.zoom_page(step);
            *handled = true;
            return;
        }
        let new = (self.scroll_y - dy).clamp(self.scroll_floor(), self.max_scroll);
        // Any scroll delta, however small, marks the frame dirty: compare bits, not a tolerance.
        if new.to_bits() != self.scroll_y.to_bits() {
            self.scroll_y = new;
            self.lifecycle.dirty = true;
            *handled = true;
        }
    }

    pub(super) fn handle_cursor_moved(
        &mut self,
        x: f64,
        y: f64,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        self.pointer = Point::new(x / scale_factor, y / scale_factor);
        let hover_changed = self.outline.pointer_moved(self.pointer)
            | self.review.pointer_moved(self.pointer)
            | self.panel_rect.contains(self.pointer)
            | self.thread_panel.contains(self.pointer)
            | self
                .layout_chips
                .iter()
                .any(|(_, r)| r.contains(self.pointer));
        if hover_changed {
            self.lifecycle.dirty = true;
            *handled = true;
        }
        if false {
        } else if self.object_drag.is_some() {
            let cp = self.pointer_to_content(self.pointer);
            let Some(drag) = self.object_drag.as_mut() else {
                return;
            };
            let dx = cp.x - drag.start_pointer.x;
            let dy = cp.y - drag.start_pointer.y;
            if dx.abs() > 1.0 || dy.abs() > 1.0 {
                drag.moved = true;
            }
            drag.x = (drag.start_x + dx).clamp(0.0, 100_000.0);
            drag.y = (drag.start_y + dy).clamp(0.0, 100_000.0);
            self.lifecycle.dirty = true;
            *handled = true;
        } else if self.ruler.is_dragging() {
            self.ruler_drag_move();
            self.lifecycle.dirty = true;
            *handled = true;
        } else if let Some(gd) = self.guide_drag {
            // Guides live in unscaled page space: map the pointer through the inverse zoom first.
            self.apply_guide_drag(gd, self.pointer_to_content(self.pointer));
            self.lifecycle.dirty = true; // repaint every move so the preview is live
            *handled = true;
        } else if let Some(granularity) = self.drag.drag() {
            self.caret_from_pointer(self.pointer_to_content(self.pointer), true);
            if granularity != Granularity::Char {
                self.extend_by_units(granularity);
            }
            *handled = true;
        }
    }

    fn extend_by_units(&mut self, granularity: Granularity) {
        let Some((start, end)) = self.drag_unit.clone() else {
            return;
        };
        let raw = self.inner.caret.clone();
        let order = |block: &str| self.laid.iter().position(|laid| laid.id == block);
        let (Some(start_block), Some(raw_block)) = (order(&start.block), order(&raw.block)) else {
            return;
        };
        let Some(end_block) = order(&end.block) else {
            return;
        };
        let Some(text) = self.laid.get(raw_block).map(|laid| laid.text.as_str()) else {
            return;
        };
        let (unit_start, unit_end) = match granularity {
            Granularity::Word => ui::text_selection::word_boundaries_chars(text, raw.offset),
            _ => (0, text.chars().count()),
        };
        let (anchor, cursor) = ui::text_selection::extend_span_drag(
            (start_block, start.offset),
            (end_block, end.offset),
            (raw_block, raw.offset),
            (raw_block, unit_start),
            (raw_block, unit_end),
        );
        let anchor = if anchor == (start_block, start.offset) {
            start
        } else {
            end
        };
        self.inner.selection_anchor = Some(anchor);
        self.inner.caret = Caret {
            block: raw.block,
            offset: cursor.1,
        };
    }

    fn handle_right_press(&mut self, state: &AutoCommit, handled: &mut bool) {
        if !self.rect.contains(self.pointer) {
            return;
        }
        self.lifecycle.focused = true;
        if self.view.styles_panel_visible && self.panel_rect.contains(self.pointer) {
            if let Some(id) = self.style_chip_at(self.pointer) {
                self.style_menu.open(&id, self.pointer);
                self.style_menu.menu_mut().set_viewport(Some(self.rect));
                self.lifecycle.dirty = true;
                *handled = true;
            }
            return;
        }
        let in_chrome = self.pointer.y > self.rect.y1 - self.layout_bar_height
            || (self.view.styles_panel_visible && self.panel_rect.contains(self.pointer))
            || self.outline.contains(self.pointer);
        if !in_chrome {
            let cp = self.pointer_to_content(self.pointer);
            let pos = [
                num::f32_of_f64(self.pointer.x),
                num::f32_of_f64(self.pointer.y),
            ];
            self.open_context_menu(state, cp, pos);
            *handled = true;
        }
    }

    fn handle_left_press(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        shift: bool,
        ctrl: bool,
        handled: &mut bool,
    ) {
        if self.handle_left_press_menu_routing(state, handled) {
            return;
        }
        if self.toast.dismiss() {
            self.lifecycle.dirty = true;
        }
        if !self.rect.contains(self.pointer) {
            return;
        }
        self.lifecycle.focused = true;
        if self.handle_left_press_panels(state, handled) {
            return;
        }
        if self.handle_left_press_find_ruler_bars(state, handled) {
            return;
        }
        self.handle_left_press_body(state, shift, ctrl, handled);
    }

    pub(super) fn handle_left_press_menu_routing(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        if self.context_menu.is_open() {
            let point = [self.pointer.x, self.pointer.y];
            if let Some(idx) = self.context_menu.item_at(point) {
                self.dispatch_context_action(idx, state);
            } else {
                self.context_menu.close();
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        if self.file_menu.is_open() {
            if let Some(choice) = self.file_menu.click(self.pointer) {
                self.apply_file_choice(state, choice);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        if self.style_menu.is_open() {
            if let Some(action) = self.style_menu.click(self.pointer) {
                self.dispatch_style_menu(action, state);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        false
    }

    fn handle_left_press_panels(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        if self
            .save_error
            .is_some_and(|close| close.contains(self.pointer))
        {
            self.save_error = None;
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        if !self.styles_panel_full_window() && self.inspector_drawer.press_dismisses(self.pointer) {
            self.view.styles_panel_visible = false;
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        if !self.styles_panel_full_window() && self.outline_drawer.press_dismisses(self.pointer) {
            if self.outline.is_open() {
                self.outline.toggle();
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        if self.view.styles_panel_visible && self.panel_rect.contains(self.pointer) {
            if let Some(hit) = self
                .panel_hits
                .iter()
                .rev()
                .find(|(r, _)| r.contains(self.pointer))
                .map(|(_, h)| h.clone())
            {
                self.apply_panel_hit(state, hit);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        if self.outline.contains(self.pointer) {
            if let Some(block_id) = self.outline.heading_at(self.pointer).map(str::to_owned) {
                self.scroll_to_block(&block_id);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        false
    }

    fn handle_left_press_find_ruler_bars(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        if self.view.ruler_visible && self.ruler.rect().contains(self.pointer) {
            self.ruler_press(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        let lbar_top = self.rect.y1 - self.layout_bar_height;
        if self.pointer.y >= lbar_top {
            if let Some(mode) = self
                .layout_chips
                .iter()
                .find(|(_, r)| r.contains(self.pointer))
                .map(|(m, _)| *m)
            {
                self.toggle_layout_mode(mode);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        false
    }

    fn handle_left_press_body(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        shift: bool,
        ctrl: bool,
        handled: &mut bool,
    ) {
        // Page-content hit-tests work in unscaled content space: map the pointer once here.
        let cp = self.pointer_to_content(self.pointer);
        if let Some(action) = self.review.action_at(self.pointer) {
            self.dispatch_review_action(action, state);
            self.lifecycle.dirty = true;
            *handled = true;
            return;
        }
        if self.layout_mode != LayoutMode::None
            && let Some(side) = self.hit_guide(cp)
        {
            let kind = if ctrl {
                DragKind::Proportional
            } else if shift {
                DragKind::Single
            } else {
                DragKind::Pair
            };
            self.guide_drag = Some(GuideDrag {
                mode: self.layout_mode,
                side,
                kind,
            });
            self.lifecycle.dirty = true;
            *handled = true;
            return;
        }
        // The rectangles and the pointer are both in content space, so the drag never drifts.
        let free_image = self
            .laid
            .iter()
            .rev()
            .find(|laid| {
                laid.kind == "image"
                    && Rect::new(
                        laid.text_x,
                        laid.y_top,
                        laid.text_x + laid.width,
                        laid.y_top + laid.height,
                    )
                    .contains(cp)
            })
            .and_then(|image| {
                self.blocks
                    .iter()
                    .find(|block| block.id == image.id)
                    .filter(|block| image_anchor(block) == "page")
                    .and_then(image_free_position)
                    .map(|(x, y, page)| (image.id.clone(), x, y, page))
            });
        if let Some((id, x, y, page)) = free_image {
            self.inner.clear_anchor();
            self.inner.set_caret(Caret {
                block: id.clone(),
                offset: 0,
            });
            self.object_drag = Some(ObjectDrag {
                id,
                start_pointer: cp,
                start_x: x,
                start_y: y,
                page,
                x,
                y,
                moved: false,
            });
            self.lifecycle.dirty = true;
            *handled = true;
            return;
        }
        let band = self.band_at_pointer(cp);
        match band {
            Some(region) if region != self.inner.region() => {
                self.inner.focus_region(state, region);
                self.refresh_blocks(state);
                self.drag.press(self.pointer, shift);
                self.lifecycle.dirty = true;
                *handled = true;
                return;
            }
            None if self.inner.in_running_region() => {
                self.inner.focus_body(state);
                self.refresh_blocks(state);
            }
            _ => {}
        }
        let intent = self.drag.press(self.pointer, shift);
        self.caret_from_pointer(cp, intent.extend);
        match intent.granularity {
            Granularity::Word => self.inner.select_word(state),
            Granularity::Line => self.select_all_in_block(state),
            Granularity::Char => {}
        }
        self.drag_unit = (intent.granularity != Granularity::Char)
            .then(|| self.inner.selection_anchor.clone())
            .flatten()
            .filter(|anchor| *anchor != self.inner.caret)
            .map(|anchor| (anchor, self.inner.caret.clone()));
        self.lifecycle.dirty = true;
        *handled = true;
    }

    fn handle_left_release(&mut self, state: &mut ui::widget::Edits<'_>, handled: &mut bool) {
        if false {
        } else if let Some(drag) = self.object_drag.take() {
            if drag.moved {
                self.perform_action(
                    state,
                    &crate::actions::set_image_placement_action(
                        &drag.id,
                        "page",
                        "none",
                        num::i64_of_f64(drag.x.round()),
                        num::i64_of_f64(drag.y.round()),
                        num::i64_of_usize(drag.page),
                    ),
                );
                self.refresh_blocks(state);
            }
            self.lifecycle.dirty = true;
            *handled = true;
        } else if self.ruler.is_dragging() {
            self.ruler_drag_release(state);
            self.lifecycle.dirty = true;
            *handled = true;
        } else if let Some(gd) = self.guide_drag.take() {
            match gd.mode {
                LayoutMode::Spacing => {
                    self.persist_stylesheet_via_actions(state);
                }
                LayoutMode::Margins => {
                    self.persist_margins(state);
                }
                LayoutMode::None => {}
            }
            self.lifecycle.dirty = true;
            *handled = true;
        } else if self.drag.release() {
            self.lifecycle.dirty = true;
            *handled = true;
        }
    }

    pub(super) fn autocorrect_before_boundary(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
    ) -> Option<DocumentAutocorrection> {
        if !ui::text_assistance::preferences().auto_correct || self.inner.has_selection() {
            return None;
        }
        let text = self.inner.current_text(state);
        let chars: Vec<char> = text.chars().collect();
        let end = self.inner.caret.offset.min(chars.len());
        let mut start = end;
        while start > 0
            && chars
                .get(start.saturating_sub(1))
                .is_some_and(|character| character.is_alphabetic() || *character == '\'')
        {
            start = start.saturating_sub(1);
        }
        if start == end {
            return None;
        }
        let original: String = chars.iter().skip(start).take(end - start).collect();
        let dict = self.dictionary();
        if dict.contains(&original) {
            return None;
        }
        let replacement = spellcheck::probable_correction(&self.proofing.language, &original)?;
        let block = self.inner.caret.block.clone();
        let region = self.inner.region_kind();
        self.splice_via_action(state, region, &block, start, end, &replacement);
        let replacement_end = start.saturating_add(replacement.chars().count());
        self.inner.caret.offset = replacement_end;
        Some(DocumentAutocorrection {
            block,
            start,
            end: replacement_end,
            boundary_end: replacement_end,
            original,
            replacement,
        })
    }

    /// The two field-granular envelopes share the input event's undo boundary.
    fn autoformat_on_space_via_actions(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        let block = self.inner.caret.block.clone();
        let offset = self.inner.caret.offset;
        let Some(view) = self.blocks.iter().find(|entry| entry.id == block) else {
            return false;
        };
        if view.kind != "paragraph" {
            return false;
        }
        let prefix: String = view.text.chars().take(offset).collect();
        let (kind, level, ordered) = match prefix.as_str() {
            "-" | "*" | "+" => ("list_item", None, Some(false)),
            "1." | "1)" => ("list_item", None, Some(true)),
            "#" => ("heading", Some(1), None),
            "##" => ("heading", Some(2), None),
            "###" => ("heading", Some(3), None),
            _ => return false,
        };
        let region = self.inner.region_kind();
        self.splice_via_action(state, region, &block, 0, offset, "");
        self.perform_action(
            state,
            &crate::actions::set_paragraph_style_action(
                region,
                vec![block.clone()],
                kind,
                level,
                ordered,
            ),
        );
        self.inner.set_caret(Caret { block, offset: 0 });
        true
    }

    pub(super) fn undo_document_autocorrection(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let Some(mut correction) = self.last_autocorrection.clone() else {
            return false;
        };
        if self.inner.caret.block != correction.block && self.inner.caret.offset == 0 {
            self.delete_via_action(state, true);
            correction.boundary_end = self.inner.caret.offset;
        }
        if self.inner.caret.block != correction.block
            || self.inner.caret.offset != correction.boundary_end
        {
            self.last_autocorrection = None;
            return false;
        }
        let text = self.inner.current_text(state);
        let corrected: String = text
            .chars()
            .skip(correction.start)
            .take(correction.end.saturating_sub(correction.start))
            .collect();
        if corrected != correction.replacement {
            self.last_autocorrection = None;
            return false;
        }
        let region = self.inner.region_kind();
        self.splice_via_action(
            state,
            region,
            &correction.block,
            correction.start,
            correction.boundary_end,
            &correction.original,
        );
        self.inner.caret.offset = correction
            .start
            .saturating_add(correction.original.chars().count());
        self.last_autocorrection = None;
        true
    }

    pub(super) fn handle_key_press(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        logical_key: &Key,
        text: Option<&str>,
        shift: bool,
        ctrl: bool,
        handled: &mut bool,
    ) {
        if self.handle_key_press_guards(state, logical_key, ctrl, handled) {
            return;
        }
        if ctrl && let Key::Character(s) = logical_key {
            self.handle_ctrl_shortcut(state, s, shift, handled);
            return;
        }
        if !matches!(logical_key, Key::Named(NamedKey::Backspace)) {
            self.last_autocorrection = None;
        }
        let cached_revision_current = self.materialized.heads == state.base_heads()
            && self.materialized.pending_ops == state.pending_ops();
        let text_block = (cached_revision_current
            && !self.inner.has_selection()
            && matches!(self.inner.region(), Region::Body)
            && (text.is_some()
                || (matches!(logical_key, Key::Named(NamedKey::Backspace))
                    && self.inner.caret().offset > 0)))
            .then(|| self.inner.caret().block.clone());
        let effect = match logical_key {
            Key::Named(named) => self
                .handle_named_edit_key(state, *named, shift)
                .unwrap_or_else(|| {
                    if self.handle_navigation_key(state, *named, shift) {
                        KeyPressEffect::Motion
                    } else {
                        self.insert_key_text(state, text)
                    }
                }),
            _ => self.insert_key_text(state, text),
        };
        if effect != KeyPressEffect::Ignored {
            if effect == KeyPressEffect::Edit {
                self.inner.clear_anchor();
            }
            if effect == KeyPressEffect::Edit {
                self.refresh_blocks_after_text_edit(state, text_block.as_deref());
            } else {
                self.refresh_blocks(state);
            }
            self.lifecycle.dirty = true;
            *handled = true;
        }
    }

    fn handle_named_edit_key(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        key: NamedKey,
        shift: bool,
    ) -> Option<KeyPressEffect> {
        match key {
            NamedKey::Tab => Some(self.handle_tab_key(state, shift)),
            NamedKey::Enter => {
                let correction = self.autocorrect_before_boundary(state);
                self.enter_via_action(state);
                self.last_autocorrection = correction;
                Some(KeyPressEffect::Edit)
            }
            NamedKey::Backspace => {
                if !self.undo_document_autocorrection(state) {
                    self.delete_via_action(state, true);
                }
                Some(KeyPressEffect::Edit)
            }
            NamedKey::Delete => {
                self.delete_via_action(state, false);
                Some(KeyPressEffect::Edit)
            }
            NamedKey::Space => {
                let correction = self.autocorrect_before_boundary(state);
                if !self.autoformat_on_space_via_actions(state) {
                    self.insert_text_via_action(state, " ");
                }
                self.last_autocorrection = correction.map(|mut correction| {
                    correction.boundary_end = self.inner.caret.offset;
                    correction
                });
                Some(KeyPressEffect::Edit)
            }
            _ => None,
        }
    }

    fn handle_tab_key(&mut self, state: &mut ui::widget::Edits<'_>, shift: bool) -> KeyPressEffect {
        let table_target = self.caret_table_target();
        let append_row = !shift
            && table_target
                .as_ref()
                .is_some_and(|(_, row, column, rows, columns)| {
                    row.saturating_add(1) == *rows && column.saturating_add(1) == *columns
                });
        if append_row {
            if let Some((_, _, _, rows, _)) = table_target {
                self.edit_table_via_action(
                    state,
                    crate::actions::TableEdit::InsertRow { at: rows },
                );
            }
            KeyPressEffect::Motion
        } else {
            self.sync_source(state.doc());
            let body = self.kept_body();
            if self
                .inner
                .with_kept_body(body, |inner| inner.tab_cell(state, !shift))
            {
                return KeyPressEffect::Motion;
            }
            let text = if self.inner.caret_tab_stops(state).is_empty() {
                "    "
            } else {
                "\t"
            };
            self.insert_text_via_action(state, text);
            KeyPressEffect::Edit
        }
    }

    fn handle_navigation_key(&mut self, state: &AutoCommit, key: NamedKey, shift: bool) -> bool {
        match key {
            NamedKey::ArrowLeft => {
                self.extend_or_clear(shift);
                self.with_kept_body(state, |inner| inner.move_left(state));
            }
            NamedKey::ArrowRight => {
                self.extend_or_clear(shift);
                self.with_kept_body(state, |inner| inner.move_right(state));
            }
            NamedKey::ArrowUp | NamedKey::ArrowDown => {
                self.extend_or_clear(shift);
                let delta = if matches!(key, NamedKey::ArrowDown) {
                    1
                } else {
                    -1
                };
                self.with_kept_body(state, |inner| {
                    if !inner.cell_vertical(state, delta > 0) {
                        inner.move_block_relative(state, delta);
                    }
                });
            }
            NamedKey::Home => {
                self.extend_or_clear(shift);
                self.inner.move_home();
            }
            NamedKey::End => {
                self.extend_or_clear(shift);
                self.inner.move_end(state);
            }
            NamedKey::PageUp | NamedKey::PageDown => {
                self.extend_or_clear(shift);
                self.move_page(matches!(key, NamedKey::PageDown), shift);
            }
            _ => return false,
        }
        true
    }

    fn insert_key_text(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        text: Option<&str>,
    ) -> KeyPressEffect {
        let typed: String = text
            .map(|text| {
                text.chars()
                    .filter(|character| !character.is_control())
                    .collect()
            })
            .unwrap_or_default();
        if typed.is_empty() {
            return KeyPressEffect::Ignored;
        }
        let correction = typed
            .chars()
            .next()
            .filter(|character| character.is_ascii_punctuation() && *character != '\'')
            .and_then(|_| self.autocorrect_before_boundary(state));
        self.insert_text_via_action(state, &typed);
        self.last_autocorrection = correction.map(|mut correction| {
            correction.boundary_end = self.inner.caret.offset;
            correction
        });
        KeyPressEffect::Edit
    }

    fn handle_modified_navigation(
        &mut self,
        state: &AutoCommit,
        key: &Key,
        modifiers: Modifiers,
    ) -> bool {
        if !self.lifecycle.focused || self.editing_field.is_some() {
            return false;
        }
        let Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight) = key else {
            return false;
        };
        let mods = modifiers.state();
        let line = mods.super_key();
        let word = mods.alt_key() || mods.control_key();
        if !line && !word {
            return false;
        }
        let left = matches!(key, Key::Named(NamedKey::ArrowLeft));
        let text = self.inner.current_text(state);
        let offset = self.inner.caret.offset;
        let byte = char_to_byte(&text, offset);
        let destination = if line {
            let layout = if self.inner.region() == Region::Body {
                self.layout_cache
                    .get(&self.inner.caret.block)
                    .filter(|_| {
                        self.laid
                            .iter()
                            .any(|block| block.id == self.inner.caret.block && block.text == text)
                    })
                    .map(|(_, layout)| layout)
            } else {
                self.region_laid
                    .iter()
                    .find(|block| block.id == self.inner.caret.block && block.text == text)
                    .map(|block| &block.layout)
            };
            let edges = layout.map_or((0, text.len()), |layout| layout.visual_line_edges(byte));
            byte_to_char(&text, if left { edges.0 } else { edges.1 })
        } else if left {
            ui::text_selection::prev_word_boundary_chars(&text, offset)
        } else if mods.alt_key() {
            // Option+Right follows macOS word-end; Ctrl+Right keeps the shared next-word start.
            let (_, end) = ui::text_selection::word_boundaries(&text, byte);
            let end = if end > byte {
                end
            } else {
                let next = ui::text_selection::next_word_boundary(&text, byte);
                ui::text_selection::word_boundaries(&text, next).1.max(next)
            };
            byte_to_char(&text, end)
        } else {
            ui::text_selection::next_word_boundary_chars(&text, offset)
        };
        self.extend_or_clear(mods.shift_key());
        if !line && destination == offset {
            self.with_kept_body(state, |inner| {
                if left {
                    inner.move_left(state);
                } else {
                    inner.move_right(state);
                }
            });
        } else {
            self.inner.caret.offset = destination;
        }
        self.lifecycle.dirty = true;
        true
    }

    fn handle_key_press_guards(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        logical_key: &Key,
        ctrl: bool,
        handled: &mut bool,
    ) -> bool {
        if !self.lifecycle.focused {
            return true;
        }
        // A free-object drag is a preview until release: Escape discards it.
        if matches!(logical_key, Key::Named(NamedKey::Escape)) && self.object_drag.is_some() {
            self.object_drag = None;
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        if matches!(logical_key, Key::Named(NamedKey::Escape))
            && (self.context_menu.is_open()
                || self.style_menu.is_open()
                || self.file_menu.is_open())
        {
            self.context_menu.close();
            self.style_menu.close();
            self.file_menu.close();
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        if self.inner.in_running_region() && matches!(logical_key, Key::Named(NamedKey::Escape)) {
            self.inner.focus_body(state);
            self.refresh_blocks(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        if self.editing_field.is_some() {
            *handled = true;
            return true;
        }
        // Ctrl+Enter splits the caret block and marks the new one to start a page.
        if ctrl && matches!(logical_key, Key::Named(NamedKey::Enter)) {
            self.page_break_via_action(state);
            self.refresh_blocks(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        false
    }

    /// A Ctrl chord is consumed even when no shortcut matches, so it never falls through to typing.
    fn handle_ctrl_shortcut(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        s: &str,
        shift: bool,
        handled: &mut bool,
    ) {
        // Save is not an editing action: do not mark the document modified after writing it.
        if s.eq_ignore_ascii_case("s") {
            self.invoke_effect(&if shift {
                WordProcessorEffect::SaveAs { path: None }
            } else {
                WordProcessorEffect::Save { path: None }
            });
            *handled = true;
            return;
        }
        if s.eq_ignore_ascii_case("o") {
            self.invoke_effect(&WordProcessorEffect::OpenDocument { path: None });
            *handled = true;
            return;
        }
        if shift && s.eq_ignore_ascii_case("l") {
            self.toggle_spellcheck();
            *handled = true;
            return;
        }
        if shift && s.eq_ignore_ascii_case("m") {
            self.toggle_markup();
            *handled = true;
            return;
        }
        if shift && s.eq_ignore_ascii_case("p") {
            self.insert_page_number_via_action(state);
            self.refresh_blocks(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return;
        }
        match s {
            "=" | "+" => {
                self.zoom_page(ui::content_zoom::ZoomStep::In);
                *handled = true;
                return;
            }
            "-" | "_" => {
                self.zoom_page(ui::content_zoom::ZoomStep::Out);
                *handled = true;
                return;
            }
            "0" => {
                self.zoom_page(ui::content_zoom::ZoomStep::Reset);
                *handled = true;
                return;
            }
            _ => {}
        }
        match s.to_lowercase().as_str() {
            // The same `RichTextAction` the toolbar and the palette dispatch.
            "b" => self.dispatch_toggle_mark(state, "strong"),
            "i" => self.dispatch_toggle_mark(state, "em"),
            "u" => self.dispatch_toggle_mark(state, "underline"),
            "." | ">" => self.dispatch_exclusive_mark(state, "super", "sub"),
            "," | "<" => self.dispatch_exclusive_mark(state, "sub", "super"),
            "c" => {
                self.clipboard_copy(state);
            }
            "x" => {
                if self.clipboard_copy(state) {
                    self.insert_text_via_action(state, "");
                }
            }
            "v" if shift => self.clipboard_paste_plain(state),
            "v" => self.clipboard_paste(state),
            "a" => self.select_all_via_primitive(state),
            "1" => self.dispatch_paragraph_style(state, "heading", Some(1), None),
            "2" => self.dispatch_paragraph_style(state, "heading", Some(2), None),
            "3" => self.dispatch_paragraph_style(state, "heading", Some(3), None),
            // z and y (undo and redo) are the host app's.
            _ => {}
        }
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
        *handled = true;
    }
}
impl Editor {
    #[expect(
        clippy::too_many_lines,
        reason = "the widget's single event entry point routes each window event to overlays, panels and the editor in priority order"
    )]
    pub(super) fn route_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.styles_panel_full_window() {
            self.context_menu.close();
            if self.thread_comment.is_some() {
                self.close_comment_thread();
            }
        }
        if let WindowEvent::Touch(touch) = event
            && self.handle_segmented_touch(touch, ctx.scale_factor, &mut ctx.state)
        {
            *ctx.handled = true;
            return;
        }
        let shift = ctx.modifiers.state().shift_key();
        let ctrl = ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();
        if self.file_menu.is_open() || self.context_menu.is_open() || self.style_menu.is_open() {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.pointer = ctx.logical(position);
            }
            let menu = if self.file_menu.is_open() {
                self.file_menu.menu_mut()
            } else if self.style_menu.is_open() {
                self.style_menu.menu_mut()
            } else {
                &mut self.context_menu
            };
            menu.handle_event(event, ctx);
            if let Some(choice) = self.file_menu.take_selection() {
                self.apply_file_choice(&mut ctx.state, choice);
            }
            if let Some(action) = self.style_menu.take_selection() {
                self.dispatch_style_menu(action, &mut ctx.state);
            }
            if let Some(index) = self.context_menu.take_selection() {
                self.dispatch_context_action(index, &mut ctx.state);
            }
            if matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::PinchGesture { .. }
                    | WindowEvent::KeyboardInput { .. }
                    | WindowEvent::Ime(_)
            ) {
                self.lifecycle.dirty = true;
                *ctx.handled = true;
                return;
            }
        }
        if self.pump_compact_swatch_palette(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            &mut ctx.state,
            ctx.path,
            ctx.handled,
        ) {
            return;
        }
        if self.color_picker.is_open()
            && self.pump_color_picker(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                &mut ctx.state,
                ctx.path,
                ctx.handled,
            )
        {
            return;
        }
        let comment_thread_handled = self.handle_comment_thread_event(
            event,
            &mut EditorEventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: *ctx.modifiers,
                state: ctx.state.reborrow(),
                path: ctx.path,
                handled: ctx.handled,
            },
        );
        if comment_thread_handled {
            return;
        }
        if self.route_hosted_fields(event, ctx) {
            return;
        }
        if self.route_styles_page_key(event, ctx) {
            return;
        }

        match event {
            WindowEvent::DroppedFile(path) => {
                self.handle_dropped_file(&mut ctx.state, path, ctx.handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // An unfocused Writer leaves Ctrl+wheel to the app the host selected.
                if self.lifecycle.focused || !ctrl {
                    self.handle_mouse_wheel(delta, ctx.scale_factor, ctrl, ctx.handled);
                }
            }
            WindowEvent::PinchGesture { delta, phase, .. } => {
                if self.lifecycle.focused && self.zoom_viewport.contains(self.pointer) {
                    *ctx.handled = true;
                    if delta.is_finite()
                        && *delta != 0.0
                        && *phase != winit::event::TouchPhase::Cancelled
                        && !self.color_picker.is_open()
                        && !self.compact_swatch_palette.is_open()
                        && !(self.styles_panel_overlay() && self.panel_rect.contains(self.pointer))
                    {
                        let before = self.zoom.factor();
                        let after = (before * (1.0 + delta).max(0.01)).clamp(ZOOM_MIN, ZOOM_MAX);
                        if self.set_zoom(after) {
                            let pointer_y = self.pointer.y - self.zoom.anchor().y;
                            self.scroll_y = (self.scroll_y + pointer_y / before
                                - pointer_y / after)
                                .max(self.scroll_floor());
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x, position.y, ctx.scale_factor, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let was_handled = *ctx.handled;
                self.handle_right_press(&ctx.state, ctx.handled);
                if !was_handled && self.lifecycle.focused && self.rect.contains(self.pointer) {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let was_handled = *ctx.handled;
                self.handle_left_press(&mut ctx.state, shift, ctrl, ctx.handled);
                if !was_handled && self.lifecycle.focused && self.rect.contains(self.pointer) {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.handle_left_release(&mut ctx.state, ctx.handled),
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                self.inner.clear_preedit();
                self.handle_key_press(
                    &mut ctx.state,
                    &Key::Character(text.as_str().into()),
                    Some(text),
                    false,
                    false,
                    ctx.handled,
                );
            }
            WindowEvent::Ime(winit::event::Ime::Preedit(text, cursor)) => {
                if text.is_empty() {
                    self.inner.clear_preedit();
                } else {
                    self.inner.set_preedit(text, *cursor);
                }
                self.lifecycle.dirty = true;
                *ctx.handled = true;
            }
            WindowEvent::Ime(winit::event::Ime::Disabled) => {
                self.inner.clear_preedit();
                self.lifecycle.dirty = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        text,
                        ..
                    },
                ..
            } => {
                if self.handle_modified_navigation(&ctx.state, logical_key, *ctx.modifiers) {
                    *ctx.handled = true;
                } else {
                    self.handle_key_press(
                        &mut ctx.state,
                        logical_key,
                        text.as_deref(),
                        shift,
                        ctrl,
                        ctx.handled,
                    );
                }
            }
            _ => {}
        }
    }
}

/// A path the toolkit spooled a drag-and-drop payload into: only files it made are deleted
/// after an insert, never one the user chose.
pub(super) fn is_dnd_spool_path(path: &std::path::Path) -> bool {
    let named_by_the_toolkit = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("winit-dnd-"));
    if !named_by_the_toolkit {
        return false;
    }
    // The toolkit spools into the first of these it can write; in a sandbox one may not be.
    ["XDG_RUNTIME_DIR", "XDG_CACHE_HOME", "HOME"]
        .into_iter()
        .filter_map(std::env::var_os)
        .filter(|v| !v.is_empty())
        .any(|dir| path.starts_with(dir))
        || tilo_paths::temp_dir().is_some_and(|dir| path.starts_with(dir))
}
