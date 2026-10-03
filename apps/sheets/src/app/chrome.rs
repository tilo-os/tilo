//! The window chrome around the grid: the command bar, the formula bar and the
//! sheet tabs, plus the transient status bubble and the loading state.
//!
//! Everything is laid out from measured role line boxes and spacing tokens, and
//! painted through the toolkit's faces (`ButtonFace`, `SegmentedFace`,
//! `StatusBubble`), so a control looks and sizes like its retained twin.
//! The bars stand on the window ground; the command bar is separated from the
//! formula bar by one hairline and the sheet tabs from the grid by another.

use super::{
    Affine, BlendMode, Button, Fill, GridMetrics, HashMap, MenuItem, OpenMenu, Point, PresenceCtx,
    PresenceKey, Rect, RoundedRect, Scene, SheetsApp, TextRenderer, TextRole, TextStyle, Theme,
    Widget, count_f64, tab_palette_color,
};
use ui::icons::Icon;
use ui::surface::separator;
use ui::widget::toolbar;
use ui::widget::{
    ButtonFace, ButtonRole, ColorSwatchFace, ControlSize, SegmentedFace, SegmentedStyle,
    paint_focus_ring,
};

/// Reference label the name box is sized for, so it never resizes with the
/// selection and always fits a two-corner range reference.
const NAME_BOX_REFERENCE: &str = "ZZ999:ZZ999";

/// The command bar's commands in order: label, the menu it opens, the action
/// it fires. The first two are menus and never leave the bar; the rest move
/// into the More menu, in order, when the window is too narrow for them. One
/// table feeds layout, paint, the label pre-measurement and the More menu, so
/// a button is always exactly as wide as the text it paints.
pub(super) const BAR_COMMANDS: [(&str, Option<OpenMenu>, Option<MenuItem>); 6] = [
    ("File", Some(OpenMenu::File), None),
    ("Format", Some(OpenMenu::Format), None),
    ("Bold", None, Some(MenuItem::ToggleBold)),
    ("AutoSum", None, Some(MenuItem::AutoSum)),
    ("Formulas", None, Some(MenuItem::ToggleFormulaView)),
    ("Document settings", None, Some(MenuItem::OpenDocSettings)),
];

/// How many of the commands never leave the bar.
const PINNED_COMMANDS: usize = 2;

/// Accessible name of the overflow button.
pub(super) const MORE_LABEL: &str = "More commands";

/// One button of the command bar.
pub(super) struct MenuButton {
    pub rect: Rect,
    pub label: String,
    /// `None` for the commands that fire an action instead of opening a menu.
    pub item: Option<MenuItem>,
    pub opens: Option<OpenMenu>,
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
    /// Intrinsic width of every command button, by label.
    pub command_w: HashMap<String, f64>,
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
}

