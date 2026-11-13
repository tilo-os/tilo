//! The editor's controls: its drawers, formatting tools, panel
//! fields, pickers and palettes, the context menu and the clipboard.
use super::block_layout::{line_spacing as block_line_spacing, line_spacing_label};
use super::image_layout::{anchor as image_anchor, wrap as image_wrap};
use super::text_runs::{char_font_family, char_font_size, char_highlight, parse_hex_rgb};
use super::toolbar::{HIGHLIGHT_PALETTE, STYLE_PALETTE, ToolAction};
use crate::WpFormat;
use crate::compact_swatch_palette::{PaletteChoice, PaletteKind, PaletteRequest};
use crate::i18n::{arg, t, t_args};
use crate::num;
use crate::paint_context::PaintContext;
use crate::style_menu::StyleMenuAction;
use crate::text_offsets::char_to_byte;
use crate::word_processor::Caret;
use automerge::{AutoCommit, ObjId};
use tilo_doc::snapshot::BlockView;
use ui::clipboard_paste;
use ui::richtext::FontFamily;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{SegmentedTouchGroup, SegmentedTouchResult};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::panels::swatch_names;
use super::{CtxAction, Editor, NumField, PanelHit, block_layout, text_runs};

impl Editor {
    fn block_index_cached(&self, block: &str) -> Option<usize> {
        self.blocks.iter().position(|b| b.id == block)
    }

    fn selection_span_cached(&self) -> Option<(Caret, Caret)> {
        crate::word_processor::ordered_selection_span(
            self.inner.selection_anchor.as_ref(),
            &self.inner.caret,
            |id| self.block_index_cached(id),
        )
    }

    pub(super) fn selection_blocks_cached(&self) -> Vec<(String, usize, usize)> {
        let Some((start, end)) = self.selection_span_cached() else {
            return vec![];
        };
        crate::word_processor::selection_block_ranges(
            &start,
            &end,
            |id| self.block_index_cached(id),
            |i| {
                let b = self.blocks.get(i)?;
                Some((b.id.clone(), b.text.chars().count()))
            },
        )
    }

    pub(super) fn selection_bytes_for(&self, block_id: &str, text: &str) -> Option<(usize, usize)> {
        let (_, s, e) = self
            .selection_blocks_cached()
            .into_iter()
            .find(|(b, _, _)| b == block_id)?;
        if s >= e {
            return None;
        }
        Some((char_to_byte(text, s), char_to_byte(text, e)))
    }

    pub(super) fn move_page(&mut self, forward: bool, extend: bool) {
        let page = self.caret_page();
        let target = if forward {
            page + 1
        } else {
            page.saturating_sub(1)
        };
        if target == page || target >= self.pages.max(1) {
            return;
        }
        let column = self.caret_screen_x();
        let Some(top) = self
            .laid
            .iter()
            .filter(|laid| laid.page == target)
            .map(|laid| laid.y_top)
            .reduce(f64::min)
        else {
            return;
        };
        let Some(column) = column else { return };
        self.caret_from_pointer(Point::new(column, top + 1.0), extend);
        self.lifecycle.dirty = true;
    }

    fn caret_screen_x(&self) -> Option<f64> {
        let caret = &self.inner.caret;
        let laid = self.laid.iter().find(|laid| laid.id == caret.block)?;
        let (_, layout) = self.layout_cache.get(&laid.id)?;
        let (x, _, _) = layout.caret_geometry(laid.caret_byte(caret.offset));
        Some(laid.text_x + f64::from(x))
    }

    pub(super) fn extend_or_clear(&mut self, shift: bool) {
        if shift {
            self.inner.set_anchor();
        } else {
            self.inner.clear_anchor();
        }
    }

    pub(super) fn mark_active_in_selection(&self, name: &str) -> bool {
        let sel: Vec<_> = self
            .selection_blocks_cached()
            .into_iter()
            .filter(|(_, s, e)| s < e)
            .collect();
        if sel.is_empty() {
            return false;
        }
        sel.iter().all(|(bid, s, e)| {
            self.blocks
                .iter()
                .find(|b| &b.id == bid)
                .is_some_and(|b| text_runs::marks_cover_range(&b.marks, name, *s, *e))
        })
    }

    pub(super) fn current_block_style(&self) -> (String, Option<i64>) {
        let b = self.blocks.iter().find(|b| b.id == self.inner.caret.block);
        let kind = b.map_or_else(|| "paragraph".into(), |b| b.kind.clone());
        let level = b.and_then(|b| b.int_attr("level"));
        (kind, level)
    }

