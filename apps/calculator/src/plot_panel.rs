//! The formula list beside a plot: one row per function or surface, each with
//! its colour, its math editor, a visibility toggle and a delete button, and
//! an add button under the list. Graph 2D's functions and Graph 3D's
//! surfaces are the same list with different words, so both screens hold one
//! of these.

use ui::charts::ChartPalette;
use ui::chrome::{SelectionState, paint_row_selection};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Button, ButtonFace, ControlSize};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use crate::chrome::SidePanel;
use crate::mathfield::{self, MathField};
use crate::model::PlotFn;

/// The words one kind of formula list uses.
#[derive(Clone, Copy, Debug)]
pub struct Words {
    /// Panel title.
    pub title: &'static str,
    /// What leads each formula (`y =`).
    pub prefix: &'static str,
    /// What an empty formula row shows.
    pub placeholder: &'static str,
    /// The add button's label.
    pub add: &'static str,
    /// The singular name of one row, for accessible names.
    pub item: &'static str,
}

/// Graph 2D's list: `y = f(x)` curves.
pub const CURVES: Words = Words {
    title: "Functions",
    prefix: "y =",
    placeholder: "f(x)",
    add: "Add function",
    item: "function",
};

/// Graph 3D's list: `z = f(x, y)` surfaces.
pub const SURFACES: Words = Words {
    title: "Surfaces",
    prefix: "z =",
    placeholder: "f(x, y)",
    add: "Add surface",
    item: "surface",
};

/// Where the caret starts in a freshly built editor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Park {
    /// Inside the first 2D structure, so a static frame shows the caret
    /// living in the notation.
    #[default]
    Structure,
    /// At the very end of the formula, with its tail in view.
    End,
    /// A representative selection, for a frame that shows the highlight.
    Selection,
}

/// Which of a row's two buttons the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowButton {
    Visibility,
    Delete,
}

/// What the pointer is over in the panel, for deciding when to repaint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hover {
    pub row: Option<usize>,
    pub button: Option<(usize, RowButton)>,
    pub add: bool,
    pub toggle: bool,
}

/// The theme's measurements the panel lays out with, cached from the last
/// draw so the event path (which has no theme) places rows the same way.
#[derive(Clone, Copy, Debug)]
struct PanelMetrics {
    s1: f64,
    s2: f64,
    s3: f64,
    small: f64,
    regular: f64,
    /// How much a text-sized extent grows with the text scale (never below
    /// 1), which the editor's minimum and maximum heights follow.
    growth: f64,
}

impl PanelMetrics {
    fn of(theme: &Theme) -> Self {
        let s = &theme.tokens.spacing;
        Self {
            s1: f64::from(s.s1),
            s2: f64::from(s.s2),
            s3: f64::from(s.s3),
            small: ButtonFace::height(ControlSize::Small, theme),
            regular: ButtonFace::height(ControlSize::Regular, theme),
            growth: f64::from(theme.text_growth()),
        }
    }

