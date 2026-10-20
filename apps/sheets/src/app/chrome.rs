//! The window chrome around the grid: the command bar, the formula bar and the
//! sheet tabs, plus the transient status bubble.
//!
//! The command bar is the toolkit's `Toolbar` in the window's header row
//! (`HeaderRowFace`), with the window's History control in its corner.
//! Everything else is laid out from measured role line boxes and spacing
//! tokens, and painted through the toolkit's faces (`ButtonFace`,
//! `SegmentedFace`, `FieldFace`), so a control looks and sizes like its
//! retained twin. The bars stand on the window ground; the command bar is
//! separated from the formula bar by one hairline and the sheet tabs from the
//! grid by another.

use super::annotations::CardMetrics;
use super::{
    Affine, BlendMode, Button, Fill, GridMetrics, HashMap, MenuItem, Point, PresenceCtx,
    PresenceKey, Rect, RoundedRect, Scene, SheetsApp, TextRenderer, TextRole, TextStyle, Theme,
    Widget, WindowEvent, tab_palette_color,
};
use ui::chrome::HeaderRowFace;
use ui::icons::Icon;
use ui::num::f64_of_usize;
use ui::surface::separator;
use ui::widget::toolbar;
use ui::widget::toolbar::Orientation;
use ui::widget::{
    ButtonFace, ColorSwatchFace, ControlSize, FieldFace, SegmentedFace, SegmentedStyle, Toolbar,
};

/// Reference label the name box is sized for, so it never resizes with the
/// selection and always fits a two-corner range reference.
const NAME_BOX_REFERENCE: &str = "ZZ999:ZZ999";

/// Where each command stands in the command bar's [`Toolbar`]: File and
/// Format open menus and never leave the bar; the rest move into the
/// toolbar's More menu, from the end, when the window is too narrow.
pub(super) mod bar {
    pub const FILE: usize = 0;
    pub const FORMAT: usize = 1;
    pub const BOLD: usize = 3;
    #[cfg(test)]
    pub const SETTINGS: usize = 6;
    /// Every command, without the separator.
    #[cfg(test)]
    pub const COMMANDS: [usize; 6] = [FILE, FORMAT, BOLD, 4, FORMULAS, SETTINGS];
    pub const FORMULAS: usize = 5;
}

/// Chrome metrics resolved once per frame from the renderer that paints the
/// chrome.
///
/// Every band height, control height and label slot in `relayout` derives
/// from the measured role line boxes, the toolkit's control heights and the
/// spacing tokens, so layout, paint and hit-testing share one metric.
/// `fallback()` stands in until the first draw; `SheetsApp::refresh_chrome_metrics`
/// swaps it for the measured set and re-runs `relayout` in the same frame, so
/// nothing is ever painted from stale geometry.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ChromeMetrics {
    /// Text scale the line boxes and widths were measured under.
    pub scale: f64,
    /// Measured line boxes of the roles the chrome is set in, in final pixels.
    pub lh_label: f64,
    pub lh_body: f64,
    pub lh_caption: f64,
    /// Measured advance of the italic "fx" label.
    pub fx_w: f64,
    /// Measured width of [`NAME_BOX_REFERENCE`] in `Body`.
    pub name_box_label_w: f64,
    /// Width of every sheet tab's name in the chosen tab's strong `Label`.
    pub tab_label_w: HashMap<String, f64>,
    /// Theme spacing tokens.
    pub s1: f64,
    pub s2: f64,
    pub s3: f64,
    pub s4: f64,
    pub s6: f64,
    pub s16: f64,
    /// Height of a `Small` control (command, tab) and of a `Regular` one (field).
    pub control_h: f64,
    pub field_h: f64,
    /// The grid's share of the same resolution.
    pub grid: GridMetrics,
    /// What the annotation dock's cards need.
    pub cards: CardMetrics,
}