    pub(super) fn apply_tool(&mut self, state: &mut ui::widget::Edits<'_>, action: ToolAction) {
        match action {
            // Every format dispatches the palette's own `RichTextAction`, as one undo boundary.
            ToolAction::Bold => self.dispatch_toggle_mark(state, "strong"),
            ToolAction::Italic => self.dispatch_toggle_mark(state, "em"),
            ToolAction::Underline => self.dispatch_toggle_mark(state, "underline"),
            ToolAction::Strike => self.dispatch_toggle_mark(state, "strike"),
            ToolAction::TextColor(Some(c)) => self.dispatch_set_mark_value(
                state,
                "color",
                crate::actions::MarkVal::Str(crate::styles::color_to_hex(c)),
            ),
            ToolAction::TextColor(None) => self.dispatch_clear_mark(state, "color"),
            ToolAction::Highlight(Some(c)) => self.dispatch_set_mark_value(
                state,
                "highlight",
                crate::actions::MarkVal::Str(crate::styles::color_to_hex(c)),
            ),
            ToolAction::Highlight(None) => self.dispatch_clear_mark(state, "highlight"),
            ToolAction::Super => self.dispatch_exclusive_mark(state, "super", "sub"),
            ToolAction::Sub => self.dispatch_exclusive_mark(state, "sub", "super"),
            ToolAction::Hyperlink => {
                if !self.selection_blocks_cached().is_empty() {
                    self.action_palette_request = Some(
                        crate::effects::WordProcessorEffect::SET_HYPERLINK
                            .on("word-processor.caret"),
                    );
                }
            }
            ToolAction::FontSizeInc => {
                let next =
                    num::i64_of_f32((self.current_font_size() + 1.0).round().clamp(6.0, 200.0));
                self.dispatch_set_mark_value(state, "size", crate::actions::MarkVal::Int(next));
            }
            ToolAction::FontSizeDec => {
                let next =
                    num::i64_of_f32((self.current_font_size() - 1.0).round().clamp(6.0, 200.0));
                self.dispatch_set_mark_value(state, "size", crate::actions::MarkVal::Int(next));
            }
            ToolAction::SetFont(family) => {
                self.dispatch_set_mark_value(
                    state,
                    "font",
                    crate::actions::MarkVal::Str(font_value(family).to_string()),
                );
            }
            ToolAction::BulletList => {
                self.dispatch_toggle_list(state, false);
                self.refresh_blocks(state);
            }
            ToolAction::NumberedList => {
                self.dispatch_toggle_list(state, true);
                self.refresh_blocks(state);
            }
            ToolAction::InsertImage => self.request_image_insert(state),
            ToolAction::InsertTable => self.insert_table_via_action(state, 2, 2),
            table_action @ (ToolAction::TableRowAdd
            | ToolAction::TableRowDel
            | ToolAction::TableColAdd
            | ToolAction::TableColDel
            | ToolAction::TableDelete) => self.apply_table_tool(state, table_action),
            ToolAction::ToggleStyles => {
                self.view.styles_panel_visible = !self.view.styles_panel_visible;
                if self.view.styles_panel_visible {
                    self.inspector_drawer.open();
                } else {
                    self.inspector_drawer.close();
                }
            }
            ToolAction::ToggleRuler => self.view.ruler_visible = !self.view.ruler_visible,
            ToolAction::ToggleOutline => {
                self.outline.toggle();
                if self.outline.is_open() {
                    self.outline_drawer.open();
                } else {
                    self.outline_drawer.close();
                }
            }
            ToolAction::InsertToc => self.generate_toc(state),
            ToolAction::TextColorPalette => self.open_compact_swatch_palette(PaletteKind::Text),
            ToolAction::HighlightPalette => {
                self.open_compact_swatch_palette(PaletteKind::Highlight);
            }
        }
    }

    fn apply_table_tool(&mut self, state: &mut ui::widget::Edits<'_>, action: ToolAction) {
        let Some((_, row, column, _, _)) = self.caret_table_target() else {
            if action == ToolAction::TableDelete {
                self.edit_table_via_action(state, crate::actions::TableEdit::Delete);
            }
            return;
        };
        let edit = match action {
            ToolAction::TableRowAdd => crate::actions::TableEdit::InsertRow {
                at: row.saturating_add(1),
            },
            ToolAction::TableRowDel => crate::actions::TableEdit::DeleteRow { row },
            ToolAction::TableColAdd => crate::actions::TableEdit::InsertColumn {
                at: column.saturating_add(1),
            },
            ToolAction::TableColDel => crate::actions::TableEdit::DeleteColumn { column },
            ToolAction::TableDelete => crate::actions::TableEdit::Delete,
            _ => return,
        };
        self.edit_table_via_action(state, edit);
    }

    fn edit_focused_style(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        persist: bool,
        f: impl FnOnce(&mut crate::styles::StyleProps),
    ) {
        let Some(key) = self.focused_style_key() else {
            return;
        };
        if let Some(st) = self.sheet.styles.get_mut(&key) {
            f(st);
        }
        self.lifecycle.dirty = true;
        if persist {
            self.persist_stylesheet_via_actions(state);
        }
    }

    pub(super) fn apply_panel_hit(&mut self, state: &mut ui::widget::Edits<'_>, hit: PanelHit) {
        match hit {
            PanelHit::Close => {
                self.view.styles_panel_visible = false;
                self.focus_hosted(None, false);
                self.lifecycle.dirty = true;
            }
            PanelHit::Pick(id) => {
                self.dispatch_apply_named_style(state, &id);
                self.refresh_blocks(state);
                self.lifecycle.dirty = true;
            }
            PanelHit::ParaAlign(a) => {
                self.dispatch_block_attr(
                    state,
                    "align",
                    crate::actions::AttrVal::Str(a.to_string()),
                );
                self.refresh_blocks(state);
                self.lifecycle.dirty = true;
            }
            PanelHit::SetColor(c) => self.edit_focused_style(state, true, |st| st.color = c),
            PanelHit::OpenColorPicker => self.open_color_picker(),
            PanelHit::NewStyle => self.apply_panel_hit_new_style(state),
            PanelHit::ImageScale(factor) => self.apply_panel_hit_image_scale(state, factor),
            PanelHit::ImageFlipH => self.apply_panel_hit_image_flip(state, true),
            PanelHit::ImageFlipV => self.apply_panel_hit_image_flip(state, false),
            PanelHit::ImageCropInset => self.apply_panel_hit_image_crop(state, true),
            PanelHit::ImageCropReset => self.apply_panel_hit_image_crop(state, false),
            PanelHit::ImageInline => {
                self.apply_panel_hit_image_placement(state, "inline", "none");
            }
            PanelHit::ImageFreePosition => {
                self.apply_panel_hit_image_placement(state, "page", "none");
            }
            PanelHit::ImageWrapLeft => {
                self.apply_panel_hit_image_placement(state, "paragraph", "left");
            }
            PanelHit::ImageWrapRight => {
                self.apply_panel_hit_image_placement(state, "paragraph", "right");
            }
        }
    }

