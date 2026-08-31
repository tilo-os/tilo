//! The schema browser tree in the sidebar: database → schema → tables →
//! columns, with a coloured type icon per column. Maintains its own
//! expansion + scroll state and renders into a caller-provided rect. Click
//! hit-testing returns a [`TreeClick`] the app turns into a session action.

use std::collections::HashSet;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use crate::model::RelationKind;
use crate::session::Session;
use crate::theme_ext::ThemeExt;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

const ROW_H: f64 = 24.0;
const INDENT: f64 = 14.0;

/// What a click in the tree resolved to.
#[derive(Clone, Debug)]
pub enum TreeClick {
    ToggleSchema(String),
    ToggleTable(String),
    SelectTable(String),
}

/// One flattened, rendered row of the tree, kept so hit-testing can map a
/// click y-coordinate back to an action.
#[derive(Clone)]
enum Row {
    Schema {
        name: String,
    },
    Table {
        name: String,
    },
    Column {
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
    expanded_tables: HashSet<String>,
    selected: Option<String>,
    /// Rows produced by the last `draw`, used for hit-testing.
    rows: Vec<Row>,
}

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
        }
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

    pub fn set_selected(&mut self, name: &str) {
        self.selected = Some(name.to_string());
    }

    pub fn toggle(&mut self, table: &str) {
        if !self.expanded_tables.remove(table) {
            self.expanded_tables.insert(table.to_string());
        }
    }

    pub fn toggle_schema(&mut self, schema: &str) {
        if !self.expanded_schemas.remove(schema) {
            self.expanded_schemas.insert(schema.to_string());
        }
    }

    pub fn scroll(&mut self, delta: isize) {
        let vis = self.visible_rows();
        let max = self.rows.len().saturating_sub(vis);
        let next = self.scroll as isize + delta;
        self.scroll = next.clamp(0, max as isize) as usize;
    }

    fn visible_rows(&self) -> usize {
        (((self.rect.height() - 30.0) / ROW_H).floor()).max(0.0) as usize
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
                    name: table.name.clone(),
                });
                if self.expanded_tables.contains(&table.name) {
                    for col in &table.columns {
                        let pk = if col.primary_key { "  PK" } else { "" };
                        let nn = if col.not_null && !col.primary_key {
                            " ·"
                        } else {
                            ""
                        };
                        let text = format!("{}{}{}  {}", col.name, pk, nn, col.decl_type);
                        rows.push(Row::Column {
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
        let body_top = self.rect.y0 + 26.0;
        if p.y < body_top {
            return None;
        }
        let rel = ((p.y - body_top) / ROW_H).floor() as usize;
        let idx = self.scroll + rel;
        match self.rows.get(idx)? {
            Row::Schema { name } => Some(TreeClick::ToggleSchema(name.clone())),
            Row::Table { name } => {
                // Click on the left disclosure triangle toggles; click on the
                // label selects. We treat the leftmost INDENT*1.5 as the
                // toggle zone.
                let toggle_zone = self.rect.x0 + INDENT * 2.2;
                if p.x < toggle_zone {
                    Some(TreeClick::ToggleTable(name.clone()))
                } else {
                    Some(TreeClick::SelectTable(name.clone()))
                }
            }
            Row::Column { table, .. } => Some(TreeClick::SelectTable(table.clone())),
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

        // Section header.
        tr.draw_text(
            scene,
            "SCHEMA",
            10.0,
            theme.text_dim,
            t * Affine::translate((r.x0 + 14.0, r.y0 + 6.0)),
            None,
        );

        // Rebuild rows each frame (cheap; schema is small).
        self.rows = self.build_rows(session);

        let body_top = r.y0 + 26.0;
        let vis = self.visible_rows();

        for vi in 0..vis {
            let idx = self.scroll + vi;
            let Some(row) = self.rows.get(idx) else { break };
            let y = body_top + vi as f64 * ROW_H;
            match row {
                Row::Schema { name } => {
                    self.draw_schema_row(scene, t, tr, theme, session, name, r, y);
                }
                Row::Table { name } => {
                    self.draw_table_row(scene, t, tr, theme, session, name, r, y);
                }
                Row::Column {
                    table, text, icon, ..
                } => self.draw_column_row(scene, t, tr, theme, session, table, text, icon, r, y),
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
        tr.draw_text(
            scene,
            tri,
            11.0,
            theme.text_dim,
            t * Affine::translate((r.x0 + 8.0, y + 5.0)),
            None,
        );
        tr.draw_text(
            scene,
            name,
            12.5,
            theme.text_primary,
            t * Affine::translate((r.x0 + 24.0, y + 4.0)),
            None,
        );
        // Table count.
        if let Some(s) = session.schema.schemas.iter().find(|s| s.name == *name) {
            let cnt = format!("{}", s.tables.len());
            let (cw, _) = tr.measure(&cnt, 11.0, None);
            tr.draw_text(
                scene,
                &cnt,
                11.0,
                theme.text_dim,
                t * Affine::translate((r.x1 - f64::from(cw) - 12.0, y + 5.0)),
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
        name: &str,
        r: Rect,
        y: f64,
    ) {
        let p = theme.db();
        let row_rect = Rect::new(r.x0 + 4.0, y, r.x1 - 4.0, y + ROW_H);
        let hover = row_rect.contains(self.cursor);
        let selected = self.selected.as_deref() == Some(name);
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
        let open = self.expanded_tables.contains(name);
        let tri = if open { "▾" } else { "▸" };
        let info = session.schema.find_table(name);
        let is_view = info.is_some_and(|t| t.kind == RelationKind::View);
        tr.draw_text(
            scene,
            tri,
            10.0,
            theme.text_dim,
            t * Affine::translate((r.x0 + INDENT + 4.0, y + 6.0)),
            None,
        );
        // Table/view glyph.
        let glyph = if is_view { "◫" } else { "▦" };
        let gcolor = if is_view {
            theme.tokens.colors.semantic.info
        } else {
            theme.accent_primary
        };
        tr.draw_text(
            scene,
            glyph,
            12.0,
            gcolor,
            t * Affine::translate((r.x0 + INDENT + 18.0, y + 4.0)),
            None,
        );
        tr.draw_text(
            scene,
            name,
            12.5,
            theme.text_primary,
            t * Affine::translate((r.x0 + INDENT + 36.0, y + 4.0)),
            Some((r.width() - INDENT - 80.0) as f32),
        );
        // Row count badge.
        if let Some(info) = info
            && let Some(rc) = info.row_count
        {
            let badge = format!("{rc}");
            let (bw, _) = tr.measure(&badge, 10.5, None);
            tr.draw_text(
                scene,
                &badge,
                10.5,
                theme.text_dim,
                t * Affine::translate((r.x1 - f64::from(bw) - 14.0, y + 5.0)),
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
        table: &str,
        text: &str,
        icon: &str,
        r: Rect,
        y: f64,
    ) {
        let p = theme.db();
        let row_rect = Rect::new(r.x0 + 4.0, y, r.x1 - 4.0, y + ROW_H);
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
        let (icon_color, _cls) = Self::column_class(session, table, text)
            .map_or((theme.text_dim, crate::model::TypeClass::Unknown), |c| {
                (theme.type_color(c), c)
            });

        // Type icon chip.
        let chip = Rect::new(
            r.x0 + INDENT * 2.0 + 6.0,
            y + 4.0,
            r.x0 + INDENT * 2.0 + 22.0,
            y + 20.0,
        )
        .to_rounded_rect(4.0);
        scene.fill(Fill::NonZero, t, icon_color, None, &chip);
        tr.draw_text(
            scene,
            icon,
            10.0,
            Color::from_rgb8(20, 20, 20),
            t * Affine::translate((r.x0 + INDENT * 2.0 + 10.0, y + 5.0)),
            None,
        );
        tr.draw_text(
            scene,
            text,
            11.5,
            theme.text_dim,
            t * Affine::translate((r.x0 + INDENT * 2.0 + 30.0, y + 5.0)),
            Some((r.width() - INDENT * 2.0 - 40.0) as f32),
        );
    }

    /// Look up a column's type class by matching its rendered text back to
    /// the schema (the row stores the formatted string).
    fn column_class(session: &Session, table: &str, text: &str) -> Option<crate::model::TypeClass> {
        let info = session.schema.find_table(table)?;
        // The row text starts with the column name.
        let col_name = text.split_whitespace().next()?;
        info.columns
            .iter()
            .find(|c| c.name == col_name)
            .map(|c| c.class)
    }
}
