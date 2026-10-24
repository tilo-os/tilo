//! Pointer, keyboard and drop input: routing a window event to the body, the
//! panels, the ruler or the toolbar, and the editing keys and shortcuts.

use super::image_layout::{anchor as image_anchor, free_position as image_free_position};
use crate::effects::WordProcessorEffect;
use crate::num;
use crate::text_offsets::{byte_to_char, char_to_byte};
use crate::word_processor::Caret;
use crate::zoom_control::{MAX as ZOOM_MAX, MIN as ZOOM_MIN, STEP as ZOOM_STEP};
use automerge::AutoCommit;
use automerge::transaction::Transactable;
use tilo_doc::model::Region;
use ui::gesture::Granularity;
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::{
    DocumentAutocorrection, DragKind, EditorEventCtx, GuideDrag, KeyPressEffect, LayoutMode,
    ObjectDrag, WordProcessorWidget,
};

impl WordProcessorWidget {
    /// Drag-and-drop a file onto the window inserts it as an image at the
    /// **drop point**: seat the caret under the last pointer position (where
    /// the file was released) before inserting, so the image lands where the
    /// user dropped it rather than wherever the caret happened to be. winit's
    /// `DroppedFile` carries no coordinates, so the tracked pointer (updated
    /// on every `CursorMoved`, including during the drag-hover) is the drop
    /// location. A drop over the page body relocates the caret; a drop over
    /// chrome falls back to the existing caret.
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

