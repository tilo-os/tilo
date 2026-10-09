//! The sample workbook screenshots show: a monthly budget with SUM /
//! AVERAGE / IF formulas, and a quarterly sales sheet with growth %, written
//! as the edits and verbs a person building it would make.
//!
//! Every identity is fixed, so a capture stages the same workbook on every
//! run.

use ui::tilo_resource::{Atom, Id, Invoke};

use crate::model::{CellFormat, Currency, DateStyle, a1_to_addr};
use crate::resource::verbs;
use crate::workbook::{Cell, ChartKind, Comment, FIRST_SHEET_ID, Sheet, Workbook};

/// The budget sheet's identity: a new workbook's first sheet, which the
/// sample fills.
pub const BUDGET_SHEET_ID: &str = FIRST_SHEET_ID;
/// The sales sheet's identity.
pub const SALES_SHEET_ID: &str = "5d1c1a52-6a1e-4c7e-9d0b-0b5a6c2e0002";

/// Cells of one sample sheet, in the order they were entered.
struct Cells(Sheet);

impl Cells {
    fn named(name: &str) -> Self {
        Self(Sheet::named(name))
    }

    fn set(&mut self, a1: &str, raw: &str) {
        if let Some(addr) = a1_to_addr(a1) {
            self.0.update(addr, |cell| cell.raw = Atom(raw.to_owned()));
        }
    }

    fn format(&mut self, a1: &str, fmt: CellFormat) {
        if let Some(addr) = a1_to_addr(a1) {
            self.0.update(addr, |cell| cell.format = fmt);
        }
    }
}

/// The sample workbook: a new workbook's blank first sheet filled as the
/// budget, and the sales sheet after it — the edits that write it, in order.
pub fn workbook() -> Vec<Invoke<Workbook>> {
    let sheets = Workbook::paths().sheets;
    let mut edits = sheets
        .item_path(&budget_id())
        .edits(&Sheet::named("Sheet1"), &budget());
    edits.push(sheets.insert_as(&Id::from_raw(SALES_SHEET_ID), 1, &sales()));
    edits
        .iter()
        .filter_map(|edit| Invoke::edit(edit).ok())
        .collect()
}

fn budget() -> Sheet {
    let mut cells = Cells::named("Budget");
    for (a1, raw) in [
        ("A1", "Category"),
        ("B1", "Jan"),
        ("C1", "Feb"),
        ("D1", "Mar"),
        ("E1", "Q1 Total"),
        ("F1", "Average"),
        ("G1", "Updated"),
    ] {
        cells.set(a1, raw);
    }
    let rows: &[(&str, f64, f64, f64)] = &[
        ("Rent", 1200.0, 1200.0, 1200.0),
        ("Groceries", 420.0, 460.0, 510.0),
        ("Utilities", 180.0, 165.0, 200.0),
        ("Transport", 90.0, 110.0, 95.0),
        ("Entertainment", 150.0, 200.0, 175.0),
    ];
    for (i, (name, jan, feb, mar)) in rows.iter().enumerate() {
        let r = i + 2;
        cells.set(&format!("A{r}"), name);
        cells.set(&format!("B{r}"), &jan.to_string());
        cells.set(&format!("C{r}"), &feb.to_string());
        cells.set(&format!("D{r}"), &mar.to_string());
        cells.set(&format!("E{r}"), &format!("=SUM(B{r}:D{r})"));
        cells.set(&format!("F{r}"), &format!("=AVERAGE(B{r}:D{r})"));
    }
    for (a1, raw) in [
        ("A7", "Total"),
        ("B7", "=SUM(B2:B6)"),
        ("C7", "=SUM(C2:C6)"),
        ("D7", "=SUM(D2:D6)"),
        ("E7", "=SUM(E2:E6)"),
        ("F7", "=AVERAGE(B7:D7)"),
        ("A9", "Monthly budget"),
        ("B9", "2600"),
        ("A10", "Over budget?"),
        ("B10", "=IF(B7>B9,\"OVER\",\"OK\")"),
        ("A11", "Headroom"),
        ("B11", "=B9-B7"),
        ("A12", "Tax set-aside"),
        ("B12", "0.18"),
        ("G2", "2024-01-15"),
    ] {
        cells.set(a1, raw);
    }
    cells.format("B12", CellFormat::Percent);
    for r in 2..=7 {
        for col in ["B", "C", "D", "E"] {
            cells.format(&format!("{col}{r}"), CellFormat::Currency(Currency::USD));
        }
        cells.format(&format!("F{r}"), CellFormat::Number);
    }
    cells.format("B9", CellFormat::Currency(Currency::USD));
    cells.format("B11", CellFormat::Currency(Currency::USD));
    cells.format("G2", CellFormat::Date(DateStyle::Iso));
    cells.0
}

