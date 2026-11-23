//! The spreadsheet formula engine, as its own library in the runtime bundle
//! (`libtilo_spreadsheet_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use ironcalc_base;

/// Run the engine's own code: evaluate a workbook's cells, from arithmetic
/// over a range to a statistical function, a date's year and a rounded
/// number.
///
/// The engine's check binary and Sheets' `--probe-spreadsheet-engine` call it,
/// so a release proves the library loads and runs against its runtime.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use ironcalc_base::Model;

    let mut model = Model::new_empty("workbook", "en", "UTC", "en")
        .map_err(|error| format!("IronCalc refuses an empty workbook: {error}"))?;
    let inputs: [(i32, i32, &str); 6] = [
        (1, 1, "2"),
        (2, 1, "3"),
        (1, 2, "=SUM(A1:A2)"),
        (2, 2, "=NORM.DIST(0,0,1,TRUE)"),
        (3, 2, "=YEAR(DATE(2026,11,15))"),
        (4, 2, "=ROUND(2.567,1)"),
    ];
    for (row, column, input) in inputs {
        model
            .set_user_input(0, row, column, input.to_string())
            .map_err(|error| format!("IronCalc refuses {input} at ({row},{column}): {error}"))?;
    }
    model.evaluate();
    let expected = [(1, 2, "5"), (2, 2, "0.5"), (3, 2, "2026"), (4, 2, "2.6")];
    for (row, column, want) in expected {
        let got = model
            .get_formatted_cell_value(0, row, column)
            .map_err(|error| format!("IronCalc cannot read ({row},{column}): {error}"))?;
        if got != want {
            return Err(format!("({row},{column}) is {got}, not {want}"));
        }
    }
    Ok(format!(
        "{} cells evaluated, {} read",
        inputs.len(),
        expected.len()
    ))
}
