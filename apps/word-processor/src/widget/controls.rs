//! The editor's controls: the toolbar and its drawers, formatting tools, panel
//! fields, pickers and palettes, the context menu and the clipboard.

use super::block_layout::line_spacing as block_line_spacing;
use super::image_layout::{anchor as image_anchor, wrap as image_wrap};
use super::text_runs::{char_font_family, char_font_size, char_highlight, parse_hex_rgb};
use super::toolbar::{HIGHLIGHT_PALETTE, STYLE_PALETTE, ToolAction, ToolEntry};
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
use ui::richtext::FontFamily;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{SegmentedTouchGroup, SegmentedTouchResult};
use ui::{PresenceCtx, clipboard_paste};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::accessibility::tool_action_id;
use super::panels::swatch_names;
use super::{CtxAction, NumField, PanelHit, WordProcessorWidget, block_layout, text_runs};

impl WordProcessorWidget {
    fn block_index_cached(&self, block: &str) -> Option<usize> {
        self.blocks.iter().position(|b| b.id == block)
    }

    /// Selection endpoints in document order, computed from the block cache
    /// (so it works during `draw`, which has no `state`).
    fn selection_span_cached(&self) -> Option<(Caret, Caret)> {
        crate::word_processor::ordered_selection_span(
            self.inner.selection_anchor.as_ref(),
            &self.inner.caret,
            |id| self.block_index_cached(id),
        )
    }

    /// Per-block selected char ranges spanning the selection (cache-based).
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

    fn current_block_style(&self) -> (String, Option<i64>) {
        let b = self.blocks.iter().find(|b| b.id == self.inner.caret.block);
        let kind = b.map_or_else(|| "paragraph".into(), |b| b.kind.clone());
        let level = b.and_then(|b| b.int_attr("level"));
        (kind, level)
    }