impl ChromeMetrics {
    /// Resolve every chrome metric for `theme` against `tr`. `commands` and
    /// `tabs` are the strings `relayout` will size a slot for.
    pub(super) fn resolve(
        tr: &mut TextRenderer,
        theme: &Theme,
        commands: &[&str],
        tabs: &[String],
    ) -> Self {
        let label = theme.text(TextRole::Label);
        let body = theme.text(TextRole::Body);
        let (_, lh_label) = tr.measure_style("Ag", None, &label);
        let (_, lh_body) = tr.measure_style("Ag", None, &body);
        let (_, lh_caption) = tr.measure_style("Ag", None, &theme.text(TextRole::Caption));
        let (fx_w, _) = tr.measure_style("fx", None, &label.clone().with_italic(true));
        let (name_box_label_w, _) = tr.measure_style(NAME_BOX_REFERENCE, None, &body);
        let command_w = commands
            .iter()
            .map(|command| {
                let width = ButtonFace::new(Rect::ZERO, command)
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small)
                    .intrinsic_width(tr, theme);
                ((*command).to_owned(), width)
            })
            .collect();
        let strong = label.strong();
        let tab_label_w = tabs
            .iter()
            .map(|name| {
                let (w, _) = tr.measure_style(name, None, &strong);
                (name.clone(), f64::from(w))
            })
            .collect();
        let sp = &theme.tokens.spacing;
        Self {
            scale: f64::from(theme.text_scale),
            lh_label: f64::from(lh_label),
            lh_body: f64::from(lh_body),
            lh_caption: f64::from(lh_caption),
            fx_w: f64::from(fx_w),
            name_box_label_w: f64::from(name_box_label_w),
            command_w,
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
        }
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
            command_w: HashMap::new(),
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
        }
    }

    /// An estimate of a label's width until the first draw has measured it.
    fn estimate(&self, label: &str) -> f64 {
        count_f64(label.chars().count()) * 7.0 * self.scale
    }

    /// A command button: its intrinsic width.
    pub(super) fn command_w(&self, label: &str) -> f64 {
        self.command_w
            .get(label)
            .copied()
            .unwrap_or_else(|| self.estimate(label) + 2.0 * self.s3)
    }

    /// How much the chrome text has grown past the 1× reference; scales the
    /// floors and ceilings that bound text-driven slots.
    pub(super) fn text_growth(&self) -> f64 {
        self.scale.max(1.0)
    }

    /// Command bar: a small control with `s2` above and below.
    pub(super) fn toolbar_h(&self) -> f64 {
        self.control_h + 2.0 * self.s2
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

    /// Sheet tabs: a small segmented control with `s1` above and below.
    pub(super) fn tabs_h(&self) -> f64 {
        self.control_h + 2.0 * self.s1
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
    /// Every chrome string `relayout` reserves a slot for, so
    /// [`ChromeMetrics::resolve`] can measure each one in the style it is
    /// painted in: the commands, then the sheet names.
    fn chrome_labels(&self) -> (Vec<&'static str>, Vec<String>) {
        (
            BAR_COMMANDS.iter().map(|(label, ..)| *label).collect(),
            self.sheet_names.clone(),
        )
    }

    /// Re-resolve the chrome metrics against the renderer about to paint. A
    /// change re-runs `relayout` immediately, so the frame that follows paints
    /// (and later hit-tests) the measured geometry rather than the fallback.
    pub(super) fn refresh_chrome_metrics(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let (commands, tabs) = self.chrome_labels();
        let metrics = ChromeMetrics::resolve(tr, theme, &commands, &tabs);
        if metrics == self.metrics {
            return;
        }
        self.grid.set_metrics(metrics.grid);
        self.metrics = metrics;
        self.relayout();
    }

    /// Lay out the bars, the grid and the sheet tabs inside the window.
    pub(super) fn relayout(&mut self) {
        let r = self.rect;
        let m = &self.metrics;
        self.toolbar_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + m.toolbar_h());
        self.formula_rect = Rect::new(
            r.x0,
            self.toolbar_rect.y1,
            r.x1,
            self.toolbar_rect.y1 + m.formula_h(),
        );
        self.tabs_rect = Rect::new(r.x0, r.y1 - m.tabs_h(), r.x1, r.y1);
        self.grid_rect = Rect::new(r.x0, self.formula_rect.y1, r.x1, self.tabs_rect.y0);
        self.grid.rect = self.grid_rect;

        self.menu_buttons = Self::command_buttons(m, self.toolbar_rect);

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
        let ty = self.tabs_rect.y0 + m.s1;
        let th = m.control_h;
        for (i, name) in self.sheet_names.iter().enumerate() {
            let coloured = self.sheet_colors.get(i).copied().flatten().is_some();
            let w = m.tab_w(name, coloured);
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

    /// Command bar: each button is its intrinsic width on one centre line,
    /// `s2` apart. What does not fit after the pinned menus moves into the
    /// More menu, whose button takes the last slot.
    fn command_buttons(m: &ChromeMetrics, bar: Rect) -> Vec<MenuButton> {
        let mut buttons = Vec::new();
        let by = bar.y0 + (bar.height() - m.control_h) / 2.0;
        let x0 = bar.x0 + m.s3;
        let right = bar.x1 - m.s3;
        let widths: Vec<f64> = BAR_COMMANDS
            .iter()
            .map(|(label, ..)| m.command_w(label))
            .collect();
        let all: f64 =
            widths.iter().sum::<f64>() + m.s2 * count_f64(widths.len().saturating_sub(1));
        let overflowing = x0 + all > right;
        // The More button is square; leave room for it when anything spills.
        let reserve = if overflowing { m.control_h + m.s2 } else { 0.0 };
        let mut x = x0;
        let mut shown = 0;
        for (index, (label, opens, item)) in BAR_COMMANDS.into_iter().enumerate() {
            let w = widths.get(index).copied().unwrap_or(0.0);
            if index >= PINNED_COMMANDS && x + w + reserve > right {
                break;
            }
            buttons.push(MenuButton {
                rect: Rect::new(x, by, x + w, by + m.control_h),
                label: label.to_owned(),
                item,
                opens,
            });
            x += w + m.s2;
            shown = index + 1;
        }
        if shown < BAR_COMMANDS.len() {
            buttons.push(MenuButton {
                rect: Rect::new(x, by, x + m.control_h, by + m.control_h),
                label: MORE_LABEL.to_owned(),
                item: None,
                opens: Some(OpenMenu::More),
            });
        }
        buttons
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
    ) {
        self.draw_command_bar(scene, transform, tr, theme);
        self.draw_name_box(scene, transform, tr, theme);
        self.draw_formula_field(scene, transform, tr, theme);
        self.draw_sheet_tabs_strip(scene, transform, tr, theme);
    }

    /// Whether the command at `item` is a toggle that is on right now.
    pub(super) fn command_is_toggled(&self, item: Option<&MenuItem>) -> bool {
        item.is_some_and(|item| self.command_state(item))
    }

    /// Command bar: the commands as plain small buttons (a toggle that is on is
    /// a lift, a menu that is open is pressed) over one hairline.
    fn draw_command_bar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        toolbar::paint_band(scene, transform, theme, self.toolbar_rect);
        for b in &self.menu_buttons {
            let open = b.opens.is_some() && b.opens == Some(self.open_menu);
            let on = self.command_is_toggled(b.item.as_ref());
            if on {
                // A toggle that is on is the lift of the bar's ground.
                ui::chrome::paint_row_selection(
                    scene,
                    transform,
                    b.rect,
                    ui::chrome::SelectionState::new(true, false, false),
                    theme.tokens.colors.surfaces.content,
                    theme,
                );
            }
            let face = if b.opens == Some(OpenMenu::More) {
                ButtonFace::icon_only(b.rect, Icon::MoreHorizontal, MORE_LABEL)
            } else {
                ButtonFace::new(b.rect, &b.label)
            }
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small)
            .hovered(!on && b.rect.contains(self.last_mouse))
            .pressed(open);
            Button::paint(scene, transform, tr, theme, &face);
        }
    }

    /// The field face of the formula bar: its raised ground with the derived
    /// edge, ringed in your colour while it has focus.
    fn paint_field(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect, focused: bool) {
        let radius = f64::from(theme.tokens.radii.control);
        ui::surface::card(
            scene,
            transform,
            rect,
            radius,
            theme.tokens.colors.surfaces.raised,
        );
        if focused {
            paint_focus_ring(scene, transform, rect, radius, theme);
        }
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
    fn draw_formula_field(
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

        let content = self.grid.active_display_raw();
        let style = theme.text(TextRole::Body);
        let text_x0 = field.x0 + self.metrics.s3;
        // Rebuild the per-character x boundaries for click/caret mapping. This
        // is derived from exactly the text being drawn, so a click maps to the
        // same glyph the user sees (whether it's the live edit buffer or the
        // active cell's stored input).
        self.formula_char_x = char_boundaries(tr, &content, &style, text_x0);
        let (_, line_h) = tr.measure_style("Ag", None, &style);
        let line_h = f64::from(line_h);
        let top = ChromeMetrics::centred_top(field, line_h);
        let radius = f64::from(theme.tokens.radii.control);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::from_rect(field, radius),
        );
        crate::grid::draw_edit_line(
            scene,
            transform,
            tr,
            theme,
            &style,
            &crate::grid::EditLine {
                text: &content,
                origin: Point::new(text_x0, top),
                line_h,
                selection: if editing {
                    self.grid.edit_selection()
                } else {
                    None
                },
                caret: if editing {
                    self.grid.edit_caret()
                } else {
                    None
                },
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
                .style(SegmentedStyle::Tabs)
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

    /// Whether the workbook is still being read from disk.
    pub(super) fn is_loading(&self) -> bool {
        (self.run.persists() && self.persistence_started && !self.pending.seeded)
            || self.shots_loading
    }

    /// While a persistent workbook is decoded on the capacity-one worker the
    /// chrome stays, but the grid is not shown: one line says what is
    /// happening. It is drawn from state rather than a timer, so reduced
    /// motion and a slow disk behave identically.
    pub(super) fn draw_loading(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let style = theme.text(TextRole::Body);
        let label = "Opening workbook";
        let (width, height) = tr.measure_style(label, None, &style);
        let centre = self.grid_rect.center();
        tr.draw_style(
            scene,
            label,
            theme.tokens.colors.text_dim,
            transform
                * Affine::translate((
                    centre.x - f64::from(width) / 2.0,
                    centre.y - f64::from(height) / 2.0,
                )),
            None,
            &style,
        );
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
            && (self.shots_status || zone.contains(self.last_mouse));
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
            let peers = presence.peers_on(&PresenceKey::Tab { id: name.clone() });
            if peers.is_empty() {
                continue;
            }
            let p0 = transform * Point::new(rect.x0, rect.y0);
            let p1 = transform * Point::new(rect.x1, rect.y1);
            let mist_rect = Rect::new(p0.x, p0.y, p1.x, p1.y);
            for user in peers {
                presence.draw_peer_mist(scene, theme, user, mist_rect);
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

    /// Where the inline rename editor for tab `idx` sits: over the tab, with
    /// room for a longer name while typing (grown with the text), clamped to
    /// the window. Re-derived by `relayout` so the editor follows its tab
    /// through any reflow.
    pub(super) fn tab_rename_rect(&self, idx: usize) -> Rect {
        let m = &self.metrics;
        let tab = self
            .tab_rects
            .iter()
            .find(|(_, i)| *i == idx)
            .map_or(self.add_tab_rect, |(r, _)| *r);
        let right = (tab.x0 + m.s16 * 2.5 * m.text_growth())
            .min(self.rect.x1 - m.s2)
            .max(tab.x1);
        Rect::new(tab.x0, tab.y0, right, tab.y1)
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
