use std::io::Read;

use automerge::AutoCommit;
use serde_json::json;
use tilo_actions::{ActionRegistry, Resource};

use super::*;
use crate::actions::{SearchableSheet, SheetsResourceCtx, provide_resources};
use crate::model::format_value;

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

/// The resource tree as the app publishes it, with the searchable cells the
/// live app keeps (sort reads them).
fn sheet_resource(doc: &AutoCommit) -> Resource {
    let mut scratch = doc.clone();
    let snap = SheetsDoc::snapshot_sheet(&mut scratch, 0, Currency::USD);
    let values = crate::formula::recalculate(&snap.raw);
    let cells = snap
        .raw
        .iter()
        .map(|(&(col, row), raw)| SearchableCell {
            col,
            row,
            raw: raw.clone(),
            display: values.get(&(col, row)).map_or_else(String::new, |value| {
                format_value(value, CellFormat::General)
            }),
        })
        .collect();
    let ctx = SheetsResourceCtx {
        sheet_names: SheetsDoc::sheet_names(&mut scratch),
        searchable_sheets: vec![SearchableSheet {
            id: SheetsDoc::sheet_resource_id(doc, 0).unwrap(),
            name: snap.name,
            cells,
        }],
        palette_actions: Vec::new(),
        selection: None,
    };
    provide_resources(&ctx, doc)
        .into_iter()
        .flat_map(|workbook| workbook.children)
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
    let mut registry = ActionRegistry::new();
    crate::actions::register(&mut registry);
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
    let mut scratch = doc.clone();
    let snap = SheetsDoc::snapshot_sheet(&mut scratch, 0, Currency::USD);
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

    let mut scratch = doc.clone();
    let snap = SheetsDoc::snapshot_sheet(&mut scratch, 0, Currency::USD);
    let values = crate::formula::recalculate(&snap.raw);
    let table =
        crate::xlsx::SheetTable::from_cells(&snap.name, &snap.raw, &snap.fmt, &values, &snap.bold);
    let path = std::env::temp_dir().join(format!("sheets-bold-{}.xlsx", uuid::Uuid::new_v4()));
    crate::xlsx::write_xlsx(&path, std::slice::from_ref(&table)).unwrap();
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
    let styles = part("xl/styles.xml");
    let sheet = part("xl/worksheets/sheet1.xml");
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
            "formulas": {"B4": "=SUM(B2:B3)"}, "bold": []})
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
    use ui::Widget as _;
    let mut app = crate::app::SheetsApp::new();
    app.shots_mode();
    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    state.commit_action(None, None);
    let mut harness = ui::headless::FrameHarness::new(state, Box::new(app), (1200, 800));
    crate::actions::register(harness.action_registry_mut());
    let sheet = harness
        .resources()
        .into_iter()
        .flat_map(|workbook| workbook.children)
        .find(|child| child.kind == "sheets.sheet")
        .unwrap();
    let address = tilo_actions::ActionAddress::new(sheet.id.as_str(), "set-cells");
    let started = harness
        .invoke_action(
            &address,
            json!({"start": "Z90", "rows": [["=1+2"]]})
                .as_object()
                .unwrap(),
            false,
            None,
        )
        .unwrap();
    let value = started.completed().and_then(|output| output.value);
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
    assert_eq!(SheetsDoc::sheet_names(&mut blank), ["sales"]);

    let mut used = workbook(&[&["kept"]]);
    import(&mut used);
    assert_eq!(SheetsDoc::sheet_names(&mut used), ["Data", "sales"]);
}
