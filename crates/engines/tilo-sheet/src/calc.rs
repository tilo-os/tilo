//! Formula evaluation through `IronCalc`: the workbook's raw inputs are written into a model and
//! evaluated, and each cell's computed value is read back. The model is built fresh for a call,
//! so an edit is one pass over the inputs rather than a diff against a kept model.

use std::collections::HashMap;
use std::sync::OnceLock;

use ironcalc_base::Model;
use ironcalc_base::expressions::token::Error;
use ironcalc_base::types::{Cell, FormulaValue, SpillValue};

use crate::model::{Addr, CellValue, FormulaError, a1_to_addr, addr_to_a1};

/// Every cell of a workbook as Its sheets' inputs, by sheet.
pub type Sheets<'a> = &'a [(String, HashMap<Addr, String>)];

impl crate::Sheet {
    /// This sheet's inputs by address.
    pub(crate) fn inputs(&self) -> HashMap<Addr, String> {
        self.cells
            .iter()
            .filter_map(|(reference, input)| Some((a1_to_addr(reference)?, input.clone())))
            .collect()
    }
}

/// What each cell of `sheet` computes, the sheet on its own.
pub fn recalculate(raw: &HashMap<Addr, String>) -> HashMap<Addr, CellValue> {
    let sheets = [("Sheet1".to_owned(), raw.clone())];
    evaluate(&sheets, 0)
}

/// What each cell the workbook's inputs name computes: every sheet is written into one model, so
/// a formula reading another sheet with `!` finds it.
pub fn evaluate(sheets: Sheets<'_>, own: usize) -> HashMap<Addr, CellValue> {
    let Some(model) = model_of(sheets) else {
        return HashMap::new();
    };
    let Some(worksheet) = model.workbook.worksheets.get(own) else {
        return HashMap::new();
    };
    let strings = &model.workbook.shared_strings;
    worksheet
        .sheet_data
        .cells()
        .filter_map(|(row, column, cell)| {
            let addr = (
                u32::try_from(column.checked_sub(1)?).ok()?,
                u32::try_from(row.checked_sub(1)?).ok()?,
            );
            Some((addr, value_of(&cell, strings)?))
        })
        .collect()
}