    fn apply_panel_hit_new_style(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(key) = self.focused_style_key() else {
            return;
        };
        let base = self.sheet.get(&key);
        let new_id = self.sheet.fresh_custom_id();
        let mut props = base.clone();
        props.name = format!("{} copy", base.name);
        self.sheet.styles.insert(new_id.clone(), props);
        self.persist_stylesheet_via_actions(state);
        self.dispatch_apply_named_style(state, &new_id);
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    fn apply_panel_hit_image_scale(&mut self, state: &mut ui::widget::Edits<'_>, factor: f64) {
        let Some(id) = self.selected_image_id() else {
            return;
        };
        let (cw, ch) = self
            .inner
            .image_size(state, &id)
            .or_else(|| self.selected_image_dims())
            .unwrap_or((240.0, 160.0));
        // Width and height change in one action, so a replay cannot stale one attribute.
        let width = num::i64_of_f64((cw * factor).round().clamp(16.0, 4_000.0));
        let height = num::i64_of_f64((ch * factor).round().clamp(16.0, 4_000.0));
        self.perform_action(
            state,
            &crate::actions::set_image_size_action(&id, width, height),
        );
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    fn apply_panel_hit_image_flip(&mut self, state: &mut ui::widget::Edits<'_>, horizontal: bool) {
        let Some(id) = self.selected_image_id() else {
            return;
        };
        let key = if horizontal { "flip_h" } else { "flip_v" };
        let active = self
            .blocks
            .iter()
            .find(|b| b.id == id)
            .is_some_and(|b| b.flag(key));
        self.perform_action(
            state,
            &crate::actions::set_block_attr_action(
                crate::actions::RegionKind::Body,
                vec![id],
                key,
                crate::actions::AttrVal::Bool(!active),
            ),
        );
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    fn apply_panel_hit_image_crop(&mut self, state: &mut ui::widget::Edits<'_>, inset: bool) {
        let Some(id) = self.selected_image_id() else {
            return;
        };
        let Some(block) = self.blocks.iter().find(|b| b.id == id) else {
            return;
        };
        let edge = |key: &str| block.int_attr(key).unwrap_or(0);
        let (mut left, mut top, mut right, mut bottom) = (
            edge("crop_left"),
            edge("crop_top"),
            edge("crop_right"),
            edge("crop_bottom"),
        );
        if inset {
            if left.saturating_add(right) > 8_500 || top.saturating_add(bottom) > 8_500 {
                return;
            }
            left += 250;
            top += 250;
            right += 250;
            bottom += 250;
        } else {
            left = 0;
            top = 0;
            right = 0;
            bottom = 0;
        }
        self.perform_action(
            state,
            &crate::actions::set_image_crop_action(&id, left, top, right, bottom),
        );
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    fn apply_panel_hit_image_placement(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        anchor: &str,
        wrap: &str,
    ) {
        let Some(id) = self.selected_image_id() else {
            return;
        };
        self.perform_action(
            state,
            &crate::actions::set_image_placement_action(&id, anchor, wrap, 0, 0, 0),
        );
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    pub(super) fn open_color_picker(&mut self) {
        let seed = self
            .focused_style_key()
            .map_or([0x1c, 0x1c, 0x20], |k| self.sheet.get(&k).color);
        self.color_picker.open(seed, self.pointer);
        self.lifecycle.dirty = true;
    }

    pub(super) fn pump_color_picker(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        mods: Modifiers,
        state: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        handled: &mut bool,
    ) -> bool {
        let result = self
            .color_picker
            .handle_event(crate::style_color_picker::EventArgs {
                event,
                scale,
                modifiers: mods,
                state: state.reborrow(),
                root,
                host: self.rect,
                panel: self.panel_rect,
            });
        if let Some(rgb) = result.changed {
            self.edit_focused_style(state, true, |st| st.color = rgb);
        }
        if result.consumed {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.pointer = Point::new(position.x / scale, position.y / scale);
            }
            self.lifecycle.dirty = true;
            *handled = true;
        }
        result.consumed
    }

    pub(super) fn open_compact_swatch_palette(&mut self, kind: PaletteKind) {
        self.context_menu.close();
        self.style_menu.close();
        self.file_menu.close();
        let (palette, title, clear_label): (&[[u8; 3]], String, String) = match kind {
            PaletteKind::Text => (
                &STYLE_PALETTE,
                t("picker-text-color"),
                t("a11y-text-color-default"),
            ),
            PaletteKind::Highlight => (
                &HIGHLIGHT_PALETTE,
                t("picker-highlight"),
                t("a11y-highlight-clear"),
            ),
        };
        let labels = swatch_names(kind);
        let anchor = Point::new(self.rect.x1, self.rect.y0);
        let current = match kind {
            PaletteKind::Text => self.current_text_color(),
            PaletteKind::Highlight => self.current_highlight_color(),
        };
        self.compact_swatch_palette.open(PaletteRequest {
            kind,
            selected: current.and_then(|rgb| palette.iter().position(|c| *c == rgb)),
            colors: palette,
            labels,
            title,
            clear_label,
            anchor,
            host: self.rect,
            theme: &self.desk_theme,
        });
        self.lifecycle.dirty = true;
    }

    pub(super) fn pump_compact_swatch_palette(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        state: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        handled: &mut bool,
    ) -> bool {
        if !self.compact_swatch_palette.is_open() {
            return false;
        }
        let choice = self
            .compact_swatch_palette
            .handle_event(event, scale, modifiers, state, root);
        if let Some((kind, choice)) = choice {
            self.apply_compact_palette_choice(state, kind, choice);
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / scale, position.y / scale);
        }
        self.lifecycle.dirty = true;
        *handled = true;
        true
    }

    pub(super) fn apply_compact_palette_choice(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        kind: PaletteKind,
        choice: PaletteChoice,
    ) {
        let action = match (kind, choice) {
            (PaletteKind::Text, PaletteChoice::Color(color)) => ToolAction::TextColor(Some(color)),
            (PaletteKind::Text, PaletteChoice::Clear) => ToolAction::TextColor(None),
            (PaletteKind::Highlight, PaletteChoice::Color(color)) => {
                ToolAction::Highlight(Some(color))
            }
            (PaletteKind::Highlight, PaletteChoice::Clear) => ToolAction::Highlight(None),
        };
        self.apply_tool(state, action);
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    fn font_probe(&self) -> Option<(&BlockView, usize)> {
        let b = self
            .blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)?;
        let pos = self.inner.caret.offset.saturating_sub(1);
        Some((b, pos))
    }

    pub(super) fn current_font(&self) -> FontFamily {
        self.font_probe()
            .and_then(|(b, pos)| char_font_family(pos, &b.marks))
            .or_else(|| {
                self.focused_style_key()
                    .map(|key| font_family_of(&self.sheet.get(&key).font))
            })
            .unwrap_or_default()
    }

    pub(super) fn list_active(&self, ordered: bool) -> bool {
        self.blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)
            .is_some_and(|b| b.kind == "list_item" && b.flag("ordered") == ordered)
    }

    pub(super) fn current_font_size(&self) -> f32 {
        if let Some(sz) = self
            .font_probe()
            .and_then(|(b, pos)| char_font_size(pos, &b.marks))
        {
            return sz;
        }
        self.focused_style_key()
            .map_or(16.5, |k| self.sheet.get(&k).size)
    }

    pub(super) fn segmented_rects(&self) -> [Vec<Rect>; 2] {
        if self.modal_capture().is_some()
            || self.color_picker.is_open()
            || self.compact_swatch_palette.is_open()
        {
            return std::array::from_fn(|_| Vec::new());
        }
        let align = self
            .panel_hits
            .iter()
            .filter_map(|(rect, hit)| matches!(hit, PanelHit::ParaAlign(_)).then_some(*rect))
            .collect();
        let placement = self
            .panel_hits
            .iter()
            .filter_map(|(rect, hit)| {
                matches!(
                    hit,
                    PanelHit::ImageInline
                        | PanelHit::ImageFreePosition
                        | PanelHit::ImageWrapLeft
                        | PanelHit::ImageWrapRight
                )
                .then_some(*rect)
            })
            .collect();
        [align, placement]
    }

    pub(super) fn handle_segmented_touch(
        &mut self,
        touch: &winit::event::Touch,
        scale: f64,
        edits: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let [align, placement] = self.segmented_rects();
        let current = self.current_align();
        let selected_align = ["left", "center", "right", "justify"]
            .iter()
            .position(|value| *value == current)
            .unwrap_or(ui::widget::toggle_group::NONE_CHOSEN);
        let image = self.selected_image_id();
        let selected_placement = self
            .blocks
            .iter()
            .find(|block| Some(&block.id) == image.as_ref())
            .map_or(ui::widget::toggle_group::NONE_CHOSEN, |block| {
                match (image_anchor(block), image_wrap(block)) {
                    ("page", _) => 1,
                    (_, "left") => 2,
                    (_, "right") => 3,
                    _ => 0,
                }
            });
        let groups = [
            SegmentedTouchGroup::new(0, &align, selected_align),
            SegmentedTouchGroup::new(1, &placement, selected_placement),
        ];
        let result = self.segmented_touch.handle(touch, scale, &groups);
        if let SegmentedTouchResult::Selected { group, index } = result {
            let hit = match group {
                0 => ["left", "center", "right", "justify"]
                    .get(index)
                    .copied()
                    .map(PanelHit::ParaAlign),
                1 => [
                    PanelHit::ImageInline,
                    PanelHit::ImageFreePosition,
                    PanelHit::ImageWrapLeft,
                    PanelHit::ImageWrapRight,
                ]
                .get(index)
                .cloned(),
                _ => None,
            };
            if let Some(hit) = hit {
                self.apply_panel_hit(edits, hit);
            }
        }
        result.handled()
    }

    pub(super) fn current_align(&self) -> String {
        self.blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)
            .and_then(|b| b.str_attr("align").map(str::to_owned))
            .unwrap_or_else(|| "left".into())
    }

    pub(super) fn current_line_spacing(&self) -> f64 {
        self.blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)
            .map_or(1.0, |b| f64::from(block_line_spacing(b)))
    }

