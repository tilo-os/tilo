//! The "Structure" tab: renders a table's columns, primary/foreign keys and
//! indexes as three stacked panels. Read-only inspection — the kind of view
//! you reach for to remember a column's type or check a constraint.

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use crate::model::TableInfo;
use crate::theme_ext::ThemeExt;
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;

/// 1× band heights; the painted bands are [`Bands`], which equal these until
/// the text scale outgrows them.
const ROW_H: f64 = 26.0;
const HEADER_H: f64 = 28.0;
/// Fractional text sizes painted in the panels (whole-number sizes — the
/// 16 px table title, 11 px panel titles, 10 px chip glyphs — stay inline).
const HEADING_FONT: f32 = 11.5;
const CELL_FONT: f32 = 12.5;

/// Band heights measured from the renderer the tab paints with, so 2× text
/// gets 2× rows instead of 31 px glyphs overprinting 26 px rows. Every row,
/// header and title band reads this one snapshot. `Default` is the tab's 1×
/// layout: 26/28 px rows and headers, a 30 px title band, an 18 px panel-title
/// band and 18 px between panels; a 12.5 px cell (15.6 px) keeps 5 px either
/// side and an 11.5 px heading (14.4 px) keeps 6 px.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Bands {
    row_h: f64,
    header_h: f64,
    title_band: f64,
    panel_title_band: f64,
    panel_gap: f64,
}

impl Default for Bands {
    fn default() -> Self {
        Self {
            row_h: ROW_H,
            header_h: HEADER_H,
            title_band: 30.0,
            panel_title_band: 18.0,
            panel_gap: 18.0,
        }
    }
}

impl Bands {
    fn measure(tr: &mut TextRenderer) -> Self {
        let one = Self::default();
        let line = |tr: &mut TextRenderer, size: f32| f64::from(tr.measure("Ag", size, None).1);
        let cell_h = line(tr, CELL_FONT);
        let heading_h = line(tr, HEADING_FONT);
        let title_h = line(tr, 16.0);
        let panel_title_h = line(tr, 11.0);
        Self {
            row_h: (cell_h + 2.0 * 5.0).max(one.row_h),
            header_h: (heading_h + 2.0 * 6.0).max(one.header_h),
            title_band: (title_h + 10.0).max(one.title_band),
            panel_title_band: (panel_title_h + 4.0).max(one.panel_title_band),
            panel_gap: one.panel_gap,
        }
    }
}

pub fn draw_structure(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    area: Rect,
    table: &TableInfo,
) {
    let m = Bands::measure(tr);
    let mut y = area.y0;

    // Title row.
    let title = format!(
        "{}   ({} columns{})",
        table.name,
        table.columns.len(),
        table
            .row_count
            .map(|c| format!(", {c} rows"))
            .unwrap_or_default()
    );
    tr.draw_text(
        scene,
        &title,
        16.0,
        theme.text_primary,
        t * Affine::translate((area.x0 + 4.0, y)),
        None,
    );
    y += m.title_band;

    y = draw_columns_panel(scene, t, tr, theme, area, y, table, &m);
    y += m.panel_gap;

    if !table.foreign_keys.is_empty() || !table.primary_keys().is_empty() {
        y = draw_keys_panel(scene, t, tr, theme, area, y, table, &m);
        y += m.panel_gap;
    }

    if !table.indexes.is_empty() {
        draw_indexes_panel(scene, t, tr, theme, area, y, table, &m);
    }
}