    pub(super) fn apply_tool(&mut self, state: &mut ui::widget::Edits<'_>, action: ToolAction) {
        match action {
            // Every document-mutating format dispatches its matching
            // `RichTextAction` through the shared action path (the same
            // EncodedAction the Ctrl+K palette dispatches), so it replays
            // byte-identically and lands as one undo boundary.
            ToolAction::Bold => self.dispatch_toggle_mark(state, "strong"),
            ToolAction::Italic => self.dispatch_toggle_mark(state, "em"),
            ToolAction::Underline => self.dispatch_toggle_mark(state, "underline"),
            ToolAction::Strike => self.dispatch_toggle_mark(state, "strike"),
            ToolAction::TextColor(Some(c)) => self.dispatch_set_mark_value(
                state,
                "color",
                crate::actions::MarkVal::Str(crate::styles::color_to_hex(c)),
            ),
            // The neutral swatch clears any per-run colour, reverting to the
            // block's style ink.
            ToolAction::TextColor(None) => self.dispatch_clear_mark(state, "color"),
            ToolAction::Highlight(Some(c)) => self.dispatch_set_mark_value(
                state,
                "highlight",
                crate::actions::MarkVal::Str(crate::styles::color_to_hex(c)),
            ),
            // The "no highlight" swatch clears any per-run highlight band.
            ToolAction::Highlight(None) => self.dispatch_clear_mark(state, "highlight"),
            ToolAction::Super => self.dispatch_exclusive_mark(state, "super", "sub"),
            ToolAction::Sub => self.dispatch_exclusive_mark(state, "sub", "super"),
            ToolAction::Hyperlink => {
                if !self.selection_blocks_cached().is_empty() {
                    self.action_palette_request = Some(tilo_actions::ActionAddress::new(
                        "word-processor.caret",
                        "set-hyperlink",
                    ));
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
            ToolAction::FontCycle => {
                let name = match self.current_font().next() {
                    FontFamily::Sans => "sans",
                    FontFamily::Serif => "serif",
                    FontFamily::Mono => "mono",
                };
                self.dispatch_set_mark_value(
                    state,
                    "font",
                    crate::actions::MarkVal::Str(name.to_string()),
                );
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
            ToolAction::NewMenu => self.open_template_picker(),
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

    /// Open the toolbar's More menu, as a person choosing it would.
    pub(super) fn open_toolbar_overflow(&mut self) {
        let _ = Widget::perform_semantic_action(
            &mut self.tool_bar,
            "toolbar.overflow",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        );
        self.lifecycle.dirty = true;
    }

    /// Mutate the focused block's style in the live widget sheet (component-
    /// instance preview), then optionally persist it to the document.
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

    /// Dispatch a click on a styles-panel target.
    pub(super) fn apply_panel_hit(&mut self, state: &mut ui::widget::Edits<'_>, hit: PanelHit) {
        match hit {
            PanelHit::Close => {
                self.view.styles_panel_visible = false;
                self.focus_hosted(None, false);
                self.lifecycle.dirty = true;
            }
            PanelHit::Pick(id) => {
                // A built-in style implies a structural kind, applied together as one
                // action/undo boundary: picking "Heading 1" yields a *real* heading
                // (present in outline/TOC), "List Item" a real list item — not merely
                // styled body text. Custom styles are pure visual presets; kind stays untouched.
                self.dispatch_apply_named_style(state, &id);
                self.refresh_blocks(state);
                self.lifecycle.dirty = true;
            }
            PanelHit::SetFont(font) => {
                self.edit_focused_style(state, true, |st| st.font = font.into());
            }
            PanelHit::ToggleBold => self.edit_focused_style(state, true, |st| st.bold = !st.bold),
            PanelHit::ToggleItalic => {
                self.edit_focused_style(state, true, |st| st.italic = !st.italic);
            }
            // Direct paragraph formatting on the current paragraph(s), not the
            // named style — only the panel exposes these, not the toolbar.
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

    /// `PanelHit::NewStyle`: duplicate the focused style under a fresh id.
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

    /// `PanelHit::ImageScale`: scale the *current on-screen* size (aspect
    /// locked) so repeated presses compound predictably, seeding from the
    /// live doc size when set, else the laid display size.
    fn apply_panel_hit_image_scale(&mut self, state: &mut ui::widget::Edits<'_>, factor: f64) {
        let Some(id) = self.selected_image_id() else {
            return;
        };
        let (cw, ch) = self
            .inner
            .image_size(state, &id)
            .or_else(|| self.selected_image_dims())
            .unwrap_or((240.0, 160.0));
        // Keep width + height in one action/undo boundary. Direct writes here
        // used to make a resize invisible to the action catalogue and could
        // replay as two independently stale attributes.
        let width = num::i64_of_f64((cw * factor).round().clamp(16.0, 4_000.0));
        let height = num::i64_of_f64((ch * factor).round().clamp(16.0, 4_000.0));
        self.perform_action(
            state,
            &crate::actions::set_image_size_action(&id, width, height),
        );
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    /// `PanelHit::ImageFlipH`/`ImageFlipV`: mirror the selected image about
    /// its horizontal (`horizontal`) or vertical axis.
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

    /// Crop or reset the selected image through one atomic action. The panel and
    /// Ctrl+K image resource intentionally calculate exactly the same 5% inset.
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
            // Preserve a visible 5% source window; at the limit the control is
            // a harmless no-op rather than writing an invalid crop.
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

    /// Choose one of the image's canonical placement modes. The free mode is
    /// page-relative and therefore does not participate in text flow; the two
    /// wrap modes preserve paragraph-relative flow.
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

    // ---- colour picker popover --------------------------------------------

    /// Open the shared [`ColorPicker`] seeded with the focused style's colour.
    pub(super) fn open_color_picker(&mut self) {
        let seed = self
            .focused_style_key()
            .map_or([0x1c, 0x1c, 0x20], |k| self.sheet.get(&k).color);
        self.color_picker.open(seed, self.pointer);
        self.lifecycle.dirty = true;
    }

    /// While the colour picker is open, route input to it (drag the SV square /
    /// hue strip / RGB sliders); an outside press or Escape closes it. A committed
    /// colour is applied live to the focused style. Returns whether the event was
    /// consumed (so the caller stops further dispatch).
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

    /// Open the compact toolbar's shared swatch grid. The labels describe the
    /// *formatting effect* rather than forcing assistive technology users to
    /// infer it from a hexadecimal value alone.
    pub(super) fn open_compact_swatch_palette(&mut self, kind: PaletteKind) {
        self.context_menu.close();
        self.style_menu.close();
        self.template_picker.close();
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
        let command = match kind {
            PaletteKind::Text => ToolAction::TextColorPalette,
            PaletteKind::Highlight => ToolAction::HighlightPalette,
        };
        let anchor = self
            .toolbar
            .iter()
            .find(|(action, _)| *action == command)
            .map_or(self.pointer, |(_, rect)| {
                Point::new(rect.x0, self.tool_band.y1)
            });
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

    /// Route compact swatch-grid input ahead of the page. A picked colour still
    /// uses the exact `ToolAction` path as the desktop toolbar, preserving one
    /// mutation/action implementation and one collaboration boundary.
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

    /// Commit any compact-palette route through the same canonical formatting
    /// actions as its pointer, keyboard, and assistive-technology callers.
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

    /// The next/previous cell id within the caret cell's table, if any.
    /// The char position the toolbar reports font state for: just left of the
    /// caret (so it reflects the run you're typing into), else the caret.
    fn font_probe(&self) -> Option<(&BlockView, usize)> {
        let b = self
            .blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)?;
        let pos = self.inner.caret.offset.saturating_sub(1);
        Some((b, pos))
    }

    fn current_font(&self) -> FontFamily {
        self.font_probe()
            .and_then(|(b, pos)| char_font_family(pos, &b.marks))
            .unwrap_or_default()
    }

    /// The effective font size at the selection: the per-run `size` character
    /// mark under the caret if present, else the resolved paragraph style's
    /// size. Seeds the toolbar's A− / A+ nudges (which write a per-run mark).
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

    pub(super) fn segmented_rects(&self) -> [Vec<Rect>; 3] {
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
        let font = self
            .panel_hits
            .iter()
            .filter_map(|(rect, hit)| matches!(hit, PanelHit::SetFont(_)).then_some(*rect))
            .collect();
        [align, placement, font]
    }

    pub(super) fn handle_segmented_touch(
        &mut self,
        touch: &winit::event::Touch,
        scale: f64,
        edits: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let [align, placement, font] = self.segmented_rects();
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
        let selected_font = self.focused_style_key().map_or(0, |key| {
            ["sans", "serif", "mono"]
                .iter()
                .position(|value| *value == self.sheet.get(&key).font)
                .unwrap_or(0)
        });
        let groups = [
            SegmentedTouchGroup::new(0, &align, selected_align),
            SegmentedTouchGroup::new(1, &placement, selected_placement),
            SegmentedTouchGroup::new(2, &font, selected_font),
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
                2 => ["sans", "serif", "mono"]
                    .get(index)
                    .copied()
                    .map(PanelHit::SetFont),
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

    /// The caret block's line-spacing multiplier (from the cached blocks), or 1.0.
    /// Drives the toolbar chip label.
    pub(super) fn current_line_spacing(&self) -> f64 {
        self.blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)
            .map_or(1.0, |b| f64::from(block_line_spacing(b)))
    }

    /// The per-run `highlight` colour covering the font-probe position (just left
    /// of the caret), if any — used to ring the active highlight swatch.
    pub(super) fn current_highlight_color(&self) -> Option<[u8; 3]> {
        let (b, pos) = self.font_probe()?;
        char_highlight(pos, &b.marks)
    }

    pub(super) fn clipboard_copy(&mut self, state: &AutoCommit) -> bool {
        if !self.inner.has_selection() {
            return false;
        }
        let (plain, blocks) = self.inner.copy_fragment(state);
        // A copy still waiting on the compositor counts as taken: a cut never
        // waits on it, and undo brings the text back if it fails after all.
        if ui::text_selection::try_clipboard_copy(&plain).outcome() == Some(false) {
            return false;
        }
        self.rich_clip = Some((plain, blocks));
        true
    }

    /// Paste with formatting when the system clipboard still holds our last rich
    /// copy; otherwise paste the clipboard's plain text (e.g. from another app).
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
        // The published `paste` primitive; a paste beyond its per-call text
        // bound (an agent limit, not a person's) keeps the direct fragment.
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

    /// Paste plain text only (Ctrl+Shift+V), stripping all formatting.
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

    // ---- right-click context menu ------------------------------------------

    /// Char span `(start, end)` of the word at char-offset `off` in `text`
    /// (whitespace-delimited, matching `WordProcessor::select_word`). `start ==
    /// end` means the offset sits on whitespace (no word).
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

    /// If the word at block-char-span `(s, e)` is flagged misspelled, return its
    /// up-to-`max` spelling suggestions. Empty vec when the word is fine. Uses a
    /// fresh `check` over just the word so it reflects the **current** dictionary
    /// (including freshly-added user words), not the stale per-block cache.
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

    /// Build + open the right-click context menu at screen `pos`. Selects the word
    /// under the click when nothing is selected, then assembles rows: misspelled-word
    /// suggestions + "Add to dictionary" (if flagged) prepended to Cut/Copy/Paste/
    /// Select All. `context_actions` mirrors the rows for left-click dispatch by index.
    pub(super) fn open_context_menu(
        &mut self,
        state: &AutoCommit,
        content_pt: Point,
        pos: [f32; 2],
    ) {
        // Place the caret under the click; select the word when there's no
        // selection so Cut/Copy act on something.
        let selected_ranges = self.inner.selection_blocks(state);
        let previous_caret = self.inner.caret.clone();
        let previous_anchor = self.inner.selection_anchor.clone();
        self.caret_from_pointer(content_pt, false);
        // Raw click char offset (before `select_word` snaps the caret to the word
        // end) — used to test which tracked-change / comment range was clicked.
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

        // Identify the clicked word's block + char span for spell-check actions.
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
        menu.constrain_to(self.rect);
        self.context_menu = menu;
        self.context_actions = actions;
        self.lifecycle.dirty = true;
    }

    /// Context-menu rows for the clicked word's spell-check suggestions (up
    /// to 3) + "Add to dictionary", when spell-check is on and the word is
    /// currently flagged.
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

    /// Context-menu rows for Cut/Copy (when there's a selection), Paste
    /// (when the clipboard holds text) and always Select All.
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

    /// Dispatch the context-menu row at `idx` against the widget, then close the
    /// menu. Separator / out-of-range rows are ignored. Returns `true` when a
    /// row ran.
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
                    // Keep it with the person's settings, then build the
                    // dictionary again so every block re-checks and the
                    // squiggle clears on the next draw.
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
                // One replayable, attributable envelope through the executor
                // (the published apply-spelling-suggestion op builds the same
                // splice), never a direct CRDT write.
                let region = self.inner.region_kind();
                self.splice_via_action(state, region, &block, start, end, &replacement);
                self.refresh_blocks(state);
                // The block text changed; its cache entry is stale.
                self.spell_cache.remove(&block);
            }
            CtxAction::Review(action) => self.dispatch_review_action(action, state),
        }
        self.context_menu.close();
        self.lifecycle.dirty = true;
        true
    }

    // ---- paragraph-style management menu ----------------------------------

    /// The style id of the picker chip under `p` in the styles panel, if any.
    /// Reads the current frame's `panel_hits` (rebuilt every draw), so the chip
    /// rects are live at right-click time.
    pub(super) fn style_chip_at(&self, p: Point) -> Option<String> {
        if self.style_pick_rect.contains(p) {
            return self.focused_style_key();
        }
        self.panel_hits.iter().find_map(|(r, h)| match h {
            PanelHit::Pick(id) if r.contains(p) => Some(id.clone()),
            _ => None,
        })
    }

    /// Apply a typed style-menu outcome. The component owns presentation and
    /// hit-testing; document mutation remains editor policy.
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
                    // Apply the copy to the caret block so the panel focuses it.
                    self.dispatch_apply_named_style(state, &new_id);
                    self.refresh_blocks(state);
                }
            }
        }
        self.lifecycle.dirty = true;
    }

    // ---- new-from-template picker -----------------------------------------

    pub(super) fn request_template(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        template: crate::Template,
    ) {
        self.apply_template(state, template);
    }

    /// Open the "New" picker under the New button (under the toolbar's
    /// leading edge while New sits in the More menu).
    pub(super) fn open_template_picker(&mut self) {
        let gap = f64::from(self.desk_theme.tokens.spacing.s2);
        let anchor = self
            .toolbar
            .iter()
            .find(|(action, _)| *action == ToolAction::NewMenu)
            .map_or_else(
                || {
                    Rect::new(
                        self.tool_band.x0,
                        self.tool_band.y0,
                        self.tool_band.x0,
                        self.tool_band.y1,
                    )
                },
                |(_, rect)| *rect,
            );
        let anchor = Rect::new(anchor.x0, anchor.y0 - gap, anchor.x1, anchor.y1 + gap);
        self.template_picker
            .set_text_scale(crate::num::f32_of_f64(self.text_scale));
        self.template_picker
            .open_anchored(anchor, self.floating_viewport());
        self.lifecycle.dirty = true;
    }

    /// Where menus and pickers may float: the window less a small margin,
    /// so a floating panel never runs flush against the window's edge.
    pub(super) fn floating_viewport(&self) -> Rect {
        self.rect
            .inset(-f64::from(self.desk_theme.tokens.spacing.s2))
    }

    /// Replace the open document's content with `template`'s Markdown, **in place**
    /// on the live `state` (preserving the host `ui::State` and its undo history /
    /// collaboration session), as a new untitled document. Re-seats the caret, refreshes the view, flags an
    /// undo point, and counts as a document-modifying edit (so autosave still
    /// protects the freshly-templated content).
    pub(super) fn apply_template(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        template: crate::Template,
    ) {
        self.replace_body_via_action(state, template.content());
        self.path = None;
        self.format = Some(WpFormat::Markdown);
        self.scroll_y = 0.0;
        self.refresh_blocks(state);
        // The whole body changed; per-block caches keyed by id/text are stale.
        self.layout_cache.clear();
        self.recent_layout_cache.clear();
        self.spell_cache.clear();
        self.lifecycle.dirty = true;
    }

    /// Test-visible labels of the currently-open context menu's rows
    /// (separators omitted), in order. Empty when the menu is closed.
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

    /// The status readouts on the trailing side of the status band.
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
        vec![
            t_args("status-words", &[arg("words", num::i64_of_usize(words))]),
            t_args("status-chars", &[arg("chars", num::i64_of_usize(chars))]),
        ]
    }

    /// Route an event to the fields hosted in the inspector and the status
    /// band (the style property fields, the line-spacing menu and the zoom
    /// field): hover always, a press on one focuses it, typing goes to the
    /// focused one. A value they produce is applied here. Returns whether
    /// one of them took the event.
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
        // An open menu owns every event until it closes.
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
                // Hover is shared with the rest of the editor.
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

    /// A press on a hosted field focuses it and goes to it; returns
    /// whether one took it.
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

    /// Hand typing to the hosted field that has the keyboard; returns
    /// whether one had it.
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

    /// Give keyboard focus to one hosted field (a style property, or the
    /// zoom field), taking it from the others; `None, false` releases it.
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

    /// Hand one event to a hosted widget.
    fn forward(widget: &mut dyn Widget, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        widget.handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
    }

    /// Apply what the hosted fields produced: a style property, a line
    /// spacing, or a zoom level.
    pub(super) fn drain_hosted_fields(&mut self, state: &mut ui::widget::Edits<'_>) {
        let changes: Vec<(NumField, f64)> = self
            .num_inputs
            .iter_mut()
            .filter_map(|slot| slot.input.take_changed().map(|v| (slot.field, v)))
            .collect();
        for (field, value) in changes {
            self.edit_focused_style(state, true, |st| field.set(st, num::f32_of_f64(value)));
        }
        if let Some(index) = self.line_spacing_pick.take()
            && let Some(spacing) = block_layout::LINE_SPACINGS.get(index).copied()
        {
            self.dispatch_block_attr(
                state,
                "line_spacing",
                crate::actions::AttrVal::Float(spacing),
            );
            self.refresh_blocks(state);
            self.lifecycle.dirty = true;
        }
        if let Some(id) = self.style_pick_choice.take() {
            self.apply_panel_hit(state, PanelHit::Pick(id));
        }
        if let Some(percent) = self.zoom_input.take_changed() {
            self.set_zoom(percent / 100.0);
        }
    }

    /// The toolbar's commands for this frame, grouped: New; bold, italic
    /// and underline; type and colour; the rarer character formats;
    /// insertion (the table group turns into
    /// table editing inside a cell); then, at the trailing edge, the view
    /// toggles. Commands that do not fit go to the toolbar's More menu.
    pub(super) fn toolbar_entries(&self) -> Vec<ToolEntry> {
        let mut entries = vec![
            ToolEntry::command(ToolAction::NewMenu, t("toolbar-new")),
            ToolEntry::Separator,
            ToolEntry::toggle(
                ToolAction::Bold,
                t("toolbar-bold"),
                self.mark_active_in_selection("strong"),
            ),
            ToolEntry::toggle(
                ToolAction::Italic,
                t("toolbar-italic"),
                self.mark_active_in_selection("em"),
            ),
            ToolEntry::toggle(
                ToolAction::Underline,
                t("toolbar-underline"),
                self.mark_active_in_selection("underline"),
            ),
            ToolEntry::Separator,
            ToolEntry::command(ToolAction::FontCycle, self.current_font().label()),
            ToolEntry::command(ToolAction::FontSizeDec, t("toolbar-size-down")),
            ToolEntry::command(ToolAction::FontSizeInc, t("toolbar-size-up")),
            ToolEntry::command(ToolAction::TextColorPalette, t("toolbar-text-color")),
            ToolEntry::command(ToolAction::HighlightPalette, t("toolbar-highlight")),
            ToolEntry::Separator,
            ToolEntry::toggle(
                ToolAction::Strike,
                t("toolbar-strikethrough"),
                self.mark_active_in_selection("strike"),
            ),
            ToolEntry::toggle(
                ToolAction::Super,
                t("toolbar-superscript"),
                self.mark_active_in_selection("super"),
            ),
            ToolEntry::toggle(
                ToolAction::Sub,
                t("toolbar-subscript"),
                self.mark_active_in_selection("sub"),
            ),
            ToolEntry::Separator,
        ];
        if !self.selection_blocks_cached().is_empty() {
            entries.push(ToolEntry::command(ToolAction::Hyperlink, t("toolbar-link")));
        }
        if self.current_block_style().0 == "cell" {
            entries.extend([
                ToolEntry::command(ToolAction::TableRowAdd, t("toolbar-row-add")),
                ToolEntry::command(ToolAction::TableRowDel, t("toolbar-row-del")),
                ToolEntry::command(ToolAction::TableColAdd, t("toolbar-col-add")),
                ToolEntry::command(ToolAction::TableColDel, t("toolbar-col-del")),
                ToolEntry::command(ToolAction::TableDelete, t("toolbar-table-delete")),
            ]);
        } else {
            entries.push(ToolEntry::command(
                ToolAction::InsertTable,
                t("toolbar-table"),
            ));
        }
        entries.extend([
            ToolEntry::command(ToolAction::InsertImage, t("toolbar-image")),
            ToolEntry::command(ToolAction::InsertToc, t("toolbar-toc")),
            ToolEntry::Spacer,
            ToolEntry::toggle(
                ToolAction::ToggleRuler,
                t("toolbar-ruler"),
                self.view.ruler_visible,
            ),
        ]);
        entries
    }

    /// Bring the hosted toolbar in line with this frame's commands: rebuilt
    /// when a command appears, goes or is renamed, otherwise only its toggles
    /// are set, so hover, focus and an open More menu survive.
    pub(super) fn sync_toolbar(&mut self) {
        let entries = self.toolbar_entries();
        let same = entries.len() == self.tool_entries.len()
            && entries
                .iter()
                .zip(&self.tool_entries)
                .all(|(a, b)| a.same_shape(b));
        if same {
            for (index, (now, before)) in entries.iter().zip(&self.tool_entries).enumerate() {
                if let (
                    ToolEntry::Command {
                        active: Some(on), ..
                    },
                    ToolEntry::Command {
                        active: Some(was), ..
                    },
                ) = (now, before)
                    && on != was
                {
                    self.tool_bar.set_active(index, *on);
                }
            }
        } else {
            let mut bar = ui::widget::Toolbar::new(ui::widget::toolbar::Orientation::Horizontal);
            for entry in &entries {
                bar = match entry {
                    ToolEntry::Command {
                        action,
                        label,
                        active,
                    } => {
                        let fired = std::rc::Rc::clone(&self.tool_fired);
                        let action = *action;
                        let on_click = move || fired.set(Some(action));
                        let command = match (action.icon(), active) {
                            (Some(icon), Some(on)) => {
                                bar.push_icon_toggle(icon, label, *on, on_click)
                            }
                            (Some(icon), None) => bar.push_icon(icon, label, on_click),
                            (None, Some(on)) => bar.push_toggle(label, *on, on_click),
                            (None, None) => bar.push_button(label, on_click),
                        };
                        let id = format!("word.toolbar.{}", tool_action_id(action));
                        // Admission runs the control callback; settlement submits
                        // document edits through their existing typed dispatcher.
                        if action.edits_document() || action == ToolAction::InsertImage {
                            command.with_effect(&id)
                        } else {
                            command.with_view(&id)
                        }
                    }
                    ToolEntry::Separator => bar.push_separator(),
                    ToolEntry::Spacer => bar.push_spacer(),
                };
            }
            self.tool_bar = bar;
            self.retained_tree_stale = true;
        }
        self.tool_entries = entries;
    }

    /// Paint the outline and the inspector on their drawers: the region
    /// beside the page when docked, the floating material over a scrim when
    /// a drawer covers the page. The outline is drawn after `self.laid` is
    /// set so its current heading reflects this frame's pagination.
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
            // A compact inspector is a page, not floating chrome. Use the
            // same material and breakpoint as shared full-window dialogs.
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

    /// Bring the outline and inspector drawers in line with what is shown,
    /// advance their reveal, and lay them out in `below` (everything under
    /// the toolbar). Returns the page's column.
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

    /// The toolbar band's height: the window's header row.
    pub(super) fn toolbar_band_height(theme: &Theme) -> f64 {
        ui::chrome::HeaderRowFace::height(theme)
    }

    /// Lay out and paint the toolbar in its band `tb` (the band has no fill
    /// of its own, only a hairline under it), then record each visible
    /// command's rect for hit-testing and the accessibility tree.
    pub(super) fn draw_toolbar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        tb: Rect,
    ) {
        self.sync_toolbar();
        ui::widget::toolbar::paint_band(scene, transform, theme, tb);
        // The band is the window's header row: the outline's toggle at the
        // leading corner, the window's History control (when offered) at the
        // trailing one with the inspector's toggle before it, the commands
        // between them.
        let leading = self.outline_drawer.header(tb, theme);
        let trailing =
            self.inspector_drawer
                .header_with_history(tb, theme, self.window_history.is_shown());
        self.outline_toggle = self.outline_drawer.toggle_rect();
        self.inspector_toggle = self.inspector_drawer.toggle_rect();
        self.window_history_rect = trailing.history_rect();
        self.outline_drawer
            .paint_toggle(scene, transform, tr, theme);
        self.inspector_drawer
            .paint_toggle(scene, transform, tr, theme);
        if let Some(button) = self.window_history.button_mut() {
            button.apply_layout_external(self.window_history_rect);
            let presence = PresenceCtx::empty();
            Widget::draw(button, scene, transform, tr, theme, &presence);
        }
        let chip = leading.metrics().chip;
        let y0 = tb.center().y - chip / 2.0;
        let inner = Rect::new(
            leading.actions.x0,
            y0,
            trailing.actions.x1.max(leading.actions.x0),
            y0 + chip,
        );
        self.tool_bar.lay_out_in(inner, tr, theme);
        let presence = PresenceCtx::empty();
        presence.viewport.set(Some(self.floating_viewport()));
        Widget::draw(&mut self.tool_bar, scene, transform, tr, theme, &presence);
        self.toolbar.clear();
        self.toolbar
            .push((ToolAction::ToggleOutline, self.outline_toggle));
        let mut target = 0usize;
        for entry in self.tool_entries.iter().take(self.tool_bar.visible_count()) {
            if let ToolEntry::Command { action, .. } = entry {
                if let Some(rect) = self.tool_bar.synthetic_accessibility_rect(&[target]) {
                    self.toolbar.push((*action, rect));
                }
                target += 1;
            }
        }
        self.toolbar
            .push((ToolAction::ToggleStyles, self.inspector_toggle));
    }

