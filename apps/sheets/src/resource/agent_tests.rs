//! What an agent does with a workbook through the published door: sheets
//! made with their content, records written by header, sheets named rather
//! than numbered, reads that answer what the cells mean, and workbooks
//! carried in and out as content.

use serde_json::{Value, json};
use ui::framework::testing::TestApp;

use super::SheetsShell;

fn app() -> TestApp<SheetsShell> {
    TestApp::<SheetsShell>::new().unwrap()
}

fn call(app: &mut TestApp<SheetsShell>, id: &str, arguments: Value) -> Value {
    app.call(id, arguments)
        .unwrap_or_else(|error| panic!("{id}: {error}"))
        .unwrap_or(Value::Null)
}

fn refusal(app: &mut TestApp<SheetsShell>, id: &str, arguments: Value) -> String {
    match app.call(id, arguments) {
        Ok(answer) => panic!("{id} was not refused: {answer:?}"),
        Err(error) => error.to_string(),
    }
}

fn sheet_names(app: &TestApp<SheetsShell>) -> Vec<String> {
    app.state()
        .sheets
        .values()
        .map(|sheet| sheet.name.0.clone())
        .collect()
}

fn read(app: &mut TestApp<SheetsShell>, sheet: &str, range: &str) -> Value {
    call(app, "read-cells", json!({ "sheet": sheet, "range": range }))
}

#[test]
fn new_sheet_writes_headers_records_and_formats_in_one_step() {
    let mut app = app();
    let answer = call(
        &mut app,
        "new-sheet",
        json!({
            "name": "Budget",
            "workbook": "Summer party",
            "columns": ["Item", "Price", "Qty", "Total"],
            "records": [
                {"Item": "Venue", "Price": 145, "Qty": 8, "Total": "=[Price]*[Qty]"},
                {"Item": "Cake", "Price": 30.5, "Total": "=B{row}*C{row}"},
            ],
            "formats": {"B2:B3": "money", "D2:D3": "currency"},
        }),
    );
    assert_eq!(
        sheet_names(&app),
        ["Budget"],
        "the new workbook's untouched Sheet1 gave way"
    );
    assert_eq!(app.state().title.as_deref(), Some("Summer party"));
    assert_eq!(answer["sheet"], "Budget");
    let budget = app.state().sheets.ids().next().unwrap().clone();
    assert_eq!(
        answer["item"],
        budget.as_str(),
        "the answer names the new sheet"
    );
    assert_eq!(answer["rows"][1], json!(["Venue", 145.0, 8.0, 1160.0]));
    assert_eq!(answer["formulas"]["D3"], "=B3*C3");
    let read = read(&mut app, "budget", "A1:D3");
    assert_eq!(read["formats"]["B2"], "currency:USD");
    assert_eq!(read["formats"]["D3"], "currency:USD");
}

#[test]
fn new_sheet_rows_take_iso_dates_and_text_marked_with_an_apostrophe() {
    let mut app = app();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Plan", "rows": [["due", "code"], ["2026-10-22", "'007"], ["2026-10-22T09:30", "'=not a formula"]]}),
    );
    let read = read(&mut app, "Plan", "A2:B3");
    assert_eq!(
        read["rows"],
        json!([
            ["2026-10-22", "007"],
            ["2026-10-22T09:30", "'=not a formula"]
        ]),
        "a date reads back as written and a marked value as text"
    );
    assert_eq!(read["formats"]["A2"], "date");
    assert_eq!(read["formats"]["B2"], "text");
}

/// `write-cells` reads a value as `set-cells` does: an ISO date is a real
/// date whether or not the write names a format, so a form's date check
/// counts it; a format naming text keeps it as typed.
#[test]
fn written_cells_take_iso_dates_as_real_dates() {
    let mut app = app();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Form", "rows": [["Born", "=COUNT(A2)"]]}),
    );
    call(
        &mut app,
        "write-cells",
        json!({"sheet": "Form", "cells": [
            {"cell": "A2", "input": "1988-04-03"},
            {"cell": "B2", "input": "=COUNT(A2)+COUNT(A3)"},
            {"cell": "A3", "input": "2026-09-25", "format": "date"},
            {"cell": "A4", "input": "2026-09-25", "format": "text"},
            {"cell": "A5", "input": "'007"},
        ]}),
    );
    let read = read(&mut app, "Form", "A2:B5");
    assert_eq!(
        read["rows"],
        json!([
            ["1988-04-03", 2.0],
            ["2026-09-25", null],
            ["2026-09-25", null],
            ["007", null]
        ])
    );
    assert_eq!(read["formats"]["A2"], "date");
    assert_eq!(read["formats"]["A3"], "date");
    assert_eq!(read["formats"]["A4"], "text");
    assert_eq!(read["formats"]["A5"], "text");
}