impl ChromeMetrics {
    /// Resolve every chrome metric for `theme` against `tr`. `tabs` are the
    /// sheet names `relayout` will size a slot for.
    pub(super) fn resolve(tr: &mut TextRenderer, theme: &Theme, tabs: &[String]) -> Self {
        let label = theme.text(TextRole::Label);
        let body = theme.text(TextRole::Body);
        let (_, lh_label) = tr.measure_style("Ag", None, &label);
        let (_, lh_body) = tr.measure_style("Ag", None, &body);
        let (_, lh_caption) = tr.measure_style("Ag", None, &theme.text(TextRole::Caption));
        let (fx_w, _) = tr.measure_style("fx", None, &label.clone().with_italic(true));
        let (name_box_label_w, _) = tr.measure_style(NAME_BOX_REFERENCE, None, &body);
        let strong = label.strong();
        let tab_label_w = tabs
            .iter()
            .map(|name| {
                let (w, _) = tr.measure_style(name, None, &strong);
                (name.clone(), f64::from(w))
            })
            .collect();
        let sp = &theme.tokens.spacing;
        let mut metrics = Self {
            scale: f64::from(theme.text_scale),
            lh_label: f64::from(lh_label),
            lh_body: f64::from(lh_body),
            lh_caption: f64::from(lh_caption),
            fx_w: f64::from(fx_w),
            name_box_label_w: f64::from(name_box_label_w),
            tab_label_w,
            s1: f64::from(sp.s1),
            s2: f64::from(sp.s2),
            s3: f64::from(sp.s3),
            s4: f64::from(sp.s4),
            s6: f64::from(sp.s6),
            s16: f64::from(sp.s16),
            control_h: ControlSize::Small.height(theme),
            field_h: ControlSize::Regular.height(theme),
            grid: GridMetrics::resolve(tr, theme),
            cards: CardMetrics::default(),
        };
        metrics.cards = CardMetrics::resolve(tr, theme, &metrics);
        metrics
    }

    /// The 1× metrics used before the first draw has a renderer. Every band
    /// lands on its floor and label slots fall back to an estimate.
    pub(super) fn fallback() -> Self {
        let theme = Theme::dark();
        let sp = &theme.tokens.spacing;
        Self {
            scale: 1.0,
            lh_label: 17.0,
            lh_body: 20.0,
            lh_caption: 16.0,
            fx_w: 14.0,
            name_box_label_w: 80.0,
            tab_label_w: HashMap::new(),
            s1: f64::from(sp.s1),
            s2: f64::from(sp.s2),
            s3: f64::from(sp.s3),
            s4: f64::from(sp.s4),
            s6: f64::from(sp.s6),
            s16: f64::from(sp.s16),
            control_h: ControlSize::Small.height(&theme),
            field_h: ControlSize::Regular.height(&theme),
            grid: GridMetrics::fallback(),
            cards: CardMetrics::default(),
        }
    }

    /// An estimate of a label's width until the first draw has measured it.
    fn estimate(&self, label: &str) -> f64 {
        f64_of_usize(label.chars().count()) * 7.0 * self.scale
    }

    /// How much the chrome text has grown past the 1× reference; scales the
    /// floors and ceilings that bound text-driven slots.
    pub(super) fn text_growth(&self) -> f64 {
        self.scale.max(1.0)
    }

    /// Formula bar: a field with `s1` above and below.
    pub(super) fn formula_h(&self) -> f64 {
        self.field_h + 2.0 * self.s1
    }

    /// Name box: wide enough for [`NAME_BOX_REFERENCE`] plus `s3` each side,
    /// never below `s16 + s8`.
    pub(super) fn name_box_w(&self) -> f64 {
        (self.name_box_label_w + 2.0 * self.s3).max(self.s16 * 1.5)
    }

    /// The "fx" affordance slot: the label plus `s2` each side.
    pub(super) fn fx_slot_w(&self) -> f64 {
        self.fx_w + 2.0 * self.s2
    }

