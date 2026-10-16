use std::io::Read;

use automerge::AutoCommit;
use serde_json::json;
use tilo_actions::{ActionRegistry, Resource};

use super::*;

fn workbook(rows: &[&[&str]]) -> AutoCommit {
    let mut doc = AutoCommit::new();
    let sheet = SheetsDoc::add_sheet(&mut doc, "Data");
    for (r, row) in (0u32..).zip(rows) {
        for (c, raw) in (0u32..).zip(row.iter()) {
            SheetsDoc::set_cell_raw(&mut doc, sheet, (c, r), raw);
        }
    }
    doc.commit();
    doc
}

/// The first sheet as the workbook publishes it.
fn sheet_resource(doc: &AutoCommit) -> Resource {
    use ui::tilo_resource::Resource as _;
    let state = crate::resource::SheetsWorkbook::read(doc).unwrap();
    crate::actions::published(&state)
        .children
        .into_iter()
        .find(|child| child.kind == "sheets.sheet")
        .unwrap()
}

fn build(doc: &AutoCommit, action: &str, args: &serde_json::Value) -> EncodedAction {
    let resource = sheet_resource(doc);
    let spec = resource
        .actions
        .iter()
        .find(|spec| spec.id == action)
        .unwrap();
    spec.build_json(args.as_object().unwrap()).unwrap()
}

fn invoke(doc: &mut AutoCommit, action: &str, args: &serde_json::Value) {
    let encoded = build(doc, action, args);
    assert_ne!(
        encoded.type_id,
        tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
        "{action} refused: {}",
        String::from_utf8_lossy(&encoded.payload)
    );
    let registry = ActionRegistry::with(crate::actions::register);
    let decoded = registry.decode(&encoded).unwrap();
    tilo_actions::apply_local(doc, decoded.as_ref(), encoded).unwrap();
}

fn read(doc: &AutoCommit, range: Option<&str>) -> serde_json::Value {
    let resource = sheet_resource(doc);
    let encoded = build(
        doc,
        "read-cells",
        &range.map_or_else(|| json!({}), |range| json!({ "range": range })),
    );
    assert_eq!(encoded.type_id, READ_OPERATION_TYPE);
    validate_read(&resource, &encoded.payload).unwrap();
    project_read(doc, &doc.clone().get_heads(), &encoded.payload)
        .unwrap()
        .content
}

fn raws(doc: &AutoCommit) -> Vec<Vec<String>> {
    let snap = SheetsDoc::snapshot_sheet(doc, 0, Currency::USD);
    let rows = snap.raw.keys().map(|addr| addr.1).max().unwrap_or(0);
    let cols = snap.raw.keys().map(|addr| addr.0).max().unwrap_or(0);
    (0..=rows)
        .map(|r| {
            (0..=cols)
                .map(|c| snap.raw.get(&(c, r)).cloned().unwrap_or_default())
                .collect()
        })
        .collect()
}

/// An `open` of a workbook answers the used cells through the one action a
/// sheet declares as its document's read.
#[test]
fn read_cells_is_the_sheets_declared_document_read() {
    let doc = workbook(&[&["Region", "Q1"], &["North", "120"]]);
    let readers: Vec<_> = sheet_resource(&doc)
        .actions
        .iter()
        .filter(|spec| spec.contract.reads_document())
        .map(|spec| spec.id.clone())
        .collect();
    assert_eq!(readers, ["read-cells"]);
    assert_eq!(read(&doc, None)["range"], "A1:B2");
}

#[test]
fn set_cells_writes_values_and_formulas_that_read_back_computed() {
    let mut doc = workbook(&[&["Region", "Q1"], &["North", "120"], &["South", "90"]]);
    invoke(
        &mut doc,
        "set-cells",
        &json!({"start": "A4", "rows": [["Total", "=SUM(B2:B3)"]]}),
    );
    invoke(
        &mut doc,
        "set-cells",
        &json!({"start": "C1", "rows": [["Q2"], [135], [null], [true]]}),
    );
    let page = read(&doc, None);
    assert_eq!(page["range"], "A1:C4");
    assert_eq!(
        page["rows"],
        json!([
            ["Region", "Q1", "Q2"],
            ["North", 120.0, 135.0],
            ["South", 90.0, null],
            ["Total", 210.0, true]
        ])
    );
    assert_eq!(page["formulas"], json!({"B4": "=SUM(B2:B3)"}));
    assert_eq!(read(&doc, Some("B4"))["rows"], json!([[210.0]]));
}

