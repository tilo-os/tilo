//! The "Structure" tab: renders a table's columns, primary/foreign keys and
//! indexes as three stacked panels. Read-only inspection — the kind of view
//! you reach for to remember a column's type or check a constraint.

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use crate::model::TableInfo;
use crate::theme_ext::ThemeExt;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

const ROW_H: f64 = 26.0;
const HEADER_H: f64 = 28.0;

pub fn draw_structure(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    area: Rect,
    table: &TableInfo,
) {
    let p = theme.db();
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
    y += 30.0;

    // --- Columns panel ---
    let col_headers = ["Name", "Type", "Null", "Key", "Default"];
    let col_x = [
        area.x0,
        area.x0 + 200.0,
        area.x0 + 360.0,
        area.x0 + 430.0,
        area.x0 + 510.0,
    ];
    y = panel_header(
        scene,
        t,
        tr,
        theme,
        area,
        y,
        "COLUMNS",
        &col_headers,
        &col_x,
    );

    for (i, col) in table.columns.iter().enumerate() {
        let row = Rect::new(area.x0, y, area.x1, y + ROW_H);
        if i % 2 == 1 {
            scene.fill(Fill::NonZero, t, p.row_alt, None, &row);
        }
        // Type icon chip.
        let chip =
            Rect::new(col_x[1] - 22.0, y + 5.0, col_x[1] - 6.0, y + 21.0).to_rounded_rect(4.0);
        scene.fill(Fill::NonZero, t, theme.type_color(col.class), None, &chip);
        tr.draw_text(
            scene,
            col.class.icon(),
            10.0,
            Color::from_rgb8(20, 20, 20),
            t * Affine::translate((col_x[1] - 18.0, y + 6.0)),
            None,
        );

        cell(scene, t, tr, theme.text_primary, &col.name, col_x[0], y);
        cell(scene, t, tr, theme.text_dim, &col.decl_type, col_x[1], y);
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
            col_x[2],
            y,
        );
        if col.primary_key {
            cell(
                scene,
                t,
                tr,
                theme.tokens.colors.semantic.warning,
                "PK",
                col_x[3],
                y,
            );
        }
        if let Some(d) = &col.default {
            cell(scene, t, tr, theme.text_dim, d, col_x[4], y);
        }
        y += ROW_H;
    }

    y += 18.0;

    // --- Keys panel ---
    if !table.foreign_keys.is_empty() || !table.primary_keys().is_empty() {
        let kh = ["Constraint", "Column", "References"];
        let kx = [area.x0, area.x0 + 220.0, area.x0 + 360.0];
        y = panel_header(scene, t, tr, theme, area, y, "KEYS", &kh, &kx);

        for col in table.primary_keys() {
            let row = Rect::new(area.x0, y, area.x1, y + ROW_H);
            scene.fill(Fill::NonZero, t, p.row_alt, None, &row);
            cell(
                scene,
                t,
                tr,
                theme.tokens.colors.semantic.warning,
                "PRIMARY KEY",
                kx[0],
                y,
            );
            cell(scene, t, tr, theme.text_primary, &col.name, kx[1], y);
            cell(scene, t, tr, theme.text_dim, "—", kx[2], y);
            y += ROW_H;
        }
        for fk in &table.foreign_keys {
            cell(
                scene,
                t,
                tr,
                theme.tokens.colors.semantic.info,
                "FOREIGN KEY",
                kx[0],
                y,
            );
            cell(scene, t, tr, theme.text_primary, &fk.column, kx[1], y);
            cell(
                scene,
                t,
                tr,
                theme.text_dim,
                &format!("{}.{}", fk.ref_table, fk.ref_column),
                kx[2],
                y,
            );
            y += ROW_H;
        }
        y += 18.0;
    }

    // --- Indexes panel ---
    if !table.indexes.is_empty() {
        let ih = ["Index", "Unique", "Columns"];
        let ix = [area.x0, area.x0 + 240.0, area.x0 + 330.0];
        y = panel_header(scene, t, tr, theme, area, y, "INDEXES", &ih, &ix);
        for (i, idx) in table.indexes.iter().enumerate() {
            let row = Rect::new(area.x0, y, area.x1, y + ROW_H);
            if i % 2 == 1 {
                scene.fill(Fill::NonZero, t, p.row_alt, None, &row);
            }
            cell(scene, t, tr, theme.text_primary, &idx.name, ix[0], y);
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
                ix[1],
                y,
            );
            cell(
                scene,
                t,
                tr,
                theme.text_dim,
                &idx.columns.join(", "),
                ix[2],
                y,
            );
            y += ROW_H;
        }
    }
}

fn panel_header(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    area: Rect,
    mut y: f64,
    title: &str,
    headers: &[&str],
    xs: &[f64],
) -> f64 {
    let p = theme.db();
    tr.draw_text(
        scene,
        title,
        11.0,
        theme.text_dim,
        t * Affine::translate((area.x0, y)),
        None,
    );
    y += 18.0;
    let hr = Rect::new(area.x0, y, area.x1, y + HEADER_H);
    scene.fill(Fill::NonZero, t, p.header_bg, None, &hr);
    for (h, x) in headers.iter().zip(xs.iter()) {
        tr.draw_text(
            scene,
            h,
            11.5,
            theme.text_primary,
            t * Affine::translate((x + 6.0, y + 7.0)),
            None,
        );
    }
    y += HEADER_H;
    scene.stroke(
        &Stroke::new(1.0),
        t,
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
) {
    tr.draw_text(
        scene,
        text,
        12.5,
        color,
        t * Affine::translate((x + 6.0, y + 6.0)),
        None,
    );
}