    /// The height a row's editor takes for a formula `content` tall: it
    /// grows to fit, between the minimum and maximum heights at this text
    /// scale.
    fn editor_height(&self, content: f64) -> f64 {
        content.clamp(
            mathfield::CARD_MIN_H * self.growth,
            mathfield::CARD_MAX_H * self.growth,
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RowGeometry {
    row: Rect,
    edit: Rect,
    vis: Rect,
    del: Rect,
}

pub struct PlotPanel {
    words: Words,
    panel: SidePanel,
    rows: Vec<RowGeometry>,
    add: Rect,
    add_w: f64,
    fields: Vec<MathField>,
    /// Per-row heights, grown to fit each formula up to a maximum. Measured
    /// in `draw`, where a text renderer is at hand, and reused by `layout`
    /// on the event path so hit-tests match what was painted.
    heights: Vec<f64>,
    /// Measured width of the row prefix at this frame's text size.
    prefix_w: f64,
    /// The side of a row button's pointer target.
    target: f64,
    hover: Hover,
    park: Park,
    /// The theme-resolved size of a formula, set in `Title`.
    formula_size: f32,
    m: PanelMetrics,
}

impl PlotPanel {
    pub fn new(words: Words) -> Self {
        let stock = Theme::dark();
        Self {
            words,
            panel: SidePanel::new(
                true,
                f64::from(ui::tokens::Metrics::BASE.panel_width_utility),
            ),
            rows: Vec::new(),
            add: Rect::ZERO,
            add_w: 0.0,
            fields: Vec::new(),
            heights: Vec::new(),
            prefix_w: 0.0,
            target: f64::from(stock.tokens.metrics.min_pointer_target),
            hover: Hover::default(),
            park: Park::default(),
            formula_size: stock.tokens.typography.size(TextRole::Title),
            m: PanelMetrics::of(&stock),
        }
    }

    pub fn words(&self) -> Words {
        self.words
    }

    pub fn side(&self) -> &SidePanel {
        &self.panel
    }

    pub fn side_mut(&mut self) -> &mut SidePanel {
        &mut self.panel
    }

    /// Where a freshly built editor parks its caret.
    pub fn set_park(&mut self, park: Park) {
        self.park = park;
    }

    /// Adopt the theme's scale and metrics.
    pub fn scale_for(&mut self, theme: &Theme) {
        self.panel.scale_for(theme);
        self.target = f64::from(theme.tokens.metrics.min_pointer_target);
        self.formula_size = theme.tokens.typography.size(TextRole::Title);
        self.m = PanelMetrics::of(theme);
    }

    // -- editors -------------------------------------------------------------

    /// Reconcile the editors with the model's rows: one per row, fed the
    /// row's current formula. Local edits survive because `set_source` is a
    /// no-op when the string is unchanged.
    pub fn sync_fields(&mut self, rows: &[PlotFn], active: usize) {
        if self.fields.len() == rows.len() {
            for (field, row) in self.fields.iter_mut().zip(rows) {
                field.set_source(&row.expr);
            }
            return;
        }
        self.fields = rows
            .iter()
            .map(|row| {
                let mut field = MathField::new(&row.expr);
                field.set_size(self.formula_size);
                field
            })
            .collect();
        if let Some(field) = self.fields.get_mut(active) {
            match self.park {
                Park::Structure => field.park_caret_in_structure(),
                Park::End => field.park_caret_at_end(),
                Park::Selection => field.park_demo_selection(),
            }
        }
    }

    pub fn active_field_mut(&mut self, active: usize) -> Option<&mut MathField> {
        self.fields.get_mut(active)
    }

    pub fn release_fields(&mut self) {
        for field in &mut self.fields {
            field.on_release();
        }
    }

    pub fn draw_remote_carets(
        &self,
        scene: &mut Scene,
        t: Affine,
        active: usize,
        cursors: &[(Point, Color)],
    ) {
        if let Some(field) = self.fields.get(active) {
            field.draw_remote_carets(scene, t, cursors);
        }
    }

    /// Measure every formula, the prefix and the add button at the size they
    /// paint at, so `layout` can place variable-height rows. A formula
    /// taller than the maximum clamps and scrolls within its editor.
    pub fn fit(&mut self, rows: &[PlotFn], active: usize, tr: &mut TextRenderer, theme: &Theme) {
        self.scale_for(theme);
        self.formula_size = mathfield::formula_size(theme, tr);
        self.sync_fields(rows, active);
        let (prefix_w, _) = tr.measure_style(self.words.prefix, None, &theme.text(TextRole::Body));
        self.prefix_w = f64::from(prefix_w);
        self.add_w = ButtonFace::new(Rect::ZERO, self.words.add)
            .icon(Icon::Plus)
            .intrinsic_width(tr, theme);
        let pad = f64::from(theme.tokens.spacing.s2);
        let renderer = f64::from(tr.text_scale());
        self.heights.clear();
        for field in &mut self.fields {
            field.set_size(self.formula_size);
            // The formula's boxes are laid out in its own size units while its
            // glyphs are drawn through the renderer, which grows them by its
            // text scale: the editor holds the grown glyphs.
            let content = if field.is_empty() {
                0.0
            } else {
                field.content_height(tr) * renderer.max(1.0)
            };
            self.heights.push(self.m.editor_height(content) + 2.0 * pad);
        }
    }

    // -- layout --------------------------------------------------------------

    /// Resolve the panel, the content column and every row for the window
    /// `area` and `count` formulas.
    pub fn layout(&mut self, area: Rect, count: usize) {
        self.panel.layout(area);
        self.rows.clear();
        self.add = Rect::ZERO;
        let body = self.panel.body();
        if body == Rect::ZERO {
            return;
        }
        let PanelMetrics {
            s1,
            s2,
            s3,
            small: button,
            regular: add_h,
            ..
        } = self.m;
        let mut y = body.y0;
        for i in 0..count {
            let height = self
                .heights
                .get(i)
                .copied()
                .unwrap_or_else(|| self.m.editor_height(0.0) + 2.0 * s2);
            let row = Rect::new(body.x0 + s2, y, body.x1 - s2, y + height);
            let mid = row.center().y;
            let del = Rect::from_center_size(
                Point::new(row.x1 - s3 - button / 2.0, mid),
                (button, button),
            );
            let vis = Rect::from_center_size(
                Point::new(del.x0 - s1 - button / 2.0, mid),
                (button, button),
            );
            let edit = Rect::new(
                row.x0 + s3 + s3 + s2 + self.prefix_w + s1,
                row.y0 + s2,
                (vis.x0 - s2).max(row.x0),
                row.y1 - s2,
            );
            self.rows.push(RowGeometry {
                row,
                edit,
                vis,
                del,
            });
            y += height + s1;
        }
        let x0 = body.x0 + s2 + s3;
        self.add = Rect::new(
            x0,
            y + s2,
            (x0 + self.add_w).min(body.x1 - s2),
            y + s2 + add_h,
        );
    }

    // -- geometry for tests and the accessibility tree ----------------------

    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        self.rows.get(i).map(|r| r.row)
    }

    pub fn field_rect(&self, i: usize) -> Option<Rect> {
        self.rows.get(i).map(|r| r.edit)
    }

    /// The visibility toggle's pointer target.
    pub fn visibility_rect(&self, i: usize) -> Option<Rect> {
        self.rows.get(i).map(|r| self.target_of(r.vis))
    }

    /// The delete button's pointer target.
    pub fn delete_rect(&self, i: usize) -> Option<Rect> {
        self.rows.get(i).map(|r| self.target_of(r.del))
    }

    pub fn add_rect(&self) -> Option<Rect> {
        (self.add != Rect::ZERO).then_some(self.add)
    }

    /// A button's pointer target: its face grown to the minimum target.
    fn target_of(&self, face: Rect) -> Rect {
        let grow_x = ((self.target - face.width()) / 2.0).max(0.0);
        let grow_y = ((self.target - face.height()) / 2.0).max(0.0);
        face.inflate(grow_x, grow_y)
    }

    // -- hit-testing ---------------------------------------------------------

    pub fn row_at(&self, p: Point) -> Option<usize> {
        self.rows.iter().position(|r| r.row.contains(p))
    }

    pub fn field_at(&self, p: Point) -> Option<usize> {
        self.rows.iter().position(|r| r.edit.contains(p))
    }

    pub fn vis_at(&self, p: Point) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| self.target_of(r.vis).contains(p))
    }

