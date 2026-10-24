//! The page chrome around the body: running header and footer bands, margin
//! guides, the ruler, the layout overlay and bar, the styles panel, the image
//! inspector, the outline and the toast.

use super::block_layout::{align as block_align, line_spacing_label, style_key as block_style_key};
use super::image_layout::{anchor as image_anchor, wrap as image_wrap};
use super::text_runs::{composed_block, running_head_runs, running_head_runs_editing};
use super::toolbar::STYLE_PALETTE;
use crate::compact_swatch_palette::PaletteKind;
use crate::i18n::{arg, t, t_args};
use crate::num;
#[cfg(test)]
use crate::ruler::{Ruler, RulerMarker};
use crate::ruler::{RulerEffect, RulerModel};
use crate::text_offsets::char_to_byte;
use std::collections::HashMap;
use tilo_doc::model::Region;
use tilo_doc::snapshot::AttrVal;
use ui::PresenceCtx;
use ui::icons::Icon;
use ui::paint::with_alpha;
use ui::richtext::{FontFamily, RichTextLayout, TextAlign};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::Widget;
use ui::widget::toast::ToastKind;
use ui::widget::{Button, ButtonFace, ButtonRole, ColorSwatchFace, ControlSize, SegmentedFace};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use super::{
    ACTIVE_BAND_FILL_ALPHA, CARET_WIDTH, DragKind, Edges, GUIDE_GRAB, GuideDrag, LayoutMode,
    NumField, PAPER_INK, PAPER_MUTED, PAPER_SELECTION_ALPHA, PanelCtx, PanelHit, RegionLaid,
    RunningBand, Side, WordProcessorWidget, shown_byte,
};

/// Screen axis of a margin/border guide for `side`: `(is_vertical, coordinate)`.
fn guide_axis(page: Rect, e: Edges, side: Side) -> (bool, f64) {
    match side {
        Side::Left => (true, page.x0 + e.left),
        Side::Right => (true, page.x1 - e.right),
        Side::Top => (false, page.y0 + e.top),
        Side::Bottom => (false, page.y1 - e.bottom),
    }
}

/// New inset for `side` from a pointer position, clamped to a sane range so a
/// margin/border can never invert or swallow the whole page.
fn inset_from_pointer(page: Rect, side: Side, p: Point) -> f64 {
    let raw = match side {
        Side::Left => p.x - page.x0,
        Side::Right => page.x1 - p.x,
        Side::Top => p.y - page.y0,
        Side::Bottom => page.y1 - p.y,
    };
    let max = match side {
        Side::Left | Side::Right => (page.width() / 2.0 - 48.0).max(8.0),
        Side::Top | Side::Bottom => (page.height() / 2.0 - 48.0).max(8.0),
    };
    raw.clamp(4.0, max)
}

/// Apply a guide drag to `e`: Pair = grabbed side + its opposite, Single = just
/// the grabbed side, Proportional = scale all four by the same factor.
fn apply_edges(e: &mut Edges, page: Rect, side: Side, p: Point, kind: DragKind) {
    let v = inset_from_pointer(page, side, p);
    match kind {
        DragKind::Single => e.set(side, v),
        DragKind::Pair => {
            e.set(side, v);
            e.set(side.opposite(), v);
        }
        DragKind::Proportional => {
            let f = v / e.get(side).max(1.0);
            let max_h = (page.width() / 2.0 - 48.0).max(8.0);
            let max_v = (page.height() / 2.0 - 48.0).max(8.0);
            e.left = (e.left * f).clamp(4.0, max_h);
            e.right = (e.right * f).clamp(4.0, max_h);
            e.top = (e.top * f).clamp(4.0, max_v);
            e.bottom = (e.bottom * f).clamp(4.0, max_v);
        }
    }
}

impl WordProcessorWidget {
    /// The selection highlight painted on paper: the paper accent, translucent
    /// so the text reads through it.
    pub(super) fn paper_selection(&self) -> Color {
        with_alpha(self.chrome.paper_accent, PAPER_SELECTION_ALPHA)
    }

    /// Toggle a layout-edit mode from a chip click.
    pub(super) fn toggle_layout_mode(&mut self, mode: LayoutMode) {
        self.layout_mode = if self.layout_mode == mode {
            LayoutMode::None
        } else {
            mode
        };
    }

    /// Persist the current page margins into the document (page setup lives in the
    /// document so it survives save/reopen). Called on margin-guide drag release.
    pub(super) fn persist_margins(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.perform_action(
            state,
            &crate::actions::set_page_margins_action([
                self.margins.top,
                self.margins.right,
                self.margins.bottom,
                self.margins.left,
            ]),
        );
    }

    /// Draw the page header (or footer) blocks for one page sheet inside the given
    /// margin band, anchored to the band's bottom (footers) or top (headers), drawn
    /// through `ctf` so it scales with the page content.
    ///
    /// When `editing` is set, the band is shaped with *raw* (1 char ⇄ 1 caret offset)
    /// runs so click-to-caret lines up with the model, and per-block laid geometry is
    /// returned for the caller to stash. Returns `(drew_anything, laid)`; `laid` is
    /// empty unless `editing`.
    pub(super) fn draw_running_band(
        scene: &mut Scene,
        ctf: Affine,
        tr: &mut TextRenderer,
        band: &RunningBand<'_>,
    ) -> (bool, Vec<RegionLaid>) {
        let RunningBand {
            blocks,
            page_no,
            rect: band,
            is_footer,
            editing,
            composition,
        } = *band;
        if blocks.is_empty() || band.height() <= 2.0 {
            return (false, Vec::new());
        }
        let wrap_w = num::f32_of_f64(band.width().max(40.0));
        // Header/footer run text is a touch smaller and muted, so it reads as
        // chrome rather than body copy (LibreOffice-style running heads).
        let size = 12.0f32;
        // Shape every block first so footers can be bottom-anchored.
        let mut shaped: Vec<RichTextLayout> = Vec::with_capacity(blocks.len());
        let mut total_h = 0.0f64;
        for b in blocks {
            // The block being composed in shapes the composition inside its
            // text, underlined.
            let composing = composition
                .filter(|(id, _)| *id == b.id)
                .map(|(_, core)| core);
            let runs = match composing {
                Some(core) if editing => running_head_runs_editing(&composed_block(b, core)),
                _ if editing => running_head_runs_editing(b),
                _ => running_head_runs(b, page_no),
            };
            let underlines = composing
                .and_then(|core| core.preedit_display_range(&b.text))
                .map(|range| vec![(range.start, range.end)])
                .unwrap_or_default();
            let align = block_align(b, TextAlign::Left);
            let layout = RichTextLayout::shape_styled(
                &mut tr.font_system,
                &runs,
                &RichTextLayout::block_style(false, size, FontFamily::Sans, 1.0),
                Some(wrap_w),
                align,
                underlines,
                vec![],
            );
            total_h += f64::from(layout.height());
            shaped.push(layout);
        }
        // Stack from the top of the band (headers) or just enough above the band
        // bottom to fit the footer's content (footers).
        let mut y = if is_footer {
            (band.y1 - total_h).max(band.y0)
        } else {
            band.y0
        };
        let mut drew = false;
        // Active-context bands draw their ink at full strength (so the editable
        // text reads as live copy); non-active bands stay muted chrome. The
        // editable ink is paper-context (dark), not chrome-context — the band sits
        // on the light paper sheet, so chrome ink (light in dark mode) would be
        // invisible there.
        let ink = if editing { PAPER_INK } else { PAPER_MUTED };
        let mut laid: Vec<RegionLaid> = Vec::new();
        // Consume `shaped` so the (non-Clone) layouts can be *moved* into the laid
        // geometry when editing, instead of cloned.
        for (idx, layout) in shaped.into_iter().enumerate() {
            let origin = Point::new(band.x0, y);
            tr.draw_rich(
                scene,
                &layout,
                ctf * Affine::translate((origin.x, origin.y)),
                ink,
            );
            let h = f64::from(layout.height());
            if editing && let Some(b) = blocks.get(idx) {
                laid.push(RegionLaid {
                    id: b.id.clone(),
                    text: b.text.clone(),
                    origin,
                    layout,
                    composition: composition
                        .filter(|(id, _)| *id == b.id)
                        .map(|(_, core)| core.clone()),
                });
            }
            y += h;
            drew = true;
        }
        (drew, laid)
    }