    pub(super) fn current_highlight_color(&self) -> Option<[u8; 3]> {
        let (b, pos) = self.font_probe()?;
        char_highlight(pos, &b.marks)
    }

    pub(super) fn clipboard_copy(&mut self, state: &AutoCommit) -> bool {
        if !self.inner.has_selection() {
            return false;
        }
        let (plain, blocks) = self.inner.copy_fragment(state);
        // A copy still waiting on the compositor counts as taken: undo brings the text back.
        if ui::text_selection::try_clipboard_copy(&plain).outcome() == Some(false) {
            return false;
        }
        self.rich_clip = Some((plain, blocks));
        true
    }

    pub(super) fn clipboard_paste(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(sys) = clipboard_paste() else {
            return;
        };
        let rich = self
            .rich_clip
            .as_ref()
            .filter(|(plain, _)| plain == &sys)
            .map(|(_, blocks)| blocks.clone());
        match rich {
            Some(blocks) => self.paste_fragment_via_action(state, blocks),
            None => self.paste_plain_text_via_action(state, &sys),
        }
    }

    fn paste_plain_text_via_action(&mut self, state: &mut ui::widget::Edits<'_>, text: &str) {
        // Beyond the primitive's per-call bound (an agent limit) the direct fragment is used.
        let content = crate::actions::editing::PasteContent::Text(text.to_string());
        let view = self.local_edit_view();
        if let Ok((envelopes, next)) = crate::actions::editing::paste(&view, state, &content) {
            for action in &envelopes {
                self.perform_action(state, action);
            }
            self.adopt_local_view(&next, state);
            return;
        }
        let blocks = text
            .replace('\r', "")
            .split('\n')
            .map(|line| crate::word_processor::ClipBlock {
                kind: "paragraph".to_string(),
                text: line.to_string(),
                marks: Vec::new(),
                level: None,
                ordered: None,
                align: None,
                attrs: std::collections::BTreeMap::default(),
            })
            .collect();
        self.paste_fragment_via_action(state, blocks);
    }

