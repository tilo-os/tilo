//! The sample workbook screenshots show: a monthly budget with SUM /
//! AVERAGE / IF formulas, and a quarterly sales sheet with growth %, written
//! as the actions a person importing it would take.
//!
//! Every identity is fixed, so a capture stages the same workbook on every
//! run.

use crate::actions::{CellWrite, ImportedSheet, SheetsAction};
use crate::doc::ChartKind;
use crate::model::{CellFormat, a1_to_addr};
use tilo_locale::{Currency, DateStyle};

/// The budget sheet's identity.
pub const BUDGET_SHEET_ID: &str = "5d1c1a52-6a1e-4c7e-9d0b-0b5a6c2e0001";
/// The sales sheet's identity.
pub const SALES_SHEET_ID: &str = "5d1c1a52-6a1e-4c7e-9d0b-0b5a6c2e0002";
const COLUMN_CHART_ID: &str = "5d1c1a52-6a1e-4c7e-9d0b-0b5a6c2e0101";
const LINE_CHART_ID: &str = "5d1c1a52-6a1e-4c7e-9d0b-0b5a6c2e0102";
const COMMENT_ID: &str = "5d1c1a52-6a1e-4c7e-9d0b-0b5a6c2e0201";
const REPLY_ID: &str = "5d1c1a52-6a1e-4c7e-9d0b-0b5a6c2e0202";

/// Cells of one sample sheet, in the order they were entered.
#[derive(Default)]
struct Cells(Vec<CellWrite>);

impl Cells {
    fn set(&mut self, a1: &str, raw: &str) {
        let Some((col, row)) = a1_to_addr(a1) else {
            return;
        };
        match self.0.iter_mut().find(|w| (w.col, w.row) == (col, row)) {
            Some(write) => raw.clone_into(&mut write.raw),
            None => self.0.push(CellWrite {
                col,
                row,
                raw: raw.to_owned(),
                fmt: None,
            }),
        }
    }

    fn format(&mut self, a1: &str, fmt: CellFormat) {
        let Some((col, row)) = a1_to_addr(a1) else {
            return;
        };
        match self.0.iter_mut().find(|w| (w.col, w.row) == (col, row)) {
            Some(write) => write.fmt = Some(fmt.as_str()),
            None => self.0.push(CellWrite {
                col,
                row,
                raw: String::new(),
                fmt: Some(fmt.as_str()),
            }),
        }
    }
}

/// The sample workbook, replacing a new workbook's blank first sheet.
pub fn workbook() -> SheetsAction {
    SheetsAction::ImportSheets {
        sheets: vec![budget(), sales()],
        replace_blank: true,
        source: None,
    }
}

fn budget() -> ImportedSheet {
    let mut cells = Cells::default();
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
    ImportedSheet {
        name: "Budget".into(),
        sheet_id: BUDGET_SHEET_ID.into(),
        cells: cells.0,
    }
}

fn sales() -> ImportedSheet {
    let mut cells = Cells::default();
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
    ImportedSheet {
        name: "Sales".into(),
        sheet_id: SALES_SHEET_ID.into(),
        cells: cells.0,
    }
}

/// Two charts over the budget and a resolved comment thread on its rent row.
pub fn charts() -> Vec<SheetsAction> {
    let chart = |chart_id: &str, title: &str, kind: ChartKind, c1: u32| SheetsAction::PutChart {
        sheet: 0,
        sheet_id: BUDGET_SHEET_ID.into(),
        chart_id: chart_id.into(),
        title: title.into(),
        kind: kind.code().into(),
        c0: 1,
        r0: 1,
        c1,
        r1: 6,
    };
    vec![
        chart(COLUMN_CHART_ID, "Quarterly spend", ChartKind::Column, 4),
        chart(LINE_CHART_ID, "Monthly trend", ChartKind::Line, 3),
    ]
}

/// A comment thread on the budget, each edit by the person who wrote it:
/// Alex asks, Sam replies.
pub fn comments() -> Vec<(&'static str, SheetsAction)> {
    vec![
        (
            "Alex",
            SheetsAction::PutComment {
                sheet: 0,
                sheet_id: BUDGET_SHEET_ID.into(),
                comment_id: COMMENT_ID.into(),
                c0: 1,
                r0: 1,
                c1: 4,
                r1: 1,
                body: "Check the rent increase before sending this budget.".into(),
            },
        ),
        (
            "Sam",
            SheetsAction::ReplyComment {
                sheet: 0,
                sheet_id: BUDGET_SHEET_ID.into(),
                comment_id: COMMENT_ID.into(),
                reply_id: REPLY_ID.into(),
                body: "I will verify the change.".into(),
            },
        ),
    ]
}

/// Resolve the sample comment thread.
pub fn resolve_comment() -> SheetsAction {
    SheetsAction::SetCommentResolved {
        sheet: 0,
        sheet_id: BUDGET_SHEET_ID.into(),
        comment_id: COMMENT_ID.into(),
        resolved: true,
    }
}

/// The seeds of the fill-series scenario: three patterns under the budget,
/// which the fill handle then extends.
pub fn fill_seeds() -> SheetsAction {
    let mut cells = Cells::default();
    for (a1, raw) in [
        ("A15", "Fill demo"),
        ("B15", "Count"),
        ("C15", "Weekday"),
        ("D15", "Task"),
        ("B16", "1"),
        ("B17", "2"),
        ("C16", "Mon"),
        ("D16", "Task 1"),
    ] {
        cells.set(a1, raw);
    }
    SheetsAction::SetCells {
        sheet: 0,
        sheet_id: BUDGET_SHEET_ID.into(),
        cells: cells.0,
    }
}

/// Write a new workbook holding the sample, as a capture's record does.
#[cfg(test)]
pub(crate) fn write(doc: &mut automerge::AutoCommit) {
    use tilo_actions::Action as _;
    use ui::tilo_resource::Resource as _;

    crate::resource::SheetsWorkbook::initialize(doc).unwrap();
    workbook().apply(doc).unwrap();
}
