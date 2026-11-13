//! The chrome around the grid: formula bar, sheet tabs and the status line.

use super::annotations::CardMetrics;
use super::{
    Affine, BlendMode, Button, Fill, GridMetrics, HashMap, MenuItem, Point, PresenceCtx,
    PresenceKey, Rect, RoundedRect, Scene, SheetsApp, TextRenderer, TextRole, TextStyle, Theme,
    tab_palette_color,
};
use ui::icons::Icon;
use ui::num::f64_of_usize;
use ui::surface::separator;
use ui::widget::DrawChild;
use ui::widget::paint_damage::PaintParts;
use ui::widget::{
    ButtonFace, ColorSwatchFace, ControlSize, FieldFace, SegmentedFace, SegmentedStyle,
};

/// Reference label the name box is sized for, so it never resizes with the
/// selection and always fits a two-corner range reference.
const NAME_BOX_REFERENCE: &str = "ZZ999:ZZ999";

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
    pub scale: f64,
    pub lh_label: f64,
    pub lh_body: f64,
    pub lh_caption: f64,
    pub fx_w: f64,
    pub name_box_label_w: f64,
    pub tab_label_w: HashMap<String, f64>,
    pub s1: f64,
    pub s2: f64,
    pub s3: f64,
    pub s4: f64,
    pub s6: f64,
    pub s16: f64,
    pub control_h: f64,
    pub field_h: f64,
    pub grid: GridMetrics,
    pub cards: CardMetrics,
}

impl ChromeMetrics {
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

    pub(super) fn centred_top(rect: Rect, line_h: f64) -> f64 {
        rect.y0 + (rect.height() - line_h) / 2.0
    }
}

impl SheetsApp {
    pub(super) fn refresh_chrome_metrics(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.chrome_theme = theme.clone();
        let metrics = ChromeMetrics::resolve(tr, theme, &self.sheet_names);
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
    }

    pub(super) fn relayout(&mut self) {
        let r = self.rect;
        let m = &self.metrics;
        self.formula_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + m.formula_h());
        self.tabs_rect = Rect::new(r.x0, r.y1 - m.tabs_h(), r.x1, r.y1);
        let content_top = self.formula_rect.y1;
        let (dock_h, dock_cards, dock_w) =
            self.dock_layout(Rect::new(r.x0, content_top, r.x1, self.tabs_rect.y0));
        self.dock_cards = dock_cards;
        self.dock_rect = Rect::new(r.x0, self.tabs_rect.y0 - dock_h, r.x1, self.tabs_rect.y0);
        self.dock_scroll
            .set_viewport(self.dock_rect, &self.chrome_theme);
        self.dock_scroll.set_content_size(dock_w, dock_h);
        self.grid_rect = Rect::new(r.x0, content_top, r.x1, self.dock_rect.y0);
        self.grid.rect = self.grid_rect;

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

        self.tab_rects.clear();
        let mut tx = self.tabs_rect.x0 + m.s3;
        let ty = self.tabs_rect.y0 + m.s2;
        let th = m.control_h;
        for (i, name) in self.sheet_names.iter().enumerate() {
            let coloured = self.sheet_colors.get(i).copied().flatten().is_some();
            let mut w = m.tab_w(name, coloured);
            if let Some(rename) = self.tab_rename.as_ref().filter(|rename| rename.idx == i) {
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

    pub(super) fn draw_chrome(
        &mut self,
        parts: &mut PaintParts,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let _ = presence;
        let reach = ui::widget::focus_ring_reach(theme);
        let name_box = self.name_box_rect.inflate(reach, reach);
        parts.draw(scene, "sheet-name-box", name_box, |scene| {
            self.draw_name_box(scene, transform, tr, theme);
        });
        let formula = self
            .fx_rect
            .union(self.formula_field_rect)
            .inflate(reach, reach);
        parts.draw(scene, "sheet-formula", formula, |scene| {
            self.draw_formula_field(scene, transform, tr, theme);
        });
        let tabs = self.tabs_rect.inflate(reach, reach);
        parts.draw(scene, "sheet-tabs", tabs, |scene| {
            self.draw_sheet_tabs_strip(scene, transform, tr, theme);
        });
    }

    fn paint_field(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect, focused: bool) {
        FieldFace::new(rect, "")
            .focused(focused)
            .paint_chrome(scene, transform, theme);
    }

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
            input.apply_layout_external(text_rect);
            input.draw_child(scene, transform, tr, theme, &PresenceCtx::default());
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

    fn draw_sheet_tabs_strip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let m = &self.metrics;
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
                .style(SegmentedStyle::Tabs)
                .control_size(ControlSize::Small)
                .hovered(hovered);
            ui::widget::ToggleGroup::paint(scene, transform, tr, theme, &face);
            for (rect, idx) in &self.tab_rects {
                let Some(slot) = self.sheet_colors.get(*idx).copied().flatten() else {
                    continue;
                };
                let d = m.tab_dot();
                let mark = Rect::from_center_size(
                    Point::new(rect.x0 + m.s3 + d / 2.0, rect.center().y),
                    (d, d),
                );
                ColorSwatchFace::new(
                    mark,
                    tab_palette_color(theme, slot),
                    crate::resource::CATALOG.t("sheets-tab-colour"),
                )
                .paint(scene, transform, tr, theme);
            }
        }
        let hovered = self.add_tab_rect.contains(self.last_mouse);
        let add_sheet = crate::resource::CATALOG.t("sheets-add-sheet");
        let face = ButtonFace::icon_only(self.add_tab_rect, Icon::Plus, &add_sheet)
            .control_size(ControlSize::Small)
            .hovered(hovered);
        Button::paint(scene, transform, tr, theme, &face);
    }

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
            .draw_child(scene, transform, tr, theme, &PresenceCtx::default());
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

pub(super) type CommandSlot = std::sync::Arc<std::sync::Mutex<Option<MenuItem>>>;

pub(super) fn post(slot: &CommandSlot, item: MenuItem) {
    if let Ok(mut pending) = slot.lock() {
        *pending = Some(item);
    }
}

pub(super) type MenuRow = (String, Option<MenuItem>, Option<bool>);