fn sales() -> Sheet {
    let mut cells = Cells::named("Sales");
    for (a1, raw) in [
        ("A1", "Region"),
        ("B1", "Q1"),
        ("C1", "Q2"),
        ("D1", "Q3"),
        ("E1", "Q4"),
        ("F1", "Year"),
        ("G1", "Growth"),
    ] {
        cells.set(a1, raw);
    }
    let rows: &[(&str, f64, f64, f64, f64)] = &[
        ("North", 12000.0, 13500.0, 14200.0, 15800.0),
        ("South", 9800.0, 10200.0, 9900.0, 11500.0),
        ("East", 15200.0, 16100.0, 17400.0, 18900.0),
        ("West", 8400.0, 9100.0, 9700.0, 10800.0),
    ];
    for (i, (name, q1, q2, q3, q4)) in rows.iter().enumerate() {
        let r = i + 2;
        cells.set(&format!("A{r}"), name);
        cells.set(&format!("B{r}"), &q1.to_string());
        cells.set(&format!("C{r}"), &q2.to_string());
        cells.set(&format!("D{r}"), &q3.to_string());
        cells.set(&format!("E{r}"), &q4.to_string());
        cells.set(&format!("F{r}"), &format!("=SUM(B{r}:E{r})"));
        cells.set(&format!("G{r}"), &format!("=(E{r}-B{r})/B{r}"));
    }
    for (a1, raw) in [
        ("A6", "Total"),
        ("B6", "=SUM(B2:B5)"),
        ("C6", "=SUM(C2:C5)"),
        ("D6", "=SUM(D2:D5)"),
        ("E6", "=SUM(E2:E5)"),
        ("F6", "=SUM(F2:F5)"),
    ] {
        cells.set(a1, raw);
    }
    for r in 2..=6 {
        for col in ["B", "C", "D", "E", "F"] {
            cells.format(&format!("{col}{r}"), CellFormat::Currency(Currency::EUR));
        }
        cells.format(&format!("G{r}"), CellFormat::Percent);
    }
    cells.0
}

fn budget_id() -> Id {
    Id::from_raw(BUDGET_SHEET_ID)
}

/// Two charts over the budget.
pub fn charts() -> Vec<Invoke<Workbook>> {
    let chart = |title: &str, kind: ChartKind, range: &str| {
        Workbook::ADD_CHART.with(verbs::AddChart {
            sheet: budget_id(),
            range: range.to_owned(),
            kind,
            title: title.to_owned(),
        })
    };
    vec![
        chart("Quarterly spend", ChartKind::Column, "B2:E7"),
        chart("Monthly trend", ChartKind::Line, "B2:D7"),
    ]
}

/// The sample comment thread's identity.
const COMMENT_ID: &str = "5d1c1a52-6a1e-4c7e-9d0b-0b5a6c2e0201";

/// A comment thread on the budget, each by the person who wrote it: Alex
/// asks, Sam replies.
pub fn comments() -> Vec<(&'static str, Invoke<Workbook>)> {
    let mut budget = budget();
    let comment = budget.area((1, 1), (4, 1)).map(|area| Comment {
        sheet: budget_id(),
        area,
        author: Atom("Alex".into()),
        body: "Check the rent increase before sending this budget.".into(),
        resolved: false,
        replies: ui::tilo_resource::Collection::new(),
    });
    let ask = comment.and_then(|comment| {
        let path = Workbook::paths().comments;
        Invoke::edit(&path.insert_as(&Id::from_raw(COMMENT_ID), 0, &comment)).ok()
    });
    let reply = Workbook::REPLY.with(verbs::Reply {
        comment: Id::from_raw(COMMENT_ID),
        text: "I will verify the change.".into(),
    });
    ask.map(|ask| ("Alex", ask))
        .into_iter()
        .chain([("Sam", reply)])
        .collect()
}

/// Resolve the sample comment thread.
pub fn resolve_comment() -> Option<Invoke<Workbook>> {
    let path = Workbook::paths()
        .comments
        .item(&Id::from_raw(COMMENT_ID))
        .resolved;
    Invoke::edit(&path.set(true)).ok()
}

/// The seeds of the fill-series scenario: three patterns under the budget,
/// which the fill handle then extends.
pub fn fill_seeds() -> Vec<Invoke<Workbook>> {
    let cells = [
        ("A15", "Fill demo"),
        ("B15", "Count"),
        ("C15", "Weekday"),
        ("D15", "Task"),
        ("B16", "1"),
        ("B17", "2"),
        ("C16", "Mon"),
        ("D16", "Task 1"),
    ]
    .iter()
    .filter_map(|(a1, raw)| Some((a1_to_addr(a1)?, Cell::of(*raw))))
    .collect::<Vec<_>>();
    let state = {
        let mut replica = ui::tilo_resource::Replica::<Workbook>::new();
        for invoke in workbook() {
            let _ = replica.apply_confirmed(invoke);
        }
        replica.state().unwrap_or_default()
    };
    crate::workbook::cell_edits(&state, &budget_id(), &cells)
        .iter()
        .filter_map(|edit| Invoke::edit(edit).ok())
        .collect()
}

/// The sample workbook, as a replica holds it.
#[cfg(test)]
pub(crate) fn replica() -> ui::tilo_resource::Replica<Workbook> {
    let mut replica = ui::tilo_resource::Replica::<Workbook>::new();
    for invoke in workbook() {
        replica.apply_confirmed(invoke).unwrap();
    }
    replica
}