    pub(super) fn clipboard_paste_plain(&mut self, state: &mut ui::widget::Edits<'_>) {
        if let Some(t) = clipboard_paste() {
            self.paste_plain_text_via_action(state, &t);
        }
    }

    pub(super) fn select_all_in_block(&mut self, state: &AutoCommit) {
        self.inner.move_home();
        self.inner.clear_anchor();
        self.inner.set_anchor();
        self.inner.move_end(state);
    }

    fn word_span_at(text: &str, off: usize) -> (usize, usize) {
        let chars: Vec<char> = text.chars().collect();
        let off = off.min(chars.len());
        let is_word = |c: char| !c.is_whitespace();
        let mut s = off;
        while s > 0 && chars.get(s - 1).is_some_and(|&c| is_word(c)) {
            s -= 1;
        }
        let mut e = off;
        while chars.get(e).is_some_and(|&c| is_word(c)) {
            e += 1;
        }
        (s, e)
    }

    fn suggestions_for_word(&mut self, word: &str) -> Option<Vec<String>> {
        if word.is_empty() {
            return None;
        }
        let language = self.proofing.language.clone();
        let dict = self.dictionary();
        if dict.contains(word) || spellcheck::spans_for_language(&language, word).is_empty() {
            return None;
        }
        Some(spellcheck::suggestions_for_language(&language, word, 5))
    }

    pub(super) fn open_context_menu(
        &mut self,
        state: &AutoCommit,
        content_pt: Point,
        pos: [f32; 2],
    ) {
        // Select the word under the click when nothing is selected, so Cut and Copy act.
        let selected_ranges = self.inner.selection_blocks(state);
        let previous_caret = self.inner.caret.clone();
        let previous_anchor = self.inner.selection_anchor.clone();
        self.caret_from_pointer(content_pt, false);
        let raw_click_off = self.inner.caret.offset;
        let block = self.inner.caret.block.clone();
        let block_text = self.inner.current_text(state);
        let inside_selection = selected_ranges.iter().any(|(id, start, end)| {
            *id == block && *start <= raw_click_off && raw_click_off <= *end
        });
        if inside_selection {
            self.inner.caret = previous_caret;
            self.inner.selection_anchor = previous_anchor;
        } else {
            self.inner.select_word(state);
        }

        let (wstart, wend) = Self::word_span_at(&block_text, raw_click_off);
        let word: String = block_text
            .chars()
            .skip(wstart)
            .take(wend - wstart)
            .collect();

        let mut menu = ContextMenu::new();
        menu.set_text_scale(crate::num::f32_of_f64(self.text_scale));
        menu.set_viewport(Some(self.rect));
        let mut actions: Vec<Option<CtxAction>> = Vec::new();

        self.push_review_rows(&mut menu, &mut actions, &block, wstart, wend, raw_click_off);
        self.push_spellcheck_rows(&mut menu, &mut actions, &block, &word, wstart, wend);
        self.push_clipboard_rows(&mut menu, &mut actions);

        menu.open_at(pos);
        self.context_menu = menu;
        self.context_actions = actions;
        self.lifecycle.dirty = true;
    }

    fn push_spellcheck_rows(
        &mut self,
        menu: &mut ContextMenu,
        actions: &mut Vec<Option<CtxAction>>,
        block: &str,
        word: &str,
        wstart: usize,
        wend: usize,
    ) {
        if !self.spellcheck_enabled() {
            return;
        }
        let Some(suggestions) = self.suggestions_for_word(word) else {
            return;
        };
        for sug in suggestions.iter().take(3) {
            menu.push(MenuItem::action(sug.clone(), || {}));
            actions.push(Some(CtxAction::ApplySuggestion {
                block: block.to_string(),
                start: wstart,
                end: wend,
                replacement: sug.clone(),
            }));
        }
        menu.push(MenuItem::action(t("menu-add-to-dictionary"), || {}));
        actions.push(Some(CtxAction::AddToDictionary(word.to_string())));
        menu.push(MenuItem::separator());
        actions.push(None);
    }

