//! The schema browser tree in the sidebar: database → schema → tables →
//! columns, with a coloured type icon per column. Maintains its own
//! expansion + scroll state and renders into a caller-provided rect. Click
//! hit-testing returns a [`TreeClick`] the app turns into a session action.

use std::collections::HashSet;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;

use crate::model::{RelationKind, RelationRef};
use crate::session::Session;
use crate::theme_ext::ThemeExt;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

const ROW_H: f64 = 24.0;
const INDENT: f64 = 14.0;
/// Height of the "SCHEMA" heading band above the first row; paint,
/// hit-testing and the visible-row budget all use this one value.
const HEADER_H: f64 = 26.0;

/// What a click in the tree resolved to.
#[derive(Clone, Debug)]
pub enum TreeClick {
    ToggleSchema(String),
    ToggleTable(RelationRef),
    SelectTable(RelationRef),
}

/// One flattened, rendered row of the tree, kept so hit-testing can map a
/// click y-coordinate back to an action.
#[derive(Clone)]
enum Row {
    Schema {
        name: String,
    },
    Table {
        schema: String,
        name: String,
    },
    Column {
        schema: String,
        table: String,
        text: String,
        icon: String,
    },
}

pub struct SchemaTree {
    rect: Rect,
    cursor: Point,
    scroll: usize,
    expanded_schemas: HashSet<String>,
    expanded_tables: HashSet<RelationRef>,
    selected: Option<RelationRef>,
    /// Rows produced by the last `draw`, used for hit-testing.
    rows: Vec<Row>,
    /// Row pitch and header band measured at the top of the last `draw`
    /// (`ROW_H` / `HEADER_H` at 1×), read by paint, `visible_rows` and `hit`.
    row_h: f64,
    header_h: f64,
}

/// The widest row text: table/schema names at 12.5 px (a 15.6 px line).
const NAME_FONT: f32 = 12.5;

impl Default for SchemaTree {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaTree {
    pub fn new() -> Self {
        let mut expanded_schemas = HashSet::new();
        expanded_schemas.insert("main".to_string());
        Self {
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            scroll: 0,
            expanded_schemas,
            expanded_tables: HashSet::new(),
            selected: None,
            rows: Vec::new(),
            row_h: ROW_H,
            header_h: HEADER_H,
        }
    }

    /// Re-measure the row pitch and header band with the renderer this draw
    /// paints with, so 2× text gets 2× rows instead of overprinting 24 px.
    fn measure_bands(&mut self, tr: &mut TextRenderer) {
        // 1×: a 12.5 px name measures 15.6 px, so 4 px of padding either side
        // stays inside the 24 px row; the 10 px "SCHEMA" label (12.5 px) with
        // 6 px either side stays inside the 26 px header.
        let name_h = f64::from(tr.measure("Ag", NAME_FONT, None).1);
        let header_label_h = f64::from(tr.measure("Ag", 10.0, None).1);
        self.row_h = (name_h + 2.0 * 4.0).max(ROW_H);
        self.header_h = (header_label_h + 2.0 * 6.0).max(HEADER_H);
    }