    /// Draw the "you are editing this band" affordance around the active
    /// header/footer band (Word-style header/footer edit mode): a subtle dashed
    /// accent outline plus a small "Header"/"Footer" tab label at the band's top
    /// edge. Drawn through `ctf` so it tracks the page zoom.
    pub(super) fn draw_active_band_chrome(
        &self,
        scene: &mut Scene,
        ctf: Affine,
        tr: &mut TextRenderer,
        band: Rect,
    ) {
        // A faint fill so the active band reads as the focused surface.
        let pad = Rect::new(band.x0 - 2.0, band.y0 - 1.0, band.x1 + 2.0, band.y1 + 1.0);
        let edge = self.chrome.paper_accent;
        scene.fill(
            Fill::NonZero,
            ctf,
            with_alpha(edge, ACTIVE_BAND_FILL_ALPHA),
            None,
            &pad,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [4.0, 3.0]),
            ctf,
            edge,
            None,
            &pad,
        );
        let label = if self.inner.region() == Region::Header {
            t("band-header")
        } else {
            t("band-footer")
        };
        let style = self.paper_theme.text(TextRole::Caption);
        let (w, h) = tr.measure_style(&label, None, &style);
        let pad_x = f64::from(self.paper_theme.tokens.spacing.s1);
        let tab = Rect::new(
            pad.x0,
            pad.y0 - f64::from(h),
            pad.x0 + f64::from(w) + 2.0 * pad_x,
            pad.y0,
        );
        let radius = f64::from(self.paper_theme.tokens.radii.detail) / 2.0;
        scene.fill(
            Fill::NonZero,
            ctf,
            edge,
            None,
            &vello::kurbo::RoundedRect::from_rect(tab, radius),
        );
        tr.draw_style(
            scene,
            &label,
            self.chrome.on_paper_accent,
            ctf * Affine::translate((tab.x0 + pad_x, tab.y0)),
            None,
            &style,
        );
    }

    /// Render the caret (and any selection highlight) inside the active
    /// header/footer band, using the laid geometry captured in `region_laid`. The
    /// band caret mirrors the body caret (same `caret_geometry` machinery) so it
    /// blinks-free but lands exactly between glyphs.
    pub(super) fn draw_band_caret(&mut self, scene: &mut Scene, ctf: Affine) {
        if !self.lifecycle.focused {
            return;
        }
        let caret = self.inner.caret();
        // Selection highlight (single-band, char range) when present.
        if let Some((s, e)) = self.inner.band_selection_range()
            && let Some(rl) = self.region_laid.iter().find(|r| r.id == s.0)
        {
            let composition = rl.composition.as_ref();
            let sb = shown_byte(composition, &rl.text, char_to_byte(&rl.text, s.1));
            let eb = shown_byte(composition, &rl.text, char_to_byte(&rl.text, e.1));
            for (rx0, ry0, rx1, ry1) in rl.layout.selection_rects(sb, eb) {
                let r = Rect::new(
                    rl.origin.x + f64::from(rx0),
                    rl.origin.y + f64::from(ry0),
                    rl.origin.x + f64::from(rx1),
                    rl.origin.y + f64::from(ry1),
                );
                scene.fill(Fill::NonZero, ctf, self.paper_selection(), None, &r);
            }
        }
        if let Some(rl) = self.region_laid.iter().find(|r| r.id == caret.block) {
            let byte = rl.composition.as_ref().map_or_else(
                || char_to_byte(&rl.text, caret.offset),
                |core| core.display_caret(&rl.text),
            );
            let (cx, cy, ch) = rl.layout.caret_geometry(byte);
            let c = Rect::new(
                rl.origin.x + f64::from(cx),
                rl.origin.y + f64::from(cy),
                rl.origin.x + f64::from(cx) + CARET_WIDTH,
                rl.origin.y + f64::from(cy) + f64::from(ch),
            );
            self.ime_rect = Some(ctf.transform_rect_bbox(c));
            scene.fill(Fill::NonZero, ctf, self.chrome.paper_accent, None, &c);
        }
    }

    /// The per-side measurement the active mode edits (the page margins).
    fn active_edges(&self) -> Option<Edges> {
        match self.layout_mode {
            LayoutMode::Margins => Some(self.margins),
            _ => None,
        }
    }

    /// The style id of the block under the caret.
    pub(super) fn focused_style_key(&self) -> Option<String> {
        self.blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)
            .map(block_style_key)
    }

    /// The `space_after` of the focused block's style.
    fn focused_space_after(&self) -> f64 {
        self.focused_style_key()
            .map_or(8.0, |k| f64::from(self.sheet.get(&k).space_after))
    }

    /// `(x-center, y)` of the spacing grip — centered in the gap *after the
    /// focused block*, so dragging it edits that block's style (every block of
    /// the same style follows).
    fn spacing_handle(&self) -> Option<(f64, f64)> {
        let l = self
            .laid
            .iter()
            .find(|l| l.id == self.inner.caret.block)
            .or_else(|| self.laid.first())?;
        let cx = f64::midpoint(self.page_rect.x0, self.page_rect.x1);
        Some((cx, l.y_top + l.height + self.focused_space_after() / 2.0))
    }

    /// Which guide (if any) is under the pointer, for the active layout mode.
    pub(super) fn hit_guide(&self, p: Point) -> Option<Side> {
        let page = self.page_rect;
        if let Some(e) = self.active_edges() {
            let mut best: Option<(Side, f64)> = None;
            for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
                let (vert, c) = guide_axis(page, e, side);
                let along = if vert {
                    p.y >= page.y0 && p.y <= page.y1
                } else {
                    p.x >= page.x0 && p.x <= page.x1
                };
                let d = if vert {
                    (p.x - c).abs()
                } else {
                    (p.y - c).abs()
                };
                if along && d <= GUIDE_GRAB && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((side, d));
                }
            }
            return best.map(|(s, _)| s);
        }
        if self.layout_mode == LayoutMode::Spacing
            && let Some((_, hy)) = self.spacing_handle()
            && (p.y - hy).abs() <= GUIDE_GRAB + 4.0
            && p.x >= page.x0
            && p.x <= page.x1
        {
            return Some(Side::Bottom); // side unused for spacing
        }
        None
    }

    /// Apply a live guide drag to the relevant layout value.
    pub(super) fn apply_guide_drag(&mut self, gd: GuideDrag, p: Point) {
        let page = self.page_rect;
        match gd.mode {
            LayoutMode::Margins => apply_edges(&mut self.margins, page, gd.side, p, gd.kind),
            LayoutMode::Spacing => {
                // Edit the focused block's *style* spacing — all blocks of that
                // style follow (component-instance model).
                let bottom = self
                    .laid
                    .iter()
                    .find(|l| l.id == self.inner.caret.block)
                    .or_else(|| self.laid.first())
                    .map(|l| l.y_top + l.height);
                let key = self.focused_style_key();
                if let (Some(bottom), Some(key)) = (bottom, key) {
                    let after = num::f32_of_f64((2.0 * (p.y - bottom)).clamp(0.0, 80.0));
                    if let Some(st) = self.sheet.styles.get_mut(&key) {
                        st.space_after = after;
                    }
                }
            }
            LayoutMode::None => {}
        }
    }

    /// The caret paragraph's left/hanging indent (px), read from the cached block
    /// views (the `indent` block attr), so the draw pass (which has no
    /// `state`) can read it. `0.0` when unset.
    fn caret_block_indent(&self) -> f64 {
        self.block_num_attr_view(&self.inner.caret.block, "indent")
    }

    /// The caret paragraph's first-line indent (px), read from cached block views.
    fn caret_block_first_line_indent(&self) -> f64 {
        self.block_num_attr_view(&self.inner.caret.block, "first_line_indent")
    }

    /// The caret paragraph's tab stops (px from the left text margin), read from
    /// cached block views (the comma-encoded `tab_stops` attr).
    fn caret_block_tab_stops(&self) -> Vec<f64> {
        let raw = self
            .blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)
            .and_then(|b| b.str_attr("tab_stops").map(str::to_owned))
            .unwrap_or_default();
        crate::word_processor::parse_tab_stops(&raw)
    }

    /// Read a numeric (`Float`/`Int`) attr off a cached block view, `0.0` absent.
    fn block_num_attr_view(&self, block: &str, key: &str) -> f64 {
        self.blocks
            .iter()
            .find(|b| b.id == block)
            .map_or(0.0, |b| match b.attrs.get(key) {
                Some(AttrVal::Float(f)) => *f,
                Some(AttrVal::Int(n)) => num::f64_of_i64(*n),
                _ => 0.0,
            })
    }

    /// Map an unscaled content x (the page coordinate space) to screen x using the
    /// cached zoom + anchor — the same scale-about the page content draws through.
    #[cfg(test)]
    pub(super) fn content_x_to_screen(&self, cx: f64) -> f64 {
        self.zoom.content_x_to_screen(cx)
    }

    fn ruler_model(&self, band: Rect, page_x: f64, page_w: f64) -> RulerModel {
        RulerModel {
            band,
            page_x,
            page_w,
            margin_left: self.margins.left,
            margin_right: self.margins.right,
            indent_left: self.caret_block_indent(),
            indent_first: self.caret_block_first_line_indent(),
            tab_stops: self.caret_block_tab_stops(),
            zoom: self.zoom.factor(),
            anchor_x: self.zoom.anchor().x,
        }
    }

    fn current_ruler_model(&self) -> RulerModel {
        self.ruler_model(self.ruler.rect(), self.page_rect.x0, self.page_rect.width())
    }

    #[cfg(test)]
    pub(super) fn ruler_markers(&self, page_x: f64, page_w: f64) -> Vec<(RulerMarker, f64)> {
        Ruler::markers(&self.ruler_model(self.ruler.rect(), page_x, page_w))
    }

    pub(super) fn draw_ruler(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        band: Rect,
        page_x: f64,
        page_w: f64,
    ) {
        let model = self.ruler_model(band, page_x, page_w);
        self.ruler.draw(scene, transform, text, theme, &model);
    }

    fn apply_ruler_preview(&mut self, effect: RulerEffect) {
        match effect {
            RulerEffect::PreviewMarginLeft(value) => self.margins.left = value,
            RulerEffect::PreviewMarginRight(value) => self.margins.right = value,
            RulerEffect::PreviewIndentLeft(value) => self.set_caret_block_indent_view(value),
            RulerEffect::PreviewIndentFirst(value) => {
                self.set_caret_block_first_line_view(value);
            }
            RulerEffect::PreviewTab { index, value } => {
                self.set_caret_tab_stop_view(index, value);
            }
            _ => {}
        }
    }

    pub(super) fn ruler_press(&mut self, state: &mut ui::widget::Edits<'_>) {
        let model = self.current_ruler_model();
        if let Some(RulerEffect::AddTab(value)) = self.ruler.press(self.pointer, &model) {
            let block = self.inner.caret.block.clone();
            let mut stops = self.inner.block_tab_stops(state, &block);
            if stops.iter().all(|stop| (*stop - value).abs() >= 2.0) {
                stops.push(value.max(0.0));
                stops.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let encoded = stops
                    .iter()
                    .map(|stop| format!("{stop:.1}"))
                    .collect::<Vec<_>>()
                    .join(",");
                self.perform_action(
                    state,
                    &crate::actions::set_block_attr_action(
                        self.inner.region_kind(),
                        vec![block],
                        "tab_stops",
                        crate::actions::AttrVal::Str(encoded),
                    ),
                );
            }
            self.refresh_blocks(state);
        }
    }

    pub(super) fn ruler_drag_move(&mut self) {
        let model = self.current_ruler_model();
        if let Some(effect) = self.ruler.drag_to(self.pointer, &model) {
            self.apply_ruler_preview(effect);
        }
    }

    pub(super) fn ruler_drag_release(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(effect) = self.ruler.release() else {
            return;
        };
        match effect {
            RulerEffect::CommitMargins => self.persist_margins(state),
            RulerEffect::CommitIndentLeft => {
                let value = self.caret_block_indent();
                let block = self.inner.caret.block.clone();
                self.perform_action(
                    state,
                    &crate::actions::set_block_attr_action(
                        self.inner.region_kind(),
                        vec![block],
                        "indent",
                        crate::actions::AttrVal::Float(value),
                    ),
                );
            }
            RulerEffect::CommitIndentFirst => {
                let value = self.caret_block_first_line_indent();
                let block = self.inner.caret.block.clone();
                self.perform_action(
                    state,
                    &crate::actions::set_block_attr_action(
                        self.inner.region_kind(),
                        vec![block],
                        "first_line_indent",
                        crate::actions::AttrVal::Float(value),
                    ),
                );
            }
            RulerEffect::CommitTabs { remove } => {
                let mut stops = self.caret_block_tab_stops();
                if let Some(index) = remove.filter(|index| *index < stops.len()) {
                    stops.remove(index);
                }
                let block = self.inner.caret.block.clone();
                let encoded = stops
                    .iter()
                    .map(|stop| format!("{stop:.1}"))
                    .collect::<Vec<_>>()
                    .join(",");
                self.perform_action(
                    state,
                    &crate::actions::set_block_attr_action(
                        self.inner.region_kind(),
                        vec![block],
                        "tab_stops",
                        crate::actions::AttrVal::Str(encoded),
                    ),
                );
            }
            _ => return,
        }
        self.refresh_blocks(state);
    }

    /// Update the caret block's `indent` attr in the cached block views only (a
    /// live drag preview before the release persists it).
    pub(super) fn set_caret_block_indent_view(&mut self, v: f64) {
        let id = self.inner.caret.block.clone();
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == id) {
            self.materialized.preview = true;
            b.attrs
                .insert("indent".to_string(), AttrVal::Float(v.max(0.0)));
        }
    }

    /// Update the caret block's `first_line_indent` attr in the cached block views.
    pub(super) fn set_caret_block_first_line_view(&mut self, v: f64) {
        let id = self.inner.caret.block.clone();
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == id) {
            self.materialized.preview = true;
            b.attrs
                .insert("first_line_indent".to_string(), AttrVal::Float(v));
        }
    }

    /// Update tab stop `i` in the cached block views (live drag preview).
    fn set_caret_tab_stop_view(&mut self, i: usize, v: f64) {
        let id = self.inner.caret.block.clone();
        let mut stops = self.caret_block_tab_stops();
        if let Some(stop) = stops.get_mut(i) {
            *stop = v.max(0.0);
        }
        let encoded = stops
            .iter()
            .map(|p| format!("{p:.1}"))
            .collect::<Vec<_>>()
            .join(",");
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == id) {
            self.materialized.preview = true;
            b.attrs
                .insert("tab_stops".to_string(), AttrVal::Str(encoded));
        }
    }

    /// Draggable guides + live value labels for the active layout mode, drawn
    /// over the page content.
    pub(super) fn draw_layout_overlay(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        page: Rect,
    ) {
        // The guides are what you are dragging: your colour, made legible
        // on the paper; the guide at rest is a quieter hairline of it.
        let theme = &self.paper_theme;
        let you = self.chrome.paper_accent;
        let rest = self.chrome.paper_edge;
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px).max(1.0);
        let grip = f64::from(theme.tokens.spacing.s1);
        let gap = f64::from(theme.tokens.spacing.s2);
        let label_style = theme.text(TextRole::Caption).numeric();
        if let Some(e) = self.active_edges() {
            let drag = self.guide_drag;
            for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
                let (vert, c) = guide_axis(page, e, side);
                let active = drag.is_some_and(|g| match g.kind {
                    DragKind::Proportional => true,
                    DragKind::Pair => g.side == side || g.side.opposite() == side,
                    DragKind::Single => g.side == side,
                });
                let (col, t) = if active {
                    (you, rule * 2.0)
                } else {
                    (rest, rule)
                };
                // At rest a guide runs through the margins only, from the
                // page's edge to the text block's corner on either side, so
                // it never runs along the text; the guide being dragged
                // spans the page.
                let (lo, hi) = if vert {
                    (page.y0 + e.top, page.y1 - e.bottom)
                } else {
                    (page.x0 + e.left, page.x1 - e.right)
                };
                let (start, end) = if vert {
                    (page.y0, page.y1)
                } else {
                    (page.x0, page.x1)
                };
                let spans = if active {
                    vec![(start, end)]
                } else {
                    vec![(start, lo), (hi, end)]
                };
                for (a, b) in spans {
                    let line = if vert {
                        Rect::new(c - t / 2.0, a, c + t / 2.0, b)
                    } else {
                        Rect::new(a, c - t / 2.0, b, c + t / 2.0)
                    };
                    scene.fill(Fill::NonZero, transform, col, None, &line);
                }
                // A grip in the margin shows where to take the guide: half
                // way across the top margin on a side guide, half way across
                // the leading margin on a top or bottom one.
                let (gx, gy) = if vert {
                    (c, f64::midpoint(start, lo))
                } else {
                    (f64::midpoint(start, lo), c)
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    you,
                    None,
                    &vello::kurbo::Circle::new((gx, gy), grip),
                );
                // The value, set just inside the page beside its guide.
                let label = format!("{}", num::i64_of_f64(e.get(side).round()));
                let (lx, ly) = if vert {
                    (c + gap, page.y0 + gap)
                } else {
                    (page.x0 + gap, c + gap)
                };
                tr.draw_style(
                    scene,
                    &label,
                    you,
                    transform * Affine::translate((lx, ly)),
                    None,
                    &label_style,
                );
            }
        } else if self.layout_mode == LayoutMode::Spacing
            && let Some((hx, hy)) = self.spacing_handle()
        {
            let half = f64::from(theme.tokens.spacing.s4);
            let pill = Rect::new(hx - half, hy - grip / 2.0, hx + half, hy + grip / 2.0);
            scene.fill(
                Fill::NonZero,
                transform,
                you,
                None,
                &vello::kurbo::RoundedRect::from_rect(pill, grip / 2.0),
            );
            let label = format!("{} px", num::i64_of_f64(self.focused_space_after().round()));
            let (_, h) = tr.measure_style(&label, None, &label_style);
            tr.draw_style(
                scene,
                &label,
                you,
                transform * Affine::translate((hx + half + gap, hy - f64::from(h) / 2.0)),
                None,
                &label_style,
            );
        }
    }

    /// The status band's height: one small control with the tightest step
    /// above and below.
    pub(super) fn status_band_height(theme: &Theme) -> f64 {
        ButtonFace::height(ControlSize::Small, theme) + 2.0 * f64::from(theme.tokens.spacing.s1)
    }

    /// The status band along the bottom: the Margins / Spacing layout modes
    /// as one segmented control, the live readout of the mode in use, the
    /// word and character counts, and the zoom field. It stands on the
    /// content ground with one hairline above it.
    pub(super) fn draw_layout_bar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bar: Rect,
    ) {
        ui::chrome::status_bar::draw(scene, transform, tr, theme, bar, "", "");
        let sp = &theme.tokens.spacing;
        let (s3, s4) = (f64::from(sp.s3), f64::from(sp.s4));
        let cy = bar.center().y;
        let control_h = ButtonFace::height(ControlSize::Small, theme);

        // The two layout modes are toggles, at most one of them in use and
        // neither at rest: plain buttons that stand on the selection lift
        // while their mode is on.
        let gap = f64::from(sp.s1);
        let mut x = bar.x0 + s3;
        let mut chips = Vec::with_capacity(2);
        for (mode, label) in [
            (LayoutMode::Margins, t("layout-margins")),
            (LayoutMode::Spacing, t("layout-spacing")),
        ] {
            let w = ButtonFace::new(Rect::ZERO, &label)
                .control_size(ControlSize::Small)
                .intrinsic_width(tr, theme);
            let rect = Rect::new(x, cy - control_h / 2.0, x + w, cy + control_h / 2.0);
            Button::paint(
                scene,
                transform,
                tr,
                theme,
                &ButtonFace::new(rect, &label)
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small)
                    .toggled(self.layout_mode == mode)
                    .hovered(rect.contains(self.pointer)),
            );
            chips.push((mode, rect));
            x = rect.x1 + gap;
        }
        let strip_x1 = x - gap;
        self.layout_chips = chips;

        // Trailing: the zoom field, then the counts before it.
        let zoom_w = self.zoom_input.preferred_width(theme, tr);
        let zoom = Rect::new(
            bar.x1 - s3 - zoom_w,
            cy - control_h / 2.0,
            bar.x1 - s3,
            cy + control_h / 2.0,
        );
        if !self.zoom_input.is_editing() {
            self.zoom_input
                .set_value((self.zoom.factor() * 100.0).round());
        }
        self.zoom_input.apply_layout_external(zoom);
        self.zoom_rect = zoom;
        Widget::draw(
            &mut self.zoom_input,
            scene,
            transform,
            tr,
            theme,
            &PresenceCtx::empty(),
        );

        let style = theme.text(TextRole::Caption).numeric();
        let ink = theme.tokens.colors.text_dim;
        let mut right = zoom.x0 - s4;
        let mut left = strip_x1 + s4;
        for readout in self.status_readouts().into_iter().rev() {
            let (w, h) = tr.measure_style(&readout, None, &style);
            let w = f64::from(w);
            if right - w < left {
                break;
            }
            tr.draw_style(
                scene,
                &readout,
                ink,
                transform * Affine::translate((right - w, cy - f64::from(h) / 2.0)),
                None,
                &style,
            );
            right -= w + s4;
        }

        if let Some(readout) = self.layout_readout() {
            let (w, h) = tr.measure_style(&readout, None, &style);
            if left + f64::from(w) <= right {
                tr.draw_style(
                    scene,
                    &readout,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((left, cy - f64::from(h) / 2.0)),
                    None,
                    &style,
                );
                left += f64::from(w) + s4;
            }
        }
        let _ = left;
    }

    /// The live value of the layout mode in use, or nothing at rest.
    fn layout_readout(&self) -> Option<String> {
        match self.layout_mode {
            LayoutMode::Margins => Some(t_args(
                "layout-margins-readout",
                &[
                    arg("l", num::i64_of_f64(self.margins.left.round())),
                    arg("t", num::i64_of_f64(self.margins.top.round())),
                    arg("r", num::i64_of_f64(self.margins.right.round())),
                    arg("b", num::i64_of_f64(self.margins.bottom.round())),
                ],
            )),
            LayoutMode::Spacing => {
                let name = self.focused_style_key().map_or_else(
                    || t("style-body"),
                    |k| crate::styles::style_display_name(&k, &self.sheet.get(&k)),
                );
                Some(t_args(
                    "layout-spacing-readout",
                    &[
                        arg("name", name),
                        arg("px", num::i64_of_f64(self.focused_space_after().round())),
                    ],
                ))
            }
            LayoutMode::None => None,
        }
    }

    /// Whether the styles panel is drawn this frame.
    pub(super) fn styles_panel_visible(&self) -> bool {
        self.view.styles_panel_visible
    }

    /// Wider compact tiles use a drawer; phones present the inspector as
    /// the foreground page. Desktop windows reserve space beside the document.
    pub(super) fn styles_panel_overlay(&self) -> bool {
        self.view.styles_panel_visible && self.inspector_drawer.is_overlay()
    }

    pub(super) fn styles_panel_full_window(&self) -> bool {
        self.view.styles_panel_visible
            && ui::widget::modal_dialog::uses_full_window(self.rect, &self.desk_theme)
    }

    /// The contextual inspector. Docked, it is the window's inspector region;
    /// on a phone it fills the window using the shared opaque page surface.
    /// A header names
    /// what it inspects beside its close button, and below it the image
    /// inspector or the paragraph's two sections scroll as one column.
    pub(super) fn draw_styles_panel(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
    ) {
        self.panel_hits.clear();
        for slot in &mut self.num_inputs {
            slot.rect = Rect::ZERO;
        }
        self.line_spacing_rect = Rect::ZERO;
        self.style_pick_rect = Rect::ZERO;
        let local = theme.clone();
        let sp = &local.tokens.spacing;
        self.panel_rect = panel;
        let pad = f64::from(local.tokens.spacing.s4);
        let x0 = panel.x0 + pad;
        let right = panel.x1 - pad;

        // Header: what the panel inspects, and its close button.
        let image = self.selected_image_id().is_some();
        let title = if image {
            t("panel-image")
        } else {
            t("toolbar-styles")
        };
        let title_style = local.text(TextRole::Heading);
        let (_, title_h) = tr.measure_style(&title, None, &title_style);
        let target = ButtonFace::height(ControlSize::Small, &local)
            .max(f64::from(local.tokens.metrics.min_pointer_target))
            .max(f64::from(title_h));
        let header = Rect::new(
            x0,
            panel.y0 + f64::from(sp.s2),
            right,
            panel.y0 + f64::from(sp.s2) + target,
        );
        let close = Rect::new(right - target, header.y0, right, header.y1);
        tr.draw_style(
            scene,
            &title,
            local.tokens.colors.text_primary,
            transform * Affine::translate((x0, header.center().y - f64::from(title_h) / 2.0)),
            Some(num::f32_of_f64((close.x0 - x0).max(1.0))),
            &title_style,
        );
        let close_label = t("panel-close");
        Button::paint(
            scene,
            transform,
            tr,
            &local,
            &ButtonFace::icon_only(close, Icon::X, &close_label)
                .hovered(close.contains(self.pointer)),
        );
        self.panel_hits.push((close, PanelHit::Close));

        // The column below the header scrolls when it is taller than the
        // panel.
        let body = Rect::new(panel.x0, header.y1 + f64::from(sp.s2), panel.x1, panel.y1);
        self.panel_body = body;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &body,
        );
        let top = body.y0 - self.panel_scroll;
        let mut ctx = PanelCtx {
            scene,
            transform,
            tr,
            theme: &local,
            x0,
            right,
            y: top,
            clip: body,
        };
        if image {
            self.draw_image_inspector(&mut ctx);
        } else if let Some(key) = self.focused_style_key() {
            let st = self.sheet.get(&key);
            self.draw_panel_this_paragraph(&mut ctx);
            self.draw_panel_style(&mut ctx, &key, &st);
        } else {
            let style = local.text(TextRole::Body);
            ctx.tr.draw_style(
                ctx.scene,
                &t("panel-caret-hint"),
                local.tokens.colors.text_dim,
                transform * Affine::translate((x0, ctx.y)),
                Some(num::f32_of_f64((right - x0).max(1.0))),
                &style,
            );
            ctx.y += f64::from(ctx.tr.measure_style("Ag", None, &style).1);
        }
        let content_h = ctx.y + pad - top;
        ctx.scene.pop_layer();
        self.panel_max_scroll = (content_h - body.height()).max(0.0);
        self.panel_scroll = self.panel_scroll.clamp(0.0, self.panel_max_scroll);
    }

    /// "This paragraph": direct formatting of the paragraph(s) under the
    /// caret — alignment as a segmented control and line spacing as a menu.
    fn draw_panel_this_paragraph(&mut self, ctx: &mut PanelCtx<'_>) {
        ctx.section(&t("panel-this-paragraph"), true);
        ctx.field_label(&t("panel-align"));
        let values = ["left", "center", "right", "justify"];
        let segments = [
            ui::widget::Segment::icon(ui::icons::Icon::AlignLeft, t("panel-align-left")),
            ui::widget::Segment::icon(ui::icons::Icon::AlignCenter, t("panel-align-center")),
            ui::widget::Segment::icon(ui::icons::Icon::AlignRight, t("panel-align-right")),
            ui::widget::Segment::icon(ui::icons::Icon::AlignJustify, t("panel-align-justify")),
        ];
        let current = self.current_align();
        let selected = values
            .iter()
            .position(|v| *v == current)
            .unwrap_or(usize::MAX);
        let rects = ctx.segmented_items(&segments, selected, self.pointer);
        for (rect, value) in rects.into_iter().zip(values) {
            self.panel_hits.push((rect, PanelHit::ParaAlign(value)));
        }
        ctx.field_label(&t("panel-line-spacing"));
        let rect = ctx.control_line();
        let current = line_spacing_label(self.current_line_spacing());
        if !self.line_spacing.is_open() {
            self.line_spacing.set_selected(current);
        }
        let viewport = self.floating_viewport();
        self.line_spacing_rect = Self::host_menu(&mut self.line_spacing, ctx, rect, viewport);
    }

    /// Lay out and draw a hosted menu in `rect`; returns where it was drawn
    /// (nothing while scrolled out of view).
    fn host_menu(
        menu: &mut ui::widget::Dropdown,
        ctx: &mut PanelCtx<'_>,
        rect: Rect,
        viewport: Rect,
    ) -> Rect {
        menu.apply_layout_external(rect);
        menu.set_viewport(viewport);
        if !ctx.visible(rect) {
            return Rect::ZERO;
        }
        Widget::draw(
            menu,
            ctx.scene,
            ctx.transform,
            ctx.tr,
            ctx.theme,
            &PresenceCtx::empty(),
        );
        rect
    }

    /// "Paragraph style": the named style under the caret, picked from a
    /// menu, then the style's properties, which change every paragraph of
    /// that style. Font, bold and italic are not among them: they are set on
    /// the selection, from the toolbar, in one place.
    fn draw_panel_style(
        &mut self,
        ctx: &mut PanelCtx<'_>,
        key: &str,
        st: &crate::styles::StyleProps,
    ) {
        ctx.section(&t("panel-style"), false);
        let options: Vec<(String, String)> = self
            .sheet
            .order()
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    crate::styles::style_display_name(id, &self.sheet.get(id)),
                )
            })
            .collect();
        if options != self.style_pick_options {
            self.style_pick.set_value_options(options.clone());
            self.style_pick_options = options;
        }
        if !self.style_pick.is_open() {
            self.style_pick.set_selected(key);
        }
        let rect = ctx.control_line();
        let viewport = self.floating_viewport();
        self.style_pick_rect = Self::host_menu(&mut self.style_pick, ctx, rect, viewport);
        ctx.caption(&t("panel-style-hint"));

        self.draw_panel_style_properties(ctx, st);
    }

    /// The style's colour, sizes and spacing, then a new style from this
    /// paragraph's look.
    fn draw_panel_style_properties(
        &mut self,
        ctx: &mut PanelCtx<'_>,
        st: &crate::styles::StyleProps,
    ) {
        // Colour: the document palette as swatches and a custom colour.
        ctx.field_label(&t("panel-colour"));
        let swatch_gap = f64::from(ctx.theme.tokens.spacing.s1);
        let count = STYLE_PALETTE.len() + 1;
        let side = ((ctx.right - ctx.x0 - swatch_gap * num::f64_of_usize(count - 1))
            / num::f64_of_usize(count))
        .min(ButtonFace::height(ControlSize::Small, ctx.theme));
        let names = swatch_names(PaletteKind::Text);
        let mut x = ctx.x0;
        for (c, name) in STYLE_PALETTE.iter().zip(names) {
            let r = Rect::new(x, ctx.y, x + side, ctx.y + side);
            if ctx.visible(r) {
                ColorSwatchFace::new(r, Color::from_rgb8(c[0], c[1], c[2]), name)
                    .selected(*c == st.color)
                    .hovered(r.contains(self.pointer))
                    .paint(ctx.scene, ctx.transform, ctx.tr, ctx.theme);
                self.panel_hits.push((r, PanelHit::SetColor(*c)));
            }
            x += side + swatch_gap;
        }
        let custom = Rect::new(x, ctx.y, x + side, ctx.y + side);
        if ctx.visible(custom) {
            let label = t("panel-colour-custom");
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.tr,
                ctx.theme,
                &ButtonFace::icon_only(custom, Icon::Plus, &label)
                    .control_size(ControlSize::Small)
                    .hovered(custom.contains(self.pointer)),
            );
            self.panel_hits.push((custom, PanelHit::OpenColorPicker));
        }
        ctx.y += side + f64::from(ctx.theme.tokens.spacing.rhythm.related);

        // Size and spacing, as number fields two to a line, each under its
        // label.
        for pair in [
            [NumField::Size, NumField::Indent],
            [NumField::SpaceBefore, NumField::SpaceAfter],
        ] {
            let (left, right) = ctx.label_pair(&pair[0].label(), &pair[1].label());
            for (field, column) in pair.into_iter().zip([left, right]) {
                let value = f64::from(field.get(st));
                let decimals = usize::from(value.fract().abs() > 0.05);
                let Some(slot) = self.num_inputs.iter_mut().find(|slot| slot.field == field) else {
                    continue;
                };
                if slot.decimals != decimals {
                    slot.input = field.input().with_precision(decimals);
                    slot.decimals = decimals;
                }
                if !slot.input.is_editing() {
                    slot.input.set_value(value);
                }
                let w = slot
                    .input
                    .preferred_width(ctx.theme, ctx.tr)
                    .min(column.width());
                let rect = Rect::new(column.x0, column.y0, column.x0 + w, column.y1);
                slot.input.apply_layout_external(rect);
                if ctx.visible(rect) {
                    Widget::draw(
                        &mut slot.input,
                        ctx.scene,
                        ctx.transform,
                        ctx.tr,
                        ctx.theme,
                        &PresenceCtx::empty(),
                    );
                    slot.rect = rect;
                } else {
                    slot.rect = Rect::ZERO;
                }
            }
        }

        // A new style from this paragraph's look.
        ctx.y += f64::from(ctx.theme.tokens.spacing.rhythm.related);
        let r = ctx.control_line();
        ctx.button(r, &t("panel-new-style"), None, self.pointer);
        if ctx.visible(r) {
            self.panel_hits.push((r, PanelHit::NewStyle));
        }
    }

    /// The image inspector shown when an image is selected: its size, a
    /// smaller / larger stepper (aspect-locked), flips, crop and placement.
    /// Its controls register as `panel_hits` so pointer and assistive
    /// technology activation share one dispatch path.
    fn draw_image_inspector(&mut self, ctx: &mut PanelCtx<'_>) {
        let (dw, dh) = self.selected_image_dims().unwrap_or((0.0, 0.0));
        let dims = format!(
            "{} × {} px",
            num::i64_of_f64(dw.round()),
            num::i64_of_f64(dh.round())
        );
        ctx.text_line(&dims, TextRole::Body, false, false);
        ctx.y += f64::from(ctx.theme.tokens.spacing.rhythm.related);

        let img_id = self.selected_image_id();
        let block = self
            .blocks
            .iter()
            .find(|b| Some(&b.id) == img_id.as_ref())
            .cloned();
        let (fh, fv) = block
            .as_ref()
            .map_or((false, false), |b| (b.flag("flip_h"), b.flag("flip_v")));

        for (label, first, second, toggles) in [
            (
                t("panel-image-size"),
                (t("panel-image-smaller"), PanelHit::ImageScale(0.9), false),
                (t("panel-image-larger"), PanelHit::ImageScale(1.1), false),
                false,
            ),
            (
                t("panel-image-flip"),
                (t("panel-image-flip-h"), PanelHit::ImageFlipH, fh),
                (t("panel-image-flip-v"), PanelHit::ImageFlipV, fv),
                true,
            ),
            (
                t("panel-image-crop"),
                (t("panel-image-crop-inset"), PanelHit::ImageCropInset, false),
                (t("panel-image-crop-reset"), PanelHit::ImageCropReset, false),
                false,
            ),
        ] {
            ctx.field_label(&label);
            let (r1, r2) = ctx.pair();
            for (rect, (label, hit, on)) in [(r1, first), (r2, second)] {
                ctx.button(rect, &label, toggles.then_some(on), self.pointer);
                if ctx.visible(rect) {
                    self.panel_hits.push((rect, hit));
                }
            }
        }

        // Placement, as one segmented control the width of the column.
        ctx.field_label(&t("panel-image-position"));
        let labels = [
            t("panel-image-inline"),
            t("panel-image-free"),
            t("panel-image-wrap-left"),
            t("panel-image-wrap-right"),
        ];
        let selected =
            block
                .as_ref()
                .map_or(usize::MAX, |b| match (image_anchor(b), image_wrap(b)) {
                    ("page", _) => 1,
                    (_, "left") => 2,
                    (_, "right") => 3,
                    _ => 0,
                });
        let rects = ctx.segmented(&labels, selected, self.pointer);
        for (rect, hit) in rects.into_iter().zip([
            PanelHit::ImageInline,
            PanelHit::ImageFreePosition,
            PanelHit::ImageWrapLeft,
            PanelHit::ImageWrapRight,
        ]) {
            self.panel_hits.push((rect, hit));
        }
    }

    /// The document outline: one entry per `heading` block (in document order),
    /// derived from the already-materialized `self.blocks` so it works mid-frame
    /// without re-materializing the doc. Excludes generated TOC lines.
    pub(super) fn outline_entries(&self) -> Vec<crate::word_processor::Heading> {
        self.blocks
            .iter()
            .filter(|b| b.kind == "heading" && !crate::word_processor::block_is_toc(b))
            .map(|b| {
                let level = match b.attrs.get("level") {
                    Some(AttrVal::Int(n)) if *n >= 1 => *n,
                    _ => 1,
                };
                crate::word_processor::Heading {
                    id: b.id.clone(),
                    level,
                    text: b.text.clone(),
                }
            })
            .collect()
    }

    /// The heading nearest (at or just above) the current scroll position — the
    /// one to highlight in the outline as "where you are". Uses the last frame's
    /// laid geometry (`y_top` is screen-space post-scroll; recover content space).
    /// Returns the heading block id, or `None` when there are no headings laid.
    pub(super) fn active_outline_block(&self) -> Option<String> {
        // Content-space top of the visible canvas region.
        let view_top = self.scroll_y;
        let mut best: Option<(f64, String)> = None; // (content_y, id)
        let mut first: Option<(f64, String)> = None;
        for l in &self.laid {
            if l.kind != "heading" {
                continue;
            }
            let cy = l.y_top + self.scroll_y; // content-space top
            if first.as_ref().is_none_or(|(y, _)| cy < *y) {
                first = Some((cy, l.id.clone()));
            }
            // The deepest heading whose top is at/above the viewport top.
            if cy <= view_top + 1.0 && best.as_ref().is_none_or(|(y, _)| cy >= *y) {
                best = Some((cy, l.id.clone()));
            }
        }
        best.or(first).map(|(_, id)| id)
    }

    /// Generate (or regenerate) the table of contents from the document headings.
    /// Page numbers are read from the last frame's laid geometry (`Laid::page`);
    /// a heading not yet laid out falls back to page 1. Delegates the block
    /// mutation to the model so the insert/replace logic stays testable.
    pub(super) fn generate_toc(&mut self, state: &mut ui::widget::Edits<'_>) {
        // Map heading block id → 1-based page from the last laid-out frame.
        let page_of: HashMap<&str, usize> = self
            .laid
            .iter()
            .map(|l| (l.id.as_str(), l.page + 1))
            .collect();
        let entries: Vec<crate::actions::TocTarget> = self
            .outline_entries()
            .into_iter()
            .map(|h| {
                let page = page_of.get(h.id.as_str()).copied();
                crate::actions::TocTarget {
                    heading: h.id,
                    level: h.level,
                    text: h.text,
                    page,
                }
            })
            .collect();
        if entries.is_empty() {
            // Nothing to build a TOC from — tell the user *why* nothing happened.
            self.toast.show(t("toast-toc-empty"), ToastKind::Info);
            self.lifecycle.dirty = true;
            return;
        }
        self.perform_action(state, &crate::actions::generate_toc_action(entries));
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
        // Scroll the freshly-inserted TOC (always at the document top) into
        // view: the table itself is the visible result.
        self.scroll_y = 0.0;
    }
}