    /// Ctrl+wheel zooms a step per notch (scroll-up = zoom in); a plain wheel
    /// scrolls, clamped to the content overflow.
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
        // Over the inspector, the wheel scrolls its column.
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
            // Ctrl + wheel zooms (a step per notch of scroll), the standard
            // editor gesture. Scroll-up (positive dy) zooms in.
            let step = if dy > 0.0 { ZOOM_STEP } else { -ZOOM_STEP };
            if self.zoom_by(step) {
                *handled = true;
            }
            return;
        }
        let new = (self.scroll_y - dy).clamp(self.scroll_floor(), self.max_scroll);
        // Exact-change check (not "close enough"): any scroll delta, however
        // tiny, should mark the frame dirty, so bit-compare rather than use a
        // tolerance that could mask a real change.
        if new.to_bits() != self.scroll_y.to_bits() {
            self.scroll_y = new;
            self.lifecycle.dirty = true;
            *handled = true;
        }
    }

    /// Track the pointer, update hover feedback, and continue
    /// whichever live drag is active (style-field scrub, ruler marker, layout
    /// guide, or a text-selection extend) — scrub takes priority, since a
    /// scrub in progress should never fall through to selection.
    pub(super) fn handle_cursor_moved(
        &mut self,
        x: f64,
        y: f64,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        self.pointer = Point::new(x / scale_factor, y / scale_factor);
        // Hover lifts on the chrome the editor paints itself.
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
            // Live-drag a ruler marker (margin / indent / tab). Updates the
            // value for an instant preview; persisted on release.
            self.ruler_drag_move();
            self.lifecycle.dirty = true;
            *handled = true;
        } else if let Some(gd) = self.guide_drag {
            // Live-drag a layout guide (margin / border / spacing). Guides
            // live in unscaled page space, so map the pointer through the
            // inverse zoom first.
            self.apply_guide_drag(gd, self.pointer_to_content(self.pointer));
            self.lifecycle.dirty = true; // repaint every move so the preview is live
            *handled = true;
        } else if let Some(granularity) = self.drag.drag() {
            // Extend the selection: keep the anchor, move the caret. The
            // `Laid` rects are unscaled, so hit-test in content space.
            self.caret_from_pointer(self.pointer_to_content(self.pointer), true);
            if granularity != Granularity::Char {
                self.extend_by_units(granularity);
            }
            // Claim the move so the host App repaints (its redraw is gated
            // behind `handled`).
            *handled = true;
        }
    }

    /// After a double- or triple-click, a drag (even the pointer's first
    /// jitter, which lands inside the clicked word) selects whole words or
    /// paragraphs and keeps the clicked one: the caret `caret_from_pointer`
    /// just placed is widened to the unit under it, on the far side of the
    /// clicked unit, as every toolkit text field does.
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

    /// Right-click: open the style-management menu when it lands on a
    /// styles-panel chip, else the editing/spell-check context menu when it
    /// lands on the page body (chrome — toolbar/panel/status bar — ignores
    /// the right button; it only hit-tests the left one).
    fn handle_right_press(&mut self, state: &AutoCommit, handled: &mut bool) {
        if !self.rect.contains(self.pointer) {
            return;
        }
        self.lifecycle.focused = true;
        // A right-click on a style chip in the styles panel opens the
        // style-management menu (Delete / Duplicate) before the generic
        // "panel is chrome, ignore" rule kicks in.
        if self.view.styles_panel_visible && self.panel_rect.contains(self.pointer) {
            if let Some(id) = self.style_chip_at(self.pointer) {
                self.style_menu.open(&id, self.pointer);
                self.style_menu.menu_mut().constrain_to(self.rect);
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

    /// Left press: route to an open menu first, then dismiss any staged
    /// toast, then (once the pointer is inside this widget and it's focused)
    /// walk the chrome regions in priority order, falling back to the page
    /// body — review affordances, layout guides, header/footer band switch,
    /// then the text-selection gesture.
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
        // Any left press dismisses a staged toast (a transient cue).
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

    /// While the context / "New" template / paragraph-style menu is open,
    /// route the left press to it: a row dispatches its action, a click
    /// outside dismisses it. Either way the click is consumed so it never
    /// reaches the page beneath. Returns whether a menu claimed the press.
    pub(super) fn handle_left_press_menu_routing(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        // While the context menu is open, route the left press to it: a
        // row dispatches its action, a click outside dismisses it. Either
        // way the click is consumed so it never reaches the page beneath.
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
        // Likewise route the press to the "New" template picker when open.
        if self.file_menu.is_open() {
            if let Some(choice) = self.file_menu.click(self.pointer) {
                self.apply_file_choice(state, choice);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // The paragraph-style menu (Delete / Duplicate) captures the press
        // the same way: a row runs its action, an outside click dismisses.
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

    /// A pending numeric-field commit, the styles panel, and the outline
    /// pane — in that priority order. Returns whether one of them claimed
    /// the press.
    fn handle_left_press_panels(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        // The failed-save banner's dismiss control.
        if self
            .save_error
            .is_some_and(|close| close.contains(self.pointer))
        {
            self.save_error = None;
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // A drawer over the page dismisses on a press on its scrim, instead
        // of placing a caret through it.
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
        // Styles panel (right side): dispatch a click on its controls.
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
        // Outline / navigation pane (left side): a click on an entry
        // scrolls that heading into view.
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

    /// The Find bar's fields, the ruler band and the bottom layout bar — in
    /// that priority order. Returns whether one of them claimed the press.
    fn handle_left_press_find_ruler_bars(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        // Ruler band (when on): grab a marker to drag, else a click in the
        // page span adds a tab stop on the current paragraph.
        if self.view.ruler_visible && self.ruler.rect().contains(self.pointer) {
            self.ruler_press(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // Bottom layout bar sits at the very bottom; document status floats above it.
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

    /// The page-body left press: right-margin review affordances, then (in a
    /// layout-edit mode) grabbing a guide, then the header/footer
    /// edit-context switch, and finally the text-selection gesture.
    fn handle_left_press_body(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        shift: bool,
        ctrl: bool,
        handled: &mut bool,
    ) {
        // Page-content hit-tests work in unscaled content space; map the
        // pointer through the inverse zoom transform once here.
        let cp = self.pointer_to_content(self.pointer);
        // Right-margin review affordances (comment Resolve, change
        // Accept/Reject) are drawn through the zoom transform, so they
        // hit-test in content space too. A hit dispatches + consumes the
        // click before it can start a text selection.
        if let Some(action) = self.review.action_at(self.pointer) {
            self.dispatch_review_action(action, state);
            self.lifecycle.dirty = true;
            *handled = true;
            return;
        }
        // In a layout-edit mode, grabbing a guide starts a drag instead of
        // a text selection.
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
        // Direct manipulation for free page objects. The laid rectangles and
        // pointer were both converted to unscaled content space, so this
        // selection never drifts at non-100% zoom.
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
        // Edit-context switch: a click into a page header/footer band
        // focuses that running region (creating an empty paragraph if the
        // region is empty); a click in the body while editing a running
        // region returns to the body. This is the entry point to Word-style
        // header/footer editing.
        let band = self.band_at_pointer(cp);
        match band {
            Some(region) if region != self.inner.region() => {
                // Switch into the band. `focus_region` seats the caret on
                // the region's (possibly newly-created) first block; the
                // band lays out next frame, after which clicks refine the
                // exact offset.
                self.inner.focus_region(state, region);
                self.refresh_blocks(state);
                self.drag.press(self.pointer, shift);
                self.lifecycle.dirty = true;
                *handled = true;
                return;
            }
            None if self.inner.in_running_region() => {
                // Click in the body area exits header/footer editing.
                self.inner.focus_body(state);
                self.refresh_blocks(state);
                // Fall through so the same click also seats the body caret.
            }
            _ => {}
        }
        // Selection gesture (ui::DragSelect): shift extends; double/triple
        // click select word/paragraph; a plain press starts a fresh drag.
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

    /// Left release: persist whichever live drag was active (style-field
    /// scrub, ruler marker, or layout guide) into the document, or — with no
    /// drag active — end a text-selection drag.
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
            // A ruler-marker drag persists on release (margins → page meta,
            // indents / tab stops → the caret block's attrs).
            self.ruler_drag_release(state);
            self.lifecycle.dirty = true;
            *handled = true;
        } else if let Some(gd) = self.guide_drag.take() {
            // A layout edit persists into the document on release: spacing
            // into the stylesheet, margins into the page-setup meta.
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

    /// A key press once the guard checks (focus, Escape, a numeric-field
    /// edit in progress, the Find bar, Ctrl+Enter's page break) and any
    /// Ctrl+<char> shortcut have both passed it through: motion keys extend
    /// or clear the selection, editing keys mutate the document, and any
    /// other printable text is inserted.
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

    /// Apply markdown-style paragraph prefixes through typed absolute actions.
    /// The two field-granular envelopes share the current input event's undo
    /// boundary; caret movement remains presentation-local.
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
        // Enter commits a word by splitting the block. Immediate Backspace
        // first rejoins that empty/new block, then restores the spelling.
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
                // editing consumes/collapses any selection
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
        // Inside a table, Tab/Shift-Tab walk cells (and Tab in the last cell
        // appends a row). Paragraph tab stops insert a tab; otherwise use the
        // conventional four-space fallback.
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

    /// Native word and visual-line selection use the toolkit's existing
    /// Unicode boundaries and shaped line geometry, preserving the anchor.
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
            // Option+Right follows macOS word-end semantics; Ctrl+Right
            // retains the shared next-word-start convention on other hosts.
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

    /// Early exits before a key press reaches editing: an unfocused widget,
    /// Escape (closing an open menu, or exiting header/footer editing), a
    /// numeric-field edit in progress, the Find bar capturing non-Ctrl keys,
    /// and Ctrl+Enter's manual page break. Returns whether one of these fully
    /// handled the event (the caller should stop dispatching further).
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
        // A free-object drag is a preview until release. Escape discards that
        // preview (rather than committing an accidental move) and leaves the
        // document/action history untouched.
        if matches!(logical_key, Key::Named(NamedKey::Escape)) && self.object_drag.is_some() {
            self.object_drag = None;
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // Escape dismisses an open context / style menu before anything else.
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
        // Escape exits header/footer editing, returning to the body.
        if self.inner.in_running_region() && matches!(logical_key, Key::Named(NamedKey::Escape)) {
            self.inner.focus_body(state);
            self.refresh_blocks(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // While a style field has the keyboard, the document does not.
        if self.editing_field.is_some() {
            *handled = true;
            return true;
        }
        // Ctrl+Enter inserts a manual page break: the caret block splits
        // and the new block is marked to start on a fresh page. (Handled
        // here because `NamedKey::Enter` is not a `Key::Character`.)
        if ctrl && matches!(logical_key, Key::Named(NamedKey::Enter)) {
            self.page_break_via_action(state);
            self.refresh_blocks(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        false
    }

    /// Ctrl+<char> shortcuts while the keyboard has focus: save/open/find,
    /// spell-check/markup view toggles, zoom, page-number insert, and the
    /// formatting/selection shortcuts dispatched through the same actions the
    /// toolbar uses. Always consumes the event once `ctrl` holds a
    /// `Character` key, whether or not `s` matched a known shortcut (so an
    /// unrecognized Ctrl+<char> doesn't fall through to plain typing).
    fn handle_ctrl_shortcut(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        s: &str,
        shift: bool,
        handled: &mut bool,
    ) {
        // Save is not an editing action — don't mark the doc
        // modified again after writing it. Ctrl+Shift+S always
        // opens the host Save-As picker; a bare Ctrl+S saves to
        // the current path (or, when untitled, prompts via the
        // same picker rather than inventing a path).
        if s.eq_ignore_ascii_case("s") {
            self.invoke_effect(&if shift {
                WordProcessorEffect::SaveAs { path: None }
            } else {
                WordProcessorEffect::Save { path: None }
            });
            *handled = true;
            return;
        }
        // Ctrl+O opens a document in place of the current one.
        if s.eq_ignore_ascii_case("o") {
            self.invoke_effect(&WordProcessorEffect::OpenDocument { path: None });
            *handled = true;
            return;
        }
        // Ctrl+Shift+L toggles the spell-check squiggles (view-only,
        // default ON). Never mutates the document.
        if shift && s.eq_ignore_ascii_case("l") {
            self.toggle_spellcheck();
            *handled = true;
            return;
        }
        // Ctrl+Shift+M toggles comment + tracked-change markup
        // (view-only, default ON). Never mutates the document.
        if shift && s.eq_ignore_ascii_case("m") {
            self.toggle_markup();
            *handled = true;
            return;
        }
        // Ctrl+Shift+P inserts a page-number field at the caret.
        // Primarily for header/footer editing (a running "Page N"),
        // but works in the body too (the field renderer substitutes
        // the page number per page).
        if shift && s.eq_ignore_ascii_case("p") {
            self.insert_page_number_via_action(state);
            self.refresh_blocks(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return;
        }
        // Zoom: Ctrl+= / Ctrl++ (in), Ctrl+- (out), Ctrl+0 (reset
        // to 100%). These are view-only — they never mutate the doc,
        // so they return before the editing path below.
        match s {
            "=" | "+" => {
                self.zoom_by(ZOOM_STEP);
                *handled = true;
                return;
            }
            "-" | "_" => {
                self.zoom_by(-ZOOM_STEP);
                *handled = true;
                return;
            }
            "0" => {
                self.set_zoom(1.0);
                *handled = true;
                return;
            }
            _ => {}
        }
        match s.to_lowercase().as_str() {
            // Formatting shortcuts dispatch the same
            // `RichTextAction` the toolbar + Ctrl+K palette
            // do (one replayable source of truth).
            "b" => self.dispatch_toggle_mark(state, "strong"),
            "i" => self.dispatch_toggle_mark(state, "em"),
            "u" => self.dispatch_toggle_mark(state, "underline"),
            // Ctrl+. → superscript, Ctrl+, → subscript (mutually
            // exclusive; matches common word-processor bindings).
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
            // Ctrl+V keeps formatting; Ctrl+Shift+V pastes plain text.
            "v" if shift => self.clipboard_paste_plain(state),
            "v" => self.clipboard_paste(state),
            "a" => self.select_all_via_primitive(state),
            // Ctrl+1/2/3 → heading levels. (Ctrl+0 is zoom-reset;
            // Body paragraph is reachable via the toolbar / panel.)
            "1" => self.dispatch_paragraph_style(state, "heading", Some(1), None),
            "2" => self.dispatch_paragraph_style(state, "heading", Some(2), None),
            "3" => self.dispatch_paragraph_style(state, "heading", Some(3), None),
            // z / y (undo/redo) are handled by the host App.
            _ => {}
        }
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
        *handled = true;
    }
}
impl WordProcessorWidget {
    #[expect(
        clippy::too_many_lines,
        reason = "the widget's single event entry point routes each window event to overlays, panels and the editor in priority order"
    )]
    pub(super) fn route_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.styles_panel_full_window() {
            // Displaced document overlays cannot act above the Styles page.
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
        // An open menu owns input until selection or dismissal. Forward
        // movement as well as presses so the shared menu can paint its hover;
        // keep navigation/typing from editing the document behind the popup.
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
        // While the colour-picker popover is open it captures input (modal-ish):
        // route drags to it, apply committed colours, and let an outside-click /
        // Escape dismiss it before any page/panel hit-testing.
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
        // The expanded comment thread owns its reply field and modal-like
        // button surface. Keep it ahead of the editor and Find bar so typing
        // never leaks into the document beneath it.
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
            // Drag-and-drop a file onto the window inserts it as an image at the
            // drop point (see `handle_dropped_file`).
            WindowEvent::DroppedFile(path) => {
                self.handle_dropped_file(&mut ctx.state, path, ctx.handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Window groups offer unclaimed events to visible apps in
                // presentation order. Ctrl+wheel represents a browser
                // trackpad pinch, so an unfocused Writer must leave it for
                // the host-selected app rather than zoom in the background.
                if self.lifecycle.focused || !ctrl {
                    self.handle_mouse_wheel(delta, ctx.scale_factor, ctrl, ctx.handled);
                }
            }
            WindowEvent::PinchGesture { delta, phase, .. } => {
                // Native pinch follows the same host-ownership rule as the
                // browser's Ctrl+wheel form above. Pointer geometry still
                // determines where a focused Writer zooms.
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
                            // Keep the document row beneath the gesture steady.
                            // Horizontal alignment remains the centered-page convention.
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
            // Right (secondary) press opens the editing / spell-check context
            // menu on the page body. Toolbar / panel / status-bar chrome ignore
            // it (they have their own hit-testing on the left button).
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

/// A path the windowing toolkit spooled a drag-and-drop payload into: lives
/// under `$XDG_RUNTIME_DIR` and is named `winit-dnd-…`. Only such files are
/// deleted after an insert — never a file the user chose.
pub(super) fn is_dnd_spool_path(path: &std::path::Path) -> bool {
    let named_by_the_toolkit = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("winit-dnd-"));
    if !named_by_the_toolkit {
        return false;
    }
    // The toolkit spools into the first of these it can actually write
    // to — inside a sandbox `$XDG_RUNTIME_DIR` is set but is not always
    // writable — so a spool file is one that sits under any of them.
    // The name check above is what keeps this from ever matching a file
    // the user chose.
    ["XDG_RUNTIME_DIR", "XDG_CACHE_HOME", "HOME"]
        .into_iter()
        .filter_map(std::env::var_os)
        .filter(|v| !v.is_empty())
        .any(|dir| path.starts_with(dir))
        || tilo_paths::temp_dir().is_some_and(|dir| path.starts_with(dir))
}