    /// Sheet tabs: a small segmented control with `s2` above and below, so a
    /// tab (and the editor that replaces it) never sits tight to the window edge.
    pub(super) fn tabs_h(&self) -> f64 {
        self.control_h + 2.0 * self.s2
    }

    /// Diameter of a tab's colour dot.
    pub(super) fn tab_dot(&self) -> f64 {
        self.s2
    }

    /// A sheet tab: its name plus `s3` each side, bounded between a floor and
    /// a ceiling (both grown with the text) so one long name can never push
    /// the add button and later tabs off-screen; the face elides a name that
    /// hits the ceiling. A tab with a colour reserves room for its dot on
    /// both sides, so its label stays centred.
    pub(super) fn tab_w(&self, name: &str, coloured: bool) -> f64 {
        let g = self.text_growth();
        let label = self
            .tab_label_w
            .get(name)
            .copied()
            .unwrap_or_else(|| self.estimate(name));
        let dot = if coloured {
            2.0 * (self.tab_dot() + self.s1)
        } else {
            0.0
        };
        (label + 2.0 * self.s3 + dot).clamp(self.s16 * g, self.s16 * 3.0 * g)
    }

    /// Top of a line box centred in `rect`.
    pub(super) fn centred_top(rect: Rect, line_h: f64) -> f64 {
        rect.y0 + (rect.height() - line_h) / 2.0
    }
}

impl SheetsApp {
    /// Re-resolve the chrome metrics against the renderer about to paint. A
    /// change re-runs `relayout` immediately, so the frame that follows paints
    /// (and later hit-tests) the measured geometry rather than the fallback.
    pub(super) fn refresh_chrome_metrics(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.sync_command_bar();
        self.chrome_theme = theme.clone();
        let metrics = ChromeMetrics::resolve(tr, theme, &self.sheet_names);
        // The tab being renamed grows with the name typed into it.
        let mut rename_moved = false;
        if let Some(rename) = &mut self.tab_rename {
            let (w, _) = tr.measure_style(
                rename.editor.current_text(),
                None,
                &theme.text(TextRole::Label),
            );
            let w = f64::from(w);
            if (w - rename.text_w).abs() > 0.5 {
                rename.text_w = w;
                rename_moved = true;
            }
        }
        if metrics != self.metrics || rename_moved {
            self.grid.set_metrics(metrics.grid);
            self.metrics = metrics;
            self.relayout();
        }
        // UI scale changes spacing without changing `text_scale`. Resolve
        // against the actual painter before opening an anchored menu, not
        // the toolbar's text-only layout cache.
        let rect = self.toolbar.get_rect();
        self.toolbar.lay_out_in(rect, tr, theme);
    }