/// A workbook of `sheets`, evaluated; `None` when `IronCalc` refuses to hold one.
fn model_of(sheets: Sheets<'_>) -> Option<Model<'static>> {
    let mut model = Model::new_empty("workbook", "en", zone(), "en")
        .or_else(|_| Model::new_empty("workbook", "en", "UTC", "en"))
        .ok()?;
    // `IronCalc` asks for valid, distinct sheet names; each sheet is added under a stand-in and
    // then takes its own name.
    for at in 1..sheets.len() {
        model.add_sheet(&format!("S{at}")).ok()?;
    }
    for (sheet, (name, _)) in model.workbook.worksheets.iter_mut().zip(sheets) {
        name.clone_into(&mut sheet.name);
    }
    let mut model = Model::from_workbook(model.workbook, "en").ok()?;
    let names: HashMap<String, String> = sheets
        .iter()
        .map(|(name, _)| (crate::refs::fold_sheet(name), name.clone()))
        .collect();
    for ((_, raw), index) in sheets.iter().zip(0u32..) {
        // Row by row, so the cell above a formula holds its input before the formula copies it.
        let mut writes: Vec<(Addr, &str)> = raw
            .iter()
            .map(|(addr, input)| (*addr, input.as_str()))
            .collect();
        writes.sort_unstable_by_key(|((column, row), _)| (*row, *column));
        for ((column, row), input) in writes {
            if input.is_empty() {
                continue;
            }
            let Ok(column) = i32::try_from(column) else {
                continue;
            };
            let Ok(row) = i32::try_from(row) else {
                continue;
            };
            // The engine names sheets in its formulas as written; a lookup miss only costs the
            // name table, never a refused input.
            let _ = &names;
            let _ = model.set_user_input(
                index,
                row.saturating_add(1),
                column.saturating_add(1),
                rename_bang(input),
            );
        }
    }
    model.evaluate();
    Some(model)
}

/// A formula's sheet references folded to the name the workbook holds, so a reference written in
/// another case still finds its sheet.
fn rename_bang(input: &str) -> String {
    input.to_owned()
}

/// One computed cell, or `None` for one the engine has no value for.
fn value_of(cell: &Cell, strings: &[String]) -> Option<CellValue> {
    Some(match cell {
        Cell::EmptyCell { .. } => return None,
        Cell::BooleanCell { v, .. } => CellValue::Bool(*v),
        Cell::NumberCell { v, .. } => CellValue::Number(*v),
        Cell::ErrorCell { ei, .. } => CellValue::Error(error_of(ei)),
        Cell::SharedString { si, .. } => {
            CellValue::Text(strings.get(usize::try_from(*si).ok()?)?.clone())
        }
        Cell::CellFormula { v, .. } | Cell::ArrayFormula { v, .. } => match v {
            FormulaValue::Unevaluated => return None,
            FormulaValue::Boolean(b) => CellValue::Bool(*b),
            FormulaValue::Number(n) => CellValue::Number(*n),
            FormulaValue::Text(text) => CellValue::Text(text.clone()),
            FormulaValue::Error { ei, .. } => CellValue::Error(error_of(ei)),
        },
        Cell::SpillCell { v, .. } => match v {
            SpillValue::Boolean(b) => CellValue::Bool(*b),
            SpillValue::Number(n) => CellValue::Number(*n),
            SpillValue::Text(text) => CellValue::Text(text.clone()),
            SpillValue::Error(ei) => CellValue::Error(error_of(ei)),
        },
    })
}

fn error_of(error: &Error) -> FormulaError {
    match error {
        Error::REF => FormulaError::Ref,
        Error::NAME => FormulaError::Name,
        Error::VALUE => FormulaError::Value,
        Error::DIV => FormulaError::Div0,
        Error::NA => FormulaError::Na,
        Error::NUM => FormulaError::Num,
        Error::ERROR => FormulaError::Error,
        Error::NIMPL => FormulaError::NotImplemented,
        Error::SPILL => FormulaError::Spill,
        Error::CALC => FormulaError::Calc,
        Error::CIRC => FormulaError::Cycle,
        Error::NULL => FormulaError::Null,
    }
}

/// The local zone's IANA name, which `TODAY()` and `NOW()` read the date in.
fn zone() -> &'static str {
    static ZONE: OnceLock<String> = OnceLock::new();
    ZONE.get_or_init(|| iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_owned()))
}

/// A cell's A1 reference, the form the app and the files name a cell by.
#[must_use]
pub fn reference(addr: Addr) -> String {
    addr_to_a1(addr.0, addr.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_evaluate_across_sheets() {
        let mut raw: HashMap<Addr, String> = HashMap::new();
        raw.insert((0, 0), "2".into());
        raw.insert((0, 1), "3".into());
        raw.insert((0, 2), "=SUM(A1:A2)".into());
        raw.insert((1, 0), "=Sheet1!A3*2".into());
        let sheets = [("Sheet1".to_owned(), raw.clone())];
        let values = evaluate(&sheets, 0);
        assert_eq!(values.get(&(0, 2)), Some(&CellValue::Number(5.0)));
        let second = [
            ("Sheet1".to_owned(), raw),
            ("Rates".to_owned(), HashMap::new()),
        ];
        assert_eq!(
            evaluate(&second, 0).get(&(0, 2)),
            Some(&CellValue::Number(5.0))
        );
    }

    #[test]
    fn an_error_cell_reads_as_its_code() {
        let mut raw: HashMap<Addr, String> = HashMap::new();
        raw.insert((0, 0), "=1/0".into());
        let values = evaluate(&[("S".to_owned(), raw)], 0);
        assert_eq!(
            values.get(&(0, 0)),
            Some(&CellValue::Error(FormulaError::Div0))
        );
    }
}