#[test]
fn a_renamed_blank_sheet_is_kept_when_a_sheet_with_content_is_added() {
    let mut app = app();
    let first = app.state().sheets.ids().next().unwrap().clone();
    call(
        &mut app,
        "field-edit",
        json!({"edit": {"path": [{"field": "sheets"}, {"item": first.as_str()}, {"field": "name"}], "op": "set", "value": "Costs"}}),
    );
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Rates", "rows": [["a", 1]]}),
    );
    assert_eq!(sheet_names(&app), ["Costs", "Rates"]);
}

#[test]
fn verbs_taking_a_sheet_take_its_name_and_refuse_one_the_workbook_lacks() {
    let mut app = app();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Q3 sales", "rows": [["x"]]}),
    );
    let answer = call(
        &mut app,
        "set-cells",
        json!({"sheet": "q3 SALES", "start": "B2", "rows": [[1, 2]]}),
    );
    assert_eq!(answer["rows"], json!([[1.0, 2.0]]));
    let refused = refusal(
        &mut app,
        "set-cells",
        json!({"sheet": "Nowhere", "start": "A1", "rows": [[1]]}),
    );
    assert!(
        refused.contains("no sheet `Nowhere`") && refused.contains("`Q3 sales`"),
        "{refused}"
    );
}

#[test]
fn set_records_appends_by_header_and_refuses_a_row_that_moved() {
    let mut app = app();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Guests", "columns": ["Name", "Party", "Meal"], "records": [{"Name": "Ana", "Party": 2}]}),
    );
    let id = app
        .state()
        .sheet_key(&ui::tilo_resource::Id::from_raw("Guests"))
        .unwrap();
    let item = format!("sheets.workbook/sheets/{}", id.as_str());
    app.call_on(
        &item,
        "set-records",
        json!({"records": [{"name": "Bo", "Meal": "veg"}, {"Name": "Cy", "Party": 3}]}),
    )
    .unwrap();
    let rows = read(&mut app, "Guests", "A1:C4")["rows"].clone();
    assert_eq!(
        rows,
        json!([
            ["Name", "Party", "Meal"],
            ["Ana", 2.0, null],
            ["Bo", null, "veg"],
            ["Cy", 3.0, null]
        ])
    );
    let stale = app
        .call_on(
            &item,
            "set-records",
            json!({"row": 1, "records": [{"Name": "Bo", "Meal": "fish"}]}),
        )
        .unwrap_err()
        .to_string();
    assert!(stale.contains("holds `Ana` under `Name`"), "{stale}");
    let unknown = refusal(
        &mut app,
        "set-records",
        json!({"sheet": "Guests", "records": [{"Guest": "Di"}]}),
    );
    assert!(
        unknown.contains("no column `Guest`") && unknown.contains("`Party`"),
        "{unknown}"
    );
}

#[test]
fn set_records_with_a_key_updates_the_matching_row_and_appends_the_rest() {
    let mut app = app();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Stock", "columns": ["Sku", "Count"], "records": [{"Sku": "A-1", "Count": 4}, {"Sku": 7, "Count": 1}]}),
    );
    call(
        &mut app,
        "set-records",
        json!({"sheet": "Stock", "key": "sku", "records": [{"Sku": "a-1", "Count": 9}, {"Sku": "7.0", "Count": 2}, {"Sku": "B-2", "Count": 5}]}),
    );
    assert_eq!(
        read(&mut app, "Stock", "A2:B4")["rows"],
        json!([["a-1", 9.0], [7.0, 2.0], ["B-2", 5.0]])
    );
    let twice = refusal(
        &mut app,
        "set-records",
        json!({"sheet": "Stock", "key": "Sku", "records": [{"Sku": "Z"}, {"Sku": "z"}]}),
    );
    assert!(twice.contains("twice"), "{twice}");
    let both = refusal(
        &mut app,
        "set-records",
        json!({"sheet": "Stock", "key": "Sku", "row": 1, "records": [{"Sku": "Z"}]}),
    );
    assert!(both.contains("not both"), "{both}");
}

#[test]
fn upsert_item_finds_a_row_by_its_header_values_and_appends_a_new_one() {
    let mut app = app();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Guests", "columns": ["Name", "Team", "Seats"], "records": [{"Name": "Ana Lima", "Team": "Ops", "Seats": 1}, {"Name": "Ben", "Team": "Web", "Seats": 2}]}),
    );
    call(
        &mut app,
        "upsert-item",
        json!({"sheet": "Guests", "match": {"name": " ana  lima"}, "value": {"Seats": 3}}),
    );
    call(
        &mut app,
        "upsert-item",
        json!({"sheet": "Guests", "match": {"Name": "Cy"}, "value": {"Team": "Ops", "Seats": 1}}),
    );
    // Run again, it changes nothing.
    call(
        &mut app,
        "upsert-item",
        json!({"sheet": "Guests", "match": {"Name": "Cy"}, "value": {"Team": "Ops", "Seats": 1}}),
    );
    assert_eq!(
        read(&mut app, "Guests", "A2:C5")["rows"],
        json!([
            ["Ana Lima", "Ops", 3.0],
            ["Ben", "Web", 2.0],
            ["Cy", "Ops", 1.0],
            [null, null, null]
        ])
    );
    let two = refusal(
        &mut app,
        "upsert-item",
        json!({"sheet": "Guests", "match": {"Team": "ops"}, "value": {"Seats": 9}}),
    );
    assert!(two.contains("rows 2 and 4"), "{two}");
}