    /// Lay out the bars, the grid and the sheet tabs inside the window.
    pub(super) fn relayout(&mut self) {
        let r = self.rect;
        self.lay_out_command_bar();
        let m = &self.metrics;
        self.formula_rect = Rect::new(
            r.x0,
            self.toolbar_rect.y1,
            r.x1,
            self.toolbar_rect.y1 + m.formula_h(),
        );
        self.tabs_rect = Rect::new(r.x0, r.y1 - m.tabs_h(), r.x1, r.y1);
        // The annotation dock stands between the grid and the tabs, so the
        // grid gives up the height instead of a card covering its cells.
        let (dock_h, dock_cards, dock_w) = self.dock_layout(Rect::new(
            r.x0,
            self.formula_rect.y1,
            r.x1,
            self.tabs_rect.y0,
        ));
        self.dock_cards = dock_cards;
        self.dock_rect = Rect::new(r.x0, self.tabs_rect.y0 - dock_h, r.x1, self.tabs_rect.y0);
        self.dock_scroll
            .set_viewport(self.dock_rect, &self.chrome_theme);
        self.dock_scroll.set_content_size(dock_w, dock_h);
        self.grid_rect = Rect::new(r.x0, self.formula_rect.y1, r.x1, self.dock_rect.y0);
        self.grid.rect = self.grid_rect;

        // Formula bar: name box, fx, field, all on one centre line at one
        // field height.
        let field_y0 = self.formula_rect.y0 + m.s1;
        let field_y1 = self.formula_rect.y1 - m.s1;
        let nb_x0 = self.formula_rect.x0 + m.s3;
        let nb_x1 = nb_x0 + m.name_box_w();
        self.name_box_rect = Rect::new(nb_x0, field_y0, nb_x1, field_y1);
        let fx_x0 = nb_x1 + m.s2;
        self.fx_rect = Rect::new(fx_x0, field_y0, fx_x0 + m.fx_slot_w(), field_y1);
        self.formula_field_rect = Rect::new(
            self.fx_rect.x1,
            field_y0,
            (self.formula_rect.x1 - m.s3).max(self.fx_rect.x1),
            field_y1,
        );

        // Sheet tabs: contiguous segments, each its name plus padding, then
        // the add button.
        self.tab_rects.clear();
        let mut tx = self.tabs_rect.x0 + m.s3;
        let ty = self.tabs_rect.y0 + m.s2;
        let th = m.control_h;
        for (i, name) in self.sheet_names.iter().enumerate() {
            let coloured = self.sheet_colors.get(i).copied().flatten().is_some();
            let mut w = m.tab_w(name, coloured);
            if let Some(rename) = self.tab_rename.as_ref().filter(|rename| rename.idx == i) {
                // The editor stands on the tab's own slot, wide enough for
                // what is typed; the tabs after it move over.
                w = w.max(rename.text_w + 4.0 * m.s3);
            }
            self.tab_rects.push((Rect::new(tx, ty, tx + w, ty + th), i));
            tx += w;
        }
        tx += m.s2;
        self.add_tab_rect = Rect::new(tx, ty, tx + th, ty + th);
        // An in-flight rename follows its tab through the reflow, so the
        // editor never detaches from the tab it renames.
        if let Some(idx) = self.tab_rename.as_ref().map(|rn| rn.idx) {
            let rect = self.tab_rename_rect(idx);
            if let Some(rn) = &mut self.tab_rename {
                rn.rect = rect;
            }
        }
        self.relayout_overlays();
    }

    /// The header row across the top of the window: the command bar in its
    /// actions band and, when the host offers it, the window's History
    /// control in the trailing corner. What does not fit moves into the
    /// toolbar's own More menu.
    fn lay_out_command_bar(&mut self) {
        let theme = &self.chrome_theme;
        let face = HeaderRowFace::new(self.rect, theme)
            .with_inset_title()
            .with_history_if(self.history.is_shown());
        self.toolbar_rect = face.row;
        self.history_rect = face.history_rect();
        let chip = face.metrics().chip;
        let band = face.actions;
        let y0 = band.center().y - chip / 2.0;
        self.toolbar
            .lay_out_at(Rect::new(band.x0, y0, band.x1, y0 + chip), theme);
    }

    /// The command bar: File and Format open their menus, Bold and Formulas
    /// are toggles, and each command reports what it runs through `slot`.
    pub(super) fn build_command_bar(slot: &CommandSlot) -> Toolbar {
        let file = menu_rows(slot, Self::file_menu_rows());
        let run = |item: MenuItem| {
            let slot = std::sync::Arc::clone(slot);
            move || post(&slot, item.clone())
        };
        Toolbar::new(Orientation::Horizontal)
            .push_menu("File", file)
            .with_priority(1)
            .push_menu("Format", Vec::new())
            .with_priority(1)
            .push_separator()
            .push_icon_toggle(Icon::Bold, "Bold", false, run(MenuItem::ToggleBold))
            .push_button("AutoSum", run(MenuItem::AutoSum))
            .push_toggle("Formulas", false, run(MenuItem::ToggleFormulaView))
            .with_accessible_name("Show formulas")
            .push_icon(
                Icon::Settings,
                "Document settings",
                run(MenuItem::OpenDocSettings),
            )
    }

