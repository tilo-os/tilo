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

/// Compare before converting: rounding an integer to f64 can equate adjacent IDs.
fn compare_integer_real(integer: i64, real: f64) -> Ordering {
    // i64::MAX rounds up to 2^63 as f64, so the upper bound is exclusive.
    // NaNs sort after all other numbers, rather than equating unequal values.
    if real.is_nan() || real >= 9_223_372_036_854_775_808.0 {
        return Ordering::Less;
    }
    if real < -9_223_372_036_854_775_808.0 {
        return Ordering::Greater;
    }
    integer.cmp(&(real as i64)).then_with(|| {
        0.0_f64
            .partial_cmp(&real.fract())
            .unwrap_or(Ordering::Equal)
    })
}

/// Nulls first, then numbers by value, then everything else by its text
/// without regard to case.
fn compare_cells(a: Option<&Cell>, b: Option<&Cell>) -> Ordering {
    match (a, b) {
        (None | Some(Cell::Null), None | Some(Cell::Null)) => Ordering::Equal,
        (None | Some(Cell::Null), Some(_)) => Ordering::Less,
        (Some(_), None | Some(Cell::Null)) => Ordering::Greater,
        // Database integers exceed f64's exact range (2^53), notably IDs.
        (Some(Cell::Integer(a)), Some(Cell::Integer(b))) => a.cmp(b),
        (Some(Cell::Integer(a)), Some(Cell::Real(b))) => compare_integer_real(*a, *b),
        (Some(Cell::Real(a)), Some(Cell::Integer(b))) => compare_integer_real(*b, *a).reverse(),
        (Some(Cell::Real(a)), Some(Cell::Real(b))) => a
            .partial_cmp(b)
            .unwrap_or_else(|| a.is_nan().cmp(&b.is_nan())),
        // Cross-type comparisons must use the same rank for both numeric types:
        // comparing their display text against nonnumbers would create cycles.
        (Some(Cell::Integer(_) | Cell::Real(_)), Some(_)) => Ordering::Less,
        (Some(_), Some(Cell::Integer(_) | Cell::Real(_))) => Ordering::Greater,
        (Some(a), Some(b)) => a.display().to_lowercase().cmp(&b.display().to_lowercase()),
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

    #[test]
    fn sorting_integer_ids_preserves_all_64_bits() {
        let values = [
            i64::MAX,
            9_007_199_254_740_993,
            9_007_199_254_740_992,
            i64::MIN + 1,
            i64::MIN,
            i64::MAX - 1,
        ];
        let mut table = ResultsTable::new("results");
        table.bind(Some(Arc::new(QueryResult {
            columns: vec!["id".into()],
            rows: values
                .iter()
                .map(|value| vec![Cell::Integer(*value)])
                .collect(),
            is_select: true,
            ..Default::default()
        })));
        table.sort_by(0);
        assert_eq!(table.display_order(), [4, 3, 2, 1, 5, 0]);
        table.sort_by(0);
        assert_eq!(table.display_order(), [0, 5, 1, 2, 3, 4]);
    }

    #[test]
    fn mixed_types_sort_consistently_in_every_input_order() {
        // SQLite columns can contain numbers and text in the same result.
        // Comparing cross-type pairs as text used to create 2 < 10.0 < "11" < 2.
        let cells = [Cell::Integer(2), Cell::Real(10.0), Cell::Text("11".into())];
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let mut table = ResultsTable::new("results");
            table.bind(Some(Arc::new(QueryResult {
                columns: vec!["mixed".into()],
                rows: order.iter().map(|&i| vec![cells[i].clone()]).collect(),
                is_select: true,
                ..Default::default()
            })));
            table.sort_by(0);
            let ascending: Vec<_> = table.display_order().iter().map(|&i| order[i]).collect();
            assert_eq!(ascending, [0, 1, 2], "input: {order:?}");
            table.sort_by(0);
            let descending: Vec<_> = table.display_order().iter().map(|&i| order[i]).collect();
            assert_eq!(descending, [2, 1, 0], "input: {order:?}");
        }
    }

    #[test]
    fn mixed_numbers_compare_exactly_and_sort_consistently() {
        // Ascending equivalence classes, including nulls, nonnumbers, both sides
        // of f64's exact integer range and i64's bounds. Every triple must be transitive.
        let groups = [
            vec![Cell::Null],
            vec![Cell::Real(f64::NEG_INFINITY)],
            vec![Cell::Real(-18_446_744_073_709_551_616.0)],
            vec![Cell::Integer(i64::MIN), Cell::Real(i64::MIN as f64)],
            vec![Cell::Integer(i64::MIN + 1)],
            vec![Cell::Integer(-9_007_199_254_740_993)],
            vec![
                Cell::Integer(-9_007_199_254_740_992),
                Cell::Real(-9_007_199_254_740_992.0),
            ],
            vec![Cell::Real(-1.5)],
            vec![Cell::Integer(-1)],
            vec![Cell::Real(-0.5)],
            vec![Cell::Integer(0), Cell::Real(-0.0), Cell::Real(0.0)],
            vec![Cell::Real(0.5)],
            vec![Cell::Integer(1)],
            vec![Cell::Real(1.5)],
            vec![Cell::Integer(2)],
            vec![Cell::Real(10.0)],
            vec![
                Cell::Integer(9_007_199_254_740_992),
                Cell::Real(9_007_199_254_740_992.0),
            ],
            vec![Cell::Integer(9_007_199_254_740_993)],
            vec![Cell::Integer(i64::MAX - 1)],
            vec![Cell::Integer(i64::MAX)],
            vec![Cell::Real(9_223_372_036_854_775_808.0)],
            vec![Cell::Real(f64::INFINITY)],
            vec![Cell::Real(f64::NAN)],
            vec![Cell::Text(String::new())],
            vec![Cell::Text("11".into())],
            vec![Cell::Text("A".into()), Cell::Text("a".into())],
            vec![Cell::Blob(10), Cell::Text("BINARY, 10 BYTES".into())],
            vec![Cell::Blob(2)],
            vec![Cell::Text("z".into())],
            vec![Cell::Text("Ä".into()), Cell::Text("ä".into())],
        ];
        let values: Vec<_> = groups
            .iter()
            .enumerate()
            .flat_map(|(rank, group)| group.iter().map(move |cell| (rank, cell)))
            .collect();
        for &(rank_a, a) in &values {
            for &(rank_b, b) in &values {
                assert_eq!(
                    compare_cells(Some(a), Some(b)),
                    rank_a.cmp(&rank_b),
                    "{a:?} vs {b:?}"
                );
                for &(_, c) in &values {
                    if compare_cells(Some(a), Some(b)).is_le()
                        && compare_cells(Some(b), Some(c)).is_le()
                    {
                        assert!(compare_cells(Some(a), Some(c)).is_le());
                    }
                }
            }
        }
        let mut table = ResultsTable::new("results");
        table.bind(Some(Arc::new(QueryResult {
            columns: vec!["number".into()],
            rows: values
                .iter()
                .rev()
                .map(|(_, cell)| vec![(*cell).clone()])
                .collect(),
            is_select: true,
            ..Default::default()
        })));
        let ranks: Vec<_> = values.iter().rev().map(|(rank, _)| *rank).collect();
        table.sort_by(0);
        let ascending: Vec<_> = table
            .display_order()
            .iter()
            .map(|&row| ranks[row])
            .collect();
        assert_eq!(
            ascending,
            values.iter().map(|(rank, _)| *rank).collect::<Vec<_>>()
        );
        table.sort_by(0);
        let descending: Vec<_> = table
            .display_order()
            .iter()
            .map(|&row| ranks[row])
            .collect();
        assert_eq!(descending, ranks);
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
