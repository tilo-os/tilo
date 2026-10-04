//! A query or table result as the toolkit's [`DataTable`]: columns sized to
//! what they hold and together as wide as the pane, numbers in tabular
//! figures, `NULL` as an absent value, and a sort that follows a header click.

use std::cell::Cell as Flag;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use taffy::{Dimension, NodeId, TaffyTree};
use ui::text_render::TextKind;
use ui::theme::Theme;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable, text_band};
use ui::widget::{EventCtx, Widget};
use vello::kurbo::Point;
use winit::event::WindowEvent;

use crate::model::{Cell, QueryResult};

/// A row of the table: its index in the result it shows.
type RowIndex = usize;

/// How many rows a column's share of the width is judged from.
const SAMPLE_ROWS: usize = 200;

/// The longest text that still asks for more room: past it a column elides.
const WIDEST_TEXT: usize = 48;

/// Room a column's text needs besides its characters: the cell padding and,
/// in a header, the sort arrow.
const PADDING_CHARS: usize = 4;

/// A column's share of the pane's width, from the text it shows: the longest
/// of its header and the rows it was judged from, in characters. Shares
/// divide the whole width, so the columns always fill the pane and a pane too
/// narrow for all of them elides each in proportion.
pub fn column_share<'a>(header: &str, cells: impl IntoIterator<Item = &'a str>) -> f32 {
    let longest = cells
        .into_iter()
        .map(|cell| cell.chars().count())
        .chain(std::iter::once(header.chars().count()))
        .max()
        .unwrap_or(0)
        .min(WIDEST_TEXT);
    let chars = u16::try_from(longest + PADDING_CHARS).unwrap_or(u16::MAX);
    f32::from(chars)
}

/// Let the node take the height its parent has left, rather than the height
/// of its content: the table then scrolls within what it is given.
pub fn fill_remaining_height(taffy: &mut TaffyTree<()>, node: NodeId) {
    if let Ok(mut style) = taffy.style(node).cloned() {
        style.size.height = Dimension::auto();
        style.min_size.height = Dimension::length(0.0);
        style.flex_grow = 1.0;
        style.flex_shrink = 1.0;
        style.flex_basis = Dimension::length(0.0);
        let _ = taffy.set_style(node, style);
    }
}

/// A line of text above a table, lined up with the text in the cells below
/// it. Under a divider it also keeps clear of it.
pub struct CaptionLine {
    text: ui::widget::Text,
    under_divider: bool,
}

impl CaptionLine {
    /// A line right above the rows.
    pub fn new(text: ui::widget::Text) -> Self {
        Self {
            text,
            under_divider: false,
        }
    }

    /// A line right below the divider of a split.
    pub fn under_divider(text: ui::widget::Text) -> Self {
        Self {
            text,
            under_divider: true,
        }
    }
}

impl ui::widget::Decorator for CaptionLine {
    type Inner = ui::widget::Text;

    fn inner(&self) -> &ui::widget::Text {
        &self.text
    }

    fn inner_mut(&mut self) -> &mut ui::widget::Text {
        &mut self.text
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Widget::build_layout(&mut self.text, taffy, theme, node_map);
        if let Ok(mut style) = taffy.style(node).cloned() {
            let spacing = &theme.tokens.spacing;
            style.margin.left = taffy::style_helpers::length(spacing.s2);
            if self.under_divider {
                style.margin.top = taffy::style_helpers::length(spacing.s3);
            }
            let _ = taffy.set_style(node, style);
        }
        node
    }