    /// Bring the bar's toggles and the Format menu's check marks in line
    /// with the sheet.
    pub(super) fn sync_command_bar(&mut self) {
        let formulas = self.grid.formula_view();
        self.toolbar.set_active(bar::BOLD, self.active_cell_bold);
        self.toolbar.set_active(bar::FORMULAS, formulas);
        self.toolbar.set_accessible_name(
            bar::FORMULAS,
            if formulas {
                "Show calculated results"
            } else {
                "Show formulas"
            },
        );
        let rows = self.format_menu_rows();
        if self.format_rows_shown.as_ref() != Some(&rows) {
            self.toolbar
                .set_menu(bar::FORMAT, menu_rows(&self.command_slot, rows.clone()));
            self.format_rows_shown = Some(rows);
        }
    }

    /// Route an event to the header row: the History control, then the
    /// command bar. While one of the bar's menus is open the bar takes every
    /// event; otherwise it keeps the presses that land on it and lets the
    /// pointer's motion through to the sheet as well. Returns whether the
    /// event was consumed.
    pub(super) fn route_command_bar(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let hovered = self.history.is_hovered();
        let took = self
            .history
            .handle_event(event, self.history_rect, ctx.scale_factor);
        if hovered != self.history.is_hovered() {
            self.pending.redraw = true;
        }
        if took {
            *ctx.handled = true;
            self.pending.redraw = true;
            return true;
        }
        let modal = self.toolbar.wants_pointer_capture();
        let before = *ctx.handled;
        self.toolbar
            .handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
        let ran = self.drain_command_bar(&mut ctx.state);
        let consumed = *ctx.handled && !before;
        if consumed || ran {
            self.pending.redraw = true;
        }
        if modal || self.toolbar.wants_pointer_capture() {
            *ctx.handled = true;
            return true;
        }
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            *ctx.handled = before;
            return false;
        }
        consumed || ran
    }

    /// Run the command the bar or one of its menus reported, if any.
    pub(super) fn drain_command_bar(&mut self, doc: &mut ui::widget::Edits<'_>) -> bool {
        let item = self
            .command_slot
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        let Some(item) = item else {
            return false;
        };
        self.run_menu_item(doc, &item);
        self.pending.redraw = true;
        true
    }

    // --- painting ------------------------------------------------------------

    /// The chrome, top to bottom. The bars stand on the window's own ground;
    /// the framework painted it already.
    pub(super) fn draw_chrome(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.draw_command_bar(scene, transform, tr, theme, presence);
        self.draw_name_box(scene, transform, tr, theme);
        self.draw_formula_field(scene, transform, tr, theme);
        self.draw_sheet_tabs_strip(scene, transform, tr, theme);
    }

    /// The header row: the command bar over one hairline, and the window's
    /// History control in its corner. The bar's menus float within the
    /// window, so its viewport is the window while it paints.
    fn draw_command_bar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        toolbar::paint_band(scene, transform, theme, self.toolbar_rect);
        let outer = presence.push_viewport(self.float_bounds());
        self.toolbar.draw(scene, transform, tr, theme, presence);
        presence.pop_viewport(outer);
        self.history
            .paint(scene, transform, tr, theme, self.history_rect);
    }

    /// The field face of the formula bar: its raised ground with the derived
    /// edge, ringed in your colour while it has focus.
    fn paint_field(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect, focused: bool) {
        FieldFace::new(rect, "")
            .focused(focused)
            .paint_chrome(scene, transform, theme);
    }

    /// Formula bar's name box: its field, and either the live edit buffer or
    /// the selection label, in the same role as the formula text.
    fn draw_name_box(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let name_box = self.name_box_rect;
        Self::paint_field(
            scene,
            transform,
            theme,
            name_box,
            self.name_box_edit.is_some(),
        );
        let inset = self.metrics.s3;
        let text_rect = Rect::new(
            name_box.x0 + inset,
            name_box.y0,
            name_box.x1 - inset,
            name_box.y1,
        );
        if let Some(input) = &mut self.name_box_edit {
            // The shared editor owns shaping, scrolling, selection, clipboard,
            // IME and caret hit-testing inside its content band.
            input.apply_layout_external(text_rect);
            input.draw(scene, transform, tr, theme, &PresenceCtx::default());
        } else {
            let style = theme.text(TextRole::Body);
            let label = self.grid.selection_label();
            let (_, line_h) = tr.measure_style(&label, None, &style);
            tr.draw_style(
                scene,
                &label,
                colors.text_primary,
                transform
                    * Affine::translate((
                        text_rect.x0,
                        ChromeMetrics::centred_top(name_box, f64::from(line_h)),
                    )),
                None,
                &style,
            );
        }
    }

    /// Formula bar's "fx" field: the "fx" label, the input field, the live text
    /// (reference-tinted while editing or selected), selection highlight and
    /// caret. Rebuilds `self.formula_char_x` for click/caret mapping every
    /// frame, since it must track exactly what's drawn.
    pub(super) fn draw_formula_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.draw_formula_fx_label(scene, transform, tr, theme);
        let editing = self.grid.is_editing();
        let field = self.formula_field_rect;
        Self::paint_field(scene, transform, theme, field, editing);

        let bar = self.grid.formula_bar();
        let content = bar.text.as_str();
        let style = theme.text(TextRole::Body);
        let inset = self.metrics.s3;
        let text_width = f64::from(tr.measure_style(content, None, &style).0);
        let available = (field.width() - 2.0 * inset).max(1.0);
        if let Some(caret) = bar.caret {
            let prefix: String = content.chars().take(caret).collect();
            let caret_x = f64::from(tr.measure_style(&prefix, None, &style).0);
            // Leave a little breathing room after the caret, including at End.
            let trailing = (available - self.metrics.s1).max(0.0);
            self.formula_scroll_x = self
                .formula_scroll_x
                .max(caret_x - trailing)
                .min(caret_x)
                .max(0.0)
                .min((text_width - trailing).max(0.0));
        } else {
            self.formula_scroll_x = 0.0;
        }
        let text_x0 = field.x0 + inset - self.formula_scroll_x;
        // Rebuild the per-character x boundaries for click/caret mapping. This
        // is derived from exactly the text being drawn, so a click maps to the
        // same glyph the user sees (whether it's the live edit buffer or the
        // active cell's stored input).
        self.formula_char_x = char_boundaries(tr, content, &style, text_x0);
        let (_, line_h) = tr.measure_style("Ag", None, &style);
        let line_h = f64::from(line_h);
        let top = ChromeMetrics::centred_top(field, line_h);
        let radius = f64::from(theme.tokens.radii.control);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::from_rect(
                Rect::new(field.x0 + inset, field.y0, field.x1 - inset, field.y1),
                radius,
            ),
        );
        crate::grid::draw_edit_line(
            scene,
            transform,
            tr,
            theme,
            &style,
            &crate::grid::EditLine {
                text: content,
                origin: Point::new(text_x0, top),
                line_h,
                selection: bar.selection,
                caret: bar.caret,
                preedit: bar.preedit,
            },
        );
        scene.pop_layer();
    }

    fn draw_formula_fx_label(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        // The label is centred in the same `fx_rect` that `handle_event`
        // hit-tests, so what the pointer lands on is exactly what is painted.
        let style = theme.text(TextRole::Label).with_italic(true);
        let m = &self.metrics;
        tr.draw_style(
            scene,
            "fx",
            theme.tokens.colors.text_dim,
            transform
                * Affine::translate((
                    self.fx_rect.center().x - m.fx_w / 2.0,
                    ChromeMetrics::centred_top(self.fx_rect, m.lh_label),
                )),
            None,
            &style,
        );
    }

    /// Bottom strip: the sheet tabs and the add-sheet button. The selection
    /// summary and any transient action message are surfaced separately as a
    /// floating status bubble, so this strip reads purely as navigation.
    fn draw_sheet_tabs_strip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let m = &self.metrics;
        // One hairline on the strip's first pixel row, on the ground it
        // stands on.
        separator(
            scene,
            transform,
            self.tabs_rect.x0,
            self.tabs_rect.x1,
            self.tabs_rect.y0 + 0.5,
            0.0,
            colors.surfaces.content,
        );
        if !self.tab_rects.is_empty() {
            let rects: Vec<Rect> = self.tab_rects.iter().map(|(rect, _)| *rect).collect();
            let labels: Vec<&str> = self
                .tab_rects
                .iter()
                .map(|(_, idx)| self.sheet_names.get(*idx).map_or("", String::as_str))
                .collect();
            let selected = self
                .tab_rects
                .iter()
                .position(|(_, idx)| *idx == self.active_sheet)
                .unwrap_or(0);
            let hovered = self
                .tab_rects
                .iter()
                .position(|(rect, _)| rect.contains(self.last_mouse));
            let face = SegmentedFace::new(&rects, &labels, selected)
                .style(SegmentedStyle::Quiet)
                .control_size(ControlSize::Small)
                .hovered(hovered);
            ui::widget::ToggleGroup::paint(scene, transform, tr, theme, &face);
            // A tab's colour is the sheet's own metadata: a dot in the
            // sheet's series colour at the tab's leading edge.
            for (rect, idx) in &self.tab_rects {
                let Some(slot) = self.sheet_colors.get(*idx).copied().flatten() else {
                    continue;
                };
                let d = m.tab_dot();
                let mark = Rect::from_center_size(
                    Point::new(rect.x0 + m.s3 + d / 2.0, rect.center().y),
                    (d, d),
                );
                ColorSwatchFace::new(mark, tab_palette_color(theme, slot), "Tab colour")
                    .paint(scene, transform, tr, theme);
            }
        }
        let hovered = self.add_tab_rect.contains(self.last_mouse);
        let face = ButtonFace::icon_only(self.add_tab_rect, Icon::Plus, "Add sheet")
            .control_size(ControlSize::Small)
            .hovered(hovered);
        Button::paint(scene, transform, tr, theme, &face);
    }

    /// The area the status bubble floats in: the grid body, clear of the
    /// scroll bar gutters and the sheet tabs.
    pub(super) fn status_area(&self) -> Rect {
        Rect::new(
            self.grid_rect.x0,
            self.grid_rect.y0,
            self.grid_rect.x1 - self.grid.scrollbar_thickness(),
            self.grid_rect.y1 - self.grid.scrollbar_thickness(),
        )
    }

    /// Paint the transient status bubble over the grid's bottom-trailing
    /// corner. It shows when the composed status text changes (a selection
    /// move, a fresh action message) or while the pointer rests in its corner,
    /// then fades.
    pub(super) fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        let area = self.status_area();
        // Resting in the bubble's corner pins it open; so does a staged
        // capture.
        let gutter = self.metrics.s6;
        let zone = self
            .status_bubble
            .rect_in(area, tr, theme)
            .inflate(gutter, gutter);
        let over = !self.status_bubble.text().is_empty()
            && (self.staged_status || zone.contains(self.last_mouse));
        self.status_bubble.set_hover(over, now);
        self.status_bubble
            .paint(scene, transform, tr, theme, area, now);
    }

    /// Paint per-peer "presence-in-navigation" on the sheet tabs: for each
    /// sheet tab a remote peer is currently on, wash the tab in that peer's
    /// colour. Reads the same `PresenceKey::Tab { id: sheet_name }` scope the
    /// tabs are keyed by, so a peer who switches to another sheet lights up
    /// the tab that leads there.
    pub(super) fn draw_sheet_tab_presence(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for (rect, idx) in &self.tab_rects {
            let Some(name) = self.sheet_names.get(*idx) else {
                continue;
            };
            let mut peers = presence
                .peers_on(&PresenceKey::Tab { id: name.clone() })
                .to_vec();
            if let Some(sheet) = self.sheet_ids.get(*idx) {
                for (key, users) in &presence.here {
                    if super::presence::CellScope::from_key(key)
                        .is_some_and(|scope| scope.sheet == sheet.as_str())
                    {
                        peers.extend(users.iter().cloned());
                    }
                }
            }
            peers.sort_by(|a, b| a.0.cmp(&b.0));
            peers.dedup();
            let p0 = transform * Point::new(rect.x0, rect.y0);
            let p1 = transform * Point::new(rect.x1, rect.y1);
            let mist_rect = Rect::new(p0.x, p0.y, p1.x, p1.y);
            for user in peers {
                presence.draw_peer_mist(scene, theme, &user, mist_rect);
            }
        }
    }

    /// The inline tab-rename editor, over the tab it renames.
    pub(super) fn draw_tab_rename(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(rn) = &mut self.tab_rename else {
            return;
        };
        rn.editor.apply_layout_external(rn.rect);
        rn.editor
            .draw(scene, transform, tr, theme, &PresenceCtx::default());
    }

    /// Where the inline rename editor for tab `idx` sits: exactly on the tab's
    /// own slot. `relayout` widens the slot for a longer name and moves the
    /// later tabs aside, so the editor never covers a neighbour, and it
    /// follows its tab through any reflow.
    pub(super) fn tab_rename_rect(&self, idx: usize) -> Rect {
        self.tab_rects
            .iter()
            .find(|(_, i)| *i == idx)
            .map_or(self.add_tab_rect, |(r, _)| *r)
    }
}