    pub fn delete_at(&self, p: Point) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| self.target_of(r.del).contains(p))
    }

    pub fn add_at(&self, p: Point) -> bool {
        self.add != Rect::ZERO && self.add.contains(p)
    }

    /// Track the pointer. Returns whether anything the panel paints changed.
    pub fn set_pointer(&mut self, p: Point) -> bool {
        let button = self
            .vis_at(p)
            .map(|i| (i, RowButton::Visibility))
            .or_else(|| self.delete_at(p).map(|i| (i, RowButton::Delete)));
        let next = Hover {
            row: self.row_at(p),
            button,
            add: self.add_at(p),
            toggle: self.panel.toggle_at(p),
        };
        let changed = next != self.hover;
        self.hover = next;
        changed
    }

    pub fn hover(&self) -> Hover {
        self.hover
    }

    // -- paint ---------------------------------------------------------------

    /// Paint the panel: the scrim of a floating drawer, the panel's ground,
    /// its header, the rows, the add button and the show/hide button.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rows: &[PlotFn],
        active: usize,
    ) {
        self.panel.paint_scrim(scene, t, theme);
        if self.panel.column() == Rect::ZERO {
            self.panel
                .paint_toggle(scene, t, tr, theme, self.hover.toggle);
            return;
        }
        let local = self.panel.paint_panel(scene, t, theme);
        self.draw_header(scene, t, tr, &local);
        self.draw_rows(scene, t, tr, &local, rows, active);
        let add = ButtonFace::new(self.add, self.words.add)
            .icon(Icon::Plus)
            .hovered(self.hover.add);
        if self.add != Rect::ZERO {
            Button::paint(scene, t, tr, &local, &add);
        }
        let ground = if self.panel.toggle_on_content() {
            theme
        } else {
            &local
        };
        self.panel
            .paint_toggle(scene, t, tr, ground, self.hover.toggle);
    }

    fn draw_header(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, local: &Theme) {
        let header = self.panel.header_band();
        let mut ctx = PaintCtx::new(scene, t, tr, local);
        let x = self.panel.toggle_rect().x1 + f64::from(local.tokens.spacing.s2);
        let room = (header.x1 - f64::from(local.tokens.spacing.s3) - x).max(0.0);
        ctx.text_role_in_band(
            self.words.title,
            TextRole::Heading,
            local.tokens.colors.text_primary,
            x,
            header,
            Some(ui::num::f32_of_f64(room)),
        );
    }

    fn draw_rows(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rows: &[PlotFn],
        active: usize,
    ) {
        let colors = &theme.tokens.colors;
        let palette = ChartPalette::from_theme(theme);
        let ground = colors.surfaces.content;
        let s = &theme.tokens.spacing;
        let (s2, s3) = (f64::from(s.s2), f64::from(s.s3));
        for (i, row) in rows.iter().enumerate() {
            let Some(geo) = self.rows.get(i).copied() else {
                continue;
            };
            let is_active = i == active;
            let hovered = self.hover.row == Some(i);
            paint_row_selection(
                scene,
                t,
                geo.row,
                SelectionState::new(is_active, hovered && !is_active, false),
                ground,
                theme,
            );
            let ink = if row.visible {
                colors.text_primary
            } else {
                colors.text_dim
            };
            // The colour key: a filled dot while the formula plots, a ring
            // while it is hidden.
            let series = palette.categorical(i);
            let centre = Point::new(geo.row.x0 + s3 + s3 / 2.0, geo.row.center().y);
            let dot = Rect::from_center_size(centre, (s3, s3));
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            if row.visible {
                ctx.fill_round(dot, s3 / 2.0, series);
            } else {
                ctx.stroke_round(dot.inset(-0.75), s3 / 2.0 - 0.75, series, 1.5);
            }
            let prefix_x = geo.row.x0 + s3 + s3 + s2;
            ctx.text_role_in_band(
                self.words.prefix,
                TextRole::Body,
                ink,
                prefix_x,
                geo.row,
                None,
            );
            let empty = self.fields.get(i).is_some_and(MathField::is_empty);
            if empty && !is_active {
                let mut ctx = PaintCtx::new(scene, t, tr, theme);
                ctx.text_role_in_band(
                    self.words.placeholder,
                    TextRole::Body,
                    colors.text_tertiary,
                    geo.edit.x0 + f64::from(s.s1),
                    geo.edit,
                    None,
                );
            } else if let Some(field) = self.fields.get_mut(i) {
                field.set_rect(geo.edit, tr);
                field.draw(scene, t, tr, theme, ink, is_active);
            }
            let name = |verb: &str| format!("{verb} {} {}", self.words.item, i + 1);
            let (show, delete) = (name("Show"), name("Delete"));
            let hover = self.hover.button;
            let vis = ButtonFace::icon_only(geo.vis, Icon::Eye, &show)
                .control_size(ControlSize::Small)
                .hovered(hover == Some((i, RowButton::Visibility)))
                .disabled(!row.visible);
            let del = ButtonFace::icon_only(geo.del, Icon::Trash, &delete)
                .control_size(ControlSize::Small)
                .hovered(hover == Some((i, RowButton::Delete)));
            Button::paint(scene, t, tr, theme, &vis);
            Button::paint(scene, t, tr, theme, &del);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PlotFn;

    fn plots(exprs: &[&str]) -> Vec<PlotFn> {
        exprs
            .iter()
            .map(|expr| PlotFn {
                expr: (*expr).into(),
                visible: true,
            })
            .collect()
    }

    /// The row a short, single-line formula gets.
    fn base_row(theme: &Theme) -> f64 {
        mathfield::card_view_height(mathfield::CARD_MIN_H)
            + 2.0 * f64::from(theme.tokens.spacing.s2)
    }

    /// Formula rows on the real fit → layout → draw path: a fraction grows to
    /// show in full, an oversized formula clamps to the maximum and scrolls, a
    /// short formula keeps the base height, and taller rows push later rows
    /// down. Fresh panel and renderer per phase.
    #[test]
    fn formula_rows_grow_to_fit_clamp_to_max_and_reflow() {
        let theme = Theme::dark();
        let area = Rect::new(0.0, 0.0, 1000.0, 700.0);

        // phase fraction_row_grows_to_show_in_full
        {
            let mut tr = TextRenderer::new();
            let mut panel = PlotPanel::new(CURVES);
            let rows = plots(&["(x^2+1)/(x-3)"]);
            panel.fit(&rows, 0, &mut tr, &theme);
            panel.layout(area, rows.len());
            let row = panel.row_rect(0).expect("row laid out");
            assert!(
                row.height() > base_row(&theme) + 8.0,
                "a fraction row must grow visibly taller than the base, got {}",
                row.height()
            );
            // The fraction fits vertically in its grown editor: no scroll.
            let content = panel.fields[0].content_height(&mut tr);
            let edit = panel.field_rect(0).expect("editor");
            assert!(
                content <= edit.height() + 0.5,
                "a fraction under the maximum must fit in full: content {content} vs editor {}",
                edit.height()
            );
        }

        // phase oversized_formula_row_grows_to_max_then_scrolls
        {
            let mut tr = TextRenderer::new();
            let mut panel = PlotPanel::new(CURVES);
            let rows = plots(&["(1/x)/(1/y)"]);
            panel.fit(&rows, 0, &mut tr, &theme);
            panel.layout(area, rows.len());
            let expected = mathfield::CARD_MAX_H + 2.0 * f64::from(theme.tokens.spacing.s2);
            let row = panel.row_rect(0).expect("row laid out");
            assert!(
                (row.height() - expected).abs() < 1e-6,
                "an oversized row must clamp to the maximum ({expected}), got {}",
                row.height()
            );
            let edit = panel.field_rect(0).expect("editor");
            let content = panel.fields[0].content_height(&mut tr);
            assert!(
                content > edit.height() + 0.5,
                "an oversized formula overflows its maxed editor (scroll, not clip): \
                 content {content} vs editor {}",
                edit.height()
            );
            panel.fields[0].set_rect(edit, &mut tr);
            assert!(panel.fields[0].overflows());
        }

        // phase short_formula_row_keeps_base_height
        {
            let mut tr = TextRenderer::new();
            let mut panel = PlotPanel::new(CURVES);
            let rows = plots(&["sin(x)"]);
            panel.fit(&rows, 0, &mut tr, &theme);
            panel.layout(area, rows.len());
            let row = panel.row_rect(0).expect("row laid out");
            assert!(
                (row.height() - base_row(&theme)).abs() < 1e-6,
                "a short formula keeps the base height, got {}",
                row.height()
            );
            let edit = panel.field_rect(0).expect("editor");
            panel.fields[0].set_rect(edit, &mut tr);
            assert!(
                !panel.fields[0].overflows(),
                "a short formula must not scroll"
            );
        }

        // phase rows_reflow_to_accommodate_taller_rows
        {
            let mut tr = TextRenderer::new();
            let mut panel = PlotPanel::new(CURVES);
            let rows = plots(&["(x^2+1)/(x-3)", "sin(x)"]);
            panel.fit(&rows, 0, &mut tr, &theme);
            panel.layout(area, rows.len());
            let (first, second) = (
                panel.row_rect(0).expect("first row"),
                panel.row_rect(1).expect("second row"),
            );
            assert!(
                second.y0 >= first.y1 - 0.01,
                "row 1 (y0={}) must start at or after the bottom of the taller row 0 (y1={})",
                second.y0,
                first.y1
            );
            let add = panel.add_rect().expect("add button");
            assert!(add.y0 >= second.y1, "the add button follows the last row");
        }
    }

    /// Every row's colour key, visibility toggle and delete button sit in the
    /// row, and the hover state reports what the pointer is over.
    #[test]
    fn pointer_reports_the_row_and_button_under_it() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut panel = PlotPanel::new(SURFACES);
        let rows = plots(&["x*y", "sin(x)*cos(y)"]);
        panel.fit(&rows, 0, &mut tr, &theme);
        panel.layout(Rect::new(0.0, 0.0, 1000.0, 700.0), rows.len());
        let vis = panel.visibility_rect(1).expect("visibility target");
        assert!(panel.set_pointer(vis.center()));
        assert_eq!(
            panel.hover().button,
            Some((1, RowButton::Visibility)),
            "the toggle of row 1"
        );
        assert_eq!(panel.hover().row, Some(1));
        let delete = panel.delete_rect(0).expect("delete target");
        assert!(panel.set_pointer(delete.center()));
        assert_eq!(panel.hover().button, Some((0, RowButton::Delete)));
        assert!(
            !panel.set_pointer(delete.center()),
            "the same position changes nothing"
        );
        assert!(panel.set_pointer(Point::new(-5.0, -5.0)));
        assert_eq!(panel.hover(), Hover::default());
    }

    /// At a large text scale the editor grows with the formula, so a stacked
    /// formula still shows in full instead of being clipped by a row that
    /// kept its 1× height.
    #[test]
    fn editors_grow_with_the_text_scale() {
        let theme = Theme::dark().with_text_scale(2.0);
        let mut tr = TextRenderer::new();
        let mut panel = PlotPanel::new(CURVES);
        let rows = plots(&["(x^2+1)/(x-3)"]);
        panel.fit(&rows, 0, &mut tr, &theme);
        panel.layout(Rect::new(0.0, 0.0, 2200.0, 900.0), rows.len());
        let edit = panel.field_rect(0).expect("editor");
        let content = panel.fields[0].content_height(&mut tr);
        assert!(
            content <= edit.height() + 0.5,
            "the stacked formula must fit its editor in full: content {content} vs {}",
            edit.height()
        );
    }

    /// A renderer that carries the text scale itself (the live desktop) grows
    /// the drawn glyphs, so the editor holds them.
    #[test]
    fn a_scaled_renderer_gets_an_editor_that_holds_the_grown_glyphs() {
        let theme = Theme::dark().with_text_scale(1.5);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(1.5);
        let mut panel = PlotPanel::new(CURVES);
        let rows = plots(&["(x^2+1)/(x-3)"]);
        panel.fit(&rows, 0, &mut tr, &theme);
        panel.layout(Rect::new(0.0, 0.0, 2200.0, 900.0), rows.len());
        let edit = panel.field_rect(0).expect("editor");
        let content = panel.fields[0].content_height(&mut tr) * 1.5;
        assert!(
            content <= edit.height() + 0.5,
            "the stacked formula must fit its editor in full: content {content} vs {}",
            edit.height()
        );
    }
}