    fn name(&self) -> &'static str {
        "CaptionLine"
    }
}

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
                    let shown: Vec<String> = result
                        .rows
                        .iter()
                        .take(SAMPLE_ROWS)
                        .filter_map(|row| row.get(index).map(Cell::display))
                        .collect();
                    let share = column_share(header, shown.iter().map(String::as_str));
                    ColumnDef::new(
                        format!("c{index}"),
                        header.clone(),
                        ColumnWidth::Flex(share),
                    )
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
        let node = Widget::build_layout(&mut self.table, taffy, theme, node_map);
        fill_remaining_height(taffy, node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // The table scrolls inside the room its parent gave it, keeping the
        // header in view, and is only as tall as its rows: the column rules
        // and the stripes stop with the last row instead of running on over
        // empty ground. Tell it how tall its body is.
        let height = f64::from(taffy.layout(node).map_or(0.0, |layout| layout.size.height));
        let room = (height - self.header).max(0.0);
        let rows = self.result.as_ref().map_or(0, |result| result.rows.len());
        let content = self.header * rows as f64;
        #[allow(clippy::cast_possible_truncation, reason = "a screen height")]
        let body = room.min(content) as f32;
        if body > 0.0 && (body - self.body).abs() > 0.5 {
            self.body = body;
            #[allow(clippy::cast_possible_truncation, reason = "a row height")]
            self.table.set_viewport(body, self.header as f32);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> Arc<QueryResult> {
        Arc::new(QueryResult {
            columns: vec!["n".into(), "name".into(), "mixed".into()],
            rows: vec![
                vec![Cell::Integer(3), Cell::Text("c".into()), Cell::Integer(1)],
                vec![
                    Cell::Integer(1),
                    Cell::Text("a".into()),
                    Cell::Text("x".into()),
                ],
                vec![Cell::Null, Cell::Text("B".into()), Cell::Null],
            ],
            is_select: true,
            ..Default::default()
        })
    }

    /// A column is numeric when every value that is there is a number, and nulls are not values.
    #[test]
    fn numeric_columns_are_the_ones_holding_only_numbers() {
        let result = result();
        assert!(is_numeric_column(&result, 0), "nulls do not make it text");
        assert!(!is_numeric_column(&result, 1));
        assert!(!is_numeric_column(&result, 2), "one text value does");
        assert!(!is_numeric_column(&result, 9), "no such column");
    }

    /// Ascending puts nulls first, then numbers by value; a second click on the same column flips it; another column starts ascending.
    #[test]
    fn a_header_click_sorts_and_the_next_one_flips() {
        let mut table = ResultsTable::new("results");
        table.bind(Some(result()));
        assert_eq!(table.sorted(), None);
        assert_eq!(table.display_order(), [0, 1, 2], "rows start as returned");

        table.sort_by(0);
        assert_eq!(table.sorted(), Some((0, true)));
        assert_eq!(table.display_order(), [2, 1, 0], "null, 1, 3");

        table.sort_by(0);
        assert_eq!(table.sorted(), Some((0, false)));
        assert_eq!(table.display_order(), [0, 1, 2], "3, 1, null");

        table.sort_by(1);
        assert_eq!(table.sorted(), Some((1, true)));
        assert_eq!(
            table.display_order(),
            [1, 2, 0],
            "text compares without regard to case: a, B, c"
        );

        // A new result starts unsorted.
        table.bind(Some(result()));
        assert_eq!(table.sorted(), None);
    }

    /// Text sorts without regard to case and numbers sort by value, not by their digits.
    #[test]
    fn cells_compare_by_value() {
        let ten = Cell::Integer(10);
        let two = Cell::Integer(2);
        assert_eq!(compare_cells(Some(&two), Some(&ten)), Ordering::Less);
        assert_eq!(
            compare_cells(Some(&Cell::Text("b".into())), Some(&Cell::Text("A".into()))),
            Ordering::Greater
        );
        assert_eq!(compare_cells(None, Some(&Cell::Null)), Ordering::Equal);
        assert_eq!(compare_cells(Some(&Cell::Null), Some(&two)), Ordering::Less);
    }

    /// A column asks for its longest text plus room for padding and the sort arrow, and a very long text does not starve the others.
    #[test]
    fn a_column_asks_for_the_width_of_its_text() {
        let short = column_share("id", ["1", "22"]);
        let header = column_share("release_date", ["1995"]);
        let cells = column_share("n", ["Strings at Dusk"]);
        assert!(header > short, "a long header asks for more");
        assert!(cells > short, "long cells ask for more");
        assert!(
            header >= 12.0 + 4.0,
            "a header always has room for its own text"
        );
        let essay = "x".repeat(500);
        let widest = column_share("body", [&essay[..WIDEST_TEXT]]);
        assert!(
            (column_share("body", [essay.as_str()]) - widest).abs() < f32::EPSILON,
            "a very long text asks for no more than the widest"
        );
    }

    /// The table is as wide as its pane, only as tall as its rows, and takes the room it has left when there are more rows than room.
    #[test]
    fn the_table_fills_its_pane_and_stops_at_its_rows() {
        use ui::headless::FrameHarness;
        use ui::theme::Theme;
        let rows = |n: usize| {
            Arc::new(QueryResult {
                columns: vec!["id".into(), "title".into()],
                rows: (0..n)
                    .map(|i| vec![Cell::Integer(i as i64), Cell::Text(format!("Row {i}"))])
                    .collect(),
                is_select: true,
                ..Default::default()
            })
        };
        for (count, scrolls) in [(3, false), (200, true)] {
            let mut table = ResultsTable::new("results");
            table.bind(Some(rows(count)));
            let mut harness =
                FrameHarness::for_root(ui::State::new(), table, (900, 400), Theme::dark());
            harness.relayout();
            harness.relayout();
            let rect = harness.targets()["results"];
            assert!(
                (rect.width() - 900.0).abs() < 1.0,
                "the table is as wide as the pane: {rect:?}"
            );
            let table = harness.root::<ResultsTable>();
            let viewport = f64::from(table.body);
            assert!(
                viewport <= rect.height(),
                "the body fits in the pane: {viewport} in {rect:?}"
            );
            assert_eq!(
                table.table.max_scroll_y() > 0.0,
                scrolls,
                "{count} rows in {rect:?}"
            );
            if !scrolls {
                assert!(
                    viewport < rect.height() - 100.0,
                    "a short result is only as tall as its rows: {viewport} in {rect:?}"
                );
            }
        }
    }
}