    /// Hosted controls receive input first; the foreground Styles page
    /// consumes remaining text instead of editing the displaced document.
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
            // Formatting, clipboard and deletion shortcuts belong to its
            // hosted fields (which have already received the event).
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

    /// Route an event to the toolbar: hover always, presses inside its band,
    /// the release that completes one, and everything while its More menu is
    /// open. A command it fires runs here. Returns whether the toolbar took
    /// the event.
    pub(super) fn route_toolbar_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let pointer = match event {
            WindowEvent::CursorMoved { position, .. } => Some(ctx.logical(position)),
            _ => None,
        };
        // The window's History control and the drawers' toggles answer for
        // their own places in the row.
        if let Some(button) = self.window_history.button_mut() {
            let mut handled = false;
            button.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if button.needs_redraw() {
                self.lifecycle.dirty = true;
            }
            if handled && !matches!(event, WindowEvent::CursorMoved { .. }) {
                *ctx.handled = true;
                return true;
            }
        }
        if matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::CursorLeft { .. }
        ) {
            self.outline_drawer.handle_event(event, ctx);
            self.inspector_drawer.handle_event(event, ctx);
        }
        let menu_open = self.tool_bar.is_overflow_open();
        let in_band = pointer.unwrap_or(self.pointer);
        let claims = menu_open
            || match event {
                WindowEvent::CursorMoved { .. } => true,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } => self.tool_band.contains(in_band),
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } => self.tool_bar.wants_pointer_capture(),
                _ => false,
            };
        if !claims {
            return false;
        }
        if !menu_open
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            )
        {
            let toggle = if self.outline_toggle.contains(in_band) {
                Some(ToolAction::ToggleOutline)
            } else if self.inspector_toggle.contains(in_band) {
                Some(ToolAction::ToggleStyles)
            } else {
                None
            };
            if let Some(action) = toggle {
                self.tool_clicked(&mut ctx.state, action);
                *ctx.handled = true;
                return true;
            }
        }
        let mut handled = false;
        Widget::handle_event(
            &mut self.tool_bar,
            event,
            &mut ui::widget::EventCtx {
                handled: &mut handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if handled
            || (matches!(event, WindowEvent::CursorMoved { .. })
                && (self.outline_toggle.contains(in_band)
                    || self.inspector_toggle.contains(in_band)))
        {
            self.lifecycle.dirty = true;
        }
        if let Some(action) = self.tool_fired.take() {
            self.tool_clicked(&mut ctx.state, action);
        }
        // Hover is shared with the rest of the editor; everything else the
        // toolbar takes is its own.
        if matches!(event, WindowEvent::CursorMoved { .. }) && !menu_open {
            return false;
        }
        *ctx.handled = true;
        true
    }

    /// Run a toolbar command chosen by pointer, keyboard or the More menu.
    pub(super) fn tool_clicked(&mut self, state: &mut ui::widget::Edits<'_>, action: ToolAction) {
        self.apply_tool(state, action);
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
        if action.edits_document() {}
    }

    /// The per-run `color` covering the font-probe position (just left of the
    /// caret), if any — used to ring the active swatch.
    pub(super) fn current_text_color(&self) -> Option<[u8; 3]> {
        let (b, pos) = self.font_probe()?;
        b.marks
            .iter()
            .find(|m| m.name == "color" && pos >= m.start && pos < m.end)
            .and_then(|m| m.value.as_str())
            .and_then(parse_hex_rgb)
    }
}
