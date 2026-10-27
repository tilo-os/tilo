//! Formulas that read other sheets of the workbook (`Rates!A2`,
//! `'Q3 data'!B2:B9`).
//!
//! Every sheet is laid side by side in one address space, sheet `k`'s
//! columns starting at `k * SHEET_SPAN`, and one [`Engine`] evaluates them
//! together: a reference to another sheet is an ordinary reference into its
//! columns, so ranges, lookups and cycle detection work across sheets with
//! no second code path.

use std::collections::HashMap;

use super::Engine;
use crate::model::{Addr, CellValue};
use crate::workbook::SheetSnapshot;

/// The columns one sheet occupies in a workbook's address space: more than
/// the grid's last column, so no sheet's cells reach the next one's.
pub(super) const SHEET_SPAN: u32 = 1 << 14;

/// A sheet name as references match it: case and surrounding space aside.
pub(super) fn fold(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Every sheet of a workbook by name and raw input, for evaluating one of
/// them whose formulas read the others.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Book {
    /// Each sheet's name and raw inputs, in workbook order.
    pub sheets: Vec<(String, HashMap<Addr, String>)>,
    /// Which of them is evaluated.
    pub own: usize,
}

impl Book {
    /// Whether any input of `raw` may read another sheet: a formula naming
    /// one with `!`. Only such a sheet is evaluated with its workbook.
    #[must_use]
    pub fn needed(raw: &HashMap<Addr, String>) -> bool {
        raw.values()
            .any(|raw| raw.starts_with('=') && raw.contains('!'))
    }
}

/// The values of the cells of `book`'s own sheet, its references to the
/// other sheets read through.
#[must_use]
pub fn recalculate_in_book(book: &Book) -> HashMap<Addr, CellValue> {
    let mut raw = HashMap::new();
    let mut sheets = HashMap::new();
    let mut own_base = None;
    for ((name, cells), at) in book.sheets.iter().zip(0u32..) {
        let Some(base) = at.checked_mul(SHEET_SPAN) else {
            break;
        };
        // The first of two sheets with one name is the one references reach.
        sheets.entry(fold(name)).or_insert(base);
        if usize::try_from(at).ok() == Some(book.own) {
            own_base = Some(base);
        }
        for (&(col, row), input) in cells {
            if col < SHEET_SPAN {
                raw.insert((col + base, row), input.clone());
            }
        }
    }
    let Some(base) = own_base else {
        return HashMap::new();
    };
    let mut engine = Engine::in_book(&raw, &sheets);
    raw.keys()
        .filter(|(col, _)| (base..base.saturating_add(SHEET_SPAN)).contains(col))
        .map(|&(col, row)| ((col - base, row), engine.resolve((col, row))))
        .collect()
}

/// What each cell of `snap` computes: with the workbook it came from when
/// its formulas read other sheets, on its own otherwise.
#[must_use]
pub fn recalculate_snapshot(snap: &SheetSnapshot) -> HashMap<Addr, CellValue> {
    match &snap.book {
        Some(book) => recalculate_in_book(book),
        None => super::recalculate(&snap.raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(name: &str, cells: &[(Addr, &str)]) -> (String, HashMap<Addr, String>) {
        (
            name.to_owned(),
            cells
                .iter()
                .map(|(addr, raw)| (*addr, (*raw).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn a_formula_reads_cells_and_ranges_of_another_sheet() {
        let book = Book {
            sheets: vec![
                sheet(
                    "Summary",
                    &[
                        ((0, 0), "=Rates!B1*2"),
                        ((1, 0), "=SUM('Q3 data'!A1:A3)"),
                        ((2, 0), "=VLOOKUP(\"b\",rates!A1:B2,2,FALSE)"),
                        ((3, 0), "=Nowhere!A1"),
                    ],
                ),
                sheet(
                    "Rates",
                    &[((0, 0), "a"), ((1, 0), "21"), ((0, 1), "b"), ((1, 1), "5")],
                ),
                sheet(
                    "Q3 data",
                    &[((0, 0), "1"), ((0, 1), "2"), ((0, 2), "=A1+A2")],
                ),
            ],
            own: 0,
        };
        let values = recalculate_in_book(&book);
        assert_eq!(values.get(&(0, 0)), Some(&CellValue::Number(42.0)));
        assert_eq!(values.get(&(1, 0)), Some(&CellValue::Number(6.0)));
        assert_eq!(values.get(&(2, 0)), Some(&CellValue::Number(5.0)));
        assert_eq!(
            values.get(&(3, 0)),
            Some(&CellValue::Error(crate::model::FormulaError::Ref)),
            "a sheet the workbook lacks is #REF!"
        );
    }

    #[test]
    fn a_cycle_across_sheets_is_found() {
        let book = Book {
            sheets: vec![
                sheet("One", &[((0, 0), "=Two!A1+1")]),
                sheet("Two", &[((0, 0), "=One!A1+1")]),
            ],
            own: 0,
        };
        assert!(
            recalculate_in_book(&book)
                .get(&(0, 0))
                .is_some_and(CellValue::is_error)
        );
    }

    #[test]
    fn inserting_rows_leaves_references_to_other_sheets() {
        use crate::model::GridEdit;
        assert_eq!(
            super::super::remap_refs("=A2+Rates!A2+'Q 3'!B2:B4", GridEdit::InsertRow(0)),
            "=A3+Rates!A2+'Q 3'!B2:B4"
        );
        assert_eq!(
            super::super::offset_refs("=Rates!A2", 0, 1),
            "=Rates!A3",
            "a copy shifts them as any other"
        );
        assert_eq!(super::super::references("Rates!A2+B1").len(), 1);
        assert!(super::super::parse_error("=Rates!A2").is_none());
    }
}