#[test]
fn set_records_on_an_empty_sheet_writes_its_headers_first() {
    let mut app = app();
    call(
        &mut app,
        "set-records",
        json!({"sheet": "Sheet1", "records": [{"City": "Oslo", "Temp": -3}]}),
    );
    assert_eq!(
        read(&mut app, "Sheet1", "A1:B2")["rows"],
        json!([["City", "Temp"], ["Oslo", -3.0]])
    );
}

#[test]
fn reads_round_numbers_to_fifteen_significant_digits() {
    let mut app = app();
    call(
        &mut app,
        "set-cells",
        json!({"sheet": "Sheet1", "start": "A1", "rows": [["=0.1+0.2", "=1-0.25-0.0000000000000001"]]}),
    );
    assert_eq!(read(&mut app, "Sheet1", "A1")["rows"], json!([[0.3]]));
}

#[test]
fn formulas_read_cells_of_other_sheets() {
    let mut app = app();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Rates", "rows": [["hourly", 21]]}),
    );
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Q3 data", "rows": [[1], [2], [3]]}),
    );
    let answer = call(
        &mut app,
        "new-sheet",
        json!({"name": "Summary", "rows": [["=Rates!B1*2", "=SUM('Q3 data'!A1:A3)", "=Gone!A1"]]}),
    );
    assert_eq!(answer["rows"], json!([[42.0, 6.0, "#REF!"]]));
    assert!(
        answer.get("formula_errors").is_none(),
        "a reference to another sheet parses: {answer}"
    );
}

#[test]
fn the_workbook_title_names_it_and_the_listing_shows_names_and_title() {
    let mut app = app();
    call(&mut app, "set-title", json!({"title": "  Lumo pricing  "}));
    assert_eq!(app.state().title.as_deref(), Some("Lumo pricing"));
    let refused = refusal(&mut app, "set-title", json!({"title": "   "}));
    assert!(refused.contains("empty"), "{refused}");
    let listing = call(&mut app, "read", json!({}));
    assert_eq!(listing["title"], "Lumo pricing");
    assert_eq!(listing["sheets"][0]["name"], "Sheet1");
}

#[test]
fn an_xlsx_kept_as_content_imports_with_formats_and_hidden_sheets() {
    let mut app = app();
    let hash = app
        .content()
        .put(&crate::interchange_tests::excel_like_xlsx_hiding_notes())
        .unwrap();
    let address = format!("tilo.content:{hash}");
    let answer = call(
        &mut app,
        "import-xlsx-content",
        json!({"content": address, "title": "Budget 2026.xlsx"}),
    );
    assert_eq!(answer["imported"], true);
    app.settle();
    let state = app.state();
    assert_eq!(sheet_names(&app), ["Budget", "Notes & Data"]);
    assert_eq!(state.title.as_deref(), Some("Budget 2026"));
    let hidden: Vec<bool> = state.sheets.values().map(|sheet| sheet.hidden).collect();
    assert_eq!(hidden, [false, true]);
    let read = read(&mut app, "Budget", "B2:B4");
    assert_eq!(read["formats"]["B2"], "currency:USD");
    assert_eq!(read["formats"]["B4"], "percent");
    let again = call(&mut app, "import-xlsx-content", json!({"content": address}));
    assert_eq!(
        again["imported"], false,
        "the same content is not added twice"
    );
    assert_eq!(app.state().sheets.len(), 2);
}

#[test]
fn the_workbook_exports_as_xlsx_content_that_imports_back() {
    let mut app = app();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Prices", "rows": [["Tea", 3.5], ["Total", "=SUM(B1:B1)"]], "formats": {"B1:B2": "money"}}),
    );
    let output = app
        .call_on_output("sheets.workbook", "export-xlsx-content", json!({}))
        .unwrap()
        .unwrap();
    let address = output.resources.first().cloned().unwrap();
    let hash = address.strip_prefix("tilo.content:").unwrap();
    let bytes = app.content().read(hash).unwrap();
    let tables = crate::xlsx::parse_xlsx(&bytes).unwrap();
    assert_eq!(tables[0].name, "Prices");
    let total = tables[0].rows[1][1].as_ref().unwrap();
    assert_eq!(total.formula.as_deref(), Some("=SUM(B1:B1)"));
    assert_eq!(
        total.format,
        crate::model::CellFormat::Currency(tilo_locale::Currency::USD)
    );
}
