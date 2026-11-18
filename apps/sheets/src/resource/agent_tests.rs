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
        refused.contains("no sheet named Nowhere") && refused.contains("Q3 sales"),
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
    assert!(refused.contains("title"), "{refused}");
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

#[test]
fn a_past_version_reads_and_shows_its_cells() {
    let mut app = app();
    call(
        &mut app,
        "write-cells",
        json!({ "sheet": "Sheet1", "cells": [{ "cell": "B2", "input": "Lighthouse rent" }] }),
    );
    let past = app.versions().pop().unwrap().hash;
    call(
        &mut app,
        "write-cells",
        json!({ "sheet": "Sheet1", "cells": [{ "cell": "B2", "input": "Harbour fees" }] }),
    );

    let read = call(
        &mut app,
        "read",
        json!({ "version": past.to_string(), "item": "Sheet1" }),
    );
    assert!(
        read["content"].to_string().contains("Lighthouse rent"),
        "{read}"
    );
    let shown = app.past_version(&[past]).unwrap().texts();
    assert!(
        shown.iter().any(|text| text.contains("Lighthouse rent")),
        "{shown:?}"
    );
}

/// A sheet made with no content answers with itself: its maker has the id to write to, and is
/// not told the call failed after it took effect.
#[test]
fn a_sheet_made_with_no_content_answers_itself() {
    let mut app = app();
    let answer = call(&mut app, "new-sheet", json!({"name": "Empty"}));
    let state = app.state();
    let (id, _) = state
        .sheets
        .iter()
        .find(|(_, sheet)| sheet.name.0 == "Empty")
        .expect("the sheet was made");
    assert_eq!(
        (&answer["sheet"], &answer["item"], &answer["rows"]),
        (&json!("Empty"), &json!(id.as_str()), &json!([])),
        "{answer}"
    );
}

/// The sheet `arguments` name, kept as CSV content and read back through `read-content`.
fn csv_of(app: &mut TestApp<SheetsShell>, arguments: Value) -> Value {
    let output = app
        .call_on_output("sheets.workbook", "export-csv-content", arguments)
        .unwrap()
        .unwrap();
    let address = output.resources.first().cloned().unwrap();
    call(
        app,
        "read-content",
        json!({ "content": address, "name": "sheet.csv" }),
    )
}

/// A named sheet leaves as CSV content, with no file to write and whatever the window shows, and
/// reads back through `read-content`.
#[test]
fn a_named_sheet_is_kept_as_csv_content_that_reads_back() {
    let mut app = app();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Prices", "rows": [["Tea", 3.5], ["Cake", 4]]}),
    );
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Notes", "rows": [["a, b", "say \"hi\""], ["plain", "x"]]}),
    );
    let notes = csv_of(&mut app, json!({"sheet": "notes"}));
    assert_eq!(
        notes["rows"],
        json!([["a, b", "say \"hi\""], ["plain", "x"]]),
        "{notes}"
    );
    let first = csv_of(&mut app, json!({}));
    assert_eq!(
        first["rows"],
        json!([["Tea", "3.5"], ["Cake", "4"]]),
        "the first sheet shown when none is named: {first}"
    );
    assert!(
        app.call_on_output(
            "sheets.workbook",
            "export-csv-content",
            json!({"sheet": "Nowhere"})
        )
        .is_err(),
        "a sheet the workbook lacks is refused"
    );
    assert_eq!(sheet_names(&app), ["Prices", "Notes"], "nothing changed");
}

