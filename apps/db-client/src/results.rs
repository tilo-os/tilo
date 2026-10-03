//! A query or table result as the toolkit's [`DataTable`]: columns sized to
//! their content, numbers in tabular figures, `NULL` as an absent value, and a
//! sort that follows a header click.

use std::cell::Cell as Flag;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use taffy::{NodeId, TaffyTree};
use ui::text_render::TextKind;
use ui::theme::Theme;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable, text_band};
use ui::widget::{EventCtx, Widget};
use vello::kurbo::Point;
use winit::event::WindowEvent;

use crate::model::{Cell, QueryResult};

/// A row of the table: its index in the result it shows.
type RowIndex = usize;

pub struct ResultsTable {
    table: DataTable<RowIndex>,
    /// The column a header click asked to sort by, until the next event.
    clicked: Rc<Flag<Option<usize>>>,
    result: Option<Arc<QueryResult>>,
    sort: Option<(usize, bool)>,
    /// The header band the table resolved for the theme it was built with.
    header: f64,
    /// The body height the table was last told to scroll within.
    body: f32,
    name: String,
}

fn is_numeric_column(result: &QueryResult, column: usize) -> bool {
    let mut any = false;
    for row in &result.rows {
        match row.get(column) {
            Some(Cell::Integer(_) | Cell::Real(_)) => any = true,
            Some(Cell::Null) | None => {}
            Some(_) => return false,
        }
    }
    any
}

fn cell_number(cell: &Cell) -> Option<f64> {
    match cell {
        Cell::Integer(value) => Some(*value as f64),
        Cell::Real(value) => Some(*value),
        _ => None,
    }
}

/// Nulls first, then numbers by value, then everything else by its text
/// without regard to case.
fn compare_cells(a: Option<&Cell>, b: Option<&Cell>) -> Ordering {
    match (a, b) {
        (None | Some(Cell::Null), None | Some(Cell::Null)) => Ordering::Equal,
        (None | Some(Cell::Null), Some(_)) => Ordering::Less,
        (Some(_), None | Some(Cell::Null)) => Ordering::Greater,
        (Some(a), Some(b)) => match (cell_number(a), cell_number(b)) {
            (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
            _ => a.display().to_lowercase().cmp(&b.display().to_lowercase()),
        },
    }
}

impl ResultsTable {
    pub fn new(name: &str) -> Self {
        let clicked = Rc::new(Flag::new(None));
        Self {
            table: Self::table_for(None, &clicked, name),
            clicked,
            result: None,
            sort: None,
            header: 0.0,
            body: 0.0,
            name: name.to_owned(),
        }
    }

    fn table_for(
        result: Option<&Arc<QueryResult>>,
        clicked: &Rc<Flag<Option<usize>>>,
        name: &str,
    ) -> DataTable<RowIndex> {
        let columns = result.map_or_else(Vec::new, |result| {
            result
                .columns
                .iter()
                .enumerate()
                .map(|(index, header)| {
                    let rows = Arc::clone(result);
                    let kind = if is_numeric_column(result, index) {
                        TextKind::Numeric
                    } else {
                        TextKind::Prose
                    };
                    ColumnDef::new(format!("c{index}"), header.clone(), ColumnWidth::Auto)
                        .sortable(true)
                        .with_kind(kind)
                        .with_optional_text(
                            move |row: &RowIndex| {
                                let cell = rows.rows.get(*row)?.get(index)?;
                                (!cell.is_null()).then(|| cell.display())
                            },
                            "NULL",
                        )
                })
                .collect()
        });
        let flag = Rc::clone(clicked);
        DataTable::new(columns)
            .ruled()
            .with_id(name)
            .with_accessible_name(name)
            .on_header_click(move |column| flag.set(Some(column)))
    }

    /// Show `result`, replacing whatever was there. The sort a person chose
    /// does not carry over to a different result.
    pub fn bind(&mut self, result: Option<Arc<QueryResult>>) {
        self.sort = None;
        self.body = 0.0;
        self.table = Self::table_for(result.as_ref(), &self.clicked, &self.name);
        if let Some(result) = &result {
            self.table.set_rows((0..result.rows.len()).collect());
        }
        self.result = result;
    }

    /// Sort by `column`: ascending the first time, then flipping.
    fn sort_by(&mut self, column: usize) {
        let Some(result) = self.result.clone() else {
            return;
        };
        let ascending = match self.sort {
            Some((current, ascending)) if current == column => !ascending,
            _ => true,
        };
        self.sort = Some((column, ascending));
        self.table.sort_by_with(
            move |a: &RowIndex, b: &RowIndex| {
                compare_cells(
                    result.rows.get(*a).and_then(|row| row.get(column)),
                    result.rows.get(*b).and_then(|row| row.get(column)),
                )
            },
            &format!("c{column}"),
            ascending,
        );
    }

    /// The column and direction the rows are sorted by.
    pub fn sorted(&self) -> Option<(usize, bool)> {
        self.sort
    }

    /// The rows in the order they are shown, as indices into the result.
    pub fn display_order(&self) -> Vec<usize> {
        self.table.view_rows().into_iter().copied().collect()
    }
}

impl ui::widget::Decorator for ResultsTable {
    type Inner = DataTable<RowIndex>;

    fn inner(&self) -> &Self::Inner {
        &self.table
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.table
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let tokens = &theme.tokens;
        self.header = text_band(
            f64::from(tokens.metrics.min_pointer_target),
            f64::from(
                tokens
                    .typography
                    .style(ui::TextRole::Caption)
                    .line_height_px(),
            ),
            f64::from(tokens.spacing.s3),
        );
        Widget::build_layout(&mut self.table, taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // The table scrolls inside the room its parent gave it, keeping the
        // header in view: tell it how tall that room's body is.
        let height = f64::from(taffy.layout(node).map_or(0.0, |layout| layout.size.height));
        let body = (height - self.header).max(0.0);
        let row = self.header;
        #[allow(clippy::cast_possible_truncation, reason = "a screen height")]
        let body = body as f32;
        if body > 0.0 && (body - self.body).abs() > 0.5 {
            self.body = body;
            #[allow(clippy::cast_possible_truncation, reason = "a row height")]
            self.table.set_viewport(body, row as f32);
        }
        Widget::apply_layout(&mut self.table, taffy, node, origin);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        Widget::handle_event(&mut self.table, event, ctx);
        if let Some(column) = self.clicked.take() {
            self.sort_by(column);
        }
    }

    fn name(&self) -> &'static str {
        "ResultsTable"
    }
}