    fn push_clipboard_rows(&self, menu: &mut ContextMenu, actions: &mut Vec<Option<CtxAction>>) {
        let has_sel = self.inner.has_selection();
        let can_paste = clipboard_paste().is_some_and(|s| !s.is_empty());
        if has_sel {
            menu.push(MenuItem::action(t("menu-cut"), || {}));
            actions.push(Some(CtxAction::Cut));
            menu.push(MenuItem::action(t("menu-copy"), || {}));
            actions.push(Some(CtxAction::Copy));
        }
        if can_paste {
            menu.push(MenuItem::action(t("menu-paste"), || {}));
            actions.push(Some(CtxAction::Paste));
        }
        if !menu.items().is_empty() {
            menu.push(MenuItem::separator());
            actions.push(None);
        }
        menu.push(MenuItem::action(t("menu-select-all"), || {}));
        actions.push(Some(CtxAction::SelectAll));
    }

    pub(super) fn dispatch_context_action(
        &mut self,
        idx: usize,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let Some(Some(action)) = self.context_actions.get(idx).cloned() else {
            return false;
        };
        match action {
            CtxAction::Cut => {
                if self.clipboard_copy(state) {
                    self.insert_text_via_action(state, "");
                }
            }
            CtxAction::Copy => {
                self.clipboard_copy(state);
            }
            CtxAction::Paste => {
                self.clipboard_paste(state);
            }
            CtxAction::SelectAll => self.select_all_via_primitive(state),
            CtxAction::AddToDictionary(word) => {
                if self.proofing.personal {
                    let result = ui::personal_dictionary::snapshot().and_then(|snapshot| {
                        ui::personal_dictionary::edit(
                            snapshot.revision,
                            ui::personal_dictionary::Edit::Add(ui::personal_dictionary::Word::new(
                                &word, None,
                            )?),
                        )
                    });
                    if let Err(error) = result {
                        eprintln!("word-processor: could not keep the shared word: {error}");
                    }
                    self.dict = None;
                    self.spell_cache.clear();
                } else if self.proofing.personal_words.add(&word) {
                    self.dict = None;
                    self.spell_cache.clear();
                }
            }
            CtxAction::ApplySuggestion {
                block,
                start,
                end,
                replacement,
            } => {
                // One attributable envelope through the executor, as the published operation does.
                let region = self.inner.region_kind();
                self.splice_via_action(state, region, &block, start, end, &replacement);
                self.refresh_blocks(state);
                // The block's text changed; its cached spans are stale.
                self.spell_cache.remove(&block);
            }
            CtxAction::Review(action) => self.dispatch_review_action(action, state),
        }
        self.context_menu.close();
        self.lifecycle.dirty = true;
        true
    }

    pub(super) fn style_chip_at(&self, p: Point) -> Option<String> {
        if self.style_pick_rect.contains(p) {
            return self.focused_style_key();
        }
        self.panel_hits.iter().find_map(|(r, h)| match h {
            PanelHit::Pick(id) if r.contains(p) => Some(id.clone()),
            _ => None,
        })
    }

    pub(super) fn dispatch_style_menu(
        &mut self,
        action: StyleMenuAction,
        state: &mut ui::widget::Edits<'_>,
    ) {
        match action {
            StyleMenuAction::Delete(id) => {
                if self.sheet.remove(&id) {
                    self.persist_stylesheet_via_actions(state);
                    self.refresh_blocks(state);
                }
            }
            StyleMenuAction::Duplicate(id) => {
                if let Some(mut properties) = self.sheet.styles.get(&id).cloned() {
                    let new_id = self.sheet.fresh_custom_id();
                    properties.name = format!("{} copy", properties.name);
                    self.sheet.styles.insert(new_id.clone(), properties);
                    self.persist_stylesheet_via_actions(state);
                    self.dispatch_apply_named_style(state, &new_id);
                    self.refresh_blocks(state);
                }
            }
        }
        self.lifecycle.dirty = true;
    }

    pub(super) fn apply_file_choice(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        choice: crate::file_menu::FileChoice,
    ) {
        use crate::file_menu::FileChoice;
        match choice {
            FileChoice::Open => {
                self.invoke_effect(&crate::effects::WordProcessorEffect::OpenDocument {
                    path: None,
                });
            }
            FileChoice::Save => {
                self.invoke_effect(&crate::effects::WordProcessorEffect::Save { path: None });
            }
            FileChoice::SaveAs => {
                self.invoke_effect(&crate::effects::WordProcessorEffect::SaveAs { path: None });
            }
            FileChoice::Template(template) => self.apply_template(state, template),
        }
    }