    /// Top-of-line y that centres `s` at `size` inside the row starting at `y`
    /// — replaces the per-site `y + 4/5/6` nudges, which only fit 1× text.
    fn text_top(&self, tr: &mut TextRenderer, s: &str, size: f32, y: f64) -> f64 {
        let (_, lh) = tr.measure(s, size, None);
        y + (self.row_h - f64::from(lh)) / 2.0
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn set_cursor(&mut self, p: Point) {
        self.cursor = p;
    }

    pub fn set_selected(&mut self, relation: RelationRef) {
        self.selected = Some(relation);
    }

    pub fn toggle(&mut self, relation: RelationRef) {
        if !self.expanded_tables.remove(&relation) {
            self.expanded_tables.insert(relation);
        }
    }

    pub fn toggle_schema(&mut self, schema: &str) {
        if !self.expanded_schemas.remove(schema) {
            self.expanded_schemas.insert(schema.to_string());
        }
    }

    /// Reveal a schema for a typed resource navigation request. Unlike the
    /// pointer toggle this is idempotent, so repeating a search activation
    /// cannot unexpectedly hide the user's destination.
    pub fn reveal_schema(&mut self, schema: &str) {
        self.expanded_schemas.insert(schema.to_string());
    }

    pub fn scroll(&mut self, delta: isize) {
        let vis = self.visible_rows();
        let max = self.rows.len().saturating_sub(vis);
        let next = self.scroll as isize + delta;
        self.scroll = next.clamp(0, max as isize) as usize;
    }

    fn visible_rows(&self) -> usize {
        (((self.rect.height() - self.header_h) / self.row_h).floor()).max(0.0) as usize
    }

    /// Build the flattened row list from the current schema + expansion
    /// state. Pure; does not draw.
    fn build_rows(&self, session: &Session) -> Vec<Row> {
        let mut rows = Vec::new();
        for schema in &session.schema.schemas {
            rows.push(Row::Schema {
                name: schema.name.clone(),
            });
            if !self.expanded_schemas.contains(&schema.name) {
                continue;
            }
            for table in &schema.tables {
                rows.push(Row::Table {
                    schema: schema.name.clone(),
                    name: table.name.clone(),
                });
                if self.expanded_tables.contains(&RelationRef {
                    schema: schema.name.clone(),
                    name: table.name.clone(),
                }) {
                    for col in &table.columns {
                        let pk = if col.primary_key { "  PK" } else { "" };
                        let nn = if col.not_null && !col.primary_key {
                            " ·"
                        } else {
                            ""
                        };
                        let text = format!("{}{}{}  {}", col.name, pk, nn, col.decl_type);
                        rows.push(Row::Column {
                            schema: schema.name.clone(),
                            table: table.name.clone(),
                            text,
                            icon: col.class.icon().to_string(),
                        });
                    }
                }
            }
        }
        rows
    }

    /// Hit-test a click. Returns the resolved action.
    pub fn hit(&self, p: Point) -> Option<TreeClick> {
        if !self.rect.contains(p) {
            return None;
        }
        let body_top = self.rect.y0 + self.header_h;
        if p.y < body_top {
            return None;
        }
        let rel = ((p.y - body_top) / self.row_h).floor() as usize;
        let idx = self.scroll + rel;
        match self.rows.get(idx)? {
            Row::Schema { name } => Some(TreeClick::ToggleSchema(name.clone())),
            Row::Table { schema, name } => {
                // Click on the left disclosure triangle toggles; click on the
                // label selects. We treat the leftmost INDENT*1.5 as the
                // toggle zone.
                let toggle_zone = self.rect.x0 + INDENT * 2.2;
                if p.x < toggle_zone {
                    Some(TreeClick::ToggleTable(RelationRef {
                        schema: schema.clone(),
                        name: name.clone(),
                    }))
                } else {
                    Some(TreeClick::SelectTable(RelationRef {
                        schema: schema.clone(),
                        name: name.clone(),
                    }))
                }
            }
            Row::Column { schema, table, .. } => Some(TreeClick::SelectTable(RelationRef {
                schema: schema.clone(),
                name: table.clone(),
            })),
        }
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        session: &Session,
    ) {
        let r = self.rect;
        self.measure_bands(tr);

        // Section header, centred on the measured header band.
        let (_, hh) = tr.measure("SCHEMA", 10.0, None);
        tr.draw_text(
            scene,
            "SCHEMA",
            10.0,
            theme.text_dim,
            t * Affine::translate((r.x0 + 14.0, r.y0 + (self.header_h - f64::from(hh)) / 2.0)),
            None,
        );

        // Rebuild rows each frame (cheap; schema is small).
        self.rows = self.build_rows(session);

        let body_top = r.y0 + self.header_h;
        let vis = self.visible_rows();

        for vi in 0..vis {
            let idx = self.scroll + vi;
            let Some(row) = self.rows.get(idx) else { break };
            let y = body_top + vi as f64 * self.row_h;
            match row {
                Row::Schema { name } => {
                    self.draw_schema_row(scene, t, tr, theme, session, name, r, y);
                }
                Row::Table { schema, name } => {
                    self.draw_table_row(scene, t, tr, theme, session, schema, name, r, y);
                }
                Row::Column {
                    schema,
                    table,
                    text,
                    icon,
                } => self.draw_column_row(
                    scene, t, tr, theme, session, schema, table, text, icon, r, y,
                ),
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one row's full draw context: canvas + theme + session + this schema's name + its y"
    )]
    fn draw_schema_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        session: &Session,
        name: &str,
        r: Rect,
        y: f64,
    ) {
        let open = self.expanded_schemas.contains(name);
        let tri = if open { "▾" } else { "▸" };
        let tri_y = self.text_top(tr, tri, 11.0, y);
        tr.draw_text(
            scene,
            tri,
            11.0,
            theme.text_dim,
            t * Affine::translate((r.x0 + 8.0, tri_y)),
            None,
        );
        let name_y = self.text_top(tr, name, NAME_FONT, y);
        tr.draw_text(
            scene,
            name,
            NAME_FONT,
            theme.text_primary,
            t * Affine::translate((r.x0 + 24.0, name_y)),
            None,
        );
        // Table count.
        if let Some(s) = session.schema.schemas.iter().find(|s| s.name == *name) {
            let cnt = format!("{}", s.tables.len());
            let (cw, _) = tr.measure(&cnt, 11.0, None);
            let cnt_y = self.text_top(tr, &cnt, 11.0, y);
            tr.draw_text(
                scene,
                &cnt,
                11.0,
                theme.text_dim,
                t * Affine::translate((r.x1 - f64::from(cw) - 12.0, cnt_y)),
                None,
            );
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one row's full draw context: canvas + theme + session + this table's name + its y"
    )]
    fn draw_table_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        session: &Session,
        schema: &str,
        name: &str,
        r: Rect,
        y: f64,
    ) {
        let p = theme.db();
        let row_rect = Rect::new(r.x0 + 4.0, y, r.x1 - 4.0, y + self.row_h);
        let hover = row_rect.contains(self.cursor);
        let relation = RelationRef {
            schema: schema.to_string(),
            name: name.to_string(),
        };
        let selected = self.selected.as_ref() == Some(&relation);
        if selected {
            scene.fill(
                Fill::NonZero,
                t,
                p.selection,
                None,
                &row_rect.to_rounded_rect(5.0),
            );
        } else if hover {
            scene.fill(
                Fill::NonZero,
                t,
                p.hover,
                None,
                &row_rect.to_rounded_rect(5.0),
            );
        }
        let open = self.expanded_tables.contains(&relation);
        let tri = if open { "▾" } else { "▸" };
        let info = session.schema.find_relation(schema, name);
        let is_view = info.is_some_and(|t| t.kind == RelationKind::View);
        let tri_y = self.text_top(tr, tri, 10.0, y);
        tr.draw_text(
            scene,
            tri,
            10.0,
            theme.text_dim,
            t * Affine::translate((r.x0 + INDENT + 4.0, tri_y)),
            None,
        );
        // Table/view glyph.
        let glyph = if is_view { "◫" } else { "▦" };
        let gcolor = if is_view {
            theme.tokens.colors.semantic.info
        } else {
            theme.accent_primary
        };
        let glyph_y = self.text_top(tr, glyph, 12.0, y);
        tr.draw_text(
            scene,
            glyph,
            12.0,
            gcolor,
            t * Affine::translate((r.x0 + INDENT + 18.0, glyph_y)),
            None,
        );
        // Elide rather than wrap: a wrapped name would paint into the next row.
        let name_max = (r.width() - INDENT - 80.0).max(24.0) as f32;
        let name = tr.elide_to_width(name, NAME_FONT, name_max);
        let name_y = self.text_top(tr, &name, NAME_FONT, y);
        tr.draw_text(
            scene,
            &name,
            NAME_FONT,
            theme.text_primary,
            t * Affine::translate((r.x0 + INDENT + 36.0, name_y)),
            None,
        );
        // Row count badge.
        if let Some(info) = info
            && let Some(rc) = info.row_count
        {
            let badge = format!("{rc}");
            let (bw, _) = tr.measure(&badge, 10.5, None);
            let badge_y = self.text_top(tr, &badge, 10.5, y);
            tr.draw_text(
                scene,
                &badge,
                10.5,
                theme.text_dim,
                t * Affine::translate((r.x1 - f64::from(bw) - 14.0, badge_y)),
                None,
            );
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one row's full draw context: canvas + theme + session + this column's table/text/icon + its y"
    )]
    fn draw_column_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        session: &Session,
        schema: &str,
        table: &str,
        text: &str,
        icon: &str,
        r: Rect,
        y: f64,
    ) {
        let p = theme.db();
        let row_rect = Rect::new(r.x0 + 4.0, y, r.x1 - 4.0, y + self.row_h);
        let hover = row_rect.contains(self.cursor);
        if hover {
            scene.fill(
                Fill::NonZero,
                t,
                p.hover,
                None,
                &row_rect.to_rounded_rect(5.0),
            );
        }
        // Resolve the type colour from the live schema so it tracks the
        // theme (avoids storing a stale colour).
        let (icon_color, _cls) = Self::column_class(session, schema, table, text)
            .map_or((theme.text_dim, crate::model::TypeClass::Unknown), |c| {
                (theme.type_color(c), c)
            });

        // Type icon chip (one `s4` square), centred on the row; its glyph
        // centred on the chip.
        let chip_side = f64::from(theme.tokens.spacing.s4);
        let chip_y0 = y + (self.row_h - chip_side) / 2.0;
        let chip_x0 = r.x0 + INDENT * 2.0 + 6.0;
        let chip = Rect::new(chip_x0, chip_y0, chip_x0 + chip_side, chip_y0 + chip_side)
            .to_rounded_rect(4.0);
        scene.fill(Fill::NonZero, t, icon_color, None, &chip);
        let (iw, ih) = tr.measure(icon, 10.0, None);
        tr.draw_text(
            scene,
            icon,
            10.0,
            theme.tokens.colors.on_surface(icon_color).ink,
            t * Affine::translate((
                chip_x0 + (chip_side - f64::from(iw)) / 2.0,
                chip_y0 + (chip_side - f64::from(ih)) / 2.0,
            )),
            None,
        );
        // Elide rather than wrap: a wrapped column would paint into the next row.
        let text_max = (r.width() - INDENT * 2.0 - 40.0).max(24.0) as f32;
        let text = tr.elide_to_width(text, 11.5, text_max);
        let text_y = self.text_top(tr, &text, 11.5, y);
        tr.draw_text(
            scene,
            &text,
            11.5,
            theme.text_dim,
            t * Affine::translate((r.x0 + INDENT * 2.0 + 30.0, text_y)),
            None,
        );
    }

    /// Look up a column's type class by matching its rendered text back to
    /// the schema (the row stores the formatted string).
    fn column_class(
        session: &Session,
        schema: &str,
        table: &str,
        text: &str,
    ) -> Option<crate::model::TypeClass> {
        let info = session.schema.find_relation(schema, table)?;
        // The row text starts with the column name.
        let col_name = text.split_whitespace().next()?;
        info.columns
            .iter()
            .find(|c| c.name == col_name)
            .map(|c| c.class)
    }
}