/// Twenty thousand cells, half of them formulas, read as values: the parts join up, each number
/// a number, and the words a cell shows come only when asked for.
#[test]
fn a_read_of_twenty_thousand_cells_joins_up_from_its_parts() {
    let mut app = app();
    call(&mut app, "new-sheet", json!({"name": "Long"}));
    for half in 0..2u32 {
        let first = half * 5000 + 1;
        let rows: Vec<Value> = (first..first + 5000)
            .map(|n| json!([n, format!("=A{n}*2")]))
            .collect();
        call(
            &mut app,
            "set-cells",
            json!({"sheet": "Long", "start": format!("A{first}"), "rows": rows}),
        );
    }
    call(
        &mut app,
        "format-cells",
        json!({"sheet": "Long", "range": "B10000", "format": "currency:USD"}),
    );

    let mut rows = Vec::new();
    let mut formulas = 0;
    let mut parts = 0;
    let mut from = Value::Null;
    loop {
        let part = call(
            &mut app,
            "read-cells",
            json!({"sheet": "Long", "from": from}),
        );
        assert_eq!(part["of"], "A1:B10000", "each part names the whole");
        assert!(part.get("shown").is_none(), "no words unless asked");
        rows.extend(part["rows"].as_array().unwrap().iter().cloned());
        formulas += part["formulas"].as_object().unwrap().len();
        parts += 1;
        match part.get("next") {
            Some(next) => from = next.clone(),
            None => break,
        }
    }
    assert_eq!((parts, rows.len(), formulas), (2, 10_000, 10_000));
    let wrong: Vec<&Value> = rows
        .iter()
        .zip(1u32..)
        .filter(|(row, n)| **row != json!([f64::from(*n), f64::from(*n) * 2.0]))
        .map(|(row, _)| row)
        .take(3)
        .collect();
    assert!(wrong.is_empty(), "every row is its two numbers: {wrong:?}");

    let end = call(
        &mut app,
        "read-cells",
        json!({"sheet": "Long", "range": "A9999:B10000", "shown": true}),
    );
    assert_eq!(end["rows"], json!([[9999.0, 19998.0], [10000.0, 20000.0]]));
    assert_eq!(
        end["shown"],
        json!([["9999", "19998"], ["10000", "$20,000.00"]])
    );
    assert_eq!(end["formats"], json!({"B10000": "currency:USD"}));
    assert!(
        end.get("next").is_none() && end.get("of").is_none(),
        "{end}"
    );

    let refused = refusal(
        &mut app,
        "read-cells",
        json!({"sheet": "Long", "range": "A1:B10", "from": 11}),
    );
    assert!(refused.contains("`from` is a row of A1:B10"), "{refused}");
}

#[test]
fn a_read_shows_the_cells_a_formula_spills_into() {
    let mut app = app();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Sales", "rows": [["Region", "Revenue"], ["North", 10], ["South", 20], ["North", 5]]}),
    );
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Summary", "rows": [["=UNIQUE(Sales!A2:A4)", "=SUMIF(Sales!A$2:A$4,A1,Sales!B$2:B$4)"]]}),
    );
    call(
        &mut app,
        "write-cells",
        json!({"sheet": "Summary", "cells": [{"cell": "B2", "input": "=SUMIF(Sales!A$2:A$4,A2,Sales!B$2:B$4)"}]}),
    );
    let whole = call(&mut app, "read", json!({"item": "Summary"}));
    assert_eq!(whole["range"], "A1:B2", "{whole}");
    assert_eq!(
        whole["rows"],
        json!([["North", "15"], ["South", "20"]]),
        "the second group is a cell the first formula fills: {whole}"
    );
    assert_eq!(
        whole["formulas"].as_object().map(serde_json::Map::len),
        Some(3),
        "a cell a formula fills holds no formula of its own: {whole}"
    );
    let part = call(&mut app, "read", json!({"item": "Summary!A2:A3"}));
    assert_eq!(part["rows"], json!([["South"], [""]]), "{part}");
}