#[expect(
    clippy::similar_names,
    reason = "`chip_x0`/`chip_y0` are the chip's origin coordinates"
)]
fn draw_columns_panel(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    area: Rect,
    y: f64,
    table: &TableInfo,
    m: &Bands,
) -> f64 {
    let p = theme.db();
    let cols = [
        ("Name", area.x0),
        ("Type", area.x0 + 200.0),
        ("Null", area.x0 + 360.0),
        ("Key", area.x0 + 430.0),
        ("Default", area.x0 + 510.0),
    ];
    let mut y = panel_header(
        &mut PaintCtx::new(scene, t, tr, theme),
        area,
        y,
        "COLUMNS",
        &cols,
        m,
    );
    let type_x = cols[1].1;

    for (i, col) in table.columns.iter().enumerate() {
        let row = Rect::new(area.x0, y, area.x1, y + m.row_h);
        if i % 2 == 1 {
            scene.fill(Fill::NonZero, t, p.row_alt, None, &row);
        }
        // Type icon chip (one `s4` square), centred on the row; its glyph
        // centred on the chip.
        let chip_side = f64::from(theme.tokens.spacing.s4);
        let chip_y0 = y + (m.row_h - chip_side) / 2.0;
        let chip_x0 = type_x - 22.0;
        let chip = Rect::new(chip_x0, chip_y0, chip_x0 + chip_side, chip_y0 + chip_side)
            .to_rounded_rect(4.0);
        let chip_color = theme.type_color(col.class);
        scene.fill(Fill::NonZero, t, chip_color, None, &chip);
        let icon = col.class.icon();
        let (iw, ih) = tr.measure(icon, 10.0, None);
        tr.draw_text(
            scene,
            icon,
            10.0,
            theme.tokens.colors.on_surface(chip_color).ink,
            t * Affine::translate((
                chip_x0 + (chip_side - f64::from(iw)) / 2.0,
                chip_y0 + (chip_side - f64::from(ih)) / 2.0,
            )),
            None,
        );

        cell(scene, t, tr, theme.text_primary, &col.name, cols[0].1, y, m);
        cell(
            scene,
            t,
            tr,
            theme.text_dim,
            &col.decl_type,
            cols[1].1,
            y,
            m,
        );
        cell(
            scene,
            t,
            tr,
            if col.not_null {
                theme.tokens.colors.semantic.danger
            } else {
                theme.text_dim
            },
            if col.not_null { "NOT NULL" } else { "nullable" },
            cols[2].1,
            y,
            m,
        );
        if col.primary_key {
            cell(
                scene,
                t,
                tr,
                theme.tokens.colors.semantic.warning,
                "PK",
                cols[3].1,
                y,
                m,
            );
        }
        if let Some(d) = &col.default {
            cell(scene, t, tr, theme.text_dim, d, cols[4].1, y, m);
        }
        y += m.row_h;
    }
    y
}

fn draw_keys_panel(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    area: Rect,
    y: f64,
    table: &TableInfo,
    m: &Bands,
) -> f64 {
    let p = theme.db();
    let kx = [
        ("Constraint", area.x0),
        ("Column", area.x0 + 220.0),
        ("References", area.x0 + 360.0),
    ];
    let mut y = panel_header(
        &mut PaintCtx::new(scene, t, tr, theme),
        area,
        y,
        "KEYS",
        &kx,
        m,
    );

    for col in table.primary_keys() {
        let row = Rect::new(area.x0, y, area.x1, y + m.row_h);
        scene.fill(Fill::NonZero, t, p.row_alt, None, &row);
        cell(
            scene,
            t,
            tr,
            theme.tokens.colors.semantic.warning,
            "PRIMARY KEY",
            kx[0].1,
            y,
            m,
        );
        cell(scene, t, tr, theme.text_primary, &col.name, kx[1].1, y, m);
        cell(scene, t, tr, theme.text_dim, "—", kx[2].1, y, m);
        y += m.row_h;
    }
    for fk in &table.foreign_keys {
        cell(
            scene,
            t,
            tr,
            theme.tokens.colors.semantic.info,
            "FOREIGN KEY",
            kx[0].1,
            y,
            m,
        );
        cell(scene, t, tr, theme.text_primary, &fk.column, kx[1].1, y, m);
        cell(
            scene,
            t,
            tr,
            theme.text_dim,
            &format!("{}.{}", fk.ref_table, fk.ref_column),
            kx[2].1,
            y,
            m,
        );
        y += m.row_h;
    }
    y
}