    pub(super) fn has_words(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| !block.text.trim().is_empty())
    }

    pub(super) fn floating_viewport(&self) -> Rect {
        self.rect
            .inset(-f64::from(self.desk_theme.tokens.spacing.s2))
    }

    /// Replace the content with the template in place, keeping the host state, its undo history
    pub(super) fn apply_template(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        template: crate::Template,
    ) {
        self.replace_body_via_action(state, template.content());
        self.path = None;
        self.kept = None;
        self.format = Some(WpFormat::Markdown);
        self.scroll_y = 0.0;
        self.refresh_blocks(state);
        self.layout_cache.clear();
        self.recent_layout_cache.clear();
        self.spell_cache.clear();
        self.lifecycle.dirty = true;
    }

    #[cfg(test)]
    pub(super) fn context_menu_labels(&self) -> Vec<String> {
        if !self.context_menu.is_open() {
            return Vec::new();
        }
        self.context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(str::to_string))
            .collect()
    }

    pub(super) fn status_readouts(&self) -> Vec<String> {
        let words: usize = self
            .blocks
            .iter()
            .map(|block| block.text.split_whitespace().count())
            .sum();
        let chars: usize = self
            .blocks
            .iter()
            .map(|block| block.text.chars().count())
            .sum();
        // The page readout is drawn last, so it survives a band too narrow for all three.
        vec![
            t_args("status-words", &[arg("words", num::i64_of_usize(words))]),
            t_args("status-chars", &[arg("chars", num::i64_of_usize(chars))]),
            self.page_readout(),
        ]
    }

    pub(super) fn route_hosted_fields(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let pointer = match event {
            WindowEvent::CursorMoved { position, .. } => ctx.logical(position),
            _ => self.pointer,
        };
        let mut took = false;
        let panel = self.styles_panel_visible();
        let open_menu = [&mut self.style_pick, &mut self.line_spacing]
            .into_iter()
            .find(|menu| panel && menu.is_open())
            .map(|menu| Self::forward(menu, event, ctx));
        if open_menu.is_some() {
            self.drain_hosted_fields(&mut ctx.state);
            *ctx.handled = true;
            self.lifecycle.dirty = true;
            return true;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                if panel {
                    for slot in &mut self.num_inputs {
                        Self::forward(&mut slot.input, event, ctx);
                    }
                    Self::forward(&mut self.line_spacing, event, ctx);
                    Self::forward(&mut self.style_pick, event, ctx);
                }
                Self::forward(&mut self.zoom_input, event, ctx);
                if *ctx.handled {
                    self.lifecycle.dirty = true;
                }
                *ctx.handled = false;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => took = self.press_hosted(pointer, panel, event, ctx),
            WindowEvent::MouseInput { .. } => {
                for slot in &mut self.num_inputs {
                    if Widget::wants_pointer_capture(&slot.input) {
                        Self::forward(&mut slot.input, event, ctx);
                        took = true;
                    }
                }
                if Widget::wants_pointer_capture(&self.zoom_input) {
                    Self::forward(&mut self.zoom_input, event, ctx);
                    took = true;
                }
                for menu in [&mut self.line_spacing, &mut self.style_pick] {
                    if Widget::wants_pointer_capture(menu) {
                        Self::forward(menu, event, ctx);
                        took = true;
                    }
                }
            }
            WindowEvent::KeyboardInput { event: key, .. } => {
                let done = key.state == ElementState::Pressed
                    && matches!(
                        key.logical_key,
                        Key::Named(NamedKey::Enter | NamedKey::Escape | NamedKey::Tab)
                    );
                took = self.type_into_hosted(event, ctx);
                if took && done {
                    self.focus_hosted(None, false);
                }
            }
            WindowEvent::Ime(_) => took = self.type_into_hosted(event, ctx),
            _ => {}
        }
        self.drain_hosted_fields(&mut ctx.state);
        if took {
            *ctx.handled = true;
            self.lifecycle.dirty = true;
        }
        took
    }

    fn press_hosted(
        &mut self,
        pointer: Point,
        panel: bool,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let field = panel
            .then(|| {
                self.num_inputs
                    .iter()
                    .find(|slot| slot.rect.contains(pointer))
                    .map(|slot| slot.field)
            })
            .flatten();
        let zoom = !self.styles_panel_full_window() && self.zoom_rect.contains(pointer);
        self.focus_hosted(field, zoom);
        if let Some(field) = field {
            if let Some(slot) = self.num_inputs.iter_mut().find(|s| s.field == field) {
                Self::forward(&mut slot.input, event, ctx);
            }
        } else if zoom {
            Self::forward(&mut self.zoom_input, event, ctx);
        } else if panel && self.line_spacing_rect.contains(pointer) {
            Self::forward(&mut self.line_spacing, event, ctx);
        } else if panel && self.style_pick_rect.contains(pointer) {
            Self::forward(&mut self.style_pick, event, ctx);
        } else {
            return false;
        }
        true
    }

    fn type_into_hosted(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let foreground = self.styles_panel_full_window();
        let body = self.panel_body;
        if let Some(field) = self.editing_field {
            let Some(slot) = self.num_inputs.iter_mut().find(|slot| {
                let visible = slot.rect.intersect(body);
                slot.field == field
                    && (!foreground || (visible.width() > 0.0 && visible.height() > 0.0))
            }) else {
                return false;
            };
            Self::forward(&mut slot.input, event, ctx);
            true
        } else if foreground
            && self.visible_styles_rect(self.line_spacing_rect).is_some()
            && Widget::is_focused(&self.line_spacing)
        {
            Self::forward(&mut self.line_spacing, event, ctx);
            true
        } else if foreground
            && self.visible_styles_rect(self.style_pick_rect).is_some()
            && Widget::is_focused(&self.style_pick)
        {
            Self::forward(&mut self.style_pick, event, ctx);
            true
        } else if !foreground && Widget::is_focused(&self.zoom_input) {
            Self::forward(&mut self.zoom_input, event, ctx);
            true
        } else {
            false
        }
    }

    pub(super) fn focus_hosted(&mut self, field: Option<NumField>, zoom: bool) {
        for slot in &mut self.num_inputs {
            let on = Some(slot.field) == field;
            if Widget::is_focused(&slot.input) != on {
                Widget::set_focus(&mut slot.input, on);
            }
        }
        if Widget::is_focused(&self.zoom_input) != zoom {
            Widget::set_focus(&mut self.zoom_input, zoom);
        }
        self.editing_field = field;
    }

    fn forward(widget: &mut dyn Widget, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        widget.handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
    }

    pub(super) fn drain_hosted_fields(&mut self, state: &mut ui::widget::Edits<'_>) {
        let changes: Vec<(NumField, f64)> = self
            .num_inputs
            .iter_mut()
            .filter_map(|slot| slot.input.take_changed().map(|v| (slot.field, v)))
            .collect();
        for (field, value) in changes {
            self.edit_focused_style(state, true, |st| field.set(st, num::f32_of_f64(value)));
        }
        // A value the panel did not put there is a pick the person made.
        let picked = self.line_spacing.selected().to_owned();
        if picked != self.line_spacing_shown {
            let spacing = block_layout::LINE_SPACINGS
                .iter()
                .find(|spacing| line_spacing_label(**spacing) == picked)
                .copied();
            if let Some(spacing) = spacing {
                self.dispatch_block_attr(
                    state,
                    "line_spacing",
                    crate::actions::AttrVal::Float(spacing),
                );
                self.refresh_blocks(state);
                self.lifecycle.dirty = true;
            }
            self.line_spacing_shown = picked;
        }
        let picked = self.style_pick.selected().to_owned();
        if picked != self.style_pick_shown {
            self.apply_panel_hit(state, PanelHit::Pick(picked.clone()));
            self.style_pick_shown = picked;
        }
        if let Some(percent) = self.zoom_input.take_changed() {
            self.set_zoom(percent / 100.0);
        }
    }

    pub(super) fn draw_drawers(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.outline_drawer.sidebar_rect().width() > 0.5 {
            let local = self.outline_drawer.paint_panel(scene, transform, theme);
            let entries = self.outline_entries();
            let active = self.active_outline_block();
            self.outline.draw(
                &mut PaintContext::new(scene, transform, text_renderer),
                &local,
                self.outline_drawer.sidebar_rect(),
                &entries,
                active.as_deref(),
            );
        } else {
            self.outline.clear_frame();
        }
        if self.styles_panel_full_window() {
            ui::floating_surface::paint_material(
                scene,
                transform,
                &self.rect,
                ui::floating_surface::Material::Opaque,
                theme,
            );
            let local = theme.on_ground(ui::chrome::panel_ground(theme));
            self.draw_styles_panel(scene, transform, text_renderer, &local, self.rect);
        } else if self.inspector_drawer.sidebar_rect().width() > 0.5 {
            let local = self.inspector_drawer.paint_panel(scene, transform, theme);
            let panel = self.inspector_drawer.sidebar_rect();
            self.draw_styles_panel(scene, transform, text_renderer, &local, panel);
        } else {
            self.panel_rect = Rect::ZERO;
            self.panel_body = Rect::ZERO;
            self.panel_hits.clear();
            for slot in &mut self.num_inputs {
                slot.rect = Rect::ZERO;
            }
            self.line_spacing_rect = Rect::ZERO;
            self.style_pick_rect = Rect::ZERO;
        }
    }

    pub(super) fn resolve_drawers(&mut self, theme: &Theme, below: Rect) -> Rect {
        for (drawer, open) in [
            (&mut self.outline_drawer, self.outline.is_open()),
            (&mut self.inspector_drawer, self.view.styles_panel_visible),
        ] {
            drawer.scale_for(theme);
            drawer.resolve(below);
            if drawer.is_open() != open {
                drawer.snap(open);
            }
            drawer.tick(theme);
        }
        let column = self.outline_drawer.resolve(below);
        self.inspector_drawer.resolve(column)
    }

    pub(super) fn route_styles_page_key(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.styles_panel_full_window() {
            return false;
        }
        match event {
            // Only application file commands may leave the foreground page.
            WindowEvent::KeyboardInput { event: key, .. }
                if (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
                    && matches!(&key.logical_key, Key::Character(key)
                        if key.eq_ignore_ascii_case("s") || key.eq_ignore_ascii_case("o")) =>
            {
                false
            }
            WindowEvent::KeyboardInput { event: key, .. } => {
                if key.state == ElementState::Pressed
                    && key.logical_key == Key::Named(NamedKey::Escape)
                {
                    self.apply_panel_hit(&mut ctx.state, PanelHit::Close);
                }
                *ctx.handled = true;
                true
            }
            WindowEvent::Ime(_)
            | WindowEvent::DroppedFile(_)
            | WindowEvent::HoveredFile(_)
            | WindowEvent::HoveredFileCancelled
            | WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                *ctx.handled = true;
                true
            }
            _ => false,
        }
    }

    pub(super) fn tool_clicked(&mut self, state: &mut ui::widget::Edits<'_>, action: ToolAction) {
        self.apply_tool(state, action);
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    pub(super) fn current_text_color(&self) -> Option<[u8; 3]> {
        let (b, pos) = self.font_probe()?;
        b.marks
            .iter()
            .find(|m| m.name == "color" && pos >= m.start && pos < m.end)
            .and_then(|m| m.value.as_str())
            .and_then(parse_hex_rgb)
    }
}

pub(super) fn font_value(family: FontFamily) -> &'static str {
    match family {
        FontFamily::Sans => "sans",
        FontFamily::Serif => "serif",
        FontFamily::Mono => "mono",
    }
}

fn font_family_of(value: &str) -> FontFamily {
    match value {
        "serif" => FontFamily::Serif,
        "mono" => FontFamily::Mono,
        _ => FontFamily::Sans,
    }
}

pub(super) fn font_label(family: FontFamily) -> String {
    t(match family {
        FontFamily::Sans => "toolbar-font-sans",
        FontFamily::Serif => "toolbar-font-serif",
        FontFamily::Mono => "toolbar-font-mono",
    })
}