/// Cumulative x positions (layout space) of every character boundary in
/// `text` when laid out in `style` starting at `x0`. The returned vector has
/// `text.chars().count() + 1` entries: `[0]` is `x0` and the last is the text's
/// right edge. Used to map a click x to a caret index and to place the caret.
pub(super) fn char_boundaries(
    tr: &mut TextRenderer,
    text: &str,
    style: &TextStyle,
    x0: f64,
) -> Vec<f64> {
    let chars: Vec<char> = text.chars().collect();
    let mut xs = Vec::with_capacity(chars.len() + 1);
    xs.push(x0);
    let mut prefix = String::new();
    for ch in &chars {
        prefix.push(*ch);
        let (w, _) = tr.measure_style(&prefix, None, style);
        xs.push(x0 + f64::from(w));
    }
    xs
}

/// What the command bar last ran, waiting for the app to act on it with the
/// document in hand.
pub(super) type CommandSlot = std::sync::Arc<std::sync::Mutex<Option<MenuItem>>>;

/// Report `item` through `slot`.
fn post(slot: &CommandSlot, item: MenuItem) {
    if let Ok(mut pending) = slot.lock() {
        *pending = Some(item);
    }
}

/// A command menu's rows as toolkit menu rows that report through `slot`.
fn menu_rows(slot: &CommandSlot, rows: Vec<MenuRow>) -> Vec<ui::widget::MenuItem> {
    rows.into_iter()
        .map(|(label, item, checked)| match item {
            None => ui::widget::MenuItem::separator(),
            Some(item) => {
                let slot = std::sync::Arc::clone(slot);
                let row = ui::widget::MenuItem::action(label, move || post(&slot, item.clone()));
                match checked {
                    Some(on) => row.with_checked(on),
                    None => row,
                }
            }
        })
        .collect()
}

/// One row of a command menu: its label, what it runs (`None` for a
/// separator) and its check state when it has one.
pub(super) type MenuRow = (String, Option<MenuItem>, Option<bool>);