fn draw_indexes_panel(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    area: Rect,
    y: f64,
    table: &TableInfo,
    m: &Bands,
) {
    let p = theme.db();
    let ix = [
        ("Index", area.x0),
        ("Unique", area.x0 + 240.0),
        ("Columns", area.x0 + 330.0),
    ];
    let mut y = panel_header(
        &mut PaintCtx::new(scene, t, tr, theme),
        area,
        y,
        "INDEXES",
        &ix,
        m,
    );
    for (i, idx) in table.indexes.iter().enumerate() {
        let row = Rect::new(area.x0, y, area.x1, y + m.row_h);
        if i % 2 == 1 {
            scene.fill(Fill::NonZero, t, p.row_alt, None, &row);
        }
        cell(scene, t, tr, theme.text_primary, &idx.name, ix[0].1, y, m);
        cell(
            scene,
            t,
            tr,
            if idx.unique {
                theme.tokens.colors.semantic.success
            } else {
                theme.text_dim
            },
            if idx.unique { "UNIQUE" } else { "—" },
            ix[1].1,
            y,
            m,
        );
        cell(
            scene,
            t,
            tr,
            theme.text_dim,
            &idx.columns.join(", "),
            ix[2].1,
            y,
            m,
        );
        y += m.row_h;
    }
}

/// The panel's title and column-heading band across `area`, starting at
/// `y`. Returns the Y position beneath its rule.
fn panel_header(
    ctx: &mut PaintCtx<'_>,
    area: Rect,
    mut y: f64,
    title: &str,
    columns: &[(&str, f64)],
    m: &Bands,
) -> f64 {
    let theme = ctx.theme;
    let p = theme.db();
    ctx.text(
        title,
        &TextStyle::new(11.0),
        theme.text_dim,
        Point::new(area.x0, y),
        None,
    );
    y += m.panel_title_band;
    ctx.fill_rect(Rect::new(area.x0, y, area.x1, y + m.header_h), p.header_bg);
    let heading = TextStyle::new(HEADING_FONT);
    for (h, x) in columns {
        // Centre each heading on the band from its measured line height so
        // the header row and the cells below share one alignment rule.
        let (_, hh) = ctx.text.measure(h, HEADING_FONT, None);
        ctx.text(
            h,
            &heading,
            theme.text_primary,
            Point::new(x + 6.0, y + (m.header_h - f64::from(hh)) / 2.0),
            None,
        );
    }
    y += m.header_h;
    ctx.scene.stroke(
        &Stroke::new(1.0),
        ctx.transform,
        theme.border_standard,
        None,
        &Line::new(Point::new(area.x0, y), Point::new(area.x1, y)),
    );
    y
}

fn cell(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    color: Color,
    text: &str,
    x: f64,
    y: f64,
    m: &Bands,
) {
    let (_, th) = tr.measure(text, CELL_FONT, None);
    tr.draw_text(
        scene,
        text,
        CELL_FONT,
        color,
        t * Affine::translate((x + 6.0, y + (m.row_h - f64::from(th)) / 2.0)),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured bands reproduce the 1× constants exactly and grow with
    /// the cell text at 2×, so rows never sit on a pitch their glyphs exceed.
    #[test]
    fn bands_match_the_constants_at_1x_and_follow_the_text_scale() {
        let mut tr = TextRenderer::new();
        let one = Bands::measure(&mut tr);
        assert_eq!(
            (
                one.row_h,
                one.header_h,
                one.title_band,
                one.panel_title_band
            ),
            (ROW_H, HEADER_H, 30.0, 18.0)
        );

        tr.set_text_scale(2.0);
        let two = Bands::measure(&mut tr);
        let (_, cell_h) = tr.measure("Ag", CELL_FONT, None);
        assert!(two.row_h >= f64::from(cell_h) + 2.0 * 5.0 && two.row_h > one.row_h);
        assert!(two.header_h > one.header_h && two.title_band > one.title_band);
    }
}