/// The first sheet exported to XLSX: its `xl/styles.xml` and
/// `xl/worksheets/sheet1.xml` parts, and the tables reading the file back
/// gives.
fn exported(doc: &AutoCommit) -> (String, String, Vec<crate::xlsx::SheetTable>) {
    let snap = SheetsDoc::snapshot_sheet(doc, 0, Currency::USD);
    let values = crate::formula::recalculate(&snap.raw);
    let table =
        crate::xlsx::SheetTable::from_cells(&snap.name, &snap.raw, &snap.fmt, &values, &snap.bold);
    let path = std::env::temp_dir().join(format!("sheets-export-{}.xlsx", uuid::Uuid::new_v4()));
    crate::xlsx::write_xlsx(&path, std::slice::from_ref(&table)).unwrap();
    let back = crate::xlsx::read_xlsx(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut part = |name: &str| {
        let mut out = String::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        out
    };
    (
        part("xl/styles.xml"),
        part("xl/worksheets/sheet1.xml"),
        back,
    )
}

#[test]
fn bold_is_stored_read_back_and_exported_to_xlsx() {
    let mut doc = workbook(&[&["Region", "Q1"], &["North", "120"]]);
    invoke(
        &mut doc,
        "format-cells",
        &json!({"range": "A1:B1", "bold": true}),
    );
    invoke(
        &mut doc,
        "format-cells",
        &json!({"range": "B2", "format": "currency"}),
    );
    assert_eq!(read(&doc, None)["bold"], json!(["A1", "B1"]));

    let (styles, sheet, _) = exported(&doc);
    assert!(styles.contains(r"<font><b/>"), "{styles}");
    // A1 and B1 share the bold style; B2 has the currency style only.
    assert!(
        sheet.contains(r#"<c r="A1" t="inlineStr" s="1">"#),
        "{sheet}"
    );
    assert!(
        sheet.contains(r#"<c r="B1" t="inlineStr" s="1">"#),
        "{sheet}"
    );
    assert!(sheet.contains(r#"<c r="B2" s="2">"#), "{sheet}");
    assert!(
        styles.contains(
            r#"<xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/>"#
        ),
        "{styles}"
    );

    invoke(
        &mut doc,
        "format-cells",
        &json!({"range": "A1", "bold": false}),
    );
    assert_eq!(read(&doc, None)["bold"], json!(["B1"]));
}

#[test]
fn sort_rows_reorders_by_a_column_and_moves_formulas_with_their_row() {
    let mut doc = workbook(&[
        &["name", "signed_up", "n"],
        &["Ada", "2026-03-02", "=ROW()"],
        &["Grace", "2026-01-15", "1"],
        &["Alan", "2026-02-20", "=B4"],
    ]);
    invoke(&mut doc, "sort-rows", &json!({"range": "A2:C4", "by": "B"}));
    assert_eq!(
        raws(&doc),
        [
            ["name", "signed_up", "n"],
            ["Grace", "2026-01-15", "1"],
            ["Alan", "2026-02-20", "=B3"],
            ["Ada", "2026-03-02", "=ROW()"],
        ]
    );
    invoke(
        &mut doc,
        "sort-rows",
        &json!({"range": "A2:C4", "by": "A", "descending": true}),
    );
    assert_eq!(
        raws(&doc)
            .iter()
            .skip(1)
            .map(|row| row[0].clone())
            .collect::<Vec<_>>(),
        ["Grace", "Alan", "Ada"]
    );
}

/// A formula's result sorts by its number even once a currency format shows
/// it as "$1,635.00", which as text would sort before "$216.00".
#[test]
fn sort_rows_orders_formatted_formula_results_by_value() {
    let mut doc = workbook(&[
        &["sku", "qty", "price", "value"],
        &["A-100", "5", "123.75", "=B2*C2"],
        &["A-101", "12", "39", "=B3*C3"],
        &["A-102", "8", "42", "=B4*C4"],
        &["A-103", "4", "54", "=B5*C5"],
        &["A-104", "15", "109", "=B6*C6"],
        &["A-105", "", "", "pending"],
    ]);
    invoke(
        &mut doc,
        "format-cells",
        &json!({"range": "C2:D7", "format": "currency"}),
    );
    let skus = |doc: &AutoCommit| {
        read(doc, Some("A2:A7"))["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row[0].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };

    invoke(&mut doc, "sort-rows", &json!({"range": "A2:D7", "by": "D"}));
    // Numbers by value, then text.
    assert_eq!(
        skus(&doc),
        ["A-103", "A-102", "A-101", "A-100", "A-104", "A-105"]
    );

    invoke(
        &mut doc,
        "sort-rows",
        &json!({"range": "A2:D6", "by": "D", "descending": true}),
    );
    assert_eq!(
        skus(&doc),
        ["A-104", "A-100", "A-101", "A-102", "A-103", "A-105"]
    );
    assert_eq!(
        read(&doc, Some("D2:D6"))["rows"],
        json!([[1635.0], [618.75], [468.0], [336.0], [216.0]])
    );

    // A literal amount shown as currency sorts by its number too.
    invoke(&mut doc, "sort-rows", &json!({"range": "A2:D6", "by": "C"}));
    assert_eq!(
        skus(&doc),
        ["A-101", "A-102", "A-103", "A-104", "A-100", "A-105"]
    );

    // The exported file carries the sorted rows, formulas pointing at their
    // own row.
    let (_, _, back) = exported(&doc);
    let rows = &back[0].rows;
    let column = |col: usize| {
        rows.iter()
            .skip(1)
            .take(5)
            .map(|row| row[col].as_ref().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        column(0)
            .iter()
            .map(|cell| cell.raw.as_str())
            .collect::<Vec<_>>(),
        ["A-101", "A-102", "A-103", "A-104", "A-100"]
    );
    assert_eq!(
        column(3)
            .iter()
            .map(|cell| cell.formula.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["=B2*C2", "=B3*C3", "=B4*C4", "=B5*C5", "=B6*C6"]
    );
}

/// `read-cells` names each formatted cell's number format as `format-cells`
/// takes it, so another sheet can be laid out exactly like this one.
#[test]
fn read_cells_answers_number_formats_by_their_format_cells_names() {
    let mut doc = workbook(&[
        &["Visits", "Orders", "Revenue", "Conversion"],
        &["1000", "25", "1200", "=B2/A2"],
        &["800", "30", "1500", "=B3/A3"],
    ]);
    assert_eq!(read(&doc, None)["formats"], json!({}));
    invoke(
        &mut doc,
        "format-cells",
        &json!({"range": "D2:D3", "format": "percent"}),
    );
    invoke(
        &mut doc,
        "format-cells",
        &json!({"range": "C2:C3", "format": "currency"}),
    );
    invoke(
        &mut doc,
        "format-cells",
        &json!({"range": "B2", "format": "number"}),
    );
    let page = read(&doc, None);
    assert_eq!(
        page["formats"],
        json!({"B2": "number", "C2": "currency", "C3": "currency",
            "D2": "percent", "D3": "percent"})
    );
    // The rest of the answer keeps its shape.
    assert_eq!(page["formulas"], json!({"D2": "=B2/A2", "D3": "=B3/A3"}));
    assert_eq!(page["rows"][1], json!([1000.0, 25.0, 1200.0, 0.025]));
    assert_eq!(
        read(&doc, Some("A1:B2"))["formats"],
        json!({"B2": "number"})
    );

    // Each name is one format-cells takes, and applying it stores the same
    // format: copying the formats onto other cells reads back the same.
    let formats = page["formats"].as_object().unwrap().clone();
    for (cell, name) in &formats {
        let target = cell.replace('2', "5").replace('3', "6");
        invoke(
            &mut doc,
            "format-cells",
            &json!({"range": target, "format": name}),
        );
    }
    assert_eq!(
        read(&doc, Some("A5:D6"))["formats"],
        json!({"B5": "number", "C5": "currency", "C6": "currency",
            "D5": "percent", "D6": "percent"})
    );
}

/// An ISO date written by `set-cells` is a real date: a serial in the date
/// format that calculates, reads back as the ISO date it was written as,
/// shows as the date and exports to XLSX as a date. Other date-like text
/// stays text.
#[test]
fn set_cells_turns_exact_iso_dates_into_real_dates() {
    let mut doc = workbook(&[&["task", "start", "days", "end"]]);
    let answered = answer(
        &mut doc,
        "set-cells",
        &json!({"start": "A2", "rows": [
            ["Launch", "2026-10-22", 10, "=B2+C2"],
            ["Review", "2026-10-22T09:30"],
            ["Slash", "22/10/2026"],
            ["Impossible", "2026-02-30"],
            ["Loose", "2026-1-5"],
            ["Spaced", " 2026-10-22"]
        ]}),
    );
    let serial = crate::model::date_to_serial(2026, 10, 22);
    assert_eq!(
        answered["rows"][0],
        json!(["Launch", "2026-10-22", 10.0, serial + 10.0]),
        "the date reads as written; the sum in a general cell is its serial"
    );
    assert_eq!(answered["formats"], json!({"B2": "date", "B3": "date"}));
    assert_eq!(answered["rows"][1][1], json!("2026-10-22T09:30"));
    assert_eq!(
        read(&doc, Some("B4:B7"))["rows"],
        json!([
            ["22/10/2026"],
            ["2026-02-30"],
            ["2026-1-5"],
            [" 2026-10-22"]
        ])
    );

    // It shows as the date it is.
    let state = {
        use ui::tilo_resource::Resource as _;
        crate::resource::SheetsWorkbook::read(&doc).unwrap()
    };
    let start = state.sheets[0]
        .cells()
        .iter()
        .find(|cell| (cell.col, cell.row) == (1, 1))
        .unwrap()
        .display
        .clone();
    assert_eq!(start, "2026-10-22");

    let (styles, _, back) = exported(&doc);
    assert!(styles.contains("yyyy-mm-dd"), "{styles}");
    let rows = &back[0].rows;
    let start = rows[1][1].as_ref().unwrap();
    assert_eq!(start.format, CellFormat::Date(DateStyle::Iso));
    assert_eq!(start.raw, serial.to_string());
    let impossible = rows[4][1].as_ref().unwrap();
    assert_eq!(impossible.raw, "2026-02-30");
    assert_ne!(impossible.format, CellFormat::Date(DateStyle::Iso));
}

/// A date read from a date-formatted cell goes back in through `set-cells`
/// as the same date, while a number in a general cell stays a number.
#[test]
fn read_cells_answers_dates_as_iso_text_that_set_cells_takes_back() {
    let mut doc = workbook(&[&["due", "count"]]);
    invoke(
        &mut doc,
        "set-cells",
        &json!({"start": "A2", "rows": [["2026-10-22", 46307]]}),
    );
    let first = read(&doc, Some("A2:B2"))["rows"].clone();
    assert_eq!(first, json!([["2026-10-22", 46307.0]]));
    invoke(
        &mut doc,
        "set-cells",
        &json!({"start": "A3", "rows": [[first[0][0].clone()]]}),
    );
    assert_eq!(read(&doc, Some("A3"))["rows"], json!([["2026-10-22"]]));
    assert_eq!(raws(&doc)[1][0], raws(&doc)[2][0]);
}

/// A leading apostrophe marks a value as text, as typing it does: the cell
/// holds the value without it and in the text format.
#[test]
fn set_cells_drops_the_apostrophe_that_marks_text() {
    let mut doc = workbook(&[&["ref", "code"]]);
    invoke(
        &mut doc,
        "set-cells",
        &json!({"start": "A2", "rows": [["'S-89761", "'Q3 note"], ["'", "it's"]]}),
    );
    assert_eq!(
        read(&doc, Some("A2:B3"))["rows"],
        json!([["S-89761", "Q3 note"], ["'", "it's"]])
    );
    assert_eq!(
        read(&doc, Some("A2:B3"))["formats"],
        json!({"A2": "text", "B2": "text"})
    );
}

#[test]
fn delete_rows_removes_several_rows_in_one_step_and_moves_bold() {
    let mut doc = workbook(&[&["h"], &["a"], &["dup"], &["b"], &["dup"], &["c"]]);
    invoke(
        &mut doc,
        "format-cells",
        &json!({"range": "A6", "bold": true}),
    );
    invoke(&mut doc, "delete-rows", &json!({"rows": [5, 3]}));
    assert_eq!(raws(&doc), [["h"], ["a"], ["b"], ["c"]]);
    assert_eq!(read(&doc, None)["bold"], json!(["A4"]));
}

/// What `action` answers its caller with, as the dispatch computes it from the
/// change it applied.
fn answer(doc: &mut AutoCommit, action: &str, args: &serde_json::Value) -> serde_json::Value {
    let before = doc.get_heads();
    invoke(doc, action, args);
    let after = doc.get_heads();
    let changes = doc.diff(&before, &after);
    let sheet = SheetsDoc::sheet_resource_id(doc, 0).unwrap();
    edit_result(doc, &sheet, action, &changes).unwrap_or(serde_json::Value::Null)
}

#[test]
fn cell_edits_answer_with_the_range_they_changed() {
    let mut doc = workbook(&[&["Region", "Q1"], &["North", "120"], &["South", "90"]]);
    let set = answer(
        &mut doc,
        "set-cells",
        &json!({"start": "A4", "rows": [["Total", "=SUM(B2:B3)"]]}),
    );
    assert_eq!(
        set,
        json!({"sheet": "Data", "range": "A4:B4", "rows": [["Total", 210.0]],
            "formulas": {"B4": "=SUM(B2:B3)"}, "bold": [], "formats": {}})
    );
    let bolded = answer(
        &mut doc,
        "format-cells",
        &json!({"range": "A1:B1", "bold": true}),
    );
    assert_eq!(bolded["range"], "A1:B1");
    assert_eq!(bolded["bold"], json!(["A1", "B1"]));
    let sorted = answer(&mut doc, "sort-rows", &json!({"range": "A2:B3", "by": "B"}));
    assert_eq!(sorted["range"], "A2:B3");
    assert_eq!(sorted["rows"], json!([["South", 90.0], ["North", 120.0]]));
    let deleted = answer(&mut doc, "delete-rows", &json!({"rows": [1]}));
    // Deleting rows rewrites the sheet, so it answers what remains from the top.
    assert_eq!(deleted["range"], "A1:B4");
    assert_eq!(
        deleted["rows"],
        json!([
            ["South", 90.0],
            ["North", 120.0],
            ["Total", 210.0],
            [null, null]
        ])
    );
    assert_eq!(deleted["formulas"], json!({"B3": "=SUM(B1:B2)"}));
    assert_eq!(deleted["bold"], json!([]));
    assert!(sorted.get("changed").is_none());
}

#[test]
fn a_large_edit_answers_its_first_rows_and_names_the_whole_range() {
    let mut doc = workbook(&[&["x"]]);
    let rows: Vec<serde_json::Value> = (0..100).map(|row| json!([row, row, row, row])).collect();
    let set = answer(&mut doc, "set-cells", &json!({"start": "A1", "rows": rows}));
    assert_eq!(set["changed"], "A1:D100");
    assert_eq!(set["range"], "A1:D64");
    assert_eq!(set["rows"].as_array().map(Vec::len), Some(64));

    let wide: Vec<serde_json::Value> = (0..300).map(|col| json!(col)).collect();
    let set = answer(
        &mut doc,
        "set-cells",
        &json!({"start": "A200", "rows": [wide]}),
    );
    assert_eq!(set["changed"], "A200:KN200");
    assert_eq!(set["rows"][0].as_array().map(Vec::len), Some(256));
}

#[test]
fn the_app_answers_a_cell_edit_through_its_action_door() {
    use ui::framework::testing::TestApp;
    let mut app = TestApp::<crate::SheetsShell>::new().unwrap();
    let sheet = app.state().sheets[0].id.clone();
    let value = app
        .call_on(
            &crate::actions::sheet_resource_id(&sheet),
            "set-cells",
            json!({"start": "Z90", "rows": [["=1+2"]]}),
        )
        .unwrap();
    assert_eq!(
        value
            .as_ref()
            .map(|value| (&value["range"], &value["rows"])),
        Some((&json!("Z90"), &json!([[3.0]]))),
        "{value:?}"
    );
}

#[test]
fn only_the_cell_edits_declare_an_answer() {
    let doc = workbook(&[&["x"]]);
    for spec in &sheet_resource(&doc).actions {
        let answers = matches!(spec.contract.result, ResultSpec::Json { .. })
            && spec.kind == OperationKind::Document;
        assert_eq!(
            answers,
            ANSWERING_EDITS.contains(&spec.id.as_str()),
            "{}",
            spec.id
        );
    }
}

#[test]
fn malformed_arguments_are_refused_by_name() {
    let doc = workbook(&[&["x"]]);
    for (action, args, message) in [
        (
            "set-cells",
            json!({"start": "nowhere", "rows": [["x"]]}),
            "start must be a cell",
        ),
        (
            "set-cells",
            json!({"start": "A1", "rows": [[{"nested": 1}]]}),
            "cell values are",
        ),
        (
            "format-cells",
            json!({"range": "A1"}),
            "give bold, format or both",
        ),
        (
            "sort-rows",
            json!({"range": "A1:B3", "by": "D"}),
            "inside range",
        ),
        ("delete-rows", json!({"rows": [0]}), "starting at 1"),
    ] {
        let encoded = build(&doc, action, &args);
        assert_eq!(
            encoded.type_id,
            tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
            "{action}"
        );
        assert!(
            String::from_utf8_lossy(&encoded.payload).contains(message),
            "{action}"
        );
    }
}

#[test]
fn a_read_names_its_own_sheet_and_stays_bounded() {
    let doc = workbook(&[&["x"]]);
    let resource = sheet_resource(&doc);
    let other = serde_json::to_vec(&ReadRequest {
        sheet: "another-sheet".into(),
        range: None,
    })
    .unwrap();
    assert_eq!(
        validate_read(&resource, &other),
        Err(SemanticReadError::InvalidRequest)
    );
    let wide = build(&doc, "read-cells", &json!({"range": "A1:Z1000"}));
    assert!(matches!(
        project_read(&doc, &doc.clone().get_heads(), &wide.payload),
        Err(SemanticReadError::Limit)
    ));
}

#[test]
fn an_import_into_a_blank_workbook_replaces_its_empty_sheet() {
    let import = |doc: &mut AutoCommit| {
        let mut action = crate::actions::import_sheets_action(vec![(
            "sales".to_string(),
            vec![CellWrite {
                col: 0,
                row: 0,
                raw: "Region".into(),
                fmt: None,
            }],
        )]);
        if let SheetsAction::ImportSheets { replace_blank, .. } = &mut action {
            *replace_blank = true;
        }
        tilo_actions::apply_local(doc, &action, encode(&action)).unwrap();
    };
    let mut blank = AutoCommit::new();
    SheetsDoc::add_sheet(&mut blank, "Sheet1");
    import(&mut blank);
    assert_eq!(SheetsDoc::sheet_names(&blank), ["sales"]);

    let mut used = workbook(&[&["kept"]]);
    import(&mut used);
    assert_eq!(SheetsDoc::sheet_names(&used), ["Data", "sales"]);
}

/// The workbook as published, with its sheet-naming cell actions.
fn workbook_resource(doc: &AutoCommit) -> Resource {
    use ui::tilo_resource::Resource as _;
    crate::actions::published(&crate::resource::SheetsWorkbook::read(doc).unwrap())
}

fn build_on_workbook(doc: &AutoCommit, action: &str, args: &serde_json::Value) -> EncodedAction {
    let resource = workbook_resource(doc);
    let spec = resource
        .actions
        .iter()
        .find(|spec| spec.id == action)
        .unwrap_or_else(|| panic!("the workbook offers {action}"));
    spec.build_json(args.as_object().unwrap()).unwrap()
}

/// A caller that knows a sheet only by name edits and reads it through the
/// workbook, the name matched ignoring case; the sheet's id works too.
#[test]
fn workbook_cell_actions_name_their_sheet() {
    let mut doc = workbook(&[&["Region", "Q1"], &["North", "0.25"]]);
    let encoded = build_on_workbook(
        &doc,
        "format-cells",
        &json!({"sheet": " data ", "range": "B2", "format": "percent", "bold": true}),
    );
    assert_ne!(
        encoded.type_id,
        tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
        "{}",
        String::from_utf8_lossy(&encoded.payload)
    );
    let registry = ActionRegistry::with(crate::actions::register);
    let decoded = registry.decode(&encoded).unwrap();
    tilo_actions::apply_local(&mut doc, decoded.as_ref(), encoded).unwrap();

    let sheet_id = SheetsDoc::sheet_resource_id(&doc, 0).unwrap();
    let read = build_on_workbook(
        &doc,
        "read-cells",
        &json!({"sheet": sheet_resource_id(&sheet_id), "range": "B2"}),
    );
    assert_eq!(read.type_id, READ_OPERATION_TYPE);
    validate_read(&workbook_resource(&doc), &read.payload).unwrap();
    let page = project_read(&doc, &doc.clone().get_heads(), &read.payload)
        .unwrap()
        .content;
    assert_eq!(page["formats"], json!({"B2": "percent"}));
    assert_eq!(page["bold"], json!(["B2"]));
}

/// An unknown sheet is refused with the names there are, so the caller
/// retries with one of them.
#[test]
fn an_unknown_sheet_is_refused_with_the_sheet_names() {
    let mut doc = workbook(&[&["x"]]);
    SheetsDoc::add_sheet(&mut doc, "Summary");
    doc.commit();
    let encoded = build_on_workbook(
        &doc,
        "set-cells",
        &json!({"sheet": "0b6f9e3e-0000-4000-8000-000000000000", "start": "A1", "rows": [[1]]}),
    );
    assert_eq!(
        encoded.type_id,
        tilo_actions::resource::INVALID_ARGUMENTS_TYPE
    );
    let message = String::from_utf8_lossy(&encoded.payload).into_owned();
    assert!(
        message.contains("no sheet `0b6f9e3e-0000-4000-8000-000000000000`")
            && message.contains("`Data`, `Summary`"),
        "{message}"
    );
}

/// A computed margin reads as the value it is to a spreadsheet's 15
/// digits, not the binary noise of the division (`0.7499999999999999`).
#[test]
fn computed_values_read_to_fifteen_significant_digits() {
    let doc = workbook(&[
        &["Price", "Cost", "Margin", "Margin %"],
        &["3.8", "0.95", "=A2-B2", "=C2/A2"],
        &["4.2", "1.1", "=A3-B3", "=C3/A3"],
    ]);
    let rows = read(&doc, None)["rows"].clone();
    assert_eq!(rows[1][2], json!(2.85));
    assert_eq!(rows[1][3], json!(0.75));
    assert_eq!(rows[2][3], json!(0.738_095_238_095_238));
}

/// The refusal `action` answers `args` with; panics when it is accepted.
fn refusal(doc: &AutoCommit, action: &str, args: &serde_json::Value) -> String {
    let encoded = build(doc, action, args);
    assert_eq!(
        encoded.type_id,
        tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
        "{action} accepted {args}"
    );
    String::from_utf8_lossy(&encoded.payload).into_owned()
}

/// A formula that does not parse is refused naming its cell and what is
/// wrong, before anything is written: not stored to show `#ERROR!`, and the
/// valid cells beside it are not written either.
#[test]
fn set_cells_refuses_a_formula_that_does_not_parse() {
    let doc = workbook(&[&["Item", "Price"], &["x", "4"]]);
    let error = refusal(
        &doc,
        "set-cells",
        &json!({"start": "A4", "rows": [[
            "Total",
            "=INDEX(A1:A5, MATCH(\"x\", B1:B5, 0)",
        ]]}),
    );
    assert!(error.contains("cell B4"), "{error}");
    assert!(error.contains("`INDEX(` is never closed"), "{error}");
    assert_eq!(raws(&doc), [["Item", "Price"], ["x", "4"]]);
}

/// A lone `=` is a formula with nothing in it, which the cell would show as
/// `#ERROR!`, so it is refused like any formula that does not parse.
#[test]
fn set_cells_refuses_a_lone_equals_sign() {
    let doc = workbook(&[&["x"]]);
    let error = refusal(&doc, "set-cells", &json!({"start": "C2", "rows": [["="]]}));
    assert!(
        error.contains("cell C2") && error.contains("nothing follows the `=`"),
        "{error}"
    );
}

/// Only input that starts with `=` is a formula: text that merely starts
/// with something odd is written as it is, and formulas that parse are
/// written even when what they compute is an error.
#[test]
fn set_cells_writes_valid_formulas_and_text_that_only_looks_odd() {
    let mut doc = workbook(&[&["1", "2"]]);
    invoke(
        &mut doc,
        "set-cells",
        &json!({"start": "A2", "rows": [
            [" =1+", "+1", "#N/A", "'=x", "50%", "(1"],
            ["=SUM(A1:B1)", "=A1/0", "=NOSUCH(1)", "='Q3 data'!A1", "=-A1%", "=\"a\"&\"b\""],
        ]}),
    );
    assert_eq!(
        raws(&doc)[1..],
        [
            [" =1+", "+1", "#N/A", "'=x", "50%", "(1"],
            [
                "=SUM(A1:B1)",
                "=A1/0",
                "=NOSUCH(1)",
                "='Q3 data'!A1",
                "=-A1%",
                "=\"a\"&\"b\""
            ],
        ]
    );
}

/// An import replaces only a new workbook's untouched blank sheet: one
/// renamed, even back to its default name, is kept beside the import.
#[test]
fn an_import_keeps_a_renamed_blank_sheet() {
    let import = |doc: &mut AutoCommit| {
        let mut action = crate::actions::import_sheets_action(vec![(
            "sales".to_string(),
            vec![CellWrite {
                col: 0,
                row: 0,
                raw: "Region".into(),
                fmt: None,
            }],
        )]);
        if let SheetsAction::ImportSheets { replace_blank, .. } = &mut action {
            *replace_blank = true;
        }
        tilo_actions::apply_local(doc, &action, encode(&action)).unwrap();
    };
    let mut renamed = AutoCommit::new();
    SheetsDoc::add_sheet(&mut renamed, "Sheet1");
    SheetsDoc::rename_sheet(&mut renamed, 0, "Costs");
    import(&mut renamed);
    assert_eq!(SheetsDoc::sheet_names(&renamed), ["Costs", "sales"]);

    let mut back = AutoCommit::new();
    SheetsDoc::add_sheet(&mut back, "Sheet1");
    SheetsDoc::rename_sheet(&mut back, 0, "Costs");
    SheetsDoc::rename_sheet(&mut back, 0, "Sheet1");
    import(&mut back);
    assert_eq!(SheetsDoc::sheet_names(&back), ["Sheet1", "sales"]);
}