/// What a formula spills into is found by search as any other cell is: by what it shows, at
/// its own cell.
#[test]
fn search_finds_the_cells_a_formula_spills_into() {
    let mut app = app();
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Sales", "rows": [["Region", "Revenue"], ["North", 10], ["South", 20], ["North", 5]]}),
    );
    call(
        &mut app,
        "new-sheet",
        json!({"name": "Summary", "rows": [["=UNIQUE(Sales!A2:A4)"]]}),
    );
    let state = app.state();
    let summary = state
        .sheet_key(&ui::tilo_resource::Id::from_raw("Summary"))
        .unwrap();
    let entries = super::Workbook::search(&state);
    let found = |title: &str| {
        entries
            .iter()
            .find(|entry| entry.title == title)
            .unwrap_or_else(|| panic!("search holds no {title}"))
    };
    let own = found("Summary!A1");
    assert!(
        own.text.contains("=UNIQUE(Sales!A2:A4)") && own.text.contains("North"),
        "the formula's own cell, by its input and what it shows: {}",
        own.text
    );
    let spilled = found("Summary!A2");
    assert_eq!(spilled.text, "A2 Summary!A2 South");
    assert_eq!(
        spilled.anchor.as_deref().and_then(super::parse_cell_anchor),
        Some((summary, (0, 1))),
        "it opens at the cell it is shown in"
    );
    assert!(
        entries.iter().all(|entry| entry.title != "Summary!A3"),
        "a cell that shows nothing is not listed"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one file after another, read as an agent reads them"
)]
fn spreadsheet_files_read_as_cells_without_joining_the_workbook() {
    use std::fmt::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let mut app = app();

    // A CSV file says nothing of its cells but their text, so that is what is read.
    let csv = dir.path().join("leads.csv");
    std::fs::write(
        &csv,
        "Company,Stage,Value\nAcme,Won,00120\n\"Birch, Ltd\",Open,300\n",
    )
    .unwrap();
    let leads = call(&mut app, "read-file", json!({ "file": csv }));
    assert_eq!(leads["format"], "csv");
    assert_eq!(leads["sheet"], "leads");
    assert_eq!(
        leads["rows"],
        json!([
            ["Company", "Stage", "Value"],
            ["Acme", "Won", "00120"],
            ["Birch, Ltd", "Open", "300"]
        ]),
        "{leads}"
    );
    assert_eq!(leads["next"], Value::Null);

    // A workbook another program wrote: its sheets, and what each cell holds.
    let xlsx = dir.path().join("budget.xlsx");
    std::fs::write(
        &xlsx,
        crate::interchange_tests::excel_like_xlsx_hiding_notes(),
    )
    .unwrap();
    let budget = call(&mut app, "read-file", json!({ "file": xlsx }));
    let sheets: Vec<(String, bool)> = budget["sheets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|sheet| {
            (
                sheet["name"].as_str().unwrap().to_owned(),
                sheet["hidden"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        sheets,
        [
            ("Budget".to_owned(), false),
            ("Notes & Data".to_owned(), true)
        ]
    );
    assert_eq!(budget["sheet"], "Budget");
    assert_eq!(budget["rows"][1][0], "Rent", "{budget}");
    assert_eq!(budget["rows"][1][1], 1200.0, "a number reads as a number");
    assert_eq!(budget["formats"]["B2"], "currency:USD");
    assert_eq!(
        budget["rows"][2][0], "2023-03-15",
        "a date reads as its ISO text: {budget}"
    );
    let notes = call(
        &mut app,
        "read-file",
        json!({ "file": xlsx, "sheet": "notes & data" }),
    );
    assert_eq!(notes["sheet"], "Notes & Data");
    assert_eq!(notes["rows"][0][0], "note <1>", "{notes}");
    assert_eq!(notes["rows"][2][2], 7.0);

    // A long sheet answers in parts that join up.
    let long = dir.path().join("long.csv");
    let mut lines = String::new();
    for n in 1..=6000 {
        let _ = writeln!(lines, "{n},x");
    }
    std::fs::write(&long, lines).unwrap();
    let first = call(&mut app, "read-file", json!({ "file": long }));
    assert_eq!(first["rows"].as_array().unwrap().len(), 5000);
    assert_eq!(first["next"], 5001);
    assert_eq!(first["total"], 6000);
    let rest = call(&mut app, "read-file", json!({ "file": long, "from": 5001 }));
    assert_eq!(rest["rows"].as_array().unwrap().len(), 1000);
    assert_eq!(rest["rows"][0][0], "5001");
    assert_eq!(rest["next"], Value::Null);

    // What is neither is refused by name, and so is a sheet the file lacks.
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "not cells").unwrap();
    let refused = refusal(&mut app, "read-file", json!({ "file": text }));
    assert!(refused.contains("not a spreadsheet"), "{refused}");
    let missing = refusal(
        &mut app,
        "read-file",
        json!({ "file": xlsx, "sheet": "Nope" }),
    );
    assert!(missing.contains("Budget"), "{missing}");
    assert_eq!(
        sheet_names(&app),
        ["Sheet1"],
        "the workbook stays as it was"
    );

    // A workbook the workspace holds as content reads the same way, formulas and all.
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
    let kept = call(
        &mut app,
        "read-content",
        json!({ "content": address, "name": "prices.xlsx" }),
    );
    assert_eq!(
        kept["rows"],
        json!([["Tea", 3.5], ["Total", 3.5]]),
        "{kept}"
    );
    assert_eq!(kept["formulas"], json!({ "B2": "=SUM(B1:B1)" }));
    assert_eq!(kept["formats"]["B2"], "currency:USD");
}