impl PanelCtx<'_> {
    /// Whether `rect` shows in the scrolled column at all.
    fn visible(&self, rect: Rect) -> bool {
        rect.y1 > self.clip.y0 && rect.y0 < self.clip.y1
    }

    /// One block of text, wrapped to the column.
    fn text_line(&mut self, text: &str, role: TextRole, strong: bool, dim: bool) {
        let style = if strong {
            self.theme.text(role).strong()
        } else {
            self.theme.text(role)
        };
        let width = num::f32_of_f64((self.right - self.x0).max(1.0));
        let colors = &self.theme.tokens.colors;
        let ink = if dim {
            colors.text_dim
        } else {
            colors.text_primary
        };
        let (_, h) = self.tr.measure_style(text, Some(width), &style);
        let rect = Rect::new(self.x0, self.y, self.right, self.y + f64::from(h));
        if self.visible(rect) {
            self.tr.draw_style(
                self.scene,
                text,
                ink,
                self.transform * Affine::translate((self.x0, self.y)),
                Some(width),
                &style,
            );
        }
        self.y += f64::from(h);
    }

    /// A section header in `Subheading`, a group's gap above it unless it is
    /// the panel's first.
    fn section(&mut self, title: &str, first: bool) {
        if !first {
            self.y += f64::from(self.theme.tokens.spacing.rhythm.group);
        }
        self.text_line(title, TextRole::Subheading, false, false);
        self.y += f64::from(self.theme.tokens.spacing.rhythm.related);
    }

    /// A field's label, in secondary ink, over the control on the line
    /// below it. Every field of the inspector is set this way, so labels
    /// and controls share one leading edge.
    fn field_label(&mut self, label: &str) {
        self.text_line(label, TextRole::Label, false, true);
        self.y += f64::from(self.theme.tokens.spacing.rhythm.tight);
    }

    /// One line of `Caption` in secondary ink under the control above it.
    fn caption(&mut self, text: &str) {
        self.y -= f64::from(self.theme.tokens.spacing.rhythm.related)
            - f64::from(self.theme.tokens.spacing.rhythm.tight);
        self.text_line(text, TextRole::Caption, false, true);
        self.y += f64::from(self.theme.tokens.spacing.rhythm.related);
    }

    /// A line one small control tall, the width of the column; returns it
    /// for the control.
    fn control_line(&mut self) -> Rect {
        let h = ButtonFace::height(ControlSize::Small, self.theme);
        let line = Rect::new(self.x0, self.y, self.right, self.y + h);
        self.y += h + f64::from(self.theme.tokens.spacing.rhythm.related);
        line
    }

    /// A control line split into two equal halves.
    fn pair(&mut self) -> (Rect, Rect) {
        let gap = f64::from(self.theme.tokens.spacing.s2);
        let line = self.control_line();
        let mid = f64::midpoint(line.x0, line.x1);
        (
            Rect::new(line.x0, line.y0, mid - gap / 2.0, line.y1),
            Rect::new(mid + gap / 2.0, line.y0, line.x1, line.y1),
        )
    }

    /// Two fields side by side, each label over its half of the control
    /// line below; returns the two halves.
    fn label_pair(&mut self, first: &str, second: &str) -> (Rect, Rect) {
        let gap = f64::from(self.theme.tokens.spacing.s2);
        let mid = f64::midpoint(self.x0, self.right);
        let style = self.theme.text(TextRole::Label);
        let ink = self.theme.tokens.colors.text_dim;
        let width = num::f32_of_f64((mid - gap / 2.0 - self.x0).max(1.0));
        let (_, h) = self.tr.measure_style(first, Some(width), &style);
        let label = Rect::new(self.x0, self.y, self.right, self.y + f64::from(h));
        if self.visible(label) {
            for (text, x) in [(first, self.x0), (second, mid + gap / 2.0)] {
                self.tr.draw_style(
                    self.scene,
                    text,
                    ink,
                    self.transform * Affine::translate((x, self.y)),
                    Some(width),
                    &style,
                );
            }
        }
        self.y += f64::from(h) + f64::from(self.theme.tokens.spacing.rhythm.tight);
        self.pair()
    }

    /// A segmented control the width of the column on its own line;
    /// returns each segment's rect (none while scrolled out of view).
    fn segmented(&mut self, labels: &[String], selected: usize, pointer: Point) -> Vec<Rect> {
        let segments: Vec<ui::widget::Segment> =
            labels.iter().map(ui::widget::Segment::new).collect();
        self.segmented_items(&segments, selected, pointer)
    }

    /// [`Self::segmented`] of segments that may show icons (each named by
    /// its label).
    fn segmented_items(
        &mut self,
        segments: &[ui::widget::Segment],
        selected: usize,
        pointer: Point,
    ) -> Vec<Rect> {
        let h = SegmentedFace::height(ControlSize::Small, self.theme);
        let row = Rect::new(self.x0, self.y, self.right, self.y + h);
        let rects = self.segmented_in(row, segments, selected, pointer);
        self.y += h + f64::from(self.theme.tokens.spacing.rhythm.related);
        rects
    }

    /// A segmented control filling `row`; returns each segment's rect.
    fn segmented_in(
        &mut self,
        row: Rect,
        segments: &[ui::widget::Segment],
        selected: usize,
        pointer: Point,
    ) -> Vec<Rect> {
        if !self.visible(row) || segments.is_empty() {
            return Vec::new();
        }
        let n = num::f64_of_usize(segments.len());
        let seg = row.width() / n;
        let rects: Vec<Rect> = (0..segments.len())
            .map(|i| {
                let i = num::f64_of_usize(i);
                Rect::new(row.x0 + i * seg, row.y0, row.x0 + (i + 1.0) * seg, row.y1)
            })
            .collect();
        let hovered = rects.iter().position(|r| r.contains(pointer));
        ui::widget::ToggleGroup::paint(
            self.scene,
            self.transform,
            self.tr,
            self.theme,
            &SegmentedFace::of_segments(&rects, segments, selected)
                .control_size(ControlSize::Small)
                .hovered(hovered),
        );
        rects
    }

    /// A small secondary button in `rect`; with `toggle`, a toggle that
    /// stands on the selection lift while on.
    fn button(&mut self, rect: Rect, label: &str, toggle: Option<bool>, pointer: Point) {
        if !self.visible(rect) {
            return;
        }
        Button::paint(
            self.scene,
            self.transform,
            self.tr,
            self.theme,
            &ButtonFace::new(rect, label)
                .control_size(ControlSize::Small)
                .toggled(toggle == Some(true))
                .hovered(rect.contains(pointer)),
        );
    }
}

/// The spoken names of a palette's swatches, in palette order.
pub(super) fn swatch_names(kind: PaletteKind) -> Vec<String> {
    match kind {
        PaletteKind::Text => [
            "color-black",
            "color-gray",
            "color-red",
            "color-blue",
            "color-green",
            "color-amber",
            "color-purple",
        ]
        .into_iter()
        .map(|key| t_args("a11y-text-colour-name", &[arg("colour", t(key))]))
        .collect(),
        PaletteKind::Highlight => [
            "color-yellow",
            "color-green",
            "color-cyan",
            "color-pink",
            "color-orange",
        ]
        .into_iter()
        .map(|key| t_args("a11y-highlight-name", &[arg("colour", t(key))]))
        .collect(),
    }
}
